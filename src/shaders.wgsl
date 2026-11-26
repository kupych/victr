// ViCTR — Physically based CRT television simulator (WGSL / wgpu)
//
// Signal textures are one NTSC field: 910 samples/line (4x subcarrier,
// 14.318 MHz) by 263 lines, including horizontal/vertical blanking and sync.

const SW: f32 = 910.0;
const NL: f32 = 263.0;
const SW_I: i32 = 910;
const NL_I: i32 = 263;
const ACT0: f32 = 134.0;
const ACT1: f32 = 887.0;
const VACT0: f32 = 21.0;
const VLINES: f32 = 240.0;
const PI: f32 = 3.14159265;
const TAU: f32 = 6.28318531;

// Packed 16-byte-aligned uniform block shared across all CRT pipeline passes.
struct Uniforms {
    // 0: time, field_phase, img_aspect, burn_rate_eff
    p0: vec4<f32>,
    // 1: frame, pattern, src_w, src_h
    u0: vec4<u32>,
    // 2: field, interlaced, vcr, has_img
    i0: vec4<i32>,
    // 3: input, comb, mask, bezel
    i1: vec4<i32>,
    // 4: flat.rgb, reset_trails (0 or 1)
    flat_reset: vec4<f32>,
    // 5: luma_sigma, i_sigma, q_sigma, chroma_delay
    bb0: vec4<f32>,
    // 6: vcr_sharp, tape_noise, chroma_noise, dropout
    bb1: vec4<f32>,
    // 7: snow, noise, ghost_amp, ghost_delay
    ch0: vec4<f32>,
    // 8: interf_amp, interf_freq, hum, impulse
    ch1: vec4<f32>,
    // 9: fine_tune, contrast, brightness, color
    dec0: vec4<f32>,
    // 10: tint, sharp, killer, rx_chroma_sigma
    dec1: vec4<f32>,
    // 11: view.xyz, curv
    view_curv: vec4<f32>,
    // 12: view_px.xy, corner, hsize
    vp0: vec4<f32>,
    // 13: vsize, hpos, vpos, pin
    geom0: vec4<f32>,
    // 14: breath, vphase, field_shift, overscan
    geom1: vec4<f32>,
    // 15: ripple, conv_dyn, spot, spot_h
    geom2: vec4<f32>,
    // 16: conv_r.xy, conv_b.xy
    conv: vec4<f32>,
    // 17: bloom, gamma, pitch, bm
    tube0: vec4<f32>,
    // 18: mask_strength, anti_moire, wire, purity
    tube1: vec4<f32>,
    // 19: purity_seed.xy, burn_amt, sat
    tube2: vec4<f32>,
    // 20: degauss.xyz, abl
    degauss_abl: vec4<f32>,
    // 21: focus.rgb, _pad
    focus: vec4<f32>,
    // 22: emission.rgb, _pad
    emission: vec4<f32>,
    // 23: cutoff.rgb, _pad
    cutoff: vec4<f32>,
    // 24: imax.rgb, _pad
    imax: vec4<f32>,
    // 25: collapse.xyzw
    collapse: vec4<f32>,
    // 26: decay.rgb, eye_k
    pers0: vec4<f32>,
    // 27: frac.rgb, tail_gain
    pers1: vec4<f32>,
    // 28: tau_fast.rgb, scan_phase
    scan0: vec4<f32>,
    // 29: tau_slow.rgb, scan_w
    scan1: vec4<f32>,
    // 30: scan_a, eye_k_eff, scan_mode (0/1), _pad
    scan2: vec4<f32>,
    // 31..33: phosphor matrix columns
    phos0: vec4<f32>,
    phos1: vec4<f32>,
    phos2: vec4<f32>,
    // 34: diffuse, halation, ambient, reflect
    glass0: vec4<f32>,
    // 35: vignette, exposure, blur_sigma_h, blur_sigma_w
    glass1: vec4<f32>,
    // 36: glass.rgb, _pad
    glass2: vec4<f32>,
    // 37: present_mode (0=SDR unorm, 1=SDR srgb, 2=scRGB nits, 3=Metal EDR, 4=HDR10 PQ), paper_white, peak_nits, ui_white
    hdr0: vec4<f32>,
    // 38: crt_rect_px (x0, y0, w, h) inside full window for PRESENT
    crt_rect: vec4<f32>,
    // 39: blur_dir.xy, blur_px.xy
    blur_p: vec4<f32>,
};

@group(0) @binding(0) var<uniform> U: Uniforms;
@group(0) @binding(1) var s_linear: sampler;
@group(0) @binding(2) var s_nearest: sampler;
@group(0) @binding(3) var s_repeat_x: sampler;
@group(0) @binding(4) var t_lines: texture_2d<f32>;
@group(0) @binding(5) var t_osd: texture_2d<f32>;
@group(1) @binding(0) var t0: texture_2d<f32>;
@group(1) @binding(1) var t1: texture_2d<f32>;
@group(1) @binding(2) var t2: texture_2d<f32>;
@group(1) @binding(3) var t3: texture_2d<f32>;

struct VsOut {
    @builtin(position) pos: vec4<f32>,
    @location(0) uv: vec2<f32>,
};

@vertex
fn vs_main(@builtin(vertex_index) vid: u32) -> VsOut {
    let x = f32((vid << 1u) & 2u);
    let y = f32(vid & 2u);
    var out: VsOut;
    out.pos = vec4<f32>(x * 2.0 - 1.0, 1.0 - y * 2.0, 0.0, 1.0);
    out.uv = vec2<f32>(x, y);
    return out;
}

// ---------------------------------------------------------------- helpers
fn gl_mod(x: f32, y: f32) -> f32 {
    return x - y * floor(x / y);
}

fn wrap_i(x: i32, n: i32) -> i32 {
    return ((x % n) + n) % n;
}

fn hashu(x_in: u32) -> u32 {
    var x = x_in;
    x = x ^ (x >> 16u); x = x * 0x7feb352du;
    x = x ^ (x >> 15u); x = x * 0x846ca68bu;
    x = x ^ (x >> 16u);
    return x;
}

fn rnd3(a: u32, b: u32, c: u32) -> f32 {
    return f32(hashu(a ^ hashu(b + 0x9e3779b9u ^ hashu(c + 0x85ebca6bu)))) * (1.0 / 4294967296.0);
}

fn gauss3(a: u32, b: u32, c: u32) -> f32 {
    let u1 = max(rnd3(a, b, c), 1e-7);
    let u2 = rnd3(a + 7919u, b, c + 104729u);
    return sqrt(-2.0 * log(u1)) * cos(TAU * u2);
}

fn hash12(p: vec2<f32>) -> f32 {
    var p3 = fract(vec3<f32>(p.x, p.y, p.x) * 0.1031);
    p3 += vec3<f32>(dot(p3, vec3<f32>(p3.y, p3.z, p3.x) + vec3<f32>(33.33)));
    return fract((p3.x + p3.y) * p3.z);
}

fn vnoise(p: vec2<f32>) -> f32 {
    let i = floor(p);
    let f = fract(p);
    let u = f * f * (3.0 - 2.0 * f);
    let a = hash12(i);
    let b = hash12(i + vec2<f32>(1.0, 0.0));
    let c = hash12(i + vec2<f32>(0.0, 1.0));
    let d = hash12(i + vec2<f32>(1.0, 1.0));
    return mix(mix(a, b, u.x), mix(c, d, u.x), u.y);
}

fn rgb2yiq(c: vec3<f32>) -> vec3<f32> {
    return vec3<f32>(
        dot(c, vec3<f32>(0.299, 0.587, 0.114)),
        dot(c, vec3<f32>(0.5959, -0.2746, -0.3213)),
        dot(c, vec3<f32>(0.2115, -0.5227, 0.3112)),
    );
}

fn yiq2rgb(y: vec3<f32>) -> vec3<f32> {
    return vec3<f32>(
        y.x + 0.9563 * y.y + 0.6210 * y.z,
        y.x - 0.2721 * y.y - 0.6474 * y.z,
        y.x - 1.1070 * y.y + 1.7046 * y.z,
    );
}

// Top-down pixel coord -> faceplate position (units: half the picture height, -1=top, +1=bottom).
fn faceFromPixel(fragxy: vec2<f32>, vpx: vec2<f32>, view: vec3<f32>, curv: f32) -> vec2<f32> {
    let ndc = fragxy / vpx * 2.0 - 1.0;
    let p = vec2<f32>(ndc.x * vpx.x / vpx.y, ndc.y) * view.z + view.xy;
    let r2 = dot(p, p);
    return p * (1.0 + curv * r2);
}

fn tubeSDF(f: vec2<f32>, corner: f32) -> f32 {
    let b = vec2<f32>(4.0 / 3.0, 1.0);
    let q = abs(f) - b + vec2<f32>(corner);
    return length(max(q, vec2<f32>(0.0))) + min(max(q.x, q.y), 0.0) - corner;
}

