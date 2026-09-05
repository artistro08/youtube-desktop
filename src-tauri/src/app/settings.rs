//! Runtime settings the user can change from the tray menu.
//!
//! `pake.json` is compiled into the binary, so anything the user toggles at
//! runtime needs a writable home. This module keeps a single `settings.json`
//! in the app config directory and mirrors it in memory, so hot paths such as
//! the window close handler can read the current value without touching disk.

use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::sync::Mutex;
use tauri::{AppHandle, Manager};

const SETTINGS_FILE: &str = "settings.json";

/// Where settings lived before the app took its own bundle identifier.
///
/// The config directory is named after the identifier, so renaming it moves the
/// file and would silently hand every existing install a fresh set of defaults.
/// Read once from the old place when the new one is empty.
const PREVIOUS_CONFIG_DIR: &str = "com.pake.youtube";

/// Every switch here is on unless the user turns it off, and an older settings
/// file missing the field keeps that behaviour.
fn default_true() -> bool {
    true
}

/// The settings as they are stored, and as the page reads them back.
///
/// Every field carries a `serde` default: a file written by an older build has
/// no key for a setting added since, and without a default the whole document
/// fails to parse and every unrelated setting silently reverts too.
#[derive(Clone, Serialize, Deserialize)]
pub struct StoredSettings {
    #[serde(default = "default_true")]
    pub tray_enabled: bool,
    /// Whether a cold start reopens the last page.
    #[serde(default = "default_true")]
    pub resume_enabled: bool,
    /// Whether minimizing puts a playing video into picture-in-picture.
    #[serde(default = "default_true")]
    pub pip_on_minimize: bool,
    /// Whether closing to the tray does the same.
    #[serde(default = "default_true")]
    pub pip_on_close: bool,
    /// Whether Shorts take part in that. Off by default: a Short popping out
    /// into a floating window while scrolling the feed is rarely wanted.
    #[serde(default)]
    pub pip_on_shorts: bool,
    /// Whether hiding the window pauses a Short that is playing. On by
    /// default: a Short left running loops out of sight until the feed moves
    /// on by itself.
    #[serde(default = "default_true")]
    pub pause_shorts: bool,
    /// Where the user was when the app last shut down, so a cold start can
    /// reopen there instead of the home feed.
    #[serde(default)]
    pub last_url: Option<String>,
}

impl Default for StoredSettings {
    fn default() -> Self {
        Self {
            tray_enabled: true,
            resume_enabled: true,
            pip_on_minimize: true,
            pip_on_close: true,
            pip_on_shorts: false,
            pause_shorts: true,
            last_url: None,
        }
    }
}

/// Read the settings the app wrote before it was renamed.
///
/// Looked for beside the current config directory, so this finds
/// `%APPDATA%\com.pake.youtube\settings.json` from
/// `%APPDATA%\com.artistro08.youtube\settings.json` without knowing where
/// either of them sits. The old file is left in place: reading it is a
/// courtesy, and deleting a user's data on an upgrade is not.
fn read_previous_settings(current: &std::path::Path) -> Option<String> {
    let previous = current
        .parent()?
        .parent()?
        .join(PREVIOUS_CONFIG_DIR)
        .join(SETTINGS_FILE);

    let raw = std::fs::read_to_string(previous).ok()?;
    eprintln!("[Pake] Carried settings over from the app's previous identifier.");
    Some(raw)
}

/// Managed state holding the user's runtime preferences.
///
/// One lock over the whole record rather than a flag apiece: the settings are
/// written out as a unit, so holding the lock across the write is also what
/// stops two toggles from interleaving into a half-updated file.
pub struct AppSettings {
    values: Mutex<StoredSettings>,
    file: Option<PathBuf>,
}

impl AppSettings {
    /// Load persisted settings, falling back to the build-time defaults when
    /// the file is missing or unreadable. A broken settings file must never
    /// stop the app from starting.
    pub fn load(app: &AppHandle) -> Self {
        let file = app
            .path()
            .app_config_dir()
            .ok()
            .map(|dir| dir.join(SETTINGS_FILE));

        let stored = file
            .as_ref()
            .and_then(|path| {
                std::fs::read_to_string(path)
                    .ok()
                    .or_else(|| read_previous_settings(path))
            })
            .and_then(|raw| match serde_json::from_str::<StoredSettings>(&raw) {
                Ok(settings) => Some(settings),
                Err(error) => {
                    // Loud, because the cost of the fallback is the user's
                    // whole configuration going back to defaults at once.
                    eprintln!(
                        "[Pake] settings.json could not be read ({error}); falling back to defaults."
                    );
                    None
                }
            })
            .unwrap_or_default();

        Self {
            values: Mutex::new(stored),
            file,
        }
    }

    /// Read one field. A poisoned lock is recovered from rather than panicked
    /// on: a settings read sits on the window close path, where giving up is
    /// worse than reading a value some other thread was midway through.
    fn read<T>(&self, get: impl FnOnce(&StoredSettings) -> T) -> T {
        let values = self
            .values
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        get(&values)
    }

    /// Change one field and write the file, unless the value already matched.
    fn write(&self, set: impl FnOnce(&mut StoredSettings) -> bool) {
        let mut values = self
            .values
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        if !set(&mut values) {
            return;
        }
        // Still holding the lock: the serialized copy has to match what the
        // next reader sees, and two writers must not race on the same file.
        self.persist(&values);
    }

