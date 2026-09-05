use crate::app::settings::AppSettings;
use crate::app::window::{
    eval_with_user_gesture, hide_all_app_windows, open_additional_window_safe,
    show_all_app_windows, toggle_all_app_windows,
};
use crate::cancel_startup_reveal;
use std::str::FromStr;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc, Mutex,
};
use std::time::{Duration, Instant};
use tauri::{
    menu::{Menu, MenuBuilder, MenuItemBuilder},
    tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent},
    AppHandle, Manager, Wry,
};
use tauri_plugin_global_shortcut::{GlobalShortcutExt, Shortcut};
use tauri_plugin_window_state::{AppHandleExt, StateFlags};

/// Shared by the config-declared tray, this builder, and every removal path.
const TRAY_ID: &str = "pake-tray";

/// What rebuilding the tray needs after startup has moved on.
///
/// The tray can be switched off and back on from the tray menu, the in-app
/// settings dialog, or `--tray`, and none of those callers are in a position to
/// carry the startup arguments around, so they are parked in managed state.
pub struct TrayRuntime {
    pub icon_path: String,
    pub init_fullscreen: bool,
    pub multi_window: bool,
    pub startup_revealed: Arc<AtomicBool>,
    /// Whether the page currently has a video the tray could pop out. Reported
    /// by the page, and the only thing the picture-in-picture item's presence
    /// in the menu depends on.
    pub video_available: Arc<AtomicBool>,
}

/// Turn the tray on or off and remember the choice.
///
/// Turning it off also brings the windows back: with no tray and
/// `hide_on_close` still in effect, a hidden window would leave the app running
/// with nothing left to click.
pub fn apply_tray_enabled(app: &AppHandle, enabled: bool) {
    if let Some(settings) = app.try_state::<AppSettings>() {
        settings.set_tray_enabled(enabled);
    }

    let Some(runtime) = app.try_state::<TrayRuntime>() else {
        eprintln!("[Pake] Tray runtime state is missing; the tray was not changed.");
        return;
    };

    if !enabled {
        show_all_app_windows(app, runtime.init_fullscreen);

        // Deferred so the tray icon is not dropped from inside its own
        // menu-event callback.
        let app_handle = app.clone();
        if let Err(error) = app.run_on_main_thread(move || {
            app_handle.remove_tray_by_id(TRAY_ID);
        }) {
            eprintln!("[Pake] Failed to remove the tray icon: {error}");
        }
        return;
    }

    if let Err(error) = set_system_tray(
        app,
        true,
        &runtime.icon_path,
        runtime.init_fullscreen,
        runtime.multi_window,
        runtime.startup_revealed.clone(),
        runtime.video_available.load(Ordering::SeqCst),
    ) {
        eprintln!("[Pake] Failed to restore the tray icon: {error}");
    }
}

/// Note whether the page has a video, and rebuild the tray menu when that
/// changes.
///
/// The picture-in-picture item is added and removed rather than greyed out:
/// Tauri's menu items can be disabled but not hidden, and a permanently greyed
/// row on every non-video page reads as something broken.
pub fn set_video_available(app: &AppHandle, available: bool) {
    let Some(runtime) = app.try_state::<TrayRuntime>() else {
        return;
    };

    if runtime.video_available.swap(available, Ordering::SeqCst) == available {
        return;
    }

    let multi_window = runtime.multi_window;
    let app_handle = app.clone();

    // Menus are native objects tied to the thread that owns the event loop.
    // Callers reach here from a command and from the tray's own menu callback,
    // and only one of those is guaranteed to be that thread already.
    if let Err(error) = app.run_on_main_thread(move || {
        let Some(tray) = app_handle.tray_by_id(TRAY_ID) else {
            return;
        };

        match build_tray_menu(&app_handle, multi_window, available) {
            Ok(menu) => {
                if let Err(error) = tray.set_menu(Some(menu)) {
                    eprintln!("[Pake] Failed to apply the rebuilt tray menu: {error}");
                }
            }
            Err(error) => eprintln!("[Pake] Failed to rebuild the tray menu: {error}"),
        }
    }) {
        eprintln!("[Pake] Failed to reach the main thread to rebuild the tray menu: {error}");
    }
}

/// Ask the page to pop the video it is showing out into a floating window.
///
/// Unlike the minimise and close paths this is an explicit request, so it takes
/// whatever video the page has rather than only a playing one, and Shorts are
/// included without the setting having to be on.
///
/// Chromium only grants the request off a real user gesture, and a tray click
/// happens outside the page, so this goes through the gesture-carrying eval.
fn request_picture_in_picture(app: &AppHandle) {
    let Some(window) = app.get_webview_window("pake") else {
        return;
    };

    eval_with_user_gesture(&window, "window.__pakeEnterPictureInPicture?.()");
}

/// The tray menu, built fresh so the picture-in-picture item can come and go.
fn build_tray_menu(
    app: &AppHandle,
    allow_multi_window: bool,
    video_available: bool,
) -> tauri::Result<Menu<Wry>> {
    // Menu events are broadcast to every handler in Tauri v2, so the tray item
    // must not share the "new_window" id with the app menu accelerator
    // (Cmd/Ctrl+N), or one click opens two windows.
    let new_window = MenuItemBuilder::with_id("tray_new_window", "New Window").build(app)?;
    let hide_app = MenuItemBuilder::with_id("hide_app", "Hide").build(app)?;
    let show_app = MenuItemBuilder::with_id("show_app", "Show").build(app)?;
    let picture_in_picture =
        MenuItemBuilder::with_id("tray_pip", "Picture-in-Picture").build(app)?;
    let disable_tray = MenuItemBuilder::with_id("disable_tray", "Disable Tray Icon").build(app)?;
    let quit = MenuItemBuilder::with_id("quit", "Quit").build(app)?;

    let mut builder = MenuBuilder::new(app);

    if allow_multi_window {
        builder = builder.item(&new_window);
    }

    builder = builder.items(&[&hide_app, &show_app]);

    if video_available {
        builder = builder.item(&picture_in_picture);
    }

    builder.separator().items(&[&disable_tray, &quit]).build()
}

