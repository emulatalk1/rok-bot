//! P8 spike — `objc2-screen-capture-kit` one-shot CGWindow capture.
//!
//! Goal: prove `objc2-screen-capture-kit` 0.3.x can capture a single
//! PNG of a known `CGWindowID` from a CLT-only Rust binary (no Xcode),
//! replacing the v0.1.x `screencapture -l <wid>` CLI subprocess that
//! costs 200-1400ms per call.
//!
//! Run procedure: see `README.md`.

use std::env;
use std::process::ExitCode;
use std::sync::Mutex;
use std::time::Instant;

use block2::RcBlock;
use dispatch2::{DispatchSemaphore, DispatchTime};
use objc2::AllocAnyThread;
use objc2::rc::Retained;
use objc2_core_graphics::{CGDataProvider, CGImage};
use objc2_foundation::NSError;
use objc2_screen_capture_kit::{
    SCContentFilter, SCScreenshotManager, SCShareableContent, SCStreamConfiguration, SCWindow,
};

const EXIT_OK: u8 = 0;
const EXIT_BAD_USAGE: u8 = 64;
const EXIT_NO_CONTENT: u8 = 10;
const EXIT_WINDOW_NOT_FOUND: u8 = 11;
const EXIT_CAPTURE_FAILED: u8 = 12;
const EXIT_IMAGE_DECODE_FAILED: u8 = 13;
const EXIT_PNG_WRITE_FAILED: u8 = 14;

// Bootstraps the CGS (Core Graphics Services) connection so ScreenCaptureKit
// can build SCStreamConfiguration objects without tripping the
// `CGS_REQUIRE_INIT` assertion. A normal AppKit app gets this for free when
// `NSApplication` is touched; a CLI Rust binary that only pulls in CG bindings
// doesn't, and SCK aborts on first use. `NSApplicationLoad` is the documented
// "register with WindowServer without entering a run loop" entry point and is
// safe to call once at startup. Linked via `AppKit.framework`.
#[link(name = "AppKit", kind = "framework")]
unsafe extern "C" {
    fn NSApplicationLoad() -> bool;
}

