# After Effects parity research for 5RoD

Research snapshot: 2026-10-08. This document uses Adobe's public user documentation and EffectCraft's public project documentation as behavior references. It does not treat Adobe's code, artwork, icons, presets, or project files as implementation material. Follow the repository's clean-room and asset rules in [AGENTS.md](../../AGENTS.md).

The tables below record the upstream baseline. This fork subsequently added panel-scoped dispatch
with active-panel precedence, contextual Project/Timeline Find, the selected-layer work-area
shortcut, easing-library import/export, linked camera F-Stop controls, persistent branching history
and native motion tools. See [development status](README.md) for implementation and validation.
Adobe assigns Ctrl+Alt+Shift+N to both Project New Folder and viewer splitting in their respective
contexts. This fork preserves New Folder; matching viewer splitting still needs implementation.

## What the current project already covers

EffectCraft is already a broad After Effects-style compositor: its README reports 306 effects, text and shape animation, expressions, Classic and Advanced 3D, tracking, a render queue, and native and browser builds. The project’s own [gaps assessment](https://github.com/storytold/effectcraft/blob/main/docs/gaps.md) makes an important distinction: feature breadth is estimated above 95%, while professional real-world readiness is estimated at 30–50%. That lower estimate reflects missing `.aep`/`.aepx` import, unmeasured behavior fidelity, unbenchmarked large-project performance, incomplete third-party plug-in compatibility, and uneven cross-platform polish. The [roadmap](https://github.com/storytold/effectcraft/blob/main/ROADMAP.md) prioritizes fidelity, reliability, stability, project import, and performance measurement ahead of more feature breadth.

“All of After Effects” is a long-term compatibility program, not a small feature sprint. AE includes decades of edge cases, extensive third-party plug-ins and scripts, numerous media and project formats, and workflows that depend on context-specific UI behavior. The credible route to a better user experience is to measure representative workflows, close high-impact gaps, and publish what is supported.

## Practical parity matrix

| Priority | Area | AE behavior to preserve | Current EffectCraft evidence and practical direction |
|---|---|---|---|
| P0 | Performance | Multi-frame rendering, responsive playback, RAM/disk caches, idle frame caching, render-time profiling, proxies, and pre-rendering. | EffectCraft already has parallel/GPU compositing, cache controls, proxies, and a render queue. Its gaps assessment says performance has not been compared with AE and large real projects have not been tested. Establish repeatable 1080p/4K projects with many layers, nested comps, long footage, and heavy effects; measure UI latency, warm/cold frame time, cache hit behavior, peak RAM/VRAM, preview fill and export throughput on the same reference machines as AE. Optimize measured bottlenecks and preserve the benchmark as a regression gate. |
| P0 | Behavior fidelity | Actions and results match the focused panel, selection, playhead, layer state, renderer, and composition settings. | Broad feature presence does not demonstrate AE behavior. Build a clean-room corpus of original scenes and compare property values and rendered frames against AE. Cover keyframe interpolation, effects, masks/mattes, text, expressions, 3D, motion blur, and time remapping. Record fidelity by feature rather than one aggregate “parity” number. |
| P0 | Reliability | Import, arrange, animate, preview, and render work without losing project state on Windows, macOS, and Linux. | The Oct. 8 gaps audit lists many user-found interaction and platform issues as fixed, with multi-monitor support and some platform limitations still outstanding. Verify the fixes with scripted real input on Windows and cross-platform smoke workflows, especially docking, viewer selection, drag/drop, keyboard focus, GPU loss, low memory, and render queue menus. |
| P0 | Existing projects | Open existing compositions with media links, nested comps, effects, expressions, and keyframes intact. | EffectCraft cannot currently open `.aep` or `.aepx`; the repository calls this the largest switching barrier. Define the clean-room scope and compatibility boundary, then implement import with a representative real-world-shaped project corpus. Preserve unknown constructs as recoverable warnings/placeholders where possible. |
| P1 | Interface and workspaces | Dock, tab, group, float and rearrange panels; save task workspaces; move panels across monitors; expose panel-specific commands. | EffectCraft already has a large AE-like menu tree and workspaces, but the project audit records panel rearrangement and multi-monitor support as gaps. Test the whole drag interaction model and saved-layout recovery at common resolutions and display scales. Avoid copying Adobe artwork; generic original controls are sufficient. |
| P1 | Shortcuts | Match Windows defaults, distinguish app-wide from active-panel bindings, make the active panel visible to conflict detection, and allow custom sets. | EffectCraft has a visual shortcut editor, presets, conflict reporting, and many familiar defaults. The source audit below found panel-scope behavior and several high-frequency bindings that need correction or review. |
| P1 | Animation/timeline | Keyframes, hold/linear/Bezier variants, temporal and spatial interpolation, value/speed graphs, editable motion paths, roving, markers, layer timing, property reveal/add shortcuts. | Most of this is implemented. The main remaining value is AE comparison on real use cases: graph handles and units, hold-keyframe transitions, spatial tangents, frame stepping, and interactions with nested/time-remapped layers. |
| P1 | Compositing and roto | Alpha/luma track mattes with inversion, masks and mask modes, blend modes, parenting, adjustment layers, tracking, roto and edge refinement. | The repository reports broad coverage, including optional learned models for Roto Brush and face tracking. Compare outputs and editability with AE and clearly expose model availability. Improve quality and behavior against references before expanding the feature checklist. |
| P1 | Text and shapes | Shape paths, nested groups, fills/strokes, nondestructive path operators, animated text source, character/range/wiggly/expression selectors, per-character transforms and text-on-path. | The project reports shape operators, text animators, selectors, path text, and per-character 3D. Validate operator ordering, shape copy/paste, direct path editing, variable fonts, font fallback, and text layout against reference examples. |
| P1 | Effects and plug-ins | Search and apply bundled effects/presets; edit effect parameters and on-canvas controls; support user extensions used in real projects. | EffectCraft advertises all 298 AE effect names plus additional effects, with many GPU-backed. Third-party AE binary plug-ins cannot run. Compare effect output and parameter behavior; grow the original Wasm plug-in API and prioritize common use cases such as particles, glows, and 3D objects. |
| P1 | Preview/render/export | Preview controls for range, frame rate, resolution, skip, audio, cache and loop; queue multiple jobs with render/output templates, progress, logs, and post-render actions. | EffectCraft already has an AE-style render queue, templates, logs, and broad output support. Its gaps assessment still flags codec efficiency and unverified camera/phone formats. Verify format interoperability, color/alpha handling, frame/audio synchronization, recovery from disk-full/device-loss conditions, and throughput on representative exports. |
| P2 | 3D | Classic/Advanced renderers, 3D layers, cameras/lights, material properties, shadows, depth of field, model import and useful scene navigation. | EffectCraft reports both renderers, camera/light tooling, and model import. The project audit calls out remaining viewer/layout and fidelity work. Use complex scenes to test cameras, nested comps, material response, shadows, 3D mattes, and render performance. |
| P2 | Audio | Audio-aware preview, waveform display, meters, audio levels and effects, and synchronized video/audio playback. | EffectCraft reports synchronized preview, meters, waveforms, and audio effects. Preserve frame pacing while audio plays and compare audio-driven effects and keyframe conversion. |
| P2 | Accessibility and localization | Keyboard navigation, readable contrast, screen-reader support, localized controls and documentation. | EffectCraft’s gaps audit says accessibility work is absent and localization is limited. Define keyboard-only, screen-reader, contrast, and system-language acceptance checks before claiming parity for these workflows. |

## Shortcut audit: Windows defaults

Adobe’s [official keyboard-shortcut reference](https://helpx.adobe.com/after-effects/desktop/get-started/keyboard-shortcuts/keyboard-shortcuts-reference.html) is the source of truth. The following review compared that reference with `crates/engine/src/shortcuts.rs`, command specs in `crates/engine/src/commands/`, and the UI command and dispatch code in `crates/ui-egui/src/menus.rs`. This is a source audit, not an interactive verification in the running app.

EffectCraft stores `Cmd` as the cross-platform primary modifier and translates it to Ctrl on Windows. Runtime dispatch now filters front-end Composition bindings to viewer panels and Timeline bindings to Timeline or viewer panels, with panel bindings ahead of application bindings. Engine/menu commands still generally use `Application`; those commands and the wider Timeline-to-viewer allowance need further context review. Adobe documents both application and panel-specific shortcuts, with panel shortcuts taking precedence while that panel is active. The [camera navigation increment](camera-acceptance.md#5rod27-navigation-increment) adds `C` cycling and camera-tool-specific `F` / `Ctrl+Shift+F` without taking Timeline's Feather binding.

| Impact | Observed binding or behavior | AE Windows reference | Assessment |
|---|---|---|---|
| High | `app.find` is `Ctrl+F`; it writes the Project panel search and opens Project. `timeline.search` has no default. | `Ctrl+F` means Find in the Project panel when Project is active and Find in Timeline when Timeline is active. | Contextual mismatch: Timeline `Ctrl+F` cannot search Timeline; it opens Project search. Add a Timeline binding/behavior and make matching panel-specific. |
| High | `file.close` is `Ctrl+W` and runs `comp.close`; `window.closePanel` has no shortcut. | `Ctrl+W` closes active viewer/panel content; `Ctrl+Shift+W` closes the active panel or all viewers of that type. | `Ctrl+W` closes the active comp regardless of which panel has focus; implement the documented focus-dependent close behavior, and add the `Ctrl+Shift+W` panel-close binding. |
| High | `comp.new` is `Ctrl+N`; `comp.settings` is `Ctrl+K`. | `Ctrl+N` creates a composition; `Ctrl+K` opens Composition Settings. | Aligned in the engine command specs and the UI routes both commands to the matching dialogs. Keep this explicit in the Windows shortcut acceptance check. |
| High | `window.panel` is a generic unbound command. | `Ctrl+0` Project, `Ctrl+Alt+0` Render Queue, `Ctrl+1` Tools, `Ctrl+2` Info, `Ctrl+3` Preview, `Ctrl+4` Audio, `Ctrl+5` Effects & Presets, `Ctrl+6` Character, `Ctrl+7` Paragraph, `Ctrl+8` Paint, `Ctrl+9` Brushes; `F3` or `Ctrl+Shift+T` opens Effect Controls. | Common panel navigation bindings are absent. Add panel-specific commands/bindings and ensure they select/show the matching panel. |
| Medium | `Alt+Shift+N` invokes `view.newViewer`. | `Ctrl+Alt+Shift+N` splits the active viewer frame and creates a viewer with opposite locked/unlocked state. | Modifier mismatch and behavior appears narrower. Review whether the command is intended to match split-frame New Viewer; if so, correct the shortcut and behavior. |
| Medium | No shortcut is bound to `comp.workArea` for set-to-selected-layer duration. | `Ctrl+Alt+B` sets the work area to the selected layers' duration, or the full composition when none are selected. | Missing common work-area command. Add a focus-aware binding while retaining `B` and `N` for work-area start/end. |
| Medium | `file.newCompFromSelection` has no default shortcut. | `Alt+\\` creates a new composition from the selection. | Missing common composition workflow shortcut. |
| Medium | `Shift+F10/F11/F12` are not exposed as workspace defaults. | Switch to a workspace. | Missing high-value workspace cycling defaults. |
| Medium | `Alt+Shift+,` / `Alt+Shift+.` panel cycling is absent. | Cycle panels in the active frame; `Shift+,` / `Shift+.` cycles items in the active viewer. | Missing navigation shortcuts. Implement with focus-aware panel/viewer state. |
| Medium | `app.find` and `timeline.search` both lack distinct panel scopes; most engine commands are labeled Application. | Adobe explicitly separates app-wide and panel-specific commands and lets a panel shortcut override an app shortcut in that panel. | Fix scope modeling and runtime dispatch before promising shortcut parity. Conflict checks and execution should use the same scope rules. |
| Medium | `Alt+Shift+P/S/R/T/A/E/M/F/L/U` exist for add/reveal properties in UI commands; reveal defaults include `P/S/R/T/A/E/M/F/L`; `U` is an engine command. | These property reveal and add-to-revealed-properties patterns are documented. | Broadly aligned. Verify double-press shortcuts (`AA`, `EE`, `FF`, `LL`, `MM`, `PP`, `RR`, `SS`, `TT`, `UU`) and timing under fast key repeat; add/reveal should respect Timeline focus. |
| Low | `view.res.quarter` uses `Ctrl+Alt+Shift+J`; `view.res.custom` has no shortcut. | `Ctrl+J`, `Ctrl+Shift+J`, `Ctrl+Alt+J` map Full, Half, Custom. | This is an extra Quarter option, not a direct AE equivalent. Consider assigning the documented third shortcut to Custom and keeping Quarter unbound, or explicitly document the deviation. |

Bindings that appear aligned include `Ctrl+S`, `Ctrl+O`, `Ctrl+Shift+S`, `Ctrl+I`, `Ctrl+Alt+I`, `Ctrl+N` new composition, `Ctrl+K` Composition Settings, `Ctrl+Y` solid, `Ctrl+Alt+Shift+Y` null, `Ctrl+Alt+Y` adjustment, `Ctrl+Shift+C` precompose, `Ctrl+Shift+D` split, `Ctrl+Alt+T` time remap, `Ctrl+Alt+R` reverse, `Ctrl+Shift+X` trim comp to work area, `F9` Easy Ease, `Shift+F3` Graph Editor, `F4` switches/modes, `B/N` work-area endpoints, `[`/`]` layer timing, `Alt+[`/`]` trim, `J/K` adjacent visible time items, and `V/H/Z/W/Y/Q/G` tool shortcuts. Tool availability and momentary-tool behavior still need review. Current Adobe documentation permits camera navigation in a 3D composition without a camera layer via the Default Camera; EffectCraft now supports that path.

Adobe’s shortcut editor documentation also says conflicts should be visible, panel commands may supersede application shortcuts while focused, and the UI can show assigned/available keys. EffectCraft already offers a shortcut editor and presets; acceptance should cover semantic parity and dispatch focus in addition to the visible key labels.

## Focused Windows shortcut gaps in the development fork

The follow-up audit identified these gaps and scoped camera improvements in the current source. Adobe's
[shortcut reference](https://helpx.adobe.com/uk/after-effects/desktop/get-started/keyboard-shortcuts/keyboard-shortcuts-reference.html)
is the behavioral reference. Source interaction tests use original small scenes; they do not
establish complete After Effects behavior or pixel fidelity.

| Workflow | Current gap | Acceptance check |
| --- | --- | --- |
| Temporary tools | Unmodified letter bindings for tool commands are spring-loaded: a quick tap latches, a hold or pointer use restores the previous tool and slot memory on release. Remapped letters use the configured bindings; Space keeps its separate Hand behavior. | Verify taps/cycles, held Hand/camera drags, overlapping keys in both release orders, explicit toolbar choices, typing, menus/dialogs and focus loss. The 300 ms tap cutoff is an EffectCraft choice and has not been measured against Adobe. |
| Camera navigation | C cycles Unified/Orbit/Pan/Dolly; 1/2/3 select individual tools. Unified uses left/middle/right buttons, and Default view navigation works without a camera layer. Cursor-centered interaction modes and parented camera transforms still need fidelity checks. | Verify camera/default-view gestures, one undo per camera drag, locked cameras, and orthographic constraints; then compare cursor modes and parenting with a behavioral reference. |
| Temporary camera navigation | Alt/Option-drag uses Unified navigation without changing the selected toolbar tool in a 3D composition. `threeD.altCameraNavigation` can disable it. Drawing, Zoom, Pan Behind and text tools preserve their existing alternate actions. | Verify all three buttons, early Alt release, selection preservation, Default view without document edits, Space priority, and disabling the preference. Alternate drawing-tool interactions still need a live reference. |
| Camera framing | Viewer F/Ctrl+Shift+F frame selected/all 3D content while a camera tool is selected; Timeline and non-camera contexts keep Feather commands. | Verify focus, camera-tool context, offscreen 2D exclusions, and selected/all 3D framing; compare framing distance and parenting with a behavioral reference. |
| 3D manipulators | In Composition/Layer panels, 4/5/6 choose Position/Scale/Rotation and V restores Universal. Original arrows, squares and rings support selected, unlocked 3D content. Local/World/View axes are available in the toolbar; narrow windows use a compact 3D menu. | Verify pointer movement, parent spaces, grouped undo, saved transforms and input ownership. This is functional coverage, without an Adobe geometry or visual-fidelity comparison. |
| Panel scopes | All Timeline bindings are allowed in Composition/Layer/Viewer panels. | Verify each panel-specific binding against Adobe's scope, with focused-panel bindings taking precedence over app bindings. |

The [3D preferences reference](https://helpx.adobe.com/in/after-effects/desktop/get-started/preferences-and-settings/preferences.html)
documents Alt/Option camera navigation as a configurable behavior. EffectCraft's new checkbox
is available in Settings ▸ 3D and through `prefs.get`/`prefs.set`; missing values in older
settings files use the enabled default. Scroll-wheel camera modes and the 1–6 preference
choices remain separate gaps.

The transform handles send one checked `layer.setTransforms3D` batch per drag step and share
one undo merge key. Selecting a parent and its child transforms the parent once. Position
converts world movement back into each parent's coordinates; Scale uses the layer's local
axes, with Shift for uniform scaling. Rotation writes Orientation while retaining authored
X/Y/Z Rotation, snaps to one degree (Shift: five degrees), and keeps fractional precision
with Ctrl/Command. A single layer's unchanged Position is retained, including its expression.
Changing the composition, time, selection, tool, view or axis space, or losing window focus, ends
the gesture. The controls do not read pixels back from the GPU or poll while idle.

Current limits: camera/light layers keep their existing camera navigation and property
controls; the new transform handles apply to 3D content layers. Auto-oriented rotation
requires disabling Auto-Orient. A rotation requiring shear under a nonuniformly scaled
parent fails with an actionable error before the batch changes the project. Singular or
cyclic parent chains and driven target properties likewise return errors. These are
remaining compatibility limits, not complete 3D parity.

The temporary letter-tool state is transient, capped at the 26 letter keys, and does not
poll or repaint while waiting for release. Tool gestures keep their document/view changes;
restoring the previous tool does not undo the work. The control channel's `ui.key` supports
separate `down` and `up` phases for the same behavior. Conflicting remaps still follow the
shortcut editor's normal conflict and panel-scope rules; changing a binding does not remove
another command's existing assignment.

Core J/K, I/O, Home/End, Page Up/Page Down navigation and F10–F12 3D view switching already
have bindings. Their presence is not evidence that every edge case matches Adobe.

## Practical acceptance checklist

- [ ] Record target AE release, Windows version, GPU, RAM/VRAM, monitor setup, and target workflows before scoring parity.
- [ ] Use original test projects and public documentation as the behavioral reference; keep any permitted AE captures and notes in the maintainer-local, gitignored `plan/aftereffects/ref/` only.
- [ ] Add at least one reference case each for composition settings, import/relink, layer timing, all interpolation kinds, graph edits, effects, expressions, masks, track mattes, text animators, shape operators, 3D, audio, and render/export.
- [ ] Compare sampled property values and output frames; publish a reproducible score per feature and note unsupported constructs.
- [ ] Benchmark small and large 1080p/4K projects in cold and warm cache states; report preview frame time, playback frame drops, cache fill, memory use, and final render time on named reference machines.
- [ ] Profile by layer/effect/cache stage; keep interactive operations responsive during render, preview, drag, scrub, and cache work.
- [ ] Verify high-frequency Windows shortcuts with Project, Timeline, Composition, text editing, dialogs, and shortcut editor each focused. Confirm panel-specific actions do not fire in unrelated panels and custom conflicts match runtime behavior.
- [ ] Check shortcuts on US and German keyboard layouts, including punctuation, brackets, `+`, `;`, `,`, `.`, `/`, and grave accent; verify Num Lock and missing numpads have sensible behavior.
- [ ] Run import → arrange → animate → preview → render flows with mouse and keyboard on Windows, macOS, and Linux. Include multiple monitors, display scaling, low memory, GPU-device loss, missing media/fonts, and disk-full errors.
- [ ] Verify save/undo/reopen and crash recovery around long renders and interruption-prone actions; malformed projects/media must produce actionable errors without losing the document.
- [ ] Clearly label unsupported project features, AE plug-ins, media formats, and expression/script limitations; do not claim exact AE compatibility until the corpus and acceptance evidence support it.
- [ ] Keep all branding original and comply with `AGENTS.md`; do not reuse Adobe artwork, UI screenshots, presets, templates, or fonts.

## Sources

Adobe primary documentation:

- [After Effects workflow overview](https://helpx.adobe.com/after-effects/desktop/get-started/understand-after-effects-workflow/workflows.html)
- [Workspaces, panels, and viewers](https://helpx.adobe.com/after-effects/desktop/get-started/get-familiar-with-the-interface/workspaces-panels-viewers.html)
- [Windows/macOS keyboard shortcut reference and shortcut editor](https://helpx.adobe.com/after-effects/desktop/get-started/keyboard-shortcuts/keyboard-shortcuts-reference.html)
- [Composition settings](https://helpx.adobe.com/after-effects/desktop/work-with-compositions/composition-settings/composition-basics.html)
- [Keyframe setup and selection](https://helpx.adobe.com/after-effects/desktop/animate-in-after-effects/animation-keyframes/setting-selecting-deleting-keyframes.html)
- [Keyframe interpolation](https://helpx.adobe.com/after-effects/desktop/animate-in-after-effects/animation-keyframes/keyframe-interpolation.html)
- [Motion paths and animation tools](https://helpx.adobe.com/after-effects/desktop/animate-in-after-effects/assorted-animation-tools/assorted-animation-tools.html)
- [Expression basics](https://helpx.adobe.com/after-effects/desktop/work-with-expressions/expression-basics/expression-basics.html) and [expression language reference](https://helpx.adobe.com/after-effects/desktop/work-with-expressions/expression-language-reference/expression-language-reference.html)
- [Shape attributes and path operations](https://helpx.adobe.com/after-effects/desktop/drawing-painting-and-paths/shapes-and-shape-attributes/shape-attributes-paint-operations-path.html)
- [Text animation](https://helpx.adobe.com/after-effects/desktop/animating-text/text-animation/animating-text.html)
- [Track mattes](https://helpx.adobe.com/after-effects/using/track-mattes-and-traveling-mattes.html) and [mask tracking and path animation](https://helpx.adobe.com/after-effects/desktop/animate-in-after-effects/animate-shape-paths-and-masks/animating-shape-paths-masks.html)
- [3D design space and viewer](https://helpx.adobe.com/after-effects/desktop/work-with-3d-composition/work-in-3d-design-space/work-in-3D-space.html), [parametric meshes](https://helpx.adobe.com/after-effects/desktop/work-with-3d-composition/create-parametric-meshes/working-with-parametric-meshes.html), and [3D model import](https://helpx.adobe.com/after-effects/desktop/import-files/import-and-add-3d-models/import-3d-model.html)
- [Multi-Frame Rendering](https://helpx.adobe.com/after-effects/desktop/render-and-export/multi-frame-rendering/multi-frame-rendering.html)
- [Memory and cache management](https://helpx.adobe.com/after-effects/desktop/memory-storage-performance/memory-and-storage/memory-storage1.html), [Composition Profiler](https://helpx.adobe.com/after-effects/desktop/work-with-compositions/composition-profiler/composition-profiler.html), [previewing](https://helpx.adobe.com/after-effects/desktop/view-and-preview/preview-video-and-audio/previewing.html), and [performance improvements](https://helpx.adobe.com/after-effects/desktop/memory-storage-performance/improve-performance/improve-performance.html)
- [Render Queue and export basics](https://helpx.adobe.com/after-effects/desktop/render-and-export/basics-of-rendering-exporting/basics-rendering-exporting.html) and [H.264 export](https://helpx.adobe.com/after-effects/desktop/render-and-export/export-h-264-from-the-after-effects/exporting-h264-from-the-after-effects-render-queue.html)
- [Audio Spectrum and Waveform effects](https://helpx.adobe.com/after-effects/desktop/apply-effects-and-animation-presets/list-of-effects/generate-effects.html)

EffectCraft project references:

- [README](https://github.com/storytold/effectcraft)
- [Roadmap](https://github.com/storytold/effectcraft/blob/main/ROADMAP.md)
- [Feature parity checklist](https://github.com/storytold/effectcraft/blob/main/docs/parity.md)
- [Gaps assessment and prioritized workstreams](https://github.com/storytold/effectcraft/blob/main/docs/gaps.md)
