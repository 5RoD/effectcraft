# Native motion tools

These original EffectCraft tools cover useful counter, quick animation, and shake workflows. They
do not load commercial After Effects plug-ins or include their preset packs. They are a first set
of native tools, not a claim of complete Digit Fiddler, KeyFast, or Shake Sauce parity.

## Animation actions

Select one or more layers and choose **Animation → Native Motion Tools → Animation Action…**.
Choose Fade, Slide, Scale, Rotate, or Trim Paths In/Out, a start and duration, easing, and optional overshoot.
Overshoot is available for Slide, Scale, and Rotate; opacity fades stay within their usual range.
Slide uses the chosen X/Y/Z axis and pixel offset; Z requires a 3D layer. Scale's endpoint is a
percentage of its original scale, and Rotation uses an angle offset. Incoming actions settle at
the property's original value at the interval's end; outgoing actions begin at its original value
at the interval's start.

Trim Paths requires shape layers. It reuses the first Trim Paths operator in the
layer's root Contents, adding one when missing. Trim In animates End from Start
to its original endpoint; Trim Out animates End back to Start. Other controls,
nested groups, and keys outside the chosen interval stay intact. Start's value
is sampled at the hidden endpoint; enabled expressions on Start or End are
rejected. Overshoot is unavailable for Trim. Operator creation and keyframes
share the same undo step, and portable settings create the operator on a new
destination layer too. Agents use `action: "trimIn"` or `"trimOut"`.

Layer staggering follows Timeline order, top to bottom, or bottom to top when reversed. Start,
duration, stagger, and overshoot times snap to composition frames, including fractional frame
rates. Separated Position dimensions and reversed/stretched layers are supported. Locked layers
are skipped. Every batch is one named undo step; invalid settings leave the entire project alone.

The result is ordinary keyframes. Keys inside the requested interval are replaced; keys outside
it remain. An enabled expression on a target property gives an error because it would override
the new animation. Generated properties and keys are selected for editing or **Save Animation
Preset…**, which reuses EffectCraft's existing portable `.ecpreset` format.
These are absolute keyframe snapshots: applying one replaces the target property's entire key
list. To regenerate relative motion around a different layer's existing animation, use the
Native Animation Action dialog's **Save Settings…** and apply that `.ecpreset` at its playhead.

Agents use `anim.nativeAction` with optional `layers`, `layer`, `comp`, `action`, `time`, `duration`,
`stagger`, `reverse`, `axis`, `distance`, `scalePercent`, `angle`, `overshoot`, and `ease`. A `curve`
object may replace the named easing preset. The optional `amount` overrides Scale's percentage or
Rotation's angle for automation.

```json
{"action":"slideIn","axis":"x","distance":-240,"duration":0.6,"stagger":0.1,"ease":"Expo Decelerate","overshoot":8}
```

## Seeded shake

Choose **Animation → Native Motion Tools → Seeded Shake…** for reproducible Position, Rotation,
and Scale deviations. Set the range, per-axis pixel amplitudes, rotation in degrees, scale in
percentage points, frequency, decay, and nonnegative integer seed. Zero decay sustains the shake;
higher decay settles it sooner. The first and last frames keep the original values, and short
ramps at each end avoid an abrupt jump.

Shake is added to the existing keyframed motion and stored once per composition frame as editable
keys. Reopening does not require a noise generator or expression runtime. The same layer,
original animation, settings, and seed produce the same result. Reapplying adds another shake;
undo first to regenerate it from the same original motion. Save the selected properties through
**Save Animation Preset…** to share an absolute keyframe snapshot. The Seeded Shake dialog's
**Save Settings…** stores parameters that add the shake around the destination's original motion.
Locked layers are skipped when applying animation presets; an all-locked target is rejected
before changing the project, history or recent-preset preferences.

Agents use `anim.nativeShake` with `layers`, `layer`, `comp`, `time`, `duration`, `frequency`,
`decay`, `x`, `y`, `z`, `rotation`, `scale`, and `seed`. One command may produce at most 200,000
keys; larger requests return an actionable error instead of an unbounded allocation. Frequency
above the composition's sampling rate can alias, so use a sufficiently high frame rate for rapid
shakes. Camera Position/Rotation are ordinary eligible properties; no proprietary camera rig is
created.

