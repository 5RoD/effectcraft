# Native Bloom proposal

Status: design only; this effect has not been implemented or benchmarked. It would be an
original EffectCraft effect, separate from the existing Glow. It does not load commercial
After Effects plug-ins or claim Deep Glow look, speed, preset, or binary compatibility.

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

## First implementation

Proposed registry id: `ec.stylize.bloom`; display name: **Native Bloom**. Registration should
make it available in the effect menu, search, and ordinary effect commands. All controls
are normal animatable properties. Existing Glow projects retain their existing effect.

Start with Radius, Exposure in stops, HDR Threshold (1 = reference white), Soft Knee,
Halo Spread, Draft/Balanced/Final Quality, and Source/Glow/Final views. Add and Screen
compositing and a glow-only result should be explicit. Defaults and parameter limits are
to be chosen from the acceptance scenes, rather than copied from a commercial product.

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

Draft/Balanced/Final must have documented sampling/work limits and deterministic results.
Quality is independent of the viewer's Full/Half/Quarter resolution. Final exports use the
selected export quality; preview-quality shortcuts must not silently reach exported frames.

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
speed or real-time claims yet. General image-allocation budgeting remains a renderer task
and is a prerequisite for accepting extreme-size bloom requests safely.
