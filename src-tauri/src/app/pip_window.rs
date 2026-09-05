//! Tidying up after the picture-in-picture window.
//!
//! The floating window belongs to WebView2, not to this app: Chromium creates
//! it in the `msedgewebview2.exe` browser process, so it arrives on the taskbar
//! wearing the WebView2 runtime's icon next to the YouTube one. There is no
//! WebView2 API for this, and the page cannot reach the window either — but it
//! is still a window owned by a child of this process, and `WM_SETICON` is the
//! ordinary way to tell Windows what a window's icon is.
//!
//! The window is found rather than handed over: enumerate the visible top-level
//! windows, keep the ones belonging to a descendant of this process, and skip
//! this process's own. What is left is the floating window.
//!
//! Its settings button is dealt with in the same place and for the same reason.
//! Edge adds that button to the window on its own account, and clicking it opens
//! `edge://settings/appearance/browserBehavior` in a browser window — a page
//! that has no business existing inside an app, and which WebView2 will not let
//! the host refuse: it is created inside the browser process rather than raised
//! as a new-window request, so `on_new_window` never sees it. The button itself
//! cannot be removed; every picture-in-picture feature WebView2 ships was tried
//! against it (see the browser arguments in `window.rs`). So the page it opens
//! is closed the moment it appears, which is the difference between a button
//! that does nothing and a button that strands the user on a blank window.

use std::collections::HashSet;
use std::os::windows::ffi::OsStrExt;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;
use windows_sys::core::BOOL;
use windows_sys::Win32::Foundation::{FALSE, HANDLE, HWND, LPARAM, TRUE};
use windows_sys::Win32::System::Diagnostics::ToolHelp::{
    CreateToolhelp32Snapshot, Process32NextW, PROCESSENTRY32W, TH32CS_SNAPPROCESS,
};
use windows_sys::Win32::UI::Shell::ExtractIconExW;
use windows_sys::Win32::UI::WindowsAndMessaging::{
    EnumWindows, GetWindowTextLengthW, GetWindowTextW, GetWindowThreadProcessId, IsWindow,
    IsWindowVisible, PostMessageW, SendMessageW, HICON, ICON_BIG, ICON_SMALL, WM_CLOSE, WM_SETICON,
};

/// Tries before giving up on the floating window appearing. Chromium creates it
/// a moment after the request resolves, so the first look usually misses.
const ATTEMPTS: u8 = 10;
const ATTEMPT_DELAY: Duration = Duration::from_millis(200);

/// How often the settings page is looked for while the floating window is up.
/// Short enough that the page is gone before it has finished drawing, and the
/// cost is one `EnumWindows` over the desktop's top-level windows.
///
/// ponytail: polling, and only while a floating window exists. A window event
/// hook scoped to the browser process would be event-driven, but it needs a
/// message loop of its own and this costs microseconds a few times a second.
const WATCH_INTERVAL: Duration = Duration::from_millis(200);

/// What the settings page's window is called. Edge titles the window after the
/// address it is showing, and no window this app opens is ever an `edge:` one.
const SETTINGS_PAGE_PREFIX: &str = "edge://";

/// One hunt at a time. The page reports picture-in-picture opening, and the
/// page is untrusted, so a compromised one must not be able to start a thread
/// per call.
static SEARCHING: AtomicBool = AtomicBool::new(false);

