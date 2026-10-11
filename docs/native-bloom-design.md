# Native Bloom checked renderer

Status: the integration registers **Native Bloom** through checked CPU and GPU runners.
Adjustment-layer dispatch stays on resident textures; ordinary layers use a checked keyed
upload/readback, with deferred passes in browser workers. Production benchmarks, moving-text visual
acceptance and the complete repository gate remain required before release. The existing
Glow renderer keeps its previous behavior. This is original EffectCraft code; it does not
load commercial After Effects plug-ins or claim Deep Glow look, speed, preset or binary
compatibility.

## Public workflow references

[Deep Glow's product page](https://www.plugineverything.com/deep-glow) describes wide falloff,
HDR and smooth thresholding, gamma correction, masking, compositing, tint, aspect/angle,
chromatic aberration, and quality controls. The current
[Deep Glow 2 product page](https://aescripts.com/deep-glow/) and
[vendor releases](https://www.plugineverything.com/releases) describe image-based iris glow,
tonemapping, per-channel radii, lens dirt, inner/outer tint, and saturation bias. Directional,
vanishing, and camera-aware 3D extrusion belong to the separate Extrude product.

These are descriptions of user-visible behavior. No vendor code, algorithms, presets,
bundled textures, or artwork would be read or copied.

## Existing EffectCraft Glow

The CPU implementation is in `crates/effects/src/misc.rs`; GPU paths are in
`crates/gpu/src/effects.rs` and `crates/gpu/src/fx_stylize.rs`.

- Original-color RGB can retain values above one in a 32-bpc project. Eight- and 16-bpc
  projects quantize and clamp between effects, so they cannot preserve those values.
- Bright extraction uses a fixed ramp from Threshold to reference white. Its controls do
  not provide independent threshold smoothness or HDR thresholds above reference white.
- One Gaussian approximation, using three running-sum box passes per active axis, supplies
  the halo. This is already more efficient than a per-pixel, radius-sized convolution.
- A/B tint, arbitrary color curves, 17 blend modes, alpha/color selection, and horizontal/
  vertical blur are available. Both the normal and extended settings have GPU paths.
- Glow does not use the supplied working-space/linear-light metadata for extraction or
  scattering. Rec.709 luminance coefficients are applied directly to the current encoding.
- There are no effect-specific downsampling/quality, source-debug, or input-mask controls.
  Ordinary layer masks already precede effects; effect-specific masks require explicit
  semantics rather than assuming post-effect compositing masks gate the glow source.

The separate Glow optimization/safety patch removes per-pixel arbitrary-map allocation,
avoids clearing fully overwritten GPU bright textures, and bounds radius arithmetic. It
does not introduce the proposed Native Bloom renderer or establish visual parity.

## Checked integration and controls

Registry id: `ec.stylize.bloom`; display name: **Native Bloom**; category: **Stylize**.
The usual Effect menu, effect controls and `effect.apply` command expose the same animatable
parameters. Registration uses `EffectRender::Checked`; existing effect functions remain
`Legacy` runners without changing their output arithmetic. Both standalone
`effects::bloom::render_checked` and `gpu::bloom::render_checked` still return
`Result<Buf, BloomError>`.

The frame-local diagnostics slot preserves the first failure's comp, layer, effect uid/id,
exact Tick and bounded message across nested/collapsed compositions and motion blur. The
checked frame boundary tries a fresh CPU render with no shared layer cache after a GPU
limitation. It returns a terminal error if the reference renderer also rejects the request.
A completed CPU retry is explicit, so deferred workers do not repeat 24 obsolete GPU passes.
Caches and GPU displays refuse a failed attempt. The public legacy image boundary logs the
failure and supplies a tiny internal placeholder, rather than caching partially evaluated
pixels; user-facing preview, frame/alpha APIs, saved-frame, PSD/EXR and export callers use the
checked boundary and reject that placeholder.

Preview retains its last completed image and presents a render-error strip with hover
details and **Retry**. This error is independent of the expression-banner preference.
Failures are remembered for the content/time/view/options key, so repaint and prefetch do
not create an automatic retry loop; an edit creates a new key. Browser frame-worker failures
reach the same state through their contextual error message. Render Queue errors become a
failed item, including offloaded worker jobs; unfinished movies never reach a download sink.
File-system encoders write an exclusively created adjacent temporary file and replace the
destination only after encoding, flush, sync and close all succeed. A failed image sequence
can retain earlier, individually completed frame files; it is not a transaction over the
entire sequence. Existing files are never replaced with unfinished encoder output.

Renderer cache keys use the fork salt `effectcraft-5rod-render-2`, including footage/layer
and comp/frame identities. The cache file format stays `v1`; upstream-rendered pixels cannot
bypass the changed expression/effect semantics or the checked evaluation path.

The registered renderer is `gpu: true`. Checked kernels use their own explicit dispatch hooks
and never enter the legacy infallible batch runner. Adjustment layers reuse the GPU core's
exact kernels on resident textures, with no effect-stage upload/readback. Ordinary source
layers currently require one checked upload/readback for Bloom, separately from adjacent
legacy effect batches; this increment does not claim an entire ordinary layer stack stays
resident. Deferred readbacks validate controls before cache lookup, dimensions, geometry and
byte length before fallible image allocation, and raise the existing miss gate while pending.
Failures still select a fresh checked CPU frame retry; neither backend lowers export quality
to satisfy a rejected memory/work request. The standalone GPU helper requires synchronous
readback, while the registered dispatcher supports deferred devices.

| Control | Default | Accepted values |
| --- | --- | --- |
| Radius (outer sigma) | 32 | 0–1024 layer pixels; radius × render scale must be finite and at most 4096 rendered pixels |
| Exposure | 0 | −16 to 16 stops; affects Glow/Final, not the emission debug view |
| Amount | 100% | 0–1000%; an explicit fade-to-zero control |
| HDR Threshold | 1 | 0–1,000,000 in linear working-space reference-white units |
| Soft Knee | 0.5 | 0–1; knee half-width is Threshold × Soft Knee |
| Halo Spread | 0.5 | 0–1; interpolates two normalized component weight sets |
| Quality | Balanced | Draft / Balanced / Final |
| View | Final | Source Emission / Glow Only / Final |
| Composite | Add | Add / Screen |

Control values with the wrong type, nonfinite numbers, fractional choices, invalid scales or
values outside the stated ranges return an error. Alpha is restricted to 0–1; nonfinite pixel
channels become zero, and straight input/decoded RGB is capped to magnitude `1e12` before
scattering. This is a documented malformed-input bound, not an HDR reference-white clamp.

1. Create a bounded shared plan before any casts, dimensions, texture creation, or loops.
   It defines the effective radius, support/padding, pyramid dimensions, normalized weights,
   sample centers, and quality limits. Validate finite values, products, checked dimensions,
   total scratch memory, and work counts. Return an actionable render error for unsupported
   requests; do not silently select a lower export quality.
2. Decode straight RGB into the working space's linear light when needed, using
   `EffectEnv.working_space`, `working_linear`, and the existing color conversions. A linear
   working space must not be decoded again. Use the working space's XYZ Y row for luminance.
   Convert authored tint colors consistently and preserve valid wide-gamut values.
3. Extract emission with an independently defined, monotone soft knee around an absolute
   HDR threshold. Preserve selected RGB radiance above one. Respect input alpha exactly
   once; zero-alpha pixels must contribute no emission, including malformed hidden RGB.
4. Extract before area downsampling. Averaging the source first can erase isolated bright
   pixels below the threshold. Use linear-light, energy-preserving reductions and consistent
   pixel-center/offset rules for odd dimensions and fractional preview resolutions.
5. Construct an original halo from a normalized mixture of small and broad Gaussian scales.
   Evaluate the broad components in a bounded multiresolution pyramid. Document its profile
   and measure it against an independent full-resolution reference; do not describe it as
   the vendor's inverse-square implementation or as a validated physical simulation.
6. Reuse scratch storage. Avoid eagerly padding and blurring every full-resolution
   intermediate; represent transparent borders in sampling/level bounds. Upsample and
   composite the final result once, in linear light, then encode back to the working space
   if necessary. GPU and CPU fallback implement the same plan and color semantics.

## Implemented profile, color and budgets

For nonzero radius, the original profile is a four-component normalized Gaussian mixture
with nominal sigmas `Radius/8`, `Radius/4`, `Radius/2`, and `Radius`. At Spread zero, weights
are `[0.55, 0.30, 0.12, 0.03]`; at Spread one they are `[0.05, 0.15, 0.30, 0.50]`. The last
weight is the remaining normalized mass. Each Gaussian uses the existing three-box
approximation. This is not a physical scattering model or a vendor profile.

| Quality | Minimum reduction factor for the smallest component | Maximum local sigma |
| --- | --- | --- |
| Draft | 4 | 4 pixels |
| Balanced | 2 | 8 pixels |
| Final | 1 | 16 pixels |

The first reduction factor doubles until the local sigma fits; subsequent components each
double it again. Thus every component uses the same bounded local box radii. Radius zero
uses one unblurred component at full resolution. Emission is extracted before reductions.
Odd missing reduction cells are transparent, and bilinear reconstruction uses cell centers.
This preserves integrated energy; it introduces grid-dependent profile/position error at
coarse quality, which still needs measured visual acceptance. Quality is independent of
Full/Half/Quarter render scale. No preview/export quality shortcut is implemented.

Extraction decodes straight RGB only when the working space is encoded, uses that space's
XYZ Y coefficients for an absolute HDR threshold, and multiplies emission by input alpha
once. A quadratic knee connects to the linear excess above threshold. Scattering and
compositing remain in the same linear working-space primaries, followed by one encode.
Add sums premultiplied radiance. Screen uses the original HDR extension
`S + G - clamp(S, 0, alphaS) × clamp(G, 0, alphaG)` per channel: the interaction is bounded
to reference white while excess HDR radiance survives. Both final operations use union alpha
`alphaS + alphaG × (1 - alphaS)`; Glow Only preserves the halo's premultiplied alpha.
Source Emission shows the extracted source before gain or scattering. Adjustment-layer
output stays in its input bounds; ordinary layer output grows to the shared support.

Before allocation, the shared plan caps every image/atlas dimension at 16,384 pixels and
every individual area at 32 × 1024 × 1024 pixels. Its conservative total byte estimate is
limited to 1536 MiB, including six input-sized buffers, four buffers per padded component,
all downsample levels, the atlas, and three output-sized copies at 16 bytes per pixel. This
covers CPU blur accumulators and GPU upload/readback/staging headroom for one render; it
does not account for unrelated renderer caches or concurrent layers. A second limit of
2,000,000,000 work units charges 32 per input pixel, 48 per padded component pixel, four
per reduced pixel, and 64 per output pixel. These are deterministic planning units, not a
latency guarantee. Device texture limits and readback capability are checked separately.

CPU image/blur/column-accumulator allocations use fallible reservation. GPU extraction,
reduction, finish and box passes write every output texel and may use uncleared scratch;
padded levels and the partially copied atlas are explicitly cleared. Neither backend
allocates a separate full-resolution padded emission image per component. Larger supported
numeric radii can still be rejected when output support or aggregate budgets exceed the
limits; rejection never silently lowers quality. Total application memory and allocation
failure outside this checked core remain separate renderer work.

## Focused verification and remaining checks

Eight CPU tests cover hostile controls/dimensions/storage/offsets, deterministic normalized
plans, continuous monotone thresholds, exact single alpha application, HDR impulse energy,
an independent full-resolution Gaussian-mixture energy/second-moment reference, odd edges
and preview scales, equivalent physical colors across working spaces, explicit views and
compositing, zero Amount and adjustment bounds. Two raster tests compare checked box passes
to the existing filter and reject malformed images/radii. Three GPU tests cover checked
CPU parity across four working-space cases, all qualities and views, Add/Screen, HDR,
alpha, odd dimensions, nonfinite hidden pixels, cold/warm NaN-poisoned scratch, transparent
reuse, rejected hostile requests, zero Radius/Amount and adjustment-layer bounds. The eight
CPU and two raster tests pass in the release test profile with Thin LTO disabled. All three
GPU tests pass on NVIDIA GeForce RTX 3050 (Vulkan), with `EFFECTCRAFT_REQUIRE_GPU=1` and
`WGPU_BACKEND=vulkan`. The 48-case comparison matrix passed both cold and warm scratch
plus transparent reuse, without changing the RGB/alpha tolerances. The required-GPU setting
prevents unavailable devices or kernel initialization errors from silently taking a skip path.
The focused GPU run passed 3/3 in 1.48 seconds; this is test duration, not a performance claim.

The resident dispatch increment passes seven GPU tests on the same required NVIDIA/Vulkan
adapter, plus the registry badge audit and the eight CPU core tests. The original 48-case
HDR/color/quality/view matrix remains unchanged. New resident tests verify input preservation
and zero effect-stage host transfers, Source Emission/Glow Only/Final, ordinary versus
adjustment bounds, deferred readback failure and malformed dimensions/geometry/storage,
rejected nonfinite/enormous controls before transfers, transformed ordinary mixed stacks at
8/16/32 bpc and half resolution, warm-cache equality, contextual rejection and cache repair.
The adjustment scene uses only the final frame readback and completes deferred rendering in
two passes without a CPU retry. These are correctness/transfer checks, not timing benchmarks.

The independent profile test initially allows 25% error in radial second moment for the
three-box/multiresolution approximation; it is not a pixel-exact Gaussian claim. Energy
retains the 1% target, and GPU parity uses the tolerances below. Moving text, transformed
layers, masks, whole-renderer fallback/export error presentation and performance acceptance
remain outside the core matrix. The integration adds checked fallback/preview/export/cache
regressions; full production visual and performance acceptance is still outstanding.

The integration's focused regressions pass for frame-local diagnostic bounds, nested/collapsed
and motion-blurred failures, failed-pixel cache rejection and repair, fork cache identity,
legacy frame equality, Session frame/alpha/PSD/EXR/snapshot rejection, remote worker errors,
immediate completion after a CPU retry, real synchronous/deferred GPU adjustment behavior,
preview terminal-state recovery, error/Retry automation rectangles, JSON browser-frame
failure/recovery, offloaded CPU/deferred-GPU Render Queue failure, late movie failure and
preserved destinations, failed final rename, and finished-file-only sinks. A failed export may
publish its completed `.txt` failure log; it cannot publish an unfinished movie. Browser
worker tests run natively with actual protocol round trips and the NVIDIA Vulkan adapter;
this increment has not run browser automation in a real browser. Selected-crate release
all-target clippy is clean with `-D warnings`; the full repository gate remains the parent
integration's responsibility. Local checks disable Thin LTO for compiler throughput, so
none of these results are production performance measurements.

## Additional controls after the core passes

- Source-mask selection must gate emission **before** scattering, so the halo can extend
  beyond the selected source region. External-layer masks need defined transform, source,
  alpha/luminance, missing-layer, and offscreen behavior, using existing host services and
  cycle limits. A separate output mask, if offered, must be labeled distinctly.
- Aspect and angle, per-channel radius, inner/outer tint, and saturation bias should reuse
  the same extraction and pyramid where possible. Measure any extra passes or resampling.
- Image-kernel glow and lens dirt are later features with bounded kernel/image sizes and
  explicit source-layer sampling. Use only independently generated/openly licensed or
  user-provided media. They are not required to establish the first wide-halo bloom.
- Reuse the project's existing output color/tonemapping policy first. Any effect-local
  tonemapping must be explicit, avoid applying the project transform twice, and keep an
  option that preserves scene-referred HDR for downstream effects/export.

## Acceptance scenes

All scenes use original synthetic pixels, original shapes/text, or openly licensed media.
Numeric reference tests use scene-referred linear values; display screenshots supplement
those tests rather than replacing them. The following tolerances are initial engineering
targets to confirm against the independently specified reference.

| Scene | Settings and required evidence |
| --- | --- |
| HDR impulses | RGB and white impulses at radiance 1, 4, and 16; radii 16, 64, and 256; alpha 1. Show core/halo profiles, no unintended RGB clipping, and normalized energy within 1% of the reference for a centered source. |
| Threshold motion | One-pixel lines and antialiased moving text at 0.125 pixels/frame; brightness 0.98–1.02 around threshold 1. Measure emission and halo energy over time against a full-resolution reference. Soft Knee must be continuous and monotone without history-dependent filtering. |
| Color management | Equivalent physical colors encoded as sRGB, linear sRGB, Rec.2020, and ACEScg. Verify transfer curves, luminance selection, and no double decoding. Compare the same linear output, rather than identical RGB triplets in different primaries. |
| Alpha and masks | Semi-transparent bright edges, zero-alpha hidden RGB, moving/offscreen source masks, and missing/self-referencing mask layers. Require no alpha-squared darkening, leaked masked emitters, NaNs, or recursion; define the missing-source result explicitly. |
| Composition | Black, white, and checkerboard backgrounds; Add, Screen, Glow Only, and fade to zero. Check premultiplication, exported alpha, exact zero-input emission, and output bounds. |
| Transforms | Ordinary camera-transformed text/shape layers, translated/scaled layers, fractional frame rates, and motion blur. Confirm layer-space radius behavior and no clipped transformed halo; this is not an extrusion-glow feature. |
| Geometry and fallback | Odd dimensions such as 1919×1079, fractional preview resolutions, large supported radii, and GPU-disabled/failure fallback. Require consistent support/offset/sample centers and finite output. |

For equivalent GPU/CPU plans, initially target RGB error at most `1e-5 + 1e-3 * abs(reference)`
and alpha error at most `1e-4`. Test repeated warm frames with poisoned scratch storage and
compare cold/warm results. Keep any driver-specific tolerance narrow and justified by data.

## Performance acceptance

Benchmark 1080p and 4K at radii 8, 64, 256, and the supported upper range for each quality,
using one effect and a representative stack. Report first-use latency separately from warm
p50/p95 latency, GPU versus CPU fallback, peak scratch/total memory, device/driver, project
color settings, and the exact production profile. Include thin text, sparse impulses, and
full-frame footage so a sparse-scene optimization cannot hide the expensive case.

Require all quality scenes to pass before treating a faster result as an improvement. Set
performance targets from those measurements and the existing Glow baseline; there are no
speed or real-time claims yet. The checked core's per-render budget is implemented in source;
general whole-renderer image budgeting remains separate work. The checked integration
surfaces supported-core rejection through preview and export, without a speed claim.
