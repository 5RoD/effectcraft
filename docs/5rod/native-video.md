# Compact native video transfer

The measured 1440p/120 OBS preview bottleneck includes uploading a full RGBA float image:
58,982,400 bytes per frame. A subsequent production profile measured about 11.26 ms of CPU
allocation/upload enqueueing per image, source-frame calls averaging 4.16 ms and CPU conversion
averaging 5.29 ms. Those wall timers overlap; they are not an additive latency breakdown and
do not time GPU execution. Distinct viewer frames averaged 24.08/s with hardware decoding,
disk caching and audio off, one effectless Full-resolution video layer. A 120 fps deadline is
8.33 ms. This evidence motivated changing the transfer representation.

## Implementation

For supported opaque 8-bit 4:2:0 movies, MediaPool shares the decoder's original plane
allocations through a codec-independent, checked `Yuv420Frame`. It uses the same exact native
Tick selection, including interpreted rate, looping and endpoint selection, as the reference
image path. No decoded float-image or layer-buffer copy is required for this source.

The GPU uploads only used plane bytes, with four-byte alignment padding. An original integer
storage-buffer kernel performs centred chroma interpolation, range normalization, matrix
conversion, clipping and source-depth quantization. Vertical-before-horizontal interpolation
matches the existing CPU conversion. Working images remain RGBA32Float. Layer placement,
opacity, linear blending, track mattes and Preserve Transparency use the existing compositor
operations. For 2560×1440, the payload is 5,529,600 bytes: 90.625% fewer bytes than RGBA32Float.
This is a payload calculation, not a measured playback speed gain.

The initial path deliberately declines masks, enabled effects, active layer styles, source
motion-blur sampling, frame blending, fields, proxies, reduced-source resampling, inverted
alpha, linear-light interpretation, non-identity source color management and other decoded
formats. Normal time stretch and no time-remap property are required. Those other cases retain
the existing cached source path. Unsupported shader/device limits
also retain that path. Failed source decoding records a media diagnostic; it does not publish
a substitute as a successfully decoded frame. The optional shader can fail to compile without
disabling the ordinary GPU compositor.

Decoded raw frames and interpreted float images share MediaPool's existing byte-budgeted LRU
and inflight-request policy. Raw identity uses the file path and exact native request Tick,
independently of alpha interpretation or reduced output size. CPU conversion and compact GPU
reads use the same raw-frame helper; unsupported native formats can fall back without decoding
the frame twice. Raw geometry, planes and float values are validated before caching, and raw
buffer sizes use checked arithmetic and the per-image byte limit. Errors do not enter the cache.

This adds no cache budget or read-ahead worker. As before, the newest cached entry can remain
when it exceeds the selected budget; other entries are evicted in access order. Purge clears
both representations, and Reload Footage removes both for the changed path. Decoder-owned
buffers and outstanding shared references remain outside the LRU's retention control.
Reads already in flight during Purge or Reload cannot repopulate the invalidated cache; a
retained identity token also protects converted image entries against that race.

## Validation and diagnostics

Synthetic GPU comparisons exercise small and odd dimensions, chroma boundaries, full/limited
range, BT.601/709/2020 coefficients and all three project depths. Complete composition tests
exercise forward/backward source times, transforms, opacity, linear blending, interpretation
fallbacks and a failed source followed by recovery. Media tests check shared plane ownership,
other-format fallback and truncated buffers; hostile dimensions and color coefficients return
errors before upload.

`ui.transfer.stats` now counts native plane-buffer uploads as well as float-image uploads.
Its CPU upload timer includes packing, resource allocation and enqueueing, but excludes GPU
DMA completion and conversion/compositing execution. Parameter/uniform uploads remain outside
these frame-payload counters. `ui.decoder.stats` source-call timers still include decoder
work; native GPU conversion does not increment the CPU conversion timer. Take snapshots
outside the observation window. Source-call timers count actual decoder API calls, excluding
EffectCraft's raw-cache hits. The cache also resets its ephemeral contents safely if its
eviction stamp counter reaches its limit; a synthetic rollover regression verifies this.

Production playback and real-source pixel acceptance must be measured separately. Synthetic
comparisons alone do not establish sustained 120 fps, monitor presentation, heavy edit
performance, every codec or every damaged stream. Hardware decoding evidence is recorded in
[hardware-decoding.md](hardware-decoding.md).

## First production capture and mixed-rate guard

A matched 30-second pair on the RTX 3050, i9-10900F and 32 GB PC used the same OBS
2560×1440/120 slice, one effectless Full-resolution layer, audio and disk caching off.
Normal Thin LTO builds of `63bba701` and `f62c645c` observed 24.19 versus 62.38 distinct
viewer frames/s including waits. Native upload CPU work averaged 2.08 ms per 5,529,600-byte
payload, versus 11.23 ms per 58,982,400-byte float payload. Process CPU use averaged
5.33 versus 1.79 cores, peak private bytes 6.55 versus 4.85 GiB; peak working set increased
2.70 to 2.93 GiB. These are single captures with uncontrolled OS/driver caches and desktop
activity, and measure app draws rather than monitor scanout.

The native capture skipped at least 1,596 selected frames over 30 seconds: **this does not
pass full 120 fps playback acceptance**. It decoded nearly 3,480 hardware frames but displayed
1,878 distinct frames. Do not assume the excess is duplicate decoding: prefetch also produces
frames which a later wall-clock draw can skip. Presentation/UI timing still needs profiling.
The adapter reports a 165 Hz desktop mode; that read alone does not establish presentation timing.

