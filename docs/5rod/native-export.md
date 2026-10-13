# Windows hardware export

The Windows H.264 render-queue backend uses the operating system's `MediaTranscoder` with
hardware acceleration requested. It does not link, bundle or run FFmpeg. Software H.264,
other output formats and browser exports retain their existing encoders.

Windows H.264 file exports request this backend by default after the production
footage checks below. Unsupported profiles fall back before rendering starts:

| `EFFECTCRAFT_H264_ENCODER` | Behavior |
|---|---|
| unset, `auto` | Request Windows encoding; fall back to software if the profile is unsupported before rendering starts |
| `software`, `off` | Existing software encoder |
| `hardware` | Request Windows encoding; report an unsupported profile or encoder error |

An invalid value warns and retains software export. The variable applies to H.264 only.
Requesting acceleration alone does not prove a particular GPU encoded the file. Device
selection and whole-export speed need observation in the production application.

## Delivery and fidelity

The existing job pipeline renders every frame, including effects, camera selection, render
settings overrides, color depth, output channels, crop and resize. H.264 receives opaque
BGRA8 pixels over the composition background, or the requested alpha matte. Source/effect
errors remain errors. Fields currently use software export rather than being mislabeled as
progressive video. Output-size/rate preflight limits hardware requests to even dimensions
of at least 64 pixels within H.264 level 5.2. Windows may reject further driver-specific
profiles; no successful output is fabricated in that case.

Video uses High profile, the output module's target bitrate, progressive scan, square output
pixels and BT.709 limited-range encoded color, matching the software path's output contract.
Frame timestamps and durations are calculated from the rational rate independently in
100-nanosecond units. A 120 fps sequence alternates 83,333/83,334-unit durations and ends at
the exact rational boundary rather than accumulating rounded frame durations.

Optional audio uses the existing composition mixer, with its selected span and mono/stereo
channels, supplied as 32-bit float PCM to the system pipeline. AAC output supports 44.1 or
48 kHz at the existing 160 kbit/s per channel. Other audio rates use software in `auto` mode.
AAC's final packet can add a small amount of padding to the container duration.

Callbacks only enqueue deferred requests. A four-request bounded queue sends them to the
job's caller, which owns the borrowed renderer and progress callback. Full video frames do
not accumulate in the request queue. Rendering uses existing parallel batches, capped by
an approximate 512 MiB source-image plus pixel-buffer budget; this does not cap all codec,
GPU, layer-cache or process memory. Audio is produced in at most 1,024-frame chunks.

## Cancellation and files

The native encoder writes an exclusively created adjacent provisional file. Only successful
sample delivery, encoder completion, stream flush/close and filesystem sync permit the final
rename. A failed or cancelled encode cannot replace a previous destination. Empty native
outputs are rejected. Only successfully published new files become cancellation cleanup
targets; previous destinations are excluded.

Progress and cancellation are polled while preparing or waiting for requests, and after
each delivered frame. Preparation and inactive sample delivery have bounded waits. Callback
exceptions are caught as a last-resort boundary, recorded and reported to the source. Deferred
requests and event handlers are released on failure. Cancellation during a rendering batch
is observed when that batch returns, as in the existing export pipeline.

## Validation

An explicitly run Windows test renders a generated 640×360 composition at 120 fps with a
midpoint layer change, output crop/resize and distinct stereo tones. External FFmpeg/ffprobe oracles check all
120 decoded frames, frame rate, channel layout, visible first/last frames and audio signal
alignment/quality. The same test cancels over an existing destination and checks its bytes
and provisional-file cleanup. Timestamp overflow and long rational-rate calculations have
ordinary unit tests; native publication has synthetic file-safety tests.

A separate generated 1440p/120 audio/video probe delivered 120 frames and 48,000 stereo
samples, decoded correctly and returned cleanly on deliberate cancellation. These are
correctness checks, not real-project export speed or maximum-quality acceptance. No export
speed improvement is claimed by those generated checks.

## First production footage comparison