// ---------------------------------------------------------------- 1. SOURCE
fn pat_bars(uv: vec2<f32>) -> vec3<f32> {
    let idx = i32(clamp(floor(uv.x * 7.0), 0.0, 6.0));
    var top = array<vec3<f32>, 7>(
        vec3<f32>(0.75), vec3<f32>(0.75, 0.75, 0.0), vec3<f32>(0.0, 0.75, 0.75), vec3<f32>(0.0, 0.75, 0.0),
        vec3<f32>(0.75, 0.0, 0.75), vec3<f32>(0.75, 0.0, 0.0), vec3<f32>(0.0, 0.0, 0.75),
    );
    var mid = array<vec3<f32>, 7>(
        vec3<f32>(0.0, 0.0, 0.75), vec3<f32>(0.0), vec3<f32>(0.75, 0.0, 0.75), vec3<f32>(0.0),
        vec3<f32>(0.0, 0.75, 0.75), vec3<f32>(0.0), vec3<f32>(0.75),
    );
    if uv.y < 0.67 { return top[idx]; }
    if uv.y < 0.75 { return mid[idx]; }
    let x = uv.x * 28.0;
    if x < 5.0 { return vec3<f32>(0.0, 0.16, 0.30); }
    if x < 10.0 { return vec3<f32>(1.0); }
    if x < 15.0 { return vec3<f32>(0.20, 0.0, 0.42); }
    if x < 20.0 { return vec3<f32>(0.0); }
    if x < 21.33 { return vec3<f32>(-0.04); }
    if x < 22.67 { return vec3<f32>(0.0); }
    if x < 24.0 { return vec3<f32>(0.04); }
    return vec3<f32>(0.0);
}

fn pat_crosshatch(uv: vec2<f32>, sz: vec2<f32>) -> vec3<f32> {
    let px = floor(uv * sz);
    let cell = sz / vec2<f32>(16.0, 12.0);
    let m = vec2<f32>(gl_mod(px.x, cell.x), gl_mod(px.y, cell.y));
    var c = vec3<f32>(0.0);
    if m.x < 1.0 || m.y < 1.0 || px.x >= sz.x - 1.0 || px.y >= sz.y - 1.0 { c = vec3<f32>(1.0); }
    let cc = abs(m - cell * 0.5);
    if cc.x < 1.0 && cc.y < 1.0 { c = vec3<f32>(1.0); }
    let p = (uv - 0.5) * vec2<f32>(4.0 / 3.0, 1.0);
    let r = length(p);
    let pxs = 1.0 / sz.y;
    if abs(r - 0.42) < pxs * 0.8 { c = vec3<f32>(1.0); }
    return c;
}

fn pat_chart(uv: vec2<f32>) -> vec3<f32> {
    let p = (uv - 0.5) * vec2<f32>(4.0 / 3.0, 1.0);
    let g = abs(fract(p * 8.0 + 0.5) - 0.5);
    var c = vec3<f32>(0.42);
    if min(g.x, g.y) < 0.035 { c = vec3<f32>(0.85); }
    if abs(p.x) > 0.5 && abs(p.y) > 0.3 {
        if p.x < 0.0 && p.y < 0.0 { c = vec3<f32>(0.8, 0.05, 0.05); }
        if p.x > 0.0 && p.y < 0.0 { c = vec3<f32>(0.05, 0.8, 0.05); }
        if p.x < 0.0 && p.y > 0.0 { c = vec3<f32>(0.05, 0.05, 0.8); }
        if p.x > 0.0 && p.y > 0.0 { c = vec3<f32>(0.9); }
    }
    let r = length(p);
    if r < 0.46 {
        let y = p.y;
        let x = (p.x + 0.46) / 0.92;
        if y < -0.30 {
            let idx = i32(clamp(floor(x * 6.0), 0.0, 5.0));
            var hues = array<vec3<f32>, 6>(
                vec3<f32>(0.75, 0.75, 0.0), vec3<f32>(0.0, 0.75, 0.75), vec3<f32>(0.0, 0.75, 0.0),
                vec3<f32>(0.75, 0.0, 0.75), vec3<f32>(0.75, 0.0, 0.0), vec3<f32>(0.0, 0.0, 0.75),
            );
            c = hues[idx];
        } else if y < -0.15 {
            c = vec3<f32>(floor(x * 8.0) / 7.0);
        } else if y < 0.15 {
            let idx = i32(clamp(floor(x * 6.0), 0.0, 5.0));
            var mhz = array<f32, 6>(0.5, 1.0, 2.0, 3.0, 3.58, 4.2);
            let u = p.x * 0.75 + 0.5;
            c = vec3<f32>(0.5 + 0.4 * sin(TAU * u * mhz[idx] * 52.6));
        } else if y < 0.30 {
            c = vec3<f32>(0.0);
            if abs(p.x) < 0.06 { c = vec3<f32>(1.0); }
            if abs(p.x - 0.25) < 0.004 || abs(p.x + 0.25) < 0.004 { c = vec3<f32>(1.0); }
        } else {
            c = mix(vec3<f32>(0.85, 0.55, 0.42), vec3<f32>(0.35, 0.18, 0.12), x);
        }
        if abs(r - 0.455) < 0.006 { c = vec3<f32>(1.0); }
    }
    return c;
}

fn pat_demo(uv: vec2<f32>, sz: vec2<f32>, t: f32) -> vec3<f32> {
    let p = (uv - 0.5) * vec2<f32>(4.0 / 3.0, 1.0);
    let hz = 0.12;
    var c: vec3<f32>;
    if p.y < hz {
        let k = clamp((p.y + 0.5) / (hz + 0.5), 0.0, 1.0);
        c = mix(vec3<f32>(0.05, 0.02, 0.18), vec3<f32>(0.95, 0.35, 0.45), k * k);
        let st = floor(uv * sz * 0.5);
        let s = hash12(st);
        if s > 0.992 && k < 0.6 { c += vec3<f32>(0.8) * (0.6 + 0.4 * sin(t * 3.0 + s * 100.0)); }
        let sc = p - vec2<f32>(0.0, -0.02);
        if length(sc) < 0.2 {
            let sun = mix(vec3<f32>(1.0, 0.92, 0.3), vec3<f32>(1.0, 0.25, 0.55), clamp((sc.y + 0.2) / 0.4, 0.0, 1.0));
            let gap = sc.y > 0.0 && fract(sc.y * 22.0 - t * 0.6) < (sc.y * 3.0);
            if !gap { c = sun; }
        }
        let h = 0.05 + 0.035 * sin(p.x * 7.0 + 1.0) + 0.02 * sin(p.x * 19.0 + 2.3);
        if p.y > hz - h { c = vec3<f32>(0.16, 0.04, 0.28); }
    } else {
        let dz = p.y - hz;
        let z = 0.18 / max(dz, 1e-3);
        let gx = p.x * z;
        let gz = z + t * 2.0;
        let lw = 0.04 * z;
        let lx = abs(fract(gx * 1.5) - 0.5);
        let lz = abs(fract(gz * 0.75) - 0.5);
        c = vec3<f32>(0.03, 0.0, 0.10);
        let glow = smoothstep(0.08 + lw * 0.1, 0.0, min(lx, lz));
        c = mix(c, vec3<f32>(1.0, 0.2, 0.85), glow);
    }
    let bc = vec2<f32>(0.45 * sin(t * 0.9), 0.30 - abs(sin(t * 2.1)) * 0.42);
    let d = p - bc;
    let br = 0.075;
    if length(d) < br {
        let q = d / br;
        let zz = sqrt(max(1.0 - dot(q, q), 0.0));
        let tex = vec2<f32>(atan2(q.x, zz) + t * 2.0, asin(clamp(q.y, -1.0, 1.0)));
        let ch = gl_mod(floor(tex.x * 2.5) + floor(tex.y * 2.5), 2.0) < 1.0;
        let bcol = select(vec3<f32>(0.9, 0.05, 0.1), vec3<f32>(1.0), ch);
        let lit = 0.35 + 0.65 * clamp(dot(vec3<f32>(q, zz), normalize(vec3<f32>(-0.5, -0.6, 0.7))), 0.0, 1.0);
        c = bcol * lit;
    }
    let sx = fract(t * 0.35) * 1.8 - 0.9;
    let sp = abs(p - vec2<f32>(sx, -0.40));
    if max(sp.x, sp.y) < 0.03 { c = vec3<f32>(0.2, 1.0, 0.9); }
    return c;
}

