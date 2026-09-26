//! File checksums (FC's Ctrl+K): streamed MD5 / SHA-1 / SHA-256, and the
//! classic `<hash>  <name>` sum files that `sha256sum -c` understands.

use std::fmt::Write as _;
use std::io::{self, Read};
use std::path::{Path, PathBuf};

use md5::Digest as _;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Algorithm {
    Md5,
    Sha1,
    Sha256,
}

impl Algorithm {
    pub const ALL: [Algorithm; 3] = [Algorithm::Sha256, Algorithm::Sha1, Algorithm::Md5];

    pub fn label(self) -> &'static str {
        match self {
            Algorithm::Md5 => "MD5",
            Algorithm::Sha1 => "SHA-1",
            Algorithm::Sha256 => "SHA-256",
        }
    }

    /// Extension of a sum file for this algorithm.
    pub fn extension(self) -> &'static str {
        match self {
            Algorithm::Md5 => "md5",
            Algorithm::Sha1 => "sha1",
            Algorithm::Sha256 => "sha256",
        }
    }
}

fn hex(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        let _ = write!(out, "{b:02x}");
    }
    out
}

/// Lower-case hex digest of the file's contents.
pub fn compute(path: &Path, algorithm: Algorithm) -> io::Result<String> {
    let mut file = std::fs::File::open(path)?;
    let mut buf = vec![0u8; 256 * 1024];
    macro_rules! run {
        ($hasher:expr) => {{
            let mut hasher = $hasher;
            loop {
                let n = file.read(&mut buf)?;
                if n == 0 {
                    break;
                }
                hasher.update(&buf[..n]);
            }
            Ok(hex(&hasher.finalize()))
        }};
    }
    match algorithm {
        Algorithm::Md5 => run!(md5::Md5::new()),
        Algorithm::Sha1 => run!(sha1::Sha1::new()),
        Algorithm::Sha256 => run!(sha2::Sha256::new()),
    }
}

/// Writes `<hash>  <name>` lines to `dir/<stem>.<ext>` and returns the path.
pub fn write_sum_file(
    dir: &Path,
    stem: &str,
    algorithm: Algorithm,
    entries: &[(String, String)],
) -> io::Result<PathBuf> {
    let path = dir.join(format!("{stem}.{}", algorithm.extension()));
    let mut text = String::new();
    for (name, hash) in entries {
        text.push_str(hash);
        text.push_str("  ");
        text.push_str(name);
        text.push('\n');
    }
    std::fs::write(&path, text)?;
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_vectors() {
        let tmp = tempfile::tempdir().unwrap();
        let file = tmp.path().join("abc");
        std::fs::write(&file, b"abc").unwrap();
        assert_eq!(
            compute(&file, Algorithm::Md5).unwrap(),
            "900150983cd24fb0d6963f7d28e17f72"
        );
        assert_eq!(
            compute(&file, Algorithm::Sha1).unwrap(),
            "a9993e364706816aba3e25717850c26c9cd0d89d"
        );
        assert_eq!(
            compute(&file, Algorithm::Sha256).unwrap(),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        // Larger than one read buffer, to exercise streaming.
        let big = tmp.path().join("big");
        std::fs::write(&big, vec![b'a'; 1_000_000]).unwrap();
        assert_eq!(
            compute(&big, Algorithm::Sha256).unwrap(),
            "cdc76e5c9914fb9281a1c7e284d73e67f1809a48a497200e046d39ccc7112cd0"
        );
        assert!(compute(&tmp.path().join("missing"), Algorithm::Md5).is_err());
    }

    #[test]
    fn sum_file_format() {
        let tmp = tempfile::tempdir().unwrap();
        let path = write_sum_file(
            tmp.path(),
            "checksums",
            Algorithm::Sha256,
            &[
                ("a.txt".into(), "aa".into()),
                ("b c.txt".into(), "bb".into()),
            ],
        )
        .unwrap();
        assert_eq!(path, tmp.path().join("checksums.sha256"));
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "aa  a.txt\nbb  b c.txt\n"
        );
    }
}
