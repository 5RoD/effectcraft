# Retiming development measurements

The measured increment shares unique source and derived frames, prepares each motion field
once, streams shutter samples in their original accumulation order and reuses GPU scratch
textures. Controls and public plans have checked storage/work limits. These limits are per
effect invocation; decoder caches, the renderer and concurrent exports need separate budgets.
See [safety and implementation](../timewarp-performance-safety.md) and the
[benchmark harness](../timewarp-benchmark.md).

## Frozen comparison

Both binaries used Rust 1.99.0, normal release Thin LTO and one codegen unit on the i9-10900F,
32 GiB RAM and RTX 3050 Vulkan device. The isolated baseline was `bc553f203802b04ae89a22985cd56acb9c66c171`;
the candidate applied the memory/safety increment to it. Compiler and other agent GPU jobs
were idle. Inputs were original generated premultiplied HDR shapes/texture at 2560×1440 and
120 fps. Normal desktop background activity remained. This comparison predates the root
0.8 integration and does not establish its production speed.

Cold medians pool three rounds of three distinct frames per setting. Nine frames are not
enough to establish tail latency. GPU times include upload, matching preparation, dispatch,
completion and final readback.

| Method | Shutter samples | CPU before → after (ms) | GPU before → after (ms) |
| --- | ---: | ---: | ---: |
| Whole | 1 | 126.84 → 33.60 | 176.48 → 121.05 |
| Whole | 4 | 295.29 → 68.94 | 281.96 → 116.39 |
| Frame Mix | 1 | 134.91 → 43.18 | 249.38 → 191.16 |
| Frame Mix | 4 | 327.57 → 152.22 | 381.91 → 205.83 |
| Pixel Motion | 1 | 269.09 → 112.45 | 404.06 → 274.02 |
| Pixel Motion | 4 | 564.22 → 228.68 | 885.50 → 361.66 |

Observed process peak working set, median across the six-case runs, fell from 853.9 to
329.1 MiB for CPU (61.5%) and 997.8 to 562.6 MiB for GPU (43.6%). The native peak counter
was observed every 20 ms. This includes initialization/pools and is not per-effect live
allocation or per-process VRAM. Pixel Motion with four samples reduced uploads over three
frames from 27 to 10 (1,592,524,800 → 589,824,000 bytes); three readbacks remain.

Candidate-only 64-sample runs completed with 329.3 MiB CPU and 568.9 MiB GPU observed peaks.
CPU Whole/Frame Mix/Pixel Motion/Extreme medians were 449/1938/2122/3187 ms;
GPU medians were 233/291/634/672 ms. The historical high-sample guard rejected the case before
allocation, so no historical 64-sample timing or output comparison exists.

**None of the measured settings met the 8.33 ms deadline for uncached 120 fps playback.**
The fastest cold Whole CPU frame took 26.25 ms; generated source images alone took roughly
10–20 ms. These are synthetic render-stage measurements. Actual codec/timeline/audio
playback and whole maximum-quality exports remain unaccepted.

## Output and correctness limits

The tiny matrix compared 39 outputs/91,884 float channels: CPU before/after and GPU
before/after were exact; maximum CPU/GPU difference was 4.17e-7. The first 1440p round
compared 18 outputs/265,420,800 channels and was also exact within each backend. All twelve
same-backend checksum comparisons across cold/warm repetitions matched.

The existing strict CPU/GPU check at 1e-3 **failed in both baseline and candidate**:
127 channels exceeded tolerance, maximum difference 0.12000397, all in Pixel Motion.
Whole and Frame Mix passed. The worst pixel at (1233,320), frame 1, one sample was CPU
`[0.18000397,0.17143029,0.09,0.6]` and GPU `[0.06,0.12,0.09,0.6]`.
The candidate-only 64-sample comparison also failed in Pixel Motion/Extreme:
1,036/176,947,200 channels exceeded tolerance, maximum 0.009130299.

Failure captures and thresholds were retained. Explicit timing-only local runners collected
subsequent cold/warm rounds after the first strict failure; those measurements do not change
the fidelity result. The frozen increment also retains the old source-fetch microsecond
quantization. Exact Tick reads and GPU motion precision require separate corrections and
acceptance. No commercial plug-in fidelity claim follows from these measurements.

Focused isolated checks passed: eleven effects/golden, one export-preservation, one disk-cache
version and seven required RTX 3050/Vulkan tests, plus four-crate strict all-target Clippy.
A small debug regression reproduces the former coordinate-overflow panic and verifies the
checked error. The integrated root build passed the complete repository gate on 2026-10-12:
2,980 tests passed, 27 optional tests ignored, formatting, strict native Clippy, layers,
31 attributed assets and 32 WebAssembly crates passed. The WebAssembly check retains the
upstream native-shortcut-state dead-code warning. Test runs disabled Thin LTO; the frozen
performance binaries above use normal Thin LTO. Integration checks do not change the failed
full-resolution Pixel Motion CPU/GPU comparison.

## Reproduction identity

Use the [comparison script](../../scripts/compare-timewarp.ps1) with the frozen binaries
and keep compilation/GPU jobs idle. Its strict playback comparison reports the failure above.
The local captures, JSON records and timing-only wrappers remain available for investigation.

| Artifact | SHA-256 |
| --- | --- |
| Baseline executable | `fca9c7f463aebcec29061fc0a9625028cab2534ff71054431ecb464492a75191` |
| Candidate executable | `bf46e9e243799c759a445e9a1f32a0df2c93b67df240f265e5bfc4dac2eea463` |
| Identical harness source | `a33b7c6aa031b4a317f08acf705f3393804399c50fc848e8613662c3664dd766` |
| Validated combined patch | `9c66f4d80483f8f92b4ca2be3f5815ceda34efaa8ae9f6faa910fc6f448aed56` |
