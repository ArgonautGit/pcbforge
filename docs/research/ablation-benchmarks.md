# RES-4: Ablation Benchmarks Survey — Fiber-Laser Copper Isolation on FR4 + UV Finishing

> **⚠️ SUPERSEDED on the machine assumption — see
> [`commarker-omni-x.md`](commarker-omni-x.md).**
>
> This survey was written around a **~20 W ComMarker B4-class fiber MOPA at
> 1064 nm**. On 2026-07-14 the operator confirmed the production laser is a
> **ComMarker Omni X, 355 nm UV galvo** — a different wavelength and a different
> ablation mechanism. **Do not lift the process parameters below (passes,
> fluence, PRESET-F0) as current.** The trace/space floor plan and the
> char-mitigation *reasoning* still read usefully; the numbers do not transfer.

**Task:** Survey documented (≤ 24 months preferred) results for fiber-laser copper isolation on FR4 and UV finishing quality: minimum trace/space, passes for 1 oz clearance, char mitigation. Check against the 8/8 → 6/6 → 4/4 mil floor plan and PRESET-F0 (~20 W ComMarker B4-class MOPA, 1 oz copper).
**Date compiled:** 2026-07-08. All URLs accessed 2026-07-08.

---

## 1. Verdict (summary)

**Nothing found contradicts the 8/8 → 6/6 → 4/4 floor plan; if anything, the plan is conservative and well supported.** Direct fiber ablation of 1 oz copper on FR4 at 0.1 mm (~4 mil) trace/space has existence proofs going back to 2021 on 20 W hardware, and recent (2025–2026) MOPA results confirm clean direct isolation is achievable — but the same sources report exactly the failure modes (uneven clearance, burnt resin between tracks, residual copper) that justify starting at 8/8 and earning the way down. Two independent recent reports show **non-MOPA fiber sources failing where MOPA succeeds**, which validates the B4-class MOPA hardware choice.

**On PRESET-F0:** no source suggests the plan's premise is wrong, but the strongest cross-source pattern is a **two-stage recipe**: a main clearance pass block (high power, 20–40 kHz, fine hatch, cross-hatched) followed by a **low-power / high-frequency cleanup pass** (20–40 % power, 60–100 kHz) to remove char without further FR4 damage. If PRESET-F0 is a single-stage preset, that is the one concrete improvement this survey suggests. Candidate starting points are in §4.

**Caution honored per task:** no single anecdote below is treated as authoritative; convergent patterns across ≥ 2 independent sources are flagged as such.

---

## 2. Recent sources (within ~24 months)

### 2.1 Direct copper ablation on FR4/FR1

**S1. LightBurn forum — "PCB etching on a MOPA after Drilling/Milling with MillMage"** — *forum-anecdote*
Maker.Josh, posted 2026-01-07. https://forum.lightburnsoftware.com/t/pcb-etching-on-a-mopa-after-drilling-milling-with-millmage/186589 (accessed 2026-07-08)
- 50 W JPT MOPA galvo. Fill (clearance) layer: **750 mm/s, 100 % power, 25 kHz, 0.025 mm line interval, 2 passes, 45° scan angle with 90° increment per pass**. Line layer same power/speed/frequency, 2 passes.
- Char note: "single pass will leave a charred look, second pass cleans up the area."
- The same job **failed to produce consistent results on a 50 W non-MOPA (GWeike BSL) fiber** — attributed to MOPA pulse control.
- Focus accuracy called out as critical for copper.

**S2. LightBurn forum — "PCB etching on a fiber laser"** — *forum-anecdote*
Aaron.F, initial post 2025-02-05 (thread activity into 2026-02). https://forum.lightburnsoftware.com/t/pcb-etching-on-a-fiber-laser/164508 (accessed 2026-07-08)
- On a standard (non-MOPA) fiber source, the author **could not find any setting that ablated copper without burning the FR4**; copper reflectivity/heat cited.
- Workaround that succeeded: laser removes a resist/UV-protective coating, then **sodium persulfate chemical etch** — clean results, no FR4 char at all.
- Thread also warns about toxic fumes from FR4 epoxy (ventilation mandatory).

