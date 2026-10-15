//! Checked composition, ROI and solid sizes, before a CPU image or GPU canvas allocation.
//! These are per-image limits, not a total renderer/decoder/effect memory budget.

/// CPU rasters follow project dimensions; a GPU's smaller texture limit is checked by its
/// uploader. The separate pixel/byte cap still rejects large square allocations.
pub const MAX_DIMENSION: u32 = effectcraft_project::limits::MAX_SIZE;
pub const MAX_PIXELS: u64 = 32 * 1024 * 1024;
pub const MAX_ROI_OFFSET: f64 = 1e12;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RasterRegion {
    pub offset: [f64; 2],
    pub size: [u32; 2],
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct FrameRaster {
    pub size: [u32; 2],
    pub roi: Option<RasterRegion>,
}

impl FrameRaster {
    pub fn output_size(self) -> [u32; 2] {
        self.roi.map_or(self.size, |region| region.size)
    }
}

fn scaled_size(width: f64, height: f64, scale: f64, round: fn(f64) -> f64) -> Result<[u32; 2], String> {
    if !scale.is_finite() || scale <= 0.0 {
        return Err("render scale must be finite and greater than zero".into());
    }
    if !width.is_finite() || !height.is_finite() || width <= 0.0 || height <= 0.0 {
        return Err("raster width and height must be finite and greater than zero".into());
    }
    let dimensions = [round(width * scale), round(height * scale)];
    if dimensions.iter().any(|dimension| !dimension.is_finite() || *dimension > f64::from(MAX_DIMENSION)) {
        return Err(format!("scaled raster exceeds the {MAX_DIMENSION}-pixel dimension limit"));
    }
    // Match the existing rounding and minimum one-pixel raster at small positive scales.
    // All floating-point products have been validated before these integer casts.
    let size = dimensions.map(|dimension| dimension.max(1.0) as u32);
    let pixels = u64::from(size[0]).checked_mul(u64::from(size[1])).ok_or("raster pixel count overflow")?;
    if pixels > MAX_PIXELS {
        return Err(format!("scaled raster has {pixels} pixels; limit {MAX_PIXELS}"));
    }
    let bytes = usize::try_from(pixels).ok().and_then(|pixels| pixels.checked_mul(std::mem::size_of::<[f32; 4]>()));
    if bytes.is_none_or(|bytes| bytes > isize::MAX as usize) {
        return Err("raster byte capacity exceeds the address space".into());
    }
    Ok(size)
}

pub fn solid(width: u32, height: u32, scale: f64) -> Result<[u32; 2], String> {
    scaled_size(f64::from(width), f64::from(height), scale, f64::ceil)
}

pub fn roi(region: [f64; 4], scale: f64) -> Result<RasterRegion, String> {
    let size = scaled_size(region[2], region[3], scale, f64::round)?;
    if region.iter().any(|value| !value.is_finite()) || !(region[0] + region[2]).is_finite() || !(region[1] + region[3]).is_finite() {
        return Err("ROI coordinates and edges must be finite".into());
    }
    let offset = [(region[0] * scale).round(), (region[1] * scale).round()];
    if offset.iter().any(|value| !value.is_finite() || value.abs() > MAX_ROI_OFFSET) {
        return Err(format!("scaled ROI origin exceeds +/-{MAX_ROI_OFFSET} pixels"));
    }
    Ok(RasterRegion { offset, size })
}

pub fn frame(width: u32, height: u32, scale: f64, region: Option<[f64; 4]>) -> Result<FrameRaster, String> {
    let size = scaled_size(f64::from(width), f64::from(height), scale, f64::round)?;
    let roi = region.map(|region| roi(region, scale)).transpose()?;
    Ok(FrameRaster { size, roi })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn checked_raster_plan_rejects_hostile_products_before_casts_and_capacity() {
        for scale in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY, -1.0, 0.0, f64::MAX] {
            assert!(frame(8, 8, scale, None).is_err(), "scale {scale}");
            assert!(solid(8, 8, scale).is_err(), "scale {scale}");
        }
        for size in [[u32::MAX, u32::MAX], [MAX_DIMENSION + 1, 1], [8192, 8192], [0, 8], [8, 0]] {
            assert!(frame(size[0], size[1], 1.0, None).is_err(), "{size:?}");
            assert!(solid(size[0], size[1], 1.0).is_err(), "{size:?}");
        }
        for region in [
            [0.0, 0.0, f64::MAX, f64::MAX],
            [f64::NAN, 0.0, 8.0, 8.0],
            [0.0, f64::INFINITY, 8.0, 8.0],
            [f64::MAX, 0.0, 8.0, 8.0],
            [0.0, 0.0, -1.0, 8.0],
            [0.0, 0.0, 0.0, 8.0],
            [0.0, 0.0, 8192.0, 8192.0],
        ] {
            assert!(frame(8, 8, 1.0, Some(region)).is_err(), "ROI {region:?}");
        }
    }

    #[test]
    fn checked_raster_plan_preserves_rounding_normal_scales_and_supported_8k() {
        for scale in [0.25, 0.5, 1.0, 2.0, 4.0] {
            let plan = frame(193, 109, scale, Some([-7.0, -3.0, 77.0, 39.0])).unwrap();
            assert_eq!(plan.size, [(193.0 * scale).round() as u32, (109.0 * scale).round() as u32]);
            let region = plan.roi.unwrap();
            assert_eq!(region.offset, [(-7.0 * scale).round(), (-3.0 * scale).round()]);
            assert_eq!(region.size, [(77.0 * scale).round() as u32, (39.0 * scale).round() as u32]);
            assert_eq!(solid(193, 109, scale).unwrap(), [(193.0 * scale).ceil() as u32, (109.0 * scale).ceil() as u32]);
        }
        assert_eq!(frame(7680, 4320, 1.0, None).unwrap().size, [7680, 4320]);
        assert_eq!(frame(8192, 4096, 1.0, None).unwrap().size, [8192, 4096]);
        assert_eq!(frame(30_000, 1, 1.0, None).unwrap().size, [30_000, 1]);
        assert_eq!(solid(1, 30_000, 1.0).unwrap(), [1, 30_000]);
        assert_eq!(frame(1, 1, f64::MIN_POSITIVE, Some([0.0, 0.0, 0.1, 0.1])).unwrap().output_size(), [1, 1]);
        assert_eq!(roi([-MAX_ROI_OFFSET, MAX_ROI_OFFSET, 1.0, 1.0], 1.0).unwrap().size, [1, 1]);
    }
}
