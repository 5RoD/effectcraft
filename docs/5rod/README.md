# EffectCraft for 5RoD — development status

This is a Windows development fork of EffectCraft 0.6.0. The requested complete After Effects
experience is the target; this build is not an accepted replacement yet. The existing installed
application and Desktop shortcut have not been removed or replaced.

## Changes in this milestone

- Project files and recovery checkpoints include undo, redo and abandoned branches. The History
  panel can revisit them after reopening. History is retained without the old 32/99 edit cap by
  default; continuous gestures still form one undo step. Unchanged items and layers are shared in
  the archive, and changed values use lossless structural patches.
- Recovery checkpoints default to 30 seconds and run in the background on desktop. A failed write
  does not advance the recovery pointer or rotation slot. The timer does not wait for an unfinished
  writer. A checkpoint that finished immediately before a crash is found even if its sentinel was
  not yet updated. Returning to saved pixels still prompts to preserve newly created redo history.
- A dark navy, white, red/pink and cyan theme follows the channel's visible palette. The theme is
  selectable as `5rod`. This modified fork uses an original geometric EffectCraft mark and removes
  the upstream trademark artwork; upstream copyright and open asset notices remain.
- Full-opacity styled layers can composite directly when channels, mattes and knockout permit it,
  removing one full-frame clone and interpolation pass. This is a targeted CPU-render change;
  broad performance claims require the measurements below.
- Layer property trees share storage between project snapshots and detach on mutation. Editing
  one layer keeps every unaffected layer's property tree shared, including restored history.
  The serialized property-tree representation is unchanged.
- Find focuses the active Project/Timeline search field. Timeline shortcuts respect panel focus.
  Ctrl+Alt+B sets the work area to the selected layers, or the whole comp when none are selected.
  Ctrl+Alt+Shift+N remains New Folder, as in Adobe's published shortcut reference.
- The Ease Presets panel can import/export a versioned library of user-created Bézier curves.
  Import validates the whole library before replacing preferences. Commercial Flow presets are
  not bundled.
- Camera Settings exposes linked aperture/F-Stop controls using the engine's existing lens model.
- Native animation actions add editable Fade/Slide/Scale/Rotate keyframes with easing, overshoot,
  layer staggering and one undo step. Seeded shake adds reproducible transform motion to existing
  animation. Both validate a complete batch before changing it. Numbers adds counter affixes,
  padding, separator styles, countdowns, fractional time and anchoring. Text Counter installs an
  animatable counter on ordinary text layers, retaining installed fonts, styling and text animators. See
  [native motion tools](../native-motion-tools.md) for controls and limitations.
- Effect-control upgrades preserve archived undo/redo and abandoned states. Reopening upgrades
  each shared composition once and keeps unchanged composition snapshots shared.
- Dust & Scratches compares CPU/GPU thresholds on a common 20-bit grid with a one-unit boundary
  margin, preventing a sub-ULP rounding difference from switching an entire output pixel. The
  maximum boundary adjustment is less than 1/2000 of an 8-bit contrast step. Increment and Save
  handles oversized numeric filename suffixes without integer overflow.
- Glow avoids per-pixel heap allocation for Arbitrary Map colors and redundant GPU texture
  clears. CPU and GPU share a validated radius/padding plan that bounds hostile evaluated
  values before integer conversion. See [Glow bounds](../glow-safety.md) for the 4096-rendered-pixel
  cap at unusually large output/vector scales.

## Compatibility and persistence limits

The `editHistory` extension is versioned and optional. The current composition remains a normal
`.ecproj` project. Original EffectCraft builds can read that composition, but **saving with an old
build discards the history extension**. Keep a copy when exchanging files with other versions.
Malformed history produces a warning while the valid current project still opens. XML project
copies made by this fork carry the extension too. New counter rigs require this fork's expression
helper and controls; older builds do not render their generated digits correctly.

Recovery checkpoints are not a per-edit durable journal. A crash can lose changes made since the
latest successful checkpoint, and no software can promise protection against all disk or hardware
failures. An append-only edit journal, persistent revision IDs, crash-boundary fault injection and
recovery diagnostics remain required for the stronger requested saving behavior.

Unlimited retained snapshots can use substantial memory in long projects. Shared layer-property
trees and archive storage reduce duplication but do not provide lazy loading of old in-memory
composition versions. See the measured long-history case in [performance](performance.md);
disk-backed navigation remains required. History nodes'
automation IDs are regenerated when reopening; the saved topology and project states are retained.

## Plug-in and After Effects scope

The requested Digit Fiddler, Flow, KeyFast, Deep Glow, Shake Sauce and Twixtor are third-party products
for After Effects. This fork does not load their binaries, scripts or proprietary presets. Original
native workflow tools now cover the counter, easing-library, quick-transform and seeded-shake
workflows described above; effect names alone are not quality parity. The integrated tools,
including font-based text counters, pass the repository gate. Native Bloom is being developed
separately; its checked renderer is not exposed as a usable effect in this milestone.

Full UI/shortcut parity, live camera preview, difficult-footage camera tracking, compositing/color
fidelity, optical-flow retiming, glow quality/performance and codec/export acceptance remain open.
See the [AE behavior audit](after-effects-parity.md), [plug-in workflow audit](plugin-parity.md),
[camera acceptance matrix](camera-acceptance.md), proposed [Native Bloom renderer](../native-bloom-design.md),
and the inherited [gap assessment](../gaps.md).

## Validation

The complete repository gate is `cargo xtask ci`: formatting, clippy, workspace tests, dependency
layering, licensed assets and WebAssembly checks. The integrated run passed on 2026-10-09 (local date):
2,342 tests passed, 26 explicitly optional tests were ignored, all 30 assets were attributed,
dependency layering passed, and 32 crates passed the WebAssembly check. Formatting and native
clippy with warnings denied also passed. The WebAssembly check reports one inherited dead-code
warning in `effectcraft-media::pool::sample_time`.

The run found and corrected a duplicate shortcut, missing Japanese menu entries, an assumption
about variable-font advances, and timing-dependent test expectations. The viewer GPU test now
uses the desktop application's device policy instead of preferring software adapters; it passes
on the RTX 3050 Vulkan device and compares viewer output with the CPU renderer.

Windows validation uses release optimization with Thin LTO disabled for the large test suite;
the production build and comparable performance measurements use the repository's normal release
profile. The measured host is an i9-10900F, 32 GB RAM and RTX 3050. Screenshots were generated from
the real UI with the original procedural demo and inspected after rendering. The counter dialog
was also inspected at 1680×1020 and 640×480; its action buttons remain visible while the fields scroll.
The normal production desktop executable launched on the RTX 3050 Vulkan device, reopened the
counter project, rendered its animated digits through the control API, and exited cleanly using
an isolated configuration folder. [Performance results](performance.md) report the measured scope.

Before replacing the installed app: all required workflows must be accepted, production binaries
must pass launch/reopen/render checks, existing projects and preferences must be backed up, and the
new installation must have a tested rollback path. Installation is not performed by this milestone.
