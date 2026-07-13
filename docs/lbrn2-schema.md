# EMIT-1 — LightBurn `.lbrn2` schema subset (evidence-derived)

Derived **by evidence only** from `samples/lbrn2/*.lbrn2` — ten projects from
the operator's real device (`DeviceName="BSLFiber"`, LightBurn Pro 2.1.03),
each differing from `base.lbrn2` in exactly one setting. Each field below cites
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

### Gap: general polyline / path shapes (blocks full EMIT-2)

The samples contain only `Type="Rect"`. PCBForge toolpaths are arbitrary
open/closed polylines (isolation contours, rub-out hatches, board-cut
segments), which LightBurn stores as `Type="Path"` — a format **not present in
any sample**, so it cannot be derived by evidence yet. Per the project rule
(evidence only, no guessing), EMIT-2's geometry emitter is deferred until one
sample containing a drawn path/polyline is provided (a closed polygon such as a
triangle, plus one open polyline). The CutSetting/layer/project serialization
above is complete and can be emitted and golden-checked against these samples
now.
