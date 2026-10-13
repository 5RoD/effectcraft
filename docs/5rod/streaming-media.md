# Native streaming media input

Native metadata probing and `MediaPool` container reads now keep a file handle and the
container's index instead of retaining every compressed byte. This removes whole-recording
loading from the MP4 playback path. It does not establish smooth 120 fps playback or add
hardware decoding; [production viewer measurements](viewer-playback-admission.md) remain
the separate playback acceptance evidence.

## Public interfaces and bounds

The integration uses the existing pinned FilmCraft revision
`2c5ba7a72619935f0481f99d59409e0d1844b234`: `ByteReader`, `SharedReader` and the
`filmcraft_codecs::reader_openers()` registry. No decoder implementation was inspected or
copied, and no dependency revision or runtime package was added. The two development
dependencies already used elsewhere in the workspace generate the small synthetic MP4.

The native reader checks regular files and captures their length. Reads validate checked
ranges and are limited to 64 MiB each. Seek/read errors retain their path and operating-system
error kind. A format declined by every streaming opener can use the existing in-memory path
only after its length passes a 512 MiB per-file budget and a fallible allocation. An accepted
container that fails retains its error, rather than retrying a whole-file read. One source is
opened per pool/path outside the global source-map lock; its failure remains available until
`forget`, allowing a repaired file to be reopened. Existing in-memory/browser inputs retain
their byte-based path.

The length is checked again after opening or buffering. Later truncated reads fail through
checked I/O; this does not detect same-size content changes or provide a coherent view of a
recording still being written. Index, decoder and decoded-frame allocations have their own
rules: neither input bound is a total process memory limit. Formats without a streaming opener,
including some still/audio inputs, can return an actionable unsupported-size error. Choosing
and mixing all three OBS audio tracks remains separate work.

## Checked evidence

Four new synthetic regressions passed:

- A logical one-terabyte MP4 uses two small populated regions and virtual zero padding.
  Opening reads less than 32 KiB; opening plus four requested frames reads less than 128 KiB.
  Sequential and backwards-seek pixels exactly match the existing byte-based decoder path.
  No physical terabyte file is created.
- Invalid/overflowing ranges, truncation and nonregular inputs return checked errors.
- An unsupported input declaring `u64::MAX` bytes fails before allocating or reading it;
  small inputs exercise both sides of the buffering limit.
- A malformed accepted container keeps its source error and caches no valid pixels. Replacing
  it with the valid fixture and forgetting the source permits a successful frame read.

The broader media run passed 48 tests with one optional test ignored. FFmpeg was explicitly
available as an external oracle, and all 17 media integration tests ran, including frame quality,
resampled/chunked audio, reduced-resolution reads, seeking, FLAC and concurrent requests.
Fixture creation is serialized within the integration-test process so parallel tests cannot
replace a generated fixture while another streaming source retains its Windows file handle.

A local read-only check used a finalized OBS 32.2.2 Hybrid MP4: 122,544,764,516 bytes
(114.128 GiB), 2560×1440/120, H.264 High/level 5.2, limited-range Rec.709 and three stereo
48 kHz AAC tracks. Dimensions, rate, codec, audio/video presence and duration match the
external metadata oracle. Metadata-only probing observed a 43,528,192-byte peak commitment;
requesting frames 0–3 observed 459,476,992 bytes (438.191 MiB). Each frame decoded at full
resolution and had a distinct pixel fingerprint. The file's size and modification time were
unchanged. These are observations sampled every 25 ms before the process exited, so its
last interval may be absent; file contents were not hashed in full.

This used the off-LTO `stream_probe` example with a 128 MiB decoded cache and a private
runner guarded at 3 GiB/120 seconds. Automatic read-ahead remained enabled. It is a small
correctness/resource check, not production timing, GPU utilization, physical presentation,
full-recording validation or a pixel-quality comparison of that recording with FFmpeg.
The footage, pixels, source path and private runner output remain outside the repository.

Diagnostic executable SHA256:
`1c67f4eadc62c4cddd8039f5126e7b5d7616dea0e171595851e3209519aec903`.
Focused media-test log SHA256:
`a46a66c4e9e54d7e69eb0eea854263be1b311ba9cfa83a8b8901ae82030f959e`.
Private capture identities: `a3d63b6ad0a14be1b1ded7f03561d5f6` (metadata) and
`144bf29b75bb46e0b75967d680550d37` (four frames).
