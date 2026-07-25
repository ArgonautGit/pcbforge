# EMIT-1 — LightBurn `.lbrn2` schema subset (evidence-derived)

Derived **by evidence only** from `samples/lbrn2/*.lbrn2` — eleven projects
from the operator's real device (`DeviceName="BSLFiber"`, LightBurn Pro
2.1.03): ten differing from `base.lbrn2` in exactly one setting, plus
`path-shape.lbrn2` (a hand-drawn polyline establishing the `Path` encoding). Each field below cites
the sample whose one-variable diff against `base` establishes it. This is the
subset PCBForge's emitter (EMIT-2) needs; it is not the whole format.

## Document structure

```xml
<?xml version="1.0" encoding="UTF-8"?>
<LightBurnProject AppVersion="2.1.03" DeviceName="BSLFiber" FormatVersion="1"
                  MaterialHeight="0" MirrorX="False" MirrorY="False"
                  AskForSendName="True">
    <Thumbnail Source="…base64 PNG…"/>   <!-- cosmetic; omittable -->
    <VariableText>…</VariableText>        <!-- defaults; not process-relevant -->
    <UIPrefs>…</UIPrefs>                  <!-- editor prefs; not process-relevant -->
    <CutSetting type="Scan|Cut">…</CutSetting>   <!-- one per layer, see below -->
    <Shape Type="Rect" CutIndex="n" …>…</Shape>  <!-- geometry, see below -->
    <Notes ShowOnLoad="0" Notes=""/>
</LightBurnProject>
```

The root `DeviceName` must match a device configured in the operator's
LightBurn (`BSLFiber` here) or LightBurn prompts on open. `MirrorX/Y` are the
galvo's mirror flags (both `False` on this rig). `Thumbnail`, `VariableText`,
and `UIPrefs` are cosmetic/editor state — every sample carries identical
blocks and none encodes process data, so the emitter may emit them verbatim or
omit them.

## `CutSetting` — one per layer

`type` attribute selects the layer mode:

| `type` | LightBurn mode | Evidence |
|---|---|---|
| `Scan` | Fill | `base` and all fill variants |
| `Cut` | Line | `line-vs-fill.lbrn2` (only change vs base) |

Child elements (`<name Value="…"/>` form). Units and the establishing diff:

| Element | Meaning | Unit | Value in `base` | Established by |
|---|---|---|---|---|
| `index` | layer index (0-based) | int | 0 | `two-layer` (0 and 1) |
| `name` | layer name | string | `C00` | `two-layer` (`C00`, `C01`) |
| `maxPower` | max power | % | 20 | fixed on this rig (never varied) |
| `maxPower2` | second power reg | % | 20 | tracks `maxPower`; emit equal |
| `speed` | scan/cut speed | **mm/s** | 1000 | `speed.lbrn2` → 2000 |
| `frequency` | pulse rate | **Hz** | 30000 | `frequency.lbrn2` → 60000 (= 60 kHz) |
| `QPulseWidth` | MOPA Q-pulse width | **ns (int)** | 1 | `pulse-width.lbrn2` → 5 |
| `interval` | fill line interval | **mm** | 0.03 | `interval.lbrn2` → 0.05 |
| `angle` | fill scan angle | **deg** | *absent ⇒ 0* | `fill-angle.lbrn2` adds `angle=45` |
| `numPasses` | fill pass count | int | *absent ⇒ 1* | `passes.lbrn2` adds `numPasses=5` |
| `globalRepeat` | whole-job repeats | int | *absent ⇒ 1* | `global-passes.lbrn2` adds `globalRepeat=5` |
| `anglePerPass` | angle increment/pass | deg | *absent ⇒ 0* | `two-layer` C01 (`20`) |
| `crossHatch` | cross-hatch fill | 0/1 | 1 | constant across samples |
| `wobbleEnable` | wobble | 0/1 | 1 | constant across samples |
| `subname` | sub-layer name | string | `sublayername` | constant |
| `priority` | run order | int | 0 | `two-layer` (0, 1) |
| `tabCount`/`tabCountMax` | tab/bridge counts | int | 1 | constant |

Notes:
- **Frequency is stored in Hz**, not kHz — `AblationParams.frequency_khz`
  must be multiplied by 1000 on the way out.