pub fn set_system_tray(
    app: &AppHandle,
    show_system_tray: bool,
    tray_icon_path: &str,
    _init_fullscreen: bool,
    allow_multi_window: bool,
    startup_revealed: Arc<AtomicBool>,
    video_available: bool,
) -> tauri::Result<()> {
    if !show_system_tray {
        app.remove_tray_by_id(TRAY_ID);
        return Ok(());
    }

    let menu = build_tray_menu(app, allow_multi_window, video_available)?;

    app.app_handle().remove_tray_by_id(TRAY_ID);

    let menu_revealed = startup_revealed.clone();
    let click_revealed = startup_revealed;
    // The id must match the one the config declares and the one the removal
    // paths use. A builder without an explicit id gets a generated one, and
    // `remove_tray_by_id(TRAY_ID)` then only removes the tray Tauri creates
    // from `app.trayIcon`, leaving this one on screen forever.
    let mut tray_builder = TrayIconBuilder::with_id(TRAY_ID)
        .menu(&menu)
        // Left click toggles the windows (handled below); without this the menu
        // pops up on left click as well, which fights that toggle.
        .show_menu_on_left_click(false)
        .tooltip("YouTube")
        .on_menu_event(move |app, event| match event.id().as_ref() {
            "tray_new_window" => {
                open_additional_window_safe(app);
            }
            "hide_app" => {
                // Hide every webview (main + multi-window clones), not only "pake".
                cancel_startup_reveal(&menu_revealed);
                hide_all_app_windows(app);
            }
            "show_app" => {
                cancel_startup_reveal(&menu_revealed);
                show_all_app_windows(app, _init_fullscreen);
            }
            "tray_pip" => {
                request_picture_in_picture(app);
            }
            "disable_tray" => {
                cancel_startup_reveal(&menu_revealed);
                apply_tray_enabled(app, false);
            }
            "quit" => {
                // Matches the flags the plugin was built with, so quitting from
                // the tray does not write back state the app never restores.
                let flags = if _init_fullscreen {
                    StateFlags::all() & !StateFlags::DECORATIONS
                } else {
                    StateFlags::all() & !StateFlags::FULLSCREEN & !StateFlags::DECORATIONS
                };
                let _ = app.save_window_state(flags);
                app.exit(0);
            }
            _ => (),
        })
        .on_tray_icon_event(move |tray, event| {
            if let TrayIconEvent::Click {
                button,
                button_state,
                ..
            } = event
            {
                // Windows emits Click twice per physical click (Down then Up).
                // Reacting to both runs the toggle twice, so a hidden window is
                // shown and immediately re-hidden and the tray looks dead (#1343).
                if button == MouseButton::Left && button_state == MouseButtonState::Up {
                    // Any tray toggle claims visibility control from startup reveal.
                    cancel_startup_reveal(&click_revealed);
                    toggle_all_app_windows(tray.app_handle(), _init_fullscreen);
                }
            }
        });

    let resolved_icon = if tray_icon_path.is_empty() {
        app.default_window_icon().cloned()
    } else {
        tauri::image::Image::from_path(tray_icon_path)
            .ok()
            .or_else(|| app.default_window_icon().cloned())
    };

    if let Some(icon) = resolved_icon {
        tray_builder = tray_builder.icon(icon);
    } else {
        eprintln!("[Pake] No tray icon available; tray will build without an icon.");
    }

    let tray = tray_builder.build(app)?;

    tray.set_icon_as_template(false)?;
    Ok(())
}

pub fn set_global_shortcut(
    app: &AppHandle,
    shortcut: String,
    _init_fullscreen: bool,
    startup_revealed: Arc<AtomicBool>,
) -> tauri::Result<()> {
    if shortcut.is_empty() {
        return Ok(());
    }

    let app_handle = app.clone();
    let shortcut_hotkey = match Shortcut::from_str(&shortcut) {
        Ok(s) => s,
        Err(error) => {
            eprintln!("[Pake] Invalid activation shortcut '{shortcut}': {error}");
            return Ok(());
        }
    };
    let last_triggered = Arc::new(Mutex::new(Instant::now()));

    if let Err(error) = app_handle.plugin(
        tauri_plugin_global_shortcut::Builder::new()
            .with_handler({
                let last_triggered = Arc::clone(&last_triggered);
                let startup_revealed = startup_revealed.clone();
                move |app, event, _shortcut| {
                    let Ok(mut last_triggered) = last_triggered.lock() else {
                        return;
                    };
                    if Instant::now().duration_since(*last_triggered) < Duration::from_millis(300) {
                        return;
                    }
                    *last_triggered = Instant::now();

                    if shortcut_hotkey.eq(event) {
                        cancel_startup_reveal(&startup_revealed);
                        toggle_all_app_windows(app, _init_fullscreen);
                    }
                }
            })
            .build(),
    ) {
        eprintln!(
            "[Pake] Failed to register global shortcut plugin '{shortcut}': {error}; continuing without it."
        );
        return Ok(());
    }

    if let Err(error) = app.global_shortcut().register(shortcut_hotkey) {
        eprintln!("[Pake] Failed to bind global shortcut '{shortcut}': {error}");
    }

    Ok(())
}
