# RES-2: LightBurn Job Automation on Linux for Galvo Devices

**Date:** 2026-07-08 (all URLs accessed 2026-07-08)
**Question:** What automation does LightBurn's current version offer on Linux for galvo devices — CLI flags, watch folders, anything that removes the "press play" prompt — as a stopgap until DRV-6 (native driver) lands?

---

## TL;DR

LightBurn's current version (2.1.03, released 2026-06-30) **does not run on Linux at all**. Linux support ended with 1.7.08 (built 2025-03-26), which remains downloadable, activatable, and supports EzCad2-based galvo devices (JCZFiber/BSLFiber). The only "press play"-removing automation surface LightBurn has ever shipped is an **undocumented loopback UDP command interface** (`LOADFILE:`/`FORCELOAD:`/`START`/`STATUS`/`PING`/`CLOSE` on port 19840) plus opening a file passed as a CLI argument. There is **no watch-folder feature**, and LightBurn Bridge is Ruida-DSP-only (irrelevant to galvo).

**Recommendation:** Pin LightBurn **1.7.08 Pro on Linux** and drive it from a PCBForge-side queue daemon over the UDP interface (`FORCELOAD:` then `START`, polling `STATUS`). One bench verification is mandatory first: confirm the 1.7.08 *Linux* build actually listens on UDP 19840 and that `START` fires a *galvo* job — neither is confirmed by any source for the Linux+galvo combination (see Unverified items).

---

## 1. Current state of LightBurn (verified 2026-07-08)

| Fact | Source |
| --- | --- |
| Current release is **2.1.03**, 2026-06-30; 2.1.00 released 2026-05-19, 2.1.02 on 2026-06-01 | Release index: <https://release.lightburnsoftware.com/LightBurn/Release/>; blog: <https://lightburnsoftware.com/blogs/news/lightburn-2-1-quick-nest-enhanced-camera-support-undo-history-and-more>, <https://lightburnsoftware.com/blogs/news/lightburn-2-1-02-patch-release> (accessed 2026-07-08) |
| **"LightBurn version 1.7 is the last version compatible with Linux"** — current official docs, which link to the 1.7.08 download | <https://docs.lightburnsoftware.com/2.1/Licensing/> (current 2.1 docs, accessed 2026-07-08) |
| End-of-Linux was announced July 2024 (staff, forum) | <https://forum.lightburnsoftware.com/t/linux-support-to-end-after-v1-7/144605> (July 2024; staff announcement); secondary: <https://hackaday.com/2024/07/31/lightburn-turns-back-the-clock-bails-on-linux-users/> (2024-07-31). *Freshness deviation: this is the newest primary statement of the decision; the current docs quote above confirms it is still in force as of 2026-07.* |
| 1.7.08 Linux builds remain downloadable: `.7z`, `.AppImage`, `.run`, all dated 2025-03-26 | <https://release.lightburnsoftware.com/LightBurn/Release/LightBurn-v1.7.08/> (accessed 2026-07-08) |
| Galvo (EzCad2/JCZFiber over USB) is supported on "PC, Mac, or Linux" in the 1.x line; Linux needs udev rules for USB permissions; 1.7 added BSLFiber | <https://docs.lightburnsoftware.com/galvo/index.html> (legacy docs, explicitly marked "LightBurn 1.6 and earlier", accessed 2026-07-08); <https://docs.lightburnsoftware.com/galvo/Setup.html> |
| EZCad3-based galvo support arrived only in **2.1** (May 2026) — i.e. **never available on Linux** | <https://lightburnsoftware.com/blogs/news/lightburn-2-1-quick-nest-enhanced-camera-support-undo-history-and-more> (2026-05-19, accessed 2026-07-08) |
| 2.x licensing: **Core** $99 (GCode only) vs **Pro** $199 (GCode + DSP + **Galvo**); $40/yr update renewal; license remains perpetually usable for versions released within its update window | <https://lightburnsoftware.com/pages/license-page>; <https://docs.lightburnsoftware.com/2.1/Licensing/> (accessed 2026-07-08) |

Licensing implication for PCBForge: galvo requires a **Pro** key. Per the current licensing docs, a key stays valid for any version released within its update period and "you will always be able to continue using the most recent version of LightBurn that is compatible with your operating system" — for Linux that is 1.7.08. A Pro license purchased today has an update window covering 1.7.08's 2025-03-26 release, so activation of 1.7.08 on Linux with a current key should work (docs statement + cross-platform key reuse per <https://docs.lightburnsoftware.com/1.7/LicenseFAQs/>); this exact new-2.x-key-on-1.7-Linux path is *inferred from the license model, not demonstrated* — cheap to verify at purchase time.

## 2. Automation surfaces, one by one