- **`QPulseWidth` is an integer ns** — `AblationParams.pulse_ns` maps directly.
- **Power is operator-fixed at 20%** on this MOPA fiber; fluence is controlled
  by `QPulseWidth` + `frequency` (see decisions.md, MOPA correction). The
  emitter still writes `maxPower`/`maxPower2` from `AblationParams.power_pct`
  so a rig that *does* vary power is supported.
- **Two distinct pass fields.** `numPasses` is the fill's own pass count (the
  natural target for `AblationParams.passes` / a `PassGroup`); `globalRepeat`
  repeats the entire job. PCBForge ablation passes → `numPasses`; `globalRepeat`
  is left at default unless a whole-job repeat is explicitly requested.
- **Absent ⇒ default.** LightBurn omits an element at its default (e.g. no
  `angle` means 0, no `numPasses` means 1). The emitter should likewise omit
  defaults to stay byte-close to hand-authored files.
- **Wobble is opt-in and always explicit.** Every sample carries
  `wobbleEnable=1` because the operator's base *config* runs wobble on — that
  is device state, not a format default. The emitter defaults wobble OFF and
  always writes an explicit 0/1 so an absent field can't inherit the device
  profile (LR-36). When enabled, the wobble geometry can be set via
  `wobbleStep` (spacing along the path, mm) and `wobbleSize` (diameter, mm);
  these field names are **inferred** from LightBurn's galvo cut settings, not
  observed in any sample (none varies them) — verify on first live use, like
  the open-`Line` primitive. At 0 they are omitted ⇒ the device profile's
  values apply.

## `Shape` — geometry

Established (Rect only) by every sample:

```xml
<Shape Type="Rect" CutIndex="0" W="10" H="10" Cr="0">
    <XForm>1 0 0 1 35 35</XForm>
</Shape>
```

- `CutIndex` binds the shape to the `CutSetting` whose `index` matches (a shape
  on layer C01 has `CutIndex="1"` — `two-layer.lbrn2`).
- `W`/`H` are the un-transformed width/height (mm); `Cr` is corner radius.
- `XForm` is a 2×3 affine `a b c d e f` = matrix `[[a c e],[b d f]]`; for a Rect
  the translation `(e, f)` is the **center** (a 10×10 rect with `…35 35` is
  centered at (35, 35)). Applies to all shape types.

### `Type="Path"` — arbitrary polylines (from `path-shape.lbrn2`)

The operator supplied an 11th sample containing a hand-drawn closed 5-sided
polyline (line tool) and an ellipse:

```xml
<Shape Type="Path" CutIndex="1" VertID="0" PrimID="0">
    <XForm>1 0 0 1 0 0</XForm>
    <VertList>V14 45c0x1c1x1V15 53c0x1c1x1V22 53c0x1c1x1V22 47c0x1c1x1V17 49c0x1c1x1</VertList>
    <PrimList>LineClosed</PrimList>
</Shape>
<Shape Type="Ellipse" CutIndex="1" Rx="7" Ry="5">
    <XForm>1 0 0 1 38 54</XForm>
</Shape>
```

- Unlike `Rect` (center in `XForm`), a `Path`'s `XForm` is **identity** and its
  vertices are **absolute mm**: `V<x> <y>` per vertex.
- **`VertID`/`PrimID` identify the shape's vertex/primitive lists and must be
  unique per shape.** Established by failure evidence: an emitted job whose 37
  Path shapes all carried `VertID="0" PrimID="0"` (copied from this
  single-path sample) burned as a fan of rays converging on shape 0's first
  vertex — LightBurn cross-links lists that share an ID. The emitter assigns a
  monotonically increasing ID per shape.
- The `c0x1c1x1` suffix is identical on every line vertex (a control-tag
  constant, not geometry) — reproduced verbatim by the emitter.
- `PrimList` is `LineClosed` for a closed polyline. The open form `Line` is
  **inferred**, not observed (the sample shape is closed) — the one inference
  in this schema; flag for verification on first live use of an open path.
- `Ellipse` carries `Rx`/`Ry` with the center in `XForm` translation (Rect
  convention). Not needed by the emitter (circles are emitted as fine
  polygons) but recorded as evidence.

This is everything EMIT-2 needs: `cam::lbrn2` emits Fill/Line layers with
`Type="Path"` shapes and is golden-checked against these samples
(`crates/cam/tests/lbrn2_golden.rs` asserts the emitted VertList for the
sample's exact pentagon matches LightBurn's own bytes).
