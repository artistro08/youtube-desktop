//! Giving the picture-in-picture window the app's own icon.
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
    EnumWindows, GetWindowThreadProcessId, IsWindowVisible, SendMessageW, HICON, ICON_BIG,
    ICON_SMALL, WM_SETICON,
};

/// Tries before giving up on the floating window appearing. Chromium creates it
/// a moment after the request resolves, so the first look usually misses.
const ATTEMPTS: u8 = 10;
const ATTEMPT_DELAY: Duration = Duration::from_millis(200);

/// One hunt at a time. The page reports picture-in-picture opening, and the
/// page is untrusted, so a compromised one must not be able to start a thread
/// per call.
static SEARCHING: AtomicBool = AtomicBool::new(false);

/// Give the floating picture-in-picture window this app's icon.
///
/// Runs on its own thread: the window does not exist yet when
/// picture-in-picture is requested, and waiting for it on the caller's thread
/// would stall the window handler that asked.
pub fn brand_picture_in_picture_window() {
    if SEARCHING.swap(true, Ordering::SeqCst) {
        return;
    }

    std::thread::spawn(|| {
        for _ in 0..ATTEMPTS {
            std::thread::sleep(ATTEMPT_DELAY);

            if let Some(window) = find_child_process_window() {
                apply_app_icon(window);
                break;
            }
        }

        SEARCHING.store(false, Ordering::SeqCst);
    });
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

/// The one visible top-level window owned by a descendant of this process.
///
/// This process's own windows are excluded, so on a normal run the only match
/// is the floating window: the WebView2 processes have no windows of their own
/// while the page is merely being rendered into this app's window.
fn find_child_process_window() -> Option<HWND> {
    let descendants = descendant_process_ids();
    if descendants.is_empty() {
        return None;
    }

    let mut found: Option<HWND> = None;
    let mut search = Search {
        descendants: &descendants,
        found: &mut found,
    };

    unsafe {
        EnumWindows(Some(consider_window), &mut search as *mut Search as LPARAM);
    }

    found
}

struct Search<'a> {
    descendants: &'a HashSet<u32>,
    found: &'a mut Option<HWND>,
}

unsafe extern "system" fn consider_window(window: HWND, state: LPARAM) -> BOOL {
    let search = unsafe { &mut *(state as *mut Search) };

    if unsafe { IsWindowVisible(window) } == FALSE {
        return TRUE;
    }

    let mut owner = 0u32;
    unsafe { GetWindowThreadProcessId(window, &mut owner) };

    if !search.descendants.contains(&owner) {
        return TRUE;
    }

    *search.found = Some(window);
    // Stop: the first match is the one.
    FALSE
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
