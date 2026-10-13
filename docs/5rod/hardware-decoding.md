# Windows hardware decoding

The Windows desktop and local CLI sessions register FilmCraft's native decoder adapter before
any footage source opens. Both native hosts compile the same guarded registration module.
CLI bridge sessions use the running desktop's existing decoder configuration. Supported
streams can use Windows Media Foundation/D3D11 hardware decoding;
unsupported streams and recoverable decoder failures retain the software path. The adapter
is a safe external Rust API; EffectCraft introduces no unsafe code and does not link or ship
FFmpeg. Registration is guarded against escaped Rust panics, with software decoding retained
when registration fails. This guard does not catch operating-system or driver process faults.

The shared FilmCraft crates are pinned together at
`cee1feb185e675a108f78067f71ad86a640ace3d`. Integration follows public API documentation and
manifests; decoder implementations were not inspected or copied. Platform registration is
Windows-only in the native executable hosts. Codec interfaces remain behind media, export and
interchange; the public media wrapper exposes only EffectCraft types.

Movie reads now request exact native frame timestamps. The updated container readers round
timestamp units to the nearest stored value, so the former half-frame offset selected the next
frame at a tie. GIF presentation-delay lookup retains its interval sample. Existing H.264
round-trip and animation tests check frame identity without weakening their tolerances.
The renderer disk-cache salt changes with this decoder integration so older decoded pixels
cannot be reused under the new media path.
Direct VP9 oracle tests explicitly select synchronous decoding for per-packet comparisons;
a separate two-worker test drains the queue and checks every returned frame's timestamp and
reconstructed planes. Runtime decoding retains the library's threaded mode.

For a repeatable comparison, set `EFFECTCRAFT_HARDWARE_DECODING=auto` or `off` before launching
the same executable. The default is `auto`. An invalid value logs a warning and selects software
decoding. The mode applies to newly opened sources; existing decoder instances retain their mode.

The read-only control query [`ui.decoder.stats`](../control-protocol.md) returns the enabled
mode, registered backend and process-wide monotonic hardware-frame, session, declined-stream
and fallback counters. Backend registration is not evidence that a particular stream used
hardware. Compare frame counters before and after the playback interval; perform queries outside
the viewer observation window. Counter deltas can include decoder read-ahead.

The CLI's `render --json` response retains `rendered` and adds a read-only `decoder`
object with the selected mode/backend, hardware-frame/session/decline/fallback counts
and source-decode/conversion work counters. These are process-wide totals, including
project loading and read-ahead. A positive hardware-frame count establishes decoder
use in that process; registration alone does not. Diagnostics and registration warnings
go to stderr so stdout remains one JSON document. CLI export speed, frame/audio quality,
backward seeks and software overrides require separate production comparisons.

This adapter currently reads decoded YUV pixels back to CPU memory. Colour conversion, frame
cache copies, composition and texture uploads still cost time. Its registration can also make
upstream export capabilities available, but EffectCraft's Render Queue encoder selection is
separate and is not established by decoder registration.

Hardware integration does not establish smooth full-resolution 120 fps playback. Acceptance
requires the same production executable, source, preview settings and disk-cache policy with
`auto` and `off`, positive hardware-frame evidence in the enabled run, and independently checked
pixels and seeks. The earlier streaming build measured about 22.3 distinct viewer frames per
second on a 30-second slice of the user's 1440p/120 OBS recording with disk caching disabled;
that software baseline does not attribute the entire cost to the decoder.

## Production comparison, 2026-10-13

The clean production build at `26e58633978e2651a71641a4402ad0daa3f770ff` passed the full
gate: 3,028 tests, zero failures, 27 ignored, strict Clippy, layers, assets and 32-crate WASM
checks. Rust 1.99.0, Thin LTO and one codegen unit produced executable SHA256
`3bc40bb69e7911d0315fe1a5ba2785b63c61efa7004243e68b4b72f773eaa3b2`.

One sequential 30-second capture per mode used the same 2560×1440/120 H.264 OBS slice,
one effectless video layer, Full resolution, Fit display, zero intentional skips, disk caching
off and audio off. No compiler or validation job ran during capture. OS/driver caching and
other desktop activity were uncontrolled. This measures viewer texture changes, including
waiting draws, rather than physical monitor presentation or multi-layer edit performance.

| Measurement | Software (`off`) | Hardware (`auto`) |
|---|---:|---:|
| Distinct viewer frames per second, including waits | 23.80 | 24.16 |
| Texture hold p50 / p95 / p99, ms | 44.05 / 51.07 / 62.39 | 44.27 / 50.12 / 60.90 |
| Longest completed texture hold, ms | 82.50 | 66.73 |
| Mean process CPU cores used | 7.29 | 5.84 |
| Peak process private bytes, GiB | 6.60 | 6.88 |
| Hardware-frame counter delta | 0 | 730 |
| Declined-stream / fallback counter deltas | 0 / 0 | 0 / 0 |

The enabled capture confirms Media Foundation hardware decoding. Its roughly 20% lower
observed CPU use is useful, but the 1.5% cadence difference is too small to establish a playback
speed gain from a single pair. Peak private bytes increased about 4.3%. Both captures had zero
minimum selected-frame skips, stale draws, reverse transitions and missing textures, and quit
normally. Full-resolution 120 fps playback still fails this acceptance case.

After the timing windows, full-resolution renders at frames 0, 61, 119, 120, 239 and 1200,
followed by a return to frame 0, were identical between modes, including alpha. Independent
FFmpeg PNG references differed by at most one 8-bit RGB value, at 73.55–74.10 dB PSNR.
Both backward-seek frame-zero results exactly matched their initial frame-zero result. These
seven samples do not establish conformance for every codec, frame or damaged stream.

The private runner SHA256 was
`f6c6adc67a9b2a1952c457cabf75b68698b0a444f7ee13012c5e2c97f8e87b0c`; source slice SHA256
`11aae53e5e59a93a61e77098fb3bf40a105c02720e81fd89ccbd69c54a352f40`.
External FFmpeg remains an oracle only. User footage and screenshots are not committed.

Subsequent read-only work counters separate source-frame calls, frame conversion and CPU
texture-upload enqueueing. They are diagnostic evidence for choosing the next optimization;
the counters themselves are not a performance improvement. Source timers include read-ahead,
decoder/cache work and allocation, and concurrent calls overlap. Upload timing excludes GPU
DMA completion and shader execution. Use before/after snapshots outside viewer observations.

The next transfer optimization is documented in [native-video.md](native-video.md): supported
opaque 8-bit video can transfer its compact native planes and convert them on the GPU.
