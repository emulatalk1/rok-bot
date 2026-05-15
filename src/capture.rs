//! In-process window capture via Apple's ScreenCaptureKit.
//!
//! v0.1.8 replaces the v0.1.x `/usr/sbin/screencapture` CLI shellout
//! (~280–1400ms wall, fork/exec/PNG-encode-on-disk overhead) with
//! `SCScreenshotManager.captureImageWithFilter:configuration:
//! completionHandler:` (~141ms steady-state, ~280ms cold). Spike-
//! validated in `spikes/p8-spike/` (commit `9c576a9`); design doc:
//! `~/.gstack/projects/emulatalk1-rok-bot/hbchuc-main-design-v0.1.8-*.md`.
//!
//! ## Threading model (T1, design v0.1.8)
//!
//! SCK posts completion handlers to an internal background queue.
//! Apple does not document this explicitly, but the spike empirically
//! confirms (5/5 main-thread `DispatchSemaphore::wait` succeeds —
//! would deadlock if SCK posted to main). The
//! `objc2-screen-capture-kit` 0.3.2 binding is a thin pass-through
//! with no queue interposition.
//!
//! v0.1.8 ports the spike's main-thread DispatchSemaphore pattern
//! verbatim BUT replaces `DispatchTime::FOREVER` with a real timeout
//! (5 s for the capture call) mapped to a structured
//! `CaptureFailed { stage: STAGE_CAPTURE_RETURNED_NIL }` error. The
//! timeout is the deadlock fail-safe — even if SCK ever changes its
//! queue semantics or fails to deliver, the bot exits cleanly with a
//! grep-able error message rather than hanging.
//!
//! ## Safety surfaces preserved from v0.1.x
//!
//! - **Refuse pre-existing symlinks at `output_path`.** SCK's PNG
//!   writer (via the `image` crate) follows symlinks just like
//!   `screencapture` did. Without this gate, a pre-placed symlink
//!   would let the capture write through to any file the user can
//!   write (`~/.ssh/authorized_keys`, etc.). Closes /review F2.
//! - **0-byte output post-check is no longer needed.** v0.1.x's
//!   silent-corrupt path (TCC revoked between preflight and
//!   capture → `screencapture` exits 0 with empty file) is replaced
//!   by SCK's structured nil-image return → `STAGE_CAPTURE_RETURNED_NIL`.
//!
//! ## Why no rollback flag (C7 from /plan-eng-review)
//!
//! Spike covers all known failure modes. v0.1.8 ships single SCK
//! path; rollback is `git revert` + `cargo build`. A `--capture-
//! backend=cli` flag would double the test matrix for marginal
//! defense.

#![allow(
    unsafe_code,
    reason = "SCK / CG FFI for in-process capture is required; the unsafe \
              surface is contained in this module."
)]
// SCK module pulls in dozens of Apple framework / type names per
// docstring; backticking every one would dominate the comment text.
// The pedantic lint is project-wide warn; this module opts out so
// the prose stays readable.
#![allow(clippy::doc_markdown)]
// `validate_inner` and synth-buffer indexing in tests need the
// allowances here; the alternatives (struct-bundling args, .get()
// chains in tests) hurt readability without buying safety in
// pure-test contexts.
#![allow(clippy::indexing_slicing, clippy::arithmetic_side_effects)]

use std::fs::{self, OpenOptions};
use std::os::unix::fs::OpenOptionsExt;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use block2::RcBlock;
use dispatch2::{DispatchSemaphore, DispatchTime};
use image::ImageEncoder;
use image::codecs::png::PngEncoder;
use objc2::AllocAnyThread;
use objc2::rc::Retained;
use objc2_core_graphics::{CGDataProvider, CGImage};
use objc2_foundation::NSError;
use objc2_screen_capture_kit::{
    SCContentFilter, SCScreenshotManager, SCStreamConfiguration, SCWindow,
};

use crate::cg_bootstrap::register_with_window_server;
use crate::error::{BotError, Result};

/// Output path was a pre-existing symlink. Refused before invoking
/// SCK — closes the local-attacker hazard documented on
/// `BotError::CaptureFailed`.
pub const STAGE_SYMLINK_REFUSED: &str = "symlink_refused";

/// `SCShareableContent.windows` did not contain a window with the
/// requested CGWindowID. Either RoK closed between discovery and
/// capture, or the cached shareable-content list (per D2 in the
/// v0.1.8 design) is stale. The cache is invalidated on this stage
/// so the next call re-fetches.
pub const STAGE_WINDOW_NOT_FOUND: &str = "window_not_found";

