# ComMarker Omni X — machine research (corrects the "B4 fiber MOPA" assumption)

**Why this exists:** the operator confirmed (2026-07-14) the production laser is
the **ComMarker Omni X**, a **355 nm UV galvo** engraver — *not* the "ComMarker
B4" fiber MOPA the backlog and RES-4 were written around. This memo records what
the Omni X actually is, from cited sources, and what it changes.

**Compiled:** 2026-07-14. URLs accessed 2026-07-14. Vendor marketing numbers are
flagged as such; treat spot-size / accuracy / max-speed as claims until measured.

---

## 1. What it is (specs)

| Property | Value | Note |
|----------|-------|------|
| Laser | **355 nm UV**, DPSS (frequency-tripled), air-cooled | not fiber, not 1064 nm |
| Power | **5 W** standard; **6 W / 10–12 W** variants cited (sources disagree — confirm the operator's unit) | |
| Working area | **150 × 150 mm** (150 mm lens); **70 × 70 mm** with the 3D/glass lens | two interchangeable lenses shipped |
| Slide extension | to **150 × 400 mm** (6 W) / **250 × 400 mm** (12 W) — "~2.5×" | matches CAM-9's "slide extension" tiling task |
| Spot size | ~**0.0019 mm** (1.9 µm) *(vendor claim)* | verify empirically |
| XY accuracy | **0.001 mm** *(vendor claim)* | |
| Max speed | **10,000 mm/s** *(vendor claim)* | |
| Focus | LiDAR / distance-sensor **autofocus**, + manual thumbscrew | no test-burn focus needed |

## 2. Controller & software — still JCZ, but "Seacad/UV" not fiber

- The Omni X uses a **JCZ (Beijing JCZ Technology) galvo controller** over the
  **XY2-100** galvo scanner protocol, loading `.cor` correction + `markcfg7`
  config files — i.e. the **EZCAD / BJJCZ family**, the *same controller lineage
  as the B4 fiber*. This is the single most important finding: **the DRV USB
  reverse-engineering method still applies** (Balor / MeerK40t galvo-driver prior
  art targets exactly this JCZ family).
- Two software paths:
  - **ComMarker Studio** (native) — required for 3D internal-glass engraving;
    drivers install from the included USB stick.
  - **LightBurn** as the **"Seacad" device class** — surface engraving only
    (no 3D). **Requires LightBurn ≥ 1.3.01.** Connects over **USB**.
- LightBurn device settings the operator uses (from ComMarker's setup guide):
  laser type **UV**, **"Galvo 2" enabled as X axis**, door-protect port **1**,
  **frequency 20–200 kHz**, **pulse 1–50 ns**.

## 3. Why UV is *good* for this project (and how it differs from fiber)

355 nm UV is well-suited to PCB work, arguably better than the fiber MOPA the
plan assumed:

- **High absorption in both copper and FR4 resin** at 355 nm.
- **Cold ablation**: UV photons break molecular bonds directly rather than
  melting/burning, giving a **small heat-affected zone, minimal charring, and
  low mechanical/thermal stress** — this substantially *defuses* the char /
  HAZ problem that dominated the fiber-era RES-4 survey and the operator's
  earlier HAZ question.
- Documented **< 50 µm** cut tolerances on FR4/copper with 355 nm.
- **Thick copper (> 2 oz) needs pulse-width tuning** to vaporize copper without
  delaminating the FR4 underneath — consistent with the operator's observation
  that pulse width is a fluence knob, and with the UV laser's 1–50 ns range.

Caveat: the cited depaneling systems are typically higher-power industrial
units; a **5–12 W** UV galvo's throughput and max copper thickness for clean
isolation are **unknown and must be found empirically** (this is exactly what
the VIS-9 ladder wizard is for). Don't assume the industrial tolerances at this
power.

## 4. What this changes in the repo

1. **RES-4 (ablation benchmarks) does not transfer.** It surveyed 1064 nm fiber
   copper isolation and char mitigation. UV cold ablation is a different
   mechanism; the two-stage char-cleanup recipe it recommended is largely moot.
   A UV-specific recipe must come from the operator's own ladder (VIS-9), not
   from the fiber literature. RES-4 should be re-headed as fiber-only / archival.
2. **Emit defaults are still plausible but need a UV re-check.** The current
   `pcbforge emit` defaults (30 kHz, 1–5 ns pulse) sit inside the UV ranges
   (20–200 kHz, 1–50 ns). The "power fixed at 20 %" / MOPA-fluence framing was
   derived on the `BSLFiber` device and should be re-derived on the Seacad/UV
   profile — see the open discrepancy below.
3. **The DRV track retargets from B4 → Omni X but keeps its method.** Because
   the Omni X is JCZ/XY2-100, the DRV-1 capture kit is reusable **as-is** (it is
   device-agnostic usbmon capture — just point it at the Omni X). The decode
   target is the Omni X's JCZ traffic under the **Seacad/UV** LightBurn profile.
   **DRV-7 is no longer a separate "is a Seacad driver even tractable?" stretch
   task** — this research answers that (yes, it's JCZ) — so DRV-7 largely folds
   into the main DRV-1..6 track. The B4/"jcz-protocol" naming should read
   "Omni X" going forward.
4. **CAM-9 slide-extension tiling is validated** against a real product feature
   (150→400 mm slide), not a hypothetical.

## 5. The `BSLFiber` device name (resolved)

The operator confirmed (2026-07-14): the machine **is** the 355 nm UV Omni X,
and the LightBurn device is simply **named `BSLFiber`** — a free-text label on a
UV machine, not an indication of a fiber source. Consequences:

- **Keep `DEFAULT_DEVICE = "BSLFiber"`** in the emitter — the root `DeviceName`
  in a `.lbrn2` must match the LightBurn device *name*, which is exactly this.
- The **UV facts in §1–§4 are load-bearing**; the fiber/RES-4 model does **not**
  apply. Ignore the fiber connotation of the label.
- Earlier-session MOPA-fluence language (power greyed at 20 %, pulse width as a
  fluence knob) still describes the operator's real behavior — it just belongs
  to the UV laser's 1–50 ns pulse / 20–200 kHz regime, not a 1064 nm MOPA.

---

## Sources (accessed 2026-07-14)

- ComMarker — Omni X UV Laser Engraver (product): https://store.commarker.com/products/omni-x-uv-laser-engraver
- ComMarker Support — Operating Omni X with LightBurn (Seacad device, UV laser type, Galvo 2 X-axis, 20–200 kHz, 1–50 ns, LightBurn ≥ 1.3.01): https://support.commarker.com/hc/en-us/articles/49297912484121-Operating-Omni-X-with-LightBurn
- ComMarker Blog — Omni X next-gen UV engraver overview: https://blog.commarker.com/archives/54800
- Hobby Laser Cutters — Omni X review (355 nm UV, lenses, autofocus, 10,000 mm/s): https://hobbylasercutters.com/commarker-omni-x/
- JCZ / EZCAD — LMCV4 galvo controllers, XY2-100 (controller family): https://www.ezcad.com/product/laser-controller/
- Full Spectrum Laser — UV PCB depaneling (cold ablation, HAZ): https://fslaser.com/blog/laser-pcb-depaneling-uv
- Han's Laser — UV vs CO2 for PCB depaneling: http://www.hanslaserus.com/knowledge/comparing-uv-and-co2-lasers-for-pcb-depaneling/
- ScienceDirect — 355 nm DPSS UV laser cutting of FR4 and BT/epoxy PCB substrates: https://www.sciencedirect.com/science/article/abs/pii/S0143816607002114
- Laser Focus World — PCB processing with UV lasers: https://www.laserfocusworld.com/industrial-laser-solutions/article/14215774/printed-circuit-board-processing-with-uv-lasers
