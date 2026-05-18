//! v0.2 continuous-loop live integration tests.
//!
//! All tests are `#[ignore]`'d by default — they require RoK running,
//! Screen Recording + Accessibility granted to the test-spawned binary,
//! and a writable temp dir. Invocation:
//!
//! ```text
//! cargo test --test sck_integration -- --ignored
//! ```
//!
//! `--ignored` runs the ignored tests in this file, NOT plus the regular
//! tests. Use `--include-ignored` for both.
//!
//! ## Why these are integration tests, not unit tests
//!
//! The v0.2 loop's per-tick `capture → match → click → verify` needs a
//! live `WindowServer`, per-binary Screen Recording + Accessibility
//! grants, and a real RoK window. Mocking any of that would need a
//! parallel-universe SCK. The pure decision core (`classify_error`,
//! `next_failure_count`, `should_stop`, `parse_max_ticks`, `select_roi`)
//! IS hermetically unit-tested in `src/`; these live tests cover the
//! `tick` / `run_loop` wiring those pure functions can't reach.
//!
//! ## v0.2 exit-code semantics (what each test asserts against)
//!
//! - Boot failure (RoK not running, a permission denied) → the v0.1.x
//!   `BotError` exit code: 10 `WindowNotFound`, 13 `PermissionsMissing`,
//!   14 `CaptureFailed`, etc. Boot runs before the loop.
//! - The loop stopped cleanly (Ctrl-C, or `completed_ticks` reached the
//!   `ROK_BOT_MAX_TICKS` cap) → exit 0. A single transient tick failure
//!   inside that run does NOT change the exit code — the loop's error
//!   policy (design D5) treats one failure as noise.
//! - `LOOP_FAILURE_BUDGET` (3) consecutive transient failures with no
//!   success → exit 21 `LoopAborted`.
//!
//! ## The world-needle placeholder caveat
//!
//! v0.2 ships `assets/targets/world-button.png` as a sentinel
//! placeholder — the needle-swap verify cannot confirm a toggle until an
//! operator crops the real world-view art. So with the placeholder in
//! place EVERY tick fails `ClickNotVerified`; a multi-tick run aborts
//! exit 21. A `ROK_BOT_MAX_TICKS=1` run still exits 0 (one failed tick,
//! then the cap is reached — a clean stop). Tests that must pass with
//! the placeholder use `MAX_TICKS=1`; the bounded-run test accepts
//! either 0 (real needle cropped) or 21 (placeholder).
//!
//! ## v0.2.1 window-recovery tests
//!
//! The `live_loop_recover*` / `live_loop_logs_pre_capture_probe` /
//! `live_loop_boot_retry_*` tests cover the v0.2.1 window-lifecycle
//! work: the pre-capture liveness probe (L4), relaunch + visibility
//! recovery (L5 / D14 / D15), and the boot retry (L1). They assert on
//! stderr log lines, not just exit codes — with the sentinel world
//! needle a recovered loop still ends exit 21, so the recovery log
//! lines are the proof recovery engaged. Recovery only runs for
//! `ROK_BOT_MAX_TICKS >= 2` (L10), so those tests spawn with a higher
//! cap. `live_loop_recovery_handles_hidden_then_gone` doubles as the
//! AC-D15 empirical check (does `not_visible` fire in Mode 2 at all?).

#![allow(clippy::doc_markdown)]
// Integration tests use the standard panic/expect idioms.
#![allow(clippy::panic, clippy::expect_used)]

use std::path::PathBuf;
use std::time::Instant;

const ROK_BOT_BIN_NAME: &str = "rok-bot";

/// Locate the `rok-bot` binary in `target/release` or `target/debug`.
fn rok_bot_binary() -> PathBuf {
    let manifest_dir = env!("CARGO_MANIFEST_DIR");
    let candidates = [
        PathBuf::from(manifest_dir)
            .join("target")
            .join("release")
            .join(ROK_BOT_BIN_NAME),
        PathBuf::from(manifest_dir)
            .join("target")
            .join("debug")
            .join(ROK_BOT_BIN_NAME),
    ];
    for p in candidates {
        if p.exists() {
            return p;
        }
    }
    panic!(
        "rok-bot binary not found in target/debug or target/release. \
         Run `cargo build --release` (or `cargo build`) before invoking \
         this integration test."
    );
}

/// Run the rok-bot binary in a clean temp working directory with the
/// given `ROK_BOT_MAX_TICKS` cap. Returns `(exit code, stderr text)`.
/// The binary writes capture PNGs relative to cwd; the tempdir keeps
/// the test isolated from the project root.
fn run_rok_bot(max_ticks: &str) -> (i32, String) {
    let tmp = tempfile::tempdir().expect("tempdir");
    let bin = rok_bot_binary();
    let output = std::process::Command::new(&bin)
        .current_dir(tmp.path())
        .env("RUST_LOG", "info")
        .env("ROK_BOT_MAX_TICKS", max_ticks)
        .output()
        .expect("rok-bot binary must execute");
    let exit = output.status.code().unwrap_or(-1);
    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
    (exit, stderr)
}

