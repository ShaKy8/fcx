//! Display strings for entry metadata.

use crate::fs::{Entry, EntryKind};

/// Binary-unit size, e.g. `512 B`, `1.5 KiB`, `3.0 GiB`.
pub fn human_size(bytes: u64) -> String {
    const UNITS: [&str; 6] = ["KiB", "MiB", "GiB", "TiB", "PiB", "EiB"];
    if bytes < 1024 {
        return format!("{bytes} B");
    }
    let mut value = bytes as f64 / 1024.0;
    let mut unit = 0;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    format!("{value:.1} {}", UNITS[unit])
}

/// `ls -l` style mode string, e.g. `drwxr-xr-x`, including setuid/setgid/sticky.
pub fn mode_string(entry: &Entry) -> String {
    let type_char = match entry.kind {
        EntryKind::Dir => 'd',
        EntryKind::Symlink { .. } | EntryKind::BrokenSymlink => 'l',
        EntryKind::File => '-',
        EntryKind::Other => '?',
    };
    let m = entry.mode;
    let bit = |mask: u32, c: char| if m & mask != 0 { c } else { '-' };
    let special = |exec: u32, special: u32, set: char| match (m & exec != 0, m & special != 0) {
        (true, true) => set,
        (false, true) => set.to_ascii_uppercase(),
        (true, false) => 'x',
        (false, false) => '-',
    };
    [
        type_char,
        bit(0o400, 'r'),
        bit(0o200, 'w'),
        special(0o100, 0o4000, 's'),
        bit(0o040, 'r'),
        bit(0o020, 'w'),
        special(0o010, 0o2000, 's'),
        bit(0o004, 'r'),
        bit(0o002, 'w'),
        special(0o001, 0o1000, 't'),
    ]
    .iter()
    .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(kind: EntryKind, mode: u32) -> Entry {
        Entry {
            name: "x".into(),
            kind,
            size: 0,
            modified: None,
            mode,
            uid: 0,
            gid: 0,
        }
    }

    #[test]
    fn sizes() {
        assert_eq!(human_size(0), "0 B");
        assert_eq!(human_size(1023), "1023 B");
        assert_eq!(human_size(1536), "1.5 KiB");
        assert_eq!(human_size(3 * 1024 * 1024 * 1024), "3.0 GiB");
        assert_eq!(human_size(u64::MAX), "16.0 EiB");
    }

    #[test]
    fn modes() {
        assert_eq!(mode_string(&entry(EntryKind::Dir, 0o755)), "drwxr-xr-x");
        assert_eq!(mode_string(&entry(EntryKind::File, 0o644)), "-rw-r--r--");
        assert_eq!(mode_string(&entry(EntryKind::File, 0o4755)), "-rwsr-xr-x");
        assert_eq!(mode_string(&entry(EntryKind::Dir, 0o1777)), "drwxrwxrwt");
        assert_eq!(mode_string(&entry(EntryKind::File, 0o2644)), "-rw-r-Sr--");
        assert_eq!(
            mode_string(&entry(EntryKind::BrokenSymlink, 0o777)),
            "lrwxrwxrwx"
        );
    }
}
