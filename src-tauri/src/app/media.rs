//! Windows System Media Transport Controls integration.
//!
//! WebView2 can publish the page's media session to Windows by itself, but that
//! session belongs to the msedgewebview2.exe process, which carries no
//! application identity: the media flyout shows "Unknown app" with no icon and
//! no way for Windows to resolve one. So the app publishes its own session
//! instead, from a process that does have an AppUserModelID, and forwards the
//! transport buttons back into the page.

use crate::util::truncate_page_text;
use serde::Deserialize;
use std::sync::Mutex;
use tauri::{AppHandle, Manager, WebviewWindow};
use windows::core::HSTRING;
use windows::Foundation::{TypedEventHandler, Uri};
use windows::Media::{
    MediaPlaybackStatus, MediaPlaybackType, SystemMediaTransportControls,
    SystemMediaTransportControlsButton, SystemMediaTransportControlsButtonPressedEventArgs,
};
use windows::Storage::Streams::RandomAccessStreamReference;
use windows::Win32::Foundation::HWND;
use windows::Win32::System::WinRT::ISystemMediaTransportControlsInterop;

/// What the page reports about whatever it is currently playing.
#[derive(Clone, Debug, Default, Deserialize, PartialEq, Eq)]
pub struct MediaState {
    pub has_media: bool,
    pub playing: bool,
    pub title: String,
    pub artist: String,
    pub artwork: String,
}

impl MediaState {
    /// Page-supplied strings are bounded, and the artwork has to be a real
    /// https URL before it is handed to the thumbnail loader.
    fn sanitized(mut self) -> Self {
        self.title = truncate_page_text(&self.title);
        self.artist = truncate_page_text(&self.artist);
        // Windows fetches this URL to draw the flyout thumbnail, so it is held
        // to the hosts YouTube actually serves artwork from rather than to any
        // https address the page cares to name.
        self.artwork = if is_youtube_artwork(&self.artwork) {
            truncate_page_text(&self.artwork)
        } else {
            String::new()
        };
        self
    }

    fn playback_status(&self) -> MediaPlaybackStatus {
        if !self.has_media {
            MediaPlaybackStatus::Closed
        } else if self.playing {
            MediaPlaybackStatus::Playing
        } else {
            MediaPlaybackStatus::Paused
        }
    }

    /// The display updater is comparatively expensive and makes the flyout
    /// flicker, so it only runs when the metadata itself changed.
    fn describes_same_item(&self, other: &Self) -> bool {
        self.title == other.title && self.artist == other.artist && self.artwork == other.artwork
    }
}

/// The hosts YouTube serves thumbnails and channel avatars from.
fn is_youtube_artwork(url: &str) -> bool {
    let Ok(parsed) = tauri::Url::parse(url) else {
        return false;
    };
    if parsed.scheme() != "https" {
        return false;
    }
    let Some(host) = parsed.host_str() else {
        return false;
    };

    ["ytimg.com", "ggpht.com", "googleusercontent.com"]
        .iter()
        .any(|domain| host == *domain || host.ends_with(&format!(".{domain}")))
}

pub struct MediaControls {
    controls: SystemMediaTransportControls,
    last_state: Mutex<MediaState>,
}

// SystemMediaTransportControls is an agile WinRT object, so it can be reached
// from the IPC threads that deliver player updates.
unsafe impl Send for MediaControls {}
unsafe impl Sync for MediaControls {}