/// Give the floating picture-in-picture window this app's icon, show it on every
/// virtual desktop if the user asked for that, then keep the settings page it
/// can open from being seen.
///
/// Runs on its own thread: the window does not exist yet when
/// picture-in-picture is requested, and waiting for it on the caller's thread
/// would stall the window handler that asked. The thread then stays for as long
/// as the floating window does, because the settings button can be clicked at
/// any point in between.
///
/// The pinning setting is read here rather than passed in because this is where
/// the window handle appears, and it is read once per window: a window that
/// opens while the setting is off stays on one desktop even if the setting is
/// turned on underneath it, which matches how the other picture-in-picture
/// settings behave.
pub fn brand_picture_in_picture_window(all_desktops: bool) {
    if SEARCHING.swap(true, Ordering::SeqCst) {
        return;
    }

    std::thread::spawn(move || {
        // Resolved once. The browser process that owns both windows is already
        // running by the time picture-in-picture is asked for, and taking a
        // process snapshot on every pass below would cost far more than the
        // window scan it is there to support.
        let descendants = descendant_process_ids();

        for _ in 0..ATTEMPTS {
            std::thread::sleep(ATTEMPT_DELAY);

            if let Some(window) = find_floating_window(&descendants) {
                apply_app_icon(window);

                if all_desktops {
                    show_on_every_desktop(window);
                }

                close_settings_page_until_closed(window, &descendants);
                break;
            }
        }

        SEARCHING.store(false, Ordering::SeqCst);
    });
}

/// Ask the shell to keep the floating window in front across desktop switches.
///
/// Retried, because the window being on screen is not the same as the shell
/// having a view for it: `GetViewForHwnd` fails for a window Chromium has only
/// just created, and the gap is a frame or two. Best-effort throughout — a
/// window that stays on one desktop is the behaviour this app had before the
/// setting existed.
fn show_on_every_desktop(window: HWND) {
    use windows::Win32::Foundation::HWND as ComHwnd;

    for _ in 0..ATTEMPTS {
        if crate::app::virtual_desktop::set_pinned(ComHwnd(window as *mut _), true) {
            return;
        }

        std::thread::sleep(ATTEMPT_DELAY);
    }
}

/// Close the settings page for as long as the floating window is on screen.
///
/// The page is recognized by its title rather than by being new, so a window
/// this app has no opinion about is never closed by accident. A page that has
/// not been titled yet survives one pass and goes on the next, which is still
/// faster than it can be read.
fn close_settings_page_until_closed(floating: HWND, descendants: &HashSet<u32>) {
    while unsafe { IsWindow(floating) } != FALSE {
        for window in child_process_windows(descendants) {
            if window == floating {
                continue;
            }

            if window_title(window).starts_with(SETTINGS_PAGE_PREFIX) {
                // Posted rather than sent: the window belongs to another
                // process, and `SendMessageW` would block this thread until
                // that process finished tearing it down.
                unsafe { PostMessageW(window, WM_CLOSE, 0, 0) };
            }
        }

        std::thread::sleep(WATCH_INTERVAL);
    }
}

/// A window's caption, or an empty string for a window without one.
fn window_title(window: HWND) -> String {
    let length = unsafe { GetWindowTextLengthW(window) };
    if length <= 0 {
        return String::new();
    }

    // One more than the reported length: `GetWindowTextW` writes a terminator
    // and reports the count without it.
    let mut buffer = vec![0u16; length as usize + 1];
    let copied = unsafe { GetWindowTextW(window, buffer.as_mut_ptr(), buffer.len() as i32) };
    if copied <= 0 {
        return String::new();
    }

    String::from_utf16_lossy(&buffer[..copied as usize])
}

/// Load the app's icon at both sizes and hand them to the window.
///
/// Pulled out of this executable by path rather than by resource id: the icon
/// is embedded by the bundler under an id this code would otherwise have to
/// guess, and `ExtractIconEx` asks for "the first icon in that file", which is
/// exactly the one Explorer and the taskbar already show for the app.
///
/// Best-effort throughout: a missing icon or a window that has since closed
/// leaves the runtime's own icon in place, which is cosmetic, not broken.
fn apply_app_icon(window: HWND) {
    let Ok(executable) = std::env::current_exe() else {
        return;
    };

    let path: Vec<u16> = executable
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();

    let mut large: HICON = std::ptr::null_mut();
    let mut small: HICON = std::ptr::null_mut();
    let extracted = unsafe { ExtractIconExW(path.as_ptr(), 0, &mut large, &mut small, 1) };

    if extracted == 0 || (large.is_null() && small.is_null()) {
        eprintln!("[Pake] The app icon could not be read for the floating window.");
        return;
    }

    // Windows takes a copy of what it needs for the taskbar, and the window
    // owns the handles from here on, so neither is destroyed.
    for (which, icon) in [(ICON_SMALL, small), (ICON_BIG, large)] {
        if icon.is_null() {
            continue;
        }
        unsafe {
            SendMessageW(window, WM_SETICON, which as usize, icon as isize);
        }
    }
}

