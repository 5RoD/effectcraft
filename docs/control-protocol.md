# Control protocol

The desktop app exposes a control channel for agents, tests and the MCP bridge:

```sh
effectcraft --control 9877        # or EFFECTCRAFT_CONTROL_PORT=9877
```

The port must be a number from 1 to 65535, and `--control` overrides the variable. A bad port or an
unknown option prints an error and the app exits with status 1 before it opens a window.

- **Transport:** TCP on `127.0.0.1` only, one JSON object per line in each direction. Connections are
  persistent; requests on one connection are answered in order.
- **Request:** `{"id": 1, "method": "engine.execute", "params": {...}}` (`id` is echoed back, `params`
  defaults to `{}`).
- **Reply:** `{"id": 1, "ok": true, "result": ...}` or `{"id": 1, "ok": false, "error": "message"}`.
- Requests run on the UI thread between frames. Input methods (`ui.click`, `ui.key` and similar)
  reply after the synthetic input has been processed. Methods that need an element that isn't drawn
  yet are retried for a few frames. A request times out after 60 s.
- **Only requests are read:** every line must be a JSON object with a string `method` (blank lines are
  skipped). Anything else (text that isn't JSON, a JSON array or number, an object without `method`,
  invalid UTF-8, a line longer than 4 MiB) gets one error reply ending in "closing the connection", and
  the server closes the connection, so nothing sent after it runs. An HTTP request (for example a web
  page's cross-origin `fetch` to `127.0.0.1:<port>`) therefore can't smuggle a command in its body: its
  request line is rejected first. At most 16 connections are served at once; further ones get an error
  line and are closed. The port has no authentication, so only enable it while you use it.

Quick test:

```sh
printf '{"id":1,"method":"engine.execute","params":{"command":"comp.new","params":{"name":"A"}}}\n' | nc 127.0.0.1 9877
```

## Engine

`layer.setTransforms3D` accepts `changes: [{layer, position?, scale?, orientation?, rotation?}]`;
each supplied transform is an XYZ vector. It applies 1–4096 updates atomically to unlocked
3D layers in the active comp, preserving animation and separated Position axes. Enabled
expressions and invalid targets reject the whole update. Optional `time` uses composition
seconds; consecutive updates with the same `merge` key form one undo step. This is the
command foundation for 3D manipulation; it does not itself add viewer handles.

| Method | Params | Result |
|---|---|---|
| `engine.execute` | `{command, params?}` | The command's result. Every engine command (`effectcraft-cli commands`) runs exactly as it does headless, so it never opens a dialog: `comp.new {}` creates a default comp. UI-only ids (`tool.*`, `view.*`, `window.*`, `playback.*`, `timeline.*`, `app.*`) fall through to the menu dispatcher. Unknown top-level params are rejected, and the error lists the accepted keys. |
| `ui.menu.invoke` | `{id, params?}` | Like a menu click: commands with empty params may open their dialog (New Composition, Solid Settings, file pickers). |
| Copy a Media Browser source path | `ui.menu.invoke {id: "mediaBrowser.copySourcePath", params: {path}}` | Copies the exact non-empty path string to the clipboard and returns `{path}`. Supports file/folder paths and virtual source identifiers. Missing, empty or non-string paths return an error. |
| UI commands (via either) | | `timeline.column {column, visible?}` (av, keys, label, num, comment, switches, modes, parent, in, out, duration, stretch), `timeline.sourceName {value?}`, `timeline.search {query}`, `timeline.reveal.<kind>` / `timeline.revealAdd.<kind>` (the property shortcuts and Shift+shortcut: position, scale, rotation, opacity, anchor, effects, masks, feather, levels, animated, and the double-press sets maskPath, maskOpacity, timeRemap, expressions, material, paint, selected, missingEffects, waveform), `timeline.keyAt.<anchor\|position\|scale\|rotation\|opacity>` (Alt+Shift+A/P/S/R/T), `timeline.twirlSelected` (Ctrl/Cmd+`), `timeline.collapseAll`, `flowchart.options {layers?, effects?, solids?, direction?: lr\|tb, comp?}`, `flowchart.graph {comp?}` → `{nodes, edges}`, `window.workspace {name}` (all After Effects workspaces). |
| `engine.commands` | `{filter?, enabledOnly?}` | `[{id, label, menu, shortcut, params, enabled, why}]` |
| `ui.menu.list` | `{}` | The menu tree with enablement. |
| `render.frame` | `{comp?, time?, max_side?, path?, base64?}` | Renders a comp frame through the session, independent of the viewer zoom or resolution. `comp` is an id or name (default: the active comp), `time` is in comp seconds (default: the CTI), `max_side` caps the longest side (0 = full size). Writes a PNG to `path` (default: a temp file) and returns `{comp, time, width, height, path}`. With `base64: true` it returns `{…, png: "<base64>"}` and writes no file. |

Useful query commands for `engine.execute` (they aren't journaled or undoable): `project.summary`,
`comp.info {comp?}`, `layer.tree {layer, comp?, depth?, time?}` (every node has a `path`),
`prop.get {layer, path, comp?, time?}`, `editor.state`, `command.list {filter?, enabledOnly?, schemas?}`,
`command.describe {command}` (params doc and JSON Schema), `app.capabilities` (version, counts,
export formats, parity summary), `jobs.list` (Progress panel jobs, e.g. the background
`footage.check` after File ▸ Open).

Native File ▸ Save, Save As and Increment and Save serialize the document and **all branching
history** in one background `projectSave` job, keeping the editor available. A menu invocation
returns `{job, started: true}`; poll `jobs.list` until that job appears in `finished`, and check
its `status` (`done` or `failed`) before relying on the file. The saved snapshot is the state at
invocation; edits made during the write remain modified. New/Open can proceed while the old
snapshot finishes, without applying its saved state to the new document. Only one manual save
writer runs per session; another save/copy is rejected until completion. Saves cannot be
cancelled halfway through an atomic write. A normal window close waits for the pending save
and checks for newer unsaved edits again.

`engine.execute` keeps synchronous saves by default. `file.save {path?, background?}`,
`file.saveAs {path, background?}` and `file.incrementAndSave {background?}` accept
`background: true` for the same job behavior. `background: false` is an explicit save barrier,
also used when answering the unsaved-changes prompt. File ▸ Open still parses synchronously;
this change does not make project loading asynchronous.

Every engine command is reachable through `engine.execute`, and every interactive widget of every
panel registers an element id; both are enforced by `crates/ui-egui/tests/ui_agent_audit.rs`.

## UI state

| Method | Params | Result |
|---|---|---|
| `ui.inspect` | `{}` | Window size, pixels per point, fps, serialized `UiState`, playback, time, active comp, selection, open dialog, last render ms, `keyboardFocus`, `windowFocused`, `workspaceStorageError`. |
| `ui.elements` | `{prefix?}` | `[{id, label, rect: [x, y, w, h]}]`: the widgets drawn in the last frame, in points. |
| `ui.set` | `{tool?, workspace?, theme?, focused?, viewer?, timeline?, menuBar?, home?}` | `viewer: {zoom (null = fit), res: Full\|Half\|Third\|Quarter\|Auto, pan: [x,y], grid, rulers, safeMargins, transparencyGrid}`; `timeline: {pps (null = fit), start, graphEditor, showModes, openLayers: [ids], openGroups: [uids]}`. |
| `ui.panel.show` / `ui.panel.close` | `{panel}` | Panels: `Project`, `Composition`, `Timeline`, `EffectControls`, `EffectsPresets`, `Info`, `Preview`, `Character`, `Paragraph`, `Align` and others. |
| `ui.playback` | `{action: play\|stop\|toggle\|status}` | `{playing, time, audio, levelsDb, peaksDb}` (audio: an audio preview is running; L/R meter levels and held peaks in dBFS) |
| `ui.viewer.observe` | `{action?: start\|stop\|reset\|status\|read, maxSamples?, seconds?, offset?, count?}` | Opt-in active composition viewer draw observations and summary. See limits below. |
| `ui.decoder.stats` | `{}` | Read-only process counters: `{enabled, backend, hardwareFrames, sessions, declined, fallbacks, decodeCalls, decodeNanoseconds, conversionCalls, conversionNanoseconds}`. A registered backend does not prove hardware use; compare frame counters around playback. Work timers include read-ahead and overlapping calls; they do not sum to total playback latency. |
| `ui.transfer.stats` | `{}` | Read-only counters for the viewer's GPU context: `{uploads, uploadBytes, uploadCpuNanoseconds, readbacks, readbackBytes}`. Frame payloads include float images and compact native video plane buffers. The upload timer measures CPU packing, texture/buffer allocation/reuse and enqueueing, including staging copies, not DMA completion or shader execution. Parameter/uniform uploads are excluded. Returns an error when no GPU context is available. |
| `ui.renderer.stats` | `{}` | Reads existing CPU/GPU timing averages by composition, exact render scale and display/readback path: `{available, paths: [{comp, scale, display, cpuMs, gpuMs, cpuFrames, gpuFrames, sinceProbe, declined}]}`. It neither chooses nor resets a backend. No accelerator returns `available: false` and an empty list. Read explicitly outside an observation window; averages are renderer work estimates, not audio continuity, presentation latency or a proof of maximum throughput. |
| `ui.resize` | `{width, height}` | Resizes the window (points). |
| `ui.focus` | `{}` | Activates the window and takes keyboard focus. Agents normally avoid this, because the app is started without stealing focus. |
| `app.quit` | `{force?}` | Closes the app. A modified project shows the Save / Don't Save / Cancel prompt first (automation ids `dialog.unsaved.save`, `dialog.unsaved.dontSave`, `dialog.unsaved.cancel`) unless `force: true`. |

`keyboardFocus` is `null` when no egui widget has keyboard focus. Otherwise it contains the
actual widget `id` (hexadecimal), its `rect: [x, y, w, h]` when available, an `automationId`
when the widget registered its response identity, and `registryCandidates` containing exact
rectangle matches. Candidates alone do not establish widget identity. Header, toolbar and
dock-tab controls register their response identity. `ui.focused` continues to identify the
focused panel; it is separate from keyboard widget focus. `windowFocused` reports the native
window focus received by egui, which can differ from the retained widget focus.

`ui.viewer.observe` records the requested and actually drawn frame keys, monotonic UI time,
native texture status and playback/caching/waiting state. It is disabled until `start`, does
not read pixels or acquire the frame cache, and preserves the existing playback scheduler.
Only the final, undiscarded UI pass of a visible active viewer produces a sample. Hidden,
minimized, occluded and mirror-only viewers produce none; snapshot and wireframe draws are
reported separately from video frames. This measures viewer drawing, **not Windows scanout**.

`maxSamples` is an integer from 1 through 120,000 (default 8,192); `seconds` is finite and
from 0.001 through 300 (default 30). Recording stops at either bound and retains its samples.
`stop` preserves them, `reset` discards them, and `status`/`read` never restart timing. Invalid
start parameters preserve the previous observation. `offset` and `count` select a raw page;
both are nonnegative integers, `count` defaults to zero and is capped at 2,048. Summary
calculation occurs only on an explicit control request; avoid polling it during a benchmark.

The summary separates distinct frames, repeated textures, stale keys, missing textures,
loops, reverse/series transitions and the minimum skipped selected frames. Gap estimates
respect the preview's configured skip step. Stale draws, changed plans, implausible cursor
jumps and intervals spanning a whole loop are not classified as dropped selected frames.
Completed texture-hold intervals include p50/p95/p99/max and their count; the last unfinished
hold is separate. The raw page retains phase flags. Start while playing to isolate sustained
playback, or start before play to include cold startup. A synthetic/headless clock establishes
bookkeeping; actual footage, display presentation and audio need separate acceptance.

When a configuration store is available, current dock and floating geometry, focused and
maximized panels, and named workspace snapshots persist in `workspaces.json`. The current
layout and named snapshots are independent: changing a divider does not overwrite the named
snapshot until `window.saveWorkspace` runs. Changes save after pointer release, at most once
per second, with a final save on normal exit. Version 1 files are limited to 1 MiB and bounded
workspace trees; extra viewer IDs must be between 1 and 65,535. Native reads enforce the size
limit while reading and report I/O and UTF-8
errors. Unreadable, invalid or newer files leave the default workspace available and are
preserved for the entire session, including after later layout changes. Load and save errors
appear in the status message and the nullable
`workspaceStorageError` field; a failed write remains eligible for retry.
`window.saveWorkspace`, `window.saveWorkspaceAs` and `window.editWorkspaces` (rename, delete)
write the file immediately. Through `ui.menu.invoke` they return an error when it isn't written
(a failed write, a file preserved after a failed load, or no configuration store);
`engine.execute` hands them to the UI and returns `{frontend}` first, so check
`workspaceStorageError` there. The changed workspaces still apply for the session, and a later
save retries them.

## Input

Points are logical window coordinates. A target is `{id}` (the element's centre; add `fx`/`fy` between
0 and 1 to pick a spot inside it) or `{x, y}`. `modifiers` is `{shift, alt, command, ctrl}`, where
`command` is ⌘ on macOS and Ctrl elsewhere.

| Method | Params |
|---|---|
| `ui.click` | `{id \| x,y, button?: left\|right\|middle, count?, modifiers?}` |
| `ui.move` | `{id \| x,y}` |
| `ui.drag` | `{from: target, to: target, steps? (12), button?: left\|right\|middle, modifiers?}`: press, move, release |
| `ui.scroll` | `{id \| x,y, dx, dy}` |
| `ui.key` | `{key, phase?: tap\|down\|up}`, for example `Space`, `F9`, `Cmd+Z`, `Cmd+Shift+Z`, `PageDown`, `Escape`. Default `tap` presses and releases; use `down`, other pointer actions, then `up` to exercise held tools. Invalid phases return an error without adding input. |
| `ui.type` | `{text}`: text input into the focused field |

3D content handles use the same input methods. In Composition/Layer panels, `4`, `5`, `6`
choose Position, Scale and Rotation gizmos; `V` selects Universal. UI commands
`view.gizmo.position`, `.scale`, `.rotation`, `.universal` work through `ui.menu.invoke` or
`engine.execute`. `view.gizmo.space {space: "local"|"world"|"view"}` chooses the axis space;
unknown values fail before changing UI state. Serialized viewer fields are `gizmo` and
`gizmo_space`, with lowercase values.

Toolbar IDs are `tools.gizmo.mode` and `tools.gizmo.space`; below 1200 logical points they
become `tools.gizmo.compact`, with the same menu-row IDs (`tools.gizmo.position`, `.scale`,
`.rotation`, `.universal`, and `tools.gizmo.space.local|world|view`). Handle IDs are
`viewer.gizmo.position.x|y|z`, `viewer.gizmo.scale.x|y|z`, and
`viewer.gizmo.rotation.x|y|z.<segment>` (0–47). These register the visible hit geometry,
including segment-sized rectangles on rotation rings. Scale uses local axes; Shift scales
uniformly. Rotation snaps to one degree, Shift to five, and Ctrl/Command keeps fractional
precision. A drag merges its checked transform batches into one undo step. See the
[3D compatibility limits](5rod/after-effects-parity.md).

`W` selects the Rotation tool and its XYZ rotation rings for 3D content. The toolbar's
`tools.rotation.target` selector (compact rows `tools.rotation.orientation` and `.rotation`)
chooses Orientation or authored X/Y/Z Rotation. The equivalent command is
`view.gizmo.rotationTarget {target: "orientation"|"rotation"}`; invalid values leave state
unchanged. `rotation_target` is saved in viewer state and defaults to `orientation`.
With W, Shift snaps to 15 degrees. Rotation mode retains full revolutions and the authored
Orientation. Bounding-box corners rotate Z, top/bottom midpoints X, and left/right midpoints Y.

Shortcut presets also accept `Physical+<shortcut>` for a keyboard position, using egui's
US-position names (for example `Physical+6` and `Physical+Minus` distinguish AZERTY's
`-` and `)` keys). Physical bindings take precedence over logical bindings for that key
press. `ui.key` accepts the same notation to test them. Ordinary shortcuts remain layout-aware.
`time.workAreaStart` and `time.workAreaEnd` navigate to the first and last included work-area
frames and can be assigned through `shortcuts.set`.

## Locating things

| Method | Params | Result |
|---|---|---|
| `ui.viewer.locate` | `{x, y}` (comp pixels) | `{x, y}` window point |
| `ui.viewer.hit` | `{id \| x,y}` (window point) | `{x, y}` comp pixel |
| `ui.timeline.locate` | `{layer (id), time?}` | `{x, y}` window point on the layer bar |

## Screenshots

| Method | Params | Result |
|---|---|---|
| `ui.screenshot` | `{path?, panel?, id?}` | `{path, width, height}`: a PNG of the window, or cropped to one panel or element. |

Element ids are stable, for example `tools.Selection`, `panel.Timeline`, `panel.tab.EffectControls`,
`panel.tab.Timeline.<comp>` (one Timeline tab per open comp; `panel.tab.Timeline` is the shown
one), `panel.tab.Viewer(<n>)` and `viewers.<n>` (View ▸ New Viewer's other Composition viewers;
`viewers.0` is the Composition panel while another viewer is active), `header.workspace.Animation`, `project.item.<id>` (a double-click opens the item, Enter
renames it), `viewer.comp`, `viewer.handle.<layer>.<i>`,
`timeline.layer.<id>.bar`, `timeline.layer.<id>.nestedMarker.<i>` (a precomp's comp markers), `timeline.layer.<id>.twirl`, `timeline.layer.<id>.row` (click selects,
Enter or a double-click renames, drag reorders), `timeline.prop.<uid>.stopwatch`,
`timeline.key.<uid>.<frame>` (and `timeline.keyAt.<uid>.<ticks>`, exact in layer-time ticks, for
keys on subframes), `timeline.cti`, `effectControls.prop.<uid>.value` (an angle's
revolutions are `timeline.prop.<uid>.revolutions`, `effectControls.prop.<uid>.revolutions` and
`properties.prop.<uid>.revolutions`) and `effects.item.<name>`. The Composition viewer adds `viewer.magnification`, `viewer.resolution`,
`viewer.roi`, `viewer.grid`, `viewer.channel`, `viewer.exposure`, `viewer.snapshot`,
`viewer.showSnapshot`, `viewer.fastPreviews` (their popup entries are `viewer.<menu>Item.<n>`),
`viewer.ruler.top|left|origin`, `viewer.mask.<uid>.vertex.<i>`, `viewer.shapePath.<uid>.vertex.<i>`,
`viewer.motionPath.<layer>.<key>[.in|.out]`, `viewer.puppetPin.<uid>[.rotate|.scale]`,
`viewer.freeTransform.handle.<i>` and
`viewer.regionOfInterest`; the Graph Editor adds `timeline.graph.transformBox[.<i>]`,
`timeline.graph.snap` and `timeline.graph.reference`. Use `ui.elements` to see what is on screen.

Viewer state that agents drive headless too: `view.snapping`, `view.snappingOptions
{edgesExtended?, collapsedFeatures?, toggle?}` (the Tools bar's Snapping options,
`header.snappingOptions`: After Effects' Snap Edges Extended and Snap to Features in Collapsed
Compositions and Text Layers), `view.channel {channel, colorized?}`,
`view.exposure {stops | delta}`, `view.resetExposure`, `view.takeSnapshot`, `view.showSnapshot`,
`view.fastPreviewMode {mode}` (one mode each, for shortcuts:
`view.fastPreviews.off|adaptive|draft|fastDraft|wireframe`, and for the channels
`view.channel.red|green|blue|alpha|redColorized|greenColorized|blueColorized|rgbStraight`), `view.res.full|half|third|quarter|auto {comp?}` and
`view.res.custom {factor, comp?}` (the comp's Resolution, saved with the project but no undo
step; `ui.set {viewer: {res}}` sets the active comp's, and `ui.inspect` reports it as
`ui.viewer.res`), `view.setRegionOfInterest {rect}`, `view.addGuide {percent?}`,
`view.addPercentGuide`, `view.convertGuides {to: percent|pixels, index?}`,
`view.moveGuide` (comp pixels, whatever the guide's unit), `view.removeGuide`, `effect.setLabel {layer?, effect, label}`; editing: `shape.newPath` (Pen on shape layers),
`shape.newShape` (the shape tools: a new group in the selected shape layer, else a new shape layer),
`shape.toolOptions` (the Tools bar's Tool Creates Shape / Mask and Fill and Stroke Options),
`mask.insertVertex`, `mask.convertVertex`, `mask.deleteVertices`, `path.freeTransform` (masks and
shape paths by uid), `keys.setSpatialTangents` (motion-path handles) and `keys.transform` (Graph
Editor transform box, timeline Alt-drag scaling).

The MCP server's bridge mode (`effectcraft-cli mcp --bridge 9877`) is a thin client of this protocol;
see [agents.md](agents.md).

Keyframe command times (`prop.set`, `prop.addKey`, `keys.set`, `keys.select`) default to layer
seconds. Set `timeBase: "comp"` for comp seconds; `keys.set.newTime` and every
`keys.select.keys[].time` use the same input base. Omitted `prop.set` or `prop.addKey` times use
the target comp's CTI. Supplied write times snap to its nearest comp frame before conversion
to stored layer time, including on stretched or reversed layers. Existing `keys.set` neighbor
constraints and roving adjustment can change the final stored time.

Explicit bases add `layerTime` and `compTime` to unsplit `prop.set` and `prop.addKey` replies,
alongside `value` or `count`. Separated Position `prop.set` keeps an array of per-axis replies.
Explicit `keys.select` replies contain `count` and `keys[]`, with timing on each key in the
resulting selection. Key metadata's `time` remains layer seconds regardless of the input base.
Without `timeBase`, these three commands retain their existing JSON value/count replies,
including outside the layer's `[in, out)` range. Timing metadata carries outside-range warnings;
`keys.set` and `prop.get.keys[]` report both bases and warnings without an explicit base too.
`prop.get.time` is comp seconds. For these commands, `path` or `prop` identifies a property, not a group.
Set a layer's offset with `layer.timing {layer, start}` (comp seconds).

Panel moves, tab ordering, edge splits and floating geometry use shared docking transactions.
`window.floatPanel` requires exactly four finite numbers when `rect` is supplied. Invalid
geometry or missing source/anchor panels leaves the layout unchanged. The persisted workspace
shape is unchanged: accordion open states and explicit heights, floating groups, and dynamic
viewer/ScriptUI identities remain app data. Pointer drops use the same shared placement rules
as commands, with floating groups resolved in their actual window stacking order.

`window.dockAction` accepts `{action}` using the shared docking action's JSON form. It
covers tab moves and ordering, split and floating geometry, activation, and closing.
For example, `{"action":{"ResizeSplit":{"path":[],"size":{"FixedFirst":360}}}}`
resizes the root split; path entries use `false` for the first child and `true` for
the second. `{"action":{"MoveFloating":{"panel":"Info","rect":[45,65,420,320]}}}`
repositions and resizes the floating group containing Info. Invalid actions leave the
layout unchanged. Pointer gestures dispatch through this same catalog command.
EffectCraft also supports `SetStackOpen` and `ResizeStack`. Dynamic panel identities use
`{"Viewer":42}` or `{"ScriptPanel":77}` in the action. Closing runs the existing viewer
and ScriptUI lifecycle callbacks; opening uses the existing panel creation commands.
