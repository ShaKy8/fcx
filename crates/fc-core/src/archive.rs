//! Archives: recognise them by name, list and extract anything libarchive
//! reads (zip, tar.*, 7z, rar, iso, deb, …), and create zip / tar / tar.gz.
//! Extraction refuses entries that would escape the destination.

use std::ffi::{OsStr, OsString};
use std::fs;
use std::io::{self, Seek, Write};
use std::path::{Component, Path, PathBuf};

/// Lower-case suffixes (without the dot) treated as archives.
pub const EXTENSIONS: &[&str] = &[
    "zip", "jar", "xpi", "apk", "7z", "rar", "tar", "tgz", "tar.gz", "tbz2", "tar.bz2", "txz",
    "tar.xz", "tzst", "tar.zst", "cpio", "iso", "deb", "rpm", "ar", "cab", "lzh", "xar",
];

pub fn is_archive(name: &OsStr) -> bool {
    let lower = name.to_string_lossy().to_lowercase();
    EXTENSIONS
        .iter()
        .any(|ext| lower.len() > ext.len() + 1 && lower.ends_with(&format!(".{ext}")))
}

/// Name without the archive suffix (`photos.tar.gz` → `photos`).
pub fn stem(name: &OsStr) -> String {
    let display = name.to_string_lossy();
    let lower = display.to_lowercase();
    // Longest matching suffix wins so `tar.gz` beats `gz`.
    let mut best = 0;
    for ext in EXTENSIONS {
        let suffix = format!(".{ext}");
        if lower.ends_with(&suffix) && suffix.len() > best {
            best = suffix.len();
        }
    }
    display[..display.len() - best].to_owned()
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    pub path: String,
    pub is_dir: bool,
}

fn map_err(err: compress_tools::Error) -> io::Error {
    io::Error::other(err.to_string())
}

pub fn list(archive: &Path) -> io::Result<Vec<Entry>> {
    let file = fs::File::open(archive)?;
    let names = compress_tools::list_archive_files(file).map_err(map_err)?;
    Ok(names
        .into_iter()
        .map(|path| Entry {
            is_dir: path.ends_with('/'),
            path,
        })
        .collect())
}

/// An entry name is safe when it is relative and never climbs with `..`.
fn is_safe(name: &str) -> bool {
    let path = Path::new(name);
    !path.is_absolute()
        && path
            .components()
            .all(|c| matches!(c, Component::Normal(_) | Component::CurDir))
}

/// Extract everything into `dest` (created if missing).
pub fn extract(archive: &Path, dest: &Path) -> io::Result<()> {
    for entry in list(archive)? {
        if !is_safe(&entry.path) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!(
                    "archive entry {:?} would escape the destination",
                    entry.path
                ),
            ));
        }
    }
    fs::create_dir_all(dest)?;
    let file = fs::File::open(archive)?;
    compress_tools::uncompress_archive(file, dest, compress_tools::Ownership::Ignore)
        .map_err(map_err)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Format {
    Zip,
    Tar,
    TarGz,
}

impl Format {
    /// The format an output name implies, if any.
    pub fn from_name(name: &OsStr) -> Option<Format> {
        let lower = name.to_string_lossy().to_lowercase();
        if lower.ends_with(".tar.gz") || lower.ends_with(".tgz") {
            Some(Format::TarGz)
        } else if lower.ends_with(".tar") {
            Some(Format::Tar)
        } else if lower.ends_with(".zip") {
            Some(Format::Zip)
        } else {
            None
        }
    }

    pub fn extension(self) -> &'static str {
        match self {
            Format::Zip => "zip",
            Format::Tar => "tar",
            Format::TarGz => "tar.gz",
        }
    }
}

/// Create `archive` containing `items` (names inside `base`); folders recurse.
pub fn create(archive: &Path, format: Format, base: &Path, items: &[OsString]) -> io::Result<()> {
    let file = fs::File::create(archive)?;
    let result: io::Result<()> = match format {
        Format::Zip => write_zip(file, base, items).map(drop),
        Format::Tar => write_tar(file, base, items).map(drop),
        Format::TarGz => {
            let encoder = flate2::write::GzEncoder::new(file, flate2::Compression::default());
            write_tar(encoder, base, items).and_then(|enc| enc.finish().map(drop))
        }
    };
    if result.is_err() {
        let _ = fs::remove_file(archive);
    }
    result
}

fn write_tar<W: Write>(writer: W, base: &Path, items: &[OsString]) -> io::Result<W> {
    let mut builder = tar::Builder::new(writer);
    builder.follow_symlinks(false);
    for name in items {
        let path = base.join(name);
        let inside = Path::new(name);
        if path.is_dir() {
            builder.append_dir_all(inside, &path)?;
        } else {
            builder.append_path_with_name(&path, inside)?;
        }
    }
    builder.into_inner()
}

