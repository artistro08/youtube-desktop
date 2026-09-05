use crate::app::config::PakeConfig;
use crate::util::{
    check_file_or_append, get_data_dir, get_download_message_with_lang, sanitize_download_filename,
    show_toast, MessageType,
};
#[cfg(target_os = "macos")]
use dispatch::Queue;
#[cfg(target_os = "macos")]
use objc2::MainThreadMarker;
#[cfg(target_os = "macos")]
use objc2_web_kit::WKUserContentController;
#[cfg(target_os = "windows")]
use std::{os::windows::ffi::OsStrExt, ptr, sync::OnceLock};
use std::{
    path::PathBuf,
    str::FromStr,
    sync::atomic::{AtomicU32, Ordering},
};
use tauri::{
    webview::{DownloadEvent, NewWindowFeatures, NewWindowResponse},
    AppHandle, Config, Manager, Url, WebviewUrl, WebviewWindow, WebviewWindowBuilder,
};

#[cfg(target_os = "windows")]
use windows_sys::Win32::UI::{
    Shell::ExtractIconExW,
    WindowsAndMessaging::{SendMessageW, ICON_BIG, WM_SETICON},
};

use tauri::Theme;

#[cfg(target_os = "macos")]
use tauri::TitleBarStyle;

#[cfg(target_os = "macos")]
fn prepare_macos_new_window_configuration(features: &NewWindowFeatures) -> tauri::Result<()> {
    let mtm = MainThreadMarker::new().ok_or_else(|| {
        std::io::Error::other("macOS new-window configuration must run on the main thread")
    })?;
    // WebKit requires the exact target configuration it supplied to be used
    // for the new page. Replace only the inherited content controller so Wry
    // can register Pake's IPC handler and scripts once on the new webview.
    let controller = unsafe { WKUserContentController::new(mtm) };
    unsafe {
        features
            .opener()
            .target_configuration
            .setUserContentController(&controller);
    }
    Ok(())
}

#[cfg(target_os = "windows")]
fn build_proxy_browser_arg(url: &Url) -> Option<String> {
    let host = url.host_str()?;
    let scheme = url.scheme();
    let port = url.port().or(match scheme {
        "http" => Some(80),
        "socks5" => Some(1080),
        _ => None,
    })?;

    match scheme {
        "http" | "socks5" => Some(format!("--proxy-server={scheme}://{host}:{port}")),
        _ => None,
    }
}

pub struct MultiWindowState {
    pub pake_config: PakeConfig,
    pub tauri_config: Config,
    next_window_index: AtomicU32,
}

impl MultiWindowState {
    pub fn new(pake_config: PakeConfig, tauri_config: Config) -> Self {
        Self {
            pake_config,
            tauri_config,
            next_window_index: AtomicU32::new(0),
        }
    }

    fn next_window_label(&self) -> String {
        let index = self.next_window_index.fetch_add(1, Ordering::Relaxed) + 1;
        format!("pake-{index}")
    }
}

pub fn set_window(
    app: &AppHandle,
    config: &PakeConfig,
    tauri_config: &Config,
) -> tauri::Result<WebviewWindow> {
    build_window_with_label(app, config, tauri_config, "pake")
}

pub fn open_additional_window(app: &AppHandle) -> tauri::Result<WebviewWindow> {
    let state = app.state::<MultiWindowState>();
    let label = state.next_window_label();
    build_window_with_label(app, &state.pake_config, &state.tauri_config, &label)
}

#[cfg(target_os = "windows")]
fn taskbar_icon_handle() -> Option<isize> {
    static TASKBAR_ICON: OnceLock<Option<isize>> = OnceLock::new();

    *TASKBAR_ICON.get_or_init(|| {
        let executable = match std::env::current_exe() {
            Ok(path) => path,
            Err(error) => {
                eprintln!(
                    "[Pake] Failed to resolve the app executable for its taskbar icon: {error}"
                );
                return None;
            }
        };
        let executable_wide: Vec<u16> = executable
            .as_os_str()
            .encode_wide()
            .chain(std::iter::once(0))
            .collect();
        let mut large_icon = ptr::null_mut();
        let extracted = unsafe {
            ExtractIconExW(
                executable_wide.as_ptr(),
                0,
                &mut large_icon,
                ptr::null_mut(),
                1,
            )
        };
        if extracted == 0 || large_icon.is_null() {
            eprintln!(
                "[Pake] Failed to extract the taskbar icon from {}.",
                executable.display()
            );
            return None;
        }

        // WM_SETICON keeps this handle rather than copying it. Cache the single
        // extracted icon for the process lifetime so repeated tray restores do
        // not leak a new HICON or leave the window with a dangling handle.
        Some(large_icon as isize)
    })
}

