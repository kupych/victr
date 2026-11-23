//! GLSL sources for the signal chain and the picture tube.
//!
//! Signal textures are "one field": 910 samples per line (4x the NTSC colour
//! subcarrier, 14.318 MHz) by 263 lines, including blanking and sync.

pub const VERTEX: &str = r#"
out vec2 v_uv;
void main() {
    vec2 p = vec2(float((gl_VertexID << 1) & 2), float(gl_VertexID & 2));
    v_uv = p;
    gl_Position = vec4(p * 2.0 - 1.0, 0.0, 1.0);
}
"#;

pub const COMMON: &str = r#"
#define SW 910.0
#define NL 263.0
#define ACT0 134.0
#define ACT1 887.0
#define VACT0 21.0
#define VLINES 240.0
#define PI 3.14159265
#define TAU 6.28318531
in vec2 v_uv;
out vec4 o_col;

uint hashu(uint x) {
    x ^= x >> 16; x *= 0x7feb352du;
    x ^= x >> 15; x *= 0x846ca68bu;
    x ^= x >> 16;
    return x;
}
float rnd3(uint a, uint b, uint c) {
    return float(hashu(a ^ hashu(b + 0x9e3779b9u ^ hashu(c + 0x85ebca6bu)))) * (1.0 / 4294967296.0);
}
float gauss3(uint a, uint b, uint c) {
    float u1 = max(rnd3(a, b, c), 1e-7);
    float u2 = rnd3(a + 7919u, b, c + 104729u);
    return sqrt(-2.0 * log(u1)) * cos(TAU * u2);
}
float hash12(vec2 p) {
    vec3 p3 = fract(vec3(p.xyx) * 0.1031);
    p3 += dot(p3, p3.yzx + 33.33);
    return fract((p3.x + p3.y) * p3.z);
}
float vnoise(vec2 p) {
    vec2 i = floor(p);
    vec2 f = fract(p);
    vec2 u = f * f * (3.0 - 2.0 * f);
    float a = hash12(i);
    float b = hash12(i + vec2(1.0, 0.0));
    float c = hash12(i + vec2(0.0, 1.0));
    float d = hash12(i + vec2(1.0, 1.0));
    return mix(mix(a, b, u.x), mix(c, d, u.x), u.y);
}
vec3 rgb2yiq(vec3 c) {
    return vec3(dot(c, vec3(0.299, 0.587, 0.114)),
                dot(c, vec3(0.5959, -0.2746, -0.3213)),
                dot(c, vec3(0.2115, -0.5227, 0.3112)));
}
vec3 yiq2rgb(vec3 y) {
    return vec3(y.x + 0.9563 * y.y + 0.6210 * y.z,
                y.x - 0.2721 * y.y - 0.6474 * y.z,
                y.x - 1.1070 * y.y + 1.7046 * y.z);
}
// Pixel of the view -> position on the faceplate (units: half the picture height).
vec2 faceFromPixel(vec2 fragxy, vec2 vpx, vec3 view, float curv) {
    vec2 ndc = fragxy / vpx * 2.0 - 1.0;
    vec2 p = vec2(ndc.x * vpx.x / vpx.y, -ndc.y) * view.z + view.xy;
    float r2 = dot(p, p);
    return p * (1.0 + curv * r2);
}
float tubeSDF(vec2 f, float corner) {
    vec2 b = vec2(4.0 / 3.0, 1.0);
    vec2 q = abs(f) - b + corner;
    return length(max(q, 0.0)) + min(max(q.x, q.y), 0.0) - corner;
}
"#;

// ---------------------------------------------------------------- source
pub const SOURCE: &str = r#"
uniform int u_pattern;
uniform float u_time;
uniform vec2 u_size;
uniform sampler2D u_img;
uniform float u_img_aspect;
uniform int u_has_img;
uniform vec3 u_flat;

vec3 bars(vec2 uv) {
    int i = int(clamp(floor(uv.x * 7.0), 0.0, 6.0));
    vec3 top[7] = vec3[7](vec3(.75), vec3(.75, .75, 0.), vec3(0., .75, .75), vec3(0., .75, 0.),
                          vec3(.75, 0., .75), vec3(.75, 0., 0.), vec3(0., 0., .75));
    vec3 mid[7] = vec3[7](vec3(0., 0., .75), vec3(0.), vec3(.75, 0., .75), vec3(0.),
                          vec3(0., .75, .75), vec3(0.), vec3(.75));
    if (uv.y < 0.67) return top[i];
    if (uv.y < 0.75) return mid[i];
    float x = uv.x * 28.0;
    if (x < 5.0) return vec3(0.0, 0.16, 0.30);
    if (x < 10.0) return vec3(1.0);
    if (x < 15.0) return vec3(0.20, 0.0, 0.42);
    if (x < 20.0) return vec3(0.0);
    if (x < 21.33) return vec3(-0.04);
    if (x < 22.67) return vec3(0.0);
    if (x < 24.0) return vec3(0.04);
    return vec3(0.0);
}

vec3 crosshatch(vec2 uv) {
    vec2 px = floor(uv * u_size);
    vec2 cell = u_size / vec2(16.0, 12.0);
    vec2 m = mod(px, cell);
    vec3 c = vec3(0.0);
    if (m.x < 1.0 || m.y < 1.0 || px.x >= u_size.x - 1.0 || px.y >= u_size.y - 1.0) c = vec3(1.0);
    vec2 cc = abs(m - cell * 0.5);
    if (cc.x < 1.0 && cc.y < 1.0) c = vec3(1.0);
    vec2 p = (uv - 0.5) * vec2(4.0 / 3.0, 1.0);
    float r = length(p);
    float pxs = 1.0 / u_size.y;
    if (abs(r - 0.42) < pxs * 0.8) c = vec3(1.0);
    return c;
}