/// `SCScreenshotManager.captureImageWithFilter:configuration:
/// completionHandler:` reported success but the CGImage pointer was
/// nil, OR the completion handler never fired before the
/// [`CAPTURE_TIMEOUT`] (T1 deadlock fail-safe). Nil with no error
/// indicates SCK gave up; a timeout indicates SCK never delivered.
pub const STAGE_CAPTURE_RETURNED_NIL: &str = "capture_returned_nil";

/// Pixel-byte read from the returned CGImage failed (CFData length
/// negative, undersized buffer, BGRA→RGBA conversion bailed) or the
/// ×2 scale canary tripped (operator on a non-Retina or scaled
/// display — see TODOS D10 for the proper fix; this canary protects
/// correctness, not coverage).
pub const STAGE_CGIMAGE_DECODE: &str = "cgimage_decode";

/// `image::save_buffer` failed to write the captured PNG. Disk full,
/// permission issue on the output dir, or the matcher's PNG round-
/// trip would fail downstream. Fires only after a successful SCK
/// capture, so this is filesystem-side.
pub const STAGE_PNG_WRITE: &str = "png_write";

/// Maximum wall clock wait for `SCScreenshotManager.captureImage...`
/// to deliver its completion handler. 5 s is comfortably above the
/// p8-spike's 141 ms steady-state and the worst-case 200 ms target
/// from the v0.1.8 acceptance criteria, but small enough that a
/// hung SCK call surfaces inside one operator-attention window
/// instead of looking like the binary froze.
///
/// T1 from `/plan-eng-review` mandates a real timeout in place of
/// the spike's `DispatchTime::FOREVER` — see module docs for
/// rationale.
pub const CAPTURE_TIMEOUT: Duration = Duration::from_secs(5);

/// Cross-thread slot for SCK completion-handler results. Wraps the
/// per-call `Mutex<Option<Retained<CGImage>>>` so its enclosing
/// `Arc` can be sent into the completion-handler closure on SCK's
/// dispatch queue.
///
/// `Retained<CGImage>` does not auto-derive Send + Sync (objc2's
/// generic `Retained` opts out by default), but the underlying
/// `CGImage` is documented thread-safe by Apple — CG objects use
/// atomic refcounting (`CGImageRetain`/`CGImageRelease`) and CGImage
/// is immutable post-construction. Mutex serializes interior
/// mutability of the Option slot itself; no actual cross-thread
/// data race exists.
///
/// Mirrors the `ContentCache` pattern in `window.rs` for the same
/// reason — see that module for the analogous SAFETY justification
/// on `SCShareableContent`.
struct ImageSlot(Mutex<Option<Retained<CGImage>>>);

// SAFETY: see ImageSlot doc comment. CGImage is Apple-documented
// thread-safe; Retained's atomic refcounting + Mutex serialization
// satisfy Send + Sync at the level the type system can't verify.
unsafe impl Send for ImageSlot {}
// SAFETY: see Send impl above. Sync is satisfied by Mutex's
// serialization of the Option slot.
unsafe impl Sync for ImageSlot {}

/// Backing-pixel scale factor for v0.1.8 captures. v0.1.7 confirmed
/// every observed config (RoK on built-in Retina + RoK on
/// BetterDisplay 2× virtual display) is exactly 2×. The capture
/// canary in `capture_window` asserts the produced CGImage matches
/// `frame.width * 2 × frame.height * 2`; mismatch fires
/// `CaptureFailed { stage: STAGE_CGIMAGE_DECODE }` so a future non-
/// Retina or fractional-scale display config fails closed instead
/// of silently feeding the matcher a wrong-dim haystack.
///
/// TODOS D10 covers the proper backing-scale-discovery fix.
const RETINA_SCALE: usize = 2;

