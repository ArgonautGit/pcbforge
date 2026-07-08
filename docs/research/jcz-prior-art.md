# RES-3: Prior Art Survey — Public Documentation of the JCZ/BJJCZ (EZCAD2-class) Galvo USB Protocol

Status: complete. All sources below were verified to exist on 2026-07-08; licenses were read
from the actual LICENSE file or site license page, not inferred.

Purpose: inventory of PUBLIC WRITTEN documentation of the BJJCZ LMC ("EZCAD2-class") USB
protocol so that DRV-2..4 can cite this file as a labeling/terminology reference. PCBForge's
driver is clean-room-implemented from **our own USB captures**; the sources here are prior-art
context and naming reference only.

## Licensing ground rules for DRV-2..4 (summary)

- **Protocol facts are not copyrightable.** Opcodes, packet framing, endpoint numbers,
  VID/PID, parameter semantics, and timing behavior are facts. Reading prose descriptions of
  them and re-stating them in our own words/code is fine.
- **GPL source code must not be copied or translated.** Do not port, transliterate, or
  paraphrase-at-the-code-level any function from a GPL repo (Balor, balor-meerk40t, OPAL)
  into this repo. Do not copy their comments, structure, or identifier schemes wholesale.
- **MIT sources (MeerK40t, galvoplotter) are license-compatible** but the project decision is
  still clean-room from our captures: use them to *label* what we observe (e.g., "this is
  what MeerK40t calls `SetLaserMode`"), not as an implementation template. If code is ever
  copied from an MIT source, attribution/license text must be carried — flag for a decision
  first.
- **CC BY-NC-SA prose (EduTechWiki)**: do not reproduce its tables into this repo (NC/SA
  terms). The facts in it may inform our own independently-worded docs.

---

## A. Primary written protocol documentation

### A1. Bryce Schroeder, "Fiber Laser Engravers" (the original reverse-engineering write-up)

- **URL:** https://www.bryce.pw/engraver.html (a mirror at
  https://www.ferazelhosting.net/engraver.html appears in search results but was not
  reachable through this environment's proxy). Accessed 2026-07-08.
- **What it documents (prose):** the founding public write-up of the protocol RE effort
  (hardware obtained Oct 2021). Asserts: target board is a Beijing JCZ **LMCV4-FIBER-M**
  (Altera FPGA + Cypress 8051 USB controller); commands are **12 bytes** (six little-endian
  int16), sent in batches of **256 commands** padded with no-ops; **three USB endpoints**
  (commands, status polling, security-dongle traffic); BULK transfers with a
  readiness/buffer-status polling discipline; the dongle interaction is optional ("the
  machine does not care if the dongle interaction is done"); board speaks **XY2-100** to the
  galvo head downstream. Also documents the two correction approaches: EZCAD-style
  **`.cor` corfile** loaded at startup (incomplete linearization) vs Balor's own
  **`.csv` calfile** (radial-basis-function interpolation compensating barrel distortion and
  converting galvo units to real units). Links a reference capture (`ThreeOvals.pcap`).
- **License:** the page itself states no license for its prose; it states the Balor software
  suite is **GPL v3** with commercial licensing available via Gnostic Instruments. Treat the
  prose as ordinary copyrighted text: facts usable, wording not.
- **Clean-room posture:** SAFE as a factual/labeling reference (framing, endpoint roles,
  correction-file concepts). Do NOT copy its prose verbatim into our docs.

### A2. Balor repository (Bryce Schroeder) — code + `Documentation/` folder

- **URL:** https://gitlab.com/bryce15/balor — accessed 2026-07-08. Last repo activity
  2022-11-06 (per GitLab API); effectively dormant.
- **License:** **GPL-3.0** — verified from the repo's `LICENSE` file (GPLv3 text; the repo
  also carries `gpl-3.0.txt`). Note: one GitLab-rendered summary elsewhere claimed "SSPL";
  the LICENSE file in `main` is GPLv3, and the author's site confirms GPLv3.
- **What it contains:** Python CLI tools (`balor-sender.py`, `balor-raster.py`, etc.) and
  library (`balor/`), a `Documentation/` directory with **`notes.odt`** and
  **`scratch 1.odt`** (the author's raw reverse-engineering notes), **`ThreeOvals.pcap`**
  (reference EZCAD2 USB capture), board photos, an example calfile (`cal_0002.csv`), and
  `readme.html` (a copy of A1).
- **Clean-room posture:** the **code is GPL — must not be copied or translated** into
  PCBForge. The `.odt` notes are prose inside a GPL repo: facts usable, text not copyable;
  prefer citing A1/A3 instead so no one has to open GPL-repo files during DRV-2. The
  **pcap is a third-party capture — do not import it**; we use our own captures.

### A3. EduTech Wiki, "LMCV4-FIBER-M"

- **URL:** https://edutechwiki.unige.ch/en/LMCV4-FIBER-M — accessed 2026-07-08. Page last
  modified 2022-01-11. Credits the RE work to Bryce Schroeder and Jason Dorie in its
  external links.
- **What it documents:** the most complete *independent prose* protocol reference found:
  board hardware overview; USB identity **VID 0x9588 / PID 0x9899** and four endpoints;
  **two command tables covering 50+ opcodes** (list/job commands and single/realtime
  commands with parameters); Windows driver IOCTL codes (0x99982010–0x999820c4). This is
  documentation-of-facts in tabular prose, not source code.
- **License:** **CC BY-NC-SA** (wiki-wide default per
  https://edutechwiki.unige.ch/en/EduTech_Wiki:Copyrights, which links the by-nc-sa 3.0
  legal code; no per-page override on this page). Accessed 2026-07-08.
- **Clean-room posture:** SAFE to read for opcode names/semantics and to cross-check our
  captures. Do NOT reproduce its tables into this repo (NC + SA terms); our driver docs must
  be independently worded from our own capture analysis, citing this page as corroboration.

### A4. MeerK40t `balormk` driver module (the actively maintained implementation)

- **URL:** https://github.com/meerk40t/meerk40t — module at
  `meerk40t/balormk/` (README:
  https://github.com/meerk40t/meerk40t/blob/main/meerk40t/balormk/README.md). Accessed
  2026-07-08. Actively maintained.
- **License:** **MIT** — verified from repo `LICENSE` ("MIT License, Copyright (c) 2021
  meerk40t"). The `balormk/README.md` licensing section explicitly states the original
  GPL Balor code "was completely scrapped in pieces over time... recoded from scratch...
  only based on some of the original research and none of the original code" (author:
  Tatarize) — i.e., the module is itself a research-based rewrite, which is why it can be
  MIT despite Balor being GPL. Useful precedent for our own clean-room claim.
- **What it documents:** `balormk/README.md` is architecture prose (device/driver/controller
  split, threading, command batching, realtime vs sequential command classes, correction and
  calibration handling, EPP 1.9 legacy mention). The protocol substance lives in code:
  `galvo_commands.py` (LMC command structure definitions), `controller.py` (~1650 lines
  implementing the LMC command protocol, queueing, status polling), `usb_connection.py`
  (pyusb transport), `balor_params.py` (timing/power parameter definitions), `clone_loader.py`
  (handling for JCZ clone-board variants).
- **Clean-room posture:** MIT — legally usable, and the best living reference for
  **naming/labeling** commands and parameters (DRV-2 should adopt or map to its command
  names for community legibility). Project discipline still applies: implement from our
  captures; use balormk to label and sanity-check, not as a porting source, unless a
  decision explicitly authorizes MIT code reuse with attribution.

### A5. galvoplotter (MeerK40t org) — standalone low-level library

- **URL:** https://github.com/meerk40t/galvoplotter (PyPI:
  https://pypi.org/project/galvoplotter/). Accessed 2026-07-08. Last release 0.2.0,
  2023-09-27; low activity.
- **License:** **MIT** — verified from repo `LICENSE` ("Copyright (c) 2023 MeerK40t").
- **What it documents:** README documents the protocol's *conceptual model* in clean prose:
  realtime commands (status, jog, pause/resume/abort) vs sequential list commands; the three
  connection states (`init`/`marking`/`lighting`); plotlike primitives (`mark`, `goto`,
  `light`, `dark`, `dwell`, `wait`); speed/timing parameters (`mark_speed`, `goto_speed`,
  `light_speed`, `dark_speed`, dwell/wait in ms). No separate protocol.md or docs folder —
  the substance is README + `galvo/` source + `examples/`.
- **Clean-room posture:** same as A4 (MIT, labeling reference). Its README's
  realtime-vs-list-command taxonomy is the cleanest published mental model for structuring
  our driver's API and DRV-2 documentation.

---

## B. Secondary sources and pointers

### B1. tatarize/balor-meerk40t — historical bridge plugin

- **URL:** https://github.com/tatarize/balor-meerk40t — accessed 2026-07-08. Last release
  0.3.0 (Jan 2022); superseded by in-tree `balormk` (A4).
- **License:** **GPL-3.0** — verified from repo `LICENSE` (it embedded original Balor code).
- **Documents:** README asserts a few facts (galvo coordinates are native uint16
  0x0000–0xFFFF with 0x8000 center, absolute positioning; Zadig/libusb setup for
  VID 0x9588 / PID 0x9899). Otherwise a code repo.
- **Posture:** GPL — do not copy/translate code. Cite only for the coordinate-space facts,
  which are corroborated by A3/A4 anyway. Prefer A4/A5.

### B2. Hackaday, "Open Source Replacement For EzCAD" (Chris Lott, 2022-01-16)

- **URL:** https://hackaday.com/2022/01/16/open-source-replacement-for-ezcad/ — accessed
  2026-07-08.
- **Documents:** journalism about Balor: RE methodology (observing EZCAD's USB traffic for
  known operations), target hardware (LMCV4-FIBER-M, Altera FPGA + Cypress 8051). No
  protocol details of its own. Standard Hackaday copyright; no code.
- **Posture:** citation-only (project history/context).

### B3. Hackaday, "Collaborative Effort Gets Laser Galvos Talking G-Code" (Dan Maloney, 2022-07-15)

- **URL:** https://hackaday.com/2022/07/15/collaborative-effort-gets-laser-galvos-talking-g-code/
  — accessed 2026-07-08.
- **Documents:** Les Wright's hardware work with the **OPAL Open Galvo** project
  (https://github.com/opengalvo/OPAL, license **GPL-2.0**, verified from repo LICENSE) —
  this is about the downstream **XY2-100 galvo-head wire protocol**, not the JCZ USB
  protocol. Relevant only if PCBForge ever needs the head-side protocol.
- **Posture:** citation-only; OPAL code is GPLv2 — do not copy.

### B4. charliex2, "Fibre laser arrives, let the games begin" (2020-01-31, updated 2020-02-09)

- **URL:** https://charliex2.wordpress.com/2020/01/31/fibre-laser-arrives-let-the-games-begin/
  — accessed 2026-07-08.
- **Documents:** pre-Balor independent RE of the **EZCAD2 Windows stack**: EZD project-file
  binary format (parameter storage offsets), LMC1/LMCMIO DLL exported-function analysis,
  proxy-DLL interception of hardware calls. Does *not* document the USB wire protocol
  itself. No license stated; includes an informational-use-only disclaimer.
- **Posture:** background only. Useful if we ever need EZD file-format facts; not a
  wire-protocol source. Facts usable; no code offered.

### B5. Maker Forums thread, "Open Source software for Chinese Galvo Fiber Laser Engravers"

- **URL:** https://forum.makerforums.info/t/open-source-software-for-chinese-galvo-fiber-laser-engravers/84489
  — started by Bryce Schroeder 2021-12-05; accessed 2026-07-08.
- **Documents:** Balor's announcement thread and ongoing community discussion (including
  the note that Bryce shared RE data with LightBurn, and MeerK40t's galvo support landing
  by Dec 2022). Discussion, not protocol documentation.
- **Posture:** citation-only (history; occasionally useful behavioral anecdotes).

### B6. MeerK40t GitHub wiki (Balor device help pages)

- **URL:** https://github.com/meerk40t/meerk40t/wiki — pages "Online Help: DEVICEBALOR",
  "Online Help balorconfig", "balorcontroller", "baloroperation". Accessed 2026-07-08.
- **Documents:** end-user GUI configuration/operation help (timing/power settings exposed to
  users, corfile selection), **not** wire-protocol documentation. No explicit license on the
  wiki itself (the code repo's MIT license does not automatically cover wiki text).
- **Posture:** useful for naming user-facing parameters (e.g., delays: laser-on/off, polygon
  delays) consistently with community conventions; don't copy text.

### B7. LightBurn galvo support (proprietary — pointer only)

- **URLs:** https://docs.lightburnsoftware.com/galvo/Installation.html and related pages —
  accessed 2026-07-08.
- LightBurn independently implemented EZCAD2-class control (Jason Dorie is credited in A3's
  external links for LMC protocol RE toward LightBurn). Closed source; its docs cover driver
  installation/setup only (they do note clone boards using VID/PID 9588/9980, a useful
  device-ID fact). **Nothing may be derived from LightBurn.**

### B8. Reimplementations in other languages

Searched 2026-07-08 for Rust/Go/C reimplementations ("ezcad-rs", crates.io, VID/PID
searches): **none found**. The Python family (Balor → balor-meerk40t → balormk →
galvoplotter) is the only public implementation lineage. Also checked: `github.com/meerk40t/balor`
does **not** exist (404) — the correct repos are those listed above. No project named
"LMOS" relating to this protocol was found; the closest hits are the LMC-name family itself
and Ruida-protocol docs (jnweiger/ruida-laser), which concern a different controller and are
out of scope.

---

## C. Protocol facts asserted by these sources (facts — usable) vs. code (must not derive)

**Facts asserted in public prose** (each corroborated by ≥2 independent sources above;
verify all against our own captures before relying on them in DRV-2):

- USB identity VID **0x9588** / PID **0x9899** (clones also seen as 9588/9980). [A3, B1, B7]
- Bulk-transfer protocol; command records are **12 bytes = 6 × int16le**, first word is the
  opcode; list commands uploaded in **256-command (3072-byte) batches** padded with NOPs. [A1, A3]
- Two command classes: **realtime/single commands** (status, jog, pause/resume/abort,
  port/light control) and **sequential list commands** (mark/goto/light/dark/dwell/wait,
  speeds, delays); a status/GetState poll returns a short (6-byte) state response and
  governs buffer readiness. [A1, A3, A5]
- Three-ish endpoint layout (command out, status in, dongle) and the fact that **dongle
  traffic is optional**. [A1, A3]
- Coordinate space: unsigned 16-bit galvo units, 0x8000 = field center, absolute
  positioning. [B1, A4]
- Correction handling: EZCAD `.cor` correction tables applied at startup vs.
  application-side calibration (Balor's RBF `.csv` calfile). [A1, A4]
- Timing/marking parameters exposed by the protocol: mark/goto/light speeds, laser on/off
  delays, polygon delay, Q-switch frequency/pulse parameters, dwell/wait times. [A4, A5, B6]
- Downstream of the controller, galvos are driven over **XY2-100** (out of scope for the USB
  driver). [A1, B3]

**Code we must not derive from (copy, port, or translate):**

- `gitlab.com/bryce15/balor` — all Python code (**GPL-3.0**), and its bundled RBF code
  (SciPy-derived, separate copyright). Also do not import its `ThreeOvals.pcap`.
- `github.com/tatarize/balor-meerk40t` — all code (**GPL-3.0**).
- `github.com/opengalvo/OPAL` — all code (**GPL-2.0**) (XY2-100 side; out of scope anyway).
- LightBurn — proprietary; nothing derivable at all.
- EZCAD2 itself, JCZ DLLs (LMC1/LMCMIO), and JCZ SDK headers — proprietary; our knowledge of
  them must come only from our own captures and the prose sources above.

**MIT code (usable in principle, but per project policy: labeling reference only unless a
decision says otherwise):** `meerk40t/meerk40t` (`balormk/`), `meerk40t/galvoplotter`.

## D. Guidance for DRV-2

1. Cite this file instead of re-searching. For command naming, use the balormk/galvoplotter
   (MIT) vocabulary; cross-check semantics against EduTechWiki's tables *by reading, not
   copying*.
2. Every protocol claim in DRV-2's docs must be grounded in **our own ComMarker B4 captures**;
   the sources here are corroboration and labels, never the primary evidence.
3. Anyone who reads GPL *code* files (not prose) while implementing DRV-2..4 should note it;
   ideally implementers work from this memo + our captures + the MIT READMEs only.