vec3 chart(vec2 uv) {
    vec2 p = (uv - 0.5) * vec2(4.0 / 3.0, 1.0);
    vec2 g = abs(fract(p * 8.0 + 0.5) - 0.5);
    vec3 c = vec3(0.42);
    if (min(g.x, g.y) < 0.035) c = vec3(0.85);
    // corner patches
    if (abs(p.x) > 0.5 && abs(p.y) > 0.3) {
        if (p.x < 0.0 && p.y < 0.0) c = vec3(0.8, 0.05, 0.05);
        if (p.x > 0.0 && p.y < 0.0) c = vec3(0.05, 0.8, 0.05);
        if (p.x < 0.0 && p.y > 0.0) c = vec3(0.05, 0.05, 0.8);
        if (p.x > 0.0 && p.y > 0.0) c = vec3(0.9);
    }
    float r = length(p);
    if (r < 0.46) {
        float y = p.y;
        float x = (p.x + 0.46) / 0.92;
        if (y < -0.30) {
            int i = int(clamp(floor(x * 6.0), 0.0, 5.0));
            vec3 hues[6] = vec3[6](vec3(.75, .75, 0.), vec3(0., .75, .75), vec3(0., .75, 0.),
                                   vec3(.75, 0., .75), vec3(.75, 0., 0.), vec3(0., 0., .75));
            c = hues[i];
        } else if (y < -0.15) {
            c = vec3(floor(x * 8.0) / 7.0);
        } else if (y < 0.15) {
            // multiburst: 0.5, 1, 2, 3, 3.58, 4.2 MHz (cycles per active line = MHz * 52.6)
            int i = int(clamp(floor(x * 6.0), 0.0, 5.0));
            float mhz[6] = float[6](0.5, 1.0, 2.0, 3.0, 3.58, 4.2);
            float u = (p.x * 0.75 + 0.5);
            c = vec3(0.5 + 0.4 * sin(TAU * u * mhz[i] * 52.6));
        } else if (y < 0.30) {
            c = vec3(0.0);
            if (abs(p.x) < 0.06) c = vec3(1.0);
            if (abs(p.x - 0.25) < 0.004 || abs(p.x + 0.25) < 0.004) c = vec3(1.0);
        } else {
            c = mix(vec3(0.85, 0.55, 0.42), vec3(0.35, 0.18, 0.12), x);
        }
        if (abs(r - 0.455) < 0.006) c = vec3(1.0);
    }
    return c;
}

vec3 demo(vec2 uv, float t) {
    vec2 p = (uv - 0.5) * vec2(4.0 / 3.0, 1.0);
    float hz = 0.12;
    vec3 c;
    if (p.y < hz) {
        float k = clamp((p.y + 0.5) / (hz + 0.5), 0.0, 1.0);
        c = mix(vec3(0.05, 0.02, 0.18), vec3(0.95, 0.35, 0.45), k * k);
        vec2 st = floor(uv * u_size * 0.5);
        float s = hash12(st);
        if (s > 0.992 && k < 0.6) c += vec3(0.8) * (0.6 + 0.4 * sin(t * 3.0 + s * 100.0));
        vec2 sc = p - vec2(0.0, -0.02);
        float sr = length(sc);
        if (sr < 0.2) {
            vec3 sun = mix(vec3(1.0, 0.92, 0.3), vec3(1.0, 0.25, 0.55), clamp((sc.y + 0.2) / 0.4, 0.0, 1.0));
            bool gap = sc.y > 0.0 && fract(sc.y * 22.0 - t * 0.6) < (sc.y * 3.0);
            if (!gap) c = sun;
        }
        float h = 0.05 + 0.035 * sin(p.x * 7.0 + 1.0) + 0.02 * sin(p.x * 19.0 + 2.3);
        if (p.y > hz - h) c = vec3(0.16, 0.04, 0.28);
    } else {
        float dz = (p.y - hz);
        float z = 0.18 / max(dz, 1e-3);
        float gx = p.x * z;
        float gz = z + t * 2.0;
        float lw = 0.04 * z;
        float lx = abs(fract(gx * 1.5) - 0.5);
        float lz = abs(fract(gz * 0.75) - 0.5);
        c = vec3(0.03, 0.0, 0.10);
        float glow = smoothstep(0.08 + lw * 0.1, 0.0, min(lx, lz));
        c = mix(c, vec3(1.0, 0.2, 0.85), glow);
    }
    // bouncing ball
    vec2 bc = vec2(0.45 * sin(t * 0.9), 0.30 - abs(sin(t * 2.1)) * 0.42);
    vec2 d = p - bc;
    float br = 0.075;
    if (length(d) < br) {
        vec2 q = d / br;
        float zz = sqrt(max(1.0 - dot(q, q), 0.0));
        vec2 tex = vec2(atan(q.x, zz) + t * 2.0, asin(clamp(q.y, -1.0, 1.0)));
        bool ch = (mod(floor(tex.x * 2.5) + floor(tex.y * 2.5), 2.0) < 1.0);
        vec3 bcol = ch ? vec3(1.0) : vec3(0.9, 0.05, 0.1);
        float lit = 0.35 + 0.65 * clamp(dot(vec3(q, zz), normalize(vec3(-0.5, -0.6, 0.7))), 0.0, 1.0);
        c = bcol * lit;
    }
    // fast sprite to show phosphor trails
    float sx = fract(t * 0.35) * 1.8 - 0.9;
    vec2 sp = abs(p - vec2(sx, -0.40));
    if (max(sp.x, sp.y) < 0.03) c = vec3(0.2, 1.0, 0.9);
    return c;
}

vec3 motion(vec2 uv, float t) {
    vec2 p = (uv - 0.5) * vec2(4.0 / 3.0, 1.0);
    vec3 c = vec3(0.0);
    float bx = sin(t * 2.0) * 0.6;
    if (abs(p.x - bx) < 0.02 && p.y < -0.1) c = vec3(1.0);
    float a = t * 1.3;
    vec2 dir = vec2(cos(a), sin(a));
    float along = dot(p - vec2(0.0, 0.18), dir);
    float across = abs(dot(p - vec2(0.0, 0.18), vec2(-dir.y, dir.x)));
    if (along > 0.0 && along < 0.25 && across < 0.006) c = vec3(1.0);
    for (int i = 0; i < 3; i++) {
        float fi = float(i);
        float x = fract(t * (0.2 + 0.15 * fi)) * 2.4 - 1.2;
        vec2 q = abs(p - vec2(x, 0.38 - 0.0 * fi) - vec2(0.0, -0.0));
        vec3 col = i == 0 ? vec3(1., 0., 0.) : (i == 1 ? vec3(0., 1., 0.) : vec3(0., 0., 1.));
        if (abs(p.y - (-0.05 + 0.1 * fi)) < 0.035 && abs(p.x - x) < 0.035) c = col;
    }
    return c;
}

vec3 ramp(vec2 uv) {
    if (uv.y < 0.4) return vec3(uv.x);
    if (uv.y < 0.6) {
        float x = uv.x * 10.0;
        if (x > 3.0 && x < 4.0) return vec3(-0.04);
        if (x > 4.5 && x < 5.5) return vec3(0.04);
        if (x > 6.0 && x < 7.0) return vec3(0.08);
        return vec3(0.0);
    }
    return vec3(floor(uv.x * 11.0) / 10.0);
}

vec3 image(vec2 uv) {
    if (u_has_img == 0) return vec3(0.1);
    float ta = 4.0 / 3.0;
    vec2 q = uv;
    if (u_img_aspect > ta) {
        float h = ta / u_img_aspect;
        q.y = (uv.y - 0.5) / h + 0.5;
    } else {
        float w = u_img_aspect / ta;
        q.x = (uv.x - 0.5) / w + 0.5;
    }
    if (q.x < 0.0 || q.x > 1.0 || q.y < 0.0 || q.y > 1.0) return vec3(0.0);
    return texture(u_img, q).rgb;
}

