//! The settings of this device (`settings.json`, next to the state file).
//!
//! The file is never synced; each device has its own. Every field has a
//! default, so a missing field or file means the default, and every change
//! reads the file, changes one field and writes it back, so that the other
//! fields survive.

use std::io::{self, Write};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::lang::Choice;

/// Name of the settings file, next to the state file.
pub const SETTINGS_FILE: &str = "settings.json";

/// The content of `settings.json`.
#[derive(Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Settings {
    /// What the person chose for the language.
    #[serde(default)]
    pub language: Choice,
    /// Whether the app has already turned "Start at login" on once, so that
    /// it never does so again after the person turned it off.
    #[serde(default, skip_serializing_if = "is_false")]
    pub autostart_offered: bool,
}

fn is_false(value: &bool) -> bool {
    !*value
}

/// The settings file that belongs to a state file.
pub fn path(state: &Path) -> PathBuf {
    state.with_file_name(SETTINGS_FILE)
}

/// The settings; a missing or unreadable file means the defaults.
pub fn load(path: &Path) -> Settings {
    std::fs::read(path)
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or_default()
}

/// Changes the settings with `change` and stores them. Writes a temporary
/// file and renames it over the old one, so a crash leaves either the old
/// or the new file. Creates the folder if no device is set up yet.
pub fn update(path: &Path, change: impl FnOnce(&mut Settings)) -> io::Result<()> {
    let mut settings = load(path);
    change(&mut settings);
    if let Some(dir) = path.parent().filter(|dir| !dir.as_os_str().is_empty()) {
        std::fs::create_dir_all(dir)?;
    }
    let mut tmp = path.as_os_str().to_owned();
    tmp.push(".tmp");
    let tmp = PathBuf::from(tmp);
    let json = serde_json::to_vec(&settings).map_err(io::Error::other)?;
    let mut file = std::fs::File::create(&tmp)?;
    file.write_all(&json)?;
    file.sync_all()?;
    drop(file);
    std::fs::rename(&tmp, path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_missing_field_means_the_default() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join(SETTINGS_FILE);
        assert!(!load(&file).autostart_offered);
        std::fs::write(&file, r#"{"language":"de"}"#).unwrap();
        let settings = load(&file);
        assert!(!settings.autostart_offered);
        assert_eq!(settings.language, Choice::De);
    }

    #[test]
    fn saves_before_the_folder_exists() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("not-yet").join(SETTINGS_FILE);
        update(&file, |s| s.language = Choice::De).unwrap();
        assert_eq!(load(&file).language, Choice::De);
    }

    #[test]
    fn changing_one_field_keeps_the_other() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join(SETTINGS_FILE);
        update(&file, |s| s.autostart_offered = true).unwrap();
        update(&file, |s| s.language = Choice::De).unwrap();
        let settings = load(&file);
        assert!(settings.autostart_offered);
        assert_eq!(settings.language, Choice::De);
        update(&file, |s| s.language = Choice::En).unwrap();
        assert!(load(&file).autostart_offered);
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 1);
    }
}
