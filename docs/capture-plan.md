# DRV-1 — B4 USB capture plan (operator procedure)

The reusable "unknown driver" method starts here: capture real USB traffic from
the **working** vendor path (LightBurn driving the ComMarker B4, a BJJCZ/JCZ
galvo controller) while **varying exactly one parameter per session**, so the
offline decode (DRV-2) can attribute every byte to a cause by differencing.

**You** drive the laser and run the captures. This document is the exact
procedure; `tools/capture.sh` is the recorder that enforces one-experiment-per-
file and writes the manifest rows. Nothing here sends anything to the machine —
capture is passive; LightBurn does all the driving.

> **Verification note.** This kit was authored in a cloud container with no USB
> stack (`usbmon`, `tshark`, `lsusb` all absent), so the commands below are
> **not yet dry-run-verified on hardware**. Before trusting them, do §2 and §7
> (dummy capture on any USB device) and reconcile any syntax against your local
> `man usbmon` / `man tshark` — the task's standing rule is *verify against the
> man pages, not memory*. Fix discrepancies in `tools/capture.sh` and note them
> in `docs/decisions.md`.

---

## 0. Safety (every emitting experiment)

Experiments 02–12 fire the beam. For all of them:

- **Lid closed**, enclosure interlock engaged, **airflow/extraction ON**.
- Target is an **anodized aluminium card** (marks legibly, no fumes of concern),
  taped flat at a known focus height. Never bare copper for these — the point is
  clean protocol traffic, not a good burn.
- **Minimum power** that still marks the card; the recipe values are recorded,
  not optimized.
- Beam dump / backstop behind the card. Eyes: OD-rated glasses even lid-closed.
- Know where the **physical e-stop** is before arming.

If anything looks wrong, stop the LightBurn job (that traffic is experiment 11's
job anyway) and power the laser down. A ruined capture is free; a fire is not.

---

## 1. One-time setup

1. **Record the device identity in `RUNLOG.md`** (repo root) — this is the
   dependency the rest of DRV blocks on. Plug the B4 in, LightBurn **closed**,
   then:

   ```sh
   lsusb
   ```

   Find the B4's line, e.g. `Bus 003 Device 011: ID 9588:9899 ...`. Copy the
   `ID vvvv:pppp`, the **bus** (003) and **device** (011) into `RUNLOG.md`:

   ```
   B4 USB ID:   9588:9899        # vendor:product from lsusb
   B4 bus:      3                # decimal bus number
   B4 dev addr: 11               # device address (changes on replug!)
   lsusb -v:    captured to captures/00-descriptors.txt (see exp 00)
   ```

   The **device address changes every replug/power-cycle** — re-check `lsusb`
   and update `RUNLOG.md` at the start of each session. The **bus** is stable
   as long as you use the same physical port; keep to one port.

2. **Load usbmon** (needs root; once per boot):

   ```sh
   sudo modprobe usbmon
   ```

   Confirm the capture interfaces exist:

   ```sh
   tshark -D | grep usbmon
   ```

   You want `usbmon<BUS>` for the B4's bus (e.g. `usbmon3`). `usbmon0` captures
   **all** buses — avoid it, it drowns the B4 in other devices' traffic.

3. **Capture permissions.** `dumpcap`/`tshark` need to read usbmon. Either run
   the capture with `sudo`, or add yourself to the `wireshark` group and ensure
   `dumpcap` has the capability (`sudo dpkg-reconfigure wireshark-common` on
   Debian/Ubuntu grants non-root capture). `tools/capture.sh` calls `dumpcap`;
   if it can't open the interface, that's this.

---

## 2. Prove the tooling first (DRV-1 done-when)

Before touching the laser, prove the recorder works on a **harmless** USB
device (a keyboard/mouse/flash drive on another bus):

```sh
# find any other USB device's bus with `lsusb`, then:
tools/capture.sh --interface usbmon<OTHER_BUS> --dev <ADDR> \
  --seconds 3 --exp 99 --desc "tooling dry-run, not the B4" --dry-target
# wiggle the mouse / re-plug the stick during the 3 s
```

You should get `captures/99-tooling-dry-run.pcapng` (nonzero size) and a new row
in `captures/MANIFEST.csv`. Open it in Wireshark and confirm you see USB URBs.
Delete the `99-*` file and its manifest row afterward — it's not part of the
matrix. **This step is the DRV-1 acceptance test.**

---

## 3. The experiment matrix (one variable per capture)

