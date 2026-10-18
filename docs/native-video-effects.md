# Compact native video input for GPU effect chains

Supported video-layer effect chains can start with compact YUV input and retain
their textures through the existing GPU effects. Correctness fixtures were checked
on an NVIDIA GeForce RTX 3050/Vulkan adapter; tests require a working device when
`EFFECTCRAFT_REQUIRE_GPU=1`, without restricting its name. Whole-project production
performance acceptance remains separate from these correctness checks.

## Change

The existing native video path uploaded compact opaque 8-bit YUV420 planes only
when a layer had no effect groups. A video layer with Native Bloom therefore took
the ordinary source path: CPU YUV conversion, a host RGBA-f32 image and its upload.
The path feeds the existing quantized GPU YUV conversion into `GpuFx`, runs
the renderer's normal evaluated effect stack, rebases its effect-space result and
uses the ordinary transform, opacity, matte and blend operations. Native Bloom's
algorithm, controls, quality and working-space behavior are unchanged.

Only enabled video effects supported by an active accelerator can enter this
attempt. Disabled effects and audio-only steps do not block it. CPU-only effects,
unknown effects and CC Composite keep the ordinary source pipeline; CC Composite
needs the original CPU snapshot. A GPU effect's unsupported control combination
can decline at execution. The new resident-only target then discards the partial
attempt without calling a CPU callback or initiating a float readback, and the
layer resumes its ordinary source path. Other resident targets retain their
existing mixed CPU/GPU behavior. Checked errors retain their contextual frame
diagnostics; a failed result cannot be published as a good frame or cached layer.

Source masks, active layer styles/non-default style blending, fields, proxies,
unsupported reduced-resolution resampling, source motion blur, frame blending, unsupported
footage interpretation/color conversion, stretch and time remap retain the
existing fallback guards. Missing video kernels/device limits also retain the
reference source path. This change applies to ordinary 2D video placement; it
does not add native source initialization to the 3D renderer.

Reduced previews of even-sized opaque 8-bit YUV420 can also keep their planes on
the GPU when the target is at most half the source size in each dimension. They
use the media source's existing half-size filter: a 2×2 luma average and separable
`[1 6 1]/8` centred chroma filter, with colour clipping after filtering. The
ordinary box-prefiltered bilinear warp handles any remaining resize, and source
bit-depth quantisation follows that resize before effects. This preserves the
CPU source's clipping and sampling order. It does not resize already clipped,
full-resolution RGB pixels in place of that filter.

Custom `FootageSource` implementations keep their existing sampler unless they
explicitly opt into `yuv420_half_size`; the native media pool and cancellation
wrapper expose that capability. Odd source dimensions and intermediate targets
larger than half retain the reference source path. The complete-resolution path
is unchanged. Production performance acceptance for this extension is pending.

Effects that read other layers or neighboring times still use their existing
host paths, which can request converted source images and additional transfers.
Native initialization does not make those dependencies resident or provide GPU
decoding. No new source/effect cache, codec, encoder or quality substitution is
introduced. Existing deferred miss gates and final-frame completion remain in use.

## Transfer arithmetic, not a benchmark

For 2560×1440 opaque 8-bit YUV420, used planes contain 5,529,600 bytes
(5.2734375 MiB; every plane is already 4-byte aligned). An RGBA-f32 source payload
is 58,982,400 bytes (56.25 MiB). A supported local effect chain should upload the
former and avoid the host float conversion/image. It still allocates the GPU
float conversion texture, effect textures and final output. Existing bounded
native upload reservations and effect planners are unchanged. These numbers are
payload arithmetic, not measured whole-process RAM, VRAM, latency or throughput.

The acceptance target is unchanged 2560×1440 at 120 fps (8.33 ms/frame), with
60 fps secondary and 144 fps optional stress. Maximum-quality exports retain
requested resolution, frame rate, color and Final effect quality. Eliminating a
conversion/upload stage does not establish those complete playback/export goals.

## Validation and required next checks

Passed with two build jobs, release LTO off/codegen16 and a separate correctness
target, with no GPU execution:

```text
cargo test --release --locked -p effectcraft-gpu native_video_effect_preflight_respects_disabled_audio_cpu_and_original_source_steps -- --nocapture
cargo clippy --release --locked -p effectcraft-gpu --all-targets -- -D warnings
```

The test verifies enabled/disabled stacks, an inactive accelerator, audio steps,
CPU-only effects, unknown IDs and original-source capture requirements.

The following four focused GPU fixtures passed individually in a coordinated
correctness window with `WGPU_BACKEND=vulkan`, `EFFECTCRAFT_REQUIRE_GPU=1` and
serial execution. Available-device requirements and the existing project-depth
parity tolerances apply to all adapters. The fixtures verify:

