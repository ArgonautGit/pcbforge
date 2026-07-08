# RES-1 — Crate Due Diligence

**Date:** 2026-07-08. **Author:** research agent (RES-1).

**Scope note:** No `Cargo.toml` exists in this workspace yet, so this audit examines the
*current released version* of each crate as of 2026-07-08 (exact version recorded per
crate), not pinned versions. Re-check on pin.

**Citation convention:** every load-bearing claim carries a URL and the access date
(2026-07-08). Claims that could not be confirmed against a fetched source today are
explicitly marked **unverified**.

**Verdict key:** PASS / PASS-with-caveats / CAUTION / FAIL, per criterion:
(1) maintenance pulse, (2) API stability, (3) use-case coverage, (4) blocking issues.

---

## Summary table

| Crate | Version examined | Maintenance | API stability | Coverage | Blocking issues | Verdict |
|---|---|---|---|---|---|---|
| i-overlay | 7.0.2 | PASS | CAUTION (3 majors in 2 months) | PASS (native i64) | none open | Adopt; pin major, isolate behind trait |
| cavalier_contours | 0.7.0 | PASS-with-caveats (fixes stranded on master) | PASS-with-caveats (0.x) | PASS-with-caveats (f64-only; collapse undocumented) | CAUTION: open offset panic #79 | Adopt via pinned git rev; fuzz inward offsets |
| nusb | 0.2.4 | PASS | PASS (0.2 settled ~1 yr) | PASS | Windows caveats (#168/#149, WinUSB rebind) | Adopt; Linux-first |
| opencv | 0.99.0 | PASS (bus-factor 1) | CAUTION (0.x churn, MSRV, OpenCV-5 split) | PASS (videoio+calib3d+aruco verified) | CAUTION: build complexity, no bundled build | Adopt; pin OpenCV 4.x, bake into CI image |
| usvg | 0.47.0 | PASS (stewardship mode) | PASS-with-caveats | PASS (Béziers kept; text flatten exists) | none (watch #877 precision) | Adopt |
| serialport | 4.9.0 | PASS-with-caveats (maintainers wanted) | PASS (4.x stable) | PASS (all 6 line methods verified) | DTR-on-open quirks (#292) | Adopt; init lines explicitly |
| egui/eframe | 0.35.0 | PASS | PASS-with-caveats (breaking ~2x/yr) | PASS (concave-fill caveat) | none | Adopt; Mesh/PaintCallback for pours |

---

## 1. i-overlay — booleans on integer polygons (GEO-1)

**Version examined:** 7.0.2, released 2026-06-24
(https://crates.io/api/v1/crates/i-overlay, accessed 2026-07-08).
Repo: https://github.com/iShape-Rust/iOverlay (per crates.io metadata, accessed 2026-07-08).

1. **Maintenance pulse — PASS.** Very active: last push 2026-07-05; releases 7.0.2
   (2026-06-24), 7.0.1 (2026-06-18), 7.0.0 (2026-06-01), 6.0.0 (2026-05-02), 5.0.0
   (2026-04-22), 4.5.0 (2026-03-22) — five+ releases in six months
   (https://crates.io/api/v1/crates/i-overlay;
   https://api.github.com/repos/iShape-Rust/iOverlay + /releases, accessed 2026-07-08).
   Effectively single-maintainer (Nail Sharipov) — bus-factor 1 (commits API, accessed
   2026-07-08).
2. **API stability — CAUTION.** Three major (breaking) bumps in ~2 months
   (5.0.0 → 6.0.0 → 7.0.0, Apr–Jun 2026): 6.0.0 broke `FloatPointCompatible`
   (associated `type Scalar`); 7.0.0 introduced a templated i16/i32/i64 solver and a
   new `EdgeOverlay` API; 5.0.0 notes say the project "adopted semantic versioning
   release cycles" (https://api.github.com/repos/iShape-Rust/iOverlay/releases,
   accessed 2026-07-08). The author back-publishes patches for older majors (4.5.2 and
   5.0.1 on 2026-05-18; crates.io API, accessed 2026-07-08), which softens the churn.
   Pin the major; expect breaking bumps a few times per year.
3. **Coverage — PASS.** Verified for 7.0.2: boolean union / intersection / difference /
   exclusion (xor); "Polygons: with holes, self-intersections, and multiple contours";
   native `i16`/`i32`/`i64` integer APIs (`IntPoint::new(x, y)` in README examples)
   alongside `f32`/`f64`; four fill rules (even-odd, non-zero, positive, negative);
   degenerate-vertex removal/simplification; an OGC-valid output mode (added 4.2.0)
   (https://docs.rs/i-overlay/latest/i_overlay/; https://github.com/iShape-Rust/iOverlay,
   accessed 2026-07-08). The native `i64` API matches PCBForge's integer-nanometer
   convention directly — no float round-trip. No formal exactness guarantee is stated,
   but pure-integer input makes results deterministic (README, accessed 2026-07-08).
   A thin conversion layer from PCBForge core types to `i_float`/`i_shape` points is
   needed (docs.rs, accessed 2026-07-08).
4. **Blocking issues — none open.** Only 5 open issues, none active correctness
   defects: #47 OGC-validity enhancement (Jan 2026), #34–36 test-coverage tasks, #9
   strict-booleans feature request (https://github.com/iShape-Rust/iOverlay/issues,
   accessed 2026-07-08). Two hole/self-touching correctness bugs were found and fixed
   fast in June 2026 (PRs #80 fixed 2026-06-18, #81 fixed 2026-06-24; commits API,
   accessed 2026-07-08) — evidence edge-case bugs exist in this class of code *and*
   that the maintainer fixes them quickly.

**Fallback:** **`clipper2`** (Rust bindings to Clipper2) v0.6.0, 2026-05-06 — boolean
clipping (intersection/union/difference/XOR) + offsetting on the industry-standard
integer-coordinate engine (https://crates.io/api/v1/crates/clipper2, accessed
2026-07-08). Secondary: `geo` v0.33.1 (2026-04-20) — mature but f64-based
(https://crates.io/api/v1/crates/geo, accessed 2026-07-08).

**Red flags → tasks:** breaking-major cadence + single maintainer → **GEO-1** (pin
exact major; isolate i-overlay behind PCBForge's own boolean-op trait so a major bump
or a clipper2 swap touches one module; adopt QA-1 property tests on hole/self-touch
cases like the ones PRs #80/#81 fixed).

## 2. cavalier_contours — offsets incl. collapse (GEO-1, CAM-2)

**Version examined:** 0.7.0, released 2026-01-02
(https://crates.io/api/v1/crates/cavalier_contours, accessed 2026-07-08).
Repo: https://github.com/jbuckmccready/cavalier_contours (crates.io metadata + fetched
2026-07-08).

1. **Maintenance pulse — PASS-with-caveats (slow release cadence).** Repo active (last
   push 2026-06-24, 221 stars, 18 open issues, not archived;
   https://api.github.com/repos/jbuckmccready/cavalier_contours, accessed 2026-07-08),
   but only one crates.io release in the last 6 months (0.7.0, 2026-01-02; prior 0.6.0
   2025-07-09 — roughly one/year historically; crates.io API, accessed 2026-07-08).
   **Important:** offset bug fixes landed on master through 2026-06-24 (see criterion 4)
   that are *not in any crates.io release* — a pinned git dependency is needed to get
   them.
2. **API stability — PASS-with-caveats.** Still 0.x after 5 years, ~1 minor/year.
   0.7.0 had one breaking change (collapsed-area parameter added to boolean options,
   defaults preserving behavior); 0.6.0 bumped MSRV to 1.88 / edition 2024
   (https://raw.githubusercontent.com/jbuckmccready/cavalier_contours/master/CHANGELOG.md,
   accessed 2026-07-08). Calm cadence; no API-stability declaration.
3. **Coverage — PASS-with-caveats.** Parallel offsetting on open, closed, and
   self-intersecting polylines with **true arcs** ("not approximated as line segments")
   plus boolean ops between two closed polylines (repo README, accessed 2026-07-08) —
   the only Rust crate found with arc-aware offsetting, which matters for KiCad pad/
   track geometry. Only rounded joins are supported for offsets (README, same access).
   `PlineSource::parallel_offset(&self, offset) -> Vec<OutputPolyline>` — the Vec
   return structurally supports zero results (full collapse) and multiple islands, but
   **collapse semantics are not explicitly documented** (verified signature only:
   https://docs.rs/cavalier_contours/latest/cavalier_contours/polyline/trait.PlineSource.html,
   accessed 2026-07-08; the 0.6.0 changelog notes offset-slice validation "addressing
   issues with collapsed polylines"). Numeric type: generic over a `Real` trait
   implemented **only for f32/f64 — no integer support**
   (https://docs.rs/cavalier_contours/latest/cavalier_contours/core/traits/trait.Real.html,
   accessed 2026-07-08); an nm↔f64 mapping layer is required (f64's 53-bit mantissa
   exactly represents integers to ~9e15, so nm at PCB scale is lossless — our
   arithmetic, not a fetched claim).
4. **Blocking issues — CAUTION.** Open, unanswered inward-offset bugs as of 2026-07-08:
   - **#79** (open, 2026-02-06): panic (`assertion left == right failed` in
     `pline_view.rs:353`) inside `parallel_offset(-3.0)` on a specific geometry; no
     visible maintainer response
     (https://github.com/jbuckmccready/cavalier_contours/issues/79, accessed 2026-07-08).
   - **#82** (open, 2026-03-24): negative offset of an *open* polyline returns empty
     while positive works (may not affect our closed-polygon case)
     (https://github.com/jbuckmccready/cavalier_contours/issues/82, accessed 2026-07-08).
   - Older open offset-adjacent: #44 (2024), #35 (2023). **#83** (debug panic on
     collapsed near-vertex offset slices) was fixed on master 2026-06-24 but is
     unreleased on crates.io (commits API, accessed 2026-07-08).

**Fallback:** **`clipper2`** v0.6.0 (2026-05-06) — offsetting (inflate/deflate) on the
integer-based Clipper2 engine (https://crates.io/api/v1/crates/clipper2, accessed
2026-07-08); note Clipper2 polygonizes arcs (inherent-to-Clipper2 knowledge,
**unverified** today). `geo-buffer` exists but is stale (last release 2023-06;
https://crates.io/api/v1/crates/geo-buffer, accessed 2026-07-08) — weak fallback.

**Red flags → tasks:** open panic #79 on negative (inward) offsets + undocumented
collapse semantics + fixes stranded on master → **CAM-2** (sliver force-clear depends
on trustworthy inward-collapse behavior: depend on a pinned git revision, wrap
`parallel_offset` in `catch_unwind` or pre-validate, and fuzz/property-test inward
collapse in QA-1) and **GEO-1** (nm↔f64 boundary layer).

## 3. nusb — async bulk USB for the native driver (DRV-3, DRV-4)

**Version examined:** 0.2.4, released 2026-06-21
(https://crates.io/api/v1/crates/nusb, accessed 2026-07-08).
Repo: https://github.com/kevinmehall/nusb (confirmed active, not archived, last push
2026-06-21; https://api.github.com/repos/kevinmehall/nusb, accessed 2026-07-08).

1. **Maintenance pulse — PASS.** Three releases in the last 6 months (0.2.2 2026-02-17,
   0.2.3 2026-03-10, 0.2.4 2026-06-21; crates.io API, accessed 2026-07-08). Steady
   commits Feb–Jun 2026: Windows threadpool IO rework, control-transfer timeouts,
   deadlock fix in `wait_next_complete`, Linux hotplug event-loop fix
   (https://api.github.com/repos/kevinmehall/nusb/commits, accessed 2026-07-08).
2. **API stability — PASS.** The 0.1 → 0.2 API redesign shipped 2025-07-27 after two
   betas; since then four patch releases over ~11 months with no breaking changes
   (crates.io API + https://api.github.com/repos/kevinmehall/nusb/releases, accessed
   2026-07-08). 0.2 breaking changes (for reference when reading old examples): methods
   return `impl MaybeFuture` (`.wait()` blocking or `.await`); old `Queue` API replaced
   by redesigned `Endpoint`; bulk/interrupt IN buffers must be multiples of max packet
   size (GitHub release notes, accessed 2026-07-08). No 1.0 roadmap stated — unverified
   beyond README/release notes.
3. **Coverage — PASS.** Verified in docs.rs 0.2.4 (accessed 2026-07-08):
   - Typed bulk endpoints: `Interface::endpoint::<Bulk, In>() / <Bulk, Out>()` yielding
     `Endpoint<Bulk, In/Out>` (https://docs.rs/nusb/latest/nusb/struct.Interface.html).
   - High-throughput queuing: `Endpoint::submit()` decoupled from completion
     (`next_complete()` async / `wait_next_complete(timeout)` blocking), `pending()`
     in-flight count, docs recommend multiple pending transfers for streaming; drop
     cancels; `cancel_all()` (https://docs.rs/nusb/latest/nusb/struct.Endpoint.html).
   - `reader()`/`writer()` adapters implementing `std::io::Read`/`Write` and async
     equivalents; `transfer_blocking()` one-shots (same page).
   - Control transfers: `Interface::control_in()/control_out()` (docs.rs Interface page).
   - Enumeration + hotplug: `list_devices()`, `watch_devices()` stream (docs.rs index).
   - Runtime-independent: async-first, pure Rust, no libusb; blocking via `.wait()`
     needs no runtime; `tokio`/`smol` features only for blocking-syscall offload
     (README + docs.rs index, accessed 2026-07-08).
   - Platforms: Linux usbfs (udev rule needed for the JCZ board), Windows **WinUSB** —
     the B4's EZCAD board ships bound to the vendor Lmcv2u/Lmcv4u driver, so Windows use
     requires rebinding via Zadig/INF — macOS IOKit (docs.rs index + README, accessed
     2026-07-08).
4. **Blocking issues — PASS-with-caveats.** Open issues as of 2026-07-08
   (https://api.github.com/search/issues?q=repo:kevinmehall/nusb+is:issue+is:open):
   #168 Windows control-transfer timeout fixed at 5 s (0.2.4 commits touch Windows
   control timeouts; whether #168 is resolved is unverified); #149 Windows hotplug can
   enumerate an interface empty right after plug-in; #211 HotplugWatch errors not
   surfaced; #47 no isochronous support (irrelevant — EZCAD2 traffic is bulk). No open
   issues found about Windows bulk transfers or cancellation (absence claim, based on
   the 12 open issues returned by that search).

**Fallback:** `rusb` (libusb bindings) — but its latest release is 0.9.4 from
2024-04-27, notably stale (https://crates.io/api/v1/crates/rusb, accessed 2026-07-08).

**Red flags → tasks:** Windows driver rebinding (WinUSB vs vendor driver) and #168/#149
→ **DRV-3/DRV-4**; plan Linux-first bring-up, treat Windows as a port.

## 4. opencv (twistedfall bindings) — videoio + calib3d + aruco (VIS-1/2/11)

**Version examined:** 0.99.0, released 2026-06-23
(https://crates.io/api/v1/crates/opencv, accessed 2026-07-08).
Repo: https://github.com/twistedfall/opencv-rust (accessed 2026-07-08).

1. **Maintenance pulse — PASS.** 0.99.0 on 2026-06-23, 0.98.2 2026-03-23, 0.98.1
   2026-01-02, 0.98.0 2025-12-18 — three releases in the last 6 months; last push
   2026-06-24, 2,449 stars, 37 open issues, not archived (crates.io API +
   https://api.github.com/repos/twistedfall/opencv-rust, accessed 2026-07-08).
   **Effectively single-maintainer**: twistedfall has 1,410 contributions vs 161 for
   the next human contributor (contributors API, accessed 2026-07-08) — bus-factor 1.
   Issues get closed in batches aligned to releases (e.g. #716 OpenCV-5 compat closed
   2026-06-23; issue listing, accessed 2026-07-08); median time-to-close **unverified**.
2. **API stability — CAUTION.** Perpetual 0.x with breaking minors roughly
   monthly-to-quarterly (0.99.0: `MatStep` gains a lifetime, fixed-size array args by
   value; 0.95.0: `MatSize` lifetime;
   https://raw.githubusercontent.com/twistedfall/opencv-rust/master/CHANGES.md, accessed
   2026-07-08). MSRV moves fast: 1.77 (0.94) → 1.81 (0.96) → 1.82 (0.97) → **1.88 +
   edition 2024 (0.99.0)**, rolling ~1-year policy (CHANGES.md + README, accessed
   2026-07-08). New instability axis in 0.99.0: **the Rust module layout now depends on
   which OpenCV C++ version you link** (see coverage).
3. **Coverage — PASS (pin OpenCV 4.x).** Verified against docs.rs directly
   (accessed 2026-07-08):
   - **videoio**: `VideoCapture` plus `CAP_V4L2`, `CAP_DSHOW`, `CAP_MSMF`,
     `CAP_GSTREAMER`, `CAP_ANY` (https://docs.rs/opencv/0.99.0/opencv/videoio/index.html).
   - **calib3d** (against OpenCV 4.x): `calibrate_camera`, `solve_pnp`, `undistort`,
     `find_chessboard_corners` all present (verified in the 0.98.2 docs build,
     https://docs.rs/opencv/0.98.2/opencv/calib3d/index.html). Under **OpenCV 5** the
     same functions split across new `calib` / `geometry` / `imgproc` modules (0.99.0
     docs.rs build appears to be OpenCV-5-based; `calib3d` page empty) — **pin OpenCV
     4.x** to keep the classic layout.
   - **ArUco**: `ArucoDetector`, `CharucoDetector`, `Dictionary`, `GridBoard`,
     `CharucoBoard` all in `opencv::objdetect` (matching upstream's 4.7+ move of aruco
     into objdetect); **no contrib feature needed** for ArucoDetector — 73 of 76
     features are on by default, and the separate `aruco`/`aruco_detector` features
     cover only the legacy contrib module
     (https://docs.rs/opencv/0.99.0/opencv/objdetect/index.html +
     https://docs.rs/crate/opencv/0.99.0/features, accessed 2026-07-08).
   Supported upstream: OpenCV 3.4 (deprecated), 4.x, 5.x (README + CHANGES.md, accessed
   2026-07-08). Covers VIS-1 (videoio), VIS-2 (calib3d), VIS-11 (objdetect ArUco).
4. **Blocking issues — CAUTION (build, not correctness).** **No bundled/vendored
   build**: system OpenCV (4.x/5.x) + clang/libclang required; INSTALL.md offers no
   automatic source build; static linking "supported and tested at least on Linux";
   Windows goes through Chocolatey or vcpkg + `OPENCV_LINK_LIBS`/`OPENCV_LINK_PATHS`/
   `OPENCV_INCLUDE_PATHS` env vars
   (https://github.com/twistedfall/opencv-rust/blob/master/INSTALL.md, accessed
   2026-07-08). Recent open issues are dominated by build/env problems (#708 "Could not
   detect OpenCV version" with 4.13 on Windows 11, open since 2026-03-01; #690 CUDA
   header; #694 macOS static deps) — **none in the sampled set mention videoio,
   calib3d, objdetect, aruco, segfaults, or unsoundness** (issues API sample of 30,
   accessed 2026-07-08). No RustSec advisory found (rustsec.org/packages/opencv.html
   404s; accessed 2026-07-08); full soundness audit of generated bindings —
   **unverified**.

**Fallback:** split by function — **`nokhwa`** 0.10.11 (2026-05-15) for camera capture
(V4L2/AVFoundation/MSMF; https://crates.io/api/v1/crates/nokhwa, accessed 2026-07-08);
**`apriltag`** 0.4.0 for fiducials (last release 2023-01-28 — stale but the C library
underneath is the de-facto standard; https://crates.io/api/v1/crates/apriltag, accessed
2026-07-08; pairs with VIS-11's AprilTag pallet ID); `kornia` 0.1.10-rc.3 is pre-1.0
and immature for calib (https://crates.io/api/v1/crates/kornia, accessed 2026-07-08);
hand-rolled Zhang calibration over `imageproc` 0.27.0 (2026-06-02) as last resort.

**Red flags → tasks:** build complexity + Windows detection issues + bus-factor →
**VIS-1/VIS-2/VIS-11** and CI (INF-2: bake OpenCV into the CI image); OpenCV-5 module
split → pin OpenCV 4.x in docs and CI; ArUco-in-objdetect confirmed, so VIS-11 is
unblocked at the binding level.

## 5. usvg — SVG ingest/flatten (ING-1)

**Version examined:** 0.47.0, released 2026-02-09
(https://crates.io/api/v1/crates/usvg, accessed 2026-07-08).
Repo: https://github.com/linebender/resvg — the resvg project now lives under the
Linebender org (crates.io repository metadata, accessed 2026-07-08).

1. **Maintenance pulse — PASS (stewardship mode).** 0.47.0 on 2026-02-09 and 0.46.0 on
   2026-01-11 — two releases in the last 6 months (crates.io API, accessed 2026-07-08).
   Maintenance-risk history: original author RazrFalcon announced he could no longer
   maintain the project on 2024-10-14 ("Transferring ownership",
   https://github.com/linebender/resvg/issues/834, accessed 2026-07-08); Linebender
   took stewardship (blog post 2024-11-04) with the explicit framing "Our role is a
   stewardship role… If things work for you now, they will continue to work for you
   going forward" (https://linebender.org/blog/tmix-10/, accessed 2026-07-08). Repo is
   active, not archived: 3.9k stars, 136 open issues
   (https://github.com/linebender/resvg, accessed 2026-07-08). Expect stability, not
   fast feature work.
2. **API stability — PASS-with-caveats.** 0.x with breaking minors ~2-4/year under
   Linebender: 0.45.0 relicensed MPL-2.0 → Apache-2.0 OR MIT; 0.46.0 bumped MSRV to
   1.87 / edition 2024 and tiny-skia 0.11 → 0.12 (breaking — tiny-skia-path types leak
   into the public API); 0.47.0 additive. Well-kept CHANGELOG
   (https://raw.githubusercontent.com/linebender/resvg/main/CHANGELOG.md, accessed
   2026-07-08).
3. **Coverage — PASS.** Verified in docs.rs 0.47.0 (accessed 2026-07-08):
   - Fully resolved simplified tree: "all the elements, attributes, references and
     other SVG features are already resolved and presented in the simplest way
     possible"; basic shapes become paths
     (https://docs.rs/usvg/latest/usvg/).
   - Béziers preserved for our own nm flattening: "Paths contain only absolute MoveTo,
     LineTo, QuadTo, CurveTo and ClosePath segments. ArcTo, implicit and relative
     segments will be converted" (same page).
   - Transforms: `usvg::Path::abs_transform()` gives the element's absolute transform
     including all ancestors (https://docs.rs/usvg/latest/usvg/struct.Path.html,
     accessed 2026-07-08). **Nuance:** `data()` returns path geometry in local
     coordinates — "absolute" refers to segment commands, not baked transforms;
     verified against resvg's own renderer, which applies the accumulated transform at
     fill time (crates/resvg/src/render.rs + src/path.rs on main, accessed 2026-07-08).
     **ING-1 must apply `abs_transform()` to `data()` itself.**
   - Text-to-paths available: `usvg::Text::flattened()` → "Text converted into paths,
     ready to render" (https://docs.rs/usvg/latest/usvg/struct.Text.html, accessed
     2026-07-08). Whether KiCad's SVG plot even emits `<text>` (it reportedly strokes
     its font as paths) is **unverified** — if it does, fontdb must be loaded or text
     silently disappears (issue #515, seen via search 2026-07-08).
   - Prior art: gerbolyze (SVG-to-Gerber PCB tool) uses usvg in production for the same
     kind of pipeline (https://github.com/jaseg/gerbolyze, seen via search 2026-07-08).
4. **Blocking issues — none found.** One watch item: #877 reports small geometric
   imprecision when round-tripping/simplifying SVG through usvg
   (https://github.com/linebender/resvg/issues/877, seen via search 2026-07-08) —
   verify at fab tolerances in ING-1's golden tests. No KiCad-specific issues found.

**Fallback:** `svgtypes` 0.16.1 (2026-01-09, also under Linebender) + `kurbo` 0.13.1
(2026-05-13) for a narrow custom KiCad-SVG parser (KiCad output is simple)
(https://crates.io/api/v1/crates/svgtypes and /kurbo, accessed 2026-07-08). `lyon_svg`
is dead (last release 2021-06-22; https://crates.io/api/v1/crates/lyon_svg, accessed
2026-07-08) — not a candidate.

**Red flags → tasks:** local-coords + `abs_transform()` nuance and text/fontdb handling
→ **ING-1** (apply transforms explicitly; decide legend-text strategy early; add a
precision golden test per #877).

## 6. serialport — modem control lines for the interlock (ORC-4)

**Version examined:** 4.9.0, released 2026-03-16
(https://crates.io/api/v1/crates/serialport, accessed 2026-07-08; docs.rs shows a
2026-07-01 rebuild date — crates.io `created_at` is authoritative).
Repo: https://github.com/serialport/serialport-rs (last push 2026-07-04, commits within
the last week; https://api.github.com/repos/serialport/serialport-rs, accessed 2026-07-08).

1. **Maintenance pulse — PASS-with-caveats.** Active (commits days ago; 4.9.0 in March,
   4.8.x Oct 2025), but the README actively solicits maintainers, "particularly for
   Windows support" — community-maintained with thin Windows coverage
   (https://raw.githubusercontent.com/serialport/serialport-rs/main/README.md, accessed
   2026-07-08). Note 4.7.0–4.7.2 are yanked on crates.io (reason unverified; plausibly
   the DTR-on-open revert, see issue #292 below).
2. **API stability — PASS.** 4.x line since ~2021, regular minor releases (4.6 → 4.9
   over ~19 months), blocking-I/O trait design stable, MSRV 1.59 (crates.io API +
   README, accessed 2026-07-08).
3. **Coverage — PASS.** All six needed methods verified on the `SerialPort` trait in
   4.9.0 docs (https://docs.rs/serialport/4.9.0/serialport/trait.SerialPort.html,
   accessed 2026-07-08): `read_clear_to_send`, `read_data_set_ready`,
   `read_carrier_detect`, `read_ring_indicator` (all `-> Result<bool>`), plus
   `write_request_to_send(level)` and `write_data_terminal_ready(level)`. **Polling
   only** — no event/blocking-wait API for line transitions; ORC-4 will poll (fine for
   an airflow interlock) or drop to `TIOCMIWAIT` ioctl on Linux for edge-triggering.
4. **Blocking issues — PASS-with-caveats.** #292 (open, updated 2026-03-23): the
   attempt to auto-assert DTR on open broke Linux USB-gadget ports and Windows/Arduino
   comms and was reverted; DTR-on-open behavior is platform-inconsistent — **ORC-4 must
   explicitly set RTS/DTR after open, never rely on open-state defaults**
   (GitHub issue search, accessed 2026-07-08). No open bugs found against the four read
   methods (absence claim from one search query).

**Fallback:** `serial2` 0.2.37 (2026-05-11, de-vri-es/serial2-rs) — verified to expose
`read_cts/read_dsr/read_cd/read_ri` and `set_rts/set_dtr`
(https://docs.rs/serial2/latest/serial2/struct.SerialPort.html, accessed 2026-07-08).

**Red flags → tasks:** DTR/RTS-on-open inconsistency (#292) + poll-only lines →
**ORC-4** (explicit line init, poll loop with debounce, treat read errors as
interlock-open/fail-safe).

## 7. egui / eframe — operator console (UI-1)

**Version examined:** egui 0.35.0 and eframe 0.35.0, both released 2026-06-25
(https://crates.io/api/v1/crates/egui and /eframe, accessed 2026-07-08).
Repo: https://github.com/emilk/egui.

1. **Maintenance pulse — PASS.** Five releases in ~4 months: 0.35.0 (2026-06-25),
   0.34.3 (2026-05-27), 0.34.2 (2026-05-04), 0.34.1/0.34.0 (2026-03-26/27)
   (crates.io API, accessed 2026-07-08). 29.6k stars; development sponsored by Rerun
   (https://github.com/emilk/egui README, accessed 2026-07-08).
2. **API stability — PASS-with-caveats.** Still 0.x; README says interfaces are "still
   in flux". 0.34.0 (2026-03) was a large breaking release: font stack `ab_glyph` →
   `skrifa + vello_cpu`; `Context::run` → `run_ui`; eframe `App::update` deprecated for
   `App::ui`; `SidePanel`/`TopBottomPanel` unified into `Panel`
   (https://raw.githubusercontent.com/emilk/egui/main/CHANGELOG.md +
   https://github.com/emilk/egui/releases, accessed 2026-07-08). Expect a meaningful
   migration ~2x/year, with good changelog notes.
3. **Coverage — PASS-with-one-sharp-edge.** Verified (accessed 2026-07-08):
   - Live camera view: `TextureHandle::set()` ("Assign a new image to an existing
     texture") and `set_partial()` for subregion updates — the standard per-frame video
     pattern (https://docs.rs/egui/latest/egui/struct.TextureHandle.html).
   - Heavy scenes: `egui::PaintCallback` with `egui_glow::CallbackFn` /
     `egui_wgpu::Callback` for backend-specific GPU drawing inside a region — the
     escape hatch for the galvo-scale toolpath preview
     (https://docs.rs/egui/latest/egui/struct.PaintCallback.html).
   - **Sharp edge:** `epaint::PathShape` fill docs state "Fill is only supported for
     convex polygons" (https://docs.rs/epaint/latest/epaint/struct.PathShape.html,
     accessed 2026-07-08). Copper pours and board outlines are concave — filled
     `Shape::Path` will render them wrong. Mitigations: pre-triangulate to
     `Shape::Mesh` (e.g. via lyon, or by reusing our own geometry kernel's
     tessellation) or use a PaintCallback.
   - Many-shapes perf: CPU tessellation is the documented bottleneck; an
     `epaint`/`egui` `rayon` feature parallelizes tessellation of large shapes; #1196
     is the optimization tracking issue (https://github.com/emilk/egui/issues/1196 and
     /1485, seen via search 2026-07-08). Net: thousands of shapes/frame feasible;
     tens of thousands wants Mesh caching or a callback (ceiling numbers are judgment,
     **unverified** benchmark).
   - Backends: `Glow` and `Wgpu` renderers both shipped and selectable in eframe 0.35.0
     (https://docs.rs/eframe/latest/eframe/enum.Renderer.html, accessed 2026-07-08);
     wgpu surface-lifecycle "random hangs" fixes landed in 0.34.3 (releases page,
     accessed 2026-07-08) — stay ≥0.34.3 for a long-running console.
4. **Blocking issues — none found.** Convex-fill limitation is by design (handled
   above); nothing else blocking surfaced.

**Fallback:** `iced` 0.14.0 (2025-12-07) or Slint 1.17.1 (2026-07-07; GPL-3.0 or
commercial license — a consideration) (https://crates.io/api/v1/crates/iced and /slint,
accessed 2026-07-08). `imgui-rs` last released 2024-05-05 — stale, weakest option
(https://crates.io/api/v1/crates/imgui, accessed 2026-07-08).

**Red flags → tasks:** concave-fill limitation + tessellation load + breaking-minor
cadence → **UI-1** (render pours as Mesh or via PaintCallback from the start; pin
minor version) and **UI-2** (AR overlay compositing over camera textures).

---

## Red-flag → task-ID index

| Red flag | Task(s) affected | Action |
|---|---|---|
| i-overlay breaking-major cadence + single maintainer | GEO-1 | Pin major; wrap behind PCBForge boolean trait; QA-1 property tests on hole/self-touch cases |
| cavalier_contours open inward-offset panic #79; collapse semantics undocumented; Mar–Jun 2026 offset fixes unreleased | CAM-2, GEO-1 | Depend on pinned git rev; fuzz/property-test inward collapse; guard against panics |
| cavalier_contours f64-only (no integer coords) | GEO-1 | nm↔f64 boundary layer (lossless at PCB scale) |
| nusb Windows: WinUSB rebind required for EZCAD board; open #168 (control timeout), #149 (hotplug enumeration) | DRV-3, DRV-4 | Linux-first bring-up; udev rule; Zadig/INF documented for Windows |
| opencv: no bundled build (system OpenCV + clang), Windows detection issues (#708), bus-factor 1 | VIS-1, VIS-2, VIS-11, INF-2 | Bake OpenCV 4.x into CI image; document install; isolate behind capture/calib traits |
| opencv 0.99.0: OpenCV-5 module split (calib3d → calib/geometry/imgproc) | VIS-2 | Pin OpenCV 4.x |
| usvg: geometry is local coords — must apply `abs_transform()`; text needs `flattened()` + fontdb; #877 precision | ING-1 | Apply transforms explicitly; decide legend-text strategy; precision golden test |
| serialport: DTR-on-open platform-inconsistent (#292); modem lines poll-only | ORC-4 | Set RTS/DTR explicitly after open; poll with debounce; fail-safe on read error |
| egui: `PathShape` fill convex-only; CPU tessellation ceiling; breaking minors ~2x/yr | UI-1, UI-2 | Render pours via `Shape::Mesh`/PaintCallback; pin minor; stay ≥0.34.3 for wgpu hang fixes |

## Method / verification depth

- crates.io version data for all seven crates (plus fallbacks `clipper2`, `geo`,
  `geo-buffer`, `rusb`, `serial2`, `tokio-serial`, `nokhwa`, `kornia`, `apriltag`,
  `imageproc`, `svgtypes`, `kurbo`, `lyon_svg`, `iced`, `slint`, `imgui`) fetched
  directly from the crates.io API on 2026-07-08.
- docs.rs API verification done directly for: serialport `SerialPort` trait, nusb
  `Interface`/`Endpoint`, usvg `Tree`/`Path`/`Text`, opencv videoio/calib3d/objdetect
  module listings + feature flags, i-overlay module docs, cavalier_contours
  `PlineSource::parallel_offset` + `Real` trait, egui `TextureHandle`/`PaintCallback`/
  `PathShape`, eframe `Renderer`.
- GitHub verification (repos API, commits, releases, issue search, README/CHANGELOG
  raw fetches) for all seven upstream repos on 2026-07-08.
- Research was parallelized across four verification agents plus direct spot-checks;
  every load-bearing claim above carries its own citation. Claims marked
  **unverified** were not confirmable against a fetched source today and must not be
  treated as established.
- Absence-of-issue claims are bounded by the specific searches run (noted inline) —
  they are "not found", not "does not exist".
