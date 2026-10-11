//! Checked output-module geometry, before encoder setup or crop/resize allocation.
//! The renderer's existing per-image limits apply to every intermediate and coded frame.

use effectcraft_project::render_queue::OutputModule;
use effectcraft_raster::Image;

use crate::{ExportError, Result};

#[derive(Clone, Copy, Debug)]
pub(crate) struct Geometry {
    pub edges: (i32, i32, i32, i32),
    pub resized: [u32; 2],
    pub coded: [u32; 2],
}

fn invalid(message: impl std::fmt::Display) -> ExportError {
    ExportError::Render(format!("output crop/resize geometry: {message}"))
}

pub(crate) fn size(width: u32, height: u32) -> Result<usize> {
    effectcraft_render::raster_bounds::frame(width, height, 1.0, None).map_err(invalid)?;
    usize::try_from(u64::from(width) * u64::from(height)).map_err(invalid)
}

pub(crate) fn validate_image(image: &Image) -> Result<()> {
    if image.data.len() != size(image.width, image.height)? {
        return Err(invalid("frame buffer length does not match its dimensions"));
    }
    Ok(())
}

pub(crate) fn image(width: u32, height: u32) -> Result<Image> {
    let pixels = size(width, height)?;
    let mut data = Vec::new();
    data.try_reserve_exact(pixels).map_err(|error| invalid(format!("cannot allocate {width}x{height} frame: {error}")))?;
    data.resize(pixels, [0.0; 4]);
    Ok(Image { width, height, data })
}

/// Negative crop edges add transparent padding; cropping everything retains one pixel.
pub(crate) fn cropped_size(width: u32, height: u32, edges: (i32, i32, i32, i32)) -> Result<[u32; 2]> {
    let (top, left, bottom, right) = edges;
    let width = u32::try_from((i64::from(width) - i64::from(left) - i64::from(right)).max(1)).map_err(invalid)?;
    let height = u32::try_from((i64::from(height) - i64::from(top) - i64::from(bottom)).max(1)).map_err(invalid)?;
    size(width, height)?;
    Ok([width, height])
}

impl Geometry {
    pub fn new(width: u32, height: u32, scale: f64, output: &OutputModule) -> Result<Self> {
        size(width, height)?;
        if !scale.is_finite() || scale <= 0.0 {
            return Err(invalid("render scale must be finite and positive"));
        }
        let crop = &output.crop;
        let edges = if crop.enabled && crop.use_roi {
            let [x, y, rw, rh] = crop.roi.ok_or_else(|| invalid("Region of Interest crop requires a region"))?;
            if [x, y, rw, rh].iter().any(|value| !value.is_finite()) || rw <= 0.0 || rh <= 0.0 {
                return Err(invalid("ROI coordinates must be finite and its width and height positive"));
            }
            let values =
                [(y * scale).round(), (x * scale).round(), f64::from(height) - ((y + rh) * scale).round(), f64::from(width) - ((x + rw) * scale).round()];
            if values.iter().any(|value| !value.is_finite() || *value < f64::from(i32::MIN) || *value > f64::from(i32::MAX)) {
                return Err(invalid("scaled ROI edges exceed the signed 32-bit pixel range"));
            }
            let [top, left, bottom, right] = values.map(|value| value as i32);
            (top, left, bottom, right)
        } else {
            crop.edges(width, height, scale)
        };
        let [width, height] = cropped_size(width, height, edges)?;
        // Apply resize to the checked crop once; never run another crop on these dimensions.
        let (width, height) = output.resized_size(width, height);
        size(width, height)?;
        let coded = output.format.coded_size(width, height);
        size(coded.0, coded.1)?;
        Ok(Self { edges, resized: [width, height], coded: [coded.0, coded.1] })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use effectcraft_project::render_queue::{Crop, OutputFormat, Resize};

    #[test]
    fn every_output_stage_is_bounded_before_allocation() {
        let mut output = OutputModule::for_format(OutputFormat::PngSequence);
        output.crop = Crop { enabled: true, left: i32::MIN, ..Default::default() };
        assert!(Geometry::new(64, 48, 1.0, &output).is_err());
        // A small final resize cannot hide an unbounded intermediate crop/pad.
        output.resize = Resize { enabled: true, width: 8, height: 8, lock_aspect: false, ..Default::default() };
        assert!(Geometry::new(64, 48, 1.0, &output).is_err());
        output.crop = Crop::default();
        for (width, height) in [(30_000, 30_000), (8192, 8192), (u32::MAX, 1)] {
            output.resize.width = width;
            output.resize.height = height;
            assert!(Geometry::new(64, 48, 1.0, &output).is_err());
        }
        for roi in [[-f64::MAX, 0.0, 1.0, 1.0], [0.0, f64::NAN, 1.0, 1.0], [0.0, 0.0, f64::INFINITY, 1.0], [0.0, 0.0, -1.0, 1.0]] {
            output.crop = Crop { enabled: true, use_roi: true, roi: Some(roi), ..Default::default() };
            assert!(Geometry::new(64, 48, 1.0, &output).is_err());
        }
        assert!(validate_image(&Image { width: 4, height: 4, data: vec![[0.0; 4]] }).is_err());
    }

    #[test]
    fn crop_padding_aspect_and_codec_rounding_preserve_supported_sizes() {
        let mut output = OutputModule::for_format(OutputFormat::PngSequence);
        output.crop = Crop { enabled: true, top: 2, left: 1, bottom: 1, ..Default::default() };
        output.resize = Resize { enabled: true, width: 8, lock_aspect: true, ..Default::default() };
        assert_eq!(Geometry::new(8, 8, 1.0, &output).unwrap().resized, [8, 6]);
        output.crop = Crop { enabled: true, top: -1, left: -2, ..Default::default() };
        output.resize.enabled = false;
        assert_eq!(Geometry::new(8, 8, 1.0, &output).unwrap().coded, [10, 9]);
        output.crop = Crop { enabled: true, use_roi: true, roi: Some([16.0, 8.0, 32.0, 32.0]), ..Default::default() };
        assert_eq!(Geometry::new(32, 24, 0.5, &output).unwrap().coded, [16, 16]);
        output.crop = Crop::default();
        output.format = OutputFormat::H264;
        assert_eq!(Geometry::new(1, 3, 1.0, &output).unwrap().coded, [2, 2]);
        assert_eq!(size(7680, 4320).unwrap(), 33_177_600);
    }
}
