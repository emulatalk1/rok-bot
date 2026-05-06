// P2 spike — throwaway. Verifies the two load-bearing assumptions of rok-bot v0.1:
//   P2a: can we capture an off-screen RoK window?
//   P2b: will the off-screen window accept a synthetic click?
//
// If P2a fails with xcap, the architecture must switch to ScreenCaptureKit.
// If P2b fails, the off-screen-window technique is dead and v0.1 needs the
// visible-window-with-anti-focus-steal fallback (see ../../TODOS.md).
//
// Run:
//   cd spikes/p2-spike
//   cargo run --release -- --title "Rise of Kingdoms"
//
// First run will prompt for Screen Recording, Accessibility, and AppleEvents
// permissions. Each grant requires a binary restart.

use anyhow::{anyhow, Context, Result};
use enigo::{Button, Coordinate, Direction, Enigo, Mouse, Settings};
use image::RgbaImage;
use std::env;
use std::process::Command;
use std::thread::sleep;
use std::time::Duration;
use tracing::{error, info, warn};
use xcap::Window;

const OFF_X: i32 = -9999;
const OFF_Y: i32 = -9999;
const POST_REPOSITION_WAIT_MS: u64 = 600;
const POST_CLICK_WAIT_MS: u64 = 800;
const REPOSITION_TOLERANCE_PX: i32 = 10;
const P2B_DIFF_THRESHOLD_PER_PIXEL: f64 = 1.0; // L1 channel delta avg across the frame

fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_target(false)
        .with_max_level(tracing::Level::INFO)
        .init();

    let title_substring = parse_title_arg();
    info!("P2 spike — looking for window with title containing {:?}", title_substring);

    // ------------------------------------------------------------------ Step 0
    let windows = Window::all()
        .context("xcap Window::all() failed — Screen Recording permission likely missing")?;
    info!("xcap enumerated {} windows", windows.len());

    let target = windows
        .iter()
        .find(|w| w.title().unwrap_or_default().contains(&title_substring))
        .ok_or_else(|| {
            let titles: Vec<_> = windows
                .iter()
                .filter_map(|w| w.title().ok())
                .filter(|t| !t.is_empty())
                .collect();
            anyhow!(
                "no window matching {:?}. Visible window titles: {:?}",
                title_substring,
                titles
            )
        })?;

    let win_title = target.title().unwrap_or_default();
    let app_name = target.app_name().unwrap_or_default();
    let initial_x = target.x().context("read window x")?;
    let initial_y = target.y().context("read window y")?;
    let width = target.width().context("read window width")?;
    let height = target.height().context("read window height")?;
    info!(
        "target: title={:?} app={:?} pos=({}, {}) size={}x{}",
        win_title, app_name, initial_x, initial_y, width, height
    );

    // The osascript "tell process X" needs the *process* name. xcap's app_name()
    // gives that on macOS; if empty, we fall back to the title substring as a
    // reasonable guess (often the same on Mac App Store iOS apps).
    let process_name = if !app_name.is_empty() {
        app_name.clone()
    } else {
        title_substring.clone()
    };

    // ------------------------------------------------------------------ Step 1
    info!("STEP 1/5 — capturing visible-window baseline frame");
    let frame_baseline = target
        .capture_image()
        .context("xcap capture failed on visible window — sanity baseline broken, not a P2 result")?;
    save_frame(&frame_baseline, "/tmp/p2_spike_0_baseline.png")?;
    info!(
        "  saved /tmp/p2_spike_0_baseline.png ({}x{})",
        frame_baseline.width(),
        frame_baseline.height()
    );

    // ------------------------------------------------------------------ Step 2
    info!("STEP 2/5 — repositioning window off-screen via osascript");
    osascript_set_position(&process_name, OFF_X, OFF_Y)
        .context("osascript reposition — likely missing Automation/AppleEvents permission")?;
    sleep(Duration::from_millis(POST_REPOSITION_WAIT_MS));

    let (x_after, y_after) = osascript_get_position(&process_name)
        .context("osascript position query")?;
    let dx = (x_after - OFF_X).abs();
    let dy = (y_after - OFF_Y).abs();
    if dx > REPOSITION_TOLERANCE_PX || dy > REPOSITION_TOLERANCE_PX {
        warn!(
            "REPOSITION VERIFY FAILED — expected near ({},{}), got ({},{}). Window may not actually be off-screen; results below may be misleading.",
            OFF_X, OFF_Y, x_after, y_after
        );
    } else {
        info!("  reposition verified: window now at ({},{})", x_after, y_after);
    }

    // ------------------------------------------------------------------ Step 3 (P2a)
    info!("STEP 3/5 — P2a: capturing off-screen window");
    let target_after = re_find(&title_substring)
        .context("could not re-find window after reposition")?;

    let p2a_pass;
    let frame_pre_click;
    match target_after.capture_image() {
        Ok(img) => {
            save_frame(&img, "/tmp/p2_spike_1_offscreen_before_click.png")?;
            info!("  P2a PASS — saved /tmp/p2_spike_1_offscreen_before_click.png");
            p2a_pass = true;
            frame_pre_click = Some(img);
        }
        Err(e) => {
            error!(
                "  P2a FAIL — xcap could not capture off-screen window: {}. Architecture must use ScreenCaptureKit.",
                e
            );
            p2a_pass = false;
            frame_pre_click = None;
        }
    }

    // ------------------------------------------------------------------ Step 4 (P2b)
    let click_x = OFF_X + (width as i32) / 2;
    let click_y = OFF_Y + (height as i32) / 2;
    info!(
        "STEP 4/5 — P2b: posting click at off-screen coords ({}, {})",
        click_x, click_y
    );
    let mut enigo = Enigo::new(&Settings::default())
        .context("enigo init — Accessibility permission likely missing")?;
    enigo
        .move_mouse(click_x, click_y, Coordinate::Abs)
        .context("enigo move_mouse")?;
    sleep(Duration::from_millis(50));
    enigo
        .button(Button::Left, Direction::Click)
        .context("enigo button click")?;
    info!("  click event posted; waiting {}ms", POST_CLICK_WAIT_MS);
    sleep(Duration::from_millis(POST_CLICK_WAIT_MS));

    // ------------------------------------------------------------------ Step 5
    info!("STEP 5/5 — capturing post-click frame and computing diff");
    let target_post = re_find(&title_substring)?;
    let p2b_diff_per_pixel = match (&frame_pre_click, target_post.capture_image()) {
        (Some(a), Ok(b)) => {
            save_frame(&b, "/tmp/p2_spike_2_offscreen_after_click.png")?;
            let total = pixel_diff_l1(a, &b);
            let per_pixel = total as f64 / (a.width() * a.height()).max(1) as f64;
            info!(
                "  diff: {} total ({:.4} avg L1 channel delta per pixel)",
                total, per_pixel
            );
            Some(per_pixel)
        }
        (None, _) => {
            warn!("  cannot evaluate P2b: P2a failed, no before-frame to compare");
            None
        }
        (_, Err(e)) => {
            error!("  post-click capture failed: {}", e);
            None
        }
    };

    // -------------------------------------------------------------- Restoration
    info!(
        "restoring window to original position ({}, {})",
        initial_x, initial_y
    );
    if let Err(e) = osascript_set_position(&process_name, initial_x, initial_y) {
        warn!("  could not restore window: {}. Move it back manually.", e);
    }

    // ------------------------------------------------------------------- Verdict
    println!();
    println!("==================================================");
    println!("              P2 SPIKE VERDICT");
    println!("==================================================");
    if p2a_pass {
        println!(" P2a (xcap captures off-screen window):  PASS");
    } else {
        println!(" P2a (xcap captures off-screen window):  FAIL  → use ScreenCaptureKit instead");
    }
    match p2b_diff_per_pixel {
        Some(d) if d > P2B_DIFF_THRESHOLD_PER_PIXEL => println!(
            " P2b (off-screen click landed):          LIKELY PASS  (diff {:.3}/pixel)",
            d
        ),
        Some(d) => println!(
            " P2b (off-screen click landed):          LIKELY FAIL  (diff {:.3}/pixel — noise level)",
            d
        ),
        None => println!(" P2b (off-screen click landed):          UNKNOWN (need P2a frames)"),
    }
    println!("--------------------------------------------------");
    println!(" Inspect frames manually to confirm:");
    println!("   /tmp/p2_spike_0_baseline.png");
    println!("   /tmp/p2_spike_1_offscreen_before_click.png");
    println!("   /tmp/p2_spike_2_offscreen_after_click.png");
    println!();
    println!(" P2b is PASS only if the post-click frame is meaningfully");
    println!(" different from the pre-click frame in a way that's NOT");
    println!(" just animation noise. Animations alone produce ~0.5-2.0/pixel diff.");
    println!("==================================================");
    Ok(())
}

