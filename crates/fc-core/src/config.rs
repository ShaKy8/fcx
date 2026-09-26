//! User settings (`~/.config/fcx/settings.toml`) and the session that is
//! restored on start (`~/.local/state/fcx/session.toml`: window, tabs).
//! Unknown keys are ignored and missing ones take defaults, so files from
//! older or newer versions still load.

use std::io;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ViewKind {
    #[default]
    Details,
    List,
    Thumbnails,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum DragDefault {
    /// Move within one filesystem, copy across (Nautilus/FreeCommander behaviour).
    #[default]
    Auto,
    Copy,
    Move,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "kebab-case")]
pub struct Settings {
    pub confirm_trash: bool,
    pub confirm_permanent_delete: bool,
    pub show_hidden: bool,
    pub default_view: ViewKind,
    pub restore_tabs: bool,
    pub drag_default: DragDefault,
    pub show_menu_bar: bool,
    pub show_toolbar: bool,
    pub show_places_bar: bool,
    pub show_functions_bar: bool,
    pub show_permissions_column: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            confirm_trash: true,
            confirm_permanent_delete: true,
            show_hidden: false,
            default_view: ViewKind::Details,
            restore_tabs: true,
            drag_default: DragDefault::Auto,
            show_menu_bar: true,
            show_toolbar: true,
            show_places_bar: true,
            show_functions_bar: true,
            show_permissions_column: false,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default, rename_all = "kebab-case")]
pub struct SideSession {
    pub tabs: Vec<PathBuf>,
    pub active: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default, rename_all = "kebab-case")]
pub struct Session {
    pub width: i32,
    pub height: i32,
    pub maximized: bool,
    pub split_position: i32,
    pub active_side: usize,
    pub left: SideSession,
    pub right: SideSession,
}

#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("cannot read {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("{path} is not valid: {source}")]
    Parse {
        path: PathBuf,
        #[source]
        source: toml::de::Error,
    },
    #[error("cannot encode: {0}")]
    Encode(#[from] toml::ser::Error),
}

fn xdg_dir(var: &str, fallback: &str) -> PathBuf {
    std::env::var_os(var)
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(fallback)))
        .unwrap_or_else(|| PathBuf::from("."))
}

pub fn config_dir() -> PathBuf {
    xdg_dir("XDG_CONFIG_HOME", ".config").join("fcx")
}

pub fn state_dir() -> PathBuf {
    xdg_dir("XDG_STATE_HOME", ".local/state").join("fcx")
}

fn load<T: Default + for<'de> Deserialize<'de>>(path: &Path) -> Result<T, ConfigError> {
    match std::fs::read_to_string(path) {
        Ok(text) => toml::from_str(&text).map_err(|source| ConfigError::Parse {
            path: path.to_path_buf(),
            source,
        }),
        Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(T::default()),
        Err(source) => Err(ConfigError::Io {
            path: path.to_path_buf(),
            source,
        }),
    }
}

fn save<T: Serialize>(path: &Path, value: &T) -> Result<(), ConfigError> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|source| ConfigError::Io {
            path: parent.to_path_buf(),
            source,
        })?;
    }
    let text = toml::to_string_pretty(value)?;
    std::fs::write(path, text).map_err(|source| ConfigError::Io {
        path: path.to_path_buf(),
        source,
    })
}

impl Settings {
    pub fn path() -> PathBuf {
        config_dir().join("settings.toml")
    }

    pub fn load(path: &Path) -> Result<Settings, ConfigError> {
        load(path)
    }

    pub fn save(&self, path: &Path) -> Result<(), ConfigError> {
        save(path, self)
    }
}

impl Session {
    pub fn path() -> PathBuf {
        state_dir().join("session.toml")
    }

    pub fn load(path: &Path) -> Result<Session, ConfigError> {
        load(path)
    }

    pub fn save(&self, path: &Path) -> Result<(), ConfigError> {
        save(path, self)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn settings_round_trip_and_defaults() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("deep/settings.toml");
        assert_eq!(Settings::load(&path).unwrap(), Settings::default());

        let custom = Settings {
            confirm_trash: false,
            default_view: ViewKind::Thumbnails,
            drag_default: DragDefault::Copy,
            show_functions_bar: false,
            ..Settings::default()
        };
        custom.save(&path).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains("default-view = \"thumbnails\""), "{text}");
        assert_eq!(Settings::load(&path).unwrap(), custom);
    }

    #[test]
    fn unknown_and_missing_keys_are_tolerated() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("settings.toml");
        std::fs::write(&path, "show-hidden = true\nfuture-option = 3\n").unwrap();
        let loaded = Settings::load(&path).unwrap();
        assert!(loaded.show_hidden);
        assert!(loaded.confirm_trash, "missing keys default");

        std::fs::write(&path, "show-hidden = \"maybe\"").unwrap();
        assert!(matches!(
            Settings::load(&path),
            Err(ConfigError::Parse { .. })
        ));
    }

    #[test]
    fn session_round_trip() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("session.toml");
        let session = Session {
            width: 1200,
            height: 760,
            maximized: false,
            split_position: 600,
            active_side: 1,
            left: SideSession {
                tabs: vec!["/home/kyle".into(), "/tmp".into()],
                active: 1,
            },
            right: SideSession {
                tabs: vec!["/".into()],
                active: 0,
            },
        };
        session.save(&path).unwrap();
        assert_eq!(Session::load(&path).unwrap(), session);
        assert_eq!(
            Session::load(&tmp.path().join("none")).unwrap(),
            Session::default()
        );
    }
}