/// Live: one bounded tick (`ROK_BOT_MAX_TICKS=1`) against RoK on the
/// built-in display. This is the v0.1.x one-shot regression anchor —
/// `MAX_TICKS=1` runs exactly one `capture → match → click → verify`.
/// The loop reaches its cap and stops cleanly → exit 0 (a transient
/// verify failure inside the single tick does not change the exit).
#[test]
#[ignore = "requires live RoK + Screen Recording + Accessibility grants"]
fn live_loop_one_tick_on_builtin_display() {
    let t0 = Instant::now();
    let (exit, stderr) = run_rok_bot("1");
    let wall_ms = t0.elapsed().as_millis();
    eprintln!(
        "[live_loop_one_tick_on_builtin_display] exit={exit} wall_ms={wall_ms}\n\
         --- stderr ---\n{stderr}"
    );
    assert_eq!(
        exit, 0,
        "expected exit 0 (boot OK + 1 tick + clean stop); got {exit}"
    );
}

/// Live: one bounded tick against RoK on a BetterDisplay virtual
/// display (Mode 2 — the recommended operating mode). Same invocation
/// and success criterion as the built-in test; the operator switches
/// RoK to the virtual display before running.
#[test]
#[ignore = "requires live RoK on a BetterDisplay virtual display"]
fn live_loop_one_tick_on_bd_virtual_display() {
    let t0 = Instant::now();
    let (exit, stderr) = run_rok_bot("1");
    let wall_ms = t0.elapsed().as_millis();
    eprintln!(
        "[live_loop_one_tick_on_bd_virtual_display] exit={exit} wall_ms={wall_ms}\n\
         --- stderr ---\n{stderr}"
    );
    assert_eq!(exit, 0, "expected exit 0; got {exit}");
}

/// Live: a bounded multi-tick run (`ROK_BOT_MAX_TICKS=3`) MUST
/// terminate — it must not hang. The loop is bounded by the cap, so
/// even a degenerate run ends. Acceptable exits:
/// - 0 — the operator cropped the real world needle; 3 ticks toggled
///   the view and the loop hit its cap cleanly.
/// - 21 — the world needle is still the sentinel placeholder; every
///   tick fails `ClickNotVerified`, 3 in a row → `LoopAborted`.
///
/// What this rules out: a hang, a crash, or a boot failure.
#[test]
#[ignore = "requires live RoK + Screen Recording + Accessibility grants"]
fn live_loop_bounded_run_terminates() {
    let t0 = Instant::now();
    let (exit, stderr) = run_rok_bot("3");
    let wall_ms = t0.elapsed().as_millis();
    eprintln!(
        "[live_loop_bounded_run_terminates] exit={exit} wall_ms={wall_ms}\n\
         --- stderr ---\n{stderr}"
    );
    assert!(
        exit == 0 || exit == 21,
        "a bounded 3-tick run must end with exit 0 (real needle, toggles \
         confirmed) or 21 (placeholder world needle, LoopAborted); got \
         {exit}. stderr:\n{stderr}"
    );
    // 3 ticks, each with a 500ms verify delay — generous ceiling for
    // test-machine variance.
    assert!(
        wall_ms <= 15_000,
        "3-tick bounded run took {wall_ms}ms (>15s) — investigate the \
         per-tick timings in stderr"
    );
}

/// Live: a single bounded tick must complete inside a sane wall budget.
/// One tick is capture + ROI match + click + 500ms verify delay + post
/// capture + verify match — roughly 1.5-2.5s. The 5s ceiling allows for
/// cold SCK warmup and test-machine variance. Surfaces the timing in
/// stderr regardless of pass/fail.
#[test]
#[ignore = "requires live RoK + Screen Recording + Accessibility grants"]
fn live_loop_one_tick_wall_budget() {
    let t0 = Instant::now();
    let (exit, stderr) = run_rok_bot("1");
    let wall_ms = t0.elapsed().as_millis();
    eprintln!(
        "[live_loop_one_tick_wall_budget] exit={exit} wall_ms={wall_ms}\n\
         --- stderr ---\n{stderr}"
    );
    assert_eq!(exit, 0, "expected exit 0; got {exit}");
    assert!(
        wall_ms <= 5000,
        "one-tick wall budget regressed: {wall_ms}ms > 5000ms ceiling. \
         Investigate the per-stage timings in stderr."
    );
}