// Apps autostarted at Windows logon can register their icons before Explorer's
// icon cache is ready (#1323). Re-assert both the small/title-bar icon and the
// large taskbar icon whenever a window becomes visible.
#[cfg(target_os = "windows")]
pub fn reapply_window_icon(window: &WebviewWindow) {
    if let Some(icon) = window.app_handle().default_window_icon().cloned() {
        if let Err(error) = window.set_icon(icon) {
            eprintln!("[Pake] Failed to re-apply the window icon: {error}");
        }
    }

    let Some(taskbar_icon) = taskbar_icon_handle() else {
        return;
    };
    match window.hwnd() {
        Ok(hwnd) => unsafe {
            SendMessageW(hwnd.0, WM_SETICON, ICON_BIG as usize, taskbar_icon);
        },
        Err(error) => {
            eprintln!("[Pake] Failed to resolve the window handle for its taskbar icon: {error}");
        }
    }
}

#[cfg(not(target_os = "windows"))]
pub fn reapply_window_icon(_window: &WebviewWindow) {}

struct WindowBuildOptions<'a> {
    label: &'a str,
    url: WebviewUrl,
    visible: bool,
    new_window_features: Option<NewWindowFeatures>,
}

fn open_requested_window(
    app: &AppHandle,
    config: &PakeConfig,
    tauri_config: &Config,
    target_url: Url,
    features: NewWindowFeatures,
) -> tauri::Result<WebviewWindow> {
    let state = app.state::<MultiWindowState>();
    let label = state.next_window_label();
    let window = build_window(
        app,
        config,
        tauri_config,
        WindowBuildOptions {
            label: &label,
            url: WebviewUrl::External(target_url.clone()),
            visible: true,
            new_window_features: Some(features),
        },
    )?;

    let title = target_url.host_str().unwrap_or(target_url.as_str());
    let _ = window.set_title(title);
    reapply_window_icon(&window);
    let _ = window.set_focus();

    Ok(window)
}

/// Run a script in the page as though the user had just clicked something.
///
/// Chromium gates a few APIs — picture-in-picture among them — behind a real
/// user gesture, and a plain eval carries none: it is refused with
/// NotAllowedError. On Windows the script goes in through the DevTools
/// protocol instead, which can mark it user-initiated. Everywhere else, and if
/// that route is unavailable, it falls back to a plain eval, which is enough
/// for anything not behind the gesture check.
pub fn eval_with_user_gesture(window: &WebviewWindow, expression: &str) {
    #[cfg(target_os = "windows")]
    {
        use windows::core::{HSTRING, PCWSTR};

        // Built with the JSON serialiser rather than by hand: the expression
        // ends up inside a JSON string and has to be escaped as one.
        let parameters = serde_json::json!({
            "expression": expression,
            "userGesture": true,
        })
        .to_string();

        // The fallback lives inside the closure rather than after it.
        // `with_webview` reports whether the closure was handed to the event
        // loop, not whether it worked, so testing its result outside would
        // call the DevTools route a success even when it failed and leave the
        // page never hearing about it at all.
        let fallback = window.clone();
        let script = expression.to_string();

        let dispatched = window.with_webview(move |webview| unsafe {
            let failure = match webview.controller().CoreWebView2() {
                Ok(core) => {
                    let parameters = HSTRING::from(parameters);
                    core.CallDevToolsProtocolMethod(
                        windows::core::w!("Runtime.evaluate"),
                        PCWSTR(parameters.as_ptr()),
                        None,
                    )
                    .err()
                    .map(|error| error.to_string())
                }
                Err(error) => Some(error.to_string()),
            };

            let Some(failure) = failure else {
                return;
            };

            // Everything except a gesture-gated API still works this way, so a
            // degraded run beats no run.
            eprintln!("[Pake] Falling back to a plain eval ({failure}).");
            if let Err(error) = fallback.eval(&script) {
                eprintln!("[Pake] Failed to run '{script}': {error}");
            }
        });

        if dispatched.is_ok() {
            return;
        }
    }

    if let Err(error) = window.eval(expression) {
        eprintln!("[Pake] Failed to run '{expression}': {error}");
    }
}

