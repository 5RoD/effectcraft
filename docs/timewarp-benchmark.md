# Reproducing the retiming comparison

This harness uses original code-generated premultiplied HDR textured shapes, an occluder,
and an optional animated matte. It measures the Timewarp effect and its generated host reads,
not a codec, complete preview, disk cache, or encoder. Keep the source identical between builds.
The existing microsecond source-fetch quantization is intentional here: a separate exact-frame
identity regression covers that fidelity defect without contaminating the memory comparison.

## Build after the compiler hold is released

Use an isolated checkout of green `bc553f203802b04ae89a22985cd56acb9c66c171` as the baseline.
Apply only this harness patch there. The candidate is that same base plus the frozen retiming
patch and this harness patch. The example needs only existing workspace `rayon` and `serde_json`
as GPU dev-dependencies; it adds no third-party package. Do not test hostile plans in the baseline.

In each checkout, with the same Rust/MSVC environment and four build jobs:

```powershell
$env:CARGO_BUILD_JOBS = '4'
$env:CARGO_TARGET_DIR = 'C:/Users/j/Documents/ChatGPT/ArtCraft/target/retiming-baseline'
C:/Users/j/.cargo/bin/cargo.exe build --release -p effectcraft-gpu --example timewarp_bench
```

Use `target/retiming-candidate` for the other checkout. Preserve normal release Thin LTO and
codegen settings for published performance measurements. Copy the two resulting binaries to
distinct names if either target directory will subsequently be reused. Record checkout IDs,
patch hashes, Cargo/rustc versions, build environment overrides and binary hashes. The runner
records binary hashes automatically. Correctness runs may use a faster linking profile, but
their timing results must not be represented as normal production release measurements.

First run focused CPU/export/GPU regressions and strict Clippy. Require the actual NVIDIA Vulkan
adapter (`WGPU_BACKEND=vulkan`, `EFFECTCRAFT_REQUIRE_GPU=1`); the harness rejects another adapter
and rejects GPU fallback. Then run with no other compiler, GPU workload, or benchmark active:

```powershell
pwsh -File scripts/compare-timewarp.ps1 `
  -BaselineBinary C:/Users/j/Documents/ChatGPT/ArtCraft/target/retiming-baseline/release/examples/timewarp_bench.exe `
  -CandidateBinary C:/Users/j/Documents/ChatGPT/ArtCraft/target/retiming-candidate/release/examples/timewarp_bench.exe `
  -OutputDirectory C:/Users/j/Documents/ChatGPT/ArtCraft/.local-output/retiming-ab `
  -Matrix Playback -Fps 120 -Frames 3 -Rounds 3
```

The default `Tiny` matrix runs only 31x19 odd-sized cases, all three methods and 1/8/64 samples,
plus Pixel Motion matte and Extreme filtering cases. `Playback` always runs that tiny matrix
first, then 2560x1440 Whole/Frame Mix/Pixel Motion with 1/4 samples. Historical 1440p 64-sample
plans are deliberately forbidden; these are not safe baseline/OOM reproductions. `Full` adds
candidate-only 1440p 64-sample cases, including Extreme Pixel Motion. That suite requires a
checked Timewarp registration and must pass the candidate's aggregate budget.

## Measurements and acceptance

Every timed instant differs; output caches are absent. CPU timings consume an already initialized
incoming buffer; GPU timings include upload, matching preparation, dispatch, completion and final
readback. Device initialization, input-buffer creation, checksums and capture writes are excluded
from effect timing but included in process wall time and process memory. Generated host-image
times are reported separately, without subtracting them from the headline timing. Transfer/read
and pool counters include any requested warmup; per-case `frame_ms` contains measured frames only.

Round 1 captures every measured float output, in exact case/frame order, with output geometry.
The runner compares baseline/candidate CPU pixels exactly, each CPU/GPU pair at 1e-3, and both
GPU variants at 1e-3. Any unavailable effect, nonfinite pixels, wrong geometry, checked error or
out-of-tolerance result stops the run. Capture comparison uses fixed-size chunks. Capture files
are bounded to 2 GiB each, disk space is checked, and successful captures are removed unless
`-KeepPixels` is supplied. Subsequent rounds keep per-frame checksums and alternate variant order.

Process peak working set comes from Windows' native peak counter, observed every 20 ms. Private
bytes are a sampled peak. These totals include shader initialization and pooled textures across
the suite; they are not per-case allocation attribution. `-SampleDeviceVram` optionally records
NVIDIA's device-wide memory-used observations at no more than four queries per second, including
other desktop usage. That number is a sampled device-wide peak, not a precise process VRAM peak;
sensor overhead is explicit and runs with/without it must be distinguished. WDDM process VRAM is
not reliably exposed by this harness. No RAM/VRAM total is promised to exclude driver allocations.

120 fps is primary (8.33 ms/frame), 60 fps secondary (16.67 ms), 144 fps optional stress (6.94 ms).
Report first-frame time, distinct-frame FPS, p50/p95/p99 and individual times with sample count;
three-frame runs alone cannot establish tail latency. Repeat warmup 0 and 1 separately. A bounded
long run can invoke the binary without captures, e.g. `run --suite playback --backend gpu --fps
120 --frames 1200 --warmup 1`. It renders ten seconds of distinct instants per case without changing
quality, samples, filtering, resolution, color or frame rate, and remains subject to checked
budgets. Use the analogous CPU run and compare render-stage throughput. Complete maximum-quality
exports still require separate decode/readback/encoding/storage measurements and 1/2/4/default
bounded concurrency; this isolated harness cannot claim full export throughput or 120 fps UI playback.