fn parse_title_arg() -> String {
    let args: Vec<String> = env::args().collect();
    args.iter()
        .position(|a| a == "--title")
        .and_then(|i| args.get(i + 1).cloned())
        .unwrap_or_else(|| "Rise of Kingdoms".to_string())
}

fn re_find(title_substring: &str) -> Result<Window> {
    let windows = Window::all()?;
    windows
        .into_iter()
        .find(|w| w.title().unwrap_or_default().contains(title_substring))
        .ok_or_else(|| anyhow!("window matching {:?} disappeared", title_substring))
}

fn save_frame(img: &RgbaImage, path: &str) -> Result<()> {
    img.save(path).with_context(|| format!("save {}", path))
}

fn osascript_set_position(process_name: &str, x: i32, y: i32) -> Result<()> {
    let script = format!(
        r#"tell application "System Events" to tell process "{}" to set position of window 1 to {{{}, {}}}"#,
        process_name, x, y
    );
    let output = Command::new("osascript")
        .arg("-e")
        .arg(&script)
        .output()
        .context("osascript invocation")?;
    if !output.status.success() {
        anyhow::bail!(
            "osascript exited {}: stderr={}",
            output.status,
            String::from_utf8_lossy(&output.stderr)
        );
    }
    Ok(())
}

fn osascript_get_position(process_name: &str) -> Result<(i32, i32)> {
    let script = format!(
        r#"tell application "System Events" to tell process "{}" to get position of window 1"#,
        process_name
    );
    let output = Command::new("osascript")
        .arg("-e")
        .arg(&script)
        .output()
        .context("osascript invocation")?;
    let text = String::from_utf8_lossy(&output.stdout).trim().to_string();
    let parts: Vec<&str> = text.split(',').map(str::trim).collect();
    if parts.len() < 2 {
        anyhow::bail!("unexpected osascript output: {:?}", text);
    }
    Ok((parts[0].parse()?, parts[1].parse()?))
}

fn pixel_diff_l1(a: &RgbaImage, b: &RgbaImage) -> u64 {
    if a.dimensions() != b.dimensions() {
        return u64::MAX;
    }
    a.pixels()
        .zip(b.pixels())
        .map(|(pa, pb)| {
            pa.0.iter()
                .zip(pb.0.iter())
                .map(|(ca, cb)| (i32::from(*ca) - i32::from(*cb)).unsigned_abs() as u64)
                .sum::<u64>()
        })
        .sum()
}
