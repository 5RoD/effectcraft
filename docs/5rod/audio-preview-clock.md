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