void main() {
    vec2 uv = gl_FragCoord.xy / u_size; // y grows downward (row 0 = top line)
    vec3 c;
    if (u_pattern == 0) c = bars(uv);
    else if (u_pattern == 1) c = crosshatch(uv);
    else if (u_pattern == 2) c = chart(uv);
    else if (u_pattern == 3) c = demo(uv, u_time);
    else if (u_pattern == 4) c = motion(uv, u_time);
    else if (u_pattern == 5) c = ramp(uv);
    else if (u_pattern == 6) c = u_flat;
    else c = image(uv);
    o_col = vec4(c, 1.0);
}
"#;

// ---------------------------------------------------------------- baseband (YIQ + VCR)
pub const BASEBAND: &str = r#"
uniform sampler2D u_src;
uniform vec2 u_src_size;
uniform int u_field;
uniform int u_interlaced;
uniform sampler2D u_lines;
uniform uint u_frame;
uniform int u_vcr;
uniform float u_luma_sigma;
uniform float u_i_sigma;
uniform float u_q_sigma;
uniform float u_chroma_delay;
uniform float u_sharp;
uniform float u_tape_noise;
uniform float u_chroma_noise;
uniform float u_dropout;

vec3 srcAt(float n, float l) {
    if (n < ACT0 || n >= ACT1) return vec3(0.0);
    float lv = l - VACT0;
    if (lv < 0.0 || lv >= VLINES) return vec3(0.0);
    float u = (n - ACT0) / (ACT1 - ACT0);
    float row = floor((lv + (u_interlaced == 1 ? float(u_field) * 0.5 : 0.0)) * u_src_size.y / VLINES);
    ivec2 px = ivec2(clamp(floor(u * u_src_size.x), 0.0, u_src_size.x - 1.0), clamp(row, 0.0, u_src_size.y - 1.0));
    vec3 yiq = rgb2yiq(texelFetch(u_src, px, 0).rgb);
    // NTSC setup: black at 7.5 IRE, white at 100 IRE.
    return vec3(0.075 + 0.925 * yiq.x, 0.925 * yiq.yz);
}

void main() {
    float n = floor(gl_FragCoord.x);
    float l = floor(gl_FragCoord.y);
    float ys = max(u_luma_sigma, 0.05);
    float is = max(u_i_sigma, 0.05);
    float qs = max(u_q_sigma, 0.05);
    float wy = 0.0, wi = 0.0, wq = 0.0, ww = 0.0;
    float y = 0.0, yw = 0.0, ci = 0.0, cq = 0.0;
    for (int j = -14; j <= 14; j++) {
        float fj = float(j);
        vec3 s = srcAt(n + fj, l);
        float ky = exp(-0.5 * fj * fj / (ys * ys));
        float kw = exp(-0.5 * fj * fj / 6.25);
        y += s.x * ky; wy += ky;
        yw += s.x * kw; ww += kw;
        vec3 sc = srcAt(n + fj - u_chroma_delay, l);
        float ki = exp(-0.5 * fj * fj / (is * is));
        float kq = exp(-0.5 * fj * fj / (qs * qs));
        ci += sc.y * ki; wi += ki;
        cq += sc.z * kq; wq += kq;
    }
    y /= wy; yw /= ww; ci /= wi; cq /= wq;
    bool active = n >= ACT0 && n < ACT1 && l >= VACT0 && l < VACT0 + VLINES;
    if (u_vcr == 1 && active) {
        vec4 L0 = texelFetch(u_lines, ivec2(int(l), 0), 0);
        vec4 L1 = texelFetch(u_lines, ivec2(int(l), 1), 0);
        float tn = L0.y;
        // VCR edge enhancement (luma peaking) - the source of the classic halos.
        y += u_sharp * (y - yw);
        uint un = uint(n), ul = uint(l);
        float g = 0.65 * gauss3(un, ul, u_frame) + 0.35 * gauss3(un + 1u, ul, u_frame);
        y += u_tape_noise * (1.0 + L1.y) * g * 0.08;
        // low-frequency chroma noise streaks (colour-under recording)
        float cn1 = vnoise(vec2(n / 22.0, l * 3.1 + float(u_frame) * 17.0)) - 0.5;
        float cn2 = vnoise(vec2(n / 22.0 + 50.0, l * 3.1 + float(u_frame) * 17.0)) - 0.5;
        ci += u_chroma_noise * cn1 * 0.25;
        cq += u_chroma_noise * cn2 * 0.25;
        // tracking noise band
        if (tn > 0.001) {
            float streak = gauss3(un / 3u, ul, u_frame + 31u);
            float bright = step(0.55, rnd3(un / 40u, ul, u_frame + 3u));
            float noisy = 0.45 + 0.35 * streak + 0.5 * bright;
            y = mix(y, noisy, clamp(tn, 0.0, 1.0) * 0.85);
            ci *= 1.0 - tn; cq *= 1.0 - tn;
        }
        // dropouts: oxide missing on the tape -> white streaks
        float pr = u_dropout * 0.02 * (1.0 + 12.0 * tn);
        if (rnd3(ul, u_frame, 77u) < pr) {
            float st = rnd3(ul, u_frame, 78u) * SW;
            float ln = 6.0 + rnd3(ul, u_frame, 79u) * 90.0;
            if (n > st && n < st + ln) {
                y = 0.85 + 0.15 * rnd3(un, ul, u_frame);
                ci = 0.0; cq = 0.0;
            }
        }
    }
    o_col = vec4(y, ci, cq, active ? 1.0 : 0.0);
}
"#;

// ---------------------------------------------------------------- channel (modulation + RF)
pub const CHANNEL: &str = r#"
uniform sampler2D u_base;
uniform int u_input;
uniform float u_field_phase;
uniform uint u_frame;
uniform float u_time;
uniform float u_snow;
uniform float u_noise;
uniform float u_ghost_amp;
uniform float u_ghost_delay;
uniform float u_interf_amp;
uniform float u_interf_freq;
uniform float u_hum;
uniform float u_impulse;
uniform float u_fine_tune;

float linePhase(float l) { return fract(0.5 * l + u_field_phase); }

float syncAt(float n, float l) {
    float half_line = SW * 0.5;
    if (l < 3.0 || (l >= 6.0 && l < 9.0)) {
        return mod(n, half_line) < 33.0 ? 1.0 : 0.0;      // equalizing pulses
    }
    if (l < 6.0) {
        return mod(n, half_line) < half_line - 67.0 ? 1.0 : 0.0; // broad vertical sync
    }
    return n < 67.0 ? 1.0 : 0.0;
}

