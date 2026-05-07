//! Window capture via Apple's `/usr/sbin/screencapture` CLI.
//!
//! v0.1.1 uses subprocess shellout for simplicity. Each capture spawns
//! `screencapture -l <wid> -x -o <path>` (~50-100ms wall-clock per call,
//! Screen-Recording-permission gated). This is fine for the one-shot
//! hello-world flow but not for continuous loops — when v0.2+ moves to
//! per-tick capture, migrate to `objc2-screen-capture-kit` per
//! [TODOS.md](../TODOS.md) P1 (zero subprocess overhead, same
//! permission gate).
//!
//! Why CLI for now: zero new dependencies, no Xcode requirement
//! (the `screencapturekit` Rust crate's build script needs the full
//! Xcode SDK; `objc2-screen-capture-kit` doesn't but is a non-trivial
//! integration). The CLI already exists on every macOS install.

use std::ffi::OsString;
use std::path::Path;
use std::process::Command;

use crate::error::{BotError, Result};

/// `/usr/sbin/screencapture` lives in macOS's system path. Hardcoding
/// the absolute path avoids `PATH` resolution surprises (e.g., if a
/// user has a script named `screencapture` shadowing the system one).
const SCREENCAPTURE_BIN: &str = "/usr/sbin/screencapture";

/// Pure: build the argv for `screencapture` capturing one window to a file.
///
/// Flags:
///   `-l <window_id>`  capture this CG window only
///   `-x`              silent (no shutter sound)
///   `-o`              omit the window shadow (tighter framing for
///                     downstream template matching)
///
/// Output format is inferred from the path extension by `screencapture`
/// (we use `.png`).
fn screencapture_args(window_id: u32, output_path: &Path) -> Vec<OsString> {
    vec![
        OsString::from("-l"),
        OsString::from(window_id.to_string()),
        OsString::from("-x"),
        OsString::from("-o"),
        output_path.as_os_str().to_owned(),
    ]
}

/// Live wrapper: capture the given CG window to `output_path` via the
/// system `screencapture` binary. Thin shim over [`capture_with_bin`].
pub fn capture_window(window_id: u32, output_path: &Path) -> Result<()> {
    capture_with_bin(SCREENCAPTURE_BIN, window_id, output_path)
}