fn pat_motion(uv: vec2<f32>, t: f32) -> vec3<f32> {
    let p = (uv - 0.5) * vec2<f32>(4.0 / 3.0, 1.0);
    var c = vec3<f32>(0.0);
    let bx = sin(t * 2.0) * 0.6;
    if abs(p.x - bx) < 0.02 && p.y < -0.1 { c = vec3<f32>(1.0); }
    let a = t * 1.3;
    let dir = vec2<f32>(cos(a), sin(a));
    let along = dot(p - vec2<f32>(0.0, 0.18), dir);
    let across = abs(dot(p - vec2<f32>(0.0, 0.18), vec2<f32>(-dir.y, dir.x)));
    if along > 0.0 && along < 0.25 && across < 0.006 { c = vec3<f32>(1.0); }
    for (var i: i32 = 0; i < 3; i += 1) {
        let fi = f32(i);
        let x = fract(t * (0.2 + 0.15 * fi)) * 2.4 - 1.2;
        var col = vec3<f32>(0.0, 0.0, 1.0);
        if i == 0 { col = vec3<f32>(1.0, 0.0, 0.0); }
        else if i == 1 { col = vec3<f32>(0.0, 1.0, 0.0); }
        if abs(p.y - (-0.05 + 0.1 * fi)) < 0.035 && abs(p.x - x) < 0.035 { c = col; }
    }
    return c;
}

fn pat_ramp(uv: vec2<f32>) -> vec3<f32> {
    if uv.y < 0.4 { return vec3<f32>(uv.x); }
    if uv.y < 0.6 {
        let x = uv.x * 10.0;
        if x > 3.0 && x < 4.0 { return vec3<f32>(-0.04); }
        if x > 4.5 && x < 5.5 { return vec3<f32>(0.04); }
        if x > 6.0 && x < 7.0 { return vec3<f32>(0.08); }
        return vec3<f32>(0.0);
    }
    return vec3<f32>(floor(uv.x * 11.0) / 10.0);
}

fn pat_image(uv: vec2<f32>) -> vec3<f32> {
    if U.i0.w == 0 { return vec3<f32>(0.1); }
    let ta = 4.0 / 3.0;
    let img_aspect = U.p0.z;
    var q = uv;
    if img_aspect > ta {
        let h = ta / img_aspect;
        q.y = (uv.y - 0.5) / h + 0.5;
    } else {
        let w = img_aspect / ta;
        q.x = (uv.x - 0.5) / w + 0.5;
    }
    if q.x < 0.0 || q.x > 1.0 || q.y < 0.0 || q.y > 1.0 { return vec3<f32>(0.0); }
    return textureSampleLevel(t0, s_linear, q, 0.0).rgb;
}

@fragment
fn fs_source(in: VsOut) -> @location(0) vec4<f32> {
    let sz = vec2<f32>(f32(U.u0.z), f32(U.u0.w));
    let uv = in.pos.xy / sz;
    let pat = U.u0.y;
    var c: vec3<f32>;
    if pat == 0u { c = pat_bars(uv); }
    else if pat == 1u { c = pat_crosshatch(uv, sz); }
    else if pat == 2u { c = pat_chart(uv); }
    else if pat == 3u { c = pat_demo(uv, sz, U.p0.x); }
    else if pat == 4u { c = pat_motion(uv, U.p0.x); }
    else if pat == 5u { c = pat_ramp(uv); }
    else if pat == 6u { c = U.flat_reset.rgb; }
    else { c = pat_image(uv); }
    return vec4<f32>(c, 1.0);
}

// ---------------------------------------------------------------- 2. BASEBAND
// t0 = src, t1 = lines
fn srcAt(n: f32, l: f32) -> vec3<f32> {
    if n < ACT0 || n >= ACT1 { return vec3<f32>(0.0); }
    let lv = l - VACT0;
    if lv < 0.0 || lv >= VLINES { return vec3<f32>(0.0); }
    let sz = vec2<f32>(f32(U.u0.z), f32(U.u0.w));
    let u = (n - ACT0) / (ACT1 - ACT0);
    let foff = select(0.0, f32(U.i0.x) * 0.5, U.i0.y == 1);
    let row = floor((lv + foff) * sz.y / VLINES);
    let px = vec2<i32>(
        i32(clamp(floor(u * sz.x), 0.0, sz.x - 1.0)),
        i32(clamp(row, 0.0, sz.y - 1.0)),
    );
    let yiq = rgb2yiq(textureLoad(t0, px, 0).rgb);
    return vec3<f32>(0.075 + 0.925 * yiq.x, 0.925 * yiq.y, 0.925 * yiq.z);
}

@fragment
fn fs_baseband(in: VsOut) -> @location(0) vec4<f32> {
    let n = floor(in.pos.x);
    let l = floor(in.pos.y);
    let ys = max(U.bb0.x, 0.05);
    let is_ = max(U.bb0.y, 0.05);
    let qs = max(U.bb0.z, 0.05);
    let chroma_delay = U.bb0.w;
    var wy = 0.0; var wi = 0.0; var wq = 0.0; var ww = 0.0;
    var y = 0.0; var yw = 0.0; var ci = 0.0; var cq = 0.0;
    for (var j: i32 = -14; j <= 14; j += 1) {
        let fj = f32(j);
        let s = srcAt(n + fj, l);
        let ky = exp(-0.5 * fj * fj / (ys * ys));
        let kw = exp(-0.5 * fj * fj / 6.25);
        y += s.x * ky; wy += ky;
        yw += s.x * kw; ww += kw;
        let sc = srcAt(n + fj - chroma_delay, l);
        let ki = exp(-0.5 * fj * fj / (is_ * is_));
        let kq = exp(-0.5 * fj * fj / (qs * qs));
        ci += sc.y * ki; wi += ki;
        cq += sc.z * kq; wq += kq;
    }
    y /= wy; yw /= ww; ci /= wi; cq /= wq;
    let is_active = n >= ACT0 && n < ACT1 && l >= VACT0 && l < VACT0 + VLINES;
    if U.i0.z == 1 && is_active {
        let li = clamp(i32(l), 0, NL_I - 1);
        let L0 = textureLoad(t_lines, vec2<i32>(li, 0), 0);
        let L1 = textureLoad(t_lines, vec2<i32>(li, 1), 0);
        let tn = L0.y;
        y += U.bb1.x * (y - yw);
        let un = u32(max(n, 0.0));
        let ul = u32(max(l, 0.0));
        let fr = U.u0.x;
        let g = 0.65 * gauss3(un, ul, fr) + 0.35 * gauss3(un + 1u, ul, fr);
        y += U.bb1.y * (1.0 + L1.y) * g * 0.08;
        let cn1 = vnoise(vec2<f32>(n / 22.0, l * 3.1 + f32(fr) * 17.0)) - 0.5;
        let cn2 = vnoise(vec2<f32>(n / 22.0 + 50.0, l * 3.1 + f32(fr) * 17.0)) - 0.5;
        ci += U.bb1.z * cn1 * 0.25;
        cq += U.bb1.z * cn2 * 0.25;
        if tn > 0.001 {
            let streak = gauss3(un / 3u, ul, fr + 31u);
            let bright = step(0.55, rnd3(un / 40u, ul, fr + 3u));
            let noisy = 0.45 + 0.35 * streak + 0.5 * bright;
            y = mix(y, noisy, clamp(tn, 0.0, 1.0) * 0.85);
            ci *= 1.0 - tn;
            cq *= 1.0 - tn;
        }
        let pr = U.bb1.w * 0.02 * (1.0 + 12.0 * tn);
        if rnd3(ul, fr, 77u) < pr {
            let st = rnd3(ul, fr, 78u) * SW;
            let ln = 6.0 + rnd3(ul, fr, 79u) * 90.0;
            if n > st && n < st + ln {
                y = 0.85 + 0.15 * rnd3(un, ul, fr);
                ci = 0.0;
                cq = 0.0;
            }
        }
    }
    return vec4<f32>(y, ci, cq, select(0.0, 1.0, is_active));
}

// ---------------------------------------------------------------- 3. CHANNEL
// t0 = base
fn linePhase(l: f32) -> f32 {
    return fract(0.5 * l + U.p0.y);
}

fn syncAt(n: f32, l: f32) -> f32 {
    let half_line = SW * 0.5;
    if l < 3.0 || (l >= 6.0 && l < 9.0) {
        return select(0.0, 1.0, gl_mod(n, half_line) < 33.0);
    }
    if l < 6.0 {
        return select(0.0, 1.0, gl_mod(n, half_line) < half_line - 67.0);
    }
    return select(0.0, 1.0, n < 67.0);
}

fn yc_at(n_in: f32, l_in: f32) -> vec2<f32> {
    var n = n_in;
    var l = l_in;
    if n < 0.0 { n += SW; l -= 1.0; }
    if n >= SW { n -= SW; l += 1.0; }
    l = gl_mod(l, NL);
    let b = textureLoad(t0, vec2<i32>(wrap_i(i32(n), SW_I), wrap_i(i32(l), NL_I)), 0);
    if syncAt(n, l) > 0.5 { return vec2<f32>(-0.4, 0.0); }
    let ph = TAU * (n * 0.25 + linePhase(l));
    var c = b.y * cos(ph) + b.z * sin(ph);
    if n >= 76.0 && n < 112.0 && l >= 9.0 { c = 0.2 * cos(ph + PI); }
    return vec2<f32>(b.x, c);
}

