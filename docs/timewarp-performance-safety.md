# Checked Timewarp and shared shutter samples

Timewarp retains the existing block-matching, tuning, crops, matte/warp layer behavior and
shutter-sample order; separate temporal/fallback fidelity changes are described below.
It prepares one image per unique source instant and
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
to `effectcraft-5rod-render-7`; the disk cache file format remains `v1`.

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

## Pixel Motion fallback continuity

This separate fidelity increment (render salt 7) changes the fallback near Error Threshold.
The old CPU and GPU could differ by one floating-point ULP in the warped-frame disagreement;
a hard comparison then selected very different plain/warped pixels. A five-pixel generated
fixture demonstrated a 0.0585 absolute channel error on NVIDIA RTX 3050/Vulkan. The frozen
memory comparisons remain unchanged and retain their recorded strict CPU/GPU failures.

Both paths now interpolate smoothly between the warped mix and the plain mix. Let `m` be
the greatest absolute RGBA channel of the two warped pixels, at least 1. The transition
half-width is `min(0.00015*m, 0.001, threshold/2)`. At or below `threshold - half-width` the
output is the warped mix; at or above `threshold + half-width` it is the plain mix. Inside,
the plain-mix weight follows `t*t*(3-2*t)`, where `t` spans 0..1 across the band. Threshold
0 and 1 disable fallback as before, and Build From One Image still bypasses it. For normalized
RGBA the total band is 0.0003, or 0.03 percentage points in the UI's 0..100 Error Threshold.
The total HDR band caps at 0.002. This is an intentional quality change within the band,
with the original CPU pixels retained exactly outside it; it is not an output-identical fix.

CPU sampling keeps f64 coordinates and bicubic weights. GPU sampling uses integer pixel
anchors and local fractional displacement, including the motion-grid quotient/remainder,
so adding motion to a full f32 frame coordinate cannot discard the fractional pixel first.
This requires no extra image buffers. Checked plan limits remain 16384 pixels per dimension,
32 million pixels per image, flow magnitude at most 65536 pixels per component, and the
existing byte/work budgets. Source and prepared RGBA channels have a checked magnitude
limit of +/-1e12. Nonfinite or larger channels, controls and mutated public plans fail
before source interpolation or GPU transfers. Source-channel validation adds one linear
scan on raw host ingress. Private union/fitting checks reuse validated owned sources
without another channel scan; public mutable plans retain their prepared-frame scan
before CPU/GPU execution. No additional image buffer is allocated for validation.
Negative linear RGB, HDR values and partial alpha remain supported; bicubic sampling retains its existing nonnegative
ringing clamp and transparent boundary.

Brightness correction also rejects nonfinite computed means/gains and means outside
+/-2e12 before division; it retains the existing positive `1e-4` mean cutoff. With a 1%
rounding allowance for the weighted-grey operations, the resulting gain is bounded by
2.02e16 and corrected grey by 2.05e28. Luma is at most 1.26 times that value. Even a
16384-by-16384 matching window has a conservative SAD bound below 2.8e37, including a
factor two allowance for a long nonnegative f32 sum. The parabola's four-term bound is
below 1.12e38, less than half of f32's maximum. Bicubic's 16 taps
with conservative 1.5-per-axis weights, matte multiplication/compositing, and 64 shutter
samples remain finite under the source bound. These are arithmetic bounds, not additional
allocations or a claim that such a large matching request passes the work budget.

Subpixel refinement compares the signed numerator to half the positive denominator
before dividing, bounding the resulting ratio to +/-0.5 without first overflowing it.
Flat or negative curvature still returns zero. A new checked matcher shares the legacy
implementation and reports nonfinite SAD/curvature before median filtering can conceal
it; Timewarp propagates this error. The normal matching path adds no atomic store,
allocation or scan per pixel/vector. Existing finite offsets are unchanged.
The public checked entry also validates equal geometry, exact storage and controls in
constant time: images at most 16384 pixels per dimension/32 million pixels, tile 2..=16384,
window 4..=16384 and search radius 0..=64. The caller remains responsible for its aggregate
matching-work budget; Timewarp retains its existing planner and fixed search radius 4.

The WGSL specification permits floating-point reassociation and fusion, so a one-ULP hard
branch is not a portable parity contract. Local coordinates and a continuous transition
reduce measured sensitivity; they do not prove universal GPU parity. Subnormal f32 values
may be flushed by a GPU, and large HDR magnitudes within the supported range can amplify
rounding; the +/-1e12 limit protects arithmetic and is not a universal absolute 1e-3 quality
guarantee throughout that range or on every driver. Acceptance must report the actual
adapter, supported scenes and maximum absolute errors. See
[WGSL floating-point accuracy and transformations](https://www.w3.org/TR/WGSL/#floating-point-accuracy).

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