/// Public capture entrypoint. Captures the given SCWindow's
/// backing-pixel content to a PNG at `output_path`. Returns a typed
/// `BotError::CaptureFailed { stage }` on failure.
///
/// Pipeline:
/// 1. Bootstrap WindowServer registration via `cg_bootstrap` (T2 —
///    idempotent, called from every SCK entrypoint so cargo test
///    callsites can't accidentally bypass `main.rs::run`).
/// 2. Build `SCContentFilter` (desktop-independent: just this
///    window) and `SCStreamConfiguration` (`frame.{w,h} * 2`,
///    BGRA, no cursor).
/// 3. Capture via `SCScreenshotManager.captureImageWithFilter:
///    configuration:completionHandler:`, parking on a
///    DispatchSemaphore with [`CAPTURE_TIMEOUT`].
/// 4. Read pixel bytes via CGDataProvider, convert BGRA→RGBA,
///    strip row padding (`bytes_per_row` is often padded past
///    `width * 4` for alignment).
/// 5. Validate captured dims match `frame.{w,h} * 2` (×2 scale
///    canary; see [`RETINA_SCALE`]).
/// 6. Write PNG via [`open_capture_output_safely`] — atomic open
///    with `O_NOFOLLOW + O_EXCL` defeats the v0.1.x-era symlink
///    TOCTOU between the pre-check and the write. Direct
///    `PngEncoder` write to the owned fd, no path round-trip.
pub fn capture_window(window: &SCWindow, output_path: &Path) -> Result<()> {
    register_with_window_server();

    // SAFETY: SCWindow.frame() reads a Cocoa-owned CGRect; SCK
    // guarantees frame() is callable off any thread for an SCWindow
    // we obtained from SCShareableContent.
    let frame = unsafe { window.frame() };
    let cap_w = (frame.size.width as usize).saturating_mul(RETINA_SCALE);
    let cap_h = (frame.size.height as usize).saturating_mul(RETINA_SCALE);

    // SAFETY: SCContentFilter::initWithDesktopIndependentWindow
    // takes ownership of the alloc result; SCK retains the SCWindow
    // reference internally.
    let filter = unsafe {
        SCContentFilter::initWithDesktopIndependentWindow(SCContentFilter::alloc(), window)
    };
    let config = build_stream_config(cap_w, cap_h);

    let cg = capture_image_sync(&filter, &config)?;

    let pw = CGImage::width(Some(&cg));
    let ph = CGImage::height(Some(&cg));
    let bpr = CGImage::bytes_per_row(Some(&cg));

    // Canary (codex #10 / design D10): expect captured dims to match
    // `frame * RETINA_SCALE`. Mismatch indicates a non-Retina or
    // scaled display we haven't tested against; fail closed with a
    // grep-able stage tag rather than silently feeding the matcher a
    // wrong-dim haystack.
    if pw != cap_w || ph != cap_h {
        tracing::warn!(
            target: "rok_bot",
            captured_w = pw,
            captured_h = ph,
            expected_w = cap_w,
            expected_h = cap_h,
            "capture dim mismatch — display backing scale is not the assumed {}×; \
             see TODOS D10 for the discovery fix",
            RETINA_SCALE
        );
        return Err(BotError::CaptureFailed {
            stage: STAGE_CGIMAGE_DECODE,
            exit_code: None,
        });
    }

    let rgba = cgimage_to_rgba(&cg, pw, ph, bpr).ok_or(BotError::CaptureFailed {
        stage: STAGE_CGIMAGE_DECODE,
        exit_code: None,
    })?;

    let file = open_capture_output_safely(output_path)?;
    let encoder = PngEncoder::new(file);
    encoder
        .write_image(
            &rgba,
            u32::try_from(pw).unwrap_or(u32::MAX),
            u32::try_from(ph).unwrap_or(u32::MAX),
            image::ExtendedColorType::Rgba8,
        )
        .map_err(|err| {
            tracing::warn!(
                target: "rok_bot",
                path = %output_path.display(),
                io_error = %err,
                "PNG encode failed after successful SCK capture + safe open"
            );
            BotError::CaptureFailed {
                stage: STAGE_PNG_WRITE,
                exit_code: None,
            }
        })?;

    Ok(())
}