/// Live: RoK not running → boot's `find_rok_window` fails before the
/// loop starts → exit 10 `WindowNotFound`. The operator's procedure:
/// quit RoK, run this test, expect exit 10; relaunch RoK afterward.
///
/// Pins that boot failure still surfaces the v0.1.x exit code — the
/// loop wrapper does not swallow it into a clean exit 0.
#[test]
#[ignore = "requires RoK to be quit before running"]
fn live_loop_rok_not_running_exits_10() {
    let (exit, stderr) = run_rok_bot("1");
    eprintln!("[live_loop_rok_not_running_exits_10] exit={exit}\n--- stderr ---\n{stderr}");
    assert_eq!(
        exit, 10,
        "RoK not running must surface as exit 10 (WindowNotFound) from boot; \
         got {exit}. stderr:\n{stderr}"
    );
}

/// Live: RoK on a Space that isn't currently displayed (operator
/// switched Spaces before running). SCK enumerates hidden-Space windows,
/// so boot's `find_rok_window` must SUCCEED — the failure to rule out is
/// exit 10 (`WindowNotFound`), which would wrongly imply RoK isn't
/// running. The per-tick `validate_at_click_site` then sees the hidden
/// window and the tick fails `WindowChanged{not_visible}` (a transient
/// failure per D12); with `MAX_TICKS=1` the loop still reaches its cap
/// and exits 0.
#[test]
#[ignore = "requires manually switching to a Space that doesn't show RoK"]
fn live_loop_rok_on_hidden_space() {
    let (exit, stderr) = run_rok_bot("1");
    eprintln!("[live_loop_rok_on_hidden_space] exit={exit}\n--- stderr ---\n{stderr}");
    assert_ne!(
        exit, 10,
        "SCK must enumerate hidden-Space windows; exit 10 (WindowNotFound) \
         means boot discovery wrongly failed. stderr:\n{stderr}"
    );
}

/// Live: the v0.2.1 L4 pre-capture liveness probe runs every tick. A
/// one-tick run against a healthy RoK must log the probe line — pins
/// that `tick` opens with `validate_window_present` before the capture
/// (so a relaunch is caught before SCK touches a dead handle).
#[test]
#[ignore = "requires live RoK + Screen Recording + Accessibility grants"]
fn live_loop_logs_pre_capture_probe() {
    let (exit, stderr) = run_rok_bot("1");
    eprintln!("[live_loop_logs_pre_capture_probe] exit={exit}\n--- stderr ---\n{stderr}");
    assert_eq!(
        exit, 0,
        "expected exit 0 (boot OK + 1 tick + clean stop); got {exit}"
    );
    assert!(
        stderr.contains("pre-capture window liveness probe OK"),
        "v0.2.1 L4: every tick must run the pre-capture probe; its log \
         line is missing from stderr:\n{stderr}"
    );
}

/// Live: the loop rides through a RoK crash-and-relaunch (v0.2.1 D14).
///
/// Operator procedure: run this test; within ~2 s of the loop starting,
/// force-quit RoK (Activity Monitor, or `kill`) and immediately
/// relaunch it. The pre-capture probe catches `window_id_gone`, the
/// loop enters relaunch recovery, re-discovers the restarted RoK, and
/// resumes — instead of aborting exit 19.
///
/// Asserts on stderr, not the exit code: with the sentinel world
/// needle the run still ends exit 21 once the `ClickNotVerified`
/// failure budget is spent, so the proof recovery worked is the
/// recovery log lines.
#[test]
#[ignore = "requires force-quitting and relaunching RoK within ~2s of loop start"]
fn live_loop_recovers_from_rok_relaunch() {
    let (exit, stderr) = run_rok_bot("8");
    eprintln!("[live_loop_recovers_from_rok_relaunch] exit={exit}\n--- stderr ---\n{stderr}");
    assert!(
        stderr.contains("window recovery starting"),
        "killing RoK mid-loop must trigger window recovery; the recovery \
         start log line is missing:\n{stderr}"
    );
    assert!(
        stderr.contains("RoK re-discovered — resuming loop"),
        "relaunch recovery must re-discover the restarted RoK and resume; \
         the re-discovery log line is missing:\n{stderr}"
    );
}

