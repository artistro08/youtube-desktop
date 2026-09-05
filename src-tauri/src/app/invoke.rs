use crate::app::navigation::{history_step, reload_window};
use crate::app::settings::{AppSettings, StoredSettings};
use crate::util::{
    check_file_or_append, get_download_message_with_lang, sanitize_download_filename, show_toast,
    truncate_page_text, MessageType,
};
use std::fs::File;
use std::io::Write;
use std::str::FromStr;
use std::sync::atomic::{AtomicI64, Ordering};
use tauri::http::Method;
use tauri::{command, AppHandle, Manager, Url, WebviewWindow};
use tauri_plugin_http::reqwest::header::{HeaderValue, COOKIE};
use tauri_plugin_http::reqwest::{ClientBuilder, Request};

use tauri::Theme;

static BADGE_COUNT: AtomicI64 = AtomicI64::new(0);
const MAX_BADGE_COUNT: i64 = 99_999;
const MAX_BADGE_LABEL_CHARS: usize = 16;

fn normalize_badge_count(count: Option<i64>) -> Option<i64> {
    count.filter(|n| (1..=MAX_BADGE_COUNT).contains(n))
}

fn normalize_badge_label(label: Option<&str>) -> Result<Option<String>, String> {
    let Some(label) = label.map(str::trim).filter(|label| !label.is_empty()) else {
        return Ok(None);
    };

    if label.chars().count() > MAX_BADGE_LABEL_CHARS {
        return Err(format!(
            "Badge label must be {MAX_BADGE_LABEL_CHARS} characters or fewer"
        ));
    }

    Ok(Some(label.to_string()))
}

fn apply_badge(app: &AppHandle, count: Option<i64>) -> Result<(), String> {
    let label = normalize_badge_count(count).map(|n| n.to_string());
    apply_badge_label(app, label.as_deref())
}

#[cfg(target_os = "macos")]
fn apply_badge_label(app: &AppHandle, label: Option<&str>) -> Result<(), String> {
    use objc2::MainThreadMarker;
    use objc2_app_kit::NSApplication;
    use objc2_foundation::NSString;

    let label = label.map(str::to_owned);
    app.run_on_main_thread(move || {
        let Some(mtm) = MainThreadMarker::new() else {
            return;
        };
        let dock_tile = NSApplication::sharedApplication(mtm).dockTile();
        let ns_label = label.as_deref().map(NSString::from_str);
        dock_tile.setBadgeLabel(ns_label.as_deref());
    })
    .map_err(|e| format!("Failed to dispatch dock badge update: {e}"))
}

#[cfg(not(target_os = "macos"))]
fn apply_badge_label(app: &AppHandle, label: Option<&str>) -> Result<(), String> {
    let window = app
        .get_webview_window("pake")
        .ok_or("Main window not found")?;
    let count = label.and_then(|s| s.parse::<i64>().ok());
    window
        .set_badge_count(count)
        .map_err(|e| format!("Failed to set badge count: {e}"))
}

#[derive(serde::Deserialize)]
pub struct DownloadFileParams {
    url: String,
    filename: String,
    language: Option<String>,
}

#[derive(serde::Deserialize)]
pub struct NotificationParams {
    title: String,
    body: String,
    icon: String,
}

#[derive(serde::Deserialize)]
pub struct YoutubeNotificationParams {
    title: String,
    body: String,
    url: String,
}

/// Build a Cookie header from the webview session so authenticated downloads
/// match what the page itself would request. Best-effort: missing cookies
/// fall through to an anonymous request.
fn cookie_header_for_url(window: &WebviewWindow, url: &Url) -> Option<HeaderValue> {
    let cookies = window.cookies_for_url(url.clone()).ok()?;
    if cookies.is_empty() {
        return None;
    }
    let header = cookies
        .iter()
        .map(|cookie| format!("{}={}", cookie.name(), cookie.value()))
        .collect::<Vec<_>>()
        .join("; ");
    HeaderValue::from_str(&header).ok()
}