// returns (luma incl. sync, modulated chroma)
vec2 yc(float n, float l) {
    if (n < 0.0) { n += SW; l -= 1.0; }
    if (n >= SW) { n -= SW; l += 1.0; }
    l = mod(l, NL);
    vec4 b = texelFetch(u_base, ivec2(int(n), int(l)), 0);
    if (syncAt(n, l) > 0.5) return vec2(-0.4, 0.0);
    float ph = TAU * (n * 0.25 + linePhase(l));
    float c = b.y * cos(ph) + b.z * sin(ph);
    if (n >= 76.0 && n < 112.0 && l >= 9.0) c = 0.2 * cos(ph + PI);  // colour burst
    return vec2(b.x, c);
}

float composite(float n, float l) {
    vec2 s = yc(n, l);
    float y = s.x;
    float c = s.y;
    if (u_input == 0) {
        // Mistuning: tuned high -> chroma falls on the IF slope and luma rings;
        // tuned low -> chroma boosted and a 920 kHz sound/chroma beat appears.
        float ft = u_fine_tune;
        c *= clamp(1.0 - ft * 1.3, 0.0, 1.6);
        if (ft > 0.0) y += ft * 0.9 * (s.x - yc(n - 3.0, l).x);
        if (ft < 0.0) c += -ft * 0.12 * sin(TAU * (n + l * SW) * (0.92 / 14.318) + u_time * 9.0);
    }
    return y + c;
}

void main() {
    float n = floor(gl_FragCoord.x);
    float l = floor(gl_FragCoord.y);
    uint un = uint(n), ul = uint(l);
    float noise = u_noise * gauss3(un, ul, u_frame * 3u + 1u);
    vec4 outv;
    if (u_input <= 1) {
        float s = composite(n, l);
        if (u_input == 0) {
            if (abs(u_ghost_amp) > 0.0005) s += u_ghost_amp * composite(n - u_ghost_delay, l);
            s += u_interf_amp * sin(TAU * (u_interf_freq * (n + l * SW + 0.37 * SW * NL * float(u_frame & 1023u))));
            float hb = sin(TAU * (l / NL + u_time * 0.06));
            s = s * (1.0 + u_hum * 0.22 * hb) + u_hum * 0.05 * hb;
            if (rnd3(ul, u_frame, 5u) < u_impulse * 0.06) {
                float c0 = rnd3(ul, u_frame, 6u) * SW;
                float w0 = 15.0 + rnd3(ul, u_frame, 7u) * 160.0;
                if (abs(n - c0) < w0) s += (rnd3(un, ul, u_frame) > 0.5 ? 1.2 : -0.6) * rnd3(un + 3u, ul, u_frame);
            }
            // tuner AGC: with a weak carrier the gain rises until noise fills the range
            float g = 1.0 / (1.0 + u_snow * 1.2);
            s = s * g + u_snow * g * gauss3(un, ul, u_frame * 3u) + (1.0 - g) * 0.30;
        }
        outv = vec4(s + noise, 0.0, 0.0, 0.0);
    } else if (u_input == 2) {
        vec2 v = yc(n, l);
        float n2 = u_noise * gauss3(un, ul, u_frame * 3u + 2u);
        outv = vec4(v.x + noise, v.y + n2, 0.0, 0.0);
    } else {
        vec4 b = texelFetch(u_base, ivec2(int(n), int(l)), 0);
        float yv = syncAt(n, l) > 0.5 ? -0.4 : b.x;
        float n2 = u_noise * gauss3(un, ul, u_frame * 3u + 2u);
        float n3 = u_noise * gauss3(un, ul, u_frame * 3u + 4u);
        outv = vec4(yv + noise, b.y + n2, b.z + n3, 0.0);
    }
    o_col = outv;
}
"#;

// ---------------------------------------------------------------- decode (the receiver)
pub const DECODE: &str = r#"
uniform sampler2D u_chan;
uniform sampler2D u_lines;
uniform int u_input;
uniform int u_comb;
uniform float u_field_phase;
uniform float u_contrast;
uniform float u_brightness;
uniform float u_color;
uniform float u_tint;
uniform float u_sharp;
uniform float u_killer;
uniform float u_chroma_sigma;

float linePhase(float l) { return fract(0.5 * l + u_field_phase); }
float C(float n, float l) {
    n = mod(n, SW);
    return texelFetch(u_chan, ivec2(int(n), int(clamp(l, 0.0, NL - 1.0))), 0).r;
}
float lumaAt(float n, float l) {
    if (u_comb == 1) return 0.5 * (C(n, l) + C(n, l - 1.0));
    // double notch at the subcarrier (fs/4): [1 0 4 0 6 0 4 0 1] / 16
    return (C(n - 4.0, l) + 4.0 * C(n - 2.0, l) + 6.0 * C(n, l) + 4.0 * C(n + 2.0, l) + C(n + 4.0, l)) / 16.0;
}
float chromaAt(float n, float l) {
    if (u_comb == 1) return 0.5 * (C(n, l) - C(n, l - 1.0));
    return C(n, l) - lumaAt(n, l);
}

void main() {
    float n = floor(gl_FragCoord.x);
    float l = floor(gl_FragCoord.y);
    vec4 L1 = texelFetch(u_lines, ivec2(int(l), 1), 0);
    float y; float ci = 0.0; float cq = 0.0;
    if (u_input == 3) {
        vec4 v = texelFetch(u_chan, ivec2(int(n), int(l)), 0);
        y = v.x; ci = v.y; cq = v.z;
    } else {
        float sig = max(u_chroma_sigma, 0.5);
        float wsum = 0.0;
        float phn = L1.x + u_tint;
        for (int j = -9; j <= 9; j++) {
            float fj = float(j);
            float w = exp(-0.5 * fj * fj / (sig * sig));
            float ch;
            if (u_input == 2) {
                ch = texelFetch(u_chan, ivec2(int(mod(n + fj, SW)), int(l)), 0).g;
            } else {
                ch = chromaAt(n + fj, l);
            }
            float ph = TAU * ((n + fj) * 0.25 + linePhase(l)) + phn;
            ci += w * 2.0 * ch * cos(ph);
            cq += w * 2.0 * ch * sin(ph);
            wsum += w;
        }
        ci /= wsum; cq /= wsum;
        if (u_input == 2) {
            float a = C(n, l);
            float b = 0.5 * (C(n - 3.0, l) + C(n + 3.0, l));
            y = a + u_sharp * (a - b);
        } else {
            float a = lumaAt(n, l);
            float b = 0.5 * (lumaAt(n - 3.0, l) + lumaAt(n + 3.0, l));
            y = a + u_sharp * (a - b);
        }
    }
    float yv = (y - 0.075) / 0.925;
    vec2 iq = vec2(ci, cq) / 0.925 * u_color * (1.0 - u_killer);
    if (u_input == 3) {
        // component input: tint rotates the I/Q plane too
        float cs = cos(u_tint), sn = sin(u_tint);
        iq = mat2(cs, sn, -sn, cs) * iq;
    }
    vec3 rgb = yiq2rgb(vec3(yv, iq));
    vec3 drive = rgb * u_contrast + u_brightness;
    o_col = vec4(drive, 1.0);
}
"#;

