//! Snapshot of a pane taken BEFORE the popup opens, handed to `pick` as a file.
//!
//! Why a file: the popup is a separate process, and capturing inside it happens a few ms
//! after the keypress, by which time the pane may have repainted (spinners, clocks, streaming
//! output). A screen can also contain secrets, so the file is created `0600`, with
//! `create_new` (never follows or overwrites an existing path), in the user's runtime dir
//! when there is one, and `pick` deletes it as soon as it has read it.

use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

fn dir() -> PathBuf {
    match std::env::var_os("XDG_RUNTIME_DIR") {
        Some(d) if Path::new(&d).is_dir() => PathBuf::from(d),
        _ => std::env::temp_dir(),
    }
}

/// Write `raw` to a fresh private file and return its path.
pub fn write(raw: &str) -> std::io::Result<PathBuf> {
    let nanos = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.subsec_nanos()).unwrap_or(0);
    for attempt in 0..8u32 {
        let path = dir().join(format!("regionsel-{}-{}-{}.ansi", std::process::id(), nanos, attempt));
        let mut opts = OpenOptions::new();
        opts.write(true).create_new(true);
        #[cfg(unix)]
        std::os::unix::fs::OpenOptionsExt::mode(&mut opts, 0o600);
        match opts.open(&path) {
            Ok(mut f) => {
                f.write_all(raw.as_bytes())?;
                return Ok(path);
            }
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e),
        }
    }
    Err(std::io::Error::new(std::io::ErrorKind::AlreadyExists, "no free snapshot file name"))
}

/// Read the snapshot and delete it, even if reading fails.
pub fn take(path: &Path) -> std::io::Result<String> {
    let raw = fs::read_to_string(path);
    let _ = fs::remove_file(path);
    raw
}

/// Number of screen rows in a `capture-pane` dump (one per line; a final newline is not a row).
pub fn count_rows(raw: &str) -> usize {
    if raw.is_empty() {
        return 0;
    }
    raw.strip_suffix('\n').unwrap_or(raw).split('\n').count()
}

/// "120x40" -> (120, 40)
pub fn parse_size(s: &str) -> Option<(u16, u16)> {
    let (w, h) = s.split_once('x')?;
    Some((w.parse().ok()?, h.parse().ok()?))
}

/// "5,12" -> (5, 12)  (x,y)
pub fn parse_cursor(s: &str) -> Option<(u16, u16)> {
    let (x, y) = s.split_once(',')?;
    Some((x.parse().ok()?, y.parse().ok()?))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn write_then_take_round_trips_and_deletes() {
        let p = write("hello \x1b[31mred\x1b[0m\n").unwrap();
        assert!(p.exists());
        assert_eq!(take(&p).unwrap(), "hello \x1b[31mred\x1b[0m\n");
        assert!(!p.exists());
    }

    #[cfg(unix)]
    #[test]
    fn snapshot_is_private_and_names_are_unique() {
        use std::os::unix::fs::PermissionsExt;
        let a = write("a").unwrap();
        let b = write("b").unwrap();
        assert_ne!(a, b);
        assert_eq!(fs::metadata(&a).unwrap().permissions().mode() & 0o777, 0o600);
        let _ = take(&a);
        let _ = take(&b);
    }

    #[test]
    fn take_removes_the_file_even_when_it_is_not_utf8() {
        let p = write("x").unwrap();
        fs::write(&p, [0xff, 0xfe]).unwrap();
        assert!(take(&p).is_err());
        assert!(!p.exists());
    }

    #[test]
    fn counts_rows_like_the_grid_parser_sees_them() {
        assert_eq!(count_rows(""), 0);
        assert_eq!(count_rows("a\nb\n"), 2);
        assert_eq!(count_rows("a\n\n"), 2); // a blank last row is still a row
        assert_eq!(count_rows("a\nb"), 2);
    }

    #[test]
    fn parses_size_and_cursor() {
        assert_eq!(parse_size("120x40"), Some((120, 40)));
        assert_eq!(parse_size("120"), None);
        assert_eq!(parse_size("ax4"), None);
        assert_eq!(parse_cursor("5,12"), Some((5, 12)));
        assert_eq!(parse_cursor("5;12"), None);
    }
}
