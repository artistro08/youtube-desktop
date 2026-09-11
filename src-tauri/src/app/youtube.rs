//! YouTube-specific integration: link routing, deep links, and the native
//! notifications the injected poller feeds.
//!
//! Links reach the app through the `youtube://` scheme it owns, which Windows
//! delivers as a command-line argument. A whole web address maps onto the
//! scheme by swapping the protocol and keeping everything else, so
//! `https://www.youtube.com/watch?v=ID` becomes
//! `youtube://www.youtube.com/watch?v=ID`. Anything that arrives and is not a
//! YouTube link goes to the system browser rather than opening here.

use tauri::{AppHandle, Manager, Url, WebviewWindow};

const DEEP_LINK_SCHEME: &str = "youtube";
const HOME_URL: &str = "https://www.youtube.com";

/// Hosts that belong to the app itself.
///
/// Deliberately narrower than the `internal_url_regex` in `pake.json`: that one
/// also has to cover the Google sign-in and payment hosts a session walks
/// through, whereas this decides what an incoming *link* is allowed to be, and
/// a link into an account page is not something a web page should be able to
/// hand this app.
const YOUTUBE_HOSTS: [&str; 4] = [
    "youtube.com",
    "youtu.be",
    "youtube-nocookie.com",
    "youtubekids.com",
];

/// True when a URL should be shown in the app rather than a browser.
pub fn is_youtube_url(url: &str) -> bool {
    let Ok(parsed) = Url::parse(url) else {
        return false;
    };
    if !matches!(parsed.scheme(), "http" | "https") {
        return false;
    }
    let Some(host) = parsed.host_str() else {
        return false;
    };
    let host = host.trim_end_matches('.').to_ascii_lowercase();

    YOUTUBE_HOSTS
        .iter()
        .any(|domain| host == *domain || host.ends_with(&format!(".{domain}")))
}

/// Pages that only make sense inside another page.
///
/// The live chat is the one that bites: it is an iframe on the watch page, and
/// reopening the app on it lands the user in a bare chat panel with no
/// masthead, no player and no way back to YouTube.
const POPOUT_PATHS: [&str; 3] = ["/live_chat", "/live_chat_replay", "/embed/"];

/// True when a URL is a whole page a session can sensibly be resumed on.
pub fn is_resumable_url(url: &str) -> bool {
    if !is_youtube_url(url) {
        return false;
    }

    let Ok(parsed) = Url::parse(url) else {
        return false;
    };
    let path = parsed.path();

    !POPOUT_PATHS
        .iter()
        .any(|popout| path == popout.trim_end_matches('/') || path.starts_with(popout))
}

/// Normalize whatever a launcher handed us into an https URL.
///
/// `youtube://watch?v=ID`, `youtube://www.youtube.com/watch?v=ID`, and a plain
/// `https://youtu.be/ID` all have to end up as the same navigable address.
///
/// A link arriving through the scheme can only ever come back out as a YouTube
/// address. Any web page can publish a `youtube://` link, and Windows shows the
/// consent prompt in this app's name, so a scheme link that named some other
/// host would let a page launder an arbitrary address through a dialog that
/// says YouTube on it. Whatever is not recognizable as a YouTube host is
/// treated as a path on YouTube instead, which is also what makes the short
/// `youtube://watch?v=ID` form work.
pub fn normalize_incoming_url(raw: &str) -> Option<String> {
    let candidate = raw.trim();
    if candidate.is_empty() {
        return None;
    }

    let Some(rest) = candidate
        .strip_prefix(&format!("{DEEP_LINK_SCHEME}://"))
        .or_else(|| candidate.strip_prefix(&format!("{DEEP_LINK_SCHEME}:")))
    else {
        // Not a deep link: accept only real web URLs.
        let parsed = Url::parse(candidate).ok()?;
        return matches!(parsed.scheme(), "http" | "https").then(|| parsed.to_string());
    };

    let rest = rest.trim_start_matches('/');
    if rest.is_empty() {
        return Some(HOME_URL.to_string());
    }

    // `youtube://youtu.be/ID` carries its own host; anything else is a path
    // relative to the YouTube home page. Decided by parsing the candidate and
    // asking whether its host really is YouTube's — a prefix test would accept
    // `youtu.be.attacker.com` as the host `youtu.be`.
    let rebuilt = match Url::parse(&format!("https://{rest}")) {
        Ok(candidate) if is_youtube_url(candidate.as_str()) => candidate.to_string(),
        _ => format!("{HOME_URL}/{rest}"),
    };

    // Belt and braces: the branch above cannot produce anything else, and the
    // callers rely on that to know a scheme link never reaches the browser.
    let parsed = Url::parse(&rebuilt).ok()?;
    is_youtube_url(parsed.as_str()).then(|| parsed.to_string())
}

