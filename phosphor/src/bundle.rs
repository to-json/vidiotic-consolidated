//! Where the app lives on disk — and how the front ends of a family find each
//! other once they are inside a macOS `.app`.
//!
//! Running from `cargo` and running from a bundle differ in three ways that
//! bite every one of these apps:
//!
//! - **Siblings move.** In a target dir the tools sit next to each other; in a
//!   bundle they are nested helper apps under `Contents/Library`. [`helper`]
//!   resolves both, so a caller keeps one code path.
//! - **Nothing reads stderr.** A Finder launch has no terminal. `init_logging`
//!   tees to `~/Library/Logs/<family>/` when bundled — behind the `logging`
//!   feature, since it is the one part of this module that costs dependencies.
//! - **Argv is not yours alone.** Launch Services may prepend a `-psn_…`
//!   process-serial argument. [`args`] drops it before your parser chokes.
//!
//! Everything outside `logging` is `std`-only and degrades to sensible
//! non-macOS behaviour, so it compiles anywhere the toolkit does — including
//! wasm32, where a front end takes the path resolution and declines the logger.

use std::ffi::OsString;
#[cfg(feature = "logging")]
use std::io::Write;
use std::path::{Path, PathBuf};

/// The directory holding the running executable.
#[must_use]
pub fn exe_dir() -> Option<PathBuf> {
    std::env::current_exe()
        .ok()?
        .parent()
        .map(Path::to_path_buf)
}

/// `Foo.app/Contents` when the running executable is `…/Foo.app/Contents/MacOS/bin`,
/// otherwise `None`. This is the one test for "am I bundled" — everything else
/// (resources, helpers, log destination) hangs off it.
#[must_use]
pub fn contents() -> Option<PathBuf> {
    let dir = exe_dir()?;
    if dir.file_name()? != "MacOS" {
        return None;
    }
    let contents = dir.parent()?;
    (contents.file_name()? == "Contents"
        && contents
            .parent()
            .and_then(|p| p.extension())
            .is_some_and(|e| e == "app"))
    .then(|| contents.to_path_buf())
}

/// Whether this process is running from inside a `.app`.
#[must_use]
pub fn is_bundled() -> bool {
    contents().is_some()
}

/// `Contents/Resources`, when bundled.
#[must_use]
pub fn resources() -> Option<PathBuf> {
    contents().map(|c| c.join("Resources"))
}

/// Locate a sibling front end's executable.
///
/// `app` is the nested helper's bundle name without `.app` (`"Vidiotic Prep"`),
/// `bin` its executable (`"vidiotic-prep"`). Tried in order:
///
/// 1. `Contents/Library/<app>.app/Contents/MacOS/<bin>` — the bundled layout.
///    Note this is the *inner* executable, not the `.app`: launching it
///    directly still gives the child its own bundle identity (`NSBundle`
///    derives that from the executable's path) while letting the parent hand
///    down environment variables, which `open(1)` would not.
/// 2. `<exe_dir>/<bin>` — the cargo target-dir layout.
/// 3. `<bin>` bare, for `$PATH` to resolve.
#[must_use]
pub fn helper(app: &str, bin: &str) -> PathBuf {
    if let Some(c) = contents() {
        let nested = c
            .join("Library")
            .join(format!("{app}.app"))
            .join("Contents/MacOS")
            .join(bin);
        if nested.exists() {
            return nested;
        }
    }
    if let Some(sibling) = exe_dir().map(|d| d.join(bin)) {
        if sibling.exists() {
            return sibling;
        }
    }
    PathBuf::from(bin)
}

fn home() -> PathBuf {
    std::env::var_os("HOME").map_or_else(|| PathBuf::from("/tmp"), PathBuf::from)
}