/// The floating window, once Chromium has created it.
///
/// This process's own windows are excluded, so on a normal run the only match
/// is the floating window: the WebView2 processes have no windows of their own
/// while the page is merely being rendered into this app's window. The settings
/// page is excluded too, in case the user is quick enough to open one before
/// the icon has been applied.
fn find_floating_window(descendants: &HashSet<u32>) -> Option<HWND> {
    child_process_windows(descendants)
        .into_iter()
        .find(|window| !window_title(*window).starts_with(SETTINGS_PAGE_PREFIX))
}

/// Every visible top-level window owned by a descendant of this process.
fn child_process_windows(descendants: &HashSet<u32>) -> Vec<HWND> {
    if descendants.is_empty() {
        return Vec::new();
    }

    let mut found: Vec<HWND> = Vec::new();
    let mut search = Search {
        descendants,
        found: &mut found,
    };

    unsafe {
        EnumWindows(Some(consider_window), &mut search as *mut Search as LPARAM);
    }

    found
}

struct Search<'a> {
    descendants: &'a HashSet<u32>,
    found: &'a mut Vec<HWND>,
}

unsafe extern "system" fn consider_window(window: HWND, state: LPARAM) -> BOOL {
    let search = unsafe { &mut *(state as *mut Search) };

    if unsafe { IsWindowVisible(window) } == FALSE {
        return TRUE;
    }

    let mut owner = 0u32;
    unsafe { GetWindowThreadProcessId(window, &mut owner) };

    if search.descendants.contains(&owner) {
        search.found.push(window);
    }

    // Carry on: the settings page and the floating window are both wanted, and
    // which one turns up first is not fixed.
    TRUE
}

/// Every process descended from this one, excluding this one.
///
/// WebView2 runs the page in a tree of `msedgewebview2.exe` processes started
/// by this app, and other applications run trees of their own, so ancestry is
/// what separates this app's floating window from anyone else's.
fn descendant_process_ids() -> HashSet<u32> {
    let snapshot: HANDLE = unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) };
    if snapshot.is_null() {
        return HashSet::new();
    }

    // Collected first because a process may be listed before its parent, so
    // ancestry cannot be resolved in one pass over the snapshot.
    let mut parents: Vec<(u32, u32)> = Vec::new();
    let mut entry: PROCESSENTRY32W = unsafe { std::mem::zeroed() };
    entry.dwSize = std::mem::size_of::<PROCESSENTRY32W>() as u32;

    while unsafe { Process32NextW(snapshot, &mut entry) } != FALSE {
        parents.push((entry.th32ProcessID, entry.th32ParentProcessID));
    }

    unsafe { windows_sys::Win32::Foundation::CloseHandle(snapshot) };

    let ours = std::process::id();
    let mut descendants: HashSet<u32> = HashSet::new();
    let mut frontier: HashSet<u32> = HashSet::from([ours]);

    // Repeated passes rather than a recursive walk: the tree is a handful of
    // processes deep, and this needs no ordering assumptions about the snapshot.
    while !frontier.is_empty() {
        let mut next = HashSet::new();
        for (process, parent) in &parents {
            if frontier.contains(parent) && *process != ours && descendants.insert(*process) {
                next.insert(*process);
            }
        }
        frontier = next;
    }

    descendants
}
