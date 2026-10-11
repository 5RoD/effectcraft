# Completed-edit recovery — development acceptance

The native desktop recovery journal closes the timer checkpoint gap for completed commands.
It is enabled by **Project → Auto-Save → Protect Every Completed Edit** independently of the
rotating autosave timer. Headless sessions that do not start crash recovery and browser sessions
retain their existing saving behavior.

A checked project/history checkpoint starts an epoch in the settings directory. Subsequent
atomic JSON records store changed composition metadata, layer patches, removed items, new
snapshot IDs and changed history suffixes. An unchanged composition is compared by shared
identity; unchanged layer property trees are not serialized again. Recovery applies data patches
and restores undo, redo, abandoned branches and their cursor. It does not rerun expressions,
scripts, importers or plugins.

Each completed outer command writes once after nested commands and batch folding finish.
Direct undo, redo, history jumps and edits also checkpoint. Atomic batches recover their final
success or rollback; a process killed during an unfinished batch can recover the preceding
completed operation. Continuous gestures still produce one undo step, with the latest completed
update protected. Native storage flushes the adjacent temporary file before renaming it. Only
after a successful write and sentinel publication is the revision reported as stored.

Write errors keep the last complete journal intact and display a persistent recovery warning
with **Retry Recovery Write**. Edits remain available in memory. Automatic journal writes stop
until retry or a successful manual project save, avoiding repeated expensive checkpoint attempts
on unavailable storage. Retry writes a fresh checked checkpoint containing all currently retained
edits. A successful newer timer checkpoint takes precedence over an older journal after a write
failure. Missing, reordered, unsupported or corrupt records stop recovery at its last valid state
and preserve the remaining journal with a warning.

The retry command waits for its checked replacement before returning the stored revision; another
write failure returns an error and retains the prior acknowledgment. A retry inside an unfinished
script or batch is rejected, so it cannot publish an intermediate transaction. Failed atomic renames
also remove their temporary file without changing the destination.

Successful manual saves start a fresh epoch containing the saved history. The prior epoch is
removed only after its replacement is published successfully. Clean exit removes only the
validated current journal's JSON records. Materialized `Recovered.ecproj` files are retained.
Original projects are not overwritten during recovery.

Native runs use separate recovery markers with nonblocking operating-system file leases. A
live session is skipped at startup; a terminated process releases its lease without requiring
PID-liveness guesses. A recovery dialog claims each dead session until it is resolved, so a
second new instance cannot consume the same choice. Multiple crashed projects are offered one
at a time. Failed opens leave their marker in place. Resolving a choice removes only its marker;
its recovered project and original journal are retained. An unresolved choice remains available
after a later launch. Rotating autosaves use a separate `Sessions/<run>` subfolder for each run,
including when two windows edit the same saved project.

`file.recoveryInfo` and `project.summary` expose the current revision, stored recovery revision and
persistent error. `file.retryEditRecovery` retries through the command/control interfaces.
`file.recoveryInfo` also exposes the offered and pending recovery choices. After successfully
opening a choice, `file.resolveRecovery` advances to the next one; `dismiss: true` explicitly
skips that choice while retaining its recovered file and journal.

Checkpoint reads/writes are limited to 512 MiB and individual edit records to 64 MiB. Recovery
rotates to a checked replacement epoch before exceeding 16,384 records or 1 GiB of encoded
journal data. Replay enforces those limits too; an oversized/tampered tail is preserved and
reported. A failed rotation keeps the previous acknowledged epoch and shows the retry warning.
Recovery markers are limited to 1 MiB each; native marker enumeration stops at 1024 entries. Unsupported
sizes produce a visible error and preserve the existing recovery files. Project/item/layer and
property IDs must stay below the exact JavaScript-integer boundary `(2^53)-1`; unsupported IDs
return an error before document loading/repair or edit commit instead of overflowing counters.

## Completed behavioral acceptance

The complete repository gate passed on 2026-10-11: 2,411 tests passed, 26 optional tests were
ignored, formatting and strict Clippy passed, dependency layering and all 30 asset attributions
passed, and 32 crates passed the WebAssembly check. Regression tests cover transactional replay,
invalid redo states, bounded input, write failures and checked retry acknowledgments.

A real Windows desktop run used three isolated processes and a private configuration folder.
It verified that a live instance is skipped, autosave slots remain separate, and another instance's
clean exit preserves the crashed journal. A filesystem rename failure produced a persistent
error; further edits remained available and retry acknowledged them only after writing. After
forced termination, recovery reproduced the entire Unicode project and portable history archive
exactly, restored all 27 history states, and navigated to an abandoned branch. Resolving the
recovery choice also closed the recovery dialog. The tested executable's SHA-256 was
`5b011f97668b0699ba1e563a82e045fbad45c7d7c61a0f9cac2e489c1bb83075`.

Twelve additional native helper-process probes exited without running destructors during partial
temporary writes, flushed temporary writes and atomic publication. They covered edit records,
session markers, replacement checkpoints and replacement markers. Every new recovery process
restored exactly the previous acknowledged or fully published project/history archive, as
appropriate to the boundary. The helper's SHA-256 was
`78ea2b2dc0ec2c0dc2a364eb6fe1b17e5431c5caa4fd71f19e7ba93fa9ee9a4c`.
These probes test the real native storage path; they are not desktop presentation tests.

An isolated exploratory storage benchmark ran three alternating protected/unprotected pairs,
each with 100 layers and 5,000 edits. With protection enabled, command medians were 4.02–4.23 ms,
p95 values were 5.04–5.14 ms and maxima were 43.16–44.72 ms. Every pair reproduced the full
5,103-state project/history archive exactly after reopening, including an abandoned branch.
Each protected run wrote 5,000 edit records and 5,000 markers. Record and marker flushing
accounted for most of the measured cost. Replay startup took 2.77–2.93 seconds and subsequent
project/history opening took 1.84–1.90 seconds. This release-without-LTO exploration excludes
UI, RPC, rendering and media decoding; it identifies work to optimize, not production performance
or a playback frame-rate claim. The machine ran no compiler or other EffectCraft test during
the measurement.

Both process suites used release optimization with LTO disabled for behavioral acceptance. They
do not establish production speed. The recovery UI was also rendered and inspected at small and
large window sizes. Production-profile launch/recovery, long-history storage latency and memory
measurements remain required. This increment is not included in the development ZIP at `d10346b`.

This is a process-crash recovery mechanism, not a promise against hardware loss, damaged storage
or deletion of the settings directory. A completed-edit write adds synchronous filesystem latency
to editing; the earlier edit benchmark did not include this new journal. Journal and project
history retain snapshots in memory during reconstruction; lazy disk-backed history and lower
reconstruction memory use remain work to do. The measured process-crash behavior does not establish
protection against all operating-system or hardware failure modes, or make arbitrarily long
history inexpensive.
