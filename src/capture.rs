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

/// Live wrapper: capture the given CG window to `output_path`.
///
/// Requires Screen Recording permission for the calling terminal app —
/// already gated by `permissions::check_screen_recording` upstream in
/// the boot sequence. If the permission is revoked between preflight
/// and this call, `screencapture` writes a 0-byte file and exits 0
/// (not great, but the failure surfaces at the next milestone's
/// template-matching step). For v0.1.1 we accept that race.
pub fn capture_window(window_id: u32, output_path: &Path) -> Result<()> {
    let args = screencapture_args(window_id, output_path);
    let status = Command::new(SCREENCAPTURE_BIN)
        .args(&args)
        .status()
        .map_err(|_| BotError::CaptureFailed { exit_code: None })?;
    if status.success() {
        Ok(())
    } else {
        Err(BotError::CaptureFailed {
            exit_code: status.code(),
        })
    }
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
    fn args_handle_path_with_spaces() {
        // OsString preserves the literal path; no shell-escaping issues
        // because we pass argv directly to Command::args, not through a shell.
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
}