/// `~/Library/Application Support/<family>` (created on demand): user-writable
/// state shared by the family's apps. Deliberately *not* inside the bundle —
/// writing there breaks its code signature.
#[must_use]
pub fn data_dir(family: &str) -> PathBuf {
    let dir = if cfg!(target_os = "macos") {
        home().join("Library/Application Support").join(family)
    } else {
        std::env::var_os("XDG_DATA_HOME")
            .map_or_else(|| home().join(".local/share"), PathBuf::from)
            .join(family.to_lowercase())
    };
    let _ = std::fs::create_dir_all(&dir);
    dir
}

/// `~/Library/Logs/<family>` (created on demand).
#[must_use]
pub fn logs_dir(family: &str) -> PathBuf {
    let dir = if cfg!(target_os = "macos") {
        home().join("Library/Logs").join(family)
    } else {
        data_dir(family).join("logs")
    };
    let _ = std::fs::create_dir_all(&dir);
    dir
}

/// Copy `src` into `dst` recursively, skipping any file that already exists —
/// a seed, not a sync. Used to plant the shipped shader library in the user's
/// data dir on first run so it can be livecoded without touching the bundle.
///
/// # Errors
/// Propagates the first I/O error from creating a directory or copying a file.
pub fn seed_dir(src: &Path, dst: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(dst)?;
    for entry in std::fs::read_dir(src)? {
        let entry = entry?;
        let (from, to) = (entry.path(), dst.join(entry.file_name()));
        if entry.file_type()?.is_dir() {
            seed_dir(&from, &to)?;
        } else if !to.exists() {
            std::fs::copy(&from, &to)?;
        }
    }
    Ok(())
}

/// The process arguments with Launch Services' `-psn_0_…` process-serial
/// argument removed. Finder-launched apps get it prepended on some macOS
/// versions, and an unsuspecting `clap` or `args().nth(1)` treats it as input.
#[must_use]
pub fn args() -> Vec<OsString> {
    std::env::args_os()
        .filter(|a| !a.to_string_lossy().starts_with("-psn_"))
        .collect()
}

/// A writer that fans one log stream out to two sinks.
#[cfg(feature = "logging")]
struct Tee(Box<dyn Write + Send>, Box<dyn Write + Send>);

#[cfg(feature = "logging")]
impl Write for Tee {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        let _ = self.1.write_all(buf);
        self.0.write(buf)
    }
    fn flush(&mut self) -> std::io::Result<()> {
        let _ = self.1.flush();
        self.0.flush()
    }
}

/// Log cap before an existing file is discarded at startup. A VJ session logs
/// steadily; nobody wants an unbounded file in `~/Library/Logs`.
#[cfg(feature = "logging")]
const LOG_LIMIT: u64 = 8 * 1024 * 1024;

/// Seconds-since-epoch rendered as `YYYY-MM-DDTHH:MM:SSZ`.
///
/// This exists so `env_logger` can be taken without its `humantime` feature,
/// whose only job is this line and which costs `jiff` (~105k LOC) to do it.
/// The bundle log is persistent and append-only, so its lines have to carry a
/// date, not just a wall time.
///
/// Civil-date conversion via days-from-epoch (Howard Hinnant's algorithm,
/// shifted to a March-based year so the leap day lands at the end). UTC only —
/// a log file that a user might mail in is better off unambiguous than local.
#[cfg(feature = "logging")]
fn format_utc(secs: u64) -> String {
    let (days, rem) = (secs / 86_400, secs % 86_400);
    let (h, mi, s) = (rem / 3600, (rem % 3600) / 60, rem % 60);

    let z = days as i64 + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);

    format!("{y:04}-{m:02}-{d:02}T{h:02}:{mi:02}:{s:02}Z")
}

