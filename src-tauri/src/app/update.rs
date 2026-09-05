//! Keeping the app up to date from its GitHub releases.
//!
//! Each release carries a `latest.json` naming the newest version and the MSI
//! that installs it, and a signature made with a key only the maintainer holds.
//! The app reads that file, and if it names a version newer than this one, it
//! downloads the installer, checks the signature against the public key built
//! into this binary, and runs it. An installer that fails that check is never
//! run — which is the point of the whole arrangement, because an app that
//! downloads and executes code from the internet is only as trustworthy as its
//! ability to say no.
//!
//! Nothing here is driven from the page. The remote YouTube page is untrusted
//! and has no permission to reach the updater, so the check runs on a timer
//! after startup and from the settings dialog, both through this app's own code.
//!
//! Windows closes the app to install an MSI — the installer cannot replace files
//! that are open — so the user is asked first, every time, rather than having
//! their video interrupted by an update they did not ask for.

use crate::util::show_toast;
use tauri::{AppHandle, Manager};
use tauri_plugin_updater::UpdaterExt;

/// How long after startup the automatic check runs.
///
/// Late enough that it is not competing with the first page load, which is what
/// the user is actually waiting for.
const STARTUP_DELAY: std::time::Duration = std::time::Duration::from_secs(20);

/// What a check found, for the settings dialog to show.
#[derive(Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdateStatus {
    /// Whether an update is available.
    pub available: bool,
    /// The version on offer, when there is one.
    pub version: Option<String>,
    /// This build's version, so the dialog can say what is installed.
    pub current_version: String,
    /// Why the check could not be completed, when it could not be.
    pub error: Option<String>,
}

/// Look for a newer release, and install one if the user agrees.
///
/// `announce` distinguishes the two callers: the automatic check after startup
/// says nothing when the app is already current, because an unprompted "you are
/// up to date" is noise, while the settings dialog's button has to answer one
/// way or the other or it looks broken.
pub async fn check(app: AppHandle, announce: bool) -> UpdateStatus {
    let current_version = app.package_info().version.to_string();

    let updater = match app.updater() {
        Ok(updater) => updater,
        Err(error) => {
            return failed(current_version, format!("Updates are unavailable: {error}"));
        }
    };

    let found = match updater.check().await {
        Ok(found) => found,
        Err(error) => {
            // Being offline is the ordinary case here, not a fault worth
            // interrupting anyone over.
            eprintln!("[Pake] Could not check for updates: {error}");
            return failed(
                current_version,
                format!("Could not check for updates: {error}"),
            );
        }
    };

    let Some(update) = found else {
        if announce {
            announce_current(&app, &current_version);
        }

        return UpdateStatus {
            available: false,
            version: None,
            current_version,
            error: None,
        };
    };

    let version = update.version.clone();

    // The download and install happen behind the user's answer, not in front of
    // it: installing closes the app, and closing the app in the middle of a
    // video because a release happened to land is not an update, it is an
    // interruption.
    if confirm(&app, &version, &current_version) {
        install(app.clone(), update).await;
    }

    UpdateStatus {
        available: true,
        version: Some(version),
        current_version,
        error: None,
    }
}

/// Download, verify and run the installer.
///
/// The signature is checked by the plugin against the public key compiled into
/// this binary before anything is executed; a download that fails that check
/// returns an error here and is discarded.
async fn install(app: AppHandle, update: tauri_plugin_updater::Update) {
    let version = update.version.clone();

    let downloaded = update
        .download_and_install(
            // Progress is deliberately not reported anywhere. The MSI is a few
            // megabytes, the app is usually in the tray while this runs, and a
            // progress bar would need a window of its own to live in.
            |_chunk, _total| {},
            || {},
        )
        .await;

    match downloaded {
        // Not reached in the normal case: Windows exits the app to run the
        // installer, so this returning at all means the installer declined to
        // start.
        Ok(()) => eprintln!("[Pake] Update {version} was installed."),
        Err(error) => {
            eprintln!("[Pake] Update {version} could not be installed: {error}");
            notify(
                &app,
                "Update failed",
                &format!("Version {version} could not be installed: {error}"),
            );
        }
    }
}

/// Ask whether to install now, and say what installing entails.
///
/// A blocking dialog, on a thread of its own so the async runtime is not held
/// while it is up.
fn confirm(app: &AppHandle, version: &str, current_version: &str) -> bool {
    use tauri_plugin_dialog::{DialogExt, MessageDialogButtons, MessageDialogKind};

    app.dialog()
        .message(format!(
            "YouTube {version} is available. You have {current_version}.\n\n\
             Installing closes the app and reopens it when it is done."
        ))
        .title("Update available")
        .kind(MessageDialogKind::Info)
        .buttons(MessageDialogButtons::OkCancelCustom(
            "Install now".to_string(),
            "Later".to_string(),
        ))
        .blocking_show()
}

/// Tell the page the app is current, when the user asked outright.
fn announce_current(app: &AppHandle, current_version: &str) {
    let Some(window) = app.get_webview_window("pake") else {
        return;
    };

    show_toast(
        &window,
        &format!("YouTube {current_version} is the latest version."),
    );
}

/// A Windows notification, for the paths with no window to toast in.
fn notify(app: &AppHandle, title: &str, body: &str) {
    use tauri_plugin_notification::NotificationExt;

    if let Err(error) = app.notification().builder().title(title).body(body).show() {
        eprintln!("[Pake] Could not show the update notification: {error}");
    }
}

/// A check that could not be completed.
fn failed(current_version: String, error: String) -> UpdateStatus {
    UpdateStatus {
        available: false,
        version: None,
        current_version,
        error: Some(error),
    }
}

/// Start the one automatic check, a little after the app opens.
pub fn check_after_startup(app: &AppHandle) {
    let app = app.clone();

    tauri::async_runtime::spawn(async move {
        tokio::time::sleep(STARTUP_DELAY).await;
        // Silent when already current: nobody asked.
        check(app, false).await;
    });
}
