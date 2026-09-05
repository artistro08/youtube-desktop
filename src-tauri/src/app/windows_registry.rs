//! Windows URL-scheme registration.
//!
//! The app claims `youtube://` and nothing else. It deliberately does not
//! register as an http/https handler: Windows has no per-domain routing, so
//! being offered as a browser would mean owning every web link on the machine
//! just to catch YouTube ones.

use std::path::Path;
use winreg::enums::HKEY_CURRENT_USER;
use winreg::RegKey;

const APP_NAME: &str = "YouTube";
const DEEP_LINK_SCHEME: &str = "youtube";

/// Register the `youtube://` scheme for the current user.
///
/// Everything is written under HKCU, so no elevation is needed. Failures are
/// logged and ignored: scheme registration is a convenience, not a startup
/// requirement.
pub fn register_url_handlers(executable: &Path) {
    if let Err(error) = write_scheme(executable) {
        eprintln!("[Pake] Failed to register the youtube:// scheme: {error}");
    }
}

fn write_scheme(executable: &Path) -> std::io::Result<()> {
    let exe = executable.display().to_string();
    let hkcu = RegKey::predef(HKEY_CURRENT_USER);

    let (scheme, _) = hkcu.create_subkey(format!(r"Software\Classes\{DEEP_LINK_SCHEME}"))?;
    scheme.set_value("", &format!("URL:{APP_NAME} Protocol"))?;
    scheme.set_value("URL Protocol", &"")?;

    let (icon, _) = scheme.create_subkey("DefaultIcon")?;
    icon.set_value("", &format!("{exe},0"))?;

    let (command, _) = scheme.create_subkey(r"shell\open\command")?;
    command.set_value("", &format!("\"{exe}\" \"%1\""))?;

    Ok(())
}