**S3. Elektor Magazine — "Fiber Laser PCB Prototyping — Early Experiments with Stephen Hawes"** — *blog (covering a video)*
2025-01-05. https://www.elektormagazine.com/news/prototype-pcbs-faster-with-fiber-laser (accessed 2026-07-08)
- xTool F1 Ultra (20 W fiber galvo). Full workflow: copper ablation, drilling, solder mask, board cutout; functional single-sided board in ~25 min.
- Uses **FR1** explicitly as "a less abrasive and easier-to-process alternative to the more common FR4." Double-sided alignment noted as unsolved.

**S4. Hawes fiber-laser-pcb-fab repository (settings file)** — *project page*
https://github.com/sphawes/fiber-laser-pcb-fab — settings.json at https://raw.githubusercontent.com/sphawes/fiber-laser-pcb-fab/main/settings.json (accessed 2026-07-08; repo has no visible date but accompanies the Jan 2025 video, S3)
- "Traces" (copper clearance) profile for the 20 W F1 Ultra: **100 % power, speed 600, 30 kHz, 10 passes**, fill vector engraving, one-way scan, density 240 / 500 DPI.
- Solder-mask removal profile: 40 % power, speed 800, 30 kHz, 6 passes.
- This is the closest published analogue to PRESET-F0 (20 W-class galvo fiber, but on FR1 not FR4).

**S5. ComMarker blog — "Can a Fiber Laser Engrave PCBs? A Practical Guide"** — *blog (vendor)*
2026-01-29. https://blog.commarker.com/archives/56093 (accessed 2026-07-08)
- Machine: ComMarker **B6 MOPA 60 W** (vendor states a 20 W fiber can achieve similar results "by lowering the speed and increasing the number of passes").
- Main PCB pass: **1000 mm/s, 100 %, 40 kHz, 200 ns Q-pulse, 0.05 mm line spacing, 45° scan angle, 3 passes, bi-directional fill + cross-hatch**.
- **Cleanup pass: 1500 mm/s, 20 %, 100 kHz, 200 ns, 0.01 mm line spacing, 0°, 2 passes** — explicitly for removing residue/char; "if the surface feels rough after this step, a second cleaning pass may be necessary."

