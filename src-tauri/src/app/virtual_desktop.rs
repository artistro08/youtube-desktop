//! Pinning the picture-in-picture window to every virtual desktop.
//!
//! A floating video is only useful while it is in front of whatever the user is
//! doing, and on Windows "whatever the user is doing" can be another virtual
//! desktop. Switching desktops takes the floating window away with the one it
//! was opened on, which is exactly when it stops doing its job.
//!
//! Windows has a documented `IVirtualDesktopManager`, and it cannot do this: it
//! moves a window to one desktop and reports which desktop a window is on, with
//! nothing about being on all of them. What the shell uses for its own
//! "Show this window on all desktops" menu item is `IVirtualDesktopPinnedApps`,
//! which is not in the SDK and not documented. It is reached the way the shell
//! reaches it, through the immersive shell's service provider.
//!
//! **This is undocumented, and it is written to fail quietly.** Microsoft has
//! changed these interface ids between Windows releases before and can do it
//! again in an update; when that happens `CoCreateInstance` or `QueryService`
//! returns an error, this module says so once on stderr, and the floating window
//! behaves as it always did. Nothing else in the app depends on it working, so
//! the cost of a future Windows release breaking it is one setting that stops
//! taking effect — never a crash, and never a window that fails to appear.
//!
//! Verified working on Windows 11 build 26300, including on the floating window,
//! which belongs to `msedgewebview2.exe` rather than to this process.

use std::ffi::c_void;
use windows::core::{Interface, GUID, HRESULT};
use windows::Win32::Foundation::HWND;
use windows::Win32::System::Com::{CoCreateInstance, IServiceProvider, CLSCTX_LOCAL_SERVER};

/// The immersive shell, which vends the virtual desktop services below.
const CLSID_IMMERSIVE_SHELL: GUID = GUID::from_u128(0xC2F03A33_21F5_47FA_B4BB_156362A2F239);

/// The service that knows which windows are pinned to every desktop.
const CLSID_VIRTUAL_DESKTOP_PINNED_APPS: GUID =
    GUID::from_u128(0xB5A399E7_1C87_46B8_88E9_FC5747B171BD);

/// `IApplicationViewCollection`, which turns a window handle into the "view"
/// the virtual desktop services deal in. Its class id and interface id are the
/// same value.
const IID_APPLICATION_VIEW_COLLECTION: GUID =
    GUID::from_u128(0x1841C6D7_4F9D_42C0_AF41_8747538F10E5);

/// `IVirtualDesktopPinnedApps`.
const IID_VIRTUAL_DESKTOP_PINNED_APPS: GUID =
    GUID::from_u128(0x4CE81583_1E4C_4632_A621_07A53543148F);

/// `IApplicationViewCollection`, hand-declared because it is not in the
/// metadata the `windows` crate is generated from.
///
/// Only `GetViewForHwnd` is called, but every method before it in the interface
/// has to be declared: this is a raw vtable, and a missing entry would silently
/// move every later method to the wrong slot. The three that are not used take
/// their arguments as raw pointers rather than modelled types, since nothing
/// here ever looks at what they return.
#[repr(C)]
struct ApplicationViewCollectionVtbl {
    query_interface:
        unsafe extern "system" fn(*mut c_void, *const GUID, *mut *mut c_void) -> HRESULT,
    add_ref: unsafe extern "system" fn(*mut c_void) -> u32,
    release: unsafe extern "system" fn(*mut c_void) -> u32,
    get_views: unsafe extern "system" fn(*mut c_void, *mut *mut c_void) -> HRESULT,
    get_views_by_z_order: unsafe extern "system" fn(*mut c_void, *mut *mut c_void) -> HRESULT,
    get_views_by_app_user_model_id:
        unsafe extern "system" fn(*mut c_void, *const u16, *mut *mut c_void) -> HRESULT,
    get_view_for_hwnd: unsafe extern "system" fn(*mut c_void, HWND, *mut *mut c_void) -> HRESULT,
}

/// `IVirtualDesktopPinnedApps`, hand-declared for the same reason.
#[repr(C)]
struct VirtualDesktopPinnedAppsVtbl {
    query_interface:
        unsafe extern "system" fn(*mut c_void, *const GUID, *mut *mut c_void) -> HRESULT,
    add_ref: unsafe extern "system" fn(*mut c_void) -> u32,
    release: unsafe extern "system" fn(*mut c_void) -> u32,
    is_app_id_pinned: unsafe extern "system" fn(*mut c_void, *const u16, *mut bool) -> HRESULT,
    pin_app_id: unsafe extern "system" fn(*mut c_void, *const u16) -> HRESULT,
    unpin_app_id: unsafe extern "system" fn(*mut c_void, *const u16) -> HRESULT,
    is_view_pinned: unsafe extern "system" fn(*mut c_void, *mut c_void, *mut bool) -> HRESULT,
    pin_view: unsafe extern "system" fn(*mut c_void, *mut c_void) -> HRESULT,
    unpin_view: unsafe extern "system" fn(*mut c_void, *mut c_void) -> HRESULT,
}

