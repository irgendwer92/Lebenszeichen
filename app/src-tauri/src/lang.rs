//! The language of the app: the setting of this device (`settings.json`)
//! and the language in effect.
//!
//! The setting lives next to the state file and is never synced; each device
//! has its own. `system` follows the language of the system: on the desktop
//! the one of the OS, on Android the first locale of the application
//! context. Languages without a translation give English.

use std::io;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use lz_client::i18n::{Language, Texts};
use serde::{Deserialize, Serialize};

use crate::settings;

/// What the person chose for the language.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Choice {
    /// Follow the language of the system.
    #[default]
    System,
    En,
    De,
}

impl Choice {
    pub fn tag(self) -> &'static str {
        match self {
            Self::System => "system",
            Self::En => "en",
            Self::De => "de",
        }
    }

    pub fn from_tag(tag: &str) -> Option<Self> {
        match tag {
            "system" => Some(Self::System),
            "en" => Some(Self::En),
            "de" => Some(Self::De),
            _ => None,
        }
    }
}

/// The settings file that belongs to a state file.
pub fn settings_path(state: &Path) -> PathBuf {
    settings::path(state)
}

/// The language setting; a missing or unreadable file means `system`.
pub fn load(path: &Path) -> Choice {
    settings::load(path).language
}

/// Stores the language setting; the other settings stay as they are.
pub fn save(path: &Path, language: Choice) -> io::Result<()> {
    settings::update(path, |settings| settings.language = language)
}

/// The language that a setting stands for; `system` asks `system`.
pub fn effective(choice: Choice, system: impl FnOnce() -> Language) -> Language {
    match choice {
        Choice::System => system(),
        Choice::En => Language::En,
        Choice::De => Language::De,
    }
}

/// The language of the system on the desktop; English if it has no
/// translation. Without any information from the OS the environment decides.
#[cfg(not(target_os = "android"))]
pub fn system() -> Language {
    match sys_locale::get_locale() {
        Some(tag) => Language::from_tag(&tag).unwrap_or_default(),
        None => Language::from_env(),
    }
}

/// The language of the system on Android, from the application context.
#[cfg(target_os = "android")]
pub fn system() -> Language {
    crate::jni_bridge::tauri_system_language()
}

static CURRENT: Mutex<Language> = Mutex::new(Language::En);

/// The language in effect for the texts that the Rust side writes.
pub fn current() -> Language {
    *CURRENT.lock().expect("lock poisoned")
}

/// The translator for [`current`].
pub fn texts() -> Texts {
    current().texts()
}

/// Makes `language` the one in effect.
pub fn set_current(language: Language) {
    *CURRENT.lock().expect("lock poisoned") = language;
}

/// Reads the setting that belongs to the state file at `state` and makes
/// the language it stands for the one in effect.
pub fn apply(state: &Path) -> Language {
    let language = effective(load(&settings_path(state)), system);
    set_current(language);
    language
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::settings::SETTINGS_FILE;

    #[test]
    fn missing_or_unreadable_file_means_system() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(SETTINGS_FILE);
        assert_eq!(load(&path), Choice::System);
        std::fs::write(&path, "not json").unwrap();
        assert_eq!(load(&path), Choice::System);
        std::fs::write(&path, r#"{"language":"fr"}"#).unwrap();
        assert_eq!(load(&path), Choice::System);
        std::fs::write(&path, "{}").unwrap();
        assert_eq!(load(&path), Choice::System);
    }

    #[test]
    fn the_setting_survives_storage() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(SETTINGS_FILE);
        for choice in [Choice::De, Choice::En, Choice::System] {
            save(&path, choice).unwrap();
            assert_eq!(load(&path), choice);
        }
        save(&path, Choice::De).unwrap();
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            r#"{"language":"de"}"#
        );
        // No temporary file is left behind.
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 1);
    }

    #[test]
    fn the_settings_lie_next_to_the_state_file() {
        assert_eq!(
            settings_path(Path::new("/data/app/device.json")),
            Path::new("/data/app/settings.json")
        );
    }

    #[test]
    fn a_chosen_language_wins_over_the_system() {
        assert_eq!(effective(Choice::De, || Language::En), Language::De);
        assert_eq!(effective(Choice::En, || Language::De), Language::En);
        assert_eq!(effective(Choice::System, || Language::De), Language::De);
    }

    #[test]
    fn choices_round_trip_through_tags() {
        for choice in [Choice::System, Choice::En, Choice::De] {
            assert_eq!(Choice::from_tag(choice.tag()), Some(choice));
        }
        assert_eq!(Choice::from_tag("fr"), None);
    }

    /// The text between the quotes that follow each `start`, in `source`.
    fn quoted_after(source: &str, start: &str) -> Vec<String> {
        source
            .match_indices(start)
            .filter_map(|(at, _)| {
                let rest = &source[at + start.len()..];
                Some(rest[..rest.find('"')?].to_owned())
            })
            .collect()
    }

    fn read(path: &str) -> String {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"));
        std::fs::read_to_string(root.join(path)).unwrap()
    }

    #[test]
    fn every_id_of_the_interface_exists_in_english() {
        let english = Language::En.raw_texts("");
        let mut used = std::collections::BTreeSet::new();
        for file in ["../ui/index.html", "../ui/alert.html"] {
            let html = read(file);
            for attribute in [
                "data-i18n=\"",
                "data-i18n-aria-label=\"",
                "data-i18n-placeholder=\"",
                "data-i18n-title=\"",
            ] {
                used.extend(quoted_after(&html, attribute));
            }
        }
        for file in ["../ui/app.js", "../ui/alert.js"] {
            // Comments may name ids as examples.
            let js = read(file)
                .lines()
                .filter(|line| !line.trim_start().starts_with("//"))
                .collect::<Vec<_>>()
                .join("\n");
            // `t("…")` and every other literal that names an id.
            used.extend(quoted_after(&js, " t(\""));
            used.extend(quoted_after(&js, "(t(\""));
            used.extend(
                quoted_after(&js, "\"ui-")
                    .into_iter()
                    .map(|rest| format!("ui-{rest}")),
            );
        }
        assert!(used.len() > 50, "found only {} ids", used.len());
        let missing: Vec<_> = used
            .iter()
            .filter(|id| !english.contains_key(*id))
            .collect();
        assert!(missing.is_empty(), "not in the English texts: {missing:?}");
        // The other way round: no `ui-` text that nothing uses.
        let unused: Vec<_> = english
            .keys()
            .filter(|id| id.starts_with("ui-") && !used.contains(*id))
            .collect();
        assert!(unused.is_empty(), "unused texts: {unused:?}");
    }

    #[test]
    fn every_id_of_the_rust_side_exists_in_english() {
        let english = Language::En.raw_texts("");
        let sources = [
            "commands.rs",
            "desktop.rs",
            "desktop/notify.rs",
            "jni_bridge.rs",
            "background.rs",
        ];
        for file in sources {
            let source = read(&format!("src/{file}"));
            for id in quoted_after(&source, "\"app-") {
                let id = format!("app-{id}");
                assert!(
                    english.contains_key(&id),
                    "{file}: {id} is not in the English texts"
                );
            }
        }
    }
}
