//! Favorite folders, persisted as TOML in the user's config directory.

use std::io;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Favorite {
    pub name: String,
    pub path: PathBuf,
}

#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Favorites {
    #[serde(default)]
    pub items: Vec<Favorite>,
}

#[derive(Debug, thiserror::Error)]
pub enum FavoritesError {
    #[error("cannot read favorites: {0}")]
    Io(#[from] io::Error),
    #[error("favorites file is not valid: {0}")]
    Parse(#[from] toml::de::Error),
    #[error("cannot encode favorites: {0}")]
    Encode(#[from] toml::ser::Error),
}

impl Favorites {
    /// `$XDG_CONFIG_HOME/fcx/favorites.toml` (or `~/.config/fcx/favorites.toml`).
    pub fn default_path() -> PathBuf {
        let base = std::env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .filter(|p| p.is_absolute())
            .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))
            .unwrap_or_else(|| PathBuf::from("."));
        base.join("fcx").join("favorites.toml")
    }

    /// A missing file is an empty list; a corrupt one is an error.
    pub fn load(path: &Path) -> Result<Favorites, FavoritesError> {
        match std::fs::read_to_string(path) {
            Ok(text) => Ok(toml::from_str(&text)?),
            Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(Favorites::default()),
            Err(err) => Err(err.into()),
        }
    }

    pub fn save(&self, path: &Path) -> Result<(), FavoritesError> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(path, toml::to_string_pretty(self)?)?;
        Ok(())
    }

    /// Adds `path` named after its last component; returns false if already present.
    pub fn add(&mut self, path: PathBuf) -> bool {
        if self.items.iter().any(|f| f.path == path) {
            return false;
        }
        let name = match path.file_name() {
            Some(name) => name.to_string_lossy().into_owned(),
            None => "/".to_owned(),
        };
        self.items.push(Favorite { name, path });
        true
    }

    pub fn remove(&mut self, index: usize) {
        if index < self.items.len() {
            self.items.remove(index);
        }
    }

    pub fn move_item(&mut self, from: usize, to: usize) {
        if from < self.items.len() && to < self.items.len() {
            let item = self.items.remove(from);
            self.items.insert(to, item);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_through_toml() {
        let tmp = tempfile::tempdir().unwrap();
        let file = tmp.path().join("nested/favorites.toml");
        let mut favs = Favorites::default();
        assert!(favs.add(PathBuf::from("/home/kyle/Projects")));
        assert!(
            !favs.add(PathBuf::from("/home/kyle/Projects")),
            "no duplicates"
        );
        assert!(favs.add(PathBuf::from("/")));
        favs.items[0].name = "Work".into();
        favs.save(&file).unwrap();

        let loaded = Favorites::load(&file).unwrap();
        assert_eq!(loaded, favs);
        assert_eq!(loaded.items[1].name, "/");
    }

    #[test]
    fn missing_file_is_empty_and_garbage_is_an_error() {
        let tmp = tempfile::tempdir().unwrap();
        assert_eq!(
            Favorites::load(&tmp.path().join("none.toml")).unwrap(),
            Favorites::default()
        );
        let bad = tmp.path().join("bad.toml");
        std::fs::write(&bad, "items = 3").unwrap();
        assert!(matches!(
            Favorites::load(&bad),
            Err(FavoritesError::Parse(_))
        ));
    }

    #[test]
    fn reorder_and_remove() {
        let mut favs = Favorites::default();
        for p in ["/a", "/b", "/c"] {
            favs.add(PathBuf::from(p));
        }
        favs.move_item(2, 0);
        assert_eq!(favs.items[0].path, Path::new("/c"));
        favs.remove(1);
        assert_eq!(favs.items.len(), 2);
        favs.remove(99);
        assert_eq!(favs.items.len(), 2);
    }
}