/// Atomically open `output_path` for capture, race-free against a
/// local attacker planting symlinks at the path. Replaces the
/// v0.1.x-era pre-condition `symlink_metadata` check (which had a
/// TOCTOU window of ~141 ms-5 s between the check and the write)
/// with kernel-side `O_NOFOLLOW + O_EXCL`.
///
/// Sequence:
/// 1. `OpenOptions::new().write(true).create_new(true).custom_
///    flags(O_NOFOLLOW).open(output_path)` — atomic open with
///    `O_CREAT | O_EXCL | O_NOFOLLOW`. Possible outcomes:
///    - Path doesn't exist → file is created and we own the fd
///      (happy path).
///    - Path is a symlink → POSIX `open` returns `EEXIST` (because
///      `O_EXCL` checks via `lstat`, treating any existing entry as
///      a collision). On macOS the order of error codes can also
///      surface as `ELOOP`; we treat both as the symlink-refusal
///      stage.
///    - Path is a regular file (left over from a previous run) →
///      `EEXIST`. We then `lstat` to confirm it's regular (not a
///      symlink that raced in), `fs::remove_file` it (which
///      unlinks the entry without following), and retry once.
/// 2. The retry has the same race-free property: any planted
///    symlink between the unlink and the retry causes the next
///    `open` to fail with `EEXIST`/`ELOOP`, never to redirect the
///    write. `O_NOFOLLOW` is the kernel-side gate; the loop just
///    handles the legitimate "previous capture left a regular
///    file" case.
///
/// Errors map to `STAGE_SYMLINK_REFUSED` when a symlink (or some
/// other non-regular planted entry) is detected, otherwise
/// `STAGE_PNG_WRITE` for disk-full / permission-denied / etc.
///
/// **Codex regression fix**: an earlier draft of this function
/// unconditionally called `fs::remove_file` BEFORE the open, which
/// silently bypassed the symlink defense (the symlink was unlinked,
/// then a fresh file was created at the path with no diversion
/// detected). The current sequence keeps the kernel-side
/// `O_NOFOLLOW` gate as the single source of truth and only unlinks
/// after explicitly confirming the existing entry is a regular
/// file.
fn open_capture_output_safely(output_path: &Path) -> Result<fs::File> {
    fn try_open(path: &Path) -> std::io::Result<fs::File> {
        OpenOptions::new()
            .write(true)
            .create_new(true)
            .custom_flags(libc::O_NOFOLLOW)
            .open(path)
    }

    match try_open(output_path) {
        Ok(file) => Ok(file),
        Err(err) => {
            let errno = err.raw_os_error();
            // ELOOP: O_NOFOLLOW caught a planted symlink directly.
            if errno == Some(libc::ELOOP) {
                tracing::warn!(
                    target: "rok_bot",
                    path = %output_path.display(),
                    io_error = %err,
                    "refusing to capture: O_NOFOLLOW caught a symlink at output path"
                );
                return Err(BotError::CaptureFailed {
                    stage: STAGE_SYMLINK_REFUSED,
                    exit_code: None,
                });
            }
            // EEXIST: path exists. Could be a stale regular file
            // (legitimate, retry) OR a planted symlink (refuse).
            // Distinguish via lstat so we never unlink a symlink
            // and then race-create over it.
            if err.kind() == std::io::ErrorKind::AlreadyExists {
                let meta = match fs::symlink_metadata(output_path) {
                    Ok(m) => m,
                    Err(stat_err) => {
                        tracing::warn!(
                            target: "rok_bot",
                            path = %output_path.display(),
                            io_error = %stat_err,
                            "refusing to capture: lstat failed after EEXIST on open"
                        );
                        return Err(BotError::CaptureFailed {
                            stage: STAGE_PNG_WRITE,
                            exit_code: None,
                        });
                    }
                };
                if meta.file_type().is_symlink() {
                    tracing::warn!(
                        target: "rok_bot",
                        path = %output_path.display(),
                        "refusing to capture: output path is a pre-existing symlink"
                    );
                    return Err(BotError::CaptureFailed {
                        stage: STAGE_SYMLINK_REFUSED,
                        exit_code: None,
                    });
                }
                if !meta.file_type().is_file() {
                    // Pipe, socket, dev node, directory — refuse.
                    tracing::warn!(
                        target: "rok_bot",
                        path = %output_path.display(),
                        file_type = ?meta.file_type(),
                        "refusing to capture: output path is not a regular file"
                    );
                    return Err(BotError::CaptureFailed {
                        stage: STAGE_SYMLINK_REFUSED,
                        exit_code: None,
                    });
                }
                // Regular file from a prior run — unlink and retry.
                // fs::remove_file does NOT follow symlinks (unlink
                // removes the path entry, not the target).
                if let Err(rm_err) = fs::remove_file(output_path) {
                    tracing::warn!(
                        target: "rok_bot",
                        path = %output_path.display(),
                        io_error = %rm_err,
                        "refusing to capture: unlink of stale regular file failed"
                    );
                    return Err(BotError::CaptureFailed {
                        stage: STAGE_PNG_WRITE,
                        exit_code: None,
                    });
                }
                // Retry. Any race-planted symlink between the
                // unlink above and this open is caught by the same
                // O_NOFOLLOW + O_EXCL combination — never diverts
                // the write.
                return try_open(output_path).map_err(|err2| {
                    let stage = if err2.raw_os_error() == Some(libc::ELOOP)
                        || err2.kind() == std::io::ErrorKind::AlreadyExists
                    {
                        STAGE_SYMLINK_REFUSED
                    } else {
                        STAGE_PNG_WRITE
                    };
                    tracing::warn!(
                        target: "rok_bot",
                        path = %output_path.display(),
                        io_error = %err2,
                        stage,
                        "refusing to capture: retry after stale-file unlink failed"
                    );
                    BotError::CaptureFailed {
                        stage,
                        exit_code: None,
                    }
                });
            }
            // Other I/O error (disk full, permission denied, etc.).
            tracing::warn!(
                target: "rok_bot",
                path = %output_path.display(),
                io_error = %err,
                "refusing to capture: O_NOFOLLOW+O_EXCL open failed"
            );
            Err(BotError::CaptureFailed {
                stage: STAGE_PNG_WRITE,
                exit_code: None,
            })
        }
    }
}