#[command]
pub async fn download_file(
    window: WebviewWindow,
    app: AppHandle,
    params: DownloadFileParams,
) -> Result<(), String> {
    // Toast on the calling window (secondary windows included), not a hard-coded
    // main-window label. Tauri injects the invoker as `window`.
    show_toast(
        &window,
        &get_download_message_with_lang(MessageType::Start, params.language.clone()),
    );

    let download_dir = app
        .path()
        .download_dir()
        .map_err(|e| format!("Failed to get download dir: {}", e))?;

    let output_path = download_dir.join(sanitize_download_filename(&params.filename));

    let path_str = output_path.to_str().ok_or("Invalid output path")?;

    let file_path = check_file_or_append(path_str);

    let client = ClientBuilder::new()
        .build()
        .map_err(|e| format!("Failed to build client: {}", e))?;

    let url = Url::from_str(&params.url).map_err(|e| format!("Invalid URL: {}", e))?;

    let mut request = Request::new(Method::GET, url.clone());
    if let Some(cookie_header) = cookie_header_for_url(&window, &url) {
        request.headers_mut().insert(COOKIE, cookie_header);
    }

    let response = client.execute(request).await;

    match response {
        Ok(mut res) => {
            // Transport success is not download success: 403/404 HTML error pages
            // must not be written as files or toasted as successful downloads.
            if !res.status().is_success() {
                show_toast(
                    &window,
                    &get_download_message_with_lang(MessageType::Failure, params.language),
                );
                return Err(format!("Download failed with HTTP status {}", res.status()));
            }

            let mut file =
                File::create(&file_path).map_err(|e| format!("Failed to create file: {}", e))?;

            while let Some(chunk) = res
                .chunk()
                .await
                .map_err(|e| format!("Failed to get chunk: {}", e))?
            {
                file.write_all(&chunk)
                    .map_err(|e| format!("Failed to write chunk: {}", e))?;
            }

            show_toast(
                &window,
                &get_download_message_with_lang(MessageType::Success, params.language.clone()),
            );
            Ok(())
        }
        Err(e) => {
            show_toast(
                &window,
                &get_download_message_with_lang(MessageType::Failure, params.language),
            );
            Err(e.to_string())
        }
    }
}

#[command]
pub fn send_notification(app: AppHandle, params: NotificationParams) -> Result<(), String> {
    use tauri_plugin_notification::NotificationExt;
    app.notification()
        .builder()
        .title(&params.title)
        .body(&params.body)
        .icon(&params.icon)
        .show()
        .map_err(|e| format!("Failed to show notification: {}", e))?;
    Ok(())
}

/// Show a YouTube notification whose click reopens the app on that item.
///
/// Called by the injected notification poller, which runs in the remote page,
/// so every field is bounded here: text is truncated and the click target must
/// be a YouTube URL, or the toast would become a way for page script to send
/// the window anywhere.
#[command]
pub fn youtube_notify(app: AppHandle, params: YoutubeNotificationParams) -> Result<(), String> {
    use crate::app::youtube::{is_youtube_url, show_notification, IncomingNotification};

    let title = truncate_page_text(&params.title);
    if title.is_empty() {
        return Err("Notification title must not be empty".to_string());
    }

    let url = params.url.trim();
    if !url.is_empty() && !is_youtube_url(url) {
        return Err(format!(
            "Refusing to open non-YouTube URL from a notification: {url}"
        ));
    }

    show_notification(
        &app,
        IncomingNotification {
            title,
            body: truncate_page_text(&params.body),
            url: url.to_string(),
        },
    )
}

/// Remember the page the user is on so the next cold start reopens there.
///
/// The page reports its own location as it navigates, so the URL is checked
/// before it is stored: only a YouTube address may become the app's start page.
#[command]
pub fn save_last_url(app: AppHandle, url: String) -> Result<(), String> {
    // Resumable, not merely YouTube: the live chat is a youtube.com page, but
    // reopening on it strands the user in a chat panel with no way out.
    if !crate::app::youtube::is_resumable_url(&url) {
        return Err(format!("Refusing to remember {url} as the start page"));
    }

    if let Some(settings) = app.try_state::<AppSettings>() {
        // The page keeps reporting either way; with resuming switched off the
        // location is simply not written down.
        if settings.resume_enabled() {
            settings.set_last_url(url);
        }
    }

    Ok(())
}

/// Report what the page is playing so the Windows media flyout can mirror it.
///
/// A no-op on platforms without transport controls, and silently ignored before
/// the controls exist, because the page starts reporting as soon as it loads.
#[command]
#[cfg_attr(not(target_os = "windows"), allow(unused_variables))]
pub fn update_media_state(app: AppHandle, state: serde_json::Value) -> Result<(), String> {
    #[cfg(target_os = "windows")]
    {
        let state: crate::app::media::MediaState = serde_json::from_value(state)
            .map_err(|error| format!("Invalid media state: {error}"))?;

        if let Some(controls) = app.try_state::<crate::app::media::MediaControls>() {
            controls
                .update(state)
                .map_err(|error| format!("Failed to update media controls: {error}"))?;
        }
    }

    Ok(())
}

/// Current state of the settings the in-app settings dialog exposes.
#[command]
pub fn get_app_settings(app: AppHandle) -> StoredSettings {
    app.try_state::<AppSettings>()
        .map(|settings| settings.snapshot())
        .unwrap_or_default()
}

/// Report whether the page is showing a video the tray could pop out.
///
/// Called by the page whenever that changes, and it drives the tray menu's
/// picture-in-picture item on and off.
#[command]
pub fn set_video_available(app: AppHandle, available: bool) {
    crate::app::setup::set_video_available(&app, available);
}