**S6. ComMarker SA material settings page** — *datasheet (vendor settings table)*
Undated page. https://www.commarkersa.co.za/material-settings/ (accessed 2026-07-08)
- PCB on **30 W fiber**: "Power 80 %, Speed 1000 mm/s, Frequency 30 kHz, Pulse width 200 ns, Line interval 0.01 mm, Passes 8"; cleanup pass 1500 mm/s, 20 %, 0.01 mm, 100 kHz, 2 passes.
- No 20 W PCB row published; vendor disclaims result variation by material/source.
- *Unverified sighting:* search-index snippets of ComMarker material-settings pages also describe a 20 W EZCAD2 PCB recipe of "~33 passes cross-hatch 45°, 666 mm/s, 88–90 %, 20 kHz, 0.04 line space + 4 cleanup passes at 1000 speed, 40 %, 60 kHz, 0.1 line space." I could not confirm this on a live page (the old blog URL now redirects to https://commarker.com/download-center); treat as **unconfirmed vendor guidance**, but note it is directionally consistent with S4/S6 (20 W needs ~3–4× the passes of 60 W).

**S7. Hackaday — "Using a Fiber Laser to Etch 0.1 mm PCB Traces"** — *blog (covering a video)*
2026-03-24. https://hackaday.com/2026/03/24/using-a-fiber-laser-to-etch-0-1-mm-pcb-traces/ — video: https://www.youtube.com/watch?v=LNJU9sC_sDA (Giangix) (accessed 2026-07-08)
- 20 W fiber laser, but **hybrid process**: laser patterns resist, then HCl + H2O2 chemical etch (2 mL water : 2 mL 30 % HCl : 2 drops 35 % H2O2, ~90 s).
- **0.1 mm (≈4 mil) traces achieved; clearance had to be opened to "a hair above 0.1 mm"** for the etch step to clear reliably.
- Relevance: at the 4/4 end of the roadmap, the hybrid route is what hobbyists actually use to get reliability from 20 W hardware — a useful fallback datum, not a contradiction of direct ablation.

**S8. LaserChina — "Precision Laser Cutting Revolutionizes PCB Production"** — *blog (vendor, 2024)*
https://www.laserchina.com/blog/precision-laser-cutting-revolutionizes-pcb-production-processes/ (accessed 2026-07-08)
- Claims nanosecond-pulse laser processing of FR4 with copper shows "minimal HAZ and insignificant carbonization or debris" in cut cross-sections. Vendor marketing — corroborates the ns-MOPA choice only weakly.

### 2.2 UV (355 nm) finishing quality

Recent (≤ 24 mo) *community* data on UV finishing of copper trace edges specifically is thin; what exists is mostly vendor material plus one older academic paper (see §3). Key recent-ish points:

**S9. Full Spectrum Laser — "Laser PCB Depaneling with a UV Laser"** — *blog (vendor, undated)*
https://fslaser.com/blog/laser-pcb-depaneling-uv (accessed 2026-07-08)
- 355 nm "cold" ablation on FR4: small HAZ, minimized charring vs IR/CO2; copper absorbs 355 nm far better than 1064 nm; focused spot ~20 µm; working power < 5 W class.

**S10. Sculpfun V5 UV product page (5 W 355 nm hobby galvo)** — *datasheet (vendor)*
https://eu.sculpfun.com/products/v5-uv-laser (accessed 2026-07-08)
- Confirms hobby-class 5 W UV galvos market PCB marking/engraving capability.
- **Warranty explicitly excludes damage from prolonged processing of reflective metals (copper, brass, gold, silver)** due to back-reflection into the optics. Practical implication for PCBForge: UV finishing passes on bare copper should be brief/edge-only, and back-reflection risk should be tracked as an equipment-lifetime issue.

---

## 3. Older sources (outside 24-month window — freshness deviation noted)

Included because recent community sources rarely publish quantified minimum trace/space for *direct* ablation; these older items remain the most-cited quantitative baselines. Dates are shown prominently.

**S11. Kurokesu (Saulius Lukse) — "Making fine pitch PCB prototypes with fiber laser"** — *blog* — **2021-01-07** (5.5 years old)
https://www.kurokesu.com/main/2021/01/07/making-fine-pitch-pcb-prototypes-with-fiber-laser/ — Hackaday coverage 2021-01-11: https://hackaday.com/2021/01/11/laser-blasts-out-high-quality-pcbs/ (accessed 2026-07-08)
- **20 W fiber laser, 1064 nm; ~20 hatched passes to clear 0.035 mm (1 oz) copper on FR4**; power ramped 50 % → 90 %.
- **0.1 mm (≈4 mil) track and clearance demonstrated**; 0402 parts soldered.
- Failure modes recorded: "FR-4 material burns and smells… danger of having burnt resin between tracks"; isolation uneven — "some areas can be burnt through, some will have remaining copper." Mitigation: ventilation; light finishing with 400-grit sandpaper.
- This is the canonical "20 passes for 1 oz on 20 W" datum and matches the pass counts implied by S4 (10 passes at higher density) and the unverified 20 W ComMarker snippet (~33 passes at lower frequency).

**S12. Fibercuit (UIST '22 academic paper)** — *paper* — **2022** (~4 years old)
https://arxiv.org/abs/2208.08502 (accessed 2026-07-08)
- Fiber galvo engraver producing "high-resolution, fine-pitch" circuits, but on **copper sheet / flexible & kirigami substrates, not copper-clad FR4** — its resolution figures (search-indexed as ~8 mil trace at ~100 % repeatability, ~4 mil clearance reliable, 4 mil/2 mil demonstrated) are **not directly transferable** to FR4 isolation routing. Cited for the repeatability-vs-record distinction, which maps well onto the 8/8-floor-then-progress plan.

**S13. "355 nm UV laser patterning and post-processing of FR4 PCB for fine pitch components" (Optics & Lasers in Engineering)** — *paper* — **2017** (~9 years old; abstract page returned HTTP 403 on 2026-07-08, so details are from the title/index only)
https://www.sciencedirect.com/science/article/abs/pii/S0143816617306772
- Academic precedent that 355 nm patterning of copper-clad FR4 for fine-pitch work is established practice; supports the UV-finishing leg of the architecture in principle. No parameter values extractable.

---

## 4. Candidate parameter starting points for PRESET-F0 (~20 W MOPA, 1 oz Cu, clearance fill)

PRESET-F0's exact values are not yet defined in-repo, so per task these are **candidate starting points with sources**, not a comparison:

| # | Source (laser) | Power | Speed | Freq | Pulse | Interval | Passes | Notes |
|---|---|---|---|---|---|---|---|---|
| C1 | S4 Hawes, 20 W xTool F1 Ultra | 100 % | 600 (xTool units, ≈mm/s) | 30 kHz | n/s | ~0.05 mm (240 density/500 DPI) | 10 | FR1, not FR4; proven end-to-end workflow |
| C2 | S11 Kurokesu, 20 W fiber (2021) | 50→90 % | n/s | n/s | n/s | hatch n/s | ~20 | 1 oz on FR4; 4/4 demonstrated but uneven |
| C3 | S5 ComMarker B6 60 W MOPA, scale down | 100 % | 1000 mm/s | 40 kHz | 200 ns | 0.05 mm | 3 @ 60 W → expect roughly 3–4× at 20 W | 45° cross-hatch; vendor says 20 W = slower + more passes |
| C4 | S6 ComMarker 30 W | 80 % | 1000 mm/s | 30 kHz | 200 ns | 0.01 mm | 8 | vendor table |
| C5 | S1 50 W JPT MOPA, scale down | 100 % | 750 mm/s | 25 kHz | n/s | 0.025 mm | 2 @ 50 W → more at 20 W | 45° + 90° rotation per pass |
| — | Cleanup pass (S5/S6 convergent) | **20–40 %** | **1000–1500 mm/s** | **60–100 kHz** | 200 ns | 0.01–0.1 mm | **2–4** | run after main block; repeat if surface rough |

**Synthesis for a 20 W MOPA on 1 oz FR4:** main block ≈ **100 % power, 600–800 mm/s, 20–30 kHz, ~200 ns, 0.02–0.05 mm interval, cross-hatched with per-pass angle rotation, budget 10–25 passes**; then a **cleanup block at 20–40 % / 60–100 kHz / 2–4 passes**. Expect to tune pass count by depth-check rather than trusting any table (convergent warning in S1, S2, S11 that focus and material variation dominate).

## 5. Char mitigation practices (cross-source)

1. **Low-power, high-frequency cleanup passes** after the clearance block (S5, S6 — vendor; convergent).
2. **A second/final fill pass "cleans up" char** left by the first (S1 — anecdote; consistent with #1).
3. **Cross-hatch with scan-angle rotation between passes** (45°, 90° increments) to even out removal and avoid localized FR4 burn-through (S1, S5).
4. **Stop-at-substrate discipline**: most char comes from continuing to fire after copper is gone (S2, S11, and the Hackaday 2026 discussion in S7); prefer more, lighter passes with inspection over overshoot.
5. **Mechanical/chemical post-clean**: 400-grit light sanding (S11); the concern is conductive burnt resin between tracks (S11) — an argument for the UV finishing pass near trace edges.
6. **Hybrid fallback**: resist patterning + chemical etch eliminates FR4 char entirely and is what multiple 20 W users adopt at fine pitch (S2, S7 — anecdotes).
7. **Ventilation/fume extraction** — FR4 epoxy fumes are hazardous (S2, S11).
8. **MOPA pulse control matters**: two independent reports of standard Q-switched fiber failing on copper/FR4 where MOPA succeeds (S1, S2).

## 6. Verdict detail vs. the plan

- **8/8 floor:** supported everywhere; no source reports 8/8 as difficult on 20 W-class hardware with multi-pass hatching. Conservative and appropriate as a floor.
- **6/6:** no direct 6/6 data point found, but it sits comfortably between the reliable-8-mil / demonstrated-4-mil bracket (S11, S12-with-caveat). No contradiction.
- **4/4:** demonstrated by direct ablation on 20 W as far back as 2021 (S11) but with explicit unevenness/char caveats; the 2026 hybrid result (S7) needed clearance slightly above 4 mil even with chemical etch. 4/4 as a *later milestone with UV finishing* is realistic; 4/4 as an early direct-ablation deliverable would be contradicted by the anecdote base.
- **PRESET-F0:** no documented parameter set beats the plan's premise, but the survey strongly suggests PRESET-F0 be structured as **main block + cleanup block** (two stages) per §4/§5, and that pass count for 1 oz at 20 W be budgeted in the 10–25 range, not single digits.

*Freshness note: quantitative minimum trace/space for direct ablation rests mainly on 2021–2022 sources (S11, S12); 2025–2026 sources confirm the process landscape (MOPA superiority, two-stage recipes, hybrid fallback) but publish fewer hard numbers.*