## Portable native settings presets

The Native Animation Action and Native Seeded Shake dialogs offer **Save Settings…** in their
fixed footer. This saves their parameters as an ordinary `.ecpreset` file without applying an
animation. Open it with **Animation → Apply Animation Preset…**, **Recent Animation Presets**,
or `anim.applyPreset {path, layers?}`. It runs at the destination playhead, against the destination
layers' current animation. Locked layers are skipped; an all-locked selection fails. Generated
keys replace only the chosen interval, retain outside keys, and create one native undo step.

A native settings file stores duration, action/axis/offset/easing or shake amplitudes/frequency/
decay/seed. Named easing is resolved to the actual curve when saving, so the receiving machine
does not need the same personal curve library. It never stores source layer ids, a composition
id, or the source playhead. Seeded noise remains deterministic for the same target layer,
settings and original animation; different destination layers use their own noise streams.

The hidden agent command is:

```json
{"command":"anim.saveNativePreset","params":{"path":"Shake.ecpreset","command":"anim.nativeShake","settings":{"duration":1,"frequency":8,"x":20,"y":20,"rotation":2,"seed":42}}}
```

The extension is a versioned parameter payload, not arbitrary command replay:

```json
{"effectcraftAnimationPreset":1,"name":"My Shake","nativeMotion":{"version":1,"command":"anim.nativeShake","settings":{"duration":1,"frequency":8,"decay":3,"x":20,"y":20,"z":0,"rotation":2,"scale":0,"seed":42}}}
```

Only the two native commands and their documented settings are allowed. Unknown fields,
unsupported versions, source-context fields, malformed scalars, unsupported curves and mixed
native/absolute payloads are rejected before an edit or recent-preset preference update.
Names are limited to 128 characters, saved native files to 64 KiB, paths to 32,768 bytes and
explicit target batches to 4,096 layers; existing duration/key/work checks still apply.
Preset files have an outer read limit of 128 MiB, allowing larger absolute keyframe snapshots.
The bundled desktop host enforces this while reading, including files that grow after their
size check; the web host checks before copying from its file store. Native settings files
also enforce the smaller 64 KiB limit after identifying the payload. Custom file-service
implementations should override `Services::read_file_limited` to bound allocation during reads.
The source form's Start field is intentionally omitted when saving. Native Action easing is
stored by value; no commercial preset data is bundled.

**Save Animation Preset…** retains its existing absolute keyframe snapshot semantics. Use
**Save Settings…** for a shake/action that should regenerate relative to another animation.
These changes still require the repository gate and a desktop save/reapply check before release.

## Counter formatting

### Ordinary text layers and installed fonts

Select one or more ordinary text layers, then choose **Animation → Native Motion Tools →
Text Counter…**. This original tool installs **Native Text Counter** controls and a small
EffectCraft Source Text expression, `ecTextCounter()`. Choose the font family/style, size, fill,
stroke, alignment, and tracking through the existing **Character** and **Paragraph** panels;
installed fonts use the normal font picker. Existing text animators, masks, layer transforms,
3D text, and motion blur keep using the ordinary text renderer.

Animate **Native Text Counter → Value** as the single number. Other controls set decimal places,
minimum integer digits, grouping, prefix/suffix, and Number or Time formatting. Time offers
HH:MM:SS, MM:SS, or Seconds, with at most six fractional places. **Use Layer Clock** adds elapsed
layer time to Value, with Clock Start, Clock Rate, and Count Down. Clock Start is the layer-time
origin of elapsed time, rather than an initial displayed value. For a 60-second countdown,
choose Time, Value `60`, Use Layer Clock, Count Down, Clock Start `0`, and Clock Rate `1`.
Layer start time, stretch, and reverse affect clock progression and Value keyframes consistently.
Number format can also use the layer clock for a constant-rate count.

**Digit width** keeps current styling by default. **Tabular digits** requests the
font's OpenType `tnum` feature; **Proportional digits** requests `pnum`. The choice
applies to existing Source Text documents, style runs and styling keyframes in the
same undo step as installation, and survives project save/reopen. The selected font
must support the requested feature. This is not a font-independent fixed-width
layout mode; prefixes, punctuation and the total number of digits can still alter
the layer's width. You can change the setting later in Character → OpenType.

