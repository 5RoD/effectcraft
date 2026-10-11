# Checked raster dimensions

Composition canvases, top-level region-of-interest (ROI) canvases/crops and solid
sources now share a checked plan on the CPU and GPU paths. Nested compositions,
collapsed compositions, Preserve Resolution sources and cached layer inputs use
the same checks before those sizes can allocate a buffer or bypass source evaluation.

The rendered dimensions must be positive and at most **16,384 pixels per axis**,
with at most **33,554,432 pixels per image** (512 MiB for premultiplied RGBA-f32).
Checked pixel/byte arithmetic also verifies the address-space capacity. These bounds
match the Native Bloom input convention. A normal 7,680 × 4,320 8K frame fits;
8,192 × 8,192 does not. Output scales of 2×/4× remain supported when their rendered
dimensions fit. Small positive scales retain the existing one-pixel minimum.
The scaled full composition must fit even with a small ROI selected: 3D ROI rendering
may allocate the full frame before cropping.

Scale must be finite and greater than zero. ROI coordinates, dimensions and edges
must be finite; dimensions must be positive. Rendered ROI origins may be negative
for the extended viewer, but their magnitude is bounded to 1e12 pixels so crop/index
arithmetic remains representable. Composition dimensions use the existing rounding,
and solid dimensions use the existing ceiling; ordinary pixels do not change.

A rejected plan records a contextual `render.raster` frame failure. Checked preview
and export consumers reject its provisional image, do not cache it, and preserve the
document. Preview retains the last completed frame and exposes its existing Retry
action. GPU fallback must pass the same CPU validation.
Video export validates composition size before codec setup can allocate a frame;
finite positive Render Settings keep their existing 0.01–4 resolution clamp.
Deferred ready-frame lookups validate current composition geometry before returning
pixels. The fork's render-cache salt advances to `effectcraft-5rod-render-3`, so
upstream and earlier candidate disk pixels cannot bypass these bounds. The disk
file format remains `v1`.

This is a focused pre-allocation fix, **not a total renderer memory budget**. An image
within these limits may still fail when system/device memory is exhausted. Decoder
buffers, Output Module crop/resize, transformed text/shape bounds, auxiliary channels and legacy effect/style
intermediates need separate allocation audits and budgeting. The general `Image`
constructor remains unchanged; it never silently substitutes an empty image.