/// Pull the first launch argument that looks like a URL.
///
/// Windows appends the clicked URL to the registered handler's command line;
/// everything else on the command line (flags, the executable path) is ignored.
pub fn url_from_args<I, S>(args: I) -> Option<String>
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    args.into_iter()
        .skip(1)
        .find_map(|arg| normalize_incoming_url(arg.as_ref()))
}

/// Show the main window and point it at `url`.
pub fn navigate_main_window(app: &AppHandle, url: &str) {
    let Some(window) = app.get_webview_window("pake") else {
        return;
    };
    let Ok(parsed) = Url::parse(url) else {
        return;
    };

    crate::app::window::show_all_app_windows(app, false);
    if let Err(error) = window.navigate(parsed) {
        eprintln!("[Pake] Failed to navigate to {url}: {error}");
    }
}

/// Apply a link the app was launched with to a window that has not been
/// revealed yet.
///
/// Unlike `handle_incoming_url` this never shows the window: startup reveal is
/// driven by the first finished page load, and forcing the window open here
/// would bring back the blank-shell flash that gate exists to prevent.
pub fn apply_launch_url(app: &AppHandle, window: &WebviewWindow, raw: &str) {
    let Some(url) = normalize_incoming_url(raw) else {
        return;
    };

    if !is_youtube_url(&url) {
        open_in_external_browser(app, &url);
        return;
    }

    let Ok(parsed) = Url::parse(&url) else {
        return;
    };
    if let Err(error) = window.navigate(parsed) {
        eprintln!("[Pake] Failed to open the launch URL {url}: {error}");
    }
}

/// Route an incoming link: YouTube stays here, everything else goes out to a
/// browser. Never fall back to the system default handler blindly, because
/// this app may be the system default handler.
pub fn handle_incoming_url(app: &AppHandle, raw: &str) {
    let Some(url) = normalize_incoming_url(raw) else {
        return;
    };

    if is_youtube_url(&url) {
        navigate_main_window(app, &url);
        return;
    }

    open_in_external_browser(app, &url);
}

/// Open a non-YouTube link in the system's default browser.
///
/// Safe to hand straight to the default handler: the app registers only the
/// `youtube://` scheme, so it can never be that handler itself.
pub fn open_in_external_browser(app: &AppHandle, url: &str) {
    use tauri_plugin_opener::OpenerExt;
    if let Err(error) = app.opener().open_url(url, None::<&str>) {
        eprintln!("[Pake] Failed to open {url} externally: {error}");
    }
}

/// A notification produced by the injected YouTube poller.
pub struct IncomingNotification {
    pub title: String,
    pub body: String,
    pub url: String,
}