/// The page has just opened a picture-in-picture window.
///
/// That window belongs to WebView2 rather than to this app, so it arrives
/// wearing the runtime's icon and on whichever virtual desktop it was opened
/// from; this is the app's cue to go and fix both. Reports nothing back: it is a
/// nudge, and every route into picture-in-picture — the title bar, the tray, the
/// page's own right-click menu — goes through the page event that calls it.
#[command]
#[cfg_attr(not(target_os = "windows"), allow(unused_variables))]
pub fn picture_in_picture_opened(app: AppHandle) {
    #[cfg(target_os = "windows")]
    {
        // Defaults to on, matching the stored setting, so a missing settings
        // record does not quietly change what the window does.
        let all_desktops = app
            .try_state::<AppSettings>()
            .is_none_or(|settings| settings.pip_all_desktops());

        crate::app::pip_window::brand_picture_in_picture_window(all_desktops);
    }
}

/// Look for a newer release because the user asked.
///
/// The automatic check runs on its own after startup; this is the settings
/// dialog's button, so it reports back either way rather than staying quiet when
/// the app is already current. Takes no input from the page — where to look and
/// which key to trust are both built into the binary.
#[command]
pub async fn check_for_updates(app: AppHandle) -> crate::app::update::UpdateStatus {
    crate::app::update::check(app, true).await
}

/// Flip one switch in the settings dialog.
///
/// One command for every switch rather than one apiece: they are the same
/// operation on the same record, and the name is validated against the known
/// set on the way in, so an unknown or malformed one from a compromised page is
/// an error rather than a silent no-op.
///
/// The tray is the exception that has to be named: turning it off removes a
/// native icon and brings the windows back, so it runs the tray's own routine
/// rather than only writing the flag.
#[command]
pub fn set_app_setting(app: AppHandle, key: String, enabled: bool) -> Result<(), String> {
    if key == "tray_enabled" {
        crate::app::setup::apply_tray_enabled(&app, enabled);
        return Ok(());
    }

    app.try_state::<AppSettings>()
        .ok_or("Settings are unavailable")?
        .set_flag(&key, enabled)
}

#[command]
pub fn set_dock_badge(app: AppHandle, count: Option<i64>) -> Result<(), String> {
    let normalized = normalize_badge_count(count);
    BADGE_COUNT.store(normalized.unwrap_or(0), Ordering::SeqCst);
    apply_badge(&app, normalized)
}

#[command]
pub fn increment_dock_badge(app: AppHandle) -> Result<(), String> {
    let current = BADGE_COUNT.load(Ordering::SeqCst);
    let next = current.saturating_add(1).clamp(1, MAX_BADGE_COUNT);
    BADGE_COUNT.store(next, Ordering::SeqCst);
    apply_badge(&app, Some(next))
}

#[command]
pub fn clear_dock_badge(app: AppHandle) -> Result<(), String> {
    BADGE_COUNT.store(0, Ordering::SeqCst);
    apply_badge(&app, None)
}

#[command]
pub fn set_dock_badge_label(app: AppHandle, label: Option<String>) -> Result<(), String> {
    BADGE_COUNT.store(0, Ordering::SeqCst);
    let label = normalize_badge_label(label.as_deref())?;
    apply_badge_label(&app, label.as_deref())
}

#[command]
pub async fn update_theme_mode(app: AppHandle, mode: String) {
    let theme = if mode == "dark" {
        Theme::Dark
    } else {
        Theme::Light
    };
    for window in app.webview_windows().values() {
        let _ = window.set_theme(Some(theme));
    }
}

// Apply native WebView zoom (WKWebView pageZoom / WebView2 ZoomFactor / WebKitGTK
// zoom level) instead of CSS hacks. CSS `transform: scale` and `html.style.zoom`
// break complex SPAs like ChatGPT (fixed positioning shifts, unrepainted layers);
// native zoom recalculates layout the same way a browser does for Cmd/Ctrl +/-.
#[command]
pub fn set_zoom(window: WebviewWindow, percent: f64) -> Result<(), String> {
    let factor = (percent / 100.0).clamp(0.3, 2.0);
    window
        .set_zoom(factor)
        .map_err(|e| format!("Failed to set zoom: {}", e))
}

/// Native navigation for injected shortcuts (Linux/Windows Ctrl+R / [ / ]).
/// Blank error pages have no JS context, so page `history` / `location` calls
/// are no-ops; these use the platform webview API instead.
#[command]
pub fn webview_navigate(window: WebviewWindow, action: String) -> Result<(), String> {
    match action.as_str() {
        "reload" => {
            reload_window(&window);
            Ok(())
        }
        "back" => {
            history_step(&window, true);
            Ok(())
        }
        "forward" => {
            history_step(&window, false);
            Ok(())
        }
        other => Err(format!(
            "Unknown webview_navigate action '{other}' (expected reload|back|forward)"
        )),
    }
}
