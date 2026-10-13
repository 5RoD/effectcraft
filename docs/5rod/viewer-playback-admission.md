# Production viewer comparison — bounded frame admission

**Uncached playback improved but still fails both requested rates.** One-frame admission
and bounded read-ahead increased actual viewer texture cadence and reduced peak process
memory in these captures. Cached playback remains limited to a 49-frame loop. This is
not complete timeline, gameplay or installation acceptance.

## Builds and matching scope

The [baseline](viewer-playback-baseline.md) is clean commit
`f47276c40cb623b0cf741ec2a5db4838f7e846f6`, measured 2026-10-12 UTC. The revised build
is clean commit `df58c830298493140c7230b117fa21ca2112272f`, measured 2026-10-13 UTC.
Both use Rust 1.99.0, native Windows x64, Thin LTO and one codegen unit. Revised desktop
SHA256: `bee324b4d2eb2a6acdf1b52b6f5ef7248d7c371dc8d04733b99d37882b9dda71`.
The full revised repository gate passed with 3,022 tests passed, none failed and 27 optional
tests ignored before the production build.

The same runner, fixture hashes, preview settings, host and RTX 3050 Vulkan backend were
used. Runner SHA256: `1c2c6a21826dc6fbb415e3fe182b4940737e185ba9da0cc6ff387221a78d4ca8`.
Each rate has one capture per build on different days. Operating-system and driver caches
were not controlled; these differences are observations rather than replicated estimates.
No compiler, competing application benchmark or GPU validation ran during the captures.
Both revised applications quit cleanly, and their screenshots were visually inspected.

The fixture is a three-second ordinary MP4, generated externally with settings matching
the supplied OBS encoder controls. The real viewer requested 2560×1440 Full resolution,
Fit display and zero intentional frame skipping, with one video layer and no effects.
Audio and idle caching were disabled. Disk caching retained its enabled default, with a
100 GiB limit and the shared platform cache folder; an isolated configuration folder does
not isolate that disk cache. Twelve seconds followed Purge All, which clears RAM and disk;
the same process then used Cache Before Playback for 35 seconds and retained earlier work.
Purge permits paused prefetch. Disk hit/write counters were not captured. Refer to the
baseline for fixture and procedure details.

## After Purge

Cadence includes playing draws waiting for pixels. Holds are completed intervals between
texture changes, not render-job timings or physical display presentation. Memory is peak
process private commitment / working set, not VRAM; CPU is mean logical cores used.

| Rate / build | Texture changes/s | Hold p50 / p95 / p99 / max (ms) | Private / working set (GiB) | Mean CPU cores |
| --- | ---: | --- | --- | ---: |
| 120 fps, baseline | 11.09 | 18.47 / 516.83 / 870.40 / 890.05 | 8.54 / 4.20 | 8.78 |
| 120 fps, revised | 21.41 | 42.46 / 67.40 / 85.18 / 91.94 | 6.75 / 2.90 | 6.65 |
| 60 fps, baseline | 11.08 | 20.46 / 331.37 / 913.22 / 3629.24 | 9.28 / 4.45 | 10.64 |
| 60 fps, revised | 22.36 | 42.40 / 77.45 / 100.22 / 134.75 | 6.42 / 2.60 | 6.32 |

Observed cadence rose 93.1% at 120 fps and 101.9% at 60 fps. Peak private commitment
fell 21.0% and 30.8%. Long holds decreased substantially, but the median hold increased:
the revised queue has fewer overlapping frame jobs and a more uniform decode cadence.
It still misses the 8.33 ms / 16.67 ms deadlines. The revised observers report no stale
textures or reverse transitions; minimum skipped selected frames are zero and two.
Waiting draws were 737/1,962 and 348/1,898. Draw counts differ with repaint cadence and
are not directly comparable frame counts.

## Cache Before Playback

| Rate / build | Texture changes/s | Hold p50 / p95 / p99 / max (ms) | Private / working set (GiB) | Mean CPU cores |
| --- | ---: | --- | --- | ---: |
| 120 fps, baseline | 119.03 | 8.05 / 11.55 / 12.18 / 13.95 | 9.12 / 4.14 | 3.78 |
| 120 fps, revised | 119.94 | 7.56 / 11.48 / 12.36 / 20.15 | 6.83 / 3.21 | 4.08 |
| 60 fps, baseline | 59.98 | 18.09 / 18.59 / 19.07 / 21.10 | 8.38 / 3.76 | 1.56 |
| 60 fps, revised | 59.96 | 18.11 / 18.71 / 24.06 / 36.84 | 6.36 / 2.88 | 0.91 |

Both revised plans shortened to frames 0–48: about 0.408 seconds at 120 fps or 0.817
seconds at 60 fps, repeatedly looped. Settled playback lasted 33.19 / 33.36 seconds, with
zero waiting draws. Its first requested playing draw followed observer start by 1.880 /
1.676 seconds; pre-play caching remains perceptible. Peak private commitment fell 25.1%
and 24.1%, but cached hold tails increased. These single captures do not establish a
cached cadence improvement. Each revised observer reports at least one skipped selected
frame, with no stale textures or reverse transitions.

Only 335/5,448 settled draws used a native GPU texture at 120 fps, versus 4,042/5,343 at
60 fps. Cache residency and CPU texture fallback remain relevant; this is not uniformly
native GPU throughput or proof that an entire clip plays uninterrupted.

## Evidence and next acceptance

Local immutable capture records remain under `.local-output/`:

- `production-build-acf078e7ebad4a2fbe749ff419997a9f/metadata.json`
- `viewer-playback-71d9f5f25c2d4c17b69d7f2fe092a2de/` — revised 120 fps
- `viewer-playback-379e89dd869249f592b15c4ea8632169/` — revised 60 fps
- `viewer-playback-admission-comparison-v1/comparison.json` — computed comparisons
- `viewer-playback-admission-comparison-v1/artifacts.json` — hashes of all four captures

The source changes retain parallel work within a frame while admitting one complete
local frame at a time. Optional read-ahead has one worker per media pool. Checked cache
readers reject malformed geometry and decompression prefixes; failed preparation releases
the queue slot. Optional disk persistence retains the admission slot until it finishes.
Separate enabled/disabled disk-cache comparisons, isolated cache directories and disk
hit/write counters remain necessary to distinguish decoding from persistence costs.

Next work must improve uncached decoding and bounded large-file streaming, then measure
real OBS Hybrid MP4 gameplay, three audio tracks, scrubbing, long clips, camera/effect
stacks and cache growth. A metadata-only external oracle confirmed a representative local
OBS recording is 2560×1440/120, H.264 High Level 5.2, 8-bit Rec.709 limited range, with
three stereo 48 kHz AAC tracks. At roughly 114 GiB it exceeds this host's 32 GB RAM;
the current whole-file media reads must not be used to benchmark or accept that import.
No recording content, screenshots or fixtures are committed.
