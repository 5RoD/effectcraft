# Camera workflow acceptance checklist

This is a behavior and test plan for the camera workflows the user emphasized. It is based on Adobe's public help pages and EffectCraft's checked-in docs/source only. It does not claim the checklist has been run, and it does not claim missing behavior has been implemented.

## 5ROD.27 navigation increment

Unified Camera is available in the Orbit toolbar flyout. `C` cycles Unified, Orbit,
Pan Camera, Dolly and back to Unified; `1/2/3` still select the individual tools.
Unified primary/middle/secondary drags dispatch orbit/pan/dolly commands. Holding
Shift constrains Unified orbit to the first movement's dominant axis, including
when the middle or secondary button started the drag. Space panning and middle
panning with other tools keep their existing behavior.

In a 3D composition without an active camera layer, navigation and framing switch
to the Default view without editing the project or creating a camera. Fixed
orthographic views permit pan/dolly but reject orbit with a recoverable error.
Active camera edits respect camera locks, merge one drag into one undo step, and
framing converts composition time to offset/stretched layer time exactly once.

`F` and `Ctrl+Shift+F` frame selected/all active 3D content when a camera tool is
selected and a viewer panel has focus. Timeline `F` continues to reveal Feather;
the shortcut editor still supplies these bindings. Camera layers and lights are
excluded from the camera-tool framing targets. The ordinary View menu framing
commands retain their existing 2D behavior.

Synthetic engine regressions are in `tests_3d.rs`; real keyboard and mouse events
are exercised by `ui_camera_navigation.rs`. This is an implementation acceptance
increment, not an After Effects pixel comparison. Cursor-centered interaction
modes, `Shift+1/2/3`, temporary single-letter tools, parented camera transforms,
transform gizmos and the broader checklist below remain open. Existing navigation
uses the camera's point of interest rather than a depth hit under the cursor.

