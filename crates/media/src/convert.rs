//! Decoded pictures → compositor [`Image`]s (premultiplied f32 RGBA, sRGB/Rec.709-encoded values).
//!
//! Y'CbCr is converted with the matrix and range the stream signals (BT.601 / BT.709 / BT.2020,
//! limited or full), upsampling chroma bilinearly with centred siting (like ffmpeg's swscale,
//! which is also how most tools subsample when encoding).
//! Values stay gamma-encoded: like After Effects, footage pixels enter the working space as they
//! are encoded in the file. Alpha follows the footage's [`AlphaMode`] interpretation.

use effectcraft_project::{AlphaMode, Footage};
use effectcraft_raster::{Image, Px};
use filmcraft_color::{Matrix, Range};
use filmcraft_frame::{Chroma, PixelData, VideoFrame};
use rayon::prelude::*;

use crate::{MediaError, Result};

/// Match the compositor's per-image limits before trusting decoder dimensions.
pub(crate) fn frame_pixel_count(width: u32, height: u32) -> Result<usize> {
    effectcraft_render::raster_bounds::solid(width, height, 1.0).map_err(MediaError::Decode)?;
    usize::try_from(u64::from(width).saturating_mul(u64::from(height))).map_err(|e| MediaError::Decode(format!("frame pixel count: {e}")))
}

fn filled<T: Clone>(len: usize, value: T) -> Result<Vec<T>> {
    let mut data = Vec::new();
    data.try_reserve_exact(len).map_err(|e| MediaError::Decode(format!("frame allocation: {e}")))?;
    data.resize(len, value);
    Ok(data)
}

fn plane_row<T>(plane: &[T], row: usize, width: usize) -> Result<&[T]> {
    let start = row.checked_mul(width).ok_or_else(|| MediaError::Decode("frame row offset overflow".into()))?;
    let end = start.checked_add(width).ok_or_else(|| MediaError::Decode("frame row end overflow".into()))?;
    plane.get(start..end).ok_or_else(|| MediaError::Decode("truncated frame plane".into()))
}

pub(crate) fn validate_frame(f: &VideoFrame) -> Result<usize> {
    let len = frame_pixel_count(f.width, f.height)?;
    let channels = len.checked_mul(4).ok_or_else(|| MediaError::Decode("frame channel count overflow".into()))?;
    let short = || MediaError::Decode("truncated decoded frame buffer".into());
    let validate_yuv = |chroma: Chroma, planes: [usize; 3], alpha: Option<usize>| -> Result<()> {
        let (sx, sy) = chroma.shifts();
        let dx = 1usize.checked_shl(sx).ok_or_else(short)?;
        let dy = 1usize.checked_shl(sy).ok_or_else(short)?;
        let chroma_len = (f.width as usize).div_ceil(dx).checked_mul((f.height as usize).div_ceil(dy)).ok_or_else(short)?;
        if planes[0] < len || planes[1] < chroma_len || planes[2] < chroma_len || alpha.is_some_and(|n| n < len) {
            return Err(short());
        }
        Ok(())
    };
    match &f.data {
        PixelData::Rgba8(data) => {
            data.get(..channels).ok_or_else(short)?;
        }
        PixelData::RgbaF32(data) => {
            let data = data.get(..channels).ok_or_else(short)?;
            if data.iter().any(|v| !v.is_finite()) {
                return Err(MediaError::Decode("non-finite decoded float frame".into()));
            }
        }
        PixelData::Yuv8 { planes, chroma, alpha } => validate_yuv(*chroma, planes.each_ref().map(|p| p.len()), alpha.as_ref().map(|a| a.len()))?,
        PixelData::Yuv16 { planes, chroma, bits, alpha } => {
            if !(8..=16).contains(bits) {
                return Err(MediaError::Decode(format!("invalid decoded YUV bit depth: {bits}; expected 8–16")));
            }
            validate_yuv(*chroma, planes.each_ref().map(|p| p.len()), alpha.as_ref().map(|a| a.len()))?;
        }
    }
    Ok(len)
}

/// How to turn a (colour, alpha) sample into a premultiplied pixel.
#[derive(Clone, Copy)]
pub(crate) struct AlphaOp {
    mode: AlphaMode,
    matte: [f32; 3],
    /// Interpret Footage ▸ Preserve RGB: float values stay as the file stores them (not
    /// sRGB-encoded).
    preserve_rgb: bool,
}