Seven full-resolution renders, including a backward return to frame zero, matched the external
FFmpeg reference to at most one RGB code value, with exact alpha and 73.55–74.10 dB PSNR.
GPU-versus-CPU rounding changed at most six of 11,059,200 RGB channels in a sample by one value.
`render.frame` uses automatic backend selection, so these samples include both GPU and CPU
renders; GPU readback counters identify the GPU samples. They are not a claim of bit-exact
CPU/GPU agreement or complete codec conformance.

A separate 120 fps source in a 60 fps composition exposed a regression before installation:
the initial native path observed 6.11 fps versus 20.97 fps on the previous build. Native
source calls averaged 85.96 ms, with 6,693 hardware-frame decodes for 323 calls, versus
7.39 ms and 1,266 hardware frames for 634 calls on the cached reference path. This motivated
an explicit rate/stretch/remap fallback guard in `23317297`. The shared raw-frame cache now
addresses repeated native reads within the existing budget, and the rate restrictions have
been removed for validation. Stretch/remap still fall back. The new cached path requires
separate production measurements before its mixed-rate performance can be accepted.

## Shared-cache production results and presentation diagnostic

The normal Thin LTO build of `2bc11354` repaired the mixed-rate regression: the same
120 fps source in a 60 fps composition observed 55.74 viewer frames/s, compared with
6.11 on the initial native path and 20.97 on the older cached float path. Source calls
averaged 6.67 ms, with 3,442 hardware frames for 1,723 calls. The capture still included
stalls up to 206 ms and does not pass perfectly smooth 60 fps playback.

A fresh matched 120 fps pair observed 24.24 versus 59.48 viewer frames/s. This establishes
about 2.45 times the older float-path cadence, but no further 120 fps gain over the earlier
62.38 fps native capture. The shared-cache capture skipped at least 1,362 selected frames;
it still fails full 120 fps. Seven checked renders retained the one-code RGB bound,
exact alpha and 73.55–74.10 dB comparison with the external reference.

Cached playback isolated an additional presentation limit: a 49-frame full-resolution loop
observed 63.58 fps after nearly all decode/render work was finished. A separate visible-window
check on the primary display observed 63.53 fps; increasing the presentation queue to two
frames observed 63.57 fps and did not help. These results do not establish a hardware ceiling.

Desktop launches accept `EFFECTCRAFT_VSYNC=auto|off`. Windows launches request
`AutoNoVsync` by default after the production checks below; other platforms retain
their existing synchronized default. `auto` explicitly retains synchronization.
`off` requests wgpu's `AutoNoVsync` mode, with its device-supported fallback;
invalid or non-Unicode values warn and retain automatic synchronization. This optional
setting can increase presentation work and permit tearing; it does not change rendered pixels
or guarantee that the physical display presents every frame. There is no global driver,
power-plan or monitor-setting change.

In the initial production-flags experiment, `off` observed 120.00 fps without selected-frame
skips on the cached 49-frame loop. Completed holds were 8.51 ms median, 8.87 ms at p95 and
9.04 ms at p99. A separate 30-second uncached run observed 103.13 fps, no selected-frame
skips, and holds of 8.45/17.13/44.84 ms at p50/p95/p99. It still slowed the playback clock
and does not pass uncached 120 fps. This experiment used an explicitly recorded uncommitted
presentation change; it is not an installed or release-accepted artifact. Audio, heavy effects,
long cached ranges and physical monitor scanout remain separate acceptance work.

The visible primary-window follow-up retained this result: 102.73 fps over 30 uncached
seconds and 119.94 fps on the cached loop, with no observed selected-frame skips. Cached
holds were 8.61/8.93/9.16 ms at p50/p95/p99. Uncached p99 remained 51.04 ms, so this
does not remove every stall. These captures launched a visible window without keyboard
activation and recorded its native visibility, minimized state and desktop rectangle before
observation; they do not infer physical scanout from the app's focus flag.

## Normal Windows default acceptance

A clean Thin LTO/codegen-one production build of `a3a0c6ab` repeated the visible
primary-window check with hardware decoding, presentation and graphics-backend
overrides all unset. Automatic selection used the RTX 3050 Vulkan compositor and
Windows hardware decoder. The same ordinary OBS MP4 excerpt retained Full
2560×1440 resolution, 120 fps, no frame skipping and audio off.

The uncached ten-second run observed 99.91 distinct texture changes/s, with
8.50/17.53/51.07 ms holds at p50/p95/p99. It still fails uncached 120 fps. The
cached 49-frame loop observed 119.94 changes/s over 14.39 steady seconds, with
8.33/9.06/9.20 ms holds and no selected-frame skips. All 6,570 uncached and
12,372 cached steady draw samples matched their requested frame keys. Cached
preparation retained the preceding texture; its 100 mismatches occurred before
steady playback, plus one paused transition.

Seven full-resolution frame samples, including a backward seek, stayed within
one RGB code value of the external reference with exact alpha. No compositor
readbacks occurred during either measured playback phase. The real window and
5rod theme were inspected. This accepts the defaults on this host; it does not
extend the cached range, prove physical scanout or establish moving-gameplay,
audio-preview or full Hybrid MP4 acceptance.