/// Bin-parameterized capture so unit tests can drive the success / non-zero
/// exit / spawn-failure branches against `/usr/bin/true`, `/usr/bin/false`,
/// and a non-existent path — without needing the real screencapture binary
/// or macOS Screen Recording permission.
///
/// Pre-conditions and post-conditions worth the noise:
///
///   * **Refuse pre-existing symlinks at `output_path`.** `screencapture`
///     follows symlinks; without this gate, a pre-placed symlink would let
///     the subprocess write PNG bytes through to any file the user can
///     write (`~/.ssh/authorized_keys`, `/etc/hosts`, etc.). Closes the
///     local-attacker hazard surfaced by /review F2.
///   * **0-byte output post-check.** When Screen Recording is revoked
///     between `permissions::check_screen_recording` and this call,
///     `screencapture` writes a 0-byte file and exits 0. Without this
///     check, the bot would log `"captured RoK window"` and exit 0 while
///     v0.1.2 would later fail with a confusing image-decode error. We
///     turn that silent-corrupt case into a clear `CaptureFailed` at the
///     actual failure site. Closes /review F1.
///
/// The window-ID TOCTOU concern (a stale `window_id` after RoK closes
/// mid-flow) is **not** addressed here — the v0.2 migration to
/// `objc2-screen-capture-kit` reshapes the capture pipeline, and any
/// re-validation we add now is throwaway. See [TODOS.md](../TODOS.md).
fn capture_with_bin(bin: &str, window_id: u32, output_path: &Path) -> Result<()> {
    if output_path
        .symlink_metadata()
        .is_ok_and(|m| m.file_type().is_symlink())
    {
        tracing::warn!(
            target: "rok_bot",
            path = %output_path.display(),
            "refusing to capture: output path is a pre-existing symlink"
        );
        return Err(BotError::CaptureFailed { exit_code: None });
    }

    let args = screencapture_args(window_id, output_path);
    let status = Command::new(bin).args(&args).status().map_err(|err| {
        // Preserve the io::Error in stderr so operators can distinguish
        // ENOENT (binary missing), EACCES (sandbox denial), EAGAIN (spawn
        // pressure) — without expanding BotError's surface.
        tracing::warn!(
            target: "rok_bot",
            bin,
            io_error = %err,
            "screencapture spawn failed"
        );
        BotError::CaptureFailed { exit_code: None }
    })?;
    if !status.success() {
        return Err(BotError::CaptureFailed {
            exit_code: status.code(),
        });
    }

    // 0-byte output → TCC race. Surface it as CaptureFailed{Some(0)}.
    let written = output_path.metadata().map(|m| m.len()).unwrap_or(0);
    if written == 0 {
        tracing::warn!(
            target: "rok_bot",
            path = %output_path.display(),
            "screencapture exited 0 but output is 0 bytes \
             (likely Screen Recording revoked mid-flow)"
        );
        return Err(BotError::CaptureFailed { exit_code: Some(0) });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn args_include_window_id_flag() {
        let args = screencapture_args(12345, Path::new("out.png"));
        let strs: Vec<&str> = args.iter().filter_map(|s| s.to_str()).collect();
        assert_eq!(strs.first().copied(), Some("-l"));
        assert_eq!(strs.get(1).copied(), Some("12345"));
    }

    #[test]
    fn args_include_silent_and_no_shadow_flags() {
        let args = screencapture_args(1, Path::new("out.png"));
        let strs: Vec<&str> = args.iter().filter_map(|s| s.to_str()).collect();
        assert!(
            strs.contains(&"-x"),
            "silent flag (-x) should be present: {strs:?}"
        );
        assert!(
            strs.contains(&"-o"),
            "no-shadow flag (-o) should be present: {strs:?}"
        );
    }

    #[test]
    fn args_end_with_output_path() {
        let path = PathBuf::from("/tmp/rok.png");
        let args = screencapture_args(42, &path);
        let last = args.last().expect("at least one arg");
        assert_eq!(last.to_str(), Some("/tmp/rok.png"));
    }

    #[test]
    fn args_preserve_spaces_in_path_through_osstring() {
        // OsString preserves the literal path; no shell-escaping issues
        // because we pass argv directly to Command::args, not through a shell.
        // Note: this asserts Rust round-trips the path correctly — it does NOT
        // assert that screencapture itself accepts spaced paths (it does, but
        // that's an Apple-side property exercised only by live runs).
        let path = PathBuf::from("/tmp/rok captures/last.png");
        let args = screencapture_args(7, &path);
        let last = args.last().expect("at least one arg");
        assert_eq!(last.to_str(), Some("/tmp/rok captures/last.png"));
    }

    #[test]
    fn args_handle_large_window_id() {
        // CGWindowID is u32; verify max value renders as decimal.
        let args = screencapture_args(u32::MAX, Path::new("x.png"));
        let strs: Vec<&str> = args.iter().filter_map(|s| s.to_str()).collect();
        assert_eq!(strs.get(1).copied(), Some("4294967295"));
    }

    #[test]
    fn args_handle_window_id_zero() {
        // Boundary: u32 zero is a valid CGWindowID value space-wise. Verify
        // it renders as "0" and doesn't get elided or treated as "no window".
        let args = screencapture_args(0, Path::new("x.png"));
        let strs: Vec<&str> = args.iter().filter_map(|s| s.to_str()).collect();
        assert_eq!(strs.get(1).copied(), Some("0"));
    }

    #[test]
    fn args_preserve_empty_path() {
        // Doesn't make semantic sense to screencapture, but the argv builder
        // shouldn't silently elide an empty OsStr — caller's job to validate.
        let args = screencapture_args(1, Path::new(""));
        let last = args.last().expect("at least one arg");
        assert_eq!(last.as_os_str(), std::ffi::OsStr::new(""));
    }

    #[cfg(unix)]
    #[test]
    fn args_preserve_non_utf8_path_bytes() {
        // OsString allows non-UTF-8 byte sequences on Unix. Future filtering
        // through to_str() (or to_string_lossy) would silently mangle these;
        // this test pins the as-bytes invariant.
        use std::ffi::OsStr;
        use std::os::unix::ffi::OsStrExt;
        let raw: &[u8] = &[0xFF, b'.', b'p', b'n', b'g'];
        let path = Path::new(OsStr::from_bytes(raw));
        let args = screencapture_args(1, path);
        let last = args.last().expect("at least one arg");
        assert_eq!(last.as_os_str().as_bytes(), raw);
    }

    /// Each test gets its own tmpfile to avoid cross-test interference under
    /// `cargo test`'s default parallel runner. Filenames embed the test's
    /// purpose so a stale leak is easy to identify.
    fn unique_tmp(stem: &str) -> PathBuf {
        std::env::temp_dir().join(format!("rok-bot-capture-test-{stem}.png"))
    }

    #[test]
    fn capture_with_bin_returns_capture_failed_on_nonzero_exit() {
        // /bin/false exits 1 without writing anything. Pre-write a non-empty
        // file so the post-condition len>0 check would otherwise pass — that
        // way we know the non-zero exit is what triggered the failure path.
        let tmp = unique_tmp("nonzero-exit");
        std::fs::write(&tmp, b"\x89PNG\r\n\x1a\n").expect("seed tmp");
        let err = capture_with_bin("/usr/bin/false", 1, &tmp).expect_err("expected CaptureFailed");
        drop(std::fs::remove_file(&tmp));
        match err {
            BotError::CaptureFailed { exit_code } => assert_eq!(exit_code, Some(1)),
            other => panic!("expected CaptureFailed{{Some(1)}}, got {other:?}"),
        }
    }

    #[test]
    fn capture_with_bin_returns_capture_failed_when_spawn_fails() {
        // ENOENT path → Command::status() returns Err → mapped to None.
        let tmp = unique_tmp("spawn-fail");
        drop(std::fs::remove_file(&tmp));
        let err = capture_with_bin("/no/such/screencapture", 1, &tmp)
            .expect_err("expected CaptureFailed");
        match err {
            BotError::CaptureFailed { exit_code } => assert_eq!(exit_code, None),
            other => panic!("expected CaptureFailed{{None}}, got {other:?}"),
        }
    }

    #[test]
    fn capture_with_bin_succeeds_when_post_condition_holds() {
        // /bin/true exits 0 without touching the file. Pre-write content so
        // the len>0 post-check passes; this exercises the happy path of the
        // wrapper without a real screencapture binary.
        let tmp = unique_tmp("happy-path");
        std::fs::write(&tmp, b"\x89PNG\r\n\x1a\nfake-png-bytes").expect("seed tmp");
        let result = capture_with_bin("/usr/bin/true", 1, &tmp);
        drop(std::fs::remove_file(&tmp));
        assert!(
            result.is_ok(),
            "expected Ok on a successful exit + non-empty file, got {result:?}"
        );
    }

    #[test]
    fn capture_with_bin_rejects_zero_byte_output() {
        // Simulates the TCC-revoke race: bin exits 0 but the output stays
        // 0 bytes. The post-condition turns this into CaptureFailed{Some(0)}.
        let tmp = unique_tmp("zero-byte");
        std::fs::write(&tmp, b"").expect("seed empty tmp");
        let err = capture_with_bin("/usr/bin/true", 1, &tmp)
            .expect_err("expected zero-byte CaptureFailed");
        drop(std::fs::remove_file(&tmp));
        match err {
            BotError::CaptureFailed { exit_code } => assert_eq!(exit_code, Some(0)),
            other => panic!("expected CaptureFailed{{Some(0)}}, got {other:?}"),
        }
    }

    #[cfg(unix)]
    #[test]
    fn capture_with_bin_refuses_pre_existing_symlink() {
        // A pre-placed symlink at output_path would let screencapture write
        // through it. The pre-condition gate refuses before invoking the bin.
        use std::os::unix::fs::symlink;
        let target = unique_tmp("symlink-target");
        let link = unique_tmp("symlink-link");
        std::fs::write(&target, b"do-not-overwrite-me").expect("seed target");
        drop(std::fs::remove_file(&link));
        symlink(&target, &link).expect("create symlink");
        let err = capture_with_bin("/usr/bin/true", 1, &link)
            .expect_err("expected refuse-symlink failure");
        drop(std::fs::remove_file(&link));
        drop(std::fs::remove_file(&target));
        match err {
            BotError::CaptureFailed { exit_code } => assert_eq!(exit_code, None),
            other => panic!("expected CaptureFailed{{None}} on symlink, got {other:?}"),
        }
    }
}
