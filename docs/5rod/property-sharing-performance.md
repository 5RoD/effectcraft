# Property storage and retained edits

The G59 change (`4e96d390`) shares immutable property data between project revisions.
Editing an opacity value detaches that property while retaining untouched animation,
text, paths and expressions. Every edited field remains isolated from older revisions.
Project JSON retains its previous representation. The property handle is eight bytes;
its data remains separately allocated, rather than disappearing.

## Measured scope

On the i9-10900F / 32 GB / RTX 3050 Windows host, five alternating paired runs of
the same generated model workload retained 64 revisions, each with one untouched
4,096-key property and one edited scalar:

| Metric, median | Before | G59 |
|---|---:|---:|
| Clone and edit 64 revisions | 21.056 ms | 0.096 ms |
| Observed process private memory | 55.8 MiB | 1.5 MiB |
| Distinct untouched key buffers | 64 | 1 |

This standalone probe uses cached full-workspace libraries with release optimization
and LTO disabled. It keeps the revisions alive for 500 ms outside the edit timer so
10 ms process-memory sampling can observe them. These are specific copying and
whole-process memory measurements, not overall application speed or timeline FPS.

Normal Thin-LTO production CLI builds were separately compared in three alternating
pairs per export case. Each rendered 120 frames of the same private OBS fixture at
2560×1440/120 fps, 300 Mbps H.264, with stereo 48 kHz AAC. Both used two Rayon workers
and BelowNormal process priority; these are regression checks, not unrestricted
maximum-throughput measurements.

| Export queue duration, median | G58 | G59 |
|---|---:|---:|
| Bare footage | 5.906 s | 5.933 s |
| Bloom | 7.347 s | 7.332 s |

Maximum observed private memory changed by −1.9% for bare footage and +0.3% for Bloom.
Both cases met the predeclared 5% queue-time / 20% private-memory regression budgets.
Every output had all 120 frames, exact frame timestamps, expected BT.709 metadata,
48 kHz stereo AAC, hardware decoding and no decoder fallback. Decoded frame and
audio fingerprints were identical across all twelve exports. No aesthetic quality
score was used.

The same production builds also ran three paired `bench --ops --small` trials
(12 compositions, 300 main-composition layers and 30 nonexistent footage paths).
Every operation was retained. Median small edit time changed from 0.083 to 0.072 ms,
and large edit time from 0.120 to 0.111 ms. Allocating separately owned properties
has a loading cost: parse time rose from 22.447 to 24.669 ms; compact parse rose
from 20.081 to 21.417 ms. Total open changed from 36.729 to 37.303 ms and startup
to first frame from 104.267 to 106.465 ms. Project generation rose from 4.893 to
5.261 ms. These small absolute changes are retained as a tradeoff, not hidden as
universal speedups. The headless CPU UI driver and fake footage do not establish
native presentation or real-media rendering performance.

The G59 full repository gate passed 4,038 tests with no failures (34 existing
ignored tests), strict native Clippy, required Vulkan GPU checks, layers, assets
and WebAssembly checks. Its normal production binaries passed fourteen additional
silent functional checks, including exact saved branch content, forced-termination
recovery, Unicode paths, GPU rendering and export timing.

## Reopening history

G59's edit-time sharing alone does not pool independently parsed archived properties.
The subsequent history reconstruction change compares each decoded property's complete
content with a previous revision before reusing its immutable data. Full layer records
are compared with the already loaded current layer. For patches limited to property
content, the previous tree is retained and only explicitly changed properties are replaced
by child position. This avoids comparing every untouched animation again; broader or
unrecognized paths use exact comparisons. Recovery uses the same paths. Reordering or
duplicate IDs cannot substitute unequal property data. A bounded serialized comparison
for full records and broader patches also preserves signed zero, which ordinary
numeric equality treats as equal to positive zero. Iterative traversal limits this optional
optimization to 65,536 nodes and 128 group levels, with at most 8 MiB of serialized scratch
per compared property; larger inputs retain their decoded content.

This does not make history disk-backed or lazy. Unlimited history still consumes
memory, changed animation still needs its own storage, and reopening must decode the
archive. The G59 measurements above do not measure this subsequent restoration change.
Native viewer/audio-device acceptance and sustained 120 fps playback remain separate.
