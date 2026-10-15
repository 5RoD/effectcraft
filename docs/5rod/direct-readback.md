# Direct native GPU readback

Native synchronous image downloads copy mapped GPU staging rows directly into the
final image allocation. Previously the mapped rows were packed into a temporary
byte vector and then copied into the image. At 2560×1440 with four f32 channels,
the removed temporary allocation and copy each span 58,982,400 bytes per frame.
The GPU transfer size, float precision and rendering work are unchanged.

This uses the existing bounded readback registry, device-failure bridge, deadline
and staging-buffer pool. Mapping completion signals the waiting worker; it reads
the mapped range and unmaps the buffer before reuse. Asynchronous and browser
readbacks retain their owned-byte callback contract. No additional worker is
created, and the GPU concurrency limit is unchanged.

Layout validation checks every source row and the exact destination length before
publishing pixels. Regression checks cover truncated rows, invalid dimensions and
stride, arithmetic overflow, extra mapped padding, exact HDR float bytes and
repeated downloads through reused staging buffers. Existing deadline,
cancellation, device-failure and deferred-render tests remain required.

## Production export measurement

Measured on 15 October 2026 on the i9-10900F, RTX 3050 8 GiB and 32 GiB RAM.
Clean production builds of `00ad74a0` and `b624b3d7` both use Thin LTO, one
codegen unit, default hardware decoding/encoding and the existing worker limits.
Each project has three alternating before/after pairs, with compiler and GPU
test jobs idle. OS caching and background applications were uncontrolled.

Each export contains 120 frames of a bounded local 2560×1440 / 120 fps gameplay
excerpt, H.264 at 300 Mbps with audio. Native Bloom uses the 32-bpc effect path.
Durations below are medians; private memory is the largest sampled process value
across the three runs. Process duration includes startup and shutdown.

| Measurement | Before | After |
| --- | ---: | ---: |
| Plain project, render-queue seconds | 7.405 | 5.854 |
| Plain project, whole-process seconds | 9.182 | 7.676 |
| Plain project, maximum private memory, GiB | 2.04 | 2.01 |
| Native Bloom, render-queue seconds | 8.955 | 7.411 |
| Native Bloom, whole-process seconds | 10.690 | 9.080 |
| Native Bloom, maximum private memory, GiB | 2.29 | 2.26 |

Render-queue time falls by about 20.9% and 17.2%, respectively; whole-process
duration falls by about 16.4% and 15.1%. Every paired queue and process duration
improves. All twelve outputs decode to 120 video frames with the expected
timestamps, geometry, limited-range BT.709 metadata and 48 kHz stereo audio.
Hardware decoding reports no fallbacks. No aesthetic quality score is assigned.

The frozen-source full correctness gate passed 3,735 tests with zero failures
and 29 explicitly ignored tests, strict native Clippy, dependency layering,
31 attributed assets and 32 WebAssembly crates. Local reports retain the raw
runs, output files and source/build hashes. These bounded exports do not prove
full-recording Hybrid MP4 behavior, sustained audio/video synchronization,
real-time 120 fps playback, or performance on every project and machine.
