//! Display arrangement and Mode detection.
//!
//! ASCII model of the coordinate space (CG / Quartz):
//!
//!     primary display (CGMainDisplayID)
//!     +--------------------------+
//!     | (0,0) origin             |
//!     |                          |
//!     |    primary bounds        |
//!     |                          |
//!     +--------------------------+
//!                          ↘ Y grows DOWNWARD
//!     X grows rightward
//!
//! Secondary displays sit at non-zero origins. A virtual display placed
//! "above" the primary in System Settings has a negative Y origin in CG
//! coordinates. A display placed "left" has negative X. The window center
//! comes from `kCGWindowBounds`, also CG coordinates, so no axis flip
//! is needed when checking which display contains the window.
//!
//! This is the **A1 fix** from /plan-eng-review: the previous design's
//! pseudocode mixed NSScreen (NS coords, Y-up, primary's bottom-left at
//! origin) with `kCGWindowBounds` (CG coords, Y-down, primary's top-left
//! at origin). On any multi-display setup that mismatch silently
//! misclassified windows. We now use CG-only — same space everywhere.

use core_graphics::display::CGDisplay;
use core_graphics::geometry::{CGPoint, CGRect};

use crate::error::{BotError, Result};
use crate::window::Window;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// Window center is on the **built-in** display (Retina panel).
    /// We test by `CGDisplayIsBuiltin`, NOT `CGDisplay::main` — the
    /// menu-bar display is configurable in System Settings, so "main"
    /// can be on an external monitor while the user's intent is
    /// "the laptop's screen." Mode 1 specifically means "visible on
    /// the laptop display." (Codex finding #3.)
    Visible,
    /// Window center is on a non-built-in display (BetterDisplay virtual,
    /// Sidecar, HDMI dummy plug, real second monitor — any of them).
    Virtual,
}

/// One display in the live arrangement, in CG coordinates.
///
/// Note: `CGRect` from `core-graphics-types` does not impl `PartialEq`,
/// so neither does `DisplayInfo`. Tests compare individual fields.
#[derive(Debug, Clone, Copy)]
pub struct DisplayInfo {
    pub bounds: CGRect,
    /// True if `CGDisplayIsBuiltin(id)` returns true. The built-in
    /// display is the laptop's Retina panel; iMacs and Studio Displays
    /// also report true. External monitors and BD virtual displays
    /// report false. Distinct from `CGMainDisplayID` (the menu-bar
    /// display) which the user can move around.
    pub is_builtin: bool,
}

/// Pure classifier — given a point and an arrangement, return the Mode.
///
/// Cleanly separates the system call (enumeration) from the logic
/// (containment test). All A1 regression cases are unit-tested against
/// this function.
///
/// # Errors
/// Returns [`BotError::WindowScreenUnresolved`] if the point falls outside
/// every display's bounds. This is a real condition during BD reconfiguration:
/// a window may briefly exist with no display under its center.
pub fn classify(center: CGPoint, displays: &[DisplayInfo]) -> Result<Mode> {
    for d in displays {
        if rect_contains(d.bounds, center) {
            return Ok(if d.is_builtin {
                Mode::Visible
            } else {
                Mode::Virtual
            });
        }
    }
    Err(BotError::WindowScreenUnresolved)
}

/// Half-open containment matching the spike: `[origin, origin + size)`.
/// Right and bottom edges belong to the next display, not this one — keeps
/// boundary cases deterministic when displays butt up against each other.
fn rect_contains(rect: CGRect, point: CGPoint) -> bool {
    point.x >= rect.origin.x
        && point.x < rect.origin.x + rect.size.width
        && point.y >= rect.origin.y
        && point.y < rect.origin.y + rect.size.height
}

/// Live system wrapper: enumerate online displays via Core Graphics
/// and classify the given window's center.
///
/// Re-enumerates on every call (no cached `CGDirectDisplayID`) per
/// Premise 8: the ID is not stable across BD disconnect/reconnect.
pub fn detect_mode(window: &Window) -> Result<Mode> {
    let displays = enumerate_displays();
    if displays.is_empty() {
        return Err(BotError::WindowScreenUnresolved);
    }
    classify(window.center(), &displays)
}

