# Direct native GPU readback

Native synchronous image downloads copy mapped GPU staging rows directly into the
final image allocation. Previously the mapped rows were packed into a temporary
byte vector and then copied into the image. At 2560×1440 with four f32 channels,
the removed temporary allocation and copy each span 58,982,400 bytes per frame.
The GPU transfer size, float precision and rendering work are unchanged.

This uses the existing bounded readback registry, device-failure bridge, deadline
and staging-buffer pool. Mapping completion signals the waiting worker; it reads
the mapped range and unmaps the buffer before reuse. Asynchronous and browser
readbacks retain their owned-byte callback contract. No additional worker is
created, and the GPU concurrency limit is unchanged.

Layout validation checks every source row and the exact destination length before
publishing pixels. Regression checks cover truncated rows, invalid dimensions and
stride, arithmetic overflow, extra mapped padding, exact HDR float bytes and
repeated downloads through reused staging buffers. Existing deadline,
cancellation, device-failure and deferred-render tests remain required.

This is a reduction in CPU allocation and copying, not an export speed claim.
Production export throughput and memory require a separate paired measurement.