### 2.1 Command-line arguments — exists, minimal, does NOT press play

- Passing a file path as an argument opens LightBurn with that file loaded. Confirmed by staff (Oz) in the canonical forum thread: <https://forum.lightburnsoftware.com/t/launch-lightburn-and-load-files-into-lightburn-using-command-line-interface/9297> (Dec 2019 — *freshness deviation: this is still the newest staff-authored CLI reference; no CLI page exists in current docs*). Anecdotal (forum).
- The only officially documented flag today is `-d` (debug logging): <https://docs.lightburnsoftware.com/2.1/Reference/EnableDebugLog/> (accessed 2026-07-08).
- **No** documented flag exists to start a job, run headless, or exit after completion. CLI alone cannot remove the "press play" prompt.

### 2.2 UDP loopback command interface — the real automation path (undocumented)

LightBurn listens for UDP datagrams (default: commands to **19840**, replies on **19841**) and a bundled Windows helper (`SendUDP.exe`) wraps it. Known command set (community-assembled; **no official documentation exists**):

`PING`, `LOADFILE:<path>`, `FORCELOAD:<path>` (loads without the "save changes?" prompt), `START` (presses play), `STATUS`, `CLOSE`, `FORCECLOSE`.

- Command list + ports: <https://forum.lightburnsoftware.com/t/full-list-of-lightburn-udp-commands/59622> (anecdotal, 2022); ports also in third-party tool README <https://github.com/bunkford/lightburn_automation> (last release 2025-06-18; Windows/Mac binaries only). *Freshness deviation: no ≤6-month source enumerates the commands; these are the newest.*
- Staff (Oz) confirmed the interface's intent — "load or import a file, start the job, and close the software, and the command set is limited to that": <https://forum.lightburnsoftware.com/t/lightburn-udp-commands-or-command-line-docs/30024> (Jan 2021, staff, anecdotal/forum).
- Most recent staff engagement: July 2025 — staff answered a UDP automation workflow question, recommending `FORCELOAD` to suppress prompts and confirming `START` in an automated (photobooth-style) pipeline: <https://forum.lightburnsoftware.com/t/lightburn-udp-automation-gcode-import-questions/174595> (2025-07, staff reply, anecdotal). *Freshness deviation: ~12 months old; newest available on this point.*
- Known limitation: the port is **not configurable** — <https://forum.lightburnsoftware.com/t/can-the-udp-port-be-configured/84964> (anecdotal) and open feature request <https://lightburn.fider.io/posts/2399/configurable-udp-port> (accessed 2026-07-08).