impl AlphaOp {
    pub(crate) fn new(mode: AlphaMode, matte: [f32; 3]) -> Self {
        Self { mode, matte, preserve_rgb: false }
    }
    /// The interpretation of footage `f`.
    pub(crate) fn of(f: &Footage) -> Self {
        Self { preserve_rgb: f.preserve_rgb, ..Self::new(f.alpha, f.premul_color) }
    }
    /// A linear float value as the compositor takes it: sRGB-encoded, or the file's own value
    /// under Preserve RGB.
    #[inline(always)]
    fn float(self, v: f32) -> f32 {
        if self.preserve_rgb { v } else { encode(v) }
    }
    /// `c` is the file's colour (straight or premultiplied as the mode says), `a` its alpha.
    #[inline(always)]
    fn apply(self, c: [f32; 3], a: f32) -> Px {
        match self.mode {
            AlphaMode::Ignore => [c[0], c[1], c[2], 1.0],
            AlphaMode::Straight => [c[0] * a, c[1] * a, c[2] * a, a],
            AlphaMode::Premultiplied => {
                // premultiplied against a matte colour m: c = s·a + m·(1−a) → s·a = c − m·(1−a)
                let k = 1.0 - a;
                let m = self.matte;
                [(c[0] - m[0] * k).max(0.0), (c[1] - m[1] * k).max(0.0), (c[2] - m[2] * k).max(0.0), a]
            }
        }
    }
}

/// Convert a FilmCraft frame to an [`Image`], reusing `buf`'s allocation when it is large enough
/// (every pixel is overwritten, so a recycled frame's contents do not matter).
pub(crate) fn frame_to_image_in(f: &VideoFrame, op: AlphaOp, mut buf: Vec<Px>) -> Result<Image> {
    let len = validate_frame(f)?;
    if op.matte.iter().any(|v| !v.is_finite()) {
        return Err(MediaError::Decode("non-finite footage alpha matte".into()));
    }
    let (w, h) = (f.width as usize, f.height as usize);
    if buf.len() != len {
        buf = filled(len, [0.0; 4])?;
    }
    let mut img = Image { width: f.width, height: f.height, data: buf };
    if w == 0 || h == 0 {
        return Ok(img);
    }
    match &f.data {
        PixelData::Rgba8(d) => {
            img.data.par_chunks_mut(w).zip(d.par_chunks(w * 4)).for_each(|(row, src)| {
                for (o, s) in row.iter_mut().zip(src.as_chunks::<4>().0.iter()) {
                    *o = op.apply([U8[s[0] as usize], U8[s[1] as usize], U8[s[2] as usize]], U8[s[3] as usize]);
                }
            });
        }
        PixelData::RgbaF32(d) => {
            // linear premultiplied → encoded premultiplied
            img.data.par_chunks_mut(w).zip(d.par_chunks(w * 4)).try_for_each(|(row, src)| -> Result<()> {
                for (o, s) in row.iter_mut().zip(src.as_chunks::<4>().0.iter()) {
                    let a = s[3];
                    let inv = if a > 0.0 { 1.0 / a } else { 0.0 };
                    let c = [op.float(s[0] * inv) * a, op.float(s[1] * inv) * a, op.float(s[2] * inv) * a];
                    *o = if op.mode == AlphaMode::Ignore { [c[0], c[1], c[2], 1.0] } else { [c[0], c[1], c[2], a] };
                    if o.iter().any(|v| !v.is_finite()) {
                        return Err(MediaError::Decode("float footage conversion produced non-finite pixels".into()));
                    }
                }
                Ok(())
            })?;
        }
        PixelData::Yuv8 { planes, chroma, alpha } => {
            let yuv = Yuv::new(f.color.matrix, f.color.range, 8);
            let amax = 255.0;
            convert_yuv(&mut img, w, h, *chroma, [&planes[0][..], &planes[1][..], &planes[2][..]], alpha.as_deref().map(|a| &a[..]), amax, &yuv, op)?;
        }
        PixelData::Yuv16 { planes, chroma, bits, alpha } => {
            let yuv = Yuv::new(f.color.matrix, f.color.range, *bits);
            let amax = ((1u32 << *bits) - 1) as f32;
            convert_yuv(&mut img, w, h, *chroma, [&planes[0][..], &planes[1][..], &planes[2][..]], alpha.as_deref().map(|a| &a[..]), amax, &yuv, op)?;
        }
    }
    Ok(img)
}