fn main() -> ExitCode {
    let (wid, out) = match parse_args() {
        Ok(a) => a,
        Err(e) => {
            print_usage(&e);
            return ExitCode::from(EXIT_BAD_USAGE);
        }
    };

    // SAFETY: idempotent; documented to be safe at any point before AppKit use.
    let app_loaded = unsafe { NSApplicationLoad() };
    eprintln!("[p8-spike] NSApplicationLoad → {app_loaded}");

    let t0 = Instant::now();

    // 1. Fetch shareable content (async → sync via semaphore).
    eprintln!("[p8-spike] requesting SCShareableContent…");
    let content = match get_shareable_content() {
        Some(c) => c,
        None => {
            eprintln!(
                "[p8-spike] no shareable content (Screen Recording denied? \
                 grant it to this binary or its parent terminal, then retry)"
            );
            return ExitCode::from(EXIT_NO_CONTENT);
        }
    };
    let t_content = t0.elapsed();
    eprintln!(
        "[p8-spike] got shareable content in {} ms",
        t_content.as_millis()
    );

    // 2. Find the SCWindow for the requested CGWindowID.
    let target = match find_window(&content, wid) {
        Some(w) => w,
        None => {
            eprintln!(
                "[p8-spike] CGWindowID {wid} not present in shareable content. \
                 Common causes: window not owned by an app on a captured display, \
                 wid stale (re-query via the parent rok-bot), or the owning app \
                 hasn't been granted Screen Recording."
            );
            return ExitCode::from(EXIT_WINDOW_NOT_FOUND);
        }
    };
    let frame = unsafe { target.frame() };
    eprintln!(
        "[p8-spike] found SCWindow wid={} frame=({:.0},{:.0}) {:.0}x{:.0} (points)",
        wid, frame.origin.x, frame.origin.y, frame.size.width, frame.size.height
    );

    // 3. Build content filter (desktop-independent: just this window).
    let filter = unsafe {
        SCContentFilter::initWithDesktopIndependentWindow(SCContentFilter::alloc(), &target)
    };

    // 4. Build stream config. Match screencapture's behavior: backing-pixel
    //    resolution (≈ points × 2 on Retina), no cursor.
    let config = unsafe { SCStreamConfiguration::new() };
    let scale = 2usize; // Retina assumption; user can re-run on 1x BD to check.
    let cap_w = (frame.size.width as usize).saturating_mul(scale);
    let cap_h = (frame.size.height as usize).saturating_mul(scale);
    unsafe {
        config.setWidth(cap_w);
        config.setHeight(cap_h);
        // kCVPixelFormatType_32BGRA = FourCC 'BGRA'. SCStreamConfiguration
        // accepts the FourCC packed as a big-endian u32.
        config.setPixelFormat(u32::from_be_bytes(*b"BGRA"));
        config.setShowsCursor(false);
    }

    // 5. Capture (async → sync).
    let t_pre_capture = t0.elapsed();
    eprintln!("[p8-spike] requesting SCScreenshotManager capture {cap_w}x{cap_h}…");
    let cg = match capture_image(&filter, &config) {
        Some(img) => img,
        None => {
            eprintln!("[p8-spike] capture returned no image");
            return ExitCode::from(EXIT_CAPTURE_FAILED);
        }
    };
    let t_post_capture = t0.elapsed();
    let capture_ms = (t_post_capture - t_pre_capture).as_millis();

    // 6. CGImage → RGBA bytes → PNG on disk.
    let pw = CGImage::width(Some(&cg));
    let ph = CGImage::height(Some(&cg));
    let bpr = CGImage::bytes_per_row(Some(&cg));
    eprintln!("[p8-spike] image: {pw}x{ph} bytes_per_row={bpr}");

    let rgba = match cgimage_to_rgba(&cg, pw, ph, bpr) {
        Some(v) => v,
        None => {
            eprintln!("[p8-spike] failed to read CGImage pixel data");
            return ExitCode::from(EXIT_IMAGE_DECODE_FAILED);
        }
    };

    if let Err(err) = image::save_buffer(&out, &rgba, pw as u32, ph as u32, image::ColorType::Rgba8)
    {
        eprintln!("[p8-spike] PNG write to {out} failed: {err}");
        return ExitCode::from(EXIT_PNG_WRITE_FAILED);
    }

    let total_ms = t0.elapsed().as_millis();
    eprintln!(
        "[p8-spike] captured {pw}x{ph} → {out} \
         (content {} ms, capture {} ms, total {} ms)",
        t_content.as_millis(),
        capture_ms,
        total_ms,
    );

    ExitCode::from(EXIT_OK)
}

fn print_usage(err: &str) {
    eprintln!("usage: p8-spike --wid <u32> --out <path.png>");
    eprintln!("       --wid: CGWindowID of the target window (see parent rok-bot log)");
    eprintln!("       --out: PNG output path");
    eprintln!("error: {err}");
}

fn parse_args() -> Result<(u32, String), String> {
    let mut wid: Option<u32> = None;
    let mut out: Option<String> = None;
    let mut args = env::args().skip(1);
    while let Some(flag) = args.next() {
        match flag.as_str() {
            "--wid" => {
                let v = args
                    .next()
                    .ok_or_else(|| "--wid missing value".to_string())?;
                wid = Some(v.parse().map_err(|e| format!("--wid: {e}"))?);
            }
            "--out" => {
                out = Some(
                    args.next()
                        .ok_or_else(|| "--out missing value".to_string())?,
                );
            }
            "-h" | "--help" => return Err("help requested".to_string()),
            other => return Err(format!("unknown flag: {other}")),
        }
    }
    let wid = wid.ok_or_else(|| "--wid required".to_string())?;
    let out = out.ok_or_else(|| "--out required".to_string())?;
    Ok((wid, out))
}

