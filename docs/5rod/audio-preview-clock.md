# Native audio preview clock

The 18 October Windows production comparison found two independent preview costs.
Auto resolution still converted video on the CPU; retaining the compact YUV source on the
GPU improved three-pair median cadence from 78.27 to 99.50 app texture changes/s. Median
CPU use fell from 6.63 to 1.76 cores, peak process commitment from 4.86 to 4.12 GB, and p99
texture hold from 21.96 to 11.60 ms. These are application observations, not monitor scanout.

The same 35-second Auto preview without audio reached 119.01 changes/s. A one-second fully
cached loop with continuous audio still reached only 99.68 changes/s: its clock advanced in
device callback increments. The three longer Auto audio runs also retained a short pause,
failing the predeclared 99% audio-active and zero-transition limits. These failures remain
open; the source optimization does not establish continuous 120 fps audiovisual playback.

## Timestamp interpolation

Native outputs publish the cumulative number of real PCM frames audible now through the
optional `AudioDevice::played_frames` method. Devices without timestamps retain their
consumed-minus-latency clock. Work-area looping uses the same exact sample-to-Tick mapping.

The native callback records its monotonic wall time, the predicted device playback delay,
the first real sample index and the real frames pulled. CPAL defines the
[callback and playback timestamps](https://docs.rs/cpal/0.16.0/cpal/struct.OutputStreamTimestamp.html).
The clock interpolates within these submitted real samples and holds throughout partial
or whole-buffer underruns, including when a callback stops arriving. It never extrapolates
beyond the known buffer. Nonlooping previews wait for the last submitted samples to drain.

The history has 64 preallocated metadata slots. Callback and UI access use `try_lock`; neither
waits for the other. Contention holds the previously published position, and callback
publication retries on the next buffer. Full storage preserves currently audible and future
records. Invalid, reversed or excessively delayed timestamps are rejected. Stop/restart
clears both the history and published position. No additional audio thread is created.

Synthetic tests cover intermediate callback times, output latency, partial/whole underruns,
bounded storage, invalid counts/timestamps, contention, lock poisoning, restart, fallback,
work-area looping and draining the final buffer. Full repository and production performance
validation are required before this clock change is accepted. Physical audiovisual output
synchronization and subjective sound/image quality are not established by these tests.

## Startup failure cleanup

The native preview now opens its output before creating the audio feeder. Previously, an output
error could return after the feeder started, leaving that worker running with its immutable
project snapshot. A rejected output is explicitly stopped; failure to spawn the feeder also
stops the already opened output. Mixing stays off the UI thread. Outputs must return without
waiting for queued samples, as the existing audio-scrub and browser paths already require.

A synthetic regression covers repeated output rejection, release of the retained project and
cleanup of a partially opened device. It uses no real audio device or native window. Execution
of the full repository gate is required before this change is committed. No throughput,
successful native launch or continuous audiovisual playback claim follows from this cleanup.

## Nonblocking stop

Native preview stop closes its sample queue, signals the feeder and signals the output worker.
It does not join a decoder read or a driver's stream destructor on the UI thread. Late mixed
blocks are discarded. A restarted output uses separate clock and latency storage, so callbacks
from the previous stream cannot advance it. Already submitted device samples retain the
device's buffering latency; this does not promise instant physical silence.

Each native audio worker registry holds at most eight active or closing workers in fixed storage.
Only completed threads are joined, before admitting another worker. Contention or eight stalled
workers returns a recoverable busy error. No additional cleanup thread or render-pool job is
created. Browser audio retains its UI-thread device lifecycle. Synthetic tests use gated reads,
late callbacks, output rejection, full and contended registries, poisoning and worker panics.
Repository validation and native playback acceptance remain required; this change does not
remove the existing policy that holds audio when the every-frame preview encounters a cache miss.

## Native audio scrubbing

Scrubbing uses one worker from the same bounded feeder registry for the lifetime of an open
native scrub output. The UI posts immutable source snapshots and immediately cancels the
previous request. A replaceable mailbox keeps the latest request while a read is in flight;
the worker does not hold the mailbox while decoding or mixing. Its wake channel has one slot.
Only mailbox contention uses a single UI retry slot, retried by the normal audio pump.

Cancellation is checked under the output queue lock before replacing samples, so a late result
cannot play after a newer gesture clears the queue. Changing the project snapshot or active
composition also retires the output, even when a new project reuses the same numeric IDs.
Stop disconnects the idle worker and rejects late output from a busy worker. Worker failures close the output and expose a retryable status
message. Device-derived allocations are capped by rejecting preview rates above 1,536,000 Hz;
ordinary device rates and the existing browser inline path are retained. Synthetic tests cover
gated reads, rapid replacement, duplicate frames, project switches, shutdown, contention, poisoning, panics,
partial output failure and hostile device rates. These changes do not establish a native
scrubbing-latency target or physical audiovisual synchronization.