/// Build an `SCStreamConfiguration` matching `screencapture`'s
/// behavior on Retina: width × height in backing pixels, BGRA pixel
/// format, no cursor. Pulled out of `capture_window` so the pure
/// builder is unit-testable without a live SCWindow.
fn build_stream_config(cap_w: usize, cap_h: usize) -> Retained<SCStreamConfiguration> {
    // SAFETY: SCStreamConfiguration::new returns a freshly-allocated
    // SCStreamConfiguration; the setters are documented thread-safe
    // on a fresh, unshared instance.
    let config = unsafe { SCStreamConfiguration::new() };
    unsafe {
        config.setWidth(cap_w);
        config.setHeight(cap_h);
        // kCVPixelFormatType_32BGRA = FourCC 'BGRA'.
        // SCStreamConfiguration accepts the FourCC packed as a big-
        // endian u32.
        config.setPixelFormat(u32::from_be_bytes(*b"BGRA"));
        config.setShowsCursor(false);
    }
    config
}

/// Synchronously fetch one CGImage via `SCScreenshotManager`. Parks
/// on a DispatchSemaphore with [`CAPTURE_TIMEOUT`] (T1 mandate);
/// timeout maps to `CaptureFailed { stage: STAGE_CAPTURE_RETURNED_NIL }`.
///
/// The slot is wrapped in `Arc<Mutex<...>>` and a clone is moved
/// into the completion-handler closure so the slot's lifetime
/// outlasts the function on a timeout path. SCK retains its own
/// copy of the `RcBlock` on its dispatch queue; if the timeout fires
/// before the handler runs, this function returns Err and `block`
/// drops, but SCK's retained block is still alive — when SCK
/// eventually fires it (capture completed late, system was slow),
/// the closure dereferences a still-live Arc<Mutex<...>> rather
/// than a freed stack-local. The late-completion write is wasted
/// (the main thread already returned with an error) but safe.
///
/// The original spike used `(&raw const slot) as usize` to bypass
/// Send for a stack-local Mutex, relying on the wait/signal
/// handshake (`FOREVER` wait could not return before the signal).
/// v0.1.8 added a 5s timeout per T1, breaking that invariant — the
/// Arc keeps the soundness story sound.
fn capture_image_sync(
    filter: &SCContentFilter,
    config: &SCStreamConfiguration,
) -> Result<Retained<CGImage>> {
    let sem = DispatchSemaphore::new(0);
    let slot: Arc<ImageSlot> = Arc::new(ImageSlot(Mutex::new(None)));
    let block = RcBlock::new({
        let sem = sem.clone();
        let slot = Arc::clone(&slot);
        move |img: *mut CGImage, err: *mut NSError| {
            if !img.is_null() {
                // SAFETY: SCK passes us an autoreleased pointer per
                // its completion-handler contract; retain to keep
                // it past the block return.
                if let Some(retained) = unsafe { Retained::retain(img) } {
                    if let Ok(mut guard) = slot.0.lock() {
                        *guard = Some(retained);
                    }
                }
            } else if !err.is_null() {
                // SAFETY: SCK passes an autoreleased NSError pointer.
                if let Some(err) = unsafe { Retained::retain(err) } {
                    tracing::warn!(
                        target: "rok_bot",
                        nserror = %err,
                        "SCK capture completion handler reported error"
                    );
                }
            }
            sem.signal();
        }
    });
    // SAFETY: SCScreenshotManager class method; filter + config are
    // live retained references; the block is owned by SCK after the
    // call (it retains its own copy of the underlying ObjC block).
    unsafe {
        SCScreenshotManager::captureImageWithFilter_configuration_completionHandler(
            filter,
            config,
            Some(&block),
        );
    }

    if !wait_with_timeout(&sem, CAPTURE_TIMEOUT) {
        tracing::warn!(
            target: "rok_bot",
            timeout_ms = CAPTURE_TIMEOUT.as_millis() as u64,
            "SCK capture timed out — completion handler never fired (T1 deadlock fail-safe)"
        );
        return Err(BotError::CaptureFailed {
            stage: STAGE_CAPTURE_RETURNED_NIL,
            exit_code: None,
        });
    }

    let result = slot.0.lock().ok().and_then(|mut g| g.take());
    result.ok_or(BotError::CaptureFailed {
        stage: STAGE_CAPTURE_RETURNED_NIL,
        exit_code: None,
    })
}

