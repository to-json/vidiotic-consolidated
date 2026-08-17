//! A project and its clips as one file.
//!
//! A `.viproj` references its clips by relative path, so on its own it is half
//! a project — the half that points at the other half. Natively that is fine,
//! because the other half is sitting in a directory next to it. A browser has
//! no directory to hand over and can give back exactly one file, so the archive
//! is not a convenience there: without it an export is a set of files that no
//! longer point at each other.
//!
//! This lives in `vidiotic-core` because *both* browser shells need it and
//! neither can depend on the other. `/chop` writes a bundle of the clips it
//! baked; `/play` writes a bundle of the session it is running. One
//! implementation, so the two cannot drift into two archive formats that a
//! reader has to tell apart.

/// CRC-32 (the IEEE polynomial), computed a bit at a time.
///
/// No table and no dependency: a project is a handful of files, and a bundle
/// that carries clips is dominated by moving their bytes by several orders of
/// magnitude. A 1 KiB lookup table would be optimising the wrong end of this.
fn crc32(data: &[u8]) -> u32 {
    let mut crc = !0u32;
    for &b in data {
        crc ^= u32::from(b);
        for _ in 0..8 {
            let mask = (crc & 1).wrapping_neg();
            crc = (crc >> 1) ^ (0xEDB8_8320 & mask);
        }
    }
    !crc
}

/// Why a bundle could not be written.
///
/// Every one of these is a limit of the zip format's 32- and 16-bit header
/// fields, not of this writer: the answer to any of them is zip64, not a wider
/// cast. They exist as errors because the alternative — saturating the field
/// and writing the archive anyway, which is what this used to do — hands
/// somebody a file their zip reader rejects, at the end of a bake, with no
/// indication that anything went wrong.
#[derive(Debug, PartialEq, Eq)]
pub enum ZipErr {
    /// An entry, or the archive itself, exceeded the 4 GiB a `u32` size field
    /// can name.
    TooLarge,
    /// More than 65535 entries — more than the count field can name.
    TooManyEntries,
    /// An entry name longer than the 65535 bytes its length field can name.
    NameTooLong,
}

impl std::fmt::Display for ZipErr {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::TooLarge => write!(f, "bundle exceeds the 4 GiB zip limit"),
            Self::TooManyEntries => write!(f, "bundle has more than 65535 entries"),
            Self::NameTooLong => write!(f, "an entry name exceeds 65535 bytes"),
        }
    }
}
impl std::error::Error for ZipErr {}

/// Pack `entries` (name, bytes) into a **stored** — uncompressed — zip.
///
/// Store rather than deflate, and that is not laziness: a `.mov` full of Hap1
/// is snappy-compressed already, so deflating it costs CPU to produce a
/// slightly larger file. The `.viproj` is a few KB of text and compresses well,
/// but not enough to be worth carrying a deflate implementation for.
///
/// # Errors
/// [`ZipErr`] when the archive would not fit the format's fixed-width header
/// fields — see there for why that is an error rather than a truncation.
pub fn zip(entries: &[(String, Vec<u8>)]) -> Result<Vec<u8>, ZipErr> {
    let mut out = Vec::new();
    let mut central = Vec::new();

    for (name, data) in entries {
        let offset = u32::try_from(out.len()).map_err(|_| ZipErr::TooLarge)?;
        let crc = crc32(data);
        let size = u32::try_from(data.len()).map_err(|_| ZipErr::TooLarge)?;
        let name_bytes = name.as_bytes();
        let name_len = u16::try_from(name_bytes.len()).map_err(|_| ZipErr::NameTooLong)?;

        // Local file header. Version 2.0, no flags, method 0 (stored), and a
        // zeroed DOS timestamp: a bake is deterministic and a clock would be
        // the one thing making two exports of one session differ.
        out.extend_from_slice(b"PK\x03\x04");
        out.extend_from_slice(&20u16.to_le_bytes());
        out.extend_from_slice(&0u16.to_le_bytes());
        out.extend_from_slice(&0u16.to_le_bytes());
        out.extend_from_slice(&0u16.to_le_bytes()); // time
        out.extend_from_slice(&0u16.to_le_bytes()); // date
        out.extend_from_slice(&crc.to_le_bytes());
        out.extend_from_slice(&size.to_le_bytes());
        out.extend_from_slice(&size.to_le_bytes());
        out.extend_from_slice(&name_len.to_le_bytes());
        out.extend_from_slice(&0u16.to_le_bytes()); // extra field length
        out.extend_from_slice(name_bytes);
        out.extend_from_slice(data);

        central.extend_from_slice(b"PK\x01\x02");
        central.extend_from_slice(&20u16.to_le_bytes()); // version made by
        central.extend_from_slice(&20u16.to_le_bytes()); // version needed
        central.extend_from_slice(&0u16.to_le_bytes());
        central.extend_from_slice(&0u16.to_le_bytes());
        central.extend_from_slice(&0u16.to_le_bytes());
        central.extend_from_slice(&0u16.to_le_bytes());
        central.extend_from_slice(&crc.to_le_bytes());
        central.extend_from_slice(&size.to_le_bytes());
        central.extend_from_slice(&size.to_le_bytes());
        central.extend_from_slice(&name_len.to_le_bytes());
        central.extend_from_slice(&0u16.to_le_bytes()); // extra
        central.extend_from_slice(&0u16.to_le_bytes()); // comment
        central.extend_from_slice(&0u16.to_le_bytes()); // disk number
        central.extend_from_slice(&0u16.to_le_bytes()); // internal attrs
        central.extend_from_slice(&0u32.to_le_bytes()); // external attrs
        central.extend_from_slice(&offset.to_le_bytes());
        central.extend_from_slice(name_bytes);
    }

    let central_offset = u32::try_from(out.len()).map_err(|_| ZipErr::TooLarge)?;
    let central_size = u32::try_from(central.len()).map_err(|_| ZipErr::TooLarge)?;
    let count = u16::try_from(entries.len()).map_err(|_| ZipErr::TooManyEntries)?;
    out.extend_from_slice(&central);
    out.extend_from_slice(b"PK\x05\x06");
    out.extend_from_slice(&0u16.to_le_bytes()); // this disk
    out.extend_from_slice(&0u16.to_le_bytes()); // disk with central dir
    out.extend_from_slice(&count.to_le_bytes());
    out.extend_from_slice(&count.to_le_bytes());
    out.extend_from_slice(&central_size.to_le_bytes());
    out.extend_from_slice(&central_offset.to_le_bytes());
    out.extend_from_slice(&0u16.to_le_bytes()); // comment length
    Ok(out)
}