- Compare CPU and compact-source GPU pixels at 8/16/32 bpc, Bloom qualities/views,
  Add/Screen and a mixed Glow/Invert/Bloom stack, with transforms, opacity, linear
  blending and cold/repeated 120-fps source times. Retain original project-depth
  comparison tolerances. Require zero host float-source calls, one compact upload
  and only the final frame readback for these local chains.
- Verify resident-only declines call no CPU callback and cause no float transfer.
  Check masks/styles, CPU-only/original-source stacks, inactive accelerators and
  a dynamic CPU-only control keep the reference output rather than partial pixels.
- Require native display Bloom to publish without a float readback, and deferred
  checked export-pass completion to need only the final frame readback. Hostile
  Bloom controls must preserve the document, retain effect identity, publish/cache
  no failed result and allow later repair.
- Rerun the prior native YUV range/matrix/odd-edge/project-depth, interpretation,
  rate and media-failure regressions. Run the repository full gate before a commit.

## Production footage acceptance

Normal Windows x64 Thin LTO/codegen-one builds of `21680b74` and `f358d26f` used
one identical private 2560×1440/120 OBS H.264 project with Native Bloom applied to
the video layer. Quality/View were Final, radius 32, amount 100%, threshold 0.6,
soft knee/spread 0.5 and operation Add, with 32 bpc and the same working/blending
space. The two-second recording segment shows a mostly static shader settings
menu; it does not represent fast gameplay, difficult motion, 3D or the original
Hybrid MP4 container. No footage or reference pixels are shipped.

Full-resolution PNG comparisons at frames 0, 1, 61, 119, 120, 239 and backward
return to zero passed with a maximum one-code RGB difference and exact alpha.
Three unclipped EXR samples had a maximum recovered working-channel difference
of 3.58e-7, within the existing 32-bpc tolerance of 1e-3, with exact alpha.
Separate desktop renders retained the same one-code/exact-alpha bounds.

| Case | Before | Compact effect input |
|---|---:|---:|
| Headless Full GPU frame, cold | 202.07 ms | 56.14 ms |
| Headless Full GPU frame, warm sampled time | 221.93 ms | 58.58 ms |
| Headless Full GPU display pass | 177.26 ms | 26.32 ms |
| Warm headless upload/readback payload | 125.9 / 125.9 MiB | 5.3 / 56.2 MiB |
| Desktop viewer changes/s including waits, 12 s | 8.32 | 41.77 |
| Desktop completed holds, p50/p95/p99 | 116.93 / 134.70 / 160.90 ms | 23.74 / 26.29 / 122.68 ms |
| Desktop sampled peak process commitment | 6.96 GB | 4.82 GB |
| 120-frame native H.264 export job / whole process | 23.47 / 25.09 s | 7.63 / 9.11 s |
| Export sampled peak process commitment | 10.46 GB | 6.87 GB |

Headless numbers are medians of three repeated-time samples, not timeline fps.
The desktop and export comparisons are single sequential pairs on the RTX 3050;
they are not repeated medians or general hardware limits. Half-resolution retains
the source fallback: its GPU frame comparison was 44.34 versus 37.44 ms. The CLI
uses its existing source decoder; desktop counters separately confirmed native
hardware decoding. RAM measurements are process totals, not VRAM or queue budgets.

The visible, unminimized desktop window used explicit fast presentation, full
resolution, audio off and skip zero. All 3,631 candidate steady requested/shown
keys matched, with no stale or missing texture draws. Its slowed playback clock
still recorded two skipped selected frames and failed uncached 120 fps. Cached
playback shortened the range to 49 frames (0.408 seconds), then observed 119.95 fps
for 11.76 seconds with no skips, stale or missing frames. This is a short cached
loop, not an entire project. Different cold-phase cache residency makes the two
cache preparation times unsuitable for a startup-speed comparison.

The export retained 1440p/120, Final Bloom, 300 Mbit/s H.264 and stereo audio with
the same Windows encoder. Both outputs decoded all 120 frames with timing error
below one microsecond, limited-range BT.709 tags and 48 kHz stereo AAC. Decoded
audio matched over the first 48,000 samples. Compressed video A/B agreement was
54.29 dB PSNR / 0.99805 SSIM; lossless selected PNG/float comparisons above verify
the algorithm separately from compression. This establishes neither lossless
H.264 nor instantaneous export.

The same final build also rechecked ordinary video without effects: 100.59 fps
uncached over ten seconds versus the previous 100.30, and 119.95 fps on the short
cached loop, with no observed selected-frame skips or steady key mismatches.
Seven frames matched the external reference within one RGB code and exact alpha.
These results used explicit presentation/encoder overrides; automatic Windows
defaults use the same paths and require their own normal-launch acceptance.
