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
formats. Those retain the existing checked source path. Unsupported shader/device limits
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
