//! Compact, validated decoded video planes, independent of the codec implementation.

use std::sync::Arc;

/// An opaque 8-bit 4:2:0 frame with centred chroma. Planes retain the decoder's shared
/// allocations; a GPU consumer need not allocate an intermediate float image.
#[derive(Debug)]
pub struct Yuv420Frame {
    width: u32,
    height: u32,
    planes: [Arc<Vec<u8>>; 3],
    /// (y_scale, y_offset, chroma_scale, chroma_offset).
    range: [f32; 4],
    /// (Cr→R, Cr→G, Cb→G, Cb→B), with the green terms subtracted.
    matrix: [f32; 4],
}

impl Yuv420Frame {
    pub fn new(width: u32, height: u32, planes: [Arc<Vec<u8>>; 3], range: [f32; 4], matrix: [f32; 4]) -> Result<Self, String> {
        crate::raster_bounds::solid(width, height, 1.0)?;
        if range.iter().chain(&matrix).any(|v| !v.is_finite()) {
            return Err("non-finite decoded video colour coefficients".into());
        }
        if range[0] <= 0.0
            || range[0] > 1.0
            || range[2] <= 0.0
            || range[2] > 1.0
            || !(0.0..=255.0).contains(&range[1])
            || !(0.0..=255.0).contains(&range[3])
            || matrix.iter().any(|v| v.abs() > 8.0)
        {
            return Err("decoded video colour coefficients outside supported bounds".into());
        }
        let frame = Self { width, height, planes, range, matrix };
        for (plane, len) in frame.planes.iter().zip(frame.plane_lengths()) {
            if plane.len() < len {
                return Err("truncated decoded video plane".into());
            }
        }
        Ok(frame)
    }

    pub fn width(&self) -> u32 {
        self.width
    }
    pub fn height(&self) -> u32 {
        self.height
    }
    pub fn range(&self) -> [f32; 4] {
        self.range
    }
    pub fn matrix(&self) -> [f32; 4] {
        self.matrix
    }
    pub fn planes(&self) -> &[Arc<Vec<u8>>; 3] {
        &self.planes
    }

    /// Construction checks the raster/address-space limits before these products.
    pub fn plane_lengths(&self) -> [usize; 3] {
        let y = self.width as usize * self.height as usize;
        let c = self.width.div_ceil(2) as usize * self.height.div_ceil(2) as usize;
        [y, c, c]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_hostile_geometry_planes_and_coefficients() {
        let planes = || [Arc::new(vec![0; 15]), Arc::new(vec![0; 6]), Arc::new(vec![0; 6])];
        assert!(Yuv420Frame::new(5, 3, planes(), [1.0; 4], [1.0; 4]).is_ok());
        assert!(Yuv420Frame::new(5, 4, planes(), [1.0; 4], [1.0; 4]).is_err());
        assert!(Yuv420Frame::new(0, 3, planes(), [1.0; 4], [1.0; 4]).is_err());
        assert!(Yuv420Frame::new(u32::MAX, u32::MAX, planes(), [1.0; 4], [1.0; 4]).is_err());
        assert!(Yuv420Frame::new(5, 3, planes(), [f32::NAN; 4], [1.0; 4]).is_err());
        assert!(Yuv420Frame::new(5, 3, planes(), [1.0; 4], [f32::INFINITY; 4]).is_err());
    }
}
