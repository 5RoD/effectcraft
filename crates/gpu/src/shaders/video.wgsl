// Original opaque 8-bit planar YUV420 conversion. Input bytes remain integer values;
// centred chroma interpolation and matrix operation order mirror media::convert.
struct Params {
    u: array<vec4<u32>, 4>,
    f: array<vec4<f32>, 12>,
}
@group(0) @binding(0) var<uniform> P: Params;
@group(0) @binding(3) var out: texture_storage_2d<rgba32float, write>;
@group(0) @binding(4) var<storage, read> bytes: array<u32>;

fn video_byte(i: u32) -> f32 {
    return f32((bytes[i / 4u] >> ((i % 4u) * 8u)) & 255u);
}

fn video_neighbour(i: u32, n: u32) -> vec2<u32> {
    let c = min(i / 2u, n - 1u);
    // Saturating subtraction: WGSL select evaluates both branches.
    let o = select(min(c + 1u, n - 1u), c - min(c, 1u), (i & 1u) == 0u);
    return vec2<u32>(c, o);
}

fn video_chroma(offset: u32, x: vec2<u32>, y: vec2<u32>, width: u32) -> f32 {
    let a = video_byte(offset + y.x * width + x.x) * 0.75 + video_byte(offset + y.y * width + x.x) * 0.25;
    let b = video_byte(offset + y.x * width + x.y) * 0.75 + video_byte(offset + y.y * width + x.y) * 0.25;
    let na = (a - P.f[0].w) * P.f[0].z;
    let nb = (b - P.f[0].w) * P.f[0].z;
    return na * 0.75 + nb * 0.25;
}

// u[0]=(luma width, chroma width, chroma height, unused), u[1]=(Cb offset, Cr offset).
// f[0]=range coefficients, f[1]=matrix coefficients, f[2]=(levels, inverse levels).
@compute @workgroup_size(16, 16)
fn video_yuv420(@builtin(global_invocation_id) gid: vec3<u32>) {
    let dims = textureDimensions(out);
    if (gid.x >= dims.x || gid.y >= dims.y) { return; }
    let x = video_neighbour(gid.x, P.u[0].y);
    let y = video_neighbour(gid.y, P.u[0].z);
    let cb = video_chroma(P.u[1].x, x, y, P.u[0].y);
    let cr = video_chroma(P.u[1].y, x, y, P.u[0].y);
    let yy = (video_byte(gid.y * P.u[0].x + gid.x) - P.f[0].y) * P.f[0].x;
    var rgb = clamp(vec3<f32>(yy + P.f[1].x * cr, yy - P.f[1].y * cr - P.f[1].z * cb, yy + P.f[1].w * cb), vec3<f32>(0.0), vec3<f32>(1.0));
    if (P.f[2].x > 0.0) { rgb = round(rgb * P.f[2].x) * P.f[2].y; }
    textureStore(out, vec2<i32>(gid.xy), vec4<f32>(rgb, 1.0));
}
