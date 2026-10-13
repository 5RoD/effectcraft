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
    let g = e.g;
    if !g.has_kernel("video_yuv420") || !g.fits(frame.width(), frame.height()) {
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
    let mut bytes = Vec::new();
    bytes.try_reserve_exact(size).map_err(|e| format!("native video upload allocation: {e}"))?;
    for ((plane, len), padding) in frame.planes().iter().zip(lengths).zip(padded) {
        let data = plane.get(..len).ok_or("truncated native video upload plane")?;
        bytes.extend_from_slice(data);
        bytes.resize(bytes.len().saturating_add(padding - len), 0);
    }
    let data = g.buffer_init(Some("native YUV420 planes"), &bytes, wgpu::BufferUsages::STORAGE);
    let mut p = Params::default();
    p.u[0] = [frame.width(), frame.width().div_ceil(2), frame.height().div_ceil(2), 0];
    p.u[1] = [padded[0] as u32, padded[0].saturating_add(padded[1]) as u32, 0, 0];
    p.f[0] = frame.range();
    p.f[1] = frame.matrix();
    p.f[2] = [levels.unwrap_or(0.0), levels.map_or(0.0, |l| 1.0 / l), 0.0, 0.0];
    let mut out = e.scratch(frame.width(), frame.height());
    g.count_upload(size, started);
    let dummy = GpuImage::new(g.dummy_texture(), 1, 1);
    e.pixels("video_yuv420", &p, &dummy, None, &out, Some(&data));
    out.levels = levels;
    Ok(Some(out))
}
