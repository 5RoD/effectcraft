# Windows hardware decoding

The Windows desktop registers FilmCraft's native decoder adapter before any footage source
opens. Supported streams can use Windows Media Foundation/D3D11 hardware decoding;
unsupported streams and recoverable decoder failures retain the software path. The adapter
is a safe external Rust API; EffectCraft introduces no unsafe code and does not link or ship
FFmpeg. Registration is guarded against escaped Rust panics, with software decoding retained
when registration fails. This guard does not catch operating-system or driver process faults.

The shared FilmCraft crates are pinned together at
`cee1feb185e675a108f78067f71ad86a640ace3d`. Integration follows public API documentation and
manifests; decoder implementations were not inspected or copied. Platform registration is
Windows-only in the native executable. Codec interfaces remain behind media, export and
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