/// A FilmCraft frame for a render that shows it at `w` × `h`, at most half its size each way:
/// what `resample(&frame_to_image(f, op), w, h)` makes, converting a quarter of the pixels.
/// Opaque 4:2:0 Y'CbCr is box-filtered to half size before conversion (each 2×2 block of luma,
/// and the [1 6 1]/8 neighbourhood of its chroma sample: the mean of the upsampled chroma its
/// pixels get), then resampled the rest of the way. Conversion is linear until it clips, so
/// this is the RGB box filter `resample` starts with, except in blocks where some colours clip.
/// `Ok(None)` for other valid frames; malformed decoded data is an error.
pub(crate) fn frame_to_image_scaled(f: &VideoFrame, w: u32, h: u32) -> Result<Option<Image>> {
    validate_frame(f)?;
    let (fw, fh) = (f.width as usize, f.height as usize);
    if w == 0 || h == 0 || fw % 2 != 0 || fh % 2 != 0 || (w as usize).saturating_mul(2) > fw || (h as usize).saturating_mul(2) > fh {
        return Ok(None);
    }
    let half = match &f.data {
        PixelData::Yuv8 { planes, chroma: Chroma::C420, alpha: None } => {
            half_420(fw, fh, [&planes[0][..], &planes[1][..], &planes[2][..]], &Yuv::new(f.color.matrix, f.color.range, 8))?
        }
        PixelData::Yuv16 { planes, chroma: Chroma::C420, bits, alpha: None } => {
            half_420(fw, fh, [&planes[0][..], &planes[1][..], &planes[2][..]], &Yuv::new(f.color.matrix, f.color.range, *bits))?
        }
        _ => return Ok(None),
    };
    Ok(Some(if (half.width, half.height) == (w, h) { half } else { effectcraft_raster::resample(&half, w, h) }))
}

/// An opaque 4:2:0 frame of even size `w` × `h` converted at half that size (see
/// [`frame_to_image_scaled`]); row bounds and scratch allocation remain fallible.
fn half_420<T: Sample>(w: usize, h: usize, [luma, cb, cr]: [&[T]; 3], k: &Yuv) -> Result<Image> {
    let (cw, ch) = (w / 2, h / 2);
    let mut img = Image { width: cw as u32, height: ch as u32, data: filled(cw * ch, [0.0; 4])? };
    img.data.par_chunks_mut(cw).enumerate().try_for_each_init(
        // Chroma rows with their edge samples repeated on both ends, for the horizontal [1 6 1].
        || -> Result<_> { Ok((filled(cw + 2, 0f32)?, filled(cw + 2, 0f32)?)) },
        |scratch, (cy, row)| -> Result<()> {
            let (uc, vc) = scratch.as_mut().map_err(|e| MediaError::Decode(e.to_string()))?;
            // The chroma rows above and below weigh 1/8, as `centred` upsampling averages out.
            let rows = [cy.saturating_sub(1), cy, (cy + 1).min(ch - 1)];
            for (dst, plane) in [(&mut *uc, cb), (&mut *vc, cr)] {
                let [above, at, below] = rows.map(|r| plane_row(plane, r, cw));
                let (above, at, below) = (above?, at?, below?);
                let middle = dst.get_mut(1..=cw).ok_or_else(|| MediaError::Decode("chroma scratch row too short".into()))?;
                for (d, ((a, b), c)) in middle.iter_mut().zip(above.iter().zip(at).zip(below)) {
                    *d = ((a.f() + c.f()) * 0.125 + b.f() * 0.75 - k.c_off) * k.c_scale;
                }
                let (Some(left), Some(right)) = (dst.get(1).copied(), dst.get(cw).copied()) else {
                    return Err(MediaError::Decode("empty chroma scratch row".into()));
                };
                let Some((first, rest)) = dst.split_first_mut() else { return Err(MediaError::Decode("empty chroma scratch row".into())) };
                *first = left;
                let Some(last) = rest.last_mut() else { return Err(MediaError::Decode("empty chroma scratch row".into())) };
                *last = right;
            }
            let (top, bottom) = (plane_row(luma, 2 * cy, w)?, plane_row(luma, 2 * cy + 1, w)?);
            let pixels = top.as_chunks::<2>().0.iter().zip(bottom.as_chunks::<2>().0).zip(uc.windows(3).zip(vc.windows(3)));
            for (o, ((a, b), (u, v))) in row.iter_mut().zip(pixels) {
                let (u, v) = ((u[0] + u[2]) * 0.125 + u[1] * 0.75, (v[0] + v[2]) * 0.125 + v[1] * 0.75);
                let yy = ((a[0].f() + a[1].f() + b[0].f() + b[1].f()) * 0.25 - k.y_off) * k.y_scale;
                *o = [(yy + k.cr_r * v).clamp(0.0, 1.0), (yy - k.cr_g * v - k.cb_g * u).clamp(0.0, 1.0), (yy + k.cb_b * u).clamp(0.0, 1.0), 1.0];
            }
            Ok(())
        },
    )?;
    Ok(img)
}

/// 0..255 → 0..1.
static U8: [f32; 256] = {
    let mut t = [0f32; 256];
    let mut i = 0;
    while i < 256 {
        t[i] = i as f32 / 255.0;
        i += 1;
    }
    t
};

