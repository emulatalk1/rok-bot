# Cargo Dependency Audit — RoK Rust Bot

**Date of audit:** 2026-05-06
**Source document:** `rok_rust_bot_research.md` § 5 (Full Verified Cargo.toml)
**Method:** Live query of the crates.io JSON API (`/api/v1/crates/<name>`) for each dependency. crates.io is a client-rendered SPA, so HTML scraping returns an empty shell — only the API call returns real version data. All timestamps below are taken from the `versions[].updated_at` field.

> **Status (snapshot — see [Cargo.toml](../Cargo.toml) for the live set):** This audit was the pre-implementation pin set. The shipped v0.1.x runtime stack as of v0.1.4 is `core-graphics 0.25`, `core-foundation 0.10`, `objc2-app-kit 0.3` (NSRunningApplication only), `objc2-foundation 0.3` (NSString only), `thiserror 2`, `anyhow 1`, `tracing 0.1`, `tracing-subscriber 0.3`, `image 0.25` (PNG only), `imageproc 0.26` (rayon feature), `tempfile 3`. Notable rejections from this audit: `xcap` (replaced by `screencapture` CLI), `enigo` (replaced by direct `core-graphics::event::CGEvent::post`), `screencapturekit`/`objc2-screen-capture-kit` (deferred to v0.2 when the continuous loop replaces single-shot CLI capture). Still out of scope: `tesseract` (OCR — no current need), `reqwest`/`tokio`/`dotenvy`/`rand` (no networking yet). Bundle-ID anti-spoof via `NSRunningApplication.runningApplicationWithProcessIdentifier` is the use case for `objc2-app-kit` + `objc2-foundation`.

---

## Executive summary

Of the **17 dependencies** declared in the research doc's Cargo.toml:

- **9 are current** and resolve correctly under the declared semver caret.
- **6 are outdated** by a minor or major version (xcap, enigo, reqwest, imageproc, rand, and the implicit core-foundation pin).
- **1 is a build-blocker** — `core-foundation = "0.9"` is incompatible with the declared `core-graphics = "0.25"`, which depends on `core-foundation 0.10.x`. The project will likely fail to compile as written.
- **2 stale-crate avoidance claims hold up**: `leptess` (last release Feb 2023) and `dotenv` (last release Oct 2019) are correctly avoided. However, the doc's framing of `dotenvy` as the "maintained replacement" is misleading — `dotenvy` itself has not shipped a release since March 2023.

The headline number: **the doc's Cargo.toml will not build as-is.** The core-foundation/core-graphics pinning conflict needs to be resolved before any of the other findings matter.

---

## Detailed findings

### Build-blocker (fix before anything else)

**core-foundation pin is incompatible with core-graphics 0.25**

`core-graphics 0.25.0` was published 2025-05-27, one day after `core-foundation 0.10.1` (2025-05-26). The 0.25 line of core-graphics is built against the 0.10 line of core-foundation; pinning `core-foundation = "0.9"` alongside `core-graphics = "0.25"` mixes two incompatible major versions of the same FFI types. Cargo will either refuse the resolve or — worse — pull both versions in and surface confusing type-mismatch errors at use sites that reference CFString/CFType handles.

**Fix:** `core-foundation = "0.10"`.

---

### Outdated dependencies

| Crate | Doc pin | Latest stable | Last publish | Bump type |
|---|---|---|---|---|
| xcap | `0.8` | **0.9.4** | 2026-04-09 | minor (0.x.y, breaking) |
| enigo | `0.5` | **0.6.1** | 2025-08-28 | minor (0.x.y, breaking) |
| reqwest | `0.12` | **0.13.3** | 2026-04-27 | minor (0.x.y, breaking) |
| imageproc | `0.25` | **0.26.2** | 2026-05-01 | minor (0.x.y, breaking) |
| rand | `0.8` | **0.10.1** | 2026-04-11 | two minor bumps behind |
| core-foundation | `0.9` | **0.10.1** | 2025-05-26 | minor + see build-blocker above |

**Notes on each:**