Run these **in order**, at the machine, LightBurn as the only thing driving the
B4. Each row is one `tools/capture.sh` invocation (see §5). **Change exactly one
thing** from the referenced baseline. Record the **exact LightBurn parameters**
you used in the `--desc` (they also land in `RUNLOG.md`'s session log).

Experiment 03 is the **line baseline**; experiment 07 is the **fill baseline**.

| NN | Name | What you do | Isolates |
|----|------|-------------|----------|
| 00 | enumeration | Plug the B4 in with capture already running; also save `lsusb -v` for it to `captures/00-descriptors.txt` | endpoint map, descriptors |
| 01 | connect + idle 30 s | Open LightBurn, let it connect to the B4, sit idle 30 s, do **not** run a job | keepalive / status polling |
| 02 | red-pointer frame | Use LightBurn's **Frame** (red pointer traces the job bounds) on a 10 mm square, no firing | pointer frames; proves they carry no laser-enable |
| 03 | line (BASELINE) | Mark one **10 mm line, Line mode**, record power/speed/freq/pulse | the mark command + a full job lifecycle |
| 04 | line, power +10 % | Exp 03 with **power +10 %**, nothing else | the power field |
| 05 | line, speed ×2 | Exp 03 with **speed ×2** | the speed field |
| 06 | line, freq changed | Exp 03 with **frequency changed** (note old→new) | the frequency field |
| 07 | fill square (BASELINE) | Mark a **10 mm square, Fill**, at a known **interval**, record it | fill/hatch encoding |
| 08 | fill, interval ×2 | Exp 07 with **interval ×2** | the interval field |
| 09 | fill, passes = 2 | Exp 07 with **passes = 2** | pass/repeat encoding |
| 10 | fill, angle 17° | Exp 07 with **fill angle 17°** | hatch-angle encoding |
| 11 | STOP mid-job | Start exp 07's job, press **STOP** partway | the abort command |
| 12 | job at +25 mm offset | Exp 03's line **moved +25 mm** in X on the LightBurn canvas | coordinate scaling / origin |
| 13 | disconnect | With capture running, close LightBurn / unplug | teardown, close command |

Rules that make the decode tractable:

- **Only one variable moves per row.** If you fat-finger two, redo the capture.
- Keep the **same target position and focus** for 03–12 except where the
  experiment is the move (12). A drifting origin muddies the coordinate decode.
- Note the **actual numbers** every time (power %, mm/s, kHz, ns, interval mm,
  passes, angle). "+10 %" in the table means *you* record 20→22 %, etc.
- If LightBurn re-sends the whole job on STOP/retry, that's fine — 11 is about
  finding the abort opcode, and re-sends are themselves evidence.

---

## 4. What "done" looks like for you

After the session you should have, in `captures/`:

- `00-descriptors.txt` + `00-*.pcapng` … `13-*.pcapng` — 14 capture files,
- every one with a row in `captures/MANIFEST.csv`,
- `RUNLOG.md` updated with the USB ID/bus and the per-experiment parameter log,
- (optionally) a phone photo of the marked card per firing experiment.

Commit `captures/`, `RUNLOG.md`, and any photos. That set is the **entire input
to DRV-2** (offline decode). Do **not** hand-edit the pcapng files.

---

## 5. Recorder usage (`tools/capture.sh`)

```sh
tools/capture.sh \
  --interface usbmon3 \        # the B4's bus interface (from `tshark -D`)
  --dev 11 \                   # B4 device address (from lsusb, this session)
  --exp 03 \                   # experiment number 00..13 (or 99 for the dry-run)
  --seconds 20 \               # auto-stop after N s; omit to stop with Ctrl-C
  --desc "10mm line, Line, 20% 500mm/s 30kHz 2ns"   # goes in the manifest + filename slug
```

It writes `captures/<NN>-<slug>.pcapng`, appends a `MANIFEST.csv` row, and
refuses to overwrite an existing experiment number (rename/remove first). Run
`tools/capture.sh --help` for the full flag list. Start the recorder **before**
you press Frame/Start in LightBurn, and give it a second of lead-in.

---

## 6. Manifest schema (`captures/MANIFEST.csv`)

One row per capture. Columns (header committed in the file):

| column | meaning |
|--------|---------|
| `exp` | experiment number, `00`–`13` (`99` = tooling dry-run) |
| `file` | capture filename, relative to `captures/` |
| `date` | ISO 8601 UTC of the capture |
| `interface` | usbmon interface used (e.g. `usbmon3`) |
| `dev_addr` | USB device address captured |
| `baseline` | the experiment this one varies from (`-` for baselines/00/01) |
| `variable` | the single parameter changed vs. baseline (`-` if none) |
| `params` | the exact recipe / action (power, speed, freq, interval, passes, angle…) |
| `desc` | free-text note |
| `sha256` | filled by `cargo xtask fixtures` when it manifests the repo |

`variable` + `baseline` are what DRV-2 differences on — fill them honestly.

---

## 7. Troubleshooting

- **`tshark -D` shows no usbmon** → `sudo modprobe usbmon`; check
  `/sys/kernel/debug/usb/usbmon/` exists (mount debugfs if not:
  `sudo mount -t debugfs none /sys/kernel/debug`).
- **`dumpcap: permission denied`** → run under `sudo`, or fix capture perms
  (§1.3).
- **B4 not in `lsusb`** → different port/cable; it may enumerate as a generic
  serial/HID bridge — record whatever VID:PID appears and note the product
  string.
- **Huge files / other devices' traffic** → you captured on `usbmon0` (all
  buses) or the wrong bus. Use the B4's specific `usbmonN`.
- **Device address changed mid-session** → you replugged/power-cycled; re-run
  `lsusb`, update `--dev` and `RUNLOG.md`. Bus stays put if the port does.
