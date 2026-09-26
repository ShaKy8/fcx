//! Text decoding for the viewer: pick an encoding for a byte buffer, and
//! render a hex dump for anything that isn't text.

/// How a buffer was interpreted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Encoding {
    Utf8,
    Utf16Le,
    Utf16Be,
    /// Bytes mapped 1:1 to U+0000..U+00FF (no NULs, invalid UTF-8).
    Latin1,
    /// Contains NUL bytes; show as hex.
    Binary,
}

impl Encoding {
    pub fn label(self) -> &'static str {
        match self {
            Encoding::Utf8 => "UTF-8",
            Encoding::Utf16Le => "UTF-16 LE",
            Encoding::Utf16Be => "UTF-16 BE",
            Encoding::Latin1 => "Latin-1",
            Encoding::Binary => "binary",
        }
    }
}

/// Decode `bytes` by sniffing: BOMs first, then valid UTF-8, then NUL-free
/// Latin-1. Binary data returns `None` text. With `cut`, an incomplete
/// multibyte sequence at the very end (the caller read only a prefix) still
/// counts as UTF-8.
pub fn decode(bytes: &[u8], cut: bool) -> (Option<String>, Encoding) {
    if bytes.starts_with(&[0xFF, 0xFE]) {
        return (Some(utf16(&bytes[2..], true)), Encoding::Utf16Le);
    }
    if bytes.starts_with(&[0xFE, 0xFF]) {
        return (Some(utf16(&bytes[2..], false)), Encoding::Utf16Be);
    }
    let body = bytes.strip_prefix(&[0xEF, 0xBB, 0xBF]).unwrap_or(bytes);
    // NUL is valid UTF-8 but never appears in text files (UTF-16 was handled above).
    if body.contains(&0) {
        return (None, Encoding::Binary);
    }
    match std::str::from_utf8(body) {
        Ok(text) => (Some(text.to_owned()), Encoding::Utf8),
        Err(err) if cut && err.error_len().is_none() => (
            Some(String::from_utf8_lossy(body).into_owned()),
            Encoding::Utf8,
        ),
        Err(_) => (
            Some(body.iter().map(|&b| b as char).collect()),
            Encoding::Latin1,
        ),
    }
}

fn utf16(bytes: &[u8], little_endian: bool) -> String {
    let units: Vec<u16> = bytes
        .as_chunks::<2>()
        .0
        .iter()
        .map(|&pair| {
            if little_endian {
                u16::from_le_bytes(pair)
            } else {
                u16::from_be_bytes(pair)
            }
        })
        .collect();
    String::from_utf16_lossy(&units)
}

/// Classic 16-bytes-per-line dump: offset, hex, ASCII.
pub fn hex_dump(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 4);
    for (i, line) in bytes.chunks(16).enumerate() {
        out.push_str(&format!("{:08x}  ", i * 16));
        for (j, b) in line.iter().enumerate() {
            out.push_str(&format!("{b:02x} "));
            if j == 7 {
                out.push(' ');
            }
        }
        for j in line.len()..16 {
            out.push_str("   ");
            if j == 7 {
                out.push(' ');
            }
        }
        out.push(' ');
        for &b in line {
            out.push(if (0x20..0x7f).contains(&b) {
                b as char
            } else {
                '.'
            });
        }
        out.push('\n');
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_encodings() {
        let d = |b: &[u8]| decode(b, false);
        assert_eq!(
            d(b"plain ascii"),
            (Some("plain ascii".into()), Encoding::Utf8)
        );
        assert_eq!(
            d("caf\u{e9} ☕".as_bytes()),
            (Some("caf\u{e9} ☕".into()), Encoding::Utf8)
        );
        assert_eq!(d(b"\xEF\xBB\xBFbom"), (Some("bom".into()), Encoding::Utf8));
        assert_eq!(
            d(b"\xFF\xFEh\x00i\x00"),
            (Some("hi".into()), Encoding::Utf16Le)
        );
        assert_eq!(
            d(b"\xFE\xFF\x00h\x00i"),
            (Some("hi".into()), Encoding::Utf16Be)
        );
        assert_eq!(d(b"caf\xe9"), (Some("caf\u{e9}".into()), Encoding::Latin1));
        assert_eq!(d(b"\x00\x01binary"), (None, Encoding::Binary));
    }

    #[test]
    fn truncated_utf8_tail_is_still_utf8() {
        let bytes = "日本語".as_bytes();
        let cut = &bytes[..bytes.len() - 1];
        let (text, enc) = decode(cut, true);
        assert_eq!(enc, Encoding::Utf8);
        assert!(text.unwrap().starts_with("日本"));
        // Without the hint the same bytes are plain Latin-1 garbage, not UTF-8.
        assert_eq!(decode(cut, false).1, Encoding::Latin1);
    }

    #[test]
    fn hex_dump_layout() {
        let dump = hex_dump(b"Hello, world! \x00\xff\x01");
        let lines: Vec<&str> = dump.lines().collect();
        assert_eq!(lines.len(), 2);
        assert_eq!(
            lines[0],
            "00000000  48 65 6c 6c 6f 2c 20 77  6f 72 6c 64 21 20 00 ff  Hello, world! .."
        );
        assert!(lines[1].starts_with("00000010  01 "));
        assert!(lines[1].ends_with("  ."));
    }
}