// ---------------------------------------------------------------- burn-in accumulation
pub const BURN: &str = r#"
uniform sampler2D u_prev;
uniform sampler2D u_drive;
uniform float u_rate;
uniform float u_gamma;
uniform float u_overscan;
void main() {
    vec2 uv = gl_FragCoord.xy / vec2(512.0, 384.0);
    vec3 prev = texelFetch(u_prev, ivec2(gl_FragCoord.xy), 0).rgb;
    // face position -> nominal raster position (ignores roll; phosphor burns where the picture usually sits)
    vec2 f = (uv * 2.0 - 1.0) * vec2(4.0 / 3.0, 1.0);
    float s = 0.5 * (ACT0 + ACT1) + f.x / (4.0 / 3.0) * 0.5 * (ACT1 - ACT0) * (1.0 - u_overscan);
    float lf = VACT0 + 0.5 * VLINES + f.y * 0.5 * VLINES * (1.0 - u_overscan);
    vec3 d = textureLod(u_drive, vec2(s / SW, lf / NL), 0.0).rgb;
    vec3 e = pow(clamp(d, 0.0, 2.0), vec3(u_gamma));
    vec3 nb = prev + u_rate * e * (1.0 - prev);
    o_col = vec4(clamp(nb, 0.0, 1.0), 1.0);
}
"#;

// ---------------------------------------------------------------- beam + shadow mask
pub const BEAM: &str = r#"
uniform vec2 u_view_px;
uniform vec3 u_view;
uniform float u_curv;
uniform float u_corner;
uniform sampler2D u_drive;
uniform sampler2D u_lines;
uniform sampler2D u_burn;
uniform float u_hsize, u_vsize, u_hpos, u_vpos, u_pin, u_breath, u_vphase, u_field_shift, u_overscan;
uniform float u_ripple, u_time;
uniform vec2 u_conv_r, u_conv_b;
uniform float u_conv_dyn;
uniform float u_spot, u_spot_h, u_bloom, u_gamma;
uniform vec3 u_focus, u_emission, u_cutoff, u_imax;
uniform int u_mask;
uniform float u_pitch, u_bm, u_mask_strength;
uniform float u_purity;
uniform vec2 u_purity_seed;
uniform vec3 u_degauss;
uniform float u_burn_amt;
uniform float u_sat;
uniform vec4 u_collapse;
uniform float u_abl;
uniform float u_maxlod;
uniform float u_wire;

float g_abl;
float g_breath;

vec2 rasterPos(vec2 f) {
    vec2 q = f;
    q.x *= 1.0 - u_pin * 0.08 * q.y * q.y;
    q.y *= 1.0 - u_pin * 0.05 * q.x * q.x;
    q -= vec2(u_hpos, u_vpos);
    float rr = sin(TAU * (q.y * 0.5 + u_time * 0.06));
    q.x += u_ripple * 0.006 * rr;
    float rx = q.x / (4.0 / 3.0 * u_hsize * g_breath * u_collapse.x * (1.0 + u_ripple * 0.004 * rr));
    float ry = q.y / (u_vsize * g_breath * u_collapse.y);
    float s = 0.5 * (ACT0 + ACT1) + rx * 0.5 * (ACT1 - ACT0) * (1.0 - u_overscan);
    float lf = VACT0 + 0.5 * VLINES + ry * 0.5 * VLINES * (1.0 - u_overscan) + u_vphase;
    return vec2(s, lf);
}

float current(float drive, int g) {
    float v = max(drive * g_abl - u_cutoff[g], 0.0);
    float raw = pow(v, u_gamma) * u_emission[g];
    float im = u_imax[g];
    return im * (1.0 - exp(-raw / im));   // cathode emission limit (worn cathodes "silver" highlights)
}

float gunEnergy(int g, vec2 f) {
    vec2 rp = rasterPos(f);
    float lf = rp.y - u_field_shift;
    float k0 = floor(lf - 0.5);
    float sig0 = u_spot * u_focus[g];
    float sh = max(u_spot_h * u_focus[g], 0.3);
    float e = 0.0;
    for (int i = -2; i <= 3; i++) {
        float k = k0 + float(i);
        // When vertical sync is locked, beams deflected past the raster edges hit blanking
        // rather than wrapping around from the opposite side of the screen.
        if (abs(u_vphase) < 2.0 && (k < 0.0 || k >= NL)) continue;
        float dy = lf - (k + 0.5);
        float kk = mod(k, NL);
        float hoff = texelFetch(u_lines, ivec2(int(kk), 0), 0).x;
        float s = rp.x + hoff;
        float ty = (kk + 0.5) / NL;
        float I = 0.0;
        I += 0.0545 * current(textureLod(u_drive, vec2((s - 2.0 * sh) / SW, ty), 0.0)[g], g);
        I += 0.2442 * current(textureLod(u_drive, vec2((s - sh) / SW, ty), 0.0)[g], g);
        I += 0.4026 * current(textureLod(u_drive, vec2(s / SW, ty), 0.0)[g], g);
        I += 0.2442 * current(textureLod(u_drive, vec2((s + sh) / SW, ty), 0.0)[g], g);
        I += 0.0545 * current(textureLod(u_drive, vec2((s + 2.0 * sh) / SW, ty), 0.0)[g], g);
        float sg = sig0 * (1.0 + u_bloom * sqrt(I));
        e += I * exp(-0.5 * dy * dy / (sg * sg)) / (sg * 2.5066283);
    }
    return e;
}

// fraction of a box footprint [x-w/2, x+w/2] covered by a periodic stripe [start, start+duty)
float boxI(float x, float duty) { return floor(x) * duty + min(fract(x), duty); }
float stripe(float x, float start, float duty, float w) {
    w = max(w, 1e-3);
    float a = x - start;
    return (boxI(a + 0.5 * w, duty) - boxI(a - 0.5 * w, duty)) / w;
}