Adobe's [current 3D navigation documentation](https://helpx.adobe.com/after-effects/desktop/work-with-3d-composition/work-in-3d-design-space/work-in-3D-space.html)
and [Default Camera documentation](https://helpx.adobe.com/after-effects/desktop/work-with-3d-composition/adjust-default-camera-settings/adjust-default-camera-settings-for-3d-compositions.html)
explicitly permit navigation without a camera layer. The older shortcut-table
note requiring a camera layer must not override that behavior.

## Current EffectCraft surface

EffectCraft has camera creation/settings commands (`layer.newCamera`, `layer.cameraSettings`), one-node/two-node camera orientation, custom 3D viewer cameras with Orbit/Pan/Dolly tools, camera-from-view creation, keyframeable camera transforms/options, orbit-null/focus-link helpers, Classic and Advanced 3D depth of field, and a 3D Camera Tracker with solve-to-camera/text/solid/null/shadow-catcher workflows. The 3D Tracker analysis is explicitly CPU-only in the current parity notes. Useful implementation references:

- [`three_d.rs`](../../crates/engine/src/commands/three_d.rs) — camera settings, camera tool operations, view switching, look-at, camera-from-view.
- [`rig3d.rs`](../../crates/engine/src/commands/rig3d.rs) — orbit null and other camera rigs.
- [`focus.rs`](../../crates/engine/src/commands/focus.rs) — focus distance links and set-focus-to-layer.
- [`camera_track.rs`](../../crates/engine/src/camera_track.rs) and [`camera_cmds.rs`](../../crates/engine/src/commands/camera_cmds.rs) — tracker solve placement and generated-camera keyframes.
- [`camera_tracker_ui.rs`](../../crates/ui-egui/src/panels/camera_tracker_ui.rs) — viewer targets, selection and create-from-solve actions.
- [`dialogs_3d.rs`](../../crates/ui-egui/src/panels/dialogs_3d.rs), [`state.rs`](../../crates/ui-egui/src/state.rs), [`menus.rs`](../../crates/ui-egui/src/menus.rs), and [`viewer.rs`](../../crates/ui-egui/src/panels/viewer.rs) — camera dialog, shortcuts, and interactive navigation.
- [`parity.md`](../parity.md) — 3D, depth-of-field and render/performance assessment; 3D Camera Tracker remains CPU-side.

## Verified gaps and risks

1. **Camera Settings still omits live Preview; lens defaults need an Adobe comparison.** This fork adds linked Aperture and F-Stop controls using the existing engine conversion. Adobe documents that relationship and says the Camera Settings Preview checkbox is enabled by default. The public documentation does not specify the aperture unit conversion or establish that EffectCraft's 50 mm default of about f/1.98 matches Adobe's preset. Live Preview and verified preset/unit parity remain open.
2. **Camera navigation modes are fixed.** EffectCraft labels its tools Orbit Around Cursor, Pan Under Cursor, and Dolly Towards Cursor and binds `1/2/3` (`crates/ui-egui/src/state.rs:99–138`); the viewer dispatches fixed orbit/pan/dolly operations (`crates/ui-egui/src/panels/viewer.rs:1318–1322`). Adobe's 3D navigation docs describe editable Orbit/Pan/Dolly interaction modes, `Shift+1/2/3` cycling, and preferences for those controls. The current EffectCraft code has no corresponding mode-cycle behavior/preferences (`state.rs`, `crates/ui-egui/src/menus.rs:99–103`).
3. **The camera-from-view command cannot exactly preserve an orthographic view.** `camera_from_view` converts an orthographic viewer state into a perspective camera using a default camera zoom (`crates/engine/src/commands/three_d.rs:624–637`). That is a reasonable camera approximation, but exact image-space equivalence needs to be tested and should not be promised as an exact orthographic camera conversion.
4. **3D Camera Tracker is a performance hotspot.** `docs/parity.md` states that the tracker stays CPU-side. Solving runs asynchronously and is cancellable, but GPU acceleration, memory use, cancellation latency, and large/high-resolution footage throughput should be measured before describing tracking as optimized.
5. **Generated camera interpolation needs real-footage validation.** `camera_track::write_camera` creates one-node camera position/orientation keys for analyzed frames and keys Zoom when focal length varies. Verify camera/layer timing offsets, frame-rate conversion, dropped frames, zoom ramps, and interpolation between sampled frames with footage beyond synthetic tests. The current behavior is documented in code, but this is not equivalent to a real-footage acceptance result.

## Acceptance checklist

### Create and configure cameras

- [ ] Create a camera from Layer ▸ New ▸ Camera, the documented Windows shortcut `Ctrl+Alt+Shift+C`, and the command path. New Camera must be undoable, selectable, named predictably, and available to the Active Camera view.
- [ ] Verify 15/20/24/28/35/50/80/135/200 mm presets at multiple composition widths. Check zoom, angle of view, focus distance, aperture, and round-trip values. Compare a known 50 mm setup with the default camera and test custom zoom/focal length/angle-of-view inputs.
- [ ] Switch Camera Settings between One-Node and Two-Node. One-Node rotation must preserve view direction when converted; Two-Node must preserve the view and follow its Point of Interest. Animate POI between targets and verify the camera turns continuously.
- [ ] Add F-Stop and live-preview controls to the Camera Settings workflow, or explicitly document their absence. Check Aperture↔F-Stop conversion across presets, comp sizes, zoom changes, and non-default values; disabling Preview should defer render updates until apply if that option is offered.
- [ ] Verify the topmost enabled camera at the current time is the rendered Active Camera; cameras outside their in/out range, disabled cameras, and multiple overlapping camera layers must switch deterministically. Compare Active Camera output with custom views and nested compositions.
- [ ] Verify creating a camera from every custom 3D view preserves perspective, framing and orientation. For orthographic views, quantify the perspective approximation; ensure the command never implies that the resulting perspective camera is truly orthographic.

### Navigate and animate

- [ ] In a focused Composition/Viewer panel, `1`, `2`, and `3` select Orbit, Pan and Dolly. Verify the commands do not fire while typing into text fields, editing expressions, or recording shortcuts; verify selection and focus do not divert these keys to an unrelated panel action.
- [ ] Compare Orbit Around Cursor, Pan Under Cursor, and Dolly Toward Cursor behavior with Adobe's defaults. Test cursor positions over near/far layers, empty space, and outside the comp bounds. Check wheel/mouse gestures, axis direction, speed, and stability at different viewer zoom levels.
- [ ] Implement/test `Shift+1`, `Shift+2`, and `Shift+3` mode cycling and persisted 3D navigation preferences; confirm the toolbar tooltip and active mode always agree. Keep camera navigation distinct from layer transform gizmos (Adobe documents `4/5/6` for gizmo selection in its 3D preference mode).
- [ ] Test navigation in Front/Left/Top/Back/Right/Bottom/Custom views without modifying project camera layers. In Active Camera view, verify navigation edits the intended camera, respects its parent and one/two-node type, and remains undoable.
- [ ] At times before, on, between and after existing keys, Orbit/Pan/Dolly must update animated camera properties with the expected keyframe or edit behavior. Continuous drags should form a sensible single undo step, not hundreds of isolated edits; undo/redo must restore both framing and keyframe data.
- [ ] Create an orbit-null rig for an unparented one-node and two-node camera. Confirm it creates a 3D parent at the intended pivot, preserves the current camera image, then orbits around the pivot when the null rotates. Test animated camera position/POI, nonzero anchors, and parented camera rejection with a clear recoverable message.
- [ ] Link Focus Distance to POI or a target layer; move/animate the target, rename it, parent it, and move it behind the camera. Verify linked focus remains correct or reports a safe, visible expression error. Test Set Focus Distance to Layer at multiple times.

### Depth of field

- [ ] Toggle camera Depth of Field in Classic 3D and Advanced 3D. Animate Focus Distance, Aperture/F-Stop, Zoom/Focal Length, and Blur Level while previewing and rendering; confirm that the focus plane, bokeh shape, tilted-plane blur, and highlight response track the camera.
- [ ] Focus by selecting the camera and target 3D layer, then use Set/Link Focus Distance to Layer. Cover targets at different depths, behind-camera targets, parented targets, nested comps, text/shapes/models, and target deletion/rename.
- [ ] In Advanced 3D, test partially transparent objects layered in front of/behind one another. Adobe specifically documents depth-of-field support for overlapping transparency; test for correct focus and compositing without halos, sorting artifacts, or a different result between preview and final render.
- [ ] Benchmark DOF at 1080p and 4K across low/high aperture, blur level, iris shapes, and multiple transparent layers. Record GPU/CPU time, memory, preview responsiveness, and quality at Full/Half/Quarter resolution. Exercise GPU device loss and fallback paths without losing the current frame or project.

### 3D Camera Tracker and generated cameras

- [ ] Apply Track Camera to footage using the Animation menu, Tracker panel and 3D Camera Tracker effect. Analyze and solve in the background, cancel both stages, switch projects while running, and verify progress/errors are clear and the last valid project survives failure.
- [ ] Test Fixed Angle of View, Variable Zoom and Specify Angle of View; Auto Detect, Typical, Mostly Flat Scene and Tripod Pan; show/render 2D Source and 3D Solved points; Average Error; Detailed Analysis; and Auto-delete Points Across Time. For each option, verify whether a re-solve is required and that the status reflects analyzed/solved state.
- [ ] Compare solves on moving-camera parallax, tripod pan, mostly planar scenes, zooming cameras, rolling shutter, low texture, moving foreground subjects, cuts, variable frame rate, and lens distortion. Delete moving-object outliers and confirm the expected points disappear across time and the result re-solves.
- [ ] Select a point, select a point-plane, shift-add points, marquee points, and reposition/resize a target. Set Ground Plane and Origin, create camera alone, create text/solid/null, create multiple layers, and create a shadow catcher plus light. Every action should use the intended point/plane, be one undo step, and preserve existing scene content.
- [ ] From a solved tracker, create a camera and verify one-node camera position/orientation keys at analyzed frames, focal-length/Zoom keys for variable zoom, frame-time alignment to the tracked layer, and matching projections between tracked features and 3D test geometry across the full shot—not only the current frame.
- [ ] Create tracker camera first, then add text/solid/null; add such content first and verify camera creation/reuse. Re-run analysis after source trim, transform, effect, or timing changes; ensure stale solve/keyframes are invalidated and regenerated without duplicating cameras.
- [ ] Exercise scene origin/ground plane changes before and after adding content. Warn that existing content is not retroactively moved if the coordinate basis changes, matching Adobe's documented behavior.
- [ ] Benchmark tracking by footage duration, resolution, frame rate, solve method and analysis detail. Keep the UI responsive during background analysis, bound RAM, provide cancellation latency and failure diagnostics, and quantify CPU/GPU limitations. Do not label the tracker “GPU accelerated” unless analysis/solve work actually runs on the GPU.

## Official Adobe references

- [Cameras, lights, and points of interest](https://helpx.adobe.com/after-effects/desktop/work-with-layers/camera-layer/cameras-lights-points-interest.html) — camera creation, one/two-node behavior, presets, zoom, aperture, F-Stop, focus and camera settings.
- [Camera animations](https://helpx.adobe.com/after-effects/how-to/camera-animation.html) — keyframed one-node and two-node camera moves and POI animation.
- [3D camera navigation and Create Camera from 3D View](https://helpx.adobe.com/after-effects/desktop/work-with-3d-composition/work-in-3d-design-space/work-in-3D-space.html) — Orbit/Pan/Dolly modes, 1/2/3 and Shift+1/2/3 controls, customizable preferences and camera-from-view flow.
- [Official keyboard shortcuts reference](https://helpx.adobe.com/after-effects/desktop/get-started/keyboard-shortcuts/keyboard-shortcuts-reference.html) — Windows `Ctrl+Alt+Shift+C`, `1/2/3`, `F10/F11/F12`, `Ctrl+Alt+Shift+\`, and tool context notes.
- [Tracking 3D camera movement](https://helpx.adobe.com/after-effects/desktop/work-with-3d-composition/3d-camera-tracker-effect/tracking-3d-camera-movement.html) — background analysis, solve modes, point editing, ground plane, Create Camera and generated layers.
- [Enable in-engine depth of field in Advanced 3D](https://helpx.adobe.com/after-effects/desktop/work-with-3d-composition/work-with-3d-scene-depth-data/enable-in_engine-depth-of-field-in-advanced-3d.html) — Advanced 3D DOF controls and transparency behavior.
