# Checked Timewarp and shared shutter samples

Timewarp retains the existing block-matching, tuning, crops, matte/warp layer behavior,
interpolation and shutter-sample order. It prepares one image per unique source instant and
shares those images through indices. Masked derivatives and motion fields are also shared
within the invocation. It streams output samples through an accumulator instead of retaining
every intermediate output. With zero Source Crops, CPU motion borrows its inputs.

The GPU uploads each unique original/cropped image and motion field once. It uses up to three
private sample textures and two accumulator textures, reusing them in command order. Every
kernel writes the whole target, including odd dimensions; pooled scratch may contain arbitrary
old pixels. Inputs are immutable throughout recorded work. Missing required kernels produce
checked errors; a completed output is never fabricated from unwritten scratch.

## Supported bounds

- Positive finite render scales, finite offsets within +/-1e12 rendered pixels, and storage
  matching the image dimensions.
- At most 16,384 pixels per dimension and 32 Mi-pixels per image or union grid. Unsupported
  source extents are rejected rather than truncated.
- At most 64 shutter samples and 128 unique source instants. Duplicate samples do not create
  additional full-frame images. Frames and samples keep their existing order and weights.
- Finite evaluated controls within their documented effect ranges. Scaled vector spacing and
  matching windows must not exceed 16,384 pixels. Crops must fit their integer representation.
- Finite source/comp times within the supported `Tick` range and frame rates through 1,000 fps
  (zero denotes an unknown rate and retains the existing 30 fps fallback). Broadcast source
  boundaries use exact Tick keys. Source, matte and warp reads use the same keys; auxiliary
  layers add the existing composition/layer-time offset in Tick arithmetic.
- A conservative per-effect peak of 1,536 MiB across source placement, current input, unique
  source/derived frames, fitted mattes/warps, matching scratch, vector storage, GPU uploads,
  fixed sample/accumulator scratch and output/readback staging.
- At most 32 billion estimated SAD comparisons, smoothing neighbours and interpolation taps
  per invocation. The estimate accounts for vector spacing, matching-window area, pyramid
  levels, smoothing iterations, unique fields, interpolation filtering and samples.

Budget errors identify the requested totals and limits. The effect does not reduce quality,
sample count, resolution, filtering or color settings to fit. These are effect-local bounds,
not a total renderer, decoder, RAM/VRAM or export-concurrency budget. The host returns already
decoded pixels; renderer/decoder preflight remains responsible for allocations made before
the effect can inspect a returned source. Existing block-matching internals use bounded
allocations; this increment is not a general allocator/OOM redesign.

The checked effect follows the existing renderer diagnostics: preview keeps its last good
frame, failed results are not cached, and export fails visibly. The fork render salt advances
to `effectcraft-5rod-render-6`; the disk cache file format remains `v1`.

## Exact source-frame identity

The separate memory increment (render salt 5) preserved the historical microsecond source
rounding for its old/new comparisons. This fidelity increment (salt 6) removes that rounding
and the frame-selection epsilon. At 60/120 fps and rational broadcast boundaries these could
select the preceding decoded frame, or advance a legitimate instant one Tick before a boundary.
Whole Frames now floors the exact source Tick. Frame Mix and Pixel Motion use that same pair;
an exactly aligned instant has zero fraction and does not estimate motion.

`EffectHost::self_at_tick`, `layer_at_tick` and `time_ticks` are additive exact-time hooks.
The native renderer overrides them, keeping individual Ticks distinct at long and negative
times and preserving the exact current layer/comp offset. Existing custom hosts need no change:
the defaults delegate to their seconds methods without a nudge. A float-only host cannot
distinguish every neighbouring Tick at long durations; it must override the hooks for exact
temporal identity. Ordinary and reversed 100% speed retain the exact current Tick. Other
evaluated speeds use the scalar's f64 arithmetic and round to a Tick. Custom frame rates are
not snapped to broadcast rates; their f64 boundary uses the first representable Tick at or
after it. No codec or source-rate interpretation changes are included.

Small generated tests cover one-pixel frame identity at 60/120 fps, rational rates, interpreted
and shifted sources, reverse speed, three-day timelines, animated matte/warp reads, exact
boundaries and +/-1/7 Tick or neighbouring f64 ULPs. The native host tests check the Tick reaching
the footage source directly. These correctness checks are separate from frozen memory A/B
measurements; they are not new playback or export-throughput evidence.

## Acceptance to run after the build window

Source-only preparation is not performance evidence. Run the effects Timewarp unit tests,
existing Timewarp golden suite, checked GPU streaming/missing-kernel tests, export regression,
render salt test, selected-crate strict Clippy and the full repository gate. Set
`EFFECTCRAFT_REQUIRE_GPU=1` and verify the actual NVIDIA Vulkan adapter for GPU acceptance;
unavailable adapters or kernels cannot count as passing parity tests. Existing poisoned-scratch
test mode exercises full-write coverage. Require identical CPU collected/streamed pixels and
GPU pixels within the existing 1e-3 premultiplied tolerance, with exact output geometry.

For release measurements use original moving textured/occluding scenes and actual licensed
OBS footage: 2560x1440 at **120 fps is primary** (8.33 ms/frame), 60 fps is secondary
(16.67 ms/frame), and 144 fps is an optional stress case (6.94 ms/frame). Include ordinary layers,
nested precomps, forward/reverse/freeze/nonlinear remapping, Whole/Frame Mix/Pixel Motion,
matte/warp layers, normal/extreme filtering and 1/8/16/64 shutter samples. Report first-frame
latency, distinct-frame FPS, p50/p95/p99 frame times, peak RAM/VRAM, upload/readback bytes,
pool misses and CPU/GPU utilization, with warm/cold and Full/Half/Quarter previews separate.
Measure GPU completion and the complete UI pipeline; recording dispatches is not frame time.

Maximum-quality exports may use the full PC. Preserve requested resolution, frame rate,
sample count, filtering, effect settings and working/output color. Compare 10-second 60/120 fps
exports (144 fps stress optional) with bounded in-flight concurrency of 1/2/4 and the CPU-thread
default. Record total wall time, render-stage FPS, conversion/readback/encoding/storage stage
times and peak RAM/VRAM. Choose the fastest bounded concurrency; increasing simultaneous heavy
GPU frames can lower throughput. Verify output frame count/timebase and decoded pixel quality
using existing external test oracles. This increment adds no cross-output-frame flow cache,
GPU motion estimator, codec changes or commercial plug-in compatibility.
