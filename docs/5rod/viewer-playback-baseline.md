# Production viewer playback baseline — 2026-10-12

**Uncached 1440p playback fails the target.** Cached playback approaches the requested rate
over a short retained range. These are baseline measurements, not an optimization comparison
or installation acceptance.

## Build and procedure

The clean Windows x64 production build is commit
`f47276c40cb623b0cf741ec2a5db4838f7e846f6`, Rust 1.99.0, normal release profile
(Thin LTO, one codegen unit). Desktop executable SHA256:
`bbab08227699fc18dca721c64a7ab6b6fbd8015ff91fcc2e9b90c817725d1771`.
The host is an i9-10900F, 32 GB RAM, RTX 3050 8 GB and a 165 Hz desktop. The application
used its Vulkan NVIDIA device. No compiler or competing validation job ran during capture.

Each isolated run used a new 2560×1440 composition and one ordinary MP4 layer without
effects. Three-second procedural fixtures used NVENC H.264 High, CQP 20, P5, High Quality
tuning, quarter-resolution multipass, adaptive quantization, two B-frames and a two-second
GOP. Assumed color settings were 8-bit 4:2:0 Rec.709 limited range. This is not gameplay or
an OBS-authored Hybrid MP4. Input SHA256 hashes:

- 120 fps: `6e45db6e07283ad264e5f9a4d765037d9eb561f31f5ba40a63b087ec8cf74cef`
- 60 fps: `77cea6671be5a79265406056003e467ab1900d174d3dc9ab6278d9c3c484a56b`

FFmpeg was used externally to generate fixtures; it is not linked or shipped.
The real 1680×1020 desktop viewer requested Full resolution, Fit display, the composition
frame rate and zero intentional frame skipping. Video was enabled; audio, idle caching,
overlays and layer controls were disabled. Renderer selection remained Auto. Default preview
budgets were 3 GiB RAM and 1 GiB GPU memory. Twelve seconds of observation followed Purge;
thirty-five seconds then used Cache Before Playback in the same process. Purge allows
paused prefetch; the second phase retains earlier work. Neither empties the OS file cache.

The bounded viewer observer records actual texture changes at application drawing,
including playing draws that wait for pixels. It was read only after each observation
window. Own-process memory/CPU counters were sampled at 100 ms intervals. Screenshots
were taken outside the windows; both applications quit cleanly.

## Results

Hold percentiles cover completed intervals between texture changes during non-caching
playback, including waits. These are not render-job timings or physical display presentation.
Private memory is process commitment; working set is resident process memory. Neither is VRAM.

| Requested rate / phase | Texture changes/s | Hold p50 / p95 / p99 / max (ms) | Playback range | Peak private / working set (GiB) |
| --- | ---: | --- | --- | --- |
| 120 fps, after Purge | 11.09 | 18.47 / 516.83 / 870.40 / 890.05 | 0–359 | 8.54 / 4.20 |
| 120 fps, Cache Before Playback | 119.03 | 8.05 / 11.55 / 12.18 / 13.95 | 0–48 | 9.12 / 4.14 |
| 60 fps, after Purge | 11.08 | 20.46 / 331.37 / 913.22 / 3629.24 | 0–179 | 9.29 / 4.45 |
| 60 fps, Cache Before Playback | 59.98 | 18.09 / 18.59 / 19.07 / 21.10 | 0–48 | 8.38 / 3.76 |

Cache Before Playback shortened both plans to **49 frames**: about 0.408 seconds at 120 fps
and 0.817 seconds at 60 fps, repeatedly looped. Playing windows were 33.86 and 34.25 seconds,
with no waiting draws. The complete observer includes startup/caching: at 60 fps it records
28 stale draws and one reverse transition before settled playback. The 120 fps observer
reports at least 32 skipped selected frames despite near-target cadence. Neither cached rate
proves complete, uninterrupted full-clip playback. Auto selection also used CPU-backed
textures for much of the cached range; this is not all-native-texture throughput.

After Purge, 294 of 547 playing draws waited at 120 fps and 199 of 650 waited at 60 fps.
Mean process CPU consumption was about 8.78 and 10.64 logical cores respectively. The 120 fps
deadline is about 8.33 ms; observed stalls exceed it substantially even for one layer.
Cold decoding, render scheduling and memory retention require investigation.

## Evidence and remaining acceptance

Local raw records remain under `.local-output/`:

- `production-build-1647fa4d33fb47248e8a4147c13090b7/metadata.json`
- `viewer-playback-5ee6a1cf5a434a5f96cb908237933899/` (120 fps)
- `viewer-playback-54842b2d3fae43c99fd24c738ef78c88/` (60 fps)
- `run-viewer-playback.py` (runner; capture metadata records its hash)

Captures retain commands/replies, preferences, project, observer/resource samples, screenshot,
logs and binary/fixture hashes. Screenshots and binary fixtures are not committed. Remaining
acceptance includes real 120 fps gameplay, OBS Hybrid MP4 boundaries/finalization, all three
audio tracks, longer projects, effect/camera stacks, scrubbing, cache growth and physical
display/audio timing. Compare changes with this frozen build under equivalent conditions;
passing unit tests alone does not establish speed.