A clean Windows x64 Thin LTO/codegen-one build of `21680b74` rendered the same
private OBS H.264 2560×1440/120 source with GPU rendering and stereo audio. Each
run exported the first 120 frames at its original resolution/rate, without effects.
The encoder was selected explicitly in this comparison, before the Windows default
changed to automatic native selection. The CLI binary SHA256 was
`0fee76b7a8381bc52705db1acb1a7df3aa1067f6abd8005b0c392bb4d4817eeb`.

| Target bitrate | Windows job / whole process | Software job / whole process | Windows / software PSNR | Windows / software SSIM |
|---|---|---|---|---|
| 80 Mbit/s | 5.873 / 7.377 s | 15.325 / 16.797 s | 42.57 / 41.41 dB | 0.9602 / 0.9506 |
| 300 Mbit/s | 5.521 / 6.992 s | 18.198 / 19.649 s | 49.79 / 46.37 dB | 0.9928 / 0.9838 |

The render-job comparison improved by 2.61× and 3.30× in these single paired runs;
whole-process improvement was 2.28× and 2.81×. This is a short clip with startup
costs, not a repeated median, long-export throughput, lossless quality or difficult
effect/3D acceptance. PSNR/SSIM compare every decoded frame with the source through
external FFmpeg; the private footage remains outside the repository.

External ffprobe counted exactly 120 frames at 120/1 with a maximum timestamp
error below one microsecond, plus stereo AAC at 48 kHz. The central decoded audio
correlated above 0.999 with the first source track on the Windows path, and above
0.998 on software. Existing mixing remains responsible for the selected tracks.
Read-only NVML observations of the production CLI's own PID confirmed a 2560×1440
H.264 encode session on the RTX 3050; the software control had none. That separate
identity check is not timing evidence.

Observed peak process commitment was about 3.57/3.64 GiB on Windows versus
5.21/5.38 GiB on software at the two bitrates. These sampled process totals include
rendering, decoded media, audio and codec buffers, not just the request queue. The
backend still transfers final rendered frames into the operating system encoder;
it does not establish a GPU-texture-to-encoder path or instantaneous export.

The subsequent `f358d26f` build also checked a real video-layer Native Bloom export
at Final effect quality and 32 bpc. The source remained 2560×1440/120 with stereo
audio, and the 120-frame H.264 target remained 300 Mbit/s. Compact GPU effect input
reduced the render job from 23.473 to 7.626 seconds and whole-process time from
25.089 to 9.106 seconds. This measures a renderer improvement with the same native
encoder, separately from the software-versus-native encoder comparison above.
All decoded frames, timestamps and BT.709 tags passed; decoded audio was identical
over the first 48,000 samples. See [video effect acceptance](../native-video-effects.md)
for fidelity, resource measurements and the limited footage scope.

## Normal Windows default acceptance

The clean Thin LTO/codegen-one `a3a0c6ab` production CLI repeated the 120-frame
2560×1440/120, stereo-audio, 300 Mbit/s job with encoder and graphics-backend
overrides unset. The job took 5.911 seconds; whole-process time was 7.957 seconds.
A separate read-only observation of that production CLI's own PID confirmed a
2560×1440 H.264 NVIDIA encoder session on the RTX 3050 without an override.

External validation counted every frame at 120/1, with timestamp errors below
one microsecond, limited-range BT.709 tags and stereo AAC at 48 kHz. Decoded video
scored 49.79 dB PSNR and 0.9928 SSIM against the source; central audio correlation
exceeded 0.999 on both channels. This is one short default acceptance run, not a
new paired speed comparison, benchmark median, long export or lossless result.
The earlier generated cancellation/preservation fixture also passed with the
encoder override unset. CLI source decoding retains its existing software path;
the desktop native decoder is a separate registration.

Public API references: [MediaStreamSource](https://learn.microsoft.com/en-us/uwp/api/windows.media.core.mediastreamsource),
[MediaTranscoder](https://learn.microsoft.com/en-us/uwp/api/windows.media.transcoding.mediatranscoder),
[AAC encoder](https://learn.microsoft.com/en-us/windows/win32/medfound/aac-encoder).