/// Synchronously fetch `SCShareableContent` by parking on a dispatch
/// semaphore while SCK runs the completion handler on its own queue.
fn get_shareable_content() -> Option<Retained<SCShareableContent>> {
    let sem = DispatchSemaphore::new(0);
    let slot: Mutex<Option<Retained<SCShareableContent>>> = Mutex::new(None);
    let block = RcBlock::new({
        let sem = sem.clone();
        let slot = &slot as *const Mutex<Option<Retained<SCShareableContent>>>;
        // Cast to usize so the closure is Send-safe; we'll deref under the
        // single-threaded semaphore-wait invariant.
        let slot_addr = slot as usize;
        move |content: *mut SCShareableContent, err: *mut NSError| {
            if !content.is_null() {
                // SAFETY: SCK passes us an autoreleased pointer; retain to
                // keep it past the block return.
                if let Some(retained) = unsafe { Retained::retain(content) } {
                    let m = unsafe {
                        &*(slot_addr as *const Mutex<Option<Retained<SCShareableContent>>>)
                    };
                    *m.lock().unwrap() = Some(retained);
                }
            } else if !err.is_null() {
                eprintln!("[p8-spike] shareable content error: {:?}", err);
            }
            sem.signal();
        }
    });
    unsafe { SCShareableContent::getShareableContentWithCompletionHandler(&block) };
    sem.wait(DispatchTime::FOREVER);
    slot.lock().unwrap().take()
}

/// Linear scan of `content.windows` for a matching `CGWindowID`.
/// SCK gates the list on Screen Recording trust — windows we can't capture
/// won't appear here.
fn find_window(content: &SCShareableContent, wid: u32) -> Option<Retained<SCWindow>> {
    let windows = unsafe { content.windows() };
    for i in 0..windows.count() {
        let w = windows.objectAtIndex(i);
        if unsafe { w.windowID() } == wid {
            return Some(w);
        }
    }
    None
}

/// Synchronously capture one frame to a `CGImage` via `SCScreenshotManager`.
fn capture_image(
    filter: &SCContentFilter,
    config: &SCStreamConfiguration,
) -> Option<Retained<CGImage>> {
    let sem = DispatchSemaphore::new(0);
    let slot: Mutex<Option<Retained<CGImage>>> = Mutex::new(None);
    let block = RcBlock::new({
        let sem = sem.clone();
        let slot_addr = (&slot as *const Mutex<Option<Retained<CGImage>>>) as usize;
        move |img: *mut CGImage, err: *mut NSError| {
            if !img.is_null() {
                if let Some(retained) = unsafe { Retained::retain(img) } {
                    let m = unsafe { &*(slot_addr as *const Mutex<Option<Retained<CGImage>>>) };
                    *m.lock().unwrap() = Some(retained);
                }
            } else if !err.is_null() {
                eprintln!("[p8-spike] capture error: {:?}", err);
            }
            sem.signal();
        }
    });
    unsafe {
        SCScreenshotManager::captureImageWithFilter_configuration_completionHandler(
            filter,
            config,
            Some(&block),
        );
    }
    sem.wait(DispatchTime::FOREVER);
    slot.lock().unwrap().take()
}

/// Pull pixel bytes out of a CGImage and convert BGRA → RGBA, stripping
/// row padding (`bytes_per_row` is often padded past `width * 4` for
/// alignment).
fn cgimage_to_rgba(cg: &CGImage, w: usize, h: usize, bpr: usize) -> Option<Vec<u8>> {
    let provider = CGImage::data_provider(Some(cg))?;
    let data = CGDataProvider::data(Some(&provider))?;
    let len_signed = data.length();
    let ptr = data.byte_ptr();
    if ptr.is_null() || len_signed < 0 {
        return None;
    }
    let len = len_signed as usize;
    if len < bpr.saturating_mul(h) {
        return None;
    }
    // SAFETY: ptr is non-null and len ≥ bpr*h; the slice is read-only for
    // the lifetime of `data`, which we hold by `CFRetained` here.
    let bytes: &[u8] = unsafe { std::slice::from_raw_parts(ptr, len) };
    let mut rgba = Vec::with_capacity(w * h * 4);
    for y in 0..h {
        let row_start = y * bpr;
        let row = &bytes[row_start..row_start + w * 4];
        for px in row.chunks_exact(4) {
            // BGRA → RGBA
            rgba.extend_from_slice(&[px[2], px[1], px[0], px[3]]);
        }
    }
    Some(rgba)
}