/// Live: relaunch recovery gives up cleanly when RoK never comes back
/// (v0.2.1 D14). Operator procedure: run this test; within ~2 s of the
/// loop starting, force-quit RoK and do NOT relaunch it. The loop
/// enters relaunch recovery, polls for the 60 s deadline, then aborts
/// `LoopAborted{window_recovery_exhausted}` → exit 21.
///
/// Runs ~60 s+ (the relaunch deadline). Relaunch RoK afterward.
#[test]
#[ignore = "requires force-quitting RoK mid-loop and NOT relaunching it (~60s run)"]
fn live_loop_recovery_exhausts_when_rok_stays_dead() {
    let (exit, stderr) = run_rok_bot("8");
    eprintln!(
        "[live_loop_recovery_exhausts_when_rok_stays_dead] exit={exit}\n\
         --- stderr ---\n{stderr}"
    );
    assert_eq!(
        exit, 21,
        "a permanently-gone RoK must abort the loop exit 21; got {exit}.\n\
         stderr:\n{stderr}"
    );
    assert!(
        stderr.contains("window_recovery_exhausted"),
        "the abort must be window_recovery_exhausted (recovery deadline), \
         not failure_budget_exhausted:\n{stderr}"
    );
}

/// Live: recovery handles a window that goes hidden and THEN gone
/// (v0.2.1 D2). Operator procedure: run this test; switch to a Space
/// that does not show RoK (the loop enters visibility recovery), then
/// force-quit RoK while it is hidden. The shared recovery sub-loop must
/// re-classify the probe, retarget from visibility to relaunch
/// re-discovery, and not hang or false-resume.
///
/// Also the AC-D15 empirical check: if `WindowChanged{not_visible}`
/// never fires for a Mode 2 BetterDisplay virtual display under a
/// hidden Space, the visibility-recovery path is never entered — note
/// that against the stderr dump.
#[test]
#[ignore = "requires hiding RoK's Space then force-quitting RoK during the wait"]
fn live_loop_recovery_handles_hidden_then_gone() {
    let (exit, stderr) = run_rok_bot("8");
    eprintln!(
        "[live_loop_recovery_handles_hidden_then_gone] exit={exit}\n\
         --- stderr ---\n{stderr}"
    );
    assert!(
        stderr.contains("window recovery starting"),
        "hiding RoK then quitting it must engage window recovery:\n{stderr}"
    );
    // The run must terminate — recovery must not hang on the transition.
    assert!(
        exit == 0 || exit == 21,
        "a hidden-then-gone recovery must end cleanly (0) or abort (21), \
         never hang; got {exit}.\nstderr:\n{stderr}"
    );
}

/// Live: boot rides through a BetterDisplay reconnect race (v0.2.1 L1).
///
/// Operator procedure: trigger a BD virtual-display reconnect (toggle
/// the display, or sleep/wake) right as this test launches the binary.
/// `main::run` retries the find-window + detect-mode pair on a
/// transient `WindowScreenUnresolved`; the retry must absorb the race.
///
/// Asserts `exit != 11`: a spurious `WindowScreenUnresolved` (exit 11)
/// leaking through means the L1 retry failed to ride out the race. On
/// a clean boot (no reconnect) the run simply exits 0 — the retry is
/// transparent. If the retry fired, stderr carries the retry log line.
#[test]
#[ignore = "requires triggering a BetterDisplay reconnect during boot"]
fn live_loop_boot_retry_survives_bd_reconnect() {
    let (exit, stderr) = run_rok_bot("1");
    eprintln!(
        "[live_loop_boot_retry_survives_bd_reconnect] exit={exit}\n\
         --- stderr ---\n{stderr}"
    );
    assert_ne!(
        exit, 11,
        "a BD reconnect race during boot must be absorbed by the L1 retry; \
         exit 11 (WindowScreenUnresolved) means it leaked through.\n\
         stderr:\n{stderr}"
    );
}

/// Live: a crash-looping RoK aborts via the recovery budget (v0.2.1 L5
/// / /review D1). Operator procedure: run this test; each time the
/// loop logs "window recovered — loop resuming", force-quit RoK again
/// and relaunch it — do this RECOVERY_BUDGET (3) times in a row so
/// every recovery succeeds but no tick ever does. After the 3rd
/// recovery with no successful tick between, the loop aborts
/// `LoopAborted{recovery_budget_exhausted}` exit 21 rather than
/// recovering forever.
#[test]
#[ignore = "requires force-quitting + relaunching RoK ~3 times in a row"]
fn live_loop_recovery_budget_aborts_on_crash_loop() {
    let (exit, stderr) = run_rok_bot("20");
    eprintln!(
        "[live_loop_recovery_budget_aborts_on_crash_loop] exit={exit}\n\
         --- stderr ---\n{stderr}"
    );
    assert_eq!(
        exit, 21,
        "a crash-looping RoK must abort via the recovery budget; got {exit}.\n\
         stderr:\n{stderr}"
    );
    assert!(
        stderr.contains("recovery_budget_exhausted"),
        "the abort must be recovery_budget_exhausted, not failure_budget \
         or a single recovery deadline:\n{stderr}"
    );
}