/// A raw COM pointer that releases itself.
///
/// These interfaces are reached through raw vtables rather than through the
/// `windows` crate's generated wrappers, so the reference counting is this
/// module's own to get right. One small guard is less error-prone than a
/// `Release` call on each of the several ways out of the function below.
struct ComPtr(*mut c_void);

impl ComPtr {
    /// The `IUnknown` vtable's first three entries are the same for every
    /// interface, so `Release` can be reached without knowing which this is.
    fn release(&self) {
        if self.0.is_null() {
            return;
        }
        unsafe {
            let vtable = *(self.0 as *mut *mut ApplicationViewCollectionVtbl);
            ((*vtable).release)(self.0);
        }
    }
}

impl Drop for ComPtr {
    fn drop(&mut self) {
        self.release();
    }
}

/// Show `window` on every virtual desktop, or stop doing so.
///
/// Returns whether the window ended up in the requested state. A `false` means
/// the shell refused or these interfaces have moved; the caller treats that as
/// cosmetic, because it is.
///
/// # Safety
///
/// `window` must be a valid top-level window handle. The window may belong to
/// another process — that is the normal case here, since the floating window is
/// WebView2's.
pub fn set_pinned(window: HWND, pinned: bool) -> bool {
    // The shell's own service provider, which is how its virtual desktop
    // services are reached. `CLSCTX_LOCAL_SERVER` because the immersive shell
    // lives in explorer.exe rather than in this process.
    let shell: IServiceProvider =
        match unsafe { CoCreateInstance(&CLSID_IMMERSIVE_SHELL, None, CLSCTX_LOCAL_SERVER) } {
            Ok(shell) => shell,
            Err(error) => {
                report("the immersive shell could not be reached", error.code());
                return false;
            }
        };

    let views = match query_service(
        &shell,
        &IID_APPLICATION_VIEW_COLLECTION,
        &IID_APPLICATION_VIEW_COLLECTION,
    ) {
        Ok(views) => views,
        Err(code) => {
            report("the window collection is unavailable", code);
            return false;
        }
    };

    // The view is the shell's own handle on the window, and it is what the
    // pinning service takes. A window that has just been created may not have
    // one yet, which is a failure rather than a reason to retry here: the caller
    // only asks once the window is on screen.
    let mut view: *mut c_void = std::ptr::null_mut();
    let result = unsafe {
        let vtable = *(views.0 as *mut *mut ApplicationViewCollectionVtbl);
        ((*vtable).get_view_for_hwnd)(views.0, window, &mut view)
    };
    if result.is_err() || view.is_null() {
        report("the window has no shell view", result);
        return false;
    }
    let view = ComPtr(view);

    let pinner = match query_service(
        &shell,
        &CLSID_VIRTUAL_DESKTOP_PINNED_APPS,
        &IID_VIRTUAL_DESKTOP_PINNED_APPS,
    ) {
        Ok(pinner) => pinner,
        Err(code) => {
            report("the pinning service is unavailable", code);
            return false;
        }
    };

    unsafe {
        let vtable = *(pinner.0 as *mut *mut VirtualDesktopPinnedAppsVtbl);

        // Asked first because pinning an already-pinned view, or unpinning one
        // that was never pinned, is not something the shell promises to ignore.
        let mut already = false;
        let result = ((*vtable).is_view_pinned)(pinner.0, view.0, &mut already);
        if result.is_err() {
            report("the window's pinned state could not be read", result);
            return false;
        }

        if already == pinned {
            return true;
        }

        let change = if pinned {
            (*vtable).pin_view
        } else {
            (*vtable).unpin_view
        };

        let result = change(pinner.0, view.0);
        if result.is_err() {
            report("the window's pinned state could not be changed", result);
            return false;
        }
    }

    true
}

/// Ask the shell for one of its services.
///
/// The `windows` crate's own `QueryService` wants a generated interface type to
/// take the interface id from, and these interfaces are not in its metadata —
/// that is the whole reason they are declared by hand here. The raw vtable entry
/// takes the id as an argument, which is what this needs.
fn query_service(
    shell: &IServiceProvider,
    service: &GUID,
    interface: &GUID,
) -> Result<ComPtr, HRESULT> {
    let mut object: *mut c_void = std::ptr::null_mut();

    let result = unsafe {
        (Interface::vtable(shell).QueryService)(
            Interface::as_raw(shell),
            service,
            interface,
            &mut object,
        )
    };

    if result.is_err() {
        return Err(result);
    }

    if object.is_null() {
        // A success with nothing in it would otherwise be dereferenced below.
        return Err(HRESULT(0));
    }

    Ok(ComPtr(object))
}

/// Say once what went wrong, without turning a cosmetic failure into noise.
///
/// These calls are made on a timer for as long as a floating window is up, so a
/// Windows release that moves the interfaces would otherwise print the same line
/// several times a second for the life of the app.
fn report(what: &str, code: HRESULT) {
    use std::sync::atomic::{AtomicBool, Ordering};
    static REPORTED: AtomicBool = AtomicBool::new(false);

    if REPORTED.swap(true, Ordering::SeqCst) {
        return;
    }

    eprintln!(
        "[Pake] Showing the floating window on every desktop is unavailable: {what} ({code:?}). \
         These Windows interfaces are undocumented and can change between releases; \
         the window still works, it just stays on one desktop."
    );
}
