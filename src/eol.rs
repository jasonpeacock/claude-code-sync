//! Line endings. Archives always store LF, and record which files were CRLF. Restore writes CRLF
//! only on Windows, and only for those: a shell script written with LF has to stay LF to run.
//!
//! Binary files are passed through untouched, or a stray 0x0A inside a PNG would be corrupted.

const CR: u8 = b'\r';
const LF: u8 = b'\n';
const NUL: u8 = 0;

const SNIFF_BYTES: usize = 8000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LineEnding {
    Crlf,
    Lf,
}

impl LineEnding {
    pub fn native() -> Self {
        if cfg!(windows) {
            Self::Crlf
        } else {
            Self::Lf
        }
    }
}

/// A NUL byte in the first few KB means binary: the heuristic git uses. Anything stricter puts
/// UTF-8 text with emoji or typographic punctuation on the binary side, where it stops being
/// line-ending normalized at all.
pub fn is_binary(bytes: &[u8]) -> bool {
    bytes.iter().take(SNIFF_BYTES).any(|b| *b == NUL)
}

/// UTF-8 is ASCII-transparent, so working at the byte level cannot split a multi-byte character.
fn to_lf(bytes: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == CR {
            if bytes.get(index + 1) == Some(&LF) {
                index += 1;
            }
            out.push(LF);
        } else {
            out.push(bytes[index]);
        }
        index += 1;
    }
    out
}

fn to_crlf(lf_bytes: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(lf_bytes.len() * 2);
    for byte in lf_bytes {
        if *byte == LF {
            out.push(CR);
        }
        out.push(*byte);
    }
    out
}

/// Whether a text file was written with CRLF. Recorded on backup, because the archive stores LF
/// and the original ending is otherwise lost.
pub fn uses_crlf(bytes: &[u8]) -> bool {
    !is_binary(bytes) && bytes.windows(2).any(|pair| pair == [CR, LF])
}

pub fn normalize_for_archive(bytes: &[u8]) -> Vec<u8> {
    if is_binary(bytes) {
        return bytes.to_vec();
    }
    to_lf(bytes)
}

/// Normalizes first, so restoring an already-CRLF buffer never double-converts it.
pub fn denormalize_for_disk(bytes: &[u8], eol: LineEnding) -> Vec<u8> {
    if is_binary(bytes) {
        return bytes.to_vec();
    }
    let lf = to_lf(bytes);
    match eol {
        LineEnding::Lf => lf,
        LineEnding::Crlf => to_crlf(&lf),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn crlf_is_recorded_only_for_text_that_uses_it() {
        assert!(uses_crlf(b"a\r\nb\r\n"));
        assert!(!uses_crlf(b"a\nb\n"));
        assert!(!uses_crlf(b"\x00\r\n"));
    }

    #[test]
    fn converts_crlf_to_lf_for_the_archive() {
        assert_eq!(normalize_for_archive(b"a\r\nb\r\n"), b"a\nb\n");
    }

    #[test]
    fn leaves_lf_alone() {
        assert_eq!(normalize_for_archive(b"a\nb\n"), b"a\nb\n");
    }

    #[test]
    fn converts_a_lone_cr_to_lf() {
        assert_eq!(normalize_for_archive(b"a\rb"), b"a\nb");
    }

    #[test]
    fn writes_crlf_when_the_target_is_windows() {
        assert_eq!(
            denormalize_for_disk(b"a\nb\n", LineEnding::Crlf),
            b"a\r\nb\r\n"
        );
    }

    #[test]
    fn writes_lf_when_the_target_is_unix() {
        assert_eq!(denormalize_for_disk(b"a\nb\n", LineEnding::Lf), b"a\nb\n");
    }

    #[test]
    fn does_not_double_convert_an_already_crlf_buffer() {
        assert_eq!(denormalize_for_disk(b"a\r\nb", LineEnding::Crlf), b"a\r\nb");
    }

    #[test]
    fn a_text_file_survives_windows_to_unix_to_windows_byte_for_byte() {
        let original: &[u8] = b"line one\r\nline two\r\n";
        let in_archive = normalize_for_archive(original);
        let on_unix = denormalize_for_disk(&in_archive, LineEnding::Lf);
        let back = denormalize_for_disk(&normalize_for_archive(&on_unix), LineEnding::Crlf);
        assert_eq!(back, original);
    }

    #[test]
    fn detects_a_nul_byte_as_binary() {
        assert!(is_binary(&[0x4d, 0x5a, 0x00, 0x01]));
    }

    #[test]
    fn treats_normal_source_as_text() {
        assert!(!is_binary(b"export const x = 1\n"));
    }

    #[test]
    fn treats_utf8_punctuation_and_emoji_as_text() {
        assert!(!is_binary("rules — “quoted” ✅\n".as_bytes()));
    }

    #[test]
    fn treats_an_empty_file_as_text() {
        assert!(!is_binary(&[]));
    }

    #[test]
    fn never_alters_a_binary_payload() {
        let png = [0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, 0x00, 0x0d];
        assert_eq!(normalize_for_archive(&png), png);
        assert_eq!(denormalize_for_disk(&png, LineEnding::Crlf), png);
    }
}
