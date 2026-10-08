# Native motion tools

These original EffectCraft tools cover useful counter, quick animation, and shake workflows. They
do not load commercial After Effects plug-ins or include their preset packs. They are a first set
of native tools, not a claim of complete Digit Fiddler, KeyFast, or Shake Sauce parity.

## Animation actions

Select one or more layers and choose **Animation → Native Motion Tools → Animation Action…**.
Choose Fade, Slide, Scale, or Rotate In/Out, a start and duration, easing, and optional overshoot.
Overshoot is available for Slide, Scale, and Rotate; opacity fades stay within their usual range.
Slide uses the chosen X/Y/Z axis and pixel offset; Z requires a 3D layer. Scale's endpoint is a
percentage of its original scale, and Rotation uses an angle offset. Incoming actions settle at
the property's original value at the interval's end; outgoing actions begin at its original value
at the interval's start.

Layer staggering follows Timeline order, top to bottom, or bottom to top when reversed. Start,
duration, stagger, and overshoot times snap to composition frames, including fractional frame
rates. Separated Position dimensions and reversed/stretched layers are supported. Locked layers
are skipped. Every batch is one named undo step; invalid settings leave the entire project alone.

The result is ordinary keyframes. Keys inside the requested interval are replaced; keys outside
it remain. An enabled expression on a target property gives an error because it would override
the new animation. Generated properties and keys are selected for editing or **Save Animation
Preset…**, which reuses EffectCraft's existing portable `.ecpreset` format.

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
**Save Animation Preset…** to reuse or share the result.

Agents use `anim.nativeShake` with `layers`, `layer`, `comp`, `time`, `duration`, `frequency`,
`decay`, `x`, `y`, `z`, `rotation`, `scale`, and `seed`. One command may produce at most 200,000
keys; larger requests return an actionable error instead of an unbounded allocation. Frequency
above the composition's sampling rate can alias, so use a sufficiently high frame rate for rapid
shakes. Camera Position/Rotation are ordinary eligible properties; no proprietary camera rig is
created.

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
`ms`, `seconds`). The response returns the layer, effect uid, Value uid, and its `@uid` property
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

## Remaining requested workflows

Flow-style curve editing and libraries use the existing Ease Presets panel; this work does not
duplicate it. KeyFast-style Trim Paths macros, dedicated action libraries and modifier shortcuts
remain. Shake preset sharing uses editable animation presets, not a separate settings library or
the vendor's preset pack. Deep Glow look/speed parity and Twixtor-quality optical-flow retiming
need independent rendering implementations and measured validation; existing effect names do
not prove those capabilities.
