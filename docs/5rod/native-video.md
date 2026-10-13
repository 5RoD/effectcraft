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
formats. Source, interpreted and composition rates must match, with normal time stretch and
no time-remap property. Those other cases retain the existing cached source path. Unsupported shader/device limits
also retain that path. Failed source decoding records a media diagnostic; it does not publish
a substitute as a successfully decoded frame. The optional shader can fail to compile without
disabling the ordinary GPU compositor.

Native planes are transient shared references; this change adds no retained frame cache or
read-ahead worker. The decoder still owns its normal source/session caching. Purge, Reload
Footage and cache budgets retain their existing meanings.

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
outside the observation window.

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
the explicit rate/stretch/remap fallback guard above. Native-frame caching and request
ordering need implementation and measurement before relaxing that guard.