Stability read: the command set has been unchanged since at least 2021 (a moderator in Aug 2023 noted it hadn't been expanded: <https://forum.lightburnsoftware.com/t/current-list-of-udp-commands/108444>, anecdotal), and on Linux the binary itself is frozen at 1.7.08 — so the interface literally cannot change under us. The flip side: it is undocumented, so there is no support contract if it misbehaves.

### 2.3 Watch folder / hot folder — does not exist

No watch-folder feature appears anywhere in current or 1.7 documentation. A September 2024 forum request for exactly this (manufacturing hot-folder) went unanswered and auto-closed: <https://forum.lightburnsoftware.com/t/hot-folder-capability/149865> (2024-09-13, anecdotal; *newest available — nothing ≤6 months*). Equivalent behavior must be built PCBForge-side: a filesystem watcher that sends `FORCELOAD:` + `START` over UDP.

### 2.4 LightBurn Bridge — irrelevant to galvo

LightBurn Bridge (Raspberry Pi network relay) is **for Ruida DSP controllers only**; galvo devices are USB-connected EzCad2 hardware. <https://docs.lightburnsoftware.com/latest/Reference/LightBurnBridge/> (accessed 2026-07-08). Not a path for PCBForge.

## 3. Verdicts against the task criteria

| Surface | Exists on Linux? | Works for galvo? | Stability across updates | Removes "press play"? |
| --- | --- | --- | --- | --- |
| CLI file-path load (+`-d`) | Yes (1.7.08 only) | Yes (loads file; device-agnostic) | Frozen forever at 1.7.08 on Linux | **No** |
| UDP interface (`FORCELOAD`/`START`/`STATUS`) | **Unverified** on the Linux build — no source either way; confirmed Windows/Mac | `START` confirmed generally; **unverified specifically for galvo** | Unchanged since ≥2021; Linux binary frozen; undocumented/unsupported | **Yes** (if it works) |
| Watch folder | No — feature does not exist | n/a | n/a | n/a |
| LightBurn Bridge | n/a (Pi appliance) | **No — Ruida DSP only** | n/a | No |
| LightBurn 2.1.x (current) | **No — no Linux builds** | Yes (incl. EZCad3, 2.1+) | Actively updated | Only via UDP, on Win/Mac |

### Unverified items (flagged)

1. **UDP listener present in the 1.7.08 Linux build.** SendUDP helper ships as `.exe`; community tooling targets Windows/Mac (<https://forum.lightburnsoftware.com/t/udp-command-line-control-windows-and-mac/173760>, 2025-06-18). No source confirms or denies the Linux binary listens on 19840. **Bench-test before committing.**
2. **`START` behavior on a galvo device** (galvo's Laser window differs from GCode/DSP; staff confirmations of `START` don't name the device family). **Bench-test.**
3. New 2.x Pro key activating 1.7.08 on Linux — inferred from license docs, not demonstrated.

## 4. Recommendation

**Adopt: LightBurn 1.7.08 Pro (AppImage) on the Linux operator box, driven by a PCBForge queue daemon over loopback UDP** — daemon watches PCBForge's output directory, sends `FORCELOAD:<job.lbrn2>` then `START`, polls `STATUS` for completion. This is the only LightBurn-provided mechanism that removes the "press play" prompt on Linux, its command surface is frozen (no update-churn risk), and it needs no changes to PCBForge's .lbrn2 output.

**Gate (do first, ~1 hour):** verify unverified items 1 and 2 on the bench — `echo -n PING | nc -u 127.0.0.1 19840` against a running 1.7.08 Linux instance, then `FORCELOAD`/`START` against the galvo. Treat the recommendation as conditional until this passes.

**Flip conditions:**

- **UDP absent or `START` inert on Linux/galvo** → flip to a small Windows host or VM (with USB passthrough) running current LightBurn 2.1.x + the same UDP driver; PCBForge on Linux talks to it over the network (remote IP UDP is known to work — bunkford tool). Alternative last resort on Linux: CLI file load + synthetic input (xdotool) — fragile, not recommended.
- **EZCad3-based controller needed** → 1.7.08 cannot drive it (EZCad3 support is 2.1+, no Linux). Flip to Windows-host option or accelerate DRV-6.
- **1.7.08 stops activating or downloads are pulled** → escalate; archive the installer and license seat now as insurance.
- **LightBurn ships Linux builds again** (no sign of this; docs reaffirm the drop as of 2026-07) → re-evaluate for 2.x features.
- **DRV-6 lands** → retire the whole path; nothing here creates lock-in (the daemon is ~100 lines and the .lbrn2 pipeline is unchanged).

## Addendum 2026-07-21 — Windows operator box, official UDP docs, galvo completion signal

Context change since the original report: the machine-identity correction
(decisions.md, 2026-07-14) established the real setup as a ComMarker Omni X
(UV galvo, JCZ/EzCad2 family) driven by **LightBurn Pro 2.1.03 on Windows 11**,
not Linux. That dissolves the Linux-specific constraints above (frozen 1.7.08,
unverified Linux UDP listener) and changes several verdicts. All URLs accessed
2026-07-21.

1. **The UDP interface is now officially documented.** LightBurn publishes an
   "Automation With UDP" guide — command list, ports, and a Python example:
   <https://docs.lightburnsoftware.com/latest/Guides/AutomationWithUDP/>.
   The "undocumented/unsupported" caveat in §2.2 no longer applies. Commands to
   UDP **19840**, replies on **19841** (bind a socket there), plain ASCII, ports
   still not configurable. No settings toggle — the listener is always on while
   LightBurn runs.
2. **Two commands missing from §2.2's list:** `IMPORT:<path>` (import centered
   in workspace — avoid; use absolute-positioned `.lbrn2` + `FORCELOAD`) and
   `LASER:<name>` (select device by name — for us `LASER:BSLFiber`). Replies:
   `OK` = success/idle, `!` = failed/busy, `?` = unknown command.
3. **Completion detection is trustworthy on galvo.** For galvo devices
   LightBurn itself executes the job, so `STATUS` returning `!` (busy) → `OK`
   (idle) reflects the actual burn — staff-endorsed polling loop:
   <https://forum.lightburnsoftware.com/t/lightburn-udp-automation-gcode-import-questions/174595>.
   (The known unreliability is Ruida/DSP, where LightBurn loses the job after
   sending it: <https://forum.lightburnsoftware.com/t/automation-in-lightburn/103319> —
   irrelevant to us.) `STATUS` requires the device selected and connected;
   `PING` returns `OK` only when no modal dialog is open, so always use
   `FORCELOAD`, never `LOADFILE`, to avoid a save-prompt wedging the loop.
4. **Galvo hardware I/O as a belt-and-braces option:** LightBurn's galvo
   support can drive a "Done Marking" output pin and accept a "Start Marking"
   input pin on the JCZ board — a hardware-level job-done/trigger signal
   independent of UDP (same staff thread as above).
5. **Still no other surface.** Release notes through 2.0.05/2.1.x add no CLI,
   scripting, REST API, or watch folder; `SendUDP.exe` remains a load-only
   wrapper with no `START`. An open feature request for a real CLI confirms the
   gap: <https://forum.lightburnsoftware.com/t/request-command-line-interface-cli-tool-for-batch-exporting-lbrn2-projects/176431>.

**Revised recommendation:** drive the existing Windows LightBurn 2.1.03
directly from PCBForge over loopback UDP — no VM, no version pinning, ~zero
dependencies from Rust (`std::net::UdpSocket`, bind `127.0.0.1:19841`, ~2 s
read timeout): `PING` (readiness) → `LASER:BSLFiber` → `FORCELOAD:<job.lbrn2>`
→ `START` → poll `STATUS` until `!`→`OK`. Natural home is a `Marker`
implementation in `crates/drivers` so orchestra's Laser stages can fire jobs
unattended. **Bench gate (unchanged in spirit):** verify `START` actually fires
a job on the Omni X galvo profile before wiring it into the stage engine — the
staff confirmations don't name the galvo device family explicitly. DRV-6
(native JCZ driver) remains the eventual replacement; this path adds no
lock-in.

## 5. Source log

All accessed 2026-07-08. Forum posts marked (A) = anecdotal.

1. <https://docs.lightburnsoftware.com/2.1/Licensing/> — official, current: 1.7 last Linux version; perpetual-use terms.
2. <https://release.lightburnsoftware.com/LightBurn/Release/> and <https://release.lightburnsoftware.com/LightBurn/Release/LightBurn-v1.7.08/> — official: 2.1.03 latest; Linux 1.7.08 artifacts dated 2025-03-26.
3. <https://lightburnsoftware.com/blogs/news/lightburn-2-1-quick-nest-enhanced-camera-support-undo-history-and-more> — official, 2026-05-19: 2.1 features incl. EZCad3 galvo.
4. <https://lightburnsoftware.com/blogs/news/lightburn-2-1-02-patch-release> — official, 2026-06-01.
5. <https://lightburnsoftware.com/pages/license-page> — official: Core/Pro pricing, galvo = Pro, $40/yr renewals.
6. <https://docs.lightburnsoftware.com/galvo/index.html>, <https://docs.lightburnsoftware.com/galvo/Setup.html> — official legacy (≤1.6 marked): galvo on Linux via USB + udev.
7. <https://docs.lightburnsoftware.com/latest/Reference/LightBurnBridge/> — official: Bridge is Ruida-only.
8. <https://docs.lightburnsoftware.com/2.1/Reference/EnableDebugLog/> — official: `-d` flag.
9. (A) <https://forum.lightburnsoftware.com/t/linux-support-to-end-after-v1-7/144605> — staff announcement, 2024-07.
10. (A) <https://forum.lightburnsoftware.com/t/launch-lightburn-and-load-files-into-lightburn-using-command-line-interface/9297> — staff (Oz), 2019-12: CLI file load, SendUDP. *Freshness deviation noted.*
11. (A) <https://forum.lightburnsoftware.com/t/lightburn-udp-commands-or-command-line-docs/30024> — staff (Oz), 2021-01: UDP scope statement.
12. (A) <https://forum.lightburnsoftware.com/t/full-list-of-lightburn-udp-commands/59622> — community, UDP command list.
13. (A) <https://forum.lightburnsoftware.com/t/lightburn-udp-automation-gcode-import-questions/174595> — staff reply, 2025-07: FORCELOAD/START automation workflow (newest staff word on UDP).
14. (A) <https://forum.lightburnsoftware.com/t/current-list-of-udp-commands/108444> — moderator, 2023-08: UDP set not expanded.
15. (A) <https://forum.lightburnsoftware.com/t/can-the-udp-port-be-configured/84964> + <https://lightburn.fider.io/posts/2399/configurable-udp-port> — port not configurable.
16. (A) <https://forum.lightburnsoftware.com/t/hot-folder-capability/149865> — 2024-09: hot-folder request, unanswered (newest available on watch folders).
17. <https://github.com/bunkford/lightburn_automation> — third-party UDP tool, release 2025-06-18: ports 19840/19841, command set, remote-IP use.
18. (A) <https://forum.lightburnsoftware.com/t/udp-command-line-control-windows-and-mac/173760> — 2025-06: community UDP CLI tool, Windows/Mac only.
19. <https://hackaday.com/2024/07/31/lightburn-turns-back-the-clock-bails-on-linux-users/> — secondary press, 2024-07-31.
