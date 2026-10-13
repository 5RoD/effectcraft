# Windows hardware export

The Windows H.264 render-queue backend uses the operating system's `MediaTranscoder` with
hardware acceleration requested. It does not link, bundle or run FFmpeg. Software H.264,
other output formats and browser exports retain their existing encoders.

This backend is opt-in while real-project performance and quality acceptance is underway:

| `EFFECTCRAFT_H264_ENCODER` | Behavior |
|---|---|
| unset, `software`, `off` | Existing software encoder |
| `hardware` | Request Windows encoding; report an unsupported profile or encoder error |
| `auto` | Request Windows encoding; fall back to software if the profile is unsupported before rendering starts |

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
speed improvement is claimed yet.

Public API references: [MediaStreamSource](https://learn.microsoft.com/en-us/uwp/api/windows.media.core.mediastreamsource),
[MediaTranscoder](https://learn.microsoft.com/en-us/uwp/api/windows.media.transcoding.mediatranscoder),
[AAC encoder](https://learn.microsoft.com/en-us/windows/win32/medfound/aac-encoder).
