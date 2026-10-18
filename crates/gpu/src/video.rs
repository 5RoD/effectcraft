//! Compact native video upload and float GPU conversion.

use effectcraft_render::video::Yuv420Frame;

use crate::context::{Enc, GpuImage, Params};

/// Preflight the enabled video stack before requesting compact source pixels. CPU-only
/// effects keep the reference source path; CC Composite also needs a CPU original snapshot.
/// Dynamic GPU-only controls are checked by the resident target, which can decline without
/// reading back or publishing a partially processed native source.
pub(crate) fn supports_effects(layer: &effectcraft_project::Layer, supports: impl Fn(&str) -> bool) -> bool {
    if !layer.switches.effects {
        return true;
    }
    layer.effects().is_none_or(|effects| {
        effects.groups().filter(|group| group.enabled).all(|group| {
            let effectcraft_project::GroupKind::Effect { effect } = &group.kind else { return true };
            let Some(spec) = effectcraft_effects::find(effect) else { return false };
            effectcraft_effects::audio_fx::is_audio_effect(spec.id) || (spec.gpu && spec.id != "ec.channel.cccomposite" && supports(spec.id))
        })
    })
}

/// Upload only the used plane bytes; padding keeps storage-buffer loads aligned.
/// `None` leaves unsupported device limits to the existing CPU source path.
pub(crate) fn upload(e: &mut Enc, frame: &Yuv420Frame, levels: Option<f32>) -> Result<Option<GpuImage>, String> {
    upload_inner(e, frame, levels, None)
}

/// Media's even, opaque 420 half-size prefilter followed by its ordinary bilinear resize.
/// Quantisation follows the final resize, before effects, just as on the CPU source path.
pub(crate) fn upload_reduced(e: &mut Enc, frame: &Yuv420Frame, levels: Option<f32>, size: (u32, u32)) -> Result<Option<GpuImage>, String> {
    upload_inner(e, frame, levels, Some(size))
}

fn upload_inner(e: &mut Enc, frame: &Yuv420Frame, levels: Option<f32>, reduced: Option<(u32, u32)>) -> Result<Option<GpuImage>, String> {
    let g = e.g;
    if let Some((w, h)) = reduced {
        if w == 0 || h == 0 || w > frame.width() / 2 || h > frame.height() / 2 {
            return Err("native reduced video dimensions must be nonzero and at most half the source".into());
        }
        if !frame.width().is_multiple_of(2) || !frame.height().is_multiple_of(2) {
            return Ok(None);
        }
    }
    let kernel = if reduced.is_some() { "video_yuv420_half" } else { "video_yuv420" };
    if !g.has_kernel(kernel) || !g.fits(frame.width(), frame.height()) {
        return Ok(None);
    }
    let started = web_time::Instant::now();
    let lengths = frame.plane_lengths();
    let padded = lengths.map(|n| n.div_ceil(4).saturating_mul(4));
    let size = padded.into_iter().try_fold(0usize, |n, len| n.checked_add(len)).ok_or("native video upload size overflow")?;
    let limits = g.device.limits();
    if size as u64 > limits.max_buffer_size || size as u64 > limits.max_storage_buffer_binding_size {
        return Ok(None);
    }
    let staging_size = wgpu::BufferSize::new(size as u64).ok_or("empty native video upload")?;
    let data = g.device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("native YUV420 planes"),
        size: staging_size.get(),
        usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });
    {
        // Assemble directly in staging memory: no temporary concatenation Vec or second
        // full-plane copy. Keep creation unmapped so driver allocation failures stay scoped.
        let mut staging = g.queue.write_buffer_with(&data, 0, staging_size).ok_or("cannot allocate native video upload staging buffer")?;
        let mut offset = 0usize;
        for ((plane, len), padding) in frame.planes().iter().zip(lengths).zip(padded) {
            let pixels_end = offset.checked_add(len).ok_or("native video upload offset overflow")?;
            let padded_end = offset.checked_add(padding).ok_or("native video upload padding overflow")?;
            if pixels_end > padded_end || padded_end > staging.len() {
                return Err("native video upload exceeds staging buffer".into());
            }
            let pixels = plane.get(..len).ok_or("truncated native video upload plane")?;
            let zeros = [0u8; 4];
            let padding_len = padding.checked_sub(len).ok_or("native video upload has invalid padding")?;
            let zeros = zeros.get(..padding_len).ok_or("native video upload padding exceeds alignment")?;
            // QueueWriteBufferView is write-only in wgpu 30. Validate its bounds and exact
            // lengths before using slice/copy_from_slice, which have no fallible variants.
            staging.slice(offset..pixels_end).copy_from_slice(pixels);
            if !zeros.is_empty() {
                staging.slice(pixels_end..padded_end).copy_from_slice(zeros);
            }
            offset = padded_end;
        }
    }
    let mut p = Params::default();
    p.u[0] = [frame.width(), frame.width().div_ceil(2), frame.height().div_ceil(2), 0];
    p.u[1] = [padded[0] as u32, padded[0].saturating_add(padded[1]) as u32, 0, 0];
    p.f[0] = frame.range();
    p.f[1] = frame.matrix();
    let initial_size = if reduced.is_some() { (frame.width() / 2, frame.height() / 2) } else { (frame.width(), frame.height()) };
    let resize = reduced.filter(|size| *size != initial_size);
    let initial_levels = if resize.is_some() { None } else { levels };
    p.f[2] = [initial_levels.unwrap_or(0.0), initial_levels.map_or(0.0, |l| 1.0 / l), 0.0, 0.0];
    let mut out = e.scratch(initial_size.0, initial_size.1);
    g.count_upload(size, started);
    let dummy = GpuImage::new(g.dummy_texture(), 1, 1);
    e.pixels(kernel, &p, &dummy, None, &out, Some(&data));
    out.levels = initial_levels;
    if let Some((w, h)) = resize {
        let transform = effectcraft_geom::Mat3::scale(effectcraft_geom::vec2(w as f64 / out.width as f64, h as f64 / out.height as f64));
        let blank = e.zeros(w, h);
        out =
            crate::ops::warp_q(e, &blank, &out, &transform, effectcraft_raster::Sampling::Bilinear, effectcraft_color::BlendMode::Normal, 1.0, 0, None, levels);
    }
    Ok(Some(out))
}