/// Pull pixel bytes out of a CGImage and convert BGRA → RGBA via
/// the pure [`bgra_buffer_to_rgba`] helper. Splits the live FFI
/// (CGDataProvider + CFData length) from the pure pixel-byte logic
/// so tests can pin every conversion edge case without needing a
/// real CGImage.
fn cgimage_to_rgba(cg: &CGImage, w: usize, h: usize, bpr: usize) -> Option<Vec<u8>> {
    let provider = CGImage::data_provider(Some(cg))?;
    let data = CGDataProvider::data(Some(&provider))?;
    let len_signed = data.length();
    let ptr = data.byte_ptr();
    if ptr.is_null() || len_signed < 0 {
        return None;
    }
    let len = usize::try_from(len_signed).ok()?;
    // SAFETY: ptr is non-null and len ≥ 0 by guards above; the slice
    // is read-only for the lifetime of `data`, which we hold by
    // `CFRetained` here.
    let bytes: &[u8] = unsafe { std::slice::from_raw_parts(ptr, len) };
    bgra_buffer_to_rgba(bytes, w, h, bpr)
}

/// Pure: turn a BGRA pixel buffer (with possible row padding) into a
/// tightly-packed RGBA `Vec<u8>`. Returns `None` if the buffer is
/// undersized for the declared dims, OR if `w * 4` would overflow
/// `bpr`.
///
/// Why row padding matters: macOS pixel buffers are typically
/// padded so each row aligns to a multiple of 16 or 64 bytes for
/// SIMD friendliness. The spike observed
/// `bpr = 2102 * 4 + 40 = 8448` (40 bytes alignment padding) on a
/// real RoK capture. A naive flat-buffer copy that ignores padding
/// would shift every row by an increasing offset and produce a
/// sheared image.
fn bgra_buffer_to_rgba(bytes: &[u8], w: usize, h: usize, bpr: usize) -> Option<Vec<u8>> {
    let row_bytes = w.checked_mul(4)?;
    if bpr < row_bytes {
        return None;
    }
    let needed = bpr.checked_mul(h)?;
    if bytes.len() < needed {
        return None;
    }
    let mut rgba = Vec::with_capacity(w.checked_mul(h)?.checked_mul(4)?);
    for y in 0..h {
        let row_start = y.checked_mul(bpr)?;
        let row_end = row_start.checked_add(row_bytes)?;
        let row = bytes.get(row_start..row_end)?;
        for px in row.chunks_exact(4) {
            // BGRA → RGBA: swap byte 0 (B) with byte 2 (R), keep G
            // and A in place.
            rgba.extend_from_slice(&[px[2], px[1], px[0], px[3]]);
        }
    }
    Some(rgba)
}

/// Park on `sem` for up to `timeout`, returning `true` if signaled
/// or `false` if the timeout fired. Wraps the dispatch2 isize
/// return code in a clearer boolean so call sites read as
/// "if signaled, continue; else timeout."
///
/// Replaces every spike-era `sem.wait(DispatchTime::FOREVER)` per
/// T1 — never wait FOREVER on an SCK semaphore. Production must
/// always carry a real timeout so a hung SCK call surfaces as a
/// structured error rather than freezing the binary.
fn wait_with_timeout(sem: &DispatchSemaphore, timeout: Duration) -> bool {
    let dt = DispatchTime::try_from(timeout).unwrap_or(DispatchTime::FOREVER);
    sem.wait(dt) == 0
}

#[cfg(test)]
mod tests {
    use super::*;

    // ---------- bgra_buffer_to_rgba (pure) ----------

    /// Helper: build a synthetic BGRA buffer with the given dims and
    /// `bpr`, filling each pixel with a deterministic pattern based on
    /// its (x, y) so a misordered row or padding bug shows up as a
    /// recognizable shift in the converted RGBA.
    fn synth_bgra(w: usize, h: usize, bpr: usize) -> Vec<u8> {
        let mut buf = vec![0u8; bpr * h];
        for y in 0..h {
            for x in 0..w {
                let i = y * bpr + x * 4;
                // Encode (x, y) into B and R so swap order is visible.
                buf[i] = u8::try_from(x & 0xFF).unwrap_or(0); // B
                buf[i + 1] = u8::try_from((x ^ y) & 0xFF).unwrap_or(0); // G
                buf[i + 2] = u8::try_from(y & 0xFF).unwrap_or(0); // R
                buf[i + 3] = 255; // A
            }
        }
        buf
    }