/// Linear → sRGB-encoded (sign-preserving, unclamped above 1 for float footage).
#[inline]
fn encode(v: f32) -> f32 {
    if v < 0.0 {
        -encode(-v)
    } else if v <= 0.003_130_8 {
        v * 12.92
    } else {
        1.055 * v.powf(1.0 / 2.4) - 0.055
    }
}

/// Integer code values → normalised Y'CbCr and the matrix coefficients.
struct Yuv {
    y_scale: f32,
    y_off: f32,
    c_scale: f32,
    c_off: f32,
    cr_r: f32,
    cr_g: f32,
    cb_g: f32,
    cb_b: f32,
}

impl Yuv {
    fn new(m: Matrix, range: Range, bits: u32) -> Self {
        let s = (1u32 << (bits - 8)) as f32;
        let (y_scale, y_off, c_scale, c_off) = match range {
            Range::Limited => (1.0 / (219.0 * s), 16.0 * s, 1.0 / (224.0 * s), 128.0 * s),
            Range::Full => {
                let max = ((1u32 << bits) - 1) as f32;
                (1.0 / max, 0.0, 1.0 / max, (1u32 << (bits - 1)) as f32)
            }
        };
        let (kr, kb) = m.kr_kb();
        let kg = 1.0 - kr - kb;
        let cr_r = 2.0 * (1.0 - kr);
        let cb_b = 2.0 * (1.0 - kb);
        Self { y_scale, y_off, c_scale, c_off, cr_r, cr_g: cr_r * kr / kg, cb_g: cb_b * kb / kg, cb_b }
    }
}

/// Share validated native planes without expanding them into an RGBA float image.
pub(crate) fn frame_to_yuv420(f: &VideoFrame, footage: &Footage) -> Result<Option<effectcraft_render::video::Yuv420Frame>> {
    validate_frame(f)?;
    if footage.premul_color.iter().any(|v| !v.is_finite()) {
        return Err(MediaError::Decode("non-finite footage alpha matte".into()));
    }
    let PixelData::Yuv8 { planes, chroma: Chroma::C420, alpha: None } = &f.data else { return Ok(None) };
    let k = Yuv::new(f.color.matrix, f.color.range, 8);
    effectcraft_render::video::Yuv420Frame::new(f.width, f.height, planes.clone(), [k.y_scale, k.y_off, k.c_scale, k.c_off], [k.cr_r, k.cr_g, k.cb_g, k.cb_b])
        .map(Some)
        .map_err(MediaError::Decode)
}

trait Sample: Copy + Send + Sync {
    fn f(self) -> f32;
}
impl Sample for u8 {
    #[inline(always)]
    fn f(self) -> f32 {
        self as f32
    }
}
impl Sample for u16 {
    #[inline(always)]
    fn f(self) -> f32 {
        self as f32
    }
}

/// Centred chroma siting (as ffmpeg's swscale and JPEG): for a subsampled axis, full-resolution
/// sample `i` lies ¼ of a chroma sample from chroma sample `i/2` towards its neighbour on that
/// side. Returns (nearest chroma index, neighbour index); the weights are ¾ and ¼.
#[inline(always)]
fn centred(i: usize, n: usize) -> (usize, usize) {
    let c = (i >> 1).min(n - 1);
    let o = if i & 1 == 0 { c.saturating_sub(1) } else { (c + 1).min(n - 1) };
    (c, o)
}