/// Settle the page's playback because the window is about to go out of sight.
///
/// Every route to a hidden window comes through here — the title bar's own
/// buttons, the taskbar, Alt+F4, the tray — so picture-in-picture and pausing
/// behave the same however the window was dismissed. The page decides what to
/// act on; this only says whether picture-in-picture was asked for.
pub fn hide_playback(app: &AppHandle, wants_picture_in_picture: bool) {
    let Some(window) = app.get_webview_window("pake") else {
        return;
    };

    // The settings travel with the call rather than being cached in the page.
    // A cache there starts empty and fills asynchronously, so a window hidden
    // in the first moments after a page load would read a missing value — and
    // "pause a playing Short" defaults to on, so the miss would fail the wrong
    // way. The app holds the values already; passing them costs nothing.
    let settings = app.try_state::<crate::app::settings::AppSettings>();
    let pause_shorts = settings
        .as_ref()
        .is_some_and(|settings| settings.pause_shorts());
    let pip_on_shorts = settings
        .as_ref()
        .is_some_and(|settings| settings.pip_on_shorts());

    eval_with_user_gesture(
        &window,
        &format!(
            "window.__pakeHidePlayback?.({{ pictureInPicture: {wants_picture_in_picture}, \
             pauseShorts: {pause_shorts}, shortsMayPopOut: {pip_on_shorts} }})"
        ),
    );
}

/// Take a floating video back into the page, because the window is visible
/// again.
///
/// The mirror of `hide_playback`, and hooked to the same set of routes: the
/// tray, the taskbar, and the title bar's own controls.
pub fn restore_playback(app: &AppHandle) {
    let Some(window) = app.get_webview_window("pake") else {
        return;
    };

    eval_with_user_gesture(&window, "window.__pakeRestorePlayback?.()");
}

/// Hand the browser's own keyboard shortcuts back to the page.
///
/// WebView2 claims a set of browser accelerators before the page ever sees the
/// key: Ctrl+Shift+C and F12 open DevTools, Ctrl+F opens the browser's find
/// bar, Ctrl+R reloads, Ctrl+P prints. That is a browser's contract, not an
/// app's, and it means a shortcut the page defines for itself never fires —
/// Ctrl+Shift+C opened DevTools instead of copying the link. Turning them off
/// leaves the app menu's own accelerators for find, reload and zoom, which do
/// the same jobs, and text editing keys (Ctrl+C, Ctrl+V, Ctrl+A, Ctrl+Z) are
/// explicitly not part of this set.
#[cfg(target_os = "windows")]
fn disable_browser_accelerator_keys(window: &WebviewWindow) {
    use webview2_com::Microsoft::Web::WebView2::Win32::ICoreWebView2Settings3;
    // Brings `cast`, which is how the newer settings interface is reached from
    // the base one COM hands back.
    use windows::core::Interface;

    let result = window.with_webview(|webview| unsafe {
        let controller = webview.controller();
        let settings = controller
            .CoreWebView2()
            .and_then(|core| core.Settings())
            .and_then(|settings| settings.cast::<ICoreWebView2Settings3>());

        match settings {
            Ok(settings) => {
                if let Err(error) = settings.SetAreBrowserAcceleratorKeysEnabled(false) {
                    eprintln!("[Pake] Failed to release the browser shortcut keys: {error}");
                }
            }
            Err(error) => {
                eprintln!("[Pake] Browser shortcut keys are not configurable here: {error}");
            }
        }
    });

    if let Err(error) = result {
        eprintln!("[Pake] Could not reach the webview to release its shortcut keys: {error}");
    }
}