    #[test]
    fn bgra_buffer_to_rgba_swaps_bgra_to_rgba_no_padding() {
        // bpr == w*4 → no padding; verify byte order swaps correctly.
        let w = 4;
        let h = 2;
        let bpr = w * 4;
        let bgra = synth_bgra(w, h, bpr);
        let rgba = bgra_buffer_to_rgba(&bgra, w, h, bpr).expect("conversion must succeed");
        assert_eq!(rgba.len(), w * h * 4);
        for y in 0..h {
            for x in 0..w {
                let i = (y * w + x) * 4;
                // After swap: R = synth's R = y, G = synth's G = x^y, B = synth's B = x, A = 255.
                assert_eq!(rgba[i], (y & 0xFF) as u8, "R at ({x},{y})");
                assert_eq!(rgba[i + 1], ((x ^ y) & 0xFF) as u8, "G at ({x},{y})");
                assert_eq!(rgba[i + 2], (x & 0xFF) as u8, "B at ({x},{y})");
                assert_eq!(rgba[i + 3], 255, "A at ({x},{y})");
            }
        }
    }

    #[test]
    fn bgra_buffer_to_rgba_strips_row_padding_when_bpr_exceeds_w_times_4() {
        // Spike observed bpr = w*4 + 40 (40 bytes alignment padding).
        // Pin that the converter ignores those 40 bytes per row.
        let w = 100;
        let h = 50;
        let bpr = w * 4 + 40;
        let bgra = synth_bgra(w, h, bpr);
        let rgba = bgra_buffer_to_rgba(&bgra, w, h, bpr).expect("padded conversion must succeed");
        assert_eq!(
            rgba.len(),
            w * h * 4,
            "RGBA must be tightly packed (no padding propagated)"
        );
        // Row 1's first pixel should still be at index w*4 (not w*4 + 40).
        let row1_first = w * 4;
        assert_eq!(rgba[row1_first], 1, "row 1 R == y == 1");
    }

    #[test]
    fn bgra_buffer_to_rgba_returns_none_for_undersized_buffer() {
        // Buffer is shorter than bpr * h → bail rather than
        // index past the end.
        let bytes = vec![0u8; 10];
        assert!(bgra_buffer_to_rgba(&bytes, 100, 100, 400).is_none());
    }

    #[test]
    fn bgra_buffer_to_rgba_returns_none_when_bpr_smaller_than_row_bytes() {
        // bpr < w*4 → invariant violated, fail closed.
        let bytes = vec![0u8; 1000];
        assert!(bgra_buffer_to_rgba(&bytes, 100, 1, 100).is_none());
    }

    #[test]
    fn bgra_buffer_to_rgba_handles_zero_height() {
        // Edge case: zero height should yield an empty RGBA buffer
        // without indexing.
        let bytes: Vec<u8> = Vec::new();
        let rgba = bgra_buffer_to_rgba(&bytes, 100, 0, 400).expect("zero height is valid");
        assert!(rgba.is_empty());
    }

    #[test]
    fn bgra_buffer_to_rgba_handles_zero_width() {
        // w=0 → row_bytes=0 → bpr must also be 0 (or any value, since
        // we copy 0 bytes per row). RGBA must be empty regardless of h.
        let bytes: Vec<u8> = vec![0u8; 100];
        let rgba = bgra_buffer_to_rgba(&bytes, 0, 5, 0).expect("zero width is valid");
        assert!(rgba.is_empty());
    }

    #[test]
    fn bgra_buffer_to_rgba_protects_against_dim_overflow() {
        // Adversarial: w * 4 would overflow usize. The pure helper
        // uses checked_mul; expect None rather than wrap-around or
        // panic.
        let bytes: Vec<u8> = vec![0u8; 16];
        // usize::MAX / 4 + 1 ensures w*4 overflows.
        let huge_w = usize::MAX / 4 + 1;
        let rgba = bgra_buffer_to_rgba(&bytes, huge_w, 1, huge_w);
        assert!(
            rgba.is_none(),
            "huge width must fail closed via checked_mul, not panic"
        );
    }

    // ---------- CAPTURE_TIMEOUT pin ----------

    #[test]
    fn capture_timeout_in_sane_range() {
        // 5 s is comfortably above the spike's 141ms steady-state and
        // the 200ms acceptance criterion, but small enough that a hung
        // SCK call surfaces inside one operator-attention window.
        // Pin so future tightening/loosening is intentional.
        assert!(
            CAPTURE_TIMEOUT >= Duration::from_secs(2),
            "CAPTURE_TIMEOUT below 2s risks timing out under capture-pipeline jitter"
        );
        assert!(
            CAPTURE_TIMEOUT <= Duration::from_secs(30),
            "CAPTURE_TIMEOUT above 30s makes a hung capture look like the binary froze"
        );
    }

    // ---------- wait_with_timeout (live dispatch2) ----------

    #[test]
    fn wait_with_timeout_returns_true_on_pre_signaled_semaphore() {
        // Pre-signal the semaphore (count starts at 0; signal bumps it
        // to 1 so wait succeeds immediately).
        let sem = DispatchSemaphore::new(0);
        sem.signal();
        assert!(
            wait_with_timeout(&sem, Duration::from_millis(100)),
            "pre-signaled semaphore must return true within timeout"
        );
    }

