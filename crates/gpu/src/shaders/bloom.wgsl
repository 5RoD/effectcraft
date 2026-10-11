// Original checked Native Bloom core. Shared plan/box radii are in effects::bloom.
// u0 = (transfer curve, view, operation, component count)
// f0 = (threshold, knee width, exposure/amount gain, output padding)
// f1 = working-space XYZ Y coefficients
// f2..5 = (cell factor, local padding, atlas y, normalized weight)
// f6..9 = (component width, component height)

fn bloom_sane(value: f32, headroom: f32) -> f32 {
    // IEEE exponent test also rejects quiet/signaling NaNs without backend-specific predicates.
    if ((bitcast<u32>(value) & 0x7f800000u) == 0x7f800000u) { return 0.0; }
    return clamp(value, -headroom, headroom);
}

fn bloom_linear(pixel: vec4<f32>) -> vec4<f32> {
    let alpha = max(bloom_sane(pixel.w, 1.0), 0.0);
    if (alpha <= 0.0) { return vec4<f32>(0.0); }
    var color = vec3<f32>(bloom_sane(bloom_sane(pixel.x, 1e12) / alpha, 1e12), bloom_sane(bloom_sane(pixel.y, 1e12) / alpha, 1e12), bloom_sane(bloom_sane(pixel.z, 1e12) / alpha, 1e12));
    if (P.u[0].x != 0u) {
        color = vec3<f32>(curve_decode(P.u[0].x, color.x), curve_decode(P.u[0].x, color.y), curve_decode(P.u[0].x, color.z));
    }
    color = vec3<f32>(bloom_sane(color.x, 1e12), bloom_sane(color.y, 1e12), bloom_sane(color.z, 1e12));
    return vec4<f32>(color * alpha, alpha);
}

fn bloom_encoded(pixel: vec4<f32>) -> vec4<f32> {
    let alpha = max(bloom_sane(pixel.w, 1.0), 0.0);
    if (alpha <= 0.0) { return vec4<f32>(0.0); }
    var color = vec3<f32>(bloom_sane(pixel.x / alpha, 1e20), bloom_sane(pixel.y / alpha, 1e20), bloom_sane(pixel.z / alpha, 1e20));
    if (P.u[0].x != 0u) {
        color = vec3<f32>(curve_encode(P.u[0].x, color.x), curve_encode(P.u[0].x, color.y), curve_encode(P.u[0].x, color.z));
    }
    color = vec3<f32>(bloom_sane(color.x, 1e20), bloom_sane(color.y, 1e20), bloom_sane(color.z, 1e20));
    return vec4<f32>(color * alpha, alpha);
}

@compute @workgroup_size(16, 16)
fn bloom_extract(@builtin(global_invocation_id) gid: vec3<u32>) {
    let position = vec2<i32>(gid.xy);
    if (any(position >= out_dims())) { return; }
    let source = bloom_linear(textureLoad(src, position, 0));
    if (source.w <= 0.0) { textureStore(out, position, vec4<f32>(0.0)); return; }
    let light = max(dot(source.xyz, P.f[1].xyz) / source.w, 0.0);
    var excess = max(light - P.f[0].x, 0.0);
    let knee = P.f[0].y;
    if (knee > 0.0) {
        let x = clamp(light - P.f[0].x + knee, 0.0, 2.0 * knee);
        excess = max(excess, x * x / (4.0 * knee));
    }
    var fraction = 0.0;
    if (light > 0.0) { fraction = clamp(excess / light, 0.0, 1.0); }
    textureStore(out, position, source * fraction);
}

@compute @workgroup_size(16, 16)
fn bloom_reduce(@builtin(global_invocation_id) gid: vec3<u32>) {
    let position = vec2<i32>(gid.xy);
    if (any(position >= out_dims())) { return; }
    let p = position * 2;
    // Missing odd-edge cells are transparent, not repeated. Reduction never thresholds again.
    let total = tex_get(src, p.x, p.y) + tex_get(src, p.x + 1, p.y) + tex_get(src, p.x, p.y + 1) + tex_get(src, p.x + 1, p.y + 1);
    textureStore(out, position, total * 0.25);
}

fn bloom_atlas_get(position: vec2<i32>, size: vec2<i32>, atlas_y: i32) -> vec4<f32> {
    if (any(position < vec2<i32>(0)) || any(position >= size)) { return vec4<f32>(0.0); }
    return textureLoad(aux, position + vec2<i32>(0, atlas_y), 0);
}

fn bloom_component(index: u32, source_position: vec2<f32>) -> vec4<f32> {
    let level = P.f[index + 2u];
    let size = vec2<i32>(P.f[index + 6u].xy);
    let p = source_position / level.x + vec2<f32>(level.y - 0.5);
    let base = vec2<i32>(floor(p));
    let t = p - floor(p);
    let a = bloom_atlas_get(base, size, i32(level.z));
    let b = bloom_atlas_get(base + vec2<i32>(1, 0), size, i32(level.z));
    let c = bloom_atlas_get(base + vec2<i32>(0, 1), size, i32(level.z));
    let d = bloom_atlas_get(base + vec2<i32>(1, 1), size, i32(level.z));
    let top = a + (b - a) * t.x;
    let bottom = c + (d - c) * t.x;
    return (top + (bottom - top) * t.y) * level.w;
}

@compute @workgroup_size(16, 16)
fn bloom_finish(@builtin(global_invocation_id) gid: vec3<u32>) {
    let position = vec2<i32>(gid.xy);
    if (any(position >= out_dims())) { return; }
    if (P.u[0].y == 0u) {
        textureStore(out, position, bloom_encoded(textureLoad(aux, position, 0)));
        return;
    }
    let source_position = vec2<f32>(position) + vec2<f32>(0.5 - P.f[0].w);
    var glow = vec4<f32>(0.0);
    for (var index = 0u; index < P.u[0].w; index++) { glow += bloom_component(index, source_position); }
    glow *= P.f[0].z;
    glow.w = clamp(glow.w, 0.0, 1.0);
    if (P.u[0].y == 1u) {
        textureStore(out, position, bloom_encoded(glow));
        return;
    }
    let source = bloom_linear(tex_get(src, position.x - i32(P.f[0].w), position.y - i32(P.f[0].w)));
    var color = source.xyz + glow.xyz;
    if (P.u[0].z == 1u) { color -= clamp(source.xyz, vec3<f32>(0.0), vec3<f32>(source.w)) * clamp(glow.xyz, vec3<f32>(0.0), vec3<f32>(glow.w)); }
    let alpha = source.w + glow.w * (1.0 - source.w);
    textureStore(out, position, bloom_encoded(vec4<f32>(color, alpha)));
}
