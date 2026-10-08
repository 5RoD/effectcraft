# Glow numeric bounds

Glow evaluates its radius through one shared CPU/GPU plan. Finite radius values are clamped
to the control's documented range of **0–1000 layer pixels**. The radius multiplied by the
buffer's render scale is capped at **4096 rendered pixels**, with blur sigma and transparent
padding derived only from that validated value. Padding is therefore at most 6146 pixels
on each side.

The complete radius range retains its existing behavior at Full, Half, Quarter, and Eighth
resolution, and at 2× and 4× output scales. Larger collapsed-vector scales retain the same
behavior until their effective radius reaches the rendered-pixel cap. Non-finite radius,
non-finite or negative scale, or an overflowed radius/scale product uses zero radius.

These bounds prevent hostile evaluated values from overflowing Glow's blur and padding
arithmetic. A total image-memory budget remains a separate renderer concern.
Finite, supported settings retain the same thresholding, Gaussian approximation, color
mapping, and compositing behavior.
