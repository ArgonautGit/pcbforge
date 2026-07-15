# Driving the PCBForge console headlessly (agent guide)

You can **see** and **interact with** the console's egui UI without a display —
to reproduce a bug, verify a change, or inspect widget/app state. Two
capabilities:

- **Interact + inspect** — click, type, press keys, dump the widget tree and a
  curated app-state summary. Needs **no GPU or display**.
- **See** — render the current frame to a PNG you can open/read. Needs a
  software GPU adapter (`scripts/headless-gpu.sh` sets it up).

The engine is `crates/ui/examples/debug_driver.rs`, built on `egui_kittest`. It
steps the real `ConsoleApp` with no window, drives widgets through the
accessibility tree, and renders frames via wgpu. **Every run starts from a fresh
app**, so a script is a deterministic replay — put the whole scenario in one
script rather than expecting state to persist between invocations.

## Run it

```sh
# Pipe a script on stdin:
printf 'tree\nclick "📷 Camera"\nstate\n' | cargo run -p ui --example debug_driver

# Or from a file:
cargo run -p ui --example debug_driver -- script.txt
```

Each command prints `OK ...` or `ERR ...`. The process exits non-zero if any
command failed. Lines starting with `#` and blank lines are ignored.

## Commands

| Command | What it does |
| --- | --- |
| `tree` | Dump the accessibility tree — every widget with its role, label, value, numeric. **Start here** to learn the labels. |
| `state` | Print `ConsoleApp::debug_summary()` — active tab, calibration/lens status, camera frame + view scale, bed-overlay field, place coords, grid/fiducial counts. |
| `click <label>` | Click the widget with that label. |
| `set <label> <value>` | Focus a widget, type `value`, press Enter. Best on editable numeric/text fields (kittest 0.30 has no accesskit SetValue). |
| `type <label> <text>` | Focus a text field and type into it. |
| `key <name>` | Press a key: `enter`, `tab`, `escape`, `space`, `backspace`, `delete`, `up`/`down`/`left`/`right`, `home`, `end`, `a`–`z`, `0`–`9`. |
| `step <n>` | Advance `n` frames. |
| `settle` | Run until the app stops requesting repaints. |
| `screenshot <path>` | Render the current frame to a PNG (needs a GPU adapter — see below). |

### Labels

- Matched **exactly first, then by substring**; if several share a label the
  driver takes the first. `click "⟳ Refresh"` and `click Refresh` both work.
- **Quote labels containing spaces:** `click "📷 Camera"`. Tab labels carry an
  emoji prefix (`🎯 Calibrate`, `✋ Place on board`) — run `tree` to read them.
- Buttons, checkboxes, and sliders carry labels. Bare `text_edit_singleline`
  fields do **not** (see *Adding widgets* below), so target them by a nearby
  labelled widget or drive via `tree`-discovered structure.
- Don't guess labels — run `tree`. Example (top of the tree):

```
Window
  Label value="PCBForge console"
  Button label="⟳ Refresh"
  Button label="🖼 Job preview"
  Button label="📷 Camera"
  Button label="🎯 Calibrate"
  Button label="✋ Place on board"
```

## The debugging loop

1. `tree` — discover the widgets and their current values.
2. `screenshot before.png` — see the starting state (optional; needs GPU).
3. `click` / `set` / `type` / `key` — perform the interaction to test.
4. `state` and/or `screenshot after.png` — observe the effect.
5. Read the printed output and the PNGs, then decide the next step.

Example script:

```
# Does switching to the Camera tab update app state?
tree
click "📷 Camera"
state
screenshot shots/camera.png
```

## Screenshots: GPU setup (one time per shell)

Screenshots (and the snapshot tests) need a wgpu adapter; software rendering is
fine. `scripts/headless-gpu.sh` finds one and exports the right env vars:

```sh
source scripts/headless-gpu.sh          # export into your shell, then run cargo
# or wrap a single command:
scripts/headless-gpu.sh cargo run -p ui --example debug_driver -- script.txt
```

Fallback chain: SwiftShader Vulkan ICD (bundled with the pre-installed Chromium)
→ lavapipe Mesa ICD → software GL. If none exists, install one:

```sh
apt-get install -y mesa-vulkan-drivers libvulkan1
```

**Gotcha:** `VAR=x ... | cargo run` sets `VAR` only for the first command in the
pipeline, not for `cargo`. Either `source scripts/headless-gpu.sh` first, or
build the example first and pipe the script into the built binary under the
wrapper: `... | scripts/headless-gpu.sh ./target/debug/examples/debug_driver`.

Without a GPU, `screenshot` prints `ERR` and the run continues — `tree`,
`state`, and all interaction still work.

## Tests

- `cargo test -p ui --test ui_interaction` — headless interaction tests, no GPU.
- Snapshot tests (`crates/ui/tests/ui_snapshots.rs`) are `#[ignore]`d so
  `cargo test` stays green without a GPU. To run them:

  ```sh
  source scripts/headless-gpu.sh
  UPDATE_SNAPSHOTS=1 cargo test -p ui --test ui_snapshots -- --ignored   # (re)write baselines
  cargo test -p ui --test ui_snapshots -- --ignored                      # compare
  ```

  Baselines live in `crates/ui/tests/snapshots/*.png` (committed, rendered with
  SwiftShader — regenerate on a different GPU if they diff). On mismatch kittest
  writes `*.new.png` / `*.diff.png` next to them (gitignored) for inspection.

## Adding widgets to the console

New widgets show up automatically in `tree` and are drivable — with two rules
for accessibility:

- Give interactive widgets a **label** (`Button::new("…")`,
  `Slider::new(...).text("…")`, `Checkbox::new(&mut b, "…")`).
- A bare `text_edit_singleline` has no label. Associate a preceding label so the
  driver can target it:

  ```rust
  let label = ui.label("Copper .gbr:");
  ui.text_edit_singleline(&mut self.emit_copper).labelled_by(label.id);
  ```

- If you add a field worth inspecting, surface it in
  `ConsoleApp::debug_summary()` so `state` reports it.