- **xcap 0.9** has had four point releases on the 0.9 line over a one-month window (0.9.0 → 0.9.4, 2026-03-09 to 2026-04-09), suggesting active rework. Review the changelog before adopting.
- **enigo 0.6.0 and 0.6.1** shipped the same day (2025-08-28), implying a quick post-release follow-up; treat 0.6.1 as the real first-stable.
- **reqwest 0.13** has been out since 2025-12-30 (5+ months stable). 0.12 is one major behind.
- **imageproc** is mid-transition: the maintainer republished **0.23.1, 0.24.1, 0.25.1, and 0.26.2 all on the same day (2026-05-01)**, which strongly suggests a coordinated backport release. 0.26 is the new mainline; older lines are still receiving fixes.
- **rand** is the most nuanced: `max_stable` is 0.10.1, but 0.8.6 was patched on 2026-04-17 — so 0.8 is still maintained, not stale. For a new project, however, prefer 0.10. (This directly answers Outstanding Question #4 in the research doc.)

---

### Stale-crate avoidance — claims verified

The doc deliberately avoids two crates. Both calls hold up:

- **leptess** — last publish 2023-02-21. Three years stale, no successor releases. ✅ Avoid.
- **dotenv** — last publish 2019-10-21. Six and a half years abandoned. ✅ Avoid.

**Caveat on the recommended replacement:** the doc describes `dotenvy` as "actively maintained." That is misleading. `dotenvy 0.15.7` last shipped on 2023-03-22 — over three years ago. The crate is *stable* (the API is small enough that no release has been needed), but it is not actively developed. This does not change the recommendation — `dotenvy` is still the better pick than `dotenv` — but the framing in the research doc should be softened.

---

### Current dependencies (no action required)

| Crate | Doc pin | Resolves to | Last publish |
|---|---|---|---|
| screencapturekit | `1.5` | 1.5.4 | 2026-03-09 | ⚠️ build needs full Xcode (Swift bridge); use `objc2-screen-capture-kit` instead
| core-graphics | `0.25` | 0.25.0 | 2025-05-27 |
| image | `0.25` | 0.25.10 | 2026-03-10 |
| tesseract | `0.15` | 0.15.2 | 2025-04-19 |
| tokio | `1` | 1.52.2 | 2026-05-04 |
| serde | `1` | 1.0.228 | 2025-09-27 |
| serde_json | `1` | 1.0.149 | 2026-01-06 |
| tracing | `0.1` | 0.1.44 | 2025-12-18 |
| tracing-subscriber | `0.3` | 0.3.23 | 2026-03-13 |
| anyhow | `1` | 1.0.102 | 2026-02-20 |

`tokio 1.52.2` was published two days before this audit — the dependency tree is on the leading edge for the major libraries.

---

## Updated Cargo.toml

Apply six pin changes (one fixes the build, five take advantage of newer releases):

```toml
[dependencies]

# Screen Capture
# screencapturekit = "1.5"   # ⚠️ Swift-bridge build requires full Xcode SDK
# Use objc2-screen-capture-kit (raw ObjC2 bindings, builds with CLT alone) instead.
objc2-screen-capture-kit = "0.3"  # ObjC2 SCK bindings — preferred for v0.1+
xcap             = "0.9"      # was "0.8" — 0.9 is the new mainline

# Input
enigo            = "0.6"      # was "0.5"
core-graphics    = "0.25"
core-foundation  = "0.10"     # was "0.9" — REQUIRED by core-graphics 0.25

# Vision
image            = "0.25"
imageproc        = "0.26"     # was "0.25"

# OCR
tesseract        = "0.15"

# Async
tokio            = { version = "1", features = ["full"] }

# HTTP
reqwest          = { version = "0.13", features = ["json"] }   # was "0.12"

# Config
serde            = { version = "1", features = ["derive"] }
serde_json       = "1"
dotenvy          = "0.15"     # stable but unchanged since 2023-03

# Logging & Errors
tracing              = "0.1"
tracing-subscriber   = { version = "0.3", features = ["env-filter"] }
anyhow               = "1"

# Utils
rand             = "0.10"     # was "0.8" — 0.9 and 0.10 both released
```

---

## Risk assessment for the migration

| Change | Migration risk | Why |
|---|---|---|
| core-foundation 0.9 → 0.10 | **Required.** Low risk. | The whole point of bumping is alignment with core-graphics 0.25. APIs in this layer are thin FFI wrappers. |
| xcap 0.8 → 0.9 | Medium. | 0.9 had four point releases in a month — API was in flux. Read CHANGELOG before adopting. |
| enigo 0.5 → 0.6 | Medium. | Major bump for an input library; signature changes are likely on the `Mouse`/`Keyboard` traits. |
| reqwest 0.12 → 0.13 | Medium. | reqwest minors typically touch the builder API and rustls/tls feature surface. |
| imageproc 0.25 → 0.26 | Low–medium. | `match_template` signature has been stable across recent versions; verify `MatchTemplateMethod::CrossCorrelationNormalized` still exists (Outstanding Question #5). |
| rand 0.8 → 0.10 | Medium. | rand 0.9 reorganized the prelude and trait names; 0.10 is a smaller follow-up. If migration cost matters, 0.8.6 is still patched — staying on 0.8 is defensible. |

---

## Methodology notes

- All data pulled from `https://crates.io/api/v1/crates/<name>` on 2026-05-06. The `crate.max_stable_version`, `crate.newest_version`, and the top of `versions[]` were inspected for each crate.
- WebFetch against the rendered crates.io HTML returns no version data — the page is a Vue SPA and only the API exposes the registry contents. Any future re-verification should use the API.
- "Last publish" reflects the most recent stable release timestamp. Yanked versions (e.g., `image 0.25.7`, `tracing 0.1.42`, `tracing-subscriber 0.3.21`) are flagged in the raw data but not surfaced as candidate versions.

---

## Recommended next steps

1. **Apply the core-foundation fix immediately** — without it, nothing else in the dependency tree matters.
2. **Decide on rand strategy** — bump to 0.10 for new code, or stay on 0.8 if the rest of the dependency graph hasn't migrated. Both are defensible; the research doc should pick one.
3. **Run `cargo update --dry-run`** after the pin changes to surface any transitive resolver conflicts before committing.
4. **Re-verify Outstanding Questions #5 and #6** in the research doc against the *new* pin set: imageproc 0.26's `MatchTemplateMethod` API and screencapturekit 1.5.4's docs.rs build status (1.5.0 had build issues per the research doc; 1.5.4 may not).
