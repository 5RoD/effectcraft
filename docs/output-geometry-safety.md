# Output crop and resize geometry

Video export checks the scaled composition, cropped intermediate, resized frame and codec-rounded
frame before encoder setup. Each image uses the renderer's existing limits: at most 16,384 pixels
per dimension and 32 Mi pixels total. A small final resize cannot bypass an oversized intermediate
crop or padding allocation. Supported 7680×4320 output remains inside the pixel limit.

ROI coordinates must be finite, widths and heights positive, and scaled crop edges representable
as signed 32-bit pixel offsets. Invalid settings return an export error with crop/resize context.
Infallible project size queries use bounded arithmetic for display; their saturated answers do
not authorize allocation or silently repair the saved settings. Negative crop edges still add
transparent padding, and cropping away the full source retains the existing one-pixel minimum.

Resize derives the locked aspect ratio from the cropped frame once. Previously the frame pipeline
applied crop again when calculating resize dimensions, disagreeing with encoder dimensions and
adding transparent rows. Both quality choices retain their existing sampling behavior, including
bilinear fractional edge coverage. H.264 and HEVC keep their existing even-dimension rounding.

Crop allocation and the final bicubic frame allocation are fallible. Cropping checks source storage
and copies bounded visible row spans, leaving padding transparent. These are per-image checks,
not a total parallel export memory budget. The legacy bilinear resampler and its prefilter scratch
allocations still need a broader allocation-failure audit; this change does not establish that all
export or codec allocations are fallible.

Synthetic regressions cover the formerly overflowing minimum crop edge and extreme ROI origin,
malformed image storage, hostile padding/resize sizes, normal padding and ROI scaling, codec
rounding, and crop plus locked resize through PNG output. Invalid output geometry preserves the
existing destination and project and creates no provisional movie. Tests use generated content.
