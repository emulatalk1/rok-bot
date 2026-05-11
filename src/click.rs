//! Synthetic click delivery.
//!
//! v0.1.5 routes through the macOS Accessibility API instead of
//! `CGEventPost`. The module is now a thin wrapper around `ax::press_at`
//! — pre-v0.1.5 logic lives in git history (`d2ce910..b1ab9ac` for the
//! CGEvent-based path; `b1ab9ac` is the p5-spike that proved the switch
//! was required). See `src/ax.rs` for the new flow's full doc.
//!
//! Why `click.rs` still exists as a separate module:
//!
//! - **Public API stability.** `main.rs::run` calls `click_at(...)` —
//!   keeping the call site signature minimizes ripple in C4 and any
//!   future click-mechanism swap (e.g., if AX press ever stops working
//!   and we need to add a different path, only this module's body
//!   changes).
//! - **Tracing surface.** The post-press info log line summarizes the
//!   click attempt at the call layer; lower in `ax::press_at` we log
//!   the per-step AX details. Two log levels of granularity.
//!
//! Things that USED to be in this module and got deleted in v0.1.5:
//!
//! - `build_click_events` — CGEvent down/up pair constructor. AX press
//!   is a single atomic action, no pair.
//! - `CLICK_GAP_MS` — sleep between down and up. No longer applicable.
//! - `REASON_SOURCE` / `REASON_DOWN` / `REASON_UP` — Quartz creation-
//!   time failure tags. Replaced by `ax::REASON_AX_*`.
//!
//! Re-exported aliases for the legacy reason names are intentionally
//! NOT provided: dead-code paths shouldn't accumulate.

use core_graphics::display::CGPoint;

use crate::ax;
use crate::error::Result;
use crate::window::Window;

/// Synthesize and post a single press at `point` (CG global-screen
/// coords) within the AX hierarchy of the process owning `window.pid`.
/// Delegates to [`ax::press_at`].
///
/// Caller (i.e., `main.rs::run`) must have already verified Accessibility
/// permission via `permissions::check_accessibility()` AND re-validated
/// the window state via `window::validate_at_click_site(&window, point)`.
/// Without those, the press path can fail in subtle ways:
/// `ClickFailed { REASON_AX_APP_RESOLVE_FAILED }` if AX is denied, or
/// `ClickFailed { REASON_AX_ELEMENT_RESOLVE_FAILED }` if the window
/// vanished mid-flight.
pub fn click_at(window: &Window, point: CGPoint) -> Result<()> {
    ax::press_at(window, point)
}