vec3 maskCoverage(vec2 f, float w, out vec3 avg) {
    float P = u_pitch;
    float bm = u_bm;
    if (u_mask == 0 || u_mask == 1) {
        float u = f.x / P;
        float duty = (1.0 - bm) / 3.0;
        vec3 cx = vec3(stripe(u, bm / 6.0, duty, w / P),
                       stripe(u, 1.0 / 3.0 + bm / 6.0, duty, w / P),
                       stripe(u, 2.0 / 3.0 + bm / 6.0, duty, w / P));
        avg = vec3(duty);
        if (u_mask == 1) {
            float hs = P * 2.2;
            float col = floor(u);
            float v = f.y / hs + 0.5 * mod(col, 2.0);
            float bridge = 0.12 + 0.3 * bm;
            float cy = stripe(v, bridge * 0.5, 1.0 - bridge, w / hs);
            cx *= cy;
            avg *= (1.0 - bridge);
        } else {
            // two damper wires of the grille
            float wt = u_wire;
            float d1 = abs(f.y - 1.0 / 3.0), d2 = abs(f.y + 1.0 / 3.0);
            float cov = clamp((min(d1, d2) - 0.5 * wt) / max(w, 1e-4) + 0.5, 0.0, 1.0);
            cx *= mix(1.0, cov, 0.85);
        }
        return cx;
    }
    // delta dot triads on a hexagonal lattice
    float d = P / 1.7320508;
    float rs = d * 0.8660254;
    float rad = 0.5 * d * (1.0 - 0.55 * bm);
    float fr = 3.14159265 * rad * rad / (d * rs);
    avg = vec3(fr / 3.0);
    vec3 cov = vec3(0.0);
    float row0 = floor(f.y / rs + 0.5);
    for (int jr = -1; jr <= 1; jr++) {
        float row = row0 + float(jr);
        float off = mod(row, 2.0) * 0.5;
        float col0 = floor(f.x / d - off + 0.5);
        for (int ic = -1; ic <= 1; ic++) {
            float col = col0 + float(ic);
            vec2 c = vec2((col + off) * d, row * rs);
            float dist = length(f - c);
            float a = clamp((rad - dist) / max(w, 1e-5) + 0.5, 0.0, 1.0);
            float q = col - (row - mod(row, 2.0)) * 0.5;
            int ci = int(mod(q - row, 3.0) + 0.5);
            if (ci == 0) cov.r += a; else if (ci == 1) cov.g += a; else cov.b += a;
        }
    }
    float fade = smoothstep(0.35, 0.9, w / d);
    return mix(cov, avg, fade);
}

void main() {
    vec2 f = faceFromPixel(gl_FragCoord.xy, u_view_px, u_view, u_curv);
    float pxw = 2.0 * u_view.z / u_view_px.y * (1.0 + 3.0 * u_curv * dot(f, f));
    float sdf = tubeSDF(f, u_corner);
    if (sdf > pxw) { o_col = vec4(0.0); return; }

    vec3 apl3 = max(textureLod(u_drive, vec2(0.5), u_maxlod).rgb, 0.0);
    float apl = dot(apl3, vec3(0.3, 0.59, 0.11));
    g_abl = 1.0 / (1.0 + u_abl * 3.0 * max(apl - 0.3, 0.0));
    g_breath = 1.0 + u_breath * 0.06 * apl;

    vec2 fn = f / vec2(4.0 / 3.0, 1.0);
    float r2_fn = dot(fn, fn);
    float dg_amp = u_degauss.x;
    float dg_t = u_degauss.y;
    float dg_ph = u_degauss.z;
    // Perimeter degauss coil Lorentz deflection: strongest near the coil loop
    // (edges/corners), oscillating at the AC mains wave along the vertical scan.
    vec2 dg_disp = vec2(0.0);
    vec2 dg_conv = vec2(0.0);
    if (dg_amp > 1e-4) {
        float edge = 0.22 + 0.78 * clamp(r2_fn, 0.0, 1.6);
        float ac_y = f.y * 3.6 + dg_t * 34.0 + dg_ph;
        float ac_x = f.x * 2.1 - dg_t * 27.0 + dg_ph * 0.7;
        float w1 = sin(ac_y) + 0.28 * sin(ac_y * 2.0 - dg_t * 15.0);
        float w2 = cos(ac_y * 0.9 + 0.6) + 0.22 * sin(ac_x);
        // Radial pinch / barrel breathing (sides pull inward and bulge outward)
        vec2 radial = f * (0.14 * edge * w1 + 0.045 * sin(dg_t * 19.0 + dg_ph));
        // Torsional swirl from the axial magnetic field through the loop
        vec2 swirl = vec2(-f.y, f.x) * (0.11 * edge * w2);
        // Rolling S-wave buckling down the screen
        vec2 swave = vec2(
            (0.060 + 0.050 * fn.x * fn.x) * sin(ac_y + fn.x * 0.8),
            (0.036 + 0.032 * fn.y * fn.y) * cos(ac_y * 1.15 - fn.x * 0.7)
        );
        dg_disp = dg_amp * (radial + swirl + swave);
        // Inline R/G/B guns enter at slightly different angles, causing transient
        // chromatic convergence separation during the magnetic surge.
        dg_conv = dg_amp * (0.35 + 0.65 * edge) * vec2(
            0.028 * sin(ac_y + 0.5) + 0.012 * fn.x * w2,
            0.016 * cos(ac_y - 0.4) + 0.010 * fn.y * w1
        );
    }

    vec3 E;
    if (u_collapse.w > 0.5) {
        // power-off: the deflection collapses and the remaining charge is dumped in a shrinking spot
        float hx = 4.0 / 3.0 * u_hsize * u_collapse.x;
        float hy = u_vsize * u_collapse.y;
        float sp = 0.006;
        float bx = smoothstep(hx + sp, hx - sp, abs(f.x));
        float by = smoothstep(hy + sp, hy - sp, abs(f.y));
        float conc = min(1.0 / max(u_collapse.x * u_collapse.y, 1e-4), 400.0);
        E = vec3(u_collapse.z * bx * by * min(conc, 60.0));
    } else {
        // static + dynamic convergence + transient degauss deflection
        vec2 fc = f + dg_disp;
        vec2 dyn = f * abs(f) * u_conv_dyn * 0.004;
        vec2 fr = fc + u_conv_r * 0.004 + dyn + dg_conv;
        vec2 fb = fc + u_conv_b * 0.004 - dyn - dg_conv;
        E = vec3(gunEnergy(0, fr), gunEnergy(1, fc), gunEnergy(2, fb));
    }

    // Purity: magnetised mask/surroundings and the degauss coil bend beams across
    // the repeating R-G-B phosphor triad, creating smooth iridescent colour bands.
    vec3 P = vec3(0.0);
    float dg_field = 0.0;
    if (dg_amp > 1e-4) {
        float edge_p = 0.25 + 0.75 * clamp(r2_fn, 0.0, 1.5);
        float r = sqrt(r2_fn);
        float ac_p = f.y * 3.2 + dg_t * 32.0 + dg_ph;
        dg_field = dg_amp * edge_p * (
            1.35 * sin(r * 4.2 - dg_t * 14.0 + ac_p * 0.65)
            + 0.75 * cos(fn.x * 2.8 + fn.y * 2.2 - dg_t * 24.0 + dg_ph)
        );
    }
    for (int g = 0; g < 3; g++) {
        float fg = float(g);
        float mag = 0.0;
        if (u_purity > 1e-4) {
            float b1 = vnoise(f * 1.2 + u_purity_seed) - 0.5;
            float b2 = vnoise(f * 0.9 + u_purity_seed + vec2(19.0, 31.0) * (fg + 1.0)) - 0.5;
            mag = (b1 * 1.6 + b2 * 1.4) * u_purity * 2.2;
        }
        // Continuous landing shift across the 3-phosphor triad (0 = own, +1 = next, +2 = third)
        float shift = mag + dg_field * (1.0 + 0.22 * (fg - 1.0));
        float u = mod(shift, 3.0);
        int i0 = int(floor(u));
        int i1 = (i0 + 1) % 3;
        float w = smoothstep(0.0, 1.0, fract(u));
        float e = E[g];
        P[(g + i0) % 3] += e * (1.0 - w);
        P[(g + i1) % 3] += e * w;
    }

    // phosphor burn-in
    vec2 buv = (f / vec2(4.0 / 3.0, 1.0)) * 0.5 + 0.5;
    vec3 burn = texture(u_burn, buv).rgb;
    P *= 1.0 - u_burn_amt * burn * 0.85;

    vec3 avg;
    // Use an exact 1-pixel footprint once triads reach Nyquist (>= 3 px/triad) so
    // 1:1 pixel-perfect phosphors stay razor-sharp, while widening and fading out
    // below ~3 px/triad to avoid moire against the display pixel grid.
    float px_per_triad = u_pitch / pxw;
    float filter_w = pxw * mix(1.5, 1.0, smoothstep(2.2, 2.95, px_per_triad));
    vec3 cov = maskCoverage(f, filter_w, avg);
    float resolvable = smoothstep(2.0, 2.95, px_per_triad);
    cov = mix(avg, cov, resolvable);
    vec3 T = mix(vec3(1.0), cov / avg, u_mask_strength);
    vec3 out_e = P * T;
    // phosphor saturation (light output flattens at high current density)
    out_e = out_e / (1.0 + u_sat * out_e) * (1.0 + u_sat);
    float edge = smoothstep(pxw, -pxw, sdf);
    // When did the beam pass this spot? (fraction of the field: lines top to bottom,
    // samples left to right). Stored for the rolling-scan pass.
    vec2 rpg = rasterPos(f);
    float lt = mod(floor(rpg.y - u_field_shift), NL);
    float tp = fract((lt + clamp(rpg.x, 0.0, SW) / SW) / NL);
    o_col = vec4(max(out_e * edge, 0.0), tp);
}
"#;