#[allow(clippy::too_many_arguments)]
fn convert_yuv<T: Sample>(
    img: &mut Image,
    w: usize,
    h: usize,
    chroma: Chroma,
    planes: [&[T]; 3],
    alpha: Option<&[T]>,
    amax: f32,
    k: &Yuv,
    op: AlphaOp,
) -> Result<()> {
    let (sx, sy) = chroma.shifts();
    let cw = w.div_ceil(1 << sx);
    let ch = h.div_ceil(1 << sy);
    let inv_amax = 1.0 / amax;
    img.data.par_chunks_mut(w).enumerate().try_for_each_init(
        || -> Result<_> { Ok((filled(cw, 0f32)?, filled(cw, 0f32)?, filled(w, 0f32)?, filled(w, 0f32)?)) },
        |scratch, (y, row)| -> Result<()> {
            let (uc, vc, uf, vf) = scratch.as_mut().map_err(|e| MediaError::Decode(e.to_string()))?;
            // vertical chroma interpolation into normalised rows (Cb, Cr in −0.5..0.5)
            let (r0, r1, t) = if sy == 1 {
                let (c, o) = centred(y, ch);
                (c, o, 0.25)
            } else {
                (y, y, 0.0)
            };
            for (dst, plane) in [(&mut *uc, planes[1]), (&mut *vc, planes[2])] {
                let a = plane_row(plane, r0, cw)?;
                let b = plane_row(plane, r1, cw)?;
                for ((d, a), b) in dst.iter_mut().zip(a).zip(b) {
                    let v = a.f() * (1.0 - t) + b.f() * t;
                    *d = (v - k.c_off) * k.c_scale;
                }
            }
            // horizontal
            if sx == 1 {
                for (x, (u, v)) in uf.iter_mut().zip(vf.iter_mut()).enumerate() {
                    let (c, o) = centred(x, cw);
                    let sample = |plane: &[f32]| -> Result<f32> {
                        let (Some(a), Some(b)) = (plane.get(c), plane.get(o)) else { return Err(MediaError::Decode("chroma sample outside frame".into())) };
                        Ok(a * 0.75 + b * 0.25)
                    };
                    *u = sample(uc)?;
                    *v = sample(vc)?;
                }
            } else {
                uf.copy_from_slice(plane_row(uc, 0, w)?);
                vf.copy_from_slice(plane_row(vc, 0, w)?);
            }
            let yrow = plane_row(planes[0], y, w)?;
            let rgb = |luma: T, u: f32, v: f32| {
                let yy = (luma.f() - k.y_off) * k.y_scale;
                [(yy + k.cr_r * v).clamp(0.0, 1.0), (yy - k.cr_g * v - k.cb_g * u).clamp(0.0, 1.0), (yy + k.cb_b * u).clamp(0.0, 1.0)]
            };
            match alpha {
                // opaque: every alpha interpretation gives the colour itself
                None => {
                    for (o, ((yy, u), v)) in row.iter_mut().zip(yrow.iter().zip(uf.iter()).zip(vf.iter())) {
                        let c = rgb(*yy, *u, *v);
                        *o = [c[0], c[1], c[2], 1.0];
                    }
                }
                Some(al) => {
                    let arow = plane_row(al, y, w)?;
                    for (o, (((yy, u), v), a)) in row.iter_mut().zip(yrow.iter().zip(uf.iter()).zip(vf.iter()).zip(arow.iter())) {
                        *o = op.apply(rgb(*yy, *u, *v), (a.f() * inv_amax).clamp(0.0, 1.0));
                    }
                }
            }
            Ok(())
        },
    )
}

/// Convert a still decoded by the `image` crate. Float images (OpenEXR, float TIFF) hold linear
/// light and are sRGB-encoded here (values above 1 are kept), unless the footage preserves RGB.
pub(crate) fn dynamic_to_image(img: &image::DynamicImage, op: AlphaOp) -> Image {
    use image::DynamicImage as D;
    let (w, h) = (img.width(), img.height());
    let mut out = Image::new(w, h);
    let wu = w as usize;
    if wu == 0 || h == 0 {
        return out;
    }
    match img {
        D::ImageRgb32F(_) | D::ImageRgba32F(_) => {
            let px = img.to_rgba32f();
            let raw = px.as_raw();
            let premul = op.mode == AlphaMode::Premultiplied;
            out.data.par_chunks_mut(wu).enumerate().for_each(|(y, row)| {
                for (x, o) in row.iter_mut().enumerate() {
                    let s = &raw[(y * wu + x) * 4..(y * wu + x) * 4 + 4];
                    let a = s[3];
                    let c = if premul {
                        let inv = if a > 0.0 { 1.0 / a } else { 0.0 };
                        [op.float(s[0] * inv) * a, op.float(s[1] * inv) * a, op.float(s[2] * inv) * a]
                    } else {
                        [op.float(s[0]), op.float(s[1]), op.float(s[2])]
                    };
                    *o = op.apply(c, a.clamp(0.0, 1.0));
                }
            });
        }
        D::ImageLuma16(_) | D::ImageLumaA16(_) | D::ImageRgb16(_) | D::ImageRgba16(_) => {
            let px = img.to_rgba16();
            let raw = px.as_raw();
            let k = 1.0 / 65535.0;
            out.data.par_chunks_mut(wu).enumerate().for_each(|(y, row)| {
                for (x, o) in row.iter_mut().enumerate() {
                    let s = &raw[(y * wu + x) * 4..(y * wu + x) * 4 + 4];
                    *o = op.apply([s[0] as f32 * k, s[1] as f32 * k, s[2] as f32 * k], s[3] as f32 * k);
                }
            });
        }
        _ => {
            let px = img.to_rgba8();
            let raw = px.as_raw();
            out.data.par_chunks_mut(wu).enumerate().for_each(|(y, row)| {
                for (x, o) in row.iter_mut().enumerate() {
                    let s = &raw[(y * wu + x) * 4..(y * wu + x) * 4 + 4];
                    *o = op.apply([U8[s[0] as usize], U8[s[1] as usize], U8[s[2] as usize]], U8[s[3] as usize]);
                }
            });
        }
    }
    out
}