Installing on multiple unlocked text layers is one undo step. Conflicting Source Text expressions
(including disabled saved expressions), malformed text values, and duplicate counter rigs are
rejected before any layer changes. Locked layers are skipped. Existing Source Text keys remain
and can animate styling. As with ordinary string expressions, generated text uses the source
document's first character style and paragraph settings; distinct per-character style runs are
not remapped onto changing digits. Use text animators for selective digit styling or animation.
The native helper identifies the control by its stable match id, so effect renaming/reordering
and layer copying preserve the rig. If multiple controls are added manually, the helper uses
the first Native Text Counter; the installer prevents creating that ambiguous setup.

Agents install the rig with `text.nativeCounter` and optional `layers`, `layer`, `comp`, `value`,
`format` (`number` or `time`), `decimals`, `padding`, `grouping` (`none`, `comma`, `period`, `space`),
`prefix`, `suffix`, `currentTime`, `countDown`, `clockRate`, `clockStart`, and `timeStyle` (`hms`,
`ms`, `seconds`), and `figureWidth` (`preserve`, `tabular`, `proportional`). The response returns the layer, effect uid, Value uid, and its `@uid` property
path, ready for `prop.set` and keyframe commands. Controls and the expression save in the project
and branching undo history. The expression helper is an EffectCraft extension, not a portable
After Effects script or a commercial plug-in.

```json
{"command":"text.nativeCounter","params":{"value":1234.5,"decimals":2,"grouping":"comma","prefix":"$","suffix":" USD"}}
```

Prefix/suffix are each bounded to 256 Unicode characters; hostile evaluated precision/padding
values are bounded before formatting. This workflow supports Unicode affixes through normal
font shaping and fallback, with explicit separator choices. Full locale/currency conventions,
rolling odometer transitions, and commercial Digit Fiddler feature parity remain unimplemented.

### Numbers effect

Apply **Effect → Text → Numbers** and open its **Format** controls. Animate **Value/Offset/Random
Max** as the single counter value. New controls add:

- Prefix and suffix text, minimum integer digits, and explicit separator styles `1,234.56`,
  `1.234,56`, and `1 234.56`.
- Count Down, Clock Rate, and Clock Start when Current Time/Date is enabled.
- HH:MM:SS, MM:SS, or Seconds with optional fractional time for the Time type.
- Left/Center/Right anchoring. Disable Proportional Spacing and set Minimum Integer Digits to keep
  the digit width stable across a known counter range.

CPU and GPU rendering use the same formatting and glyph coverage. Existing projects keep their
defaults. Numbers still uses its original built-in ASCII stroke font; use the ordinary text rig
above for installed fonts, Unicode affixes, and text animators. Full language-specific localization
and unsupported currency glyphs are not provided by the Numbers effect. Prefix/suffix rendering is bounded to 256 characters
each so malformed files cannot create unbounded glyph lists per frame.

## Animated Timewarp Speed

Timewarp Speed now accumulates source time from layer time zero. A linear ramp from
50% to 150% over two seconds reaches source time 0.75 seconds at output time one
second; multiplying the latest 100% speed by the entire elapsed second incorrectly
reached source time one second. Held zero/negative speeds freeze or reverse from
the accumulated position instead of jumping back to the beginning. Source Frame
remains an explicit frame-position curve.

Linear, Hold, Bézier and auto-Bézier speed curves use prepared cumulative areas.
Steady queries search their key intervals rather than replaying prior frames. The
renderer caches prepared curves against weak immutable property-tree identities,
with a 16 MiB/512-entry bound; edits invalidate the identity, and cached curves do
not retain old project trees. Motion-blur samples integrate the speed at each
output sample time, including rate changes inside the exposure. Constant and
single-key rates retain the existing exact Tick transport.
Keyed constant 100%, -100% and 0% rates also use integer identity, checked negation
and zero paths. Retimed values are checked against the supported Tick range after
rounding, including the asymmetric positive/negative endpoints.