fn composite_at(n: f32, l: f32) -> f32 {
    let s = yc_at(n, l);
    var y = s.x;
    var c = s.y;
    if abs(U.dec0.x) > 0.0001 {
        let ft = U.dec0.x;
        c *= clamp(1.0 - ft * 1.3, 0.0, 1.6);
        if ft > 0.0 { y += ft * 0.9 * (s.x - yc_at(n - 3.0, l).x); }
        if ft < 0.0 { c += -ft * 0.12 * sin(TAU * (n + l * SW) * (0.92 / 14.318) + U.p0.x * 9.0); }
    }
    return y + c;
}

@fragment
fn fs_channel(in: VsOut) -> @location(0) vec4<f32> {
    let n = floor(in.pos.x);
    let l = floor(in.pos.y);
    let un = u32(max(n, 0.0));
    let ul = u32(max(l, 0.0));
    let fr = U.u0.x;
    let noise = U.ch0.y * gauss3(un, ul, fr * 3u + 1u);
    let inp = U.i1.x;
    var outv: vec4<f32>;
    if inp <= 1 {
        var s = composite_at(n, l);
        if inp == 0 {
            if abs(U.ch0.z) > 0.0005 { s += U.ch0.z * composite_at(n - U.ch0.w, l); }
            s += U.ch1.x * sin(TAU * (U.ch1.y * (n + l * SW + 0.37 * SW * NL * f32(fr & 1023u))));
            let hb = sin(TAU * (l / NL + U.p0.x * 0.06));
            s = s * (1.0 + U.ch1.z * 0.22 * hb) + U.ch1.z * 0.05 * hb;
            if rnd3(ul, fr, 5u) < U.ch1.w * 0.06 {
                let c0 = rnd3(ul, fr, 6u) * SW;
                let w0 = 15.0 + rnd3(ul, fr, 7u) * 160.0;
                if abs(n - c0) < w0 {
                    s += select(-0.6, 1.2, rnd3(un, ul, fr) > 0.5) * rnd3(un + 3u, ul, fr);
                }
            }
            let snow = U.ch0.x;
            let g = 1.0 / (1.0 + snow * 1.2);
            s = s * g + snow * g * gauss3(un, ul, fr * 3u) + (1.0 - g) * 0.30;
        }
        outv = vec4<f32>(s + noise, 0.0, 0.0, 0.0);
    } else if inp == 2 {
        let v = yc_at(n, l);
        let n2 = U.ch0.y * gauss3(un, ul, fr * 3u + 2u);
        outv = vec4<f32>(v.x + noise, v.y + n2, 0.0, 0.0);
    } else {
        let b = textureLoad(t0, vec2<i32>(wrap_i(i32(n), SW_I), wrap_i(i32(l), NL_I)), 0);
        let yv = select(b.x, -0.4, syncAt(n, l) > 0.5);
        let n2 = U.ch0.y * gauss3(un, ul, fr * 3u + 2u);
        let n3 = U.ch0.y * gauss3(un, ul, fr * 3u + 4u);
        outv = vec4<f32>(yv + noise, b.y + n2, b.z + n3, 0.0);
    }
    return outv;
}

// ---------------------------------------------------------------- 4. DECODE
// t0 = chan, t1 = lines
fn chanC(n: f32, l: f32) -> f32 {
    let ni = wrap_i(i32(floor(n)), SW_I);
    let li = clamp(i32(floor(l)), 0, NL_I - 1);
    return textureLoad(t0, vec2<i32>(ni, li), 0).r;
}

fn lumaAt(n: f32, l: f32) -> f32 {
    if U.i1.y == 1 { return 0.5 * (chanC(n, l) + chanC(n, l - 1.0)); }
    return (chanC(n - 4.0, l) + 4.0 * chanC(n - 2.0, l) + 6.0 * chanC(n, l) + 4.0 * chanC(n + 2.0, l) + chanC(n + 4.0, l)) / 16.0;
}

fn chromaAt(n: f32, l: f32) -> f32 {
    if U.i1.y == 1 { return 0.5 * (chanC(n, l) - chanC(n, l - 1.0)); }
    return chanC(n, l) - lumaAt(n, l);
}

@fragment
fn fs_decode(in: VsOut) -> @location(0) vec4<f32> {
    let n = floor(in.pos.x);
    let l = floor(in.pos.y);
    let li = clamp(i32(l), 0, NL_I - 1);
    let L1 = textureLoad(t_lines, vec2<i32>(li, 1), 0);
    var y = 0.0;
    var ci = 0.0;
    var cq = 0.0;
    let inp = U.i1.x;
    if inp == 3 {
        let v = textureLoad(t0, vec2<i32>(wrap_i(i32(n), SW_I), li), 0);
        y = v.x; ci = v.y; cq = v.z;
    } else {
        let sig = max(U.dec1.w, 0.5);
        var wsum = 0.0;
        let phn = L1.x + U.dec1.x;
        for (var j: i32 = -9; j <= 9; j += 1) {
            let fj = f32(j);
            let w = exp(-0.5 * fj * fj / (sig * sig));
            var ch: f32;
            if inp == 2 {
                ch = textureLoad(t0, vec2<i32>(wrap_i(i32(n + fj), SW_I), li), 0).g;
            } else {
                ch = chromaAt(n + fj, l);
            }
            let ph = TAU * ((n + fj) * 0.25 + linePhase(l)) + phn;
            ci += w * 2.0 * ch * cos(ph);
            cq += w * 2.0 * ch * sin(ph);
            wsum += w;
        }
        ci /= wsum;
        cq /= wsum;
        if inp == 2 {
            let a = chanC(n, l);
            let b = 0.5 * (chanC(n - 3.0, l) + chanC(n + 3.0, l));
            y = a + U.dec1.y * (a - b);
        } else {
            let a = lumaAt(n, l);
            let b = 0.5 * (lumaAt(n - 3.0, l) + lumaAt(n + 3.0, l));
            y = a + U.dec1.y * (a - b);
        }
    }
    let yv = (y - 0.075) / 0.925;
    var iq = vec2<f32>(ci, cq) / 0.925 * U.dec0.w * (1.0 - U.dec1.z);
    if inp == 3 {
        let cs = cos(U.dec1.x);
        let sn = sin(U.dec1.x);
        iq = vec2<f32>(cs * iq.x - sn * iq.y, sn * iq.x + cs * iq.y);
    }
    let rgb = yiq2rgb(vec3<f32>(yv, iq.x, iq.y));
    let drive = rgb * U.dec0.y + vec3<f32>(U.dec0.z);
    return vec4<f32>(drive, 1.0);
}

// ---------------------------------------------------------------- 4b. APL (Average Picture Level)
// 1x1 average of active drive area (samples 16x16 grid cleanly in one pass)
@fragment
fn fs_apl(in: VsOut) -> @location(0) vec4<f32> {
    var acc = vec3<f32>(0.0);
    for (var j: i32 = 0; j < 16; j += 1) {
        let v = (VACT0 + (f32(j) + 0.5) * (VLINES / 16.0)) / NL;
        for (var i: i32 = 0; i < 16; i += 1) {
            let u = (ACT0 + (f32(i) + 0.5) * ((ACT1 - ACT0) / 16.0)) / SW;
            acc += max(textureSampleLevel(t0, s_linear, vec2<f32>(u, v), 0.0).rgb, vec3<f32>(0.0));
        }
    }
    return vec4<f32>(acc / 256.0, 1.0);
}

// ---------------------------------------------------------------- 5. BURN-IN
// t0 = prev_burn, t1 = drive
@fragment
fn fs_burn(in: VsOut) -> @location(0) vec4<f32> {
    let uv = in.pos.xy / vec2<f32>(512.0, 384.0);
    let prev = textureLoad(t0, vec2<i32>(in.pos.xy), 0).rgb;
    let f = (uv * 2.0 - 1.0) * vec2<f32>(4.0 / 3.0, 1.0);
    let overscan = U.geom1.w;
    let s = 0.5 * (ACT0 + ACT1) + f.x / (4.0 / 3.0) * 0.5 * (ACT1 - ACT0) * (1.0 - overscan);
    let lf = VACT0 + 0.5 * VLINES + f.y * 0.5 * VLINES * (1.0 - overscan);
    let d = textureSampleLevel(t1, s_repeat_x, vec2<f32>(s / SW, lf / NL), 0.0).rgb;
    let e = pow(clamp(d, vec3<f32>(0.0), vec3<f32>(2.0)), vec3<f32>(U.tube0.y));
    let nb = prev + U.p0.w * e * (vec3<f32>(1.0) - prev);
    return vec4<f32>(clamp(nb, vec3<f32>(0.0), vec3<f32>(1.0)), 1.0);
}

