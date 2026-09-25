//! Key chords and the chord → action table.
//!
//! The GUI turns a key event into a [`Chord`] and asks the [`Keymap`] which
//! [`Action`] it means. Nothing here knows about GDK; key names are plain strings.

use std::collections::HashMap;
use std::str::FromStr;

use serde::Deserialize;

use crate::action::{Action, UnknownAction};

pub const DEFAULT_KEYMAP_TOML: &str = include_str!("keymap.default.toml");

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct Mods {
    pub ctrl: bool,
    pub shift: bool,
    pub alt: bool,
    pub super_: bool,
}

/// A key plus modifiers. `key` is a lowercased GDK key name (`f5`, `return`, `a`).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Chord {
    pub key: String,
    pub mods: Mods,
}

impl Chord {
    pub fn new(key: &str, mods: Mods) -> Self {
        Chord {
            key: normalize_key(key),
            mods,
        }
    }
}

/// Lowercases and maps common aliases onto GDK names.
pub fn normalize_key(key: &str) -> String {
    let lower = key.trim().to_ascii_lowercase();
    match lower.as_str() {
        "esc" => "escape",
        "del" => "delete",
        "ins" => "insert",
        "enter" | "kp_enter" => "return",
        "pgup" | "pageup" => "page_up",
        "pgdn" | "pagedown" | "page_dn" => "page_down",
        "iso_left_tab" => "tab",
        "kp_add" => "plus",
        "kp_subtract" => "minus",
        "kp_multiply" => "asterisk",
        "kp_divide" => "slash",
        "kp_insert" => "insert",
        "kp_delete" => "delete",
        "kp_home" => "home",
        "kp_end" => "end",
        "kp_up" => "up",
        "kp_down" => "down",
        "kp_left" => "left",
        "kp_right" => "right",
        "kp_page_up" => "page_up",
        "kp_page_down" => "page_down",
        _ => return lower,
    }
    .to_owned()
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ChordError {
    #[error("empty key chord")]
    Empty,
    #[error("key chord {0:?} names more than one key")]
    TooManyKeys(String),
    #[error("key chord {0:?} has no key, only modifiers")]
    NoKey(String),
}

impl FromStr for Chord {
    type Err = ChordError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        if s.trim().is_empty() {
            return Err(ChordError::Empty);
        }
        let mut mods = Mods::default();
        let mut key: Option<&str> = None;
        for part in s.split('+') {
            let part = part.trim();
            match part.to_ascii_lowercase().as_str() {
                "ctrl" | "control" => mods.ctrl = true,
                "shift" => mods.shift = true,
                "alt" | "meta" => mods.alt = true,
                "super" | "mod4" | "win" => mods.super_ = true,
                "" => return Err(ChordError::NoKey(s.to_owned())),
                _ => {
                    if key.is_some() {
                        return Err(ChordError::TooManyKeys(s.to_owned()));
                    }
                    key = Some(part);
                }
            }
        }
        key.map(|k| Chord::new(k, mods))
            .ok_or_else(|| ChordError::NoKey(s.to_owned()))
    }
}

