//! Freedesktop thumbnail cache paths (the spec Nautilus, GNOME, and KDE
//! share): `$XDG_CACHE_HOME/thumbnails/<size>/<md5 of file URI>.png`, with
//! `Thumb::URI` and `Thumb::MTime` text chunks for validation. Reading and
//! writing the PNGs is the GUI's job; this module only knows where they go.

use std::path::PathBuf;

use md5::Digest as _;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Size {
    /// 128×128
    Normal,
    /// 256×256
    Large,
}

impl Size {
    pub fn pixels(self) -> i32 {
        match self {
            Size::Normal => 128,
            Size::Large => 256,
        }
    }

    fn dir_name(self) -> &'static str {
        match self {
            Size::Normal => "normal",
            Size::Large => "large",
        }
    }
}

pub fn cache_dir(size: Size) -> PathBuf {
    let base = std::env::var_os("XDG_CACHE_HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".cache")))
        .unwrap_or_else(|| PathBuf::from("."));
    base.join("thumbnails").join(size.dir_name())
}

/// Lower-case hex MD5 of the file URI, as the spec requires.
pub fn key(uri: &str) -> String {
    let digest = md5::Md5::digest(uri.as_bytes());
    digest.iter().map(|b| format!("{b:02x}")).collect()
}

pub fn cache_path(uri: &str, size: Size) -> PathBuf {
    cache_dir(size).join(format!("{}.png", key(uri)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spec_example_hash() {
        // From the Thumbnail Managing Standard's own example.
        assert_eq!(
            key("file:///home/jens/photos/me.png"),
            "c6ee772d9e49320e97ec29a7eb5b1697"
        );
    }

    #[test]
    fn paths_follow_xdg() {
        let uri = "file:///x";
        let path = cache_path(uri, Size::Normal);
        assert_eq!(
            path.file_name().unwrap().to_string_lossy(),
            format!("{}.png", key(uri))
        );
        assert!(path.parent().unwrap().ends_with("thumbnails/normal"));
        assert!(cache_dir(Size::Large).ends_with("thumbnails/large"));
        assert_eq!(Size::Large.pixels(), 256);
    }
}