// ---------------------------------------------------------------- 6. BEAM + MASK
// t0 = drive, t1 = lines, t2 = burn, t3 = apl (1x1)
fn rasterPos(f: vec2<f32>, g_breath: f32) -> vec2<f32> {
    var q = f;
    let pin = U.geom0.w;
    q.x *= 1.0 - pin * 0.08 * q.y * q.y;
    q.y *= 1.0 - pin * 0.05 * q.x * q.x;
    q -= vec2<f32>(U.geom0.y, U.geom0.z);
    let ripple = U.geom2.x;
    let rr = sin(TAU * (q.y * 0.5 + U.p0.x * 0.06));
    q.x += ripple * 0.006 * rr;
    let rx = q.x / (4.0 / 3.0 * U.vp0.w * g_breath * U.collapse.x * (1.0 + ripple * 0.004 * rr));
    let ry = q.y / (U.geom0.x * g_breath * U.collapse.y);
    let overscan = U.geom1.w;
    let s = 0.5 * (ACT0 + ACT1) + rx * 0.5 * (ACT1 - ACT0) * (1.0 - overscan);
    let lf = VACT0 + 0.5 * VLINES + ry * 0.5 * VLINES * (1.0 - overscan) + U.geom1.y;
    return vec2<f32>(s, lf);
}

fn gun_current(drive: f32, g: i32, g_abl: f32) -> f32 {
    var cut = U.cutoff.z;
    var em = U.emission.z;
    var im = U.imax.z;
    if g == 0 { cut = U.cutoff.x; em = U.emission.x; im = U.imax.x; }
    else if g == 1 { cut = U.cutoff.y; em = U.emission.y; im = U.imax.y; }
    let v = max(drive * g_abl - cut, 0.0);
    let raw = pow(v, U.tube0.y) * em;
    return im * (1.0 - exp(-raw / im));
}

fn sample_drive_ch(s: f32, ty: f32, g: i32) -> f32 {
    let v = textureSampleLevel(t0, s_repeat_x, vec2<f32>(s / SW, ty), 0.0);
    if g == 0 { return v.r; }
    if g == 1 { return v.g; }
    return v.b;
}

fn gunEnergy(g: i32, f: vec2<f32>, g_abl: f32, g_breath: f32, g_scan_floor: f32) -> f32 {
    let rp = rasterPos(f, g_breath);
    let lf = rp.y - U.geom1.z;
    let k0 = floor(lf - 0.5);
    var foc = U.focus.z;
    if g == 0 { foc = U.focus.x; }
    else if g == 1 { foc = U.focus.y; }
    let sig0 = U.geom2.z * foc;
    let sh = max(U.geom2.w * foc, 0.3);
    let vphase = U.geom1.y;

    // Post-decoder TV Jungle IC RGB Fast-Blanking OSD injection:
    // immune to RF snow & NTSC chroma bleed, yet passes through electron guns,
    // scanlines, convergence, shadow mask, persistence, glass bloom, and HDR.
    var osd_a = 0.0;
    var osd_ch = 0.0;
    if U.scan2.w > 0.5 {
        let ouv = (f / vec2<f32>(4.0 / 3.0, 1.0)) * 0.5 + 0.5;
        if ouv.x >= 0.0 && ouv.x <= 1.0 && ouv.y >= 0.0 && ouv.y <= 1.0 {
            let ov = textureSampleLevel(t_osd, s_linear, ouv, 0.0);
            osd_a = ov.a;
            if g == 0 { osd_ch = ov.r; }
            else if g == 1 { osd_ch = ov.g; }
            else { osd_ch = ov.b; }
            osd_ch *= 1.05;
        }
    }

    var e = 0.0;
    for (var i: i32 = -2; i <= 3; i += 1) {
        let k = k0 + f32(i);
        if abs(vphase) < 2.0 && (k < 0.0 || k >= NL) { continue; }
        let dy = lf - (k + 0.5);
        let kk = gl_mod(k, NL);
        let hoff = textureLoad(t_lines, vec2<i32>(clamp(i32(kk), 0, NL_I - 1), 0), 0).x;
        let s = rp.x + hoff;
        let ty = (kk + 0.5) / NL;
        var cur = 0.0;
        let d0 = mix(sample_drive_ch(s - 2.0 * sh, ty, g), osd_ch, osd_a);
        let d1 = mix(sample_drive_ch(s - sh, ty, g), osd_ch, osd_a);
        let d2 = mix(sample_drive_ch(s, ty, g), osd_ch, osd_a);
        let d3 = mix(sample_drive_ch(s + sh, ty, g), osd_ch, osd_a);
        let d4 = mix(sample_drive_ch(s + 2.0 * sh, ty, g), osd_ch, osd_a);
        cur += 0.0545 * gun_current(d0, g, g_abl);
        cur += 0.2442 * gun_current(d1, g, g_abl);
        cur += 0.4026 * gun_current(d2, g, g_abl);
        cur += 0.2442 * gun_current(d3, g, g_abl);
        cur += 0.0545 * gun_current(d4, g, g_abl);
        var sg = sig0 * (1.0 + U.tube0.x * sqrt(max(cur, 0.0)));
        sg = sqrt(sg * sg + g_scan_floor * g_scan_floor);
        e += cur * exp(-0.5 * dy * dy / (sg * sg)) / (sg * 2.5066283);
    }
    return e;
}

fn boxI(x: f32, duty: f32) -> f32 {
    return floor(x) * duty + min(fract(x), duty);
}

fn stripe(x: f32, start: f32, duty: f32, w_in: f32) -> f32 {
    let w = max(w_in, 1e-3);
    let a = x - start;
    return (boxI(a + 0.5 * w, duty) - boxI(a - 0.5 * w, duty)) / w;
}

struct MaskOut {
    cov: vec3<f32>,
    avg: vec3<f32>,
};

fn maskCoverage(f: vec2<f32>, tube_y: f32, w: f32, flat_k: f32) -> MaskOut {
    let P = U.tube0.z;
    let bm = U.tube0.w;
    let mask_id = U.i1.z;
    if mask_id == 0 || mask_id == 1 {
        let u = f.x / P;
        let duty = (1.0 - bm) / 3.0;
        var cx = vec3<f32>(
            stripe(u, bm / 6.0, duty, w / P),
            stripe(u, 1.0 / 3.0 + bm / 6.0, duty, w / P),
            stripe(u, 2.0 / 3.0 + bm / 6.0, duty, w / P),
        );
        var avg = vec3<f32>(duty);
        if mask_id == 1 {
            let hs_raw = P * 2.2;
            let hs_even_px = max(floor(hs_raw / (2.0 * max(w, 1e-5)) + 0.5) * 2.0, 4.0);
            let hs = mix(hs_raw, hs_even_px * w, flat_k);
            let col = floor(u);
            let v = f.y / hs + 0.5 * gl_mod(col, 2.0);
            let bridge = 0.12 + 0.3 * bm;
            let cy = stripe(v, bridge * 0.5, 1.0 - bridge, w / hs);
            cx *= cy;
            avg *= (1.0 - bridge);
        } else {
            let wt = U.tube1.z;
            let d1 = abs(tube_y - 1.0 / 3.0);
            let d2 = abs(tube_y + 1.0 / 3.0);
            let cov = clamp((min(d1, d2) - 0.5 * wt) / max(w, 1e-4) + 0.5, 0.0, 1.0);
            cx *= mix(1.0, cov, 0.85);
        }
        return MaskOut(cx, avg);
    }
    let d = P / 1.7320508;
    let rs = d * 0.8660254;
    let rad = 0.5 * d * (1.0 - 0.55 * bm);
    let fr = PI * rad * rad / (d * rs);
    let avg = vec3<f32>(fr / 3.0);
    var cov = vec3<f32>(0.0);
    let row0 = floor(f.y / rs + 0.5);
    for (var jr: i32 = -1; jr <= 1; jr += 1) {
        let row = row0 + f32(jr);
        let off = gl_mod(row, 2.0) * 0.5;
        let col0 = floor(f.x / d - off + 0.5);
        for (var ic: i32 = -1; ic <= 1; ic += 1) {
            let col = col0 + f32(ic);
            let c = vec2<f32>((col + off) * d, row * rs);
            let dist = length(f - c);
            let a = clamp((rad - dist) / max(w, 1e-5) + 0.5, 0.0, 1.0);
            let q = col - (row - gl_mod(row, 2.0)) * 0.5;
            let ci = i32(gl_mod(q - row, 3.0) + 0.5);
            if ci == 0 { cov.r += a; }
            else if ci == 1 { cov.g += a; }
            else { cov.b += a; }
        }
    }
    let fade = smoothstep(0.35, 0.9, w / d);
    return MaskOut(mix(cov, avg, fade), avg);
}