/// Initialise `env_logger`, teeing to `~/Library/Logs/<family>/<name>.log`
/// when bundled (where stderr goes nowhere) and to stderr alone otherwise.
/// `RUST_LOG` still governs filtering; `default` is the fallback filter.
///
/// Call once, first thing in `main`.
#[cfg(feature = "logging")]
pub fn init_logging(family: &str, name: &str, default: &str) {
    let mut builder =
        env_logger::Builder::from_env(env_logger::Env::default().default_filter_or(default));
    // Replaces what env_logger's `humantime` feature would have emitted. Same
    // shape as its default line: `[<ts> <LEVEL> <target>] <msg>`.
    builder.format(|f, record| {
        let secs = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_secs());
        writeln!(
            f,
            "[{} {} {}] {}",
            format_utc(secs),
            record.level(),
            record.target(),
            record.args()
        )
    });
    if is_bundled() {
        let path = logs_dir(family).join(format!("{name}.log"));
        if std::fs::metadata(&path).is_ok_and(|m| m.len() > LOG_LIMIT) {
            let _ = std::fs::remove_file(&path);
        }
        if let Ok(file) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
        {
            builder.target(env_logger::Target::Pipe(Box::new(Tee(
                Box::new(std::io::stderr()),
                Box::new(file),
            ))));
        }
    }
    let _ = builder.try_init();
    if let Some(c) = contents() {
        log::info!("{name}: bundled at {}", c.display());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// [`format_utc`] is hand-rolled civil-date arithmetic standing in for what
    /// `env_logger`'s `humantime` feature used to do, so it gets pinned against
    /// known-good values — including the boundaries where this class of
    /// algorithm goes wrong: a day rollover, a leap day, and a century year
    /// that *is* a leap year (2000) versus one that is not (2100).
    #[cfg(feature = "logging")]
    #[test]
    fn format_utc_matches_known_timestamps() {
        for (secs, want) in [
            (0u64, "1970-01-01T00:00:00Z"),
            (1, "1970-01-01T00:00:01Z"),
            (86_399, "1970-01-01T23:59:59Z"),
            (86_400, "1970-01-02T00:00:00Z"),
            (951_782_400, "2000-02-29T00:00:00Z"),
            (1_000_000_000, "2001-09-09T01:46:40Z"),
            (1_751_000_000, "2025-06-27T04:53:20Z"),
            (1_767_225_600, "2026-01-01T00:00:00Z"),
            (4_102_444_800, "2100-01-01T00:00:00Z"),
        ] {
            assert_eq!(format_utc(secs), want, "at {secs}");
        }
    }

    #[test]
    fn helper_falls_back_to_a_bare_name() {
        // Nothing named this exists next to the test binary or in a bundle.
        assert_eq!(
            helper("No Such App", "definitely-not-a-real-binary"),
            PathBuf::from("definitely-not-a-real-binary")
        );
    }

    #[test]
    fn psn_argument_is_dropped() {
        // args() reads the real argv, so exercise the predicate it applies.
        let raw = ["vidiotic", "-psn_0_1234567", "/tmp/a.viproj"];
        let kept: Vec<_> = raw.iter().filter(|a| !a.starts_with("-psn_")).collect();
        assert_eq!(kept, ["vidiotic", "/tmp/a.viproj"].iter().collect::<Vec<_>>());
    }

    #[test]
    fn seed_skips_files_that_already_exist() {
        let tmp = std::env::temp_dir().join(format!("phosphor-seed-{}", std::process::id()));
        let (src, dst) = (tmp.join("src"), tmp.join("dst"));
        std::fs::create_dir_all(src.join("sub")).unwrap();
        std::fs::write(src.join("a.frag"), "shipped").unwrap();
        std::fs::write(src.join("sub/b.frag"), "shipped").unwrap();
        std::fs::create_dir_all(&dst).unwrap();
        std::fs::write(dst.join("a.frag"), "mine").unwrap();

        seed_dir(&src, &dst).unwrap();

        assert_eq!(std::fs::read_to_string(dst.join("a.frag")).unwrap(), "mine");
        assert_eq!(
            std::fs::read_to_string(dst.join("sub/b.frag")).unwrap(),
            "shipped"
        );
        let _ = std::fs::remove_dir_all(&tmp);
    }
}