    #[test]
    fn wait_with_timeout_returns_false_when_no_signal_arrives() {
        // No signal will fire; wait must time out and return false
        // rather than blocking forever (T1 contract).
        let sem = DispatchSemaphore::new(0);
        let start = std::time::Instant::now();
        assert!(
            !wait_with_timeout(&sem, Duration::from_millis(50)),
            "unsignaled semaphore must return false after timeout"
        );
        // Sanity: it actually took at least the timeout.
        assert!(
            start.elapsed() >= Duration::from_millis(40),
            "wait_with_timeout must respect the requested duration"
        );
    }

    // ---------- build_stream_config ----------

    #[test]
    fn build_stream_config_produces_valid_configuration() {
        // We can't easily inspect SCStreamConfiguration's fields from
        // Rust (the binding doesn't expose getters for everything we
        // set), but we can confirm the call chain doesn't panic and
        // returns a Retained pointer. Real validation is via the
        // live integration tests in tests/sck_integration.rs — those
        // exercise the full filter+config+capture pipeline.
        let _config = build_stream_config(2102, 1640);
        // Smoke test: dropping the Retained must release cleanly.
    }

    // ---------- STAGE_* constants pinning ----------

    /// All five STAGE_* constants in this module must round-trip into
    /// the operator-facing log line via `BotError::CaptureFailed`. The
    /// error.rs side already tests the Display contract; here we just
    /// pin the literal string values so a typo can't silently slip
    /// through both modules' tests at once.
    #[test]
    fn stage_constants_match_documented_values() {
        assert_eq!(STAGE_SYMLINK_REFUSED, "symlink_refused");
        assert_eq!(STAGE_WINDOW_NOT_FOUND, "window_not_found");
        assert_eq!(STAGE_CAPTURE_RETURNED_NIL, "capture_returned_nil");
        assert_eq!(STAGE_CGIMAGE_DECODE, "cgimage_decode");
        assert_eq!(STAGE_PNG_WRITE, "png_write");
    }

    // ---------- open_capture_output_safely (race-free file open) ----------
    //
    // The v0.1.x symlink TOCTOU (pre-check then write) was closed in
    // v0.1.8 by replacing the pre-check with `O_NOFOLLOW + O_EXCL` at
    // the atomic open seam. These tests exercise the kernel-side
    // refusal directly — no SCWindow needed since the open path runs
    // before any SCK call.

    #[cfg(unix)]
    #[test]
    fn open_capture_output_safely_refuses_symlink_with_elool() {
        use std::os::unix::fs::symlink;
        let dir = tempfile::tempdir().expect("tempdir");
        let target = dir.path().join("target.png");
        let link = dir.path().join("link.png");
        fs::write(&target, b"do-not-overwrite-me").expect("seed target");
        symlink(&target, &link).expect("create symlink");

        let err = open_capture_output_safely(&link).expect_err("symlink must refuse");
        match err {
            BotError::CaptureFailed {
                stage,
                exit_code: None,
            } => {
                assert_eq!(
                    stage, STAGE_SYMLINK_REFUSED,
                    "ELOOP from O_NOFOLLOW must map to STAGE_SYMLINK_REFUSED"
                );
            }
            other => panic!("expected CaptureFailed{{symlink_refused}}, got {other:?}"),
        }

        // Confirm the target was NOT overwritten through the symlink.
        let contents = fs::read(&target).expect("target must still be readable");
        assert_eq!(
            contents, b"do-not-overwrite-me",
            "target must be untouched — the symlink defense MUST prevent writes"
        );
    }

    #[cfg(unix)]
    #[test]
    fn open_capture_output_safely_succeeds_on_fresh_path() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("fresh.png");
        let file = open_capture_output_safely(&path).expect("fresh path must open");
        drop(file);
        assert!(path.exists(), "fresh open must create the file");
    }

    #[cfg(unix)]
    #[test]
    fn open_capture_output_safely_replaces_stale_regular_file() {
        // Previous capture left a regular PNG at this path; the next
        // capture must succeed. The fs::remove_file + create_new
        // sequence handles this without following symlinks.
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("stale.png");
        fs::write(&path, b"stale data from previous run").expect("seed stale file");
        let file =
            open_capture_output_safely(&path).expect("stale regular file must be overwritten");
        drop(file);
        let contents = fs::read(&path).expect("file must be readable after re-open");
        assert!(
            contents.is_empty(),
            "open with create_new must produce an empty file (PNG bytes come from encoder)"
        );
    }
}