When every retimed sample is outside a nested composition, Timewarp now returns
transparent pixels using the current source's validated geometry. It no longer
silently substitutes the current frame. CPU and GPU use the same prepared samples;
source-range behavior is consistent with the renderer's existing precomp bounds.

Speed expressions use bounded adaptive numerical integration in layer time.
Smooth constant, linear and polynomial rates are supported; failed, non-finite
or non-converging expressions return a render error with context. This numerical
path has an evaluation budget and does not establish exact integration for every
possible expression or discontinuity. Source Frame provides an explicit mapping
when a rate expression exceeds that budget. Changes to the renderer salt prevent
older disk frames from masking the corrected behavior.

Tests cover analytic areas, overlapping easing influences, seeks in different
orders, freeze/reverse ramps, property edits, weak-cache lifetime, native expression
timing and shutter changes. The `integral_bench` example measures preparation and
lookup cost separately; it does not measure decoding, optical flow or full playback.
This fixes a native retiming behavior, not commercial Twixtor algorithm compatibility
or complete After Effects Timewarp fidelity (including its fractional-frame phase).

## Precise easing curves

**Window → Ease Presets** edits a working curve without changing project keyframes. The four
numeric fields are the normalized Bézier handles **x1, y1, x2, y2**; commit a number with Enter
or by leaving its field. Values retain full floating-point precision rather than the rounded
influence/speed summary above them.

The text field accepts `cubic-bezier(0.4, 3, 0.6, -2)`, a four-number array, or four comma-separated
numbers. Press Enter or **Use Curve** to load it. **Copy Curve** copies the current valid curve;
**Paste Curve** reads the desktop clipboard. When clipboard reading is unavailable, paste into
the field with the platform paste shortcut and commit it there. Invalid input shows an error
and retains the last usable curve. Rejected clipboard input also retains the previous text;
an invalid typed draft stays available for correction.

The existing engine supports x1 from `0.001` to `1`, x2 from `0` to `0.999`, and relative endpoint
speeds from `-100` to `100`. Valid CSS curves outside those engine bounds are reported as
unsupported rather than silently altered. Overshoot within the supported range is allowed.
The graph fits both handles, including large overshoot, and keeps one scale throughout each
pointer drag; it fits the result again after release. All editors and library controls share
the panel's vertical scroll area.

**Apply** changes the selected editable keyframe pairs in one undo step. Capture, named presets
and portable library import/export use the same engine curves. These are original editing
controls, not commercial Flow binary or preset compatibility. The new controls require the
combined repository gate and desktop validation before release acceptance.

## Remaining requested workflows

Flow-style curve editing and libraries use the Ease Presets panel. Applying a curve skips
locked layers in a mixed selection and creates one undo
step for the editable pairs. An all-locked selection returns an unlock instruction without
changing the project or history; read-only curve capture remains available. Native Action and
Seeded Shake share portable parameters through `.ecpreset`, Apply Animation Preset and Recent
Animation Presets. Native Trim Paths actions now cover layer-level draw-on/off;
KeyFast-specific modifier shortcuts remain. No commercial
preset pack is bundled. Deep Glow look/speed parity and Twixtor-quality optical-flow retiming
need independent rendering implementations and measured validation; existing effect names do
not prove those capabilities.

## Small production workflow acceptance

The clean Windows production build of `9e2d1e57` passed38 additional real-app
checks using original synthetic projects. They cover locked-key read-only
capture and atomic Apply rejection, exact saved animation keys/values/geometry
and history, Timewarp Source Frame edits with undo/redo and abandoned branches,
save/reopen renders, and actual History panel clicks restoring both branches of
a counter edit. The earlier counter/easing/action/shake run passed18 checks.

All eight easing controls remain legible and reachable in the default short
dock and a maximized640×480 panel; both graph handles remain editable after
scrolling. The screenshots were inspected. The small motion roundtrip retained
exact document/key/history state and alpha; its rendered PNG differs by one
green code at one of921,600 pixels, within the existing display bound. Counter,
easing and retime save/reopen PNGs were byte-identical in these cases.

This accepts the scoped editing and persistence workflows. It does not establish
full commercial plug-in equivalence, optical-flow quality, full-resolution
retiming speed or that every panel fits simultaneously in a640×480 window.
