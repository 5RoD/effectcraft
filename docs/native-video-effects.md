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
reduced-resolution resampling, source motion blur, frame blending, unsupported
footage interpretation/color conversion, stretch and time remap retain the
existing fallback guards. Missing video kernels/device limits also retain the
reference source path. This change applies to ordinary 2D video placement; it
does not add native source initialization to the 3D renderer.

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

After correctness, build identical normal-profile baseline/candidate applications
and use private OBS 1440p120 footage with representative local effect chains
in a separate idle-machine window. Capture completed output fidelity, upload bytes,
internal/final readbacks, cold and cached native painted-frame cadence and process
memory with honest labels. Measure whole maximum-quality export separately; never
infer it from GPU dispatch timing. No performance gain is claimed for this candidate.