/// Open a multi-window clone of the home app. The window is built hidden and
/// revealed on its first real page load (see `lib.rs` on_page_load) so Cmd+N
/// does not flash an empty shell the way the main window used to.
pub fn open_additional_window_safe(app: &AppHandle) {
    #[cfg(target_os = "windows")]
    {
        let app_handle = app.clone();
        std::thread::spawn(move || {
            if let Ok(window) = open_additional_window(&app_handle) {
                // Fallback if PageLoadEvent::Finished never arrives.
                let fallback = window.clone();
                tauri::async_runtime::spawn(async move {
                    tokio::time::sleep(tokio::time::Duration::from_millis(3_000)).await;
                    reveal_built_window(&fallback);
                });
            }
        });
    }

    #[cfg(not(target_os = "windows"))]
    {
        if let Ok(window) = open_additional_window(app) {
            let fallback = window.clone();
            tauri::async_runtime::spawn(async move {
                tokio::time::sleep(tokio::time::Duration::from_millis(3_000)).await;
                reveal_built_window(&fallback);
            });
        }
    }
}

/// Show a window that was built hidden once content is ready (or the fallback
/// timer fires). No-ops when already visible so page-load and fallback can race.
pub fn reveal_built_window(window: &WebviewWindow) {
    if window.is_visible().unwrap_or(true) {
        return;
    }
    let _ = window.show();
    reapply_window_icon(window);
    let _ = window.set_focus();
}

/// True when any Pake webview window is currently on screen.
///
/// A minimized window does not count. Windows keeps `IsWindowVisible` true while
/// a window is iconic, and `hide_on_close` minimizes before hiding, so treating
/// minimized as visible makes the tray toggle hide an already-invisible window
/// instead of restoring it (#1343).
pub fn any_app_window_visible(app: &AppHandle) -> bool {
    app.webview_windows().values().any(|window| {
        window.is_visible().unwrap_or(false) && !window.is_minimized().unwrap_or(false)
    })
}

/// Hide every webview window (main + multi-window clones). Used by tray Hide
/// and the activation shortcut so secondary windows are not left on screen.
pub fn hide_all_app_windows(app: &AppHandle) {
    // Hiding to the tray is the same dismissal the close button performs, so it
    // takes the same setting.
    let wants_picture_in_picture = app
        .try_state::<crate::app::settings::AppSettings>()
        .is_some_and(|settings| settings.pip_on_close());
    hide_playback(app, wants_picture_in_picture);

    for window in app.webview_windows().values() {
        let _ = window.hide();
    }
}

/// Show every webview window, reassert icons, and focus the main window.
pub fn show_all_app_windows(app: &AppHandle, init_fullscreen: bool) {
    let windows = app.webview_windows();
    for window in windows.values() {
        let _ = window.unminimize();
        let _ = window.show();
        reapply_window_icon(window);
        #[cfg(target_os = "linux")]
        if init_fullscreen && !window.is_fullscreen().unwrap_or(false) {
            let _ = window.set_fullscreen(true);
        }
    }
    #[cfg(not(target_os = "linux"))]
    let _ = init_fullscreen;

    if let Some(main) = windows.get("pake") {
        let _ = main.set_focus();
    } else if let Some(any) = windows.values().next() {
        let _ = any.set_focus();
    }

    // The window the video was popped out of is back, so the video belongs in
    // it again rather than in a floating window on top of it.
    restore_playback(app);
}

/// Tray-click / activation-shortcut toggle: hide all if anything is visible,
/// otherwise show all. Cancels startup reveal when the caller has already done
/// so; this helper only touches visibility.
pub fn toggle_all_app_windows(app: &AppHandle, init_fullscreen: bool) {
    if any_app_window_visible(app) {
        hide_all_app_windows(app);
    } else {
        show_all_app_windows(app, init_fullscreen);
    }
}

fn build_window_with_label(
    app: &AppHandle,
    config: &PakeConfig,
    tauri_config: &Config,
    label: &str,
) -> tauri::Result<WebviewWindow> {
    let window_config = config.windows.first().ok_or_else(|| {
        tauri::Error::Io(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "pake.json must define at least one window configuration",
        ))
    })?;
    let url = match window_config.url_type.as_str() {
        "web" => {
            let parsed = window_config.url.parse().map_err(|err| {
                tauri::Error::Io(std::io::Error::new(
                    std::io::ErrorKind::InvalidInput,
                    format!(
                        "Invalid 'web' url '{}' in pake.json: {err}",
                        window_config.url
                    ),
                ))
            })?;
            WebviewUrl::App(parsed)
        }
        "local" => WebviewUrl::App(PathBuf::from(&window_config.url)),
        other => {
            return Err(tauri::Error::Io(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                format!("url_type must be 'web' or 'local', got '{other}'"),
            )));
        }
    };

    build_window(
        app,
        config,
        tauri_config,
        WindowBuildOptions {
            label,
            url,
            visible: false,
            new_window_features: None,
        },
    )
}