impl MediaControls {
    /// Attach a transport-control session to the main window and wire its
    /// buttons back into the page.
    pub fn new(window: &WebviewWindow) -> windows::core::Result<Self> {
        let hwnd = HWND(
            window
                .hwnd()
                .map_err(|error| {
                    windows::core::Error::new(
                        windows::Win32::Foundation::E_HANDLE,
                        format!("No window handle for media controls: {error}"),
                    )
                })?
                .0,
        );

        let interop = windows::core::factory::<
            SystemMediaTransportControls,
            ISystemMediaTransportControlsInterop,
        >()?;
        let controls: SystemMediaTransportControls = unsafe { interop.GetForWindow(hwnd)? };

        controls.SetIsEnabled(true)?;
        controls.SetIsPlayEnabled(true)?;
        controls.SetIsPauseEnabled(true)?;
        controls.SetIsNextEnabled(true)?;
        controls.SetIsPreviousEnabled(true)?;
        controls.SetIsStopEnabled(false)?;
        controls.SetPlaybackStatus(MediaPlaybackStatus::Closed)?;

        let app = window.app_handle().clone();
        controls.ButtonPressed(&TypedEventHandler::<
            SystemMediaTransportControls,
            SystemMediaTransportControlsButtonPressedEventArgs,
        >::new(move |_sender, args| {
            let Some(args) = args.as_ref() else {
                return Ok(());
            };
            if let Some(action) = button_action(args.Button()?) {
                dispatch_to_page(&app, action);
            }
            Ok(())
        }))?;

        Ok(Self {
            controls,
            last_state: Mutex::new(MediaState::default()),
        })
    }

    /// Push the page's current playback state onto the Windows session.
    pub fn update(&self, state: MediaState) -> windows::core::Result<()> {
        let state = state.sanitized();

        let Ok(mut last_state) = self.last_state.lock() else {
            return Ok(());
        };
        if *last_state == state {
            return Ok(());
        }

        self.controls.SetPlaybackStatus(state.playback_status())?;

        if !state.describes_same_item(&last_state) {
            let updater = self.controls.DisplayUpdater()?;
            updater.SetType(MediaPlaybackType::Music)?;

            let music = updater.MusicProperties()?;
            music.SetTitle(&HSTRING::from(&state.title))?;
            music.SetArtist(&HSTRING::from(&state.artist))?;

            if state.artwork.is_empty() {
                updater.SetThumbnail(None)?;
            } else if let Ok(uri) = Uri::CreateUri(&HSTRING::from(&state.artwork)) {
                updater.SetThumbnail(&RandomAccessStreamReference::CreateFromUri(&uri)?)?;
            }

            updater.Update()?;
        }

        *last_state = state;
        Ok(())
    }
}

fn button_action(button: SystemMediaTransportControlsButton) -> Option<&'static str> {
    match button {
        SystemMediaTransportControlsButton::Play => Some("play"),
        SystemMediaTransportControlsButton::Pause => Some("pause"),
        SystemMediaTransportControlsButton::Stop => Some("pause"),
        SystemMediaTransportControlsButton::Next => Some("next"),
        SystemMediaTransportControlsButton::Previous => Some("previous"),
        _ => None,
    }
}

/// Hand a transport button to the player. The action names are a fixed set, so
/// nothing user- or page-supplied is ever formatted into the script.
fn dispatch_to_page(app: &AppHandle, action: &str) {
    let Some(window) = app.get_webview_window("pake") else {
        return;
    };
    let script = format!("window.__pakeMediaCommand && window.__pakeMediaCommand('{action}')");
    if let Err(error) = window.eval(&script) {
        eprintln!("[Pake] Failed to forward the '{action}' media button: {error}");
    }
}

/// Give the process the identity Windows needs to name and illustrate the
/// media session. Without it the flyout falls back to the executable name and
/// shows "Unknown app"; with it, Windows resolves the Start Menu shortcut that
/// carries the same id and uses the app's name and icon.
pub fn set_app_user_model_id(identifier: &str) {
    use windows_sys::Win32::UI::Shell::SetCurrentProcessExplicitAppUserModelID;

    let wide: Vec<u16> = identifier
        .encode_utf16()
        .chain(std::iter::once(0))
        .collect();

    let result = unsafe { SetCurrentProcessExplicitAppUserModelID(wide.as_ptr()) };
    if result != 0 {
        eprintln!("[Pake] Failed to set the app user model id (HRESULT {result:#x}).");
    }
}