// ---------------------------------------------------------------- persistence + eye integration
pub const TAIL: &str = r#"
uniform sampler2D u_e;
uniform sampler2D u_prev;
uniform vec3 u_decay;
uniform vec3 u_frac;
uniform float u_reset;
void main() {
    ivec2 p = ivec2(gl_FragCoord.xy);
    vec3 e = texelFetch(u_e, p, 0).rgb;
    vec3 t = texelFetch(u_prev, p, 0).rgb * (1.0 - u_reset);
    o_col = vec4(t * u_decay + u_frac * e * (1.0 - u_decay), 1.0);
}
"#;

pub const EYE: &str = r#"
uniform sampler2D u_e;
uniform sampler2D u_tail;
uniform sampler2D u_prev;
uniform vec3 u_frac;
uniform float u_k;
uniform float u_tail_gain;
void main() {
    ivec2 p = ivec2(gl_FragCoord.xy);
    vec3 e = texelFetch(u_e, p, 0).rgb;
    vec3 t = texelFetch(u_tail, p, 0).rgb;
    vec3 L = (1.0 - u_frac) * e + u_tail_gain * t;
    vec3 prev = texelFetch(u_prev, p, 0).rgb;
    o_col = vec4(mix(prev, L, u_k), 1.0);
}
"#;

// ---------------------------------------------------------------- rolling scan
// Each spot was excited when the beam last swept over it; its light then decays
// (fast phosphor decay + slow afterglow). A displayed frame shows only the light
// emitted during that frame, so with short frames you see the beam paint the
// picture. Units: one field = 1.0. Each excitation is normalised so a whole field
// averages to the steady-state brightness.
pub const SCAN: &str = r#"
uniform sampler2D u_cur;
uniform sampler2D u_prev;
uniform float u_phase;
uniform float u_w;
uniform vec3 u_tf;
uniform vec3 u_ts;
uniform vec3 u_frac;
uniform float u_a;

float term0(float a, float w, float tau) {
    return a <= w ? 1.0 - exp(-a / tau) : exp(-(a - w) / tau) - exp(-a / tau);
}
// all earlier excitations (one per field) with the same energy
float term1(float a1, float w, float tau) {
    return exp(-(a1 - w) / tau) * (1.0 - exp(-w / tau)) / max(1.0 - exp(-1.0 / tau), 1e-6);
}

void main() {
    ivec2 p = ivec2(gl_FragCoord.xy);
    vec4 c = texelFetch(u_cur, p, 0);
    vec3 pe = texelFetch(u_prev, p, 0).rgb;
    float tp = c.a;
    float a0;
    vec3 E0;
    if (tp <= u_phase) { a0 = u_phase - tp; E0 = c.rgb; }
    else { a0 = 1.0 + u_phase - tp; E0 = pe; }
    vec3 E1 = pe;
    float a1 = a0 + 1.0;
    float w = clamp(u_w, 1e-5, 1.0);
    vec3 o;
    for (int i = 0; i < 3; i++) {
        float tf = max(u_tf[i], 1e-6);
        float ts = max(u_ts[i], 1e-6);
        float fast = E0[i] * term0(a0, w, tf) + E1[i] * term1(a1, w, tf);
        float slow = E0[i] * term0(a0, w, ts) + E1[i] * term1(a1, w, ts);
        o[i] = mix(fast, slow, u_frac[i]);
    }
    o_col = vec4(max(o * (u_a / w), 0.0), 1.0);
}
"#;

pub const DOWN: &str = r#"
uniform sampler2D u_tex;
uniform vec2 u_src_px;
void main() {
    vec2 c = gl_FragCoord.xy * 4.0;
    vec2 ip = 1.0 / u_src_px;
    vec3 s = texture(u_tex, (c + vec2(-1.0, -1.0)) * ip).rgb
           + texture(u_tex, (c + vec2(1.0, -1.0)) * ip).rgb
           + texture(u_tex, (c + vec2(-1.0, 1.0)) * ip).rgb
           + texture(u_tex, (c + vec2(1.0, 1.0)) * ip).rgb;
    o_col = vec4(s * 0.25, 1.0);
}
"#;