/// Straight RGBA `f32` pixels (layered stills: Photoshop, SVG) to an [`Image`].
pub(crate) fn straight_to_image(w: u32, h: u32, px: &[[f32; 4]], op: AlphaOp) -> Image {
    let mut out = Image::new(w, h);
    let wu = w as usize;
    if wu == 0 || h == 0 {
        return out;
    }
    out.data.par_chunks_mut(wu).enumerate().for_each(|(y, row)| {
        for (x, o) in row.iter_mut().enumerate() {
            let s = px[y * wu + x];
            *o = op.apply([s[0], s[1], s[2]], s[3].clamp(0.0, 1.0));
        }
    });
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    fn frame_to_image(f: &VideoFrame, op: AlphaOp) -> Image {
        frame_to_image_in(f, op, Vec::new()).expect("valid decoded test frame")
    }

    #[test]
    fn native_video_shares_planes_and_declines_other_valid_formats() {
        let footage = Footage::default();
        let mut f = yuv_frame(126, 91, 173, filmcraft_color::ColorInfo::REC709);
        let native = frame_to_yuv420(&f, &footage).unwrap().unwrap();
        let PixelData::Yuv8 { planes, alpha, .. } = &mut f.data else { panic!("test YUV") };
        for (a, b) in planes.iter().zip(native.planes()) {
            assert!(Arc::ptr_eq(a, b), "native transport must retain decoder allocations");
        }
        *alpha = Some(Arc::new(vec![255; 8]));
        assert!(frame_to_yuv420(&f, &footage).unwrap().is_none());
        f.data = PixelData::Rgba8(Arc::new(vec![255; 32]));
        assert!(frame_to_yuv420(&f, &footage).unwrap().is_none());
        f.data = PixelData::Rgba8(Arc::new(vec![255; 31]));
        assert!(frame_to_yuv420(&f, &footage).is_err(), "invalid data cannot be a successful fallback");
        let bad_matte = Footage { premul_color: [f32::NAN; 3], ..Default::default() };
        assert!(frame_to_yuv420(&yuv_frame(126, 91, 173, filmcraft_color::ColorInfo::REC709), &bad_matte).is_err());
    }

    #[test]
    fn malformed_rgba_frame_does_not_panic() {
        let frame = VideoFrame {
            width: 1,
            height: 1,
            data: PixelData::Rgba8(Arc::new(vec![255; 3])),
            color: filmcraft_color::ColorInfo::REC709,
            par: (1, 1),
            pts: filmcraft_time::Tick::ZERO,
        };
        let result = std::panic::catch_unwind(|| frame_to_image_in(&frame, AlphaOp::new(AlphaMode::Straight, [0.0; 3]), Vec::new()));
        assert!(result.is_ok(), "a truncated decoded frame must return an error, not panic");
        assert!(result.unwrap().unwrap_err().to_string().contains("truncated"));
    }

    fn frame_to_image_scaled(f: &VideoFrame, w: u32, h: u32) -> Option<Image> {
        super::frame_to_image_scaled(f, w, h).expect("valid decoded test frame")
    }

    #[test]
    fn malformed_yuv_planes_alpha_and_bit_depth_return_errors() {
        let op = AlphaOp::new(AlphaMode::Straight, [0.0; 3]);
        for plane in 0..3 {
            let mut f = yuv_frame(126, 128, 128, filmcraft_color::ColorInfo::REC709);
            if let PixelData::Yuv8 { planes, .. } = &mut f.data {
                planes[plane] = Arc::new(Vec::new());
            }
            assert!(frame_to_image_in(&f, op, Vec::new()).is_err());
            assert!(super::frame_to_image_scaled(&f, 2, 1).is_err());
        }
        let mut f = yuv_frame(126, 128, 128, filmcraft_color::ColorInfo::REC709);
        if let PixelData::Yuv8 { alpha, .. } = &mut f.data {
            *alpha = Some(Arc::new(vec![255; 7]));
        }
        assert!(frame_to_image_in(&f, op, Vec::new()).is_err());
        for bits in [0, 1, 7, 17, 31, 32, u32::MAX] {
            f.data =
                PixelData::Yuv16 { planes: [Arc::new(vec![512; 8]), Arc::new(vec![512; 2]), Arc::new(vec![512; 2])], chroma: Chroma::C420, bits, alpha: None };
            let outcome = std::panic::catch_unwind(|| frame_to_image_in(&f, op, Vec::new()));
            assert!(outcome.unwrap().unwrap_err().to_string().contains("bit depth"), "bits {bits}");
            assert!(super::frame_to_image_scaled(&f, 2, 1).is_err());
        }
        // A failed conversion leaves the next valid conversion intact.
        assert!(frame_to_image_in(&yuv_frame(235, 128, 128, filmcraft_color::ColorInfo::REC709), op, Vec::new()).unwrap().data.iter().all(|p| p[3] == 1.0));
    }

    #[test]
    fn hostile_frame_dimensions_and_non_finite_float_pixels_are_bounded_errors() {
        let op = AlphaOp::new(AlphaMode::Straight, [0.0; 3]);
        let mut f = yuv_frame(126, 128, 128, filmcraft_color::ColorInfo::REC709);
        for (width, height) in [(u32::MAX, u32::MAX), (16_385, 1), (8192, 8192), (0, 1)] {
            f.width = width;
            f.height = height;
            assert!(frame_to_image_in(&f, op, Vec::new()).is_err());
        }
        f.width = 1;
        f.height = 1;
        for value in [f32::NAN, f32::INFINITY, f32::NEG_INFINITY] {
            f.data = PixelData::RgbaF32(Arc::new(vec![value, 0.0, 0.0, 1.0]));
            assert!(frame_to_image_in(&f, op, Vec::new()).is_err());
        }
        f.data = PixelData::RgbaF32(Arc::new(vec![f32::MAX, 0.0, 0.0, 1e-40]));
        assert!(frame_to_image_in(&f, op, Vec::new()).is_err());
    }

    fn yuv_frame(y: u8, u: u8, v: u8, color: filmcraft_color::ColorInfo) -> VideoFrame {
        VideoFrame {
            width: 4,
            height: 2,
            data: PixelData::Yuv8 { planes: [Arc::new(vec![y; 8]), Arc::new(vec![u; 2]), Arc::new(vec![v; 2])], chroma: Chroma::C420, alpha: None },
            color,
            par: (1, 1),
            pts: filmcraft_time::Tick::ZERO,
        }
    }

    #[test]
    fn limited_range_grey_and_white() {
        let op = AlphaOp::new(AlphaMode::Straight, [0.0; 3]);
        let img = frame_to_image(&yuv_frame(235, 128, 128, filmcraft_color::ColorInfo::REC709), op);
        assert!(img.data.iter().all(|p| (p[0] - 1.0).abs() < 1e-5 && (p[1] - 1.0).abs() < 1e-5 && p[3] == 1.0));
        let img = frame_to_image(&yuv_frame(126, 128, 128, filmcraft_color::ColorInfo::REC709), op);
        assert!((img.data[0][0] - 110.0 / 219.0).abs() < 1e-5);
    }

    /// A smooth opaque 4:2:0 frame whose colours never clip.
    fn smooth_420(w: usize, h: usize) -> VideoFrame {
        let plane = |w: usize, h: usize, base: f32, amp: f32, fx: f32, fy: f32| -> Arc<Vec<u8>> {
            Arc::new((0..w * h).map(|i| (base + amp * ((i % w) as f32 * fx + (i / w) as f32 * fy).sin()) as u8).collect())
        };
        VideoFrame {
            width: w as u32,
            height: h as u32,
            data: PixelData::Yuv8 {
                planes: [plane(w, h, 125.0, 45.0, 0.31, 0.17), plane(w / 2, h / 2, 128.0, 10.0, 0.4, -0.3), plane(w / 2, h / 2, 128.0, 10.0, -0.2, 0.5)],
                chroma: Chroma::C420,
                alpha: None,
            },
            color: filmcraft_color::ColorInfo::REC709,
            par: (1, 1),
            pts: filmcraft_time::Tick::ZERO,
        }
    }

    #[test]
    fn scaled_conversion_matches_resampling_the_converted_frame() {
        let f = smooth_420(96, 64);
        let full = frame_to_image(&f, AlphaOp::new(AlphaMode::Straight, [0.0; 3]));
        for (w, h) in [(48, 32), (32, 21), (24, 16), (13, 9)] {
            let scaled = frame_to_image_scaled(&f, w, h).expect("4:2:0, at most half size");
            let want = effectcraft_raster::resample(&full, w, h);
            assert_eq!((scaled.width, scaled.height), (w, h));
            let worst = scaled.data.iter().zip(&want.data).flat_map(|(a, b)| (0..4).map(move |c| (a[c] - b[c]).abs())).fold(0.0f32, f32::max);
            assert!(worst < 1e-5, "{w}×{h}: {worst}");
        }
        // More than half the size, odd sizes, other chroma layouts: convert the full frame.
        assert!(frame_to_image_scaled(&f, 49, 32).is_none());
        let mut odd = smooth_420(95, 64);
        if let PixelData::Yuv8 { planes, .. } = &mut odd.data {
            planes[1] = Arc::new(vec![128; 48 * 32]);
            planes[2] = Arc::new(vec![128; 48 * 32]);
        }
        assert!(frame_to_image_scaled(&odd, 40, 30).is_none());
        assert!(frame_to_image_scaled(&yuv_frame(100, 128, 128, filmcraft_color::ColorInfo::REC709), 2, 1).is_some());
        let mut f444 = smooth_420(96, 64);
        if let PixelData::Yuv8 { planes, chroma, .. } = &mut f444.data {
            *chroma = Chroma::C444;
            planes[1] = Arc::new(vec![128; 96 * 64]);
            planes[2] = Arc::new(vec![128; 96 * 64]);
        }
        assert!(frame_to_image_scaled(&f444, 48, 32).is_none());
    }

    #[test]
    fn bt709_red() {
        // 8-bit limited BT.709 red: Y 63, Cb 102, Cr 240
        let op = AlphaOp::new(AlphaMode::Straight, [0.0; 3]);
        let p = frame_to_image(&yuv_frame(63, 102, 240, filmcraft_color::ColorInfo::REC709), op).data[0];
        assert!(p[0] > 0.98 && p[1] < 0.02 && p[2] < 0.02, "{p:?}");
    }

    #[test]
    fn alpha_modes() {
        let c = [0.8, 0.4, 0.2];
        let s = AlphaOp::new(AlphaMode::Straight, [0.0; 3]).apply(c, 0.5);
        assert_eq!(s, [0.4, 0.2, 0.1, 0.5]);
        let p = AlphaOp::new(AlphaMode::Premultiplied, [0.0; 3]).apply([0.4, 0.2, 0.1], 0.5);
        assert_eq!(p, [0.4, 0.2, 0.1, 0.5]);
        // premultiplied against white: c = s·a + 1·(1−a)
        let p = AlphaOp::new(AlphaMode::Premultiplied, [1.0; 3]).apply([0.9, 0.7, 0.6], 0.5);
        assert!((p[0] - 0.4).abs() < 1e-6 && (p[2] - 0.1).abs() < 1e-6);
        let i = AlphaOp::new(AlphaMode::Ignore, [0.0; 3]).apply(c, 0.0);
        assert_eq!(i, [0.8, 0.4, 0.2, 1.0]);
    }

    #[test]
    fn float_stills_are_encoded() {
        let img = image::DynamicImage::ImageRgba32F(image::ImageBuffer::from_raw(1, 1, vec![0.2140, 1.0, 4.0, 1.0]).expect("buf"));
        let p = dynamic_to_image(&img, AlphaOp::new(AlphaMode::Premultiplied, [0.0; 3])).data[0];
        assert!((p[0] - 0.5).abs() < 1e-3 && (p[1] - 1.0).abs() < 1e-5 && p[2] > 1.5, "{p:?}");
    }

    /// `cargo test -p effectcraft-media --lib bench -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn bench_convert_1080p() {
        let (w, h) = (1920usize, 1080usize);
        let f = VideoFrame {
            width: w as u32,
            height: h as u32,
            data: PixelData::Yuv8 {
                planes: [Arc::new((0..w * h).map(|i| (i % 251) as u8).collect()), Arc::new(vec![100; w * h / 4]), Arc::new(vec![150; w * h / 4])],
                chroma: Chroma::C420,
                alpha: None,
            },
            color: filmcraft_color::ColorInfo::REC709,
            par: (1, 1),
            pts: filmcraft_time::Tick::ZERO,
        };
        let op = AlphaOp::new(AlphaMode::Straight, [0.0; 3]);
        for _ in 0..3 {
            let t = std::time::Instant::now();
            for _ in 0..20 {
                std::hint::black_box(Image::new(1920, 1080));
            }
            eprintln!("alloc {:?}", t.elapsed() / 20);
            let t = std::time::Instant::now();
            for _ in 0..20 {
                std::hint::black_box(frame_to_image(&f, op));
            }
            eprintln!("convert {:?}", t.elapsed() / 20);
            let mut keep = Vec::new();
            let t = std::time::Instant::now();
            for _ in 0..20 {
                keep.push(frame_to_image(&f, op));
            }
            eprintln!("convert into fresh memory {:?}", t.elapsed() / 20);
            let t = std::time::Instant::now();
            for _ in 0..20 {
                std::hint::black_box(frame_to_image_scaled(&f, 640, 360));
            }
            eprintln!("convert at a third {:?}", t.elapsed() / 20);
        }
    }
}