fn write_zip<W: Write + Seek>(writer: W, base: &Path, items: &[OsString]) -> io::Result<W> {
    let mut zip = zip::ZipWriter::new(writer);
    let options = zip::write::SimpleFileOptions::default()
        .compression_method(zip::CompressionMethod::Deflated)
        .unix_permissions(0o644);
    let dir_options = options.unix_permissions(0o755);
    let mut stack: Vec<(PathBuf, PathBuf)> = items
        .iter()
        .map(|n| (base.join(n), PathBuf::from(n)))
        .collect();
    stack.reverse();
    while let Some((path, inside)) = stack.pop() {
        let name = inside.to_string_lossy().into_owned();
        let meta = fs::symlink_metadata(&path)?;
        if meta.is_dir() {
            zip.add_directory(format!("{name}/"), dir_options)?;
            let mut children: Vec<fs::DirEntry> = fs::read_dir(&path)?.flatten().collect();
            children.sort_by_key(|e| e.file_name());
            for child in children.into_iter().rev() {
                stack.push((child.path(), inside.join(child.file_name())));
            }
        } else if meta.is_file() {
            zip.start_file(name, options)?;
            let mut file = fs::File::open(&path)?;
            io::copy(&mut file, &mut zip)?;
        }
        // Symlinks and special files are skipped.
    }
    zip.finish().map_err(io::Error::other)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tree(root: &Path) -> Vec<OsString> {
        fs::create_dir_all(root.join("src/dir/deep")).unwrap();
        fs::write(root.join("src/a.txt"), "alpha").unwrap();
        fs::write(root.join("src/dir/b.txt"), "bravo").unwrap();
        fs::write(root.join("src/dir/deep/c.bin"), vec![7u8; 5000]).unwrap();
        fs::write(root.join("src/top.md"), "# top").unwrap();
        vec!["dir".into(), "a.txt".into(), "top.md".into()]
    }

    fn read_all(dir: &Path) -> Vec<(String, Vec<u8>)> {
        let mut out = Vec::new();
        let mut stack = vec![dir.to_path_buf()];
        while let Some(d) = stack.pop() {
            for e in fs::read_dir(&d).unwrap().flatten() {
                if e.path().is_dir() {
                    stack.push(e.path());
                } else {
                    let rel = e
                        .path()
                        .strip_prefix(dir)
                        .unwrap()
                        .to_string_lossy()
                        .into_owned();
                    out.push((rel, fs::read(e.path()).unwrap()));
                }
            }
        }
        out.sort();
        out
    }

    #[test]
    fn names() {
        assert!(is_archive(OsStr::new("Photos.ZIP")));
        assert!(is_archive(OsStr::new("backup.tar.gz")));
        assert!(is_archive(OsStr::new("x.7z")));
        assert!(!is_archive(OsStr::new("zip")));
        assert!(!is_archive(OsStr::new("notes.txt")));
        assert_eq!(stem(OsStr::new("backup.tar.gz")), "backup");
        assert_eq!(stem(OsStr::new("Photos.ZIP")), "Photos");
        assert_eq!(Format::from_name(OsStr::new("a.tgz")), Some(Format::TarGz));
        assert_eq!(Format::from_name(OsStr::new("a.tar")), Some(Format::Tar));
        assert_eq!(Format::from_name(OsStr::new("a.ZIP")), Some(Format::Zip));
        assert_eq!(Format::from_name(OsStr::new("a.7z")), None);
    }

    #[test]
    fn roundtrip_all_formats() {
        for format in [Format::Zip, Format::Tar, Format::TarGz] {
            let tmp = tempfile::tempdir().unwrap();
            let items = tree(tmp.path());
            let archive = tmp.path().join(format!("out.{}", format.extension()));
            create(&archive, format, &tmp.path().join("src"), &items).unwrap();
            assert!(is_archive(archive.file_name().unwrap()));

            let listed = list(&archive).unwrap();
            let paths: Vec<&str> = listed.iter().map(|e| e.path.as_str()).collect();
            assert!(paths.contains(&"a.txt"), "{format:?}: {paths:?}");
            assert!(
                paths.iter().any(|p| p.starts_with("dir/deep/c.bin")),
                "{format:?}: {paths:?}"
            );

            let dest = tmp.path().join("out");
            extract(&archive, &dest).unwrap();
            assert_eq!(
                read_all(&dest),
                read_all(&tmp.path().join("src")),
                "{format:?}"
            );
        }
    }

    #[test]
    fn refuses_escaping_entries() {
        let tmp = tempfile::tempdir().unwrap();
        let archive = tmp.path().join("evil.zip");
        {
            let file = fs::File::create(&archive).unwrap();
            let mut zip = zip::ZipWriter::new(file);
            let options = zip::write::SimpleFileOptions::default();
            zip.start_file("../escape.txt", options).unwrap();
            zip.write_all(b"nope").unwrap();
            zip.finish().unwrap();
        }
        let dest = tmp.path().join("dest");
        let err = extract(&archive, &dest).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidData);
        assert!(!tmp.path().join("escape.txt").exists());
        assert!(!is_safe("/etc/passwd") && !is_safe("a/../../b") && is_safe("a/./b"));
    }

    #[test]
    fn create_failure_leaves_no_partial_file() {
        let tmp = tempfile::tempdir().unwrap();
        let archive = tmp.path().join("bad.zip");
        let err = create(&archive, Format::Zip, tmp.path(), &["missing".into()]).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::NotFound);
        assert!(!archive.exists());
    }
}