    /// The whole record, for handing to the page in one go.
    pub fn snapshot(&self) -> StoredSettings {
        self.read(Clone::clone)
    }

    pub fn tray_enabled(&self) -> bool {
        self.read(|settings| settings.tray_enabled)
    }

    pub fn resume_enabled(&self) -> bool {
        self.read(|settings| settings.resume_enabled)
    }

    pub fn pip_on_minimize(&self) -> bool {
        self.read(|settings| settings.pip_on_minimize)
    }

    pub fn pip_on_close(&self) -> bool {
        self.read(|settings| settings.pip_on_close)
    }

    pub fn pip_on_shorts(&self) -> bool {
        self.read(|settings| settings.pip_on_shorts)
    }

    pub fn pause_shorts(&self) -> bool {
        self.read(|settings| settings.pause_shorts)
    }

    /// Set one switch by the name the settings dialog knows it as.
    ///
    /// The name arrives from the page, which is untrusted, so an unknown one is
    /// an error rather than a silent no-op. `last_url` is deliberately absent:
    /// it is not a switch, and the page has its own command for it.
    pub fn set_flag(&self, key: &str, enabled: bool) -> Result<(), String> {
        let field: fn(&mut StoredSettings) -> &mut bool = match key {
            "tray_enabled" => |settings| &mut settings.tray_enabled,
            "resume_enabled" => |settings| &mut settings.resume_enabled,
            "pip_on_minimize" => |settings| &mut settings.pip_on_minimize,
            "pip_on_close" => |settings| &mut settings.pip_on_close,
            "pip_on_shorts" => |settings| &mut settings.pip_on_shorts,
            "pause_shorts" => |settings| &mut settings.pause_shorts,
            other => return Err(format!("Unknown setting '{other}'")),
        };

        self.write(|settings| {
            let value = field(settings);
            let changed = *value != enabled;
            *value = enabled;
            changed
        });

        Ok(())
    }

    /// The page the app was last on, if one was recorded.
    pub fn last_url(&self) -> Option<String> {
        self.read(|settings| settings.last_url.clone())
    }

    /// Remember the current page. Called as the user navigates, so it returns
    /// early when nothing changed rather than rewriting the file each time.
    pub fn set_last_url(&self, url: String) {
        self.write(|settings| {
            if settings.last_url.as_deref() == Some(url.as_str()) {
                return false;
            }
            settings.last_url = Some(url);
            true
        });
    }

    /// Update the tray preference and write it back. Persistence is
    /// best-effort: a failed write is reported but never blocks the toggle.
    pub fn set_tray_enabled(&self, enabled: bool) {
        let _ = self.set_flag("tray_enabled", enabled);
    }

    /// Write the file in one atomic step.
    ///
    /// Written beside the target and renamed over it: `fs::write` truncates
    /// first, so a crash partway through would leave a half-written file, and
    /// an unparseable settings file costs the user every setting at once. A
    /// rename within one directory is atomic on NTFS.
    fn persist(&self, values: &StoredSettings) {
        let Some(file) = self.file.as_ref() else {
            eprintln!("[Pake] No config directory available; settings were not saved.");
            return;
        };

        let Ok(serialized) = serde_json::to_string_pretty(values) else {
            eprintln!("[Pake] Settings could not be serialized; they were not saved.");
            return;
        };

        if let Some(parent) = file.parent() {
            if let Err(error) = std::fs::create_dir_all(parent) {
                eprintln!("[Pake] Failed to create the settings directory: {error}");
                return;
            }
        }

        let temporary = file.with_extension("json.tmp");
        if let Err(error) = std::fs::write(&temporary, serialized) {
            eprintln!("[Pake] Failed to save settings: {error}");
            return;
        }

        if let Err(error) = std::fs::rename(&temporary, file) {
            eprintln!("[Pake] Failed to replace the settings file: {error}");
            let _ = std::fs::remove_file(&temporary);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A file written before a setting existed must keep that setting's
    /// documented default, and must not cost the user the settings it does
    /// name.
    #[test]
    fn older_file_keeps_defaults_for_missing_fields() {
        let raw = r#"{"tray_enabled": false, "last_url": "https://www.youtube.com/watch?v=x"}"#;
        let settings: StoredSettings = serde_json::from_str(raw).expect("older file should parse");

        assert!(!settings.tray_enabled, "the stored value must survive");
        assert_eq!(
            settings.last_url.as_deref(),
            Some("https://www.youtube.com/watch?v=x")
        );
        assert!(settings.resume_enabled);
        assert!(settings.pip_on_minimize);
        assert!(settings.pip_on_close);
        assert!(settings.pause_shorts);
        assert!(!settings.pip_on_shorts, "Shorts stay opt-in");
    }

    /// The very first field added is the one most likely to be missing from a
    /// hand-edited file, and it must not take the document down with it.
    #[test]
    fn empty_object_parses_to_defaults() {
        let settings: StoredSettings = serde_json::from_str("{}").expect("empty object parses");

        assert!(settings.tray_enabled);
        assert!(settings.pause_shorts);
        assert_eq!(settings.last_url, None);
    }

    #[test]
    fn unknown_setting_is_rejected() {
        let settings = AppSettings {
            values: Mutex::new(StoredSettings::default()),
            file: None,
        };

        assert!(settings.set_flag("pip_on_close", false).is_ok());
        assert!(!settings.pip_on_close());
        assert!(settings.set_flag("last_url", true).is_err());
        assert!(settings.set_flag("../../etc/passwd", true).is_err());
    }
}