#[derive(Debug, thiserror::Error)]
pub enum KeymapError {
    #[error("keymap is not valid TOML: {0}")]
    Toml(#[from] toml::de::Error),
    #[error(transparent)]
    Chord(#[from] ChordError),
    #[error("binding for {chord}: {source}")]
    Action {
        chord: String,
        #[source]
        source: UnknownAction,
    },
}

#[derive(Deserialize)]
struct KeymapFile {
    #[serde(default)]
    bindings: HashMap<String, String>,
}

#[derive(Debug, Clone, Default)]
pub struct Keymap {
    bindings: HashMap<Chord, Action>,
}

impl Keymap {
    /// The built-in bindings. Panics only if the embedded default file is broken,
    /// which the test suite guards against.
    pub fn defaults() -> Self {
        let mut keymap = Keymap::default();
        keymap
            .merge_toml(DEFAULT_KEYMAP_TOML)
            .expect("embedded default keymap is valid");
        keymap
    }

    /// Layer a user keymap over the current one. Binding a chord to `"none"` removes it.
    pub fn merge_toml(&mut self, toml_text: &str) -> Result<(), KeymapError> {
        let file: KeymapFile = toml::from_str(toml_text)?;
        for (chord_text, action_id) in file.bindings {
            let chord: Chord = chord_text.parse()?;
            if action_id == "none" {
                self.bindings.remove(&chord);
                continue;
            }
            let action = action_id
                .parse::<Action>()
                .map_err(|source| KeymapError::Action {
                    chord: chord_text,
                    source,
                })?;
            self.bindings.insert(chord, action);
        }
        Ok(())
    }

    pub fn lookup(&self, chord: &Chord) -> Option<Action> {
        self.bindings.get(chord).copied()
    }

    pub fn chords_for(&self, action: Action) -> Vec<&Chord> {
        let mut chords: Vec<&Chord> = self
            .bindings
            .iter()
            .filter(|(_, a)| **a == action)
            .map(|(c, _)| c)
            .collect();
        chords.sort_by(|a, b| a.key.cmp(&b.key));
        chords
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn chord(s: &str) -> Chord {
        s.parse().unwrap()
    }

    #[test]
    fn parses_modifiers_and_normalizes_names() {
        let c = chord("Ctrl+Shift+F5");
        assert_eq!(c.key, "f5");
        assert!(c.mods.ctrl && c.mods.shift && !c.mods.alt);
        assert_eq!(chord("Esc"), chord("Escape"));
        assert_eq!(chord("ctrl + PgUp"), chord("Control+Page_Up"));
        assert_eq!(chord("KP_Enter"), chord("Return"));
        assert_eq!(chord("Shift+Tab").key, "tab");
    }

    #[test]
    fn rejects_bad_chords() {
        assert_eq!("".parse::<Chord>(), Err(ChordError::Empty));
        assert_eq!(
            "Ctrl+Shift".parse::<Chord>(),
            Err(ChordError::NoKey("Ctrl+Shift".into()))
        );
        assert_eq!(
            "a+b".parse::<Chord>(),
            Err(ChordError::TooManyKeys("a+b".into()))
        );
        assert_eq!(
            "Ctrl+".parse::<Chord>(),
            Err(ChordError::NoKey("Ctrl+".into()))
        );
    }

    #[test]
    fn defaults_load_and_cover_every_action() {
        let keymap = Keymap::defaults();
        for action in Action::ALL {
            assert!(
                !keymap.chords_for(action).is_empty(),
                "{action} has no default binding"
            );
        }
        assert_eq!(keymap.lookup(&chord("Tab")), Some(Action::SwitchPane));
        assert_eq!(keymap.lookup(&chord("Ctrl+h")), Some(Action::ToggleHidden));
        assert_eq!(keymap.lookup(&chord("Ctrl+Shift+h")), None);
    }

    #[test]
    fn user_overrides_and_unbinds() {
        let mut keymap = Keymap::defaults();
        keymap
            .merge_toml(
                r#"
                [bindings]
                "Ctrl+h" = "none"
                "Alt+period" = "toggle-hidden"
                "#,
            )
            .unwrap();
        assert_eq!(keymap.lookup(&chord("Ctrl+h")), None);
        assert_eq!(
            keymap.lookup(&chord("Alt+period")),
            Some(Action::ToggleHidden)
        );
    }

    #[test]
    fn reports_unknown_actions_and_bad_toml() {
        let mut keymap = Keymap::default();
        let err = keymap
            .merge_toml("[bindings]\nF9 = \"launch-rockets\"")
            .unwrap_err();
        assert!(matches!(err, KeymapError::Action { .. }), "{err}");
        assert!(matches!(
            keymap.merge_toml("bindings = 3").unwrap_err(),
            KeymapError::Toml(_)
        ));
    }
}