pub const BLUR: &str = r#"
uniform sampler2D u_tex;
uniform vec2 u_px;
uniform vec2 u_dir;
uniform float u_sigma;
void main() {
    vec2 uv = gl_FragCoord.xy / u_px;
    float st = max(u_sigma / 3.0, 1.0);
    vec3 acc = vec3(0.0);
    float ws = 0.0;
    for (int i = -9; i <= 9; i++) {
        float x = float(i) * st;
        float w = exp(-0.5 * x * x / (u_sigma * u_sigma + 0.01));
        acc += w * texture(u_tex, uv + u_dir * x / u_px).rgb;
        ws += w;
    }
    o_col = vec4(acc / ws, 1.0);
}
"#;

// ---------------------------------------------------------------- final: glass, room, bezel
pub const COMPOSITE: &str = r#"
uniform sampler2D u_eye;
uniform sampler2D u_bs;
uniform sampler2D u_bw;
uniform vec2 u_view_px;
uniform vec2 u_off;
uniform vec3 u_view;
uniform float u_curv, u_corner;
uniform mat3 u_phos;
uniform float u_diffuse, u_halation, u_ambient, u_reflect, u_vignette, u_exposure;
uniform vec3 u_glass;
uniform int u_bezel;
uniform uint u_frame;

vec3 tonemap(vec3 x) {
    vec3 hi = 0.8 + 0.2 * (1.0 - exp(-(x - 0.8) / 0.2));
    return mix(x, hi, step(vec3(0.8), x));
}

void main() {
    vec2 loc = gl_FragCoord.xy - u_off;
    vec2 uv = loc / u_view_px;
    vec2 f = faceFromPixel(loc, u_view_px, u_view, u_curv);
    float pxw = 2.0 * u_view.z / u_view_px.y;
    float sdf = tubeSDF(f, u_corner);
    vec3 phos = texture(u_eye, uv).rgb + u_diffuse * texture(u_bs, uv).rgb + u_halation * texture(u_bw, uv).rgb;
    vec3 lin = max(u_phos * phos, 0.0);
    lin *= u_glass;
    float r2 = dot(f / vec2(4.0 / 3.0, 1.0), f / vec2(4.0 / 3.0, 1.0));
    lin *= 1.0 - u_vignette * 0.25 * r2;
    // room light reflected by the grey phosphor screen and the curved glass
    float glassArea = smoothstep(0.07 + pxw, 0.07 - pxw, sdf);
    vec3 screenGrey = vec3(0.30, 0.31, 0.30) * 0.05 * u_ambient * smoothstep(pxw, -pxw, sdf);
    vec2 hl = f - vec2(-0.75, -0.62);
    float spec = exp(-dot(hl, hl) / 0.18) * 0.06 + exp(-pow(abs(f.y + 0.85), 2.0) / 0.01) * 0.015;
    vec3 refl = vec3(0.9, 0.95, 1.0) * spec * u_reflect * u_ambient * glassArea;
    lin = lin * glassArea + screenGrey + refl + vec3(0.002, 0.0022, 0.0025) * glassArea;
    vec3 col = tonemap(lin * u_exposure);
    vec3 room = vec3(0.0);   // pure black around the TV
    if (u_bezel == 1 && sdf > 0.07 - pxw) {
        // Retro woodgrain television cabinet: moulded dark charcoal inner mask,
        // thin brushed-aluminum pinstripe trim, and warm walnut woodgrain surround.
        float d = sdf - 0.07;
        float bw = 0.21;
        float e = 0.002;
        vec2 grad = vec2(tubeSDF(f + vec2(e, 0.0), u_corner) - tubeSDF(f - vec2(e, 0.0), u_corner),
                         tubeSDF(f + vec2(0.0, e), u_corner) - tubeSDF(f - vec2(0.0, e), u_corner));
        vec2 n2 = normalize(grad + 1e-6);
        float up = dot(n2, vec2(0.0, -1.0));             // +1 on the top edge
        float bevel = 1.0 - smoothstep(0.0, 0.045, d);   // 1 at the inner lip
        float outer = smoothstep(bw - 0.032, bw, d);     // rounding of the outer edge
        float lightBevel = 0.5 - 0.5 * up;
        float lightOuter = 0.5 + 0.5 * up;
        float shade = mix(0.58, 1.0 + 0.85 * lightBevel, bevel);
        shade = mix(shade, 0.35 + 1.05 * lightOuter, outer);

        // Moulded charcoal inner surround (d = 0..0.092)
        vec3 plastic = vec3(0.017, 0.016, 0.015) * shade;
        plastic *= 0.93 + 0.14 * hash12(floor(loc * 0.8));

        // Brushed-aluminum pinstripe trim (d = 0.092..0.105)
        float chromeMask = smoothstep(0.088, 0.094, d) * (1.0 - smoothstep(0.102, 0.108, d));
        float brush = 0.90 + 0.10 * sin(f.y * 260.0 + f.x * 12.0);
        vec3 chrome = vec3(0.14, 0.132, 0.118) * brush * (0.65 + 0.65 * lightOuter);

        // Faux walnut woodgrain outer cabinet (d > 0.105)
        float woodMask = smoothstep(0.102, 0.110, d);
        float warp = 3.2 * vnoise(vec2(f.x * 2.4, f.y * 8.0)) + 1.4 * vnoise(vec2(f.x * 7.0, f.y * 20.0));
        float g1 = 0.5 + 0.5 * sin(f.y * 42.0 + warp);
        float g2 = 0.5 + 0.5 * sin(f.y * 115.0 + warp * 2.3 + f.x * 2.0);
        float grain = g1 * 0.68 + g2 * 0.32;
        vec3 walnut = mix(vec3(0.022, 0.010, 0.0045), vec3(0.068, 0.031, 0.012), grain);
        walnut *= shade * (0.94 + 0.12 * vnoise(f * 36.0));

        vec3 cab = mix(plastic, walnut, woodMask);
        cab = mix(cab, chrome, chromeMask);
        cab *= 0.55 + u_ambient * 1.9;
        // subtle picture light spill onto the inner bevel lip
        cab += lin * 0.032 * bevel;
        vec3 bz = tonemap(cab * u_exposure);
        float a = smoothstep(0.07 - pxw, 0.07 + pxw, sdf);
        float outside = smoothstep(bw - pxw, bw + pxw, d);
        col = mix(col, bz, a);
        col = mix(col, room, outside);
    } else if (u_bezel == 0 && sdf > 0.07) {
        col = room;
    }
    col = pow(clamp(col, 0.0, 1.0), vec3(1.0 / 2.2));
    float dn = rnd3(uint(gl_FragCoord.x), uint(gl_FragCoord.y), u_frame) - 0.5;
    // dither against banding, but leave true black exactly black
    col += dn / 255.0 * step(1.0 / 512.0, max(col.r, max(col.g, col.b)));
    o_col = vec4(col, 1.0);
}
"#;