/// Pure: map a detected `Mode` to the v0.1 contract:
///     `Mode::Visible` → `Ok(())` (Mode 1 happy path)
///     `Mode::Virtual` → `Err(BotError::RokNotOnPrimary)` (Mode 2 deferred to v0.2)
///
/// Extracted from `main::run` so both arms are unit-testable. A future
/// refactor that flipped the arms or returned `Ok` for `Virtual` would
/// fail [`mode_visible_maps_to_ok`] / [`mode_virtual_maps_to_rok_not_on_primary`].
pub const fn mode_to_result(mode: Mode) -> Result<()> {
    match mode {
        Mode::Visible => Ok(()),
        Mode::Virtual => Err(BotError::RokNotOnPrimary),
    }
}

fn enumerate_displays() -> Vec<DisplayInfo> {
    let ids = match CGDisplay::active_displays() {
        Ok(ids) => ids,
        Err(err) => {
            tracing::warn!(
                target: "rok_bot",
                "CGDisplay::active_displays failed (CG error code {err:?}); \
                 detect_mode will return WindowScreenUnresolved",
            );
            return Vec::new();
        }
    };
    ids.into_iter()
        .map(|id| {
            let display = CGDisplay::new(id);
            DisplayInfo {
                bounds: display.bounds(),
                is_builtin: display.is_builtin(),
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use core_graphics::geometry::CGSize;

    fn rect(x: f64, y: f64, w: f64, h: f64) -> CGRect {
        CGRect::new(&CGPoint::new(x, y), &CGSize::new(w, h))
    }

    fn pt(x: f64, y: f64) -> CGPoint {
        CGPoint::new(x, y)
    }

    /// Test helper — built-in display (laptop panel). Test names use
    /// "primary" semantically; underlying field is `is_builtin`.
    fn primary(bounds: CGRect) -> DisplayInfo {
        DisplayInfo {
            bounds,
            is_builtin: true,
        }
    }

    fn secondary(bounds: CGRect) -> DisplayInfo {
        DisplayInfo {
            bounds,
            is_builtin: false,
        }
    }

    // ---- Single-display arrangements ----

    #[test]
    fn single_display_window_centered_is_visible() {
        let displays = [primary(rect(0.0, 0.0, 1920.0, 1080.0))];
        assert_eq!(
            classify(pt(960.0, 540.0), &displays).unwrap(),
            Mode::Visible
        );
    }

    #[test]
    fn single_display_window_off_screen_is_unresolved() {
        let displays = [primary(rect(0.0, 0.0, 1920.0, 1080.0))];
        let err = classify(pt(-100.0, -100.0), &displays).unwrap_err();
        assert_eq!(err, BotError::WindowScreenUnresolved);
    }

    // ---- A1 regression: secondary LEFT of primary (negative X coords) ----

    #[test]
    fn secondary_left_of_primary_window_on_secondary_is_virtual() {
        // BD virtual display arranged to the LEFT of primary in System Settings →
        // its CG bounds origin is at NEGATIVE X.
        let displays = [
            primary(rect(0.0, 0.0, 1920.0, 1080.0)),
            secondary(rect(-1920.0, 0.0, 1920.0, 1080.0)),
        ];
        // Center of the secondary display
        assert_eq!(
            classify(pt(-960.0, 540.0), &displays).unwrap(),
            Mode::Virtual,
            "window centered on the LEFT secondary should be Virtual; \
             this case is the regression that NSScreen+NS-coords would silently mis-classify"
        );
    }

    #[test]
    fn secondary_left_of_primary_window_on_primary_is_visible() {
        let displays = [
            primary(rect(0.0, 0.0, 1920.0, 1080.0)),
            secondary(rect(-1920.0, 0.0, 1920.0, 1080.0)),
        ];
        assert_eq!(
            classify(pt(960.0, 540.0), &displays).unwrap(),
            Mode::Visible
        );
    }

    // ---- A1 regression: secondary ABOVE primary (negative Y coords) ----

    #[test]
    fn secondary_above_primary_negative_y_is_virtual() {
        // Display arranged above primary → negative Y origin in CG coords.
        let displays = [
            primary(rect(0.0, 0.0, 1920.0, 1080.0)),
            secondary(rect(0.0, -1080.0, 1920.0, 1080.0)),
        ];
        assert_eq!(
            classify(pt(960.0, -540.0), &displays).unwrap(),
            Mode::Virtual
        );
    }

    // ---- A1 regression: 3-display arrangement, window on display #2 of 3 ----

    #[test]
    fn three_display_arrangement_window_on_middle_secondary() {
        let displays = [
            primary(rect(0.0, 0.0, 1920.0, 1080.0)),
            secondary(rect(1920.0, 0.0, 1920.0, 1080.0)),
            secondary(rect(3840.0, 0.0, 1920.0, 1080.0)),
        ];
        // Window on the middle (first non-primary) display
        assert_eq!(
            classify(pt(2880.0, 540.0), &displays).unwrap(),
            Mode::Virtual
        );
        // Window on the rightmost
        assert_eq!(
            classify(pt(4800.0, 540.0), &displays).unwrap(),
            Mode::Virtual
        );
        // Sanity: window on primary
        assert_eq!(
            classify(pt(960.0, 540.0), &displays).unwrap(),
            Mode::Visible
        );
    }

    // ---- Gap between displays (BD reconfiguration transient) ----

    #[test]
    fn point_in_gap_between_displays_is_unresolved() {
        // Two displays with a gap (1920..3000 in X). A window center landing
        // in the gap (most likely during a BD reconnect transient) returns
        // WindowScreenUnresolved, not a wrong Mode.
        let displays = [
            primary(rect(0.0, 0.0, 1920.0, 1080.0)),
            secondary(rect(3000.0, 0.0, 1920.0, 1080.0)),
        ];
        let err = classify(pt(2500.0, 540.0), &displays).unwrap_err();
        assert_eq!(err, BotError::WindowScreenUnresolved);
    }

    // ---- Boundary semantics ----

    #[test]
    fn boundary_belongs_to_display_whose_origin_includes_it() {
        // Two adjacent displays, no gap. Right edge of primary (x=1920) is the
        // origin of secondary. With half-open [origin, origin+size), the point
        // (1920, ...) belongs to secondary, not primary.
        let displays = [
            primary(rect(0.0, 0.0, 1920.0, 1080.0)),
            secondary(rect(1920.0, 0.0, 1920.0, 1080.0)),
        ];
        assert_eq!(
            classify(pt(1920.0, 540.0), &displays).unwrap(),
            Mode::Virtual
        );
        // ...and (1919, ...) still belongs to primary.
        assert_eq!(
            classify(pt(1919.0, 540.0), &displays).unwrap(),
            Mode::Visible
        );
    }

    #[test]
    fn empty_arrangement_returns_unresolved() {
        let err = classify(pt(0.0, 0.0), &[]).unwrap_err();
        assert_eq!(err, BotError::WindowScreenUnresolved);
    }

    // ---- Mode enum derives (C1) ----

    #[test]
    fn mode_supports_eq_and_copy() {
        let a = Mode::Visible;
        let b = a; // Copy
        assert_eq!(a, b);
        assert_ne!(Mode::Visible, Mode::Virtual);
    }

    // ---- mode_to_result contract (Testing T1) ----

    #[test]
    fn mode_visible_maps_to_ok() {
        assert!(mode_to_result(Mode::Visible).is_ok());
    }

    #[test]
    fn mode_virtual_maps_to_rok_not_on_primary() {
        assert_eq!(
            mode_to_result(Mode::Virtual).unwrap_err(),
            BotError::RokNotOnPrimary
        );
    }
}