@fragment
fn fs_beam(in: VsOut) -> @location(0) vec4<f32> {
    let vpx = U.vp0.xy;
    let view = U.view_curv.xyz;
    let curv = U.view_curv.w;
    let f = faceFromPixel(in.pos.xy, vpx, view, curv);
    let pxw_flat = 2.0 * view.z / vpx.y;
    let pxw = pxw_flat * (1.0 + 3.0 * curv * dot(f, f));
    let sdf = tubeSDF(f, U.vp0.z);
    if sdf > pxw { return vec4<f32>(0.0); }

    let apl3 = max(textureLoad(t3, vec2<i32>(0, 0), 0).rgb, vec3<f32>(0.0));
    let apl = dot(apl3, vec3<f32>(0.3, 0.59, 0.11));
    let g_abl = 1.0 / (1.0 + U.degauss_abl.w * 3.0 * max(apl - 0.3, 0.0));
    let g_breath = 1.0 + U.geom1.x * 0.06 * apl;
    let lines_per_px = pxw * 0.5 * VLINES * (1.0 - U.geom1.w) / max(U.geom0.x * g_breath, 0.2);
    let g_scan_floor = U.tube1.y * 0.42 * lines_per_px;

    let fn_pos = f / vec2<f32>(4.0 / 3.0, 1.0);
    let r2_fn = dot(fn_pos, fn_pos);
    let dg_amp = U.degauss_abl.x;
    let dg_t = U.degauss_abl.y;
    let dg_ph = U.degauss_abl.z;
    var dg_disp = vec2<f32>(0.0);
    var dg_conv = vec2<f32>(0.0);
    if dg_amp > 1e-4 {
        let edge = 0.22 + 0.78 * clamp(r2_fn, 0.0, 1.6);
        let ac_y = f.y * 3.6 + dg_t * 34.0 + dg_ph;
        let ac_x = f.x * 2.1 - dg_t * 27.0 + dg_ph * 0.7;
        let w1 = sin(ac_y) + 0.28 * sin(ac_y * 2.0 - dg_t * 15.0);
        let w2 = cos(ac_y * 0.9 + 0.6) + 0.22 * sin(ac_x);
        let radial = f * (0.14 * edge * w1 + 0.045 * sin(dg_t * 19.0 + dg_ph));
        let swirl = vec2<f32>(-f.y, f.x) * (0.11 * edge * w2);
        let swave = vec2<f32>(
            (0.060 + 0.050 * fn_pos.x * fn_pos.x) * sin(ac_y + fn_pos.x * 0.8),
            (0.036 + 0.032 * fn_pos.y * fn_pos.y) * cos(ac_y * 1.15 - fn_pos.x * 0.7),
        );
        dg_disp = dg_amp * (radial + swirl + swave);
        dg_conv = dg_amp * (0.35 + 0.65 * edge) * vec2<f32>(
            0.028 * sin(ac_y + 0.5) + 0.012 * fn_pos.x * w2,
            0.016 * cos(ac_y - 0.4) + 0.010 * fn_pos.y * w1,
        );
    }

    var E: vec3<f32>;
    if U.collapse.w > 0.5 {
        let hx = 4.0 / 3.0 * U.vp0.w * U.collapse.x;
        let hy = U.geom0.x * U.collapse.y;
        let sp = 0.006;
        let bx = smoothstep(hx + sp, hx - sp, abs(f.x));
        let by = smoothstep(hy + sp, hy - sp, abs(f.y));
        let conc = min(1.0 / max(U.collapse.x * U.collapse.y, 1e-4), 400.0);
        E = vec3<f32>(U.collapse.z * bx * by * min(conc, 60.0));
    } else {
        let fc = f + dg_disp;
        let dyn = f * abs(f) * U.geom2.y * 0.004;
        let fr = fc + U.conv.xy * 0.004 + dyn + dg_conv;
        let fb = fc + U.conv.zw * 0.004 - dyn - dg_conv;
        E = vec3<f32>(
            gunEnergy(0, fr, g_abl, g_breath, g_scan_floor),
            gunEnergy(1, fc, g_abl, g_breath, g_scan_floor),
            gunEnergy(2, fb, g_abl, g_breath, g_scan_floor),
        );
    }

    // Purity: continuous cyclic landing shift across the 3-phosphor triad
    var P = array<f32, 3>(0.0, 0.0, 0.0);
    var Eval = array<f32, 3>(E.x, E.y, E.z);
    var dg_field = 0.0;
    if dg_amp > 1e-4 {
        let edge_p = 0.25 + 0.75 * clamp(r2_fn, 0.0, 1.5);
        let r = sqrt(r2_fn);
        let ac_p = f.y * 3.2 + dg_t * 32.0 + dg_ph;
        dg_field = dg_amp * edge_p * (
            1.35 * sin(r * 4.2 - dg_t * 14.0 + ac_p * 0.65)
            + 0.75 * cos(fn_pos.x * 2.8 + fn_pos.y * 2.2 - dg_t * 24.0 + dg_ph)
        );
    }
    let purity = U.tube1.w;
    let pseed = U.tube2.xy;
    for (var g: i32 = 0; g < 3; g += 1) {
        let fg = f32(g);
        var mag = 0.0;
        if purity > 1e-4 {
            let b1 = vnoise(f * 1.2 + pseed) - 0.5;
            let b2 = vnoise(f * 0.9 + pseed + vec2<f32>(19.0, 31.0) * (fg + 1.0)) - 0.5;
            mag = (b1 * 1.6 + b2 * 1.4) * purity * 2.2;
        }
        let shift = mag + dg_field * (1.0 + 0.22 * (fg - 1.0));
        let u = gl_mod(shift, 3.0);
        let i0 = i32(floor(u)) % 3;
        let i1 = (i0 + 1) % 3;
        let w = smoothstep(0.0, 1.0, fract(u));
        let e = Eval[g];
        P[(g + i0) % 3] += e * (1.0 - w);
        P[(g + i1) % 3] += e * w;
    }
    var Pvec = vec3<f32>(P[0], P[1], P[2]);

    // Phosphor burn-in
    let buv = (f / vec2<f32>(4.0 / 3.0, 1.0)) * 0.5 + 0.5;
    let burn = textureSampleLevel(t2, s_linear, buv, 0.0).rgb;
    Pvec *= vec3<f32>(1.0) - U.tube2.z * burn * 0.85;

    // Anti-moire screen-aligned mask around 1x zoom, blending back to curved on zoom-in
    let low_zoom = 1.0 - smoothstep(5.5, 10.0, U.tube0.z / pxw_flat);
    let flat_k = U.tube1.y * low_zoom;
    let p_grid = vec2<f32>(in.pos.x - 0.5 * vpx.x, in.pos.y - 0.5 * vpx.y) * pxw_flat;
    let f_mask = mix(f, p_grid, flat_k);
    let pxw_mask = mix(pxw, pxw_flat, flat_k);
    let px_per_triad = U.tube0.z / pxw_mask;
    let filter_w = pxw_mask * mix(1.5, 1.0, smoothstep(2.2, 2.95, px_per_triad));
    let mo = maskCoverage(f_mask, f.y, filter_w, flat_k);
    let resolvable = smoothstep(2.0, 2.95, px_per_triad);
    let cov = mix(mo.avg, mo.cov, resolvable);
    let T = mix(vec3<f32>(1.0), cov / max(mo.avg, vec3<f32>(1e-5)), U.tube1.x);
    var out_e = Pvec * T;
    let sat = U.tube2.w;
    out_e = out_e / (vec3<f32>(1.0) + sat * out_e) * (1.0 + sat);
    let edge = smoothstep(pxw, -pxw, sdf);

    let rpg = rasterPos(f, g_breath);
    let lt = gl_mod(floor(rpg.y - U.geom1.z), NL);
    let tp = fract((lt + clamp(rpg.x, 0.0, SW) / SW) / NL);
    return vec4<f32>(max(out_e * edge, vec3<f32>(0.0)), tp);
}

// ---------------------------------------------------------------- 7. TAIL / SCAN / EYE
// fs_tail: t0 = e, t1 = prev_tail
@fragment
fn fs_tail(in: VsOut) -> @location(0) vec4<f32> {
    let p = vec2<i32>(in.pos.xy);
    let e = textureLoad(t0, p, 0).rgb;
    let t = textureLoad(t1, p, 0).rgb * (1.0 - U.flat_reset.w);
    let dec = U.pers0.rgb;
    let fr = U.pers1.rgb;
    return vec4<f32>(t * dec + fr * e * (vec3<f32>(1.0) - dec), 1.0);
}

// fs_scan: t0 = cur_e, t1 = prev_e
fn term0(a: f32, w: f32, tau: f32) -> f32 {
    if a <= w { return 1.0 - exp(-a / tau); }
    return exp(-(a - w) / tau) - exp(-a / tau);
}