/// The link a toast click opens: the notified page on the `youtube://` scheme
/// this app owns, or the bare scheme (the home page) when there is no page.
///
/// A scheme link rather than an in-process callback because the callback dies
/// with the process. Toasts outlive the app in the notification centre, and a
/// click there has to open the page whether the app is still running or not.
/// Windows launches the scheme's handler, which is this app, and the launch
/// path already knows how to hand a link to a running instance or start on it.
fn toast_launch_url(url: &str) -> String {
    match url.split_once("://") {
        Some((_, rest)) if !rest.is_empty() => format!("{DEEP_LINK_SCHEME}://{rest}"),
        _ => format!("{DEEP_LINK_SCHEME}://"),
    }
}

/// Text made safe to sit inside toast XML, in an attribute or an element.
fn escape_xml(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

/// Show a native notification whose click opens the app on the notified page.
///
/// The Windows toast is built directly instead of going through the
/// notification plugin because only the raw toast XML can carry the launch
/// link, and "notifications open in the app" is the whole point here.
#[cfg(target_os = "windows")]
pub fn show_notification(
    app: &AppHandle,
    notification: IncomingNotification,
) -> Result<(), String> {
    use windows::{
        core::HSTRING,
        Data::Xml::Dom::XmlDocument,
        UI::Notifications::{ToastNotification, ToastNotificationManager},
    };

    let icon = app
        .path()
        .resolve("png/youtube_512.png", tauri::path::BaseDirectory::Resource)
        .ok()
        .filter(|path| path.exists())
        .map(|path| {
            format!(
                r#"<image placement="appLogoOverride" src="file:///{}" alt="YouTube"/>"#,
                escape_xml(&path.display().to_string())
            )
        })
        .unwrap_or_default();

    let xml = format!(
        r#"<toast duration="short" activationType="protocol" launch="{}">
            <visual>
                <binding template="ToastGeneric">
                    {icon}
                    <text>{}</text>
                    <text>{}</text>
                </binding>
            </visual>
        </toast>"#,
        escape_xml(&toast_launch_url(&notification.url)),
        escape_xml(&notification.title),
        escape_xml(&notification.body),
    );

    let describe = |error: windows::core::Error| format!("Failed to show notification: {error}");

    let document = XmlDocument::new().map_err(describe)?;
    document.LoadXml(&HSTRING::from(xml)).map_err(describe)?;
    let toast = ToastNotification::CreateToastNotification(&document).map_err(describe)?;

    // The app id must be one an installed Start Menu shortcut carries, or
    // Windows drops the toast without a word. The installer writes that
    // shortcut; a build run straight from `cargo` has none and notifies nobody.
    let app_id = HSTRING::from(app.config().identifier.as_str());
    ToastNotificationManager::CreateToastNotifierWithId(&app_id)
        .map_err(describe)?
        .Show(&toast)
        .map_err(describe)
}

/// Non-Windows fallback: the notification plugin cannot carry a launch link,
/// so the click cannot be routed. The toast itself still works.
#[cfg(not(target_os = "windows"))]
pub fn show_notification(
    app: &AppHandle,
    notification: IncomingNotification,
) -> Result<(), String> {
    use tauri_plugin_notification::NotificationExt;

    app.notification()
        .builder()
        .title(&notification.title)
        .body(&notification.body)
        .show()
        .map_err(|error| format!("Failed to show notification: {error}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recognizes_youtube_hosts() {
        for url in [
            "https://www.youtube.com/watch?v=abc",
            "https://youtu.be/abc",
            "http://music.youtube.com/",
            "https://www.youtube-nocookie.com/embed/abc",
        ] {
            assert!(is_youtube_url(url), "{url} should be internal");
        }
    }

    #[test]
    fn rejects_lookalike_and_non_web_urls() {
        for url in [
            "https://notyoutube.com/watch",
            "https://youtube.com.evil.test/watch",
            "file:///c:/youtube.com",
            "not a url",
        ] {
            assert!(!is_youtube_url(url), "{url} should be external");
        }
    }

    #[test]
    fn normalizes_deep_links_to_https() {
        assert_eq!(
            normalize_incoming_url("youtube://watch?v=abc").unwrap(),
            "https://www.youtube.com/watch?v=abc"
        );
        assert_eq!(
            normalize_incoming_url("youtube://youtu.be/abc").unwrap(),
            "https://youtu.be/abc"
        );
        // A whole web address with its protocol swapped for the scheme.
        assert_eq!(
            normalize_incoming_url("youtube://youtube.com/watch?v=CFutHcNqepk").unwrap(),
            "https://youtube.com/watch?v=CFutHcNqepk"
        );
        assert_eq!(
            normalize_incoming_url("youtube://www.youtube.com/watch?v=CFutHcNqepk").unwrap(),
            "https://www.youtube.com/watch?v=CFutHcNqepk"
        );
        assert_eq!(
            normalize_incoming_url("youtube://").unwrap(),
            "https://www.youtube.com"
        );
    }

    /// Any web page can publish a `youtube://` link, so the scheme must not be
    /// usable to name a host of the attacker's choosing: Windows shows the
    /// consent prompt in this app's name, and the result would otherwise be
    /// handed to the default browser.
    #[test]
    fn deep_links_cannot_name_a_foreign_host() {
        for raw in [
            "youtube://youtu.be.attacker.test/phish",
            "youtube://youtube.com.evil.test/watch",
            "youtube://attacker.test/phish",
            "youtube://user:pass@attacker.test/",
        ] {
            let normalized =
                normalize_incoming_url(raw).unwrap_or_else(|| panic!("{raw} should normalize"));

            assert!(
                is_youtube_url(&normalized),
                "{raw} became {normalized}, which would be sent to the browser"
            );
        }
    }

    /// The live chat is a youtube.com page, but reopening the app on it lands
    /// the user in a bare chat panel with no player and no way back.
    #[test]
    fn popout_pages_are_not_resumable() {
        for url in [
            "https://www.youtube.com/live_chat?v=Op60Oq1pXqU",
            "https://www.youtube.com/live_chat_replay?continuation=abc",
            "https://www.youtube.com/embed/CFutHcNqepk",
        ] {
            assert!(is_youtube_url(url), "{url} is still a YouTube page");
            assert!(
                !is_resumable_url(url),
                "{url} must not become the start page"
            );
        }

        for url in [
            "https://www.youtube.com/watch?v=Op60Oq1pXqU",
            "https://www.youtube.com/",
            "https://www.youtube.com/feed/subscriptions",
        ] {
            assert!(is_resumable_url(url), "{url} should be resumable");
        }
    }

    /// A toast click launches the page on the app's own scheme, so the link
    /// must round-trip through `normalize_incoming_url` back to the same page.
    #[test]
    fn toast_launch_links_round_trip_through_the_scheme() {
        let page = "https://www.youtube.com/watch?v=abc&lc=Ugw123";
        let launch = toast_launch_url(page);
        assert_eq!(launch, "youtube://www.youtube.com/watch?v=abc&lc=Ugw123");
        assert_eq!(normalize_incoming_url(&launch).unwrap(), page);

        // No page: the bare scheme opens the app on the home page.
        assert_eq!(
            normalize_incoming_url(&toast_launch_url("")).unwrap(),
            HOME_URL
        );
    }

    #[test]
    fn xml_escaping_covers_every_special_character() {
        assert_eq!(
            escape_xml(r#"a&b<c>d"e'f"#),
            "a&amp;b&lt;c&gt;d&quot;e&apos;f"
        );
    }

    #[test]
    fn ignores_arguments_that_are_not_urls() {
        assert_eq!(url_from_args(["app.exe", "--tray"]), None);
        assert_eq!(
            url_from_args(["app.exe", "--tray", "https://youtu.be/abc"]).unwrap(),
            "https://youtu.be/abc"
        );
        // The executable path itself must never be treated as a link.
        assert_eq!(url_from_args(["https://youtu.be/abc"]), None);
    }
}
