# Video upload and export processing measurements

Measured on 15 October 2026, on the i9-10900F (10 cores / 20 threads), RTX 3050
8 GiB and 32 GiB RAM. The comparison is between production Windows builds of
`3f95f50f` (upstream 0.10 integration) and `7a1bf195` (direct video staging and
overlapping export frame processing). Both use Thin LTO, one codegen unit and
the same default hardware decoding, encoder and viewer settings.

Each case has three pairs, with the order alternating between pairs. Compiler
and GPU test jobs were idle. OS file caching and background applications were
uncontrolled. These numbers describe this host and these workloads.

## Uncached timeline playback

A bounded local gameplay excerpt supplies 2560×1440 H.264 at 120 fps. Each run
purges the application cache, disables its disk cache, requests Full resolution,
and observes ten seconds of the actual desktop viewer. Audio and monitor scanout
are outside this measurement. The private source footage is not committed.

| Measurement | Before | After |
| --- | ---: | ---: |
| Median distinct displayed textures/s, including waits | 94.11 | 110.46 |
| Range across three runs, textures/s | 93.24–94.85 | 110.21–110.77 |
| Median p99 completed texture hold, ms | 51.75 | 40.60 |
| Median mean upload CPU time per upload, ms | 2.21 | 1.08 |
| Maximum sampled process private memory, GiB | 3.82 | 3.85 |

Cadence improves by about 17.4%; upload CPU time falls by about 51.2%. Hardware
decoding remains active without fallbacks, and compositor readback bytes remain
zero. The 120 fps target is still unmet: the observed frame holds include spikes
of roughly 67 ms. Sustained audio-synchronized playback and display scanout still
need separate acceptance. No aesthetic quality score is assigned to this footage.

The upload now assembles checked, padded YUV planes directly in the GPU queue's
staging view. It removes a temporary concatenation allocation and one full-plane
CPU copy. The bytes transferred to the GPU are unchanged. Regression tests cover
odd sizes, ranges, matrices, project depths and two staged frames before submission.

## Export

Each project exports 120 frames at 2560×1440 / 120 fps, H.264 at 300 Mbps with
audio. One project has no added effects; the other enables 32-bpc Native Bloom.
The table reports medians, except memory, which is the largest sampled private
memory value across the three runs. Process duration includes startup and shutdown.

| Measurement | Before | After |
| --- | ---: | ---: |
| Plain project, render-queue seconds | 8.671 | 7.990 |
| Plain project, whole-process seconds | 10.492 | 10.051 |
| Plain project, maximum private memory, GiB | 2.32 | 2.04 |
| Native Bloom, render-queue seconds | 9.881 | 9.467 |
| Native Bloom, whole-process seconds | 11.737 | 11.272 |
| Native Bloom, maximum private memory, GiB | 2.55 | 2.33 |

Render-queue duration falls by about 7.9% and 4.2%, respectively; whole-process
duration falls by about 4.2% and 4.0%. Every paired run improves. CPU time also
fell for the plain project; the Bloom CPU-time median rose from 19.344 to 19.859
seconds despite its shorter elapsed duration, so this is not a universal CPU-work
reduction. The comparison retains the upstream GPU concurrency cap and bounded
frame window. Completed frames enter CPU processing while later frames render,
using the existing Rayon pool. For the Windows encoder, this overlaps CPU frame
preparation; it does not establish concurrent rendering and hardware encoding.

All twelve output files were independently checked for 120 decoded video frames,
their exact 120 fps timestamps, 2560×1440 geometry, limited-range BT.709 metadata,
and decoded 48 kHz stereo audio. Hardware decoding had no fallbacks. Requested
output settings were retained; the user will judge visual quality separately.

This change improves the bounded 0.10 pipeline. The earlier upstream integration's
export throughput regression against the 0.8-based build remains a separate item.

## Correctness and remaining work

The frozen-source full gate passed 3,715 tests with zero failures and 29 explicitly
ignored tests, strict native Clippy, dependency layering, 31 attributed assets and
32 WebAssembly crates. The normal release desktop then passed 40 live checks of
3D handles, shortcuts, undo/redo and project save/reopen. Generated UI screenshots
were inspected locally. GPU telemetry was sampled device-wide and includes other
applications; process CPU and memory samples belong to the tested child process.

The local reports retain individual runs, binaries and hashes. Full-recording
Hybrid MP4 behavior, sustained audio/video synchronization, more complex projects,
remaining native workflow gaps and the final installation/rollback checks remain
open. The installed application and the original recording are unchanged.