fn term1(a1: f32, w: f32, tau: f32) -> f32 {
    return exp(-(a1 - w) / tau) * (1.0 - exp(-w / tau)) / max(1.0 - exp(-1.0 / tau), 1e-6);
}

@fragment
fn fs_scan(in: VsOut) -> @location(0) vec4<f32> {
    let p = vec2<i32>(in.pos.xy);
    let c = textureLoad(t0, p, 0);
    let pe = textureLoad(t1, p, 0).rgb;
    let tp = c.a;
    let phase = U.scan0.w;
    var a0: f32;
    var E0: vec3<f32>;
    if tp <= phase { a0 = phase - tp; E0 = c.rgb; }
    else { a0 = 1.0 + phase - tp; E0 = pe; }
    let a1 = a0 + 1.0;
    let w = clamp(U.scan1.w, 1e-5, 1.0);
    let tf = max(U.scan0.rgb, vec3<f32>(1e-6));
    let ts = max(U.scan1.rgb, vec3<f32>(1e-6));
    let fr = U.pers1.rgb;
    let r_fast = E0.r * term0(a0, w, tf.r) + pe.r * term1(a1, w, tf.r);
    let r_slow = E0.r * term0(a0, w, ts.r) + pe.r * term1(a1, w, ts.r);
    let g_fast = E0.g * term0(a0, w, tf.g) + pe.g * term1(a1, w, tf.g);
    let g_slow = E0.g * term0(a0, w, ts.g) + pe.g * term1(a1, w, ts.g);
    let b_fast = E0.b * term0(a0, w, tf.b) + pe.b * term1(a1, w, tf.b);
    let b_slow = E0.b * term0(a0, w, ts.b) + pe.b * term1(a1, w, ts.b);
    let o = vec3<f32>(
        mix(r_fast, r_slow, fr.r),
        mix(g_fast, g_slow, fr.g),
        mix(b_fast, b_slow, fr.b),
    );
    return vec4<f32>(max(o * (U.scan2.x / w), vec3<f32>(0.0)), 1.0);
}

// fs_eye: t0 = e_or_sc, t1 = tail, t2 = prev_eye
@fragment
fn fs_eye(in: VsOut) -> @location(0) vec4<f32> {
    let p = vec2<i32>(in.pos.xy);
    let e = textureLoad(t0, p, 0).rgb;
    let t = textureLoad(t1, p, 0).rgb;
    let prev = textureLoad(t2, p, 0).rgb;
    let use_tail = U.pers1.w;
    let fr = U.pers1.rgb * use_tail;
    let L = (vec3<f32>(1.0) - fr) * e + use_tail * t;
    return vec4<f32>(mix(prev, L, U.scan2.y), 1.0);
}

// ---------------------------------------------------------------- 8. GLASS BLUR
// fs_down: t0 = eye
@fragment
fn fs_down(in: VsOut) -> @location(0) vec4<f32> {
    let c = in.pos.xy * 4.0;
    let ip = vec2<f32>(1.0) / U.vp0.xy;
    let s = textureSampleLevel(t0, s_linear, (c + vec2<f32>(-1.0, -1.0)) * ip, 0.0).rgb
          + textureSampleLevel(t0, s_linear, (c + vec2<f32>(1.0, -1.0)) * ip, 0.0).rgb
          + textureSampleLevel(t0, s_linear, (c + vec2<f32>(-1.0, 1.0)) * ip, 0.0).rgb
          + textureSampleLevel(t0, s_linear, (c + vec2<f32>(1.0, 1.0)) * ip, 0.0).rgb;
    return vec4<f32>(s * 0.25, 1.0);
}

// fs_blur: t0 = src_q
@fragment
fn fs_blur(in: VsOut) -> @location(0) vec4<f32> {
    let px = U.blur_p.zw;
    let dir = U.blur_p.xy;
    let sigma = U.glass1.z;
    let uv = in.pos.xy / px;
    let st = max(sigma / 3.0, 1.0);
    var acc = vec3<f32>(0.0);
    var ws = 0.0;
    for (var i: i32 = -9; i <= 9; i += 1) {
        let x = f32(i) * st;
        let w = exp(-0.5 * x * x / (sigma * sigma + 0.01));
        acc += w * textureSampleLevel(t0, s_linear, uv + dir * x / px, 0.0).rgb;
        ws += w;
    }
    return vec4<f32>(acc / ws, 1.0);
}

// ---------------------------------------------------------------- 9. COMPOSITE (Linear HDR CRT)
// t0 = eye, t1 = diffuse_q, t2 = halation_q
// Outputs pure linear scene-referred HDR light (1.0 = nominal paper white; phosphors peak 3-5x+).
@fragment
fn fs_composite(in: VsOut) -> @location(0) vec4<f32> {
    let loc = in.pos.xy;
    let vpx = U.vp0.xy;
    let uv = loc / vpx;
    let view = U.view_curv.xyz;
    let curv = U.view_curv.w;
    let f = faceFromPixel(loc, vpx, view, curv);
    let pxw = 2.0 * view.z / vpx.y;
    let sdf = tubeSDF(f, U.vp0.z);

    let phos = textureSampleLevel(t0, s_linear, uv, 0.0).rgb
             + U.glass0.x * textureSampleLevel(t1, s_linear, uv, 0.0).rgb
             + U.glass0.y * textureSampleLevel(t2, s_linear, uv, 0.0).rgb;
    let m_phos = mat3x3<f32>(U.phos0.xyz, U.phos1.xyz, U.phos2.xyz);
    var lin = max(m_phos * phos, vec3<f32>(0.0));
    lin *= U.glass2.rgb;
    let fn_pos = f / vec2<f32>(4.0 / 3.0, 1.0);
    let r2 = dot(fn_pos, fn_pos);
    lin *= 1.0 - U.glass1.x * 0.25 * r2;

    let amb = U.glass0.z;
    let glassArea = smoothstep(0.07 + pxw, 0.07 - pxw, sdf);
    let screenGrey = vec3<f32>(0.30, 0.31, 0.30) * 0.05 * amb * smoothstep(pxw, -pxw, sdf);
    let hl = f - vec2<f32>(-0.75, -0.62);
    let spec = exp(-dot(hl, hl) / 0.18) * 0.06 + exp(-pow(abs(f.y + 0.85), 2.0) / 0.01) * 0.015;
    let refl = vec3<f32>(0.9, 0.95, 1.0) * spec * U.glass0.w * amb * glassArea;
    let ambFloor = min(amb * 2.0, 1.0);
    lin = lin * glassArea + screenGrey + refl + vec3<f32>(0.002, 0.0022, 0.0025) * ambFloor * glassArea;
    var scene_lin = lin * U.glass1.y;
    let room = vec3<f32>(0.0);

    if U.i1.w == 1 && sdf > 0.07 - pxw {
        let d = sdf - 0.07;
        let bw = 0.21;
        let e = 0.002;
        let grad = vec2<f32>(
            tubeSDF(f + vec2<f32>(e, 0.0), U.vp0.z) - tubeSDF(f - vec2<f32>(e, 0.0), U.vp0.z),
            tubeSDF(f + vec2<f32>(0.0, e), U.vp0.z) - tubeSDF(f - vec2<f32>(0.0, e), U.vp0.z),
        );
        let n2 = normalize(grad + vec2<f32>(1e-6));
        let up = dot(n2, vec2<f32>(0.0, -1.0));
        let bevel = 1.0 - smoothstep(0.0, 0.045, d);
        let outer = smoothstep(bw - 0.032, bw, d);
        let lightBevel = 0.5 - 0.5 * up;
        let lightOuter = 0.5 + 0.5 * up;
        var shade = mix(0.58, 1.0 + 0.85 * lightBevel, bevel);
        shade = mix(shade, 0.35 + 1.05 * lightOuter, outer);

        var plastic = vec3<f32>(0.017, 0.016, 0.015) * shade;
        plastic *= 0.93 + 0.14 * hash12(floor(loc * 0.8));

        let chromeMask = smoothstep(0.088, 0.094, d) * (1.0 - smoothstep(0.102, 0.108, d));
        let brush = 0.90 + 0.10 * sin(f.y * 260.0 + f.x * 12.0);
        let chrome = vec3<f32>(0.14, 0.132, 0.118) * brush * (0.65 + 0.65 * lightOuter);

        let woodMask = smoothstep(0.102, 0.110, d);
        let warp = 3.2 * vnoise(vec2<f32>(f.x * 2.4, f.y * 8.0)) + 1.4 * vnoise(vec2<f32>(f.x * 7.0, f.y * 20.0));
        let g1 = 0.5 + 0.5 * sin(f.y * 42.0 + warp);
        let g2 = 0.5 + 0.5 * sin(f.y * 115.0 + warp * 2.3 + f.x * 2.0);
        let grain = g1 * 0.68 + g2 * 0.32;
        var walnut = mix(vec3<f32>(0.022, 0.010, 0.0045), vec3<f32>(0.068, 0.031, 0.012), grain);
        walnut *= shade * (0.94 + 0.12 * vnoise(f * 36.0));

        var cab = mix(plastic, walnut, woodMask);
        cab = mix(cab, chrome, chromeMask);
        // Ambient room lighting scales smoothly down to true dark when Room light -> 0.
        cab *= amb * 2.6;

        // Real CRT phosphor screen spill (independent of Room light!):
        // Sample the glowing picture just inside the nearest glass edge so the inner plastic
        // bevel, brushed metal trim, and satin walnut catch true color-accurate reflections.
        let duv_scale = vec2<f32>(vpx.y / vpx.x, 1.0) / max(2.0 * view.z * (1.0 + curv * r2), 0.001);
        let uv_near = clamp(uv - n2 * (d * 1.15 + 0.045) * duv_scale, vec2<f32>(0.005), vec2<f32>(0.995));
        let uv_far = clamp(uv - n2 * (d * 0.75 + 0.085) * duv_scale, vec2<f32>(0.005), vec2<f32>(0.995));
        let spill_tight = max(m_phos * textureSampleLevel(t1, s_linear, uv_near, 0.0).rgb, vec3<f32>(0.0)) * U.glass2.rgb;
        let spill_wide = max(m_phos * textureSampleLevel(t2, s_linear, uv_far, 0.0).rgb, vec3<f32>(0.0)) * U.glass2.rgb;

        // Glossy mirror-like reflection along the angled inner bezel lip
        let bevel_refl = spill_tight * bevel * (0.05 + 0.15 * (0.45 + 0.55 * lightBevel));
        // Soft matte bounce across the dark plastic surround
        let plastic_spill = spill_wide * vec3<f32>(0.032, 0.030, 0.028) * (1.0 - woodMask) * exp(-d * 9.0);
        // Brushed aluminum / chrome strip specular catchlight
        let chrome_spill = spill_wide * vec3<f32>(0.15, 0.142, 0.128) * brush * chromeMask * (0.65 + 0.55 * lightOuter);
        // Warm diffuse bounce + satin lacquer sheen on the walnut frame (fades cleanly toward outer edge)
        let wood_falloff = exp(-max(d - 0.095, 0.0) * 7.5);
        let wood_spill = spill_wide * (walnut * 1.65 + vec3<f32>(0.011, 0.008, 0.005)) * woodMask * wood_falloff;

        cab += bevel_refl + plastic_spill + chrome_spill + wood_spill;
        let bz_lin = cab * U.glass1.y;
        let a = smoothstep(0.07 - pxw, 0.07 + pxw, sdf);
        let outside = smoothstep(bw - pxw, bw + pxw, d);
        scene_lin = mix(scene_lin, bz_lin, a);
        scene_lin = mix(scene_lin, room, outside);
    } else if U.i1.w == 0 && sdf > 0.07 {
        scene_lin = room;
    }
    return vec4<f32>(max(scene_lin, vec3<f32>(0.0)), 1.0);
}