fn build_window(
    app: &AppHandle,
    config: &PakeConfig,
    tauri_config: &Config,
    opts: WindowBuildOptions,
) -> tauri::Result<WebviewWindow> {
    let WindowBuildOptions {
        label,
        url,
        visible,
        new_window_features,
    } = opts;
    #[cfg(target_os = "macos")]
    let use_native_window_tabbing = config.multi_window && new_window_features.is_none();
    #[cfg(target_os = "macos")]
    let prefer_native_window_tabbing = use_native_window_tabbing && label != "pake";

    let package_name = tauri_config
        .product_name
        .clone()
        .unwrap_or_else(|| "pake".to_string());
    let _data_dir = get_data_dir(app, package_name).map_err(tauri::Error::Io)?;

    let window_config = config.windows.first().ok_or_else(|| {
        tauri::Error::Io(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "pake.json must define at least one window configuration",
        ))
    })?;

    // On macOS both HTTP Basic auth and certificate bypass use the same
    // navigation-delegate proxy. Start on a neutral page so the proxy is in
    // place before the target can issue its first authentication challenge.
    #[cfg(target_os = "macos")]
    let auth_target = if label == "pake"
        && window_config.url_type == "web"
        && (config.basic_auth || window_config.ignore_certificate_errors)
    {
        Url::parse(&window_config.url).ok()
    } else {
        None
    };

    // The delegate must be installed before the first TLS challenge. Start on
    // a neutral page, then navigate from the with_webview callback below.
    #[cfg(target_os = "macos")]
    let url = if auth_target.is_some() {
        WebviewUrl::CustomProtocol(
            Url::parse("about:blank").expect("about:blank must be a valid URL"),
        )
    } else {
        url
    };

    let user_agent = config.user_agent.get();

    let config_script = format!(
        "window.pakeConfig = {}",
        serde_json::to_string(&window_config).unwrap_or_else(|_| "{}".to_string())
    );

    // Platform-specific title: macOS prefers empty, others fallback to product name
    let effective_title = window_config.title.as_deref().unwrap_or_else(|| {
        if cfg!(target_os = "macos") {
            ""
        } else {
            tauri_config.product_name.as_deref().unwrap_or("")
        }
    });

    let mut window_builder = WebviewWindowBuilder::new(app, label, url)
        .title(effective_title)
        .visible(visible)
        .user_agent(user_agent)
        .resizable(window_config.resizable)
        .maximized(window_config.maximize);

    #[cfg(target_os = "windows")]
    {
        let scale_factor = app
            .primary_monitor()
            .ok()
            .flatten()
            .map(|m| m.scale_factor())
            .unwrap_or(1.0);
        let logical_width = window_config.width / scale_factor;
        let logical_height = window_config.height / scale_factor;
        window_builder = window_builder.inner_size(logical_width, logical_height);
    }

    #[cfg(not(target_os = "windows"))]
    {
        window_builder = window_builder.inner_size(window_config.width, window_config.height);
    }

    window_builder = window_builder
        .always_on_top(window_config.always_on_top)
        .incognito(window_config.incognito);

    #[cfg(any(target_os = "windows", target_os = "macos"))]
    {
        window_builder = window_builder.fullscreen(window_config.fullscreen);
    }

    if window_config.min_width > 0.0 || window_config.min_height > 0.0 {
        let min_w = if window_config.min_width > 0.0 {
            window_config.min_width
        } else {
            window_config.width
        };
        let min_h = if window_config.min_height > 0.0 {
            window_config.min_height
        } else {
            window_config.height
        };
        window_builder = window_builder.min_inner_size(min_w, min_h);
    }

    if !window_config.enable_drag_drop {
        window_builder = window_builder.disable_drag_drop_handler();
    }

    if window_config.new_window {
        let app_handle = app.clone();
        let popup_config = config.clone();
        let popup_tauri_config = tauri_config.clone();
        window_builder = window_builder.on_new_window(move |target_url, features| {
            match open_requested_window(
                &app_handle,
                &popup_config,
                &popup_tauri_config,
                target_url,
                features,
            ) {
                Ok(window) => NewWindowResponse::Create { window },
                Err(error) => {
                    eprintln!("[Pake] Failed to open requested window: {error}");
                    NewWindowResponse::Deny
                }
            }
        });
    }

    // Add initialization scripts. Order matters: pakeConfig must land before
    // any script that reads it (e.g. fullscreen polyfill checks for an opt-out
    // flag), and toast must register `window.pakeToast` before Rust code
    // calls show_toast().
    window_builder = window_builder.initialization_script(&config_script);

    // find.js is opt-in via --enable-find and no-ops at runtime when disabled,
    // so only inject its ~700 lines when the feature is on. Avoids parsing the
    // find UI on every page load in the common (find-off) case. Matches the
    // enable_find gating already applied to the Find menu item.
    if window_config.enable_find {
        window_builder = window_builder.initialization_script(include_str!("../inject/find.js"));
    }

    window_builder = window_builder
        .initialization_script(include_str!("../inject/toast.js"))
        .initialization_script(include_str!("../inject/fullscreen.js"))
        .initialization_script(include_str!("../inject/event.js"))
        .initialization_script(include_str!("../inject/style.js"))
        .initialization_script(include_str!("../inject/theme_refresh.js"))
        .initialization_script(include_str!("../inject/auth.js"))
        .initialization_script(include_str!("../inject/custom.js"))
        .initialization_script(include_str!("../inject/titlebar.js"))
        .initialization_script(include_str!("../inject/youtube.js"));

    // Media playback support on WebView2:
    // - MediaSessionService and HardwareMediaKeyHandling are switched off on
    //   purpose. They publish the page's media session to Windows from the
    //   msedgewebview2.exe process, which has no application identity, so the
    //   media flyout reads "Unknown app" with no icon. `app/media.rs` publishes
    //   an owned session instead; leaving Chromium's on would mean two sessions
    //   competing for the same media keys.
    // - CalculateNativeWinOcclusion is disabled because Chromium treats a
    //   hidden or fully covered window as occluded and throttles it; for a tray
    //   app that is exactly when playback must keep running.
    // Chromium honours only the last `--enable-features` switch on a command
    // line, so every enabled feature has to be collected into one list.
    #[cfg(target_os = "windows")]
    let mut windows_enabled_features: Vec<&str> = Vec::new();

    // Overlay scrollbars: the thin idle line that widens under the pointer, the
    // same behaviour Edge has on Windows 11. Chromium already paints the
    // Fluent style by default; this is the switch that makes it an overlay, so
    // it also stops reserving a column of layout for itself. It is Chromium's
    // own chrome://flags/#overlay-scrollbars, not a Pake setting.
    #[cfg(target_os = "windows")]
    windows_enabled_features.push("OverlayScrollbar");

    #[cfg(target_os = "windows")]
    let mut windows_browser_args = String::from(
        "--disable-features=msWebOOUI,msPdfOOUI,msSmartScreenProtection,CalculateNativeWinOcclusion,MediaSessionService,HardwareMediaKeyHandling \
         --disable-blink-features=AutomationControlled \
         --autoplay-policy=no-user-gesture-required",
    );

    #[cfg(target_os = "linux")]
    let mut linux_browser_args = String::from("--disable-blink-features=AutomationControlled");

    if window_config.ignore_certificate_errors {
        #[cfg(target_os = "windows")]
        {
            windows_browser_args.push_str(" --ignore-certificate-errors");
        }

        #[cfg(target_os = "linux")]
        {
            linux_browser_args.push_str(" --ignore-certificate-errors");
        }
    }

    if window_config.enable_wasm {
        #[cfg(target_os = "windows")]
        {
            windows_enabled_features.push("SharedArrayBuffer");
            windows_browser_args.push_str(" --enable-unsafe-webgpu");
        }

        #[cfg(target_os = "linux")]
        {
            linux_browser_args.push_str(" --enable-features=SharedArrayBuffer");
            linux_browser_args.push_str(" --enable-unsafe-webgpu");
        }

        #[cfg(target_os = "macos")]
        {
            window_builder = window_builder
                .additional_browser_args("--enable-features=SharedArrayBuffer")
                .additional_browser_args("--enable-unsafe-webgpu");
        }
    }

    let mut parsed_proxy_url: Option<Url> = None;

    // Default to following the system theme (None), only force dark when explicitly set.
    // Computed once; the matching platform block below is the sole consumer.
    let theme = if window_config.dark_mode {
        Some(Theme::Dark)
    } else {
        None // Follow system theme
    };

    // Platform-specific configuration must be set before proxy on Windows/Linux
    #[cfg(target_os = "macos")]
    {
        let title_bar_style = if window_config.hide_title_bar {
            TitleBarStyle::Overlay
        } else {
            TitleBarStyle::Visible
        };
        window_builder = window_builder.title_bar_style(title_bar_style);
        window_builder = window_builder.theme(theme);

        // Tauri disables automatic tabbing unless an identifier is provided.
        // Existing multi-window apps already have a stable bundle identifier,
        // so use it without exposing another CLI/config option. Web-created
        // popups keep their own native window.
        if use_native_window_tabbing {
            window_builder = window_builder
                .tabbing_identifier(&tauri_config.identifier)
                .on_document_title_changed(|window, title| {
                    if !title.trim().is_empty() {
                        if let Err(error) = window.set_title(&title) {
                            eprintln!("[Pake] Failed to update the macOS tab title: {error}");
                        }
                    }
                });
        }
    }

    // Windows and Linux: set data_directory before proxy_url
    #[cfg(not(target_os = "macos"))]
    {
        window_builder = window_builder.data_directory(_data_dir).theme(theme);

        if window_config.hide_window_decorations {
            window_builder = window_builder.decorations(false);
        }

        if !config.proxy_url.is_empty() {
            if let Ok(proxy_url) = Url::from_str(&config.proxy_url) {
                parsed_proxy_url = Some(proxy_url.clone());
                #[cfg(target_os = "windows")]
                {
                    if let Some(arg) = build_proxy_browser_arg(&proxy_url) {
                        windows_browser_args.push(' ');
                        windows_browser_args.push_str(&arg);
                    }
                }
            }
        }

        #[cfg(target_os = "windows")]
        {
            if !windows_enabled_features.is_empty() {
                windows_browser_args.push_str(&format!(
                    " --enable-features={}",
                    windows_enabled_features.join(",")
                ));
            }
            window_builder = window_builder.additional_browser_args(&windows_browser_args);
        }

        #[cfg(target_os = "linux")]
        {
            window_builder = window_builder.additional_browser_args(&linux_browser_args);
        }
    }

    // Set proxy after platform-specific configs (required for Windows/Linux)
    if parsed_proxy_url.is_none() && !config.proxy_url.is_empty() {
        if let Ok(proxy_url) = Url::from_str(&config.proxy_url) {
            parsed_proxy_url = Some(proxy_url);
        }
    }

    if let Some(proxy_url) = parsed_proxy_url {
        window_builder = window_builder.proxy_url(proxy_url);
        #[cfg(debug_assertions)]
        println!("Proxy configured: {}", config.proxy_url);
    }

    if let Some(features) = new_window_features {
        #[cfg(target_os = "macos")]
        prepare_macos_new_window_configuration(&features)?;

        window_builder = window_builder.window_features(features).focused(true);
    }

    // Capture webview-initiated downloads (blob:, data:, Content-Disposition,
    // etc.) and write them to the OS Downloads folder. This is essential for
    // sites with a strict Content-Security-Policy (e.g. Gemini): their
    // `connect-src` blocks Tauri's IPC origin, so downloads cannot be routed
    // through the JS bridge, and downloads triggered from a sandboxed iframe
    // can't reach the IPC either. Letting the browser download natively and
    // catching it here is independent of the page CSP and the IPC channel.
    {
        let download_handle = app.clone();
        window_builder = window_builder.on_download(move |webview, event| match event {
            DownloadEvent::Requested { url, destination } => {
                match download_handle.path().download_dir() {
                    Ok(download_dir) => {
                        let filename = destination
                            .file_name()
                            .map(|name| name.to_string_lossy().to_string())
                            .filter(|name| !name.is_empty())
                            .or_else(|| {
                                url.path_segments()
                                    .and_then(|mut segments| segments.next_back())
                                    .map(|segment| segment.to_string())
                                    .filter(|segment| !segment.is_empty())
                            })
                            .unwrap_or_else(|| "download".to_string());

                        let target = download_dir.join(sanitize_download_filename(&filename));
                        if let Some(path_str) = target.to_str() {
                            *destination = PathBuf::from(check_file_or_append(path_str));
                        }
                    }
                    Err(error) => {
                        eprintln!("[Pake] Failed to resolve download dir: {error}");
                    }
                }
                true
            }
            DownloadEvent::Finished {
                url: _,
                path: _,
                success,
            } => {
                // Toast on the window that started the download (including
                // secondary multi-window labels), not a hard-coded "pake".
                let toast_window = download_handle
                    .get_webview_window(webview.label())
                    .or_else(|| download_handle.get_webview_window("pake"));
                if let Some(window) = toast_window {
                    let message_type = if success {
                        MessageType::Success
                    } else {
                        MessageType::Failure
                    };
                    show_toast(&window, &get_download_message_with_lang(message_type, None));
                }
                true
            }
            _ => true,
        });
    }

    window_builder = window_builder.on_navigation(|_| true);

    let window = window_builder.build()?;

    #[cfg(target_os = "windows")]
    disable_browser_accelerator_keys(&window);

    // A shared identifier alone leaves each NSWindow in automatic mode.
    // Prefer tabs only for Cmd+N clones so they join the main window's tab
    // group. The main window stays bar-less until another tab exists, while
    // web-auth and window.open popups remain separate native windows.
    #[cfg(target_os = "macos")]
    if prefer_native_window_tabbing {
        let tabbing_window = window.clone();
        Queue::main().exec_async(move || match tabbing_window.ns_window() {
            Ok(ns_window_ptr) => unsafe {
                let Some(ns_window) =
                    objc2::rc::Retained::retain(ns_window_ptr as *mut objc2_app_kit::NSWindow)
                else {
                    eprintln!("[Pake] Failed to retain the macOS window for tabbing.");
                    return;
                };
                ns_window.setTabbingMode(objc2_app_kit::NSWindowTabbingMode::Preferred);
            },
            Err(error) => {
                eprintln!("[Pake] Failed to access the macOS window for tabbing: {error}");
            }
        });
    }

    // WKWebView does not show an HTTP Basic login dialog and ignores Chromium's
    // certificate-error flag. Install one host-scoped delegate for both flows
    // on the process-lifetime main window, then navigate to the real target.
    #[cfg(target_os = "macos")]
    if let Some(target_url) = auth_target {
        let allowed_host = target_url
            .host_str()
            .expect("web URLs must have a host")
            .to_owned();
        let prompt_for_basic_auth = config.basic_auth;
        let allow_invalid_certificates = window_config.ignore_certificate_errors;
        let auth_window = window.clone();
        Queue::main().exec_async(move || {
            if let Err(error) = auth_window.with_webview(move |webview| {
                if !crate::app::auth::install_auth_delegate_and_navigate(
                    webview.inner(),
                    allowed_host,
                    target_url.to_string(),
                    prompt_for_basic_auth,
                    allow_invalid_certificates,
                ) {
                    eprintln!("[Pake] Failed to configure macOS authentication handling.");
                }
            }) {
                eprintln!("[Pake] Failed to access the macOS webview: {error}");
            }
        });
    }

    Ok(window)
}

#[cfg(all(test, target_os = "windows"))]
mod proxy_arg_tests {
    use super::*;

    fn parse(url: &str) -> Url {
        Url::from_str(url).unwrap()
    }

    #[test]
    fn http_url_with_explicit_port() {
        let arg = build_proxy_browser_arg(&parse("http://127.0.0.1:7890")).unwrap();
        assert_eq!(arg, "--proxy-server=http://127.0.0.1:7890");
    }

    #[test]
    fn http_url_uses_default_port_when_missing() {
        let arg = build_proxy_browser_arg(&parse("http://proxy.local")).unwrap();
        assert_eq!(arg, "--proxy-server=http://proxy.local:80");
    }

    #[test]
    fn socks5_url_uses_default_port_when_missing() {
        let arg = build_proxy_browser_arg(&parse("socks5://proxy.local")).unwrap();
        assert_eq!(arg, "--proxy-server=socks5://proxy.local:1080");
    }

    #[test]
    fn https_scheme_is_not_supported_yet() {
        // https proxies fall back to platform proxy_url; we only emit a CLI arg
        // for http/socks5 today.
        assert!(build_proxy_browser_arg(&parse("https://proxy.local:8443")).is_none());
    }
}