/// A file name safe to put in an archive, or to name a project with.
///
/// Deliberately narrow: what comes in is a name a visitor typed into a text
/// field, and what it becomes is an entry in an archive somebody else's zip
/// reader will walk and a file their filesystem will hold. Alphanumerics,
/// dashes and underscores are the intersection of everything that survives
/// that trip unchanged.
#[must_use]
pub fn sanitize(name: &str) -> String {
    let s: String = name
        .chars()
        .map(|c| if c.is_alphanumeric() || c == '-' || c == '_' { c } else { '_' })
        .collect();
    if s.is_empty() {
        "span".into()
    } else {
        s
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(target_arch = "wasm32")]
    use wasm_bindgen_test::wasm_bindgen_test as test;

    #[test]
    fn crc32_matches_the_standard_check_value() {
        assert_eq!(crc32(b"123456789"), 0xCBF4_3926);
    }

    /// The structure a reader actually walks: the end record has to point at
    /// the central directory, and each central entry at its local header.
    #[test]
    fn the_zip_central_directory_points_at_the_local_headers() {
        let entries = vec![
            ("p.viproj".to_string(), b"(version: 3)".to_vec()),
            ("clips/a.mov".to_string(), vec![7u8; 300]),
        ];
        let z = zip(&entries).unwrap();

        let end = z.len() - 22;
        assert_eq!(&z[end..end + 4], b"PK\x05\x06");
        let count = u16::from_le_bytes([z[end + 10], z[end + 11]]);
        assert_eq!(count, 2);
        let size = u32::from_le_bytes(z[end + 12..end + 16].try_into().unwrap()) as usize;
        let offset = u32::from_le_bytes(z[end + 16..end + 20].try_into().unwrap()) as usize;
        assert_eq!(offset + size, end, "the central directory must abut the end record");
        assert_eq!(&z[offset..offset + 4], b"PK\x01\x02");

        // Each central entry's local-header offset must land on a signature,
        // and the name recorded there must match.
        let mut p = offset;
        for (name, data) in &entries {
            assert_eq!(&z[p..p + 4], b"PK\x01\x02");
            let local = u32::from_le_bytes(z[p + 42..p + 46].try_into().unwrap()) as usize;
            assert_eq!(&z[local..local + 4], b"PK\x03\x04");
            let n = u16::from_le_bytes([z[local + 26], z[local + 27]]) as usize;
            assert_eq!(&z[local + 30..local + 30 + n], name.as_bytes());
            let stored = u32::from_le_bytes(z[local + 18..local + 22].try_into().unwrap()) as usize;
            assert_eq!(stored, data.len());
            assert_eq!(&z[local + 30 + n..local + 30 + n + data.len()], data.as_slice());
            p += 46 + name.len();
        }
    }

    #[test]
    fn an_empty_archive_is_still_a_valid_one() {
        let z = zip(&[]).unwrap();
        assert_eq!(z.len(), 22);
        assert_eq!(&z[0..4], b"PK\x05\x06");
    }

    /// A name that cannot fit the header field is an error, not a truncated
    /// name field pointing at the wrong bytes.
    #[test]
    fn an_overlong_entry_name_is_refused() {
        let entries = vec![("n".repeat(70_000), b"x".to_vec())];
        assert_eq!(zip(&entries), Err(ZipErr::NameTooLong));
    }

    #[test]
    fn sanitize_keeps_what_survives_a_filesystem() {
        assert_eq!(sanitize("friday night"), "friday_night");
        assert_eq!(sanitize("set-01_final"), "set-01_final");
        // Every separator and dot becomes an underscore, so a name that tried
        // to be a path stops being one.
        assert_eq!(sanitize("../../etc/passwd"), "______etc_passwd");
        assert_eq!(sanitize(""), "span");
    }
}