// ---------------------------------------------------------------- 10. PRESENT (Swapchain HDR / SDR + UI)
// t0 = linear HDR CRT texture (crt_rect.zw), t1 = premultiplied sRGB egui UI overlay (full window)
fn srgb_to_linear(s: vec3<f32>) -> vec3<f32> {
    let low = s / 12.92;
    let hi = pow(max((s + 0.055) / 1.055, vec3<f32>(0.0)), vec3<f32>(2.4));
    return select(hi, low, s < vec3<f32>(0.04045));
}

fn linear_to_srgb(l: vec3<f32>) -> vec3<f32> {
    let c = clamp(l, vec3<f32>(0.0), vec3<f32>(1.0));
    let low = c * 12.92;
    let hi = 1.055 * pow(max(c, vec3<f32>(0.0)), vec3<f32>(1.0 / 2.4)) - 0.055;
    return select(hi, low, c < vec3<f32>(0.0031308));
}

fn tonemap_sdr(x: vec3<f32>) -> vec3<f32> {
    let hi = 0.8 + 0.2 * (vec3<f32>(1.0) - exp(-(x - 0.8) / 0.2));
    return select(x, hi, x >= vec3<f32>(0.8));
}

fn hdr_rolloff(nits: vec3<f32>, peak: f32) -> vec3<f32> {
    let knee = peak * 0.82;
    let span = max(peak - knee, 1.0);
    let hi = knee + span * (vec3<f32>(1.0) - exp(-(nits - knee) / span));
    return select(nits, hi, nits >= vec3<f32>(knee));
}

fn rec709_to_rec2020(c: vec3<f32>) -> vec3<f32> {
    let m = mat3x3<f32>(
        vec3<f32>(0.627404, 0.069097, 0.016391),
        vec3<f32>(0.329283, 0.919540, 0.088013),
        vec3<f32>(0.043313, 0.011362, 0.895595),
    );
    return max(m * c, vec3<f32>(0.0));
}

fn linear_nits_to_pq(nits: vec3<f32>) -> vec3<f32> {
    let m1 = 2610.0 / 16384.0;
    let m2 = 2523.0 / 4096.0 * 128.0;
    let c1 = 3424.0 / 4096.0;
    let c2 = 2413.0 / 4096.0 * 32.0;
    let c3 = 2392.0 / 4096.0 * 32.0;
    let y = pow(clamp(nits / 10000.0, vec3<f32>(0.0), vec3<f32>(1.0)), vec3<f32>(m1));
    return pow(max((c1 + c2 * y) / (1.0 + c3 * y), vec3<f32>(0.0)), vec3<f32>(m2));
}

@fragment
fn fs_present(in: VsOut) -> @location(0) vec4<f32> {
    let pos = in.pos.xy;
    let r = U.crt_rect;
    var crt_lin = vec3<f32>(0.0);
    if pos.x >= r.x && pos.y >= r.y && pos.x < r.x + r.z && pos.y < r.y + r.w {
        let crt_px = vec2<i32>(pos - r.xy);
        crt_lin = textureLoad(t0, crt_px, 0).rgb;
    }

    // Premultiplied sRGB egui UI overlay
    let ui_px = vec2<i32>(pos);
    let ui_raw = textureLoad(t1, ui_px, 0);
    let ui_a = clamp(ui_raw.a, 0.0, 1.0);
    // Un-premultiply before converting sRGB -> linear, then re-apply alpha
    let ui_srgb = select(vec3<f32>(0.0), clamp(ui_raw.rgb / max(ui_a, 1e-4), vec3<f32>(0.0), vec3<f32>(1.0)), ui_a > 1e-4);
    let ui_lin = srgb_to_linear(ui_srgb) * ui_a;

    let mode = i32(U.hdr0.x + 0.5);
    let paper = U.hdr0.y;
    let peak = max(U.hdr0.z, 100.0);
    let ui_nits = U.hdr0.w;

    if mode == 2 {
        // Native HDR scRGB (Rgba16Float, ExtendedSrgbLinear): 1.0 = 80 nits (Windows DX12 / Linux Vulkan)
        let crt_nits = hdr_rolloff(crt_lin * paper, peak);
        let out_nits = crt_nits * (1.0 - ui_a) + ui_lin * ui_nits;
        return vec4<f32>(out_nits / 80.0, 1.0);
    }
    if mode == 3 {
        // Native HDR Metal EDR (Rgba16Float, ExtendedSrgbLinear on macOS): 1.0 = SDR white
        let headroom = max(peak / max(paper, 1.0), 1.0);
        let crt_edr = hdr_rolloff(crt_lin, headroom);
        let out_edr = crt_edr * (1.0 - ui_a) + ui_lin;
        return vec4<f32>(out_edr, 1.0);
    }
    if mode == 4 {
        // Native HDR10 PQ (Rgb10a2Unorm, Bt2100Pq): 10-bit ST.2084 PQ
        let crt_nits = hdr_rolloff(crt_lin * paper, peak);
        let out_nits = crt_nits * (1.0 - ui_a) + ui_lin * ui_nits;
        let pq = linear_nits_to_pq(rec709_to_rec2020(out_nits));
        return vec4<f32>(pq, 1.0);
    }

    // SDR output (mode 0 = *Unorm gamma-encoded, mode 1 = *Srgb linear-encoded)
    let crt_sdr = pow(clamp(tonemap_sdr(crt_lin), vec3<f32>(0.0), vec3<f32>(1.0)), vec3<f32>(1.0 / 2.2));
    var col = crt_sdr * (1.0 - ui_a) + ui_raw.rgb;
    let dn = rnd3(u32(pos.x), u32(pos.y), U.u0.x) - 0.5;
    col += (dn / 255.0) * step(1.0 / 512.0, max(col.r, max(col.g, col.b)));
    col = clamp(col, vec3<f32>(0.0), vec3<f32>(1.0));
    if mode == 1 {
        return vec4<f32>(srgb_to_linear(col), 1.0);
    }
    return vec4<f32>(col, 1.0);
}
