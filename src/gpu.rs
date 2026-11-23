//! OpenGL pipeline: source -> baseband -> channel -> decode -> beam/mask ->
//! phosphor persistence -> eye integration -> glass blur -> composite.

use crate::shaders;
use eframe::glow::{self, HasContext};
use std::collections::HashMap;

pub const SW: i32 = 910;
pub const NL: i32 = 263;
const BURN_W: i32 = 512;
const BURN_H: i32 = 384;

/// Everything one frame of rendering needs, computed on the CPU.
#[derive(Clone, Default)]
pub struct FrameInput {
    pub new_field: bool,
    pub lines: Vec<f32>, // NL * 2 * 4
    pub u: Uniforms,
    pub image: Option<(u64, std::sync::Arc<(usize, usize, Vec<u8>)>)>,
    pub clear_burn: bool,
    pub burn_now: bool,
    pub reset_trails: bool,
}

#[derive(Clone, Default)]
pub struct Uniforms {
    pub time: f32,
    pub frame: u32,
    pub pattern: i32,
    pub flat: [f32; 3],
    pub src_w: i32,
    pub src_h: i32,
    pub field: i32,
    pub interlaced: i32,
    pub field_phase: f32,
    // baseband
    pub vcr: i32,
    pub luma_sigma: f32,
    pub i_sigma: f32,
    pub q_sigma: f32,
    pub chroma_delay: f32,
    pub vcr_sharp: f32,
    pub tape_noise: f32,
    pub chroma_noise: f32,
    pub dropout: f32,
    // channel
    pub input: i32,
    pub snow: f32,
    pub noise: f32,
    pub ghost_amp: f32,
    pub ghost_delay: f32,
    pub interf_amp: f32,
    pub interf_freq: f32,
    pub hum: f32,
    pub impulse: f32,
    pub fine_tune: f32,
    // decode
    pub comb: i32,
    pub contrast: f32,
    pub brightness: f32,
    pub color: f32,
    pub tint: f32,
    pub sharp: f32,
    pub killer: f32,
    pub rx_chroma_sigma: f32,
    // view
    pub view: [f32; 3],
    pub curv: f32,
    pub corner: f32,
    // beam
    pub hsize: f32,
    pub vsize: f32,
    pub hpos: f32,
    pub vpos: f32,
    pub pin: f32,
    pub breath: f32,
    pub vphase: f32,
    pub field_shift: f32,
    pub overscan: f32,
    pub ripple: f32,
    pub conv_r: [f32; 2],
    pub conv_b: [f32; 2],
    pub conv_dyn: f32,
    pub spot: f32,
    pub spot_h: f32,
    pub bloom: f32,
    pub gamma: f32,
    pub focus: [f32; 3],
    pub emission: [f32; 3],
    pub cutoff: [f32; 3],
    pub imax: [f32; 3],
    pub mask: i32,
    pub pitch: f32,
    pub bm: f32,
    pub mask_strength: f32,
    pub anti_moire: f32,
    pub wire: f32,
    pub purity: f32,
    pub purity_seed: [f32; 2],
    pub degauss: [f32; 3],
    pub burn_amt: f32,
    pub burn_rate: f32,
    pub sat: f32,
    pub collapse: [f32; 4],
    pub abl: f32,
    // persistence
    pub decay: [f32; 3],
    pub frac: [f32; 3],
    pub eye_k: f32,
    // rolling scan
    pub scan: i32,
    pub scan_phase: f32,
    pub scan_w: f32,
    pub tau_fast: [f32; 3],
    pub tau_slow: [f32; 3],
    pub scan_a: f32,
    pub eye_k_frame: f32,
    // glass
    pub phos: [f32; 9],
    pub diffuse: f32,
    pub halation: f32,
    pub diffuse_sigma_px: f32,
    pub halation_sigma_px: f32,
    pub ambient: f32,
    pub reflect: f32,
    pub vignette: f32,
    pub exposure: f32,
    pub glass: [f32; 3],
    pub bezel: i32,
    pub hdr: i32,
    pub hdr_paper_white: f32,
    pub hdr_peak_nits: f32,
}

struct Program {
    id: glow::Program,
    locs: HashMap<&'static str, Option<glow::UniformLocation>>,
}

struct Target {
    tex: glow::Texture,
    fbo: glow::Framebuffer,
    w: i32,
    h: i32,
}

pub struct Gpu {
    vao: glow::VertexArray,
    p_source: Program,
    p_base: Program,
    p_chan: Program,
    p_decode: Program,
    p_burn: Program,
    p_beam: Program,
    p_tail: Program,
    p_eye: Program,
    p_down: Program,
    p_blur: Program,
    p_comp: Program,
    p_scan: Program,
    lines_tex: glow::Texture,
    img_tex: glow::Texture,
    img_id: u64,
    img_aspect: f32,
    has_img: bool,
    src: Option<Target>,
    base: Target,
    chan: Target,
    drive: Target,
    drive_levels: i32,
    burn: [Target; 2],
    burn_cur: usize,
    view: Option<ViewTargets>,
    pub error: Option<String>,
}

struct ViewTargets {
    w: i32,
    h: i32,
    e: [Target; 2],
    e_cur: usize,
    sc: Target,
    tail: [Target; 2],
    eye: [Target; 2],
    q: [Target; 3],
    cur: usize,
}

unsafe fn compile(gl: &glow::Context, header: &str, frag: &str) -> Result<Program, String> {
    unsafe {
        let program = gl.create_program()?;
        let vs = gl.create_shader(glow::VERTEX_SHADER)?;
        gl.shader_source(vs, &format!("{header}{}", shaders::VERTEX));
        gl.compile_shader(vs);
        if !gl.get_shader_compile_status(vs) {
            return Err(format!("vertex: {}", gl.get_shader_info_log(vs)));
        }
        let fs = gl.create_shader(glow::FRAGMENT_SHADER)?;
        gl.shader_source(fs, &format!("{header}{}{frag}", shaders::COMMON));
        gl.compile_shader(fs);
        if !gl.get_shader_compile_status(fs) {
            return Err(format!("fragment: {}", gl.get_shader_info_log(fs)));
        }
        gl.attach_shader(program, vs);
        gl.attach_shader(program, fs);
        gl.link_program(program);
        if !gl.get_program_link_status(program) {
            return Err(format!("link: {}", gl.get_program_info_log(program)));
        }
        gl.detach_shader(program, vs);
        gl.detach_shader(program, fs);
        gl.delete_shader(vs);
        gl.delete_shader(fs);
        Ok(Program { id: program, locs: HashMap::new() })
    }
}

impl Program {
    fn loc(&mut self, gl: &glow::Context, name: &'static str) -> Option<glow::UniformLocation> {
        let id = self.id;
        self.locs
            .entry(name)
            .or_insert_with(|| unsafe { gl.get_uniform_location(id, name) })
            .clone()
    }
    fn f1(&mut self, gl: &glow::Context, n: &'static str, v: f32) {
        let l = self.loc(gl, n);
        unsafe { gl.uniform_1_f32(l.as_ref(), v) }
    }
    fn f2(&mut self, gl: &glow::Context, n: &'static str, v: [f32; 2]) {
        let l = self.loc(gl, n);
        unsafe { gl.uniform_2_f32(l.as_ref(), v[0], v[1]) }
    }
    fn f3(&mut self, gl: &glow::Context, n: &'static str, v: [f32; 3]) {
        let l = self.loc(gl, n);
        unsafe { gl.uniform_3_f32(l.as_ref(), v[0], v[1], v[2]) }
    }
    fn f4(&mut self, gl: &glow::Context, n: &'static str, v: [f32; 4]) {
        let l = self.loc(gl, n);
        unsafe { gl.uniform_4_f32(l.as_ref(), v[0], v[1], v[2], v[3]) }
    }
    fn i1(&mut self, gl: &glow::Context, n: &'static str, v: i32) {
        let l = self.loc(gl, n);
        unsafe { gl.uniform_1_i32(l.as_ref(), v) }
    }
    fn u1(&mut self, gl: &glow::Context, n: &'static str, v: u32) {
        let l = self.loc(gl, n);
        unsafe { gl.uniform_1_u32(l.as_ref(), v) }
    }
    fn m3(&mut self, gl: &glow::Context, n: &'static str, v: &[f32; 9]) {
        let l = self.loc(gl, n);
        unsafe { gl.uniform_matrix_3_f32_slice(l.as_ref(), false, v) }
    }
    fn tex(&mut self, gl: &glow::Context, n: &'static str, unit: u32, t: glow::Texture) {
        let l = self.loc(gl, n);
        unsafe {
            gl.active_texture(glow::TEXTURE0 + unit);
            gl.bind_texture(glow::TEXTURE_2D, Some(t));
            gl.uniform_1_i32(l.as_ref(), unit as i32);
        }
    }
}

unsafe fn make_tex(gl: &glow::Context, w: i32, h: i32, linear: bool, repeat_x: bool) -> Result<glow::Texture, String> {
    unsafe {
        let t = gl.create_texture()?;
        gl.bind_texture(glow::TEXTURE_2D, Some(t));
        gl.tex_image_2d(
            glow::TEXTURE_2D,
            0,
            glow::RGBA16F as i32,
            w,
            h,
            0,
            glow::RGBA,
            glow::HALF_FLOAT,
            glow::PixelUnpackData::Slice(None),
        );
        let filt = if linear { glow::LINEAR } else { glow::NEAREST } as i32;
        gl.tex_parameter_i32(glow::TEXTURE_2D, glow::TEXTURE_MIN_FILTER, filt);
        gl.tex_parameter_i32(glow::TEXTURE_2D, glow::TEXTURE_MAG_FILTER, filt);
        let wx = if repeat_x { glow::REPEAT } else { glow::CLAMP_TO_EDGE } as i32;
        gl.tex_parameter_i32(glow::TEXTURE_2D, glow::TEXTURE_WRAP_S, wx);
        gl.tex_parameter_i32(glow::TEXTURE_2D, glow::TEXTURE_WRAP_T, glow::CLAMP_TO_EDGE as i32);
        Ok(t)
    }
}

unsafe fn make_target(gl: &glow::Context, w: i32, h: i32, linear: bool, repeat_x: bool) -> Result<Target, String> {
    unsafe {
        let tex = make_tex(gl, w, h, linear, repeat_x)?;
        let fbo = gl.create_framebuffer()?;
        gl.bind_framebuffer(glow::FRAMEBUFFER, Some(fbo));
        gl.framebuffer_texture_2d(glow::FRAMEBUFFER, glow::COLOR_ATTACHMENT0, glow::TEXTURE_2D, Some(tex), 0);
        let st = gl.check_framebuffer_status(glow::FRAMEBUFFER);
        gl.clear_color(0.0, 0.0, 0.0, 0.0);
        gl.viewport(0, 0, w, h);
        gl.clear(glow::COLOR_BUFFER_BIT);
        gl.bind_framebuffer(glow::FRAMEBUFFER, None);
        if st != glow::FRAMEBUFFER_COMPLETE {
            return Err(format!("framebuffer incomplete: 0x{st:x}"));
        }
        Ok(Target { tex, fbo, w, h })
    }
}

unsafe fn free_target(gl: &glow::Context, t: &Target) {
    unsafe {
        gl.delete_framebuffer(t.fbo);
        gl.delete_texture(t.tex);
    }
}

impl Gpu {
    pub fn new(gl: &glow::Context) -> Result<Self, String> {
        unsafe {
            let header = if gl.version().is_embedded {
                "#version 300 es\nprecision highp float;\nprecision highp int;\nprecision highp sampler2D;\n"
            } else {
                "#version 330 core\n"
            };
            let c = |src: &str| compile(gl, header, src);
            let p_source = c(shaders::SOURCE).map_err(|e| format!("source {e}"))?;
            let p_base = c(shaders::BASEBAND).map_err(|e| format!("baseband {e}"))?;
            let p_chan = c(shaders::CHANNEL).map_err(|e| format!("channel {e}"))?;
            let p_decode = c(shaders::DECODE).map_err(|e| format!("decode {e}"))?;
            let p_burn = c(shaders::BURN).map_err(|e| format!("burn {e}"))?;
            let p_beam = c(shaders::BEAM).map_err(|e| format!("beam {e}"))?;
            let p_tail = c(shaders::TAIL).map_err(|e| format!("tail {e}"))?;
            let p_eye = c(shaders::EYE).map_err(|e| format!("eye {e}"))?;
            let p_down = c(shaders::DOWN).map_err(|e| format!("down {e}"))?;
            let p_blur = c(shaders::BLUR).map_err(|e| format!("blur {e}"))?;
            let p_comp = c(shaders::COMPOSITE).map_err(|e| format!("composite {e}"))?;
            let p_scan = c(shaders::SCAN).map_err(|e| format!("scan {e}"))?;
            let vao = gl.create_vertex_array()?;

            let lines_tex = gl.create_texture()?;
            gl.bind_texture(glow::TEXTURE_2D, Some(lines_tex));
            gl.tex_image_2d(
                glow::TEXTURE_2D,
                0,
                glow::RGBA32F as i32,
                NL,
                2,
                0,
                glow::RGBA,
                glow::FLOAT,
                glow::PixelUnpackData::Slice(None),
            );
            for (p, v) in [
                (glow::TEXTURE_MIN_FILTER, glow::NEAREST),
                (glow::TEXTURE_MAG_FILTER, glow::NEAREST),
                (glow::TEXTURE_WRAP_S, glow::CLAMP_TO_EDGE),
                (glow::TEXTURE_WRAP_T, glow::CLAMP_TO_EDGE),
            ] {
                gl.tex_parameter_i32(glow::TEXTURE_2D, p, v as i32);
            }

            let img_tex = gl.create_texture()?;
            gl.bind_texture(glow::TEXTURE_2D, Some(img_tex));
            gl.tex_image_2d(
                glow::TEXTURE_2D,
                0,
                glow::RGBA8 as i32,
                1,
                1,
                0,
                glow::RGBA,
                glow::UNSIGNED_BYTE,
                glow::PixelUnpackData::Slice(Some(&[20, 20, 20, 255])),
            );
            gl.tex_parameter_i32(glow::TEXTURE_2D, glow::TEXTURE_MIN_FILTER, glow::LINEAR as i32);
            gl.tex_parameter_i32(glow::TEXTURE_2D, glow::TEXTURE_MAG_FILTER, glow::LINEAR as i32);

            let base = make_target(gl, SW, NL, false, true)?;
            let chan = make_target(gl, SW, NL, false, true)?;
            let drive = make_target(gl, SW, NL, true, true)?;
            gl.bind_texture(glow::TEXTURE_2D, Some(drive.tex));
            gl.tex_parameter_i32(glow::TEXTURE_2D, glow::TEXTURE_MIN_FILTER, glow::LINEAR_MIPMAP_NEAREST as i32);
            gl.generate_mipmap(glow::TEXTURE_2D);
            let drive_levels = (SW.max(NL) as f32).log2().floor() as i32;
            let burn = [
                make_target(gl, BURN_W, BURN_H, true, false)?,
                make_target(gl, BURN_W, BURN_H, true, false)?,
            ];
            gl.bind_texture(glow::TEXTURE_2D, None);
            gl.bind_framebuffer(glow::FRAMEBUFFER, None);

            Ok(Gpu {
                vao,
                p_source,
                p_base,
                p_chan,
                p_decode,
                p_burn,
                p_beam,
                p_tail,
                p_eye,
                p_down,
                p_blur,
                p_comp,
                p_scan,
                lines_tex,
                img_tex,
                img_id: 0,
                img_aspect: 4.0 / 3.0,
                has_img: false,
                src: None,
                base,
                chan,
                drive,
                drive_levels,
                burn,
                burn_cur: 0,
                view: None,
                error: None,
            })
        }
    }

    unsafe fn ensure_view(&mut self, gl: &glow::Context, w: i32, h: i32) -> Result<(), String> {
        unsafe {
            if let Some(v) = &self.view {
                if v.w == w && v.h == h {
                    return Ok(());
                }
            }
            if let Some(v) = self.view.take() {
                free_target(gl, &v.sc);
                for t in v.e.iter().chain(v.tail.iter()).chain(v.eye.iter()).chain(v.q.iter()) {
                    free_target(gl, t);
                }
            }
            let qw = (w / 4).max(1);
            let qh = (h / 4).max(1);
            self.view = Some(ViewTargets {
                w,
                h,
                e: [make_target(gl, w, h, false, false)?, make_target(gl, w, h, false, false)?],
                e_cur: 0,
                sc: make_target(gl, w, h, false, false)?,
                tail: [make_target(gl, w, h, false, false)?, make_target(gl, w, h, false, false)?],
                eye: [make_target(gl, w, h, true, false)?, make_target(gl, w, h, true, false)?],
                q: [
                    make_target(gl, qw, qh, true, false)?,
                    make_target(gl, qw, qh, true, false)?,
                    make_target(gl, qw, qh, true, false)?,
                ],
                cur: 0,
            });
            Ok(())
        }
    }

    unsafe fn ensure_src(&mut self, gl: &glow::Context, w: i32, h: i32) -> Result<(), String> {
        unsafe {
            if let Some(s) = &self.src {
                if s.w == w && s.h == h {
                    return Ok(());
                }
            }
            if let Some(s) = self.src.take() {
                free_target(gl, &s);
            }
            self.src = Some(make_target(gl, w, h, false, false)?);
            Ok(())
        }
    }

    unsafe fn draw(&self, gl: &glow::Context, t: &Target) {
        unsafe {
            gl.bind_framebuffer(glow::FRAMEBUFFER, Some(t.fbo));
            gl.viewport(0, 0, t.w, t.h);
            gl.draw_arrays(glow::TRIANGLES, 0, 3);
        }
    }

    /// Eye / camera integration followed by the glass blurs (diffusion + halation).
    #[allow(clippy::too_many_arguments)]
    unsafe fn eye_and_glass(
        &mut self,
        gl: &glow::Context,
        u: &Uniforms,
        e_tex: glow::Texture,
        tail_tex: glow::Texture,
        frac: [f32; 3],
        tail_gain: f32,
        k: f32,
        vw: i32,
        vh: i32,
    ) {
        unsafe {
            let view = self.view.as_ref().unwrap();
            let cur = view.cur;
            let nxt = 1 - cur;
            let p = &mut self.p_eye;
            gl.use_program(Some(p.id));
            p.tex(gl, "u_e", 0, e_tex);
            p.tex(gl, "u_tail", 1, tail_tex);
            p.tex(gl, "u_prev", 2, view.eye[cur].tex);
            p.f3(gl, "u_frac", frac);
            p.f1(gl, "u_tail_gain", tail_gain);
            p.f1(gl, "u_k", k);
            self.draw(gl, &view.eye[nxt]);

            let p = &mut self.p_down;
            gl.use_program(Some(p.id));
            p.tex(gl, "u_tex", 0, view.eye[nxt].tex);
            p.f2(gl, "u_src_px", [vw as f32, vh as f32]);
            self.draw(gl, &view.q[0]);
            let qpx = [view.q[0].w as f32, view.q[0].h as f32];
            let passes = [
                (0usize, 1usize, [1.0f32, 0.0f32], u.diffuse_sigma_px),
                (1, 0, [0.0, 1.0], u.diffuse_sigma_px),
                (0, 1, [1.0, 0.0], u.halation_sigma_px),
                (1, 2, [0.0, 1.0], u.halation_sigma_px),
            ];
            for (from, to, dir, sigma) in passes {
                let p = &mut self.p_blur;
                gl.use_program(Some(p.id));
                p.tex(gl, "u_tex", 0, view.q[from].tex);
                p.f2(gl, "u_px", qpx);
                p.f2(gl, "u_dir", dir);
                p.f1(gl, "u_sigma", sigma);
                self.draw(gl, &view.q[to]);
            }
            self.view.as_mut().unwrap().cur = nxt;
        }
    }

    pub fn paint(&mut self, gl: &glow::Context, fi: &FrameInput, vp: [i32; 4]) {
        if self.error.is_some() {
            return;
        }
        if let Err(e) = unsafe { self.paint_inner(gl, fi, vp) } {
            self.error = Some(e);
        }
    }

    unsafe fn paint_inner(&mut self, gl: &glow::Context, fi: &FrameInput, vp: [i32; 4]) -> Result<(), String> {
        unsafe {
            let [vx, vy, vw, vh] = vp;
            if vw < 4 || vh < 4 {
                return Ok(());
            }
            let u = &fi.u;
            gl.disable(glow::SCISSOR_TEST);
            gl.disable(glow::BLEND);
            gl.disable(glow::DEPTH_TEST);
            gl.disable(glow::CULL_FACE);
            gl.bind_vertex_array(Some(self.vao));

            // image upload
            if let Some((id, img)) = &fi.image {
                if *id != self.img_id {
                    self.img_id = *id;
                    let (w, h, data) = &**img;
                    gl.bind_texture(glow::TEXTURE_2D, Some(self.img_tex));
                    gl.pixel_store_i32(glow::UNPACK_ALIGNMENT, 1);
                    gl.tex_image_2d(
                        glow::TEXTURE_2D,
                        0,
                        glow::RGBA8 as i32,
                        *w as i32,
                        *h as i32,
                        0,
                        glow::RGBA,
                        glow::UNSIGNED_BYTE,
                        glow::PixelUnpackData::Slice(Some(data)),
                    );
                    gl.generate_mipmap(glow::TEXTURE_2D);
                    gl.tex_parameter_i32(glow::TEXTURE_2D, glow::TEXTURE_MIN_FILTER, glow::LINEAR_MIPMAP_LINEAR as i32);
                    gl.pixel_store_i32(glow::UNPACK_ALIGNMENT, 4);
                    self.img_aspect = *w as f32 / (*h).max(1) as f32;
                    self.has_img = true;
                }
            }

            self.ensure_view(gl, vw, vh)?;
            self.ensure_src(gl, u.src_w, u.src_h)?;

            if fi.clear_burn {
                for t in &self.burn {
                    gl.bind_framebuffer(glow::FRAMEBUFFER, Some(t.fbo));
                    gl.viewport(0, 0, t.w, t.h);
                    gl.clear_color(0.0, 0.0, 0.0, 1.0);
                    gl.clear(glow::COLOR_BUFFER_BIT);
                }
            }

            if fi.new_field {
                // CPU line model (sync, tracking, timebase)
                if fi.lines.len() == (NL * 2 * 4) as usize {
                    let bytes = std::slice::from_raw_parts(fi.lines.as_ptr() as *const u8, fi.lines.len() * 4);
                    gl.bind_texture(glow::TEXTURE_2D, Some(self.lines_tex));
                    gl.pixel_store_i32(glow::UNPACK_ALIGNMENT, 4);
                    gl.tex_sub_image_2d(
                        glow::TEXTURE_2D,
                        0,
                        0,
                        0,
                        NL,
                        2,
                        glow::RGBA,
                        glow::FLOAT,
                        glow::PixelUnpackData::Slice(Some(bytes)),
                    );
                }

                // 1. source picture
                let src = self.src.as_ref().unwrap();
                let p = &mut self.p_source;
                gl.use_program(Some(p.id));
                p.i1(gl, "u_pattern", u.pattern);
                p.f1(gl, "u_time", u.time);
                p.f2(gl, "u_size", [src.w as f32, src.h as f32]);
                p.tex(gl, "u_img", 0, self.img_tex);
                p.f1(gl, "u_img_aspect", self.img_aspect);
                p.i1(gl, "u_has_img", self.has_img as i32);
                p.f3(gl, "u_flat", u.flat);
                let (src_tex, src_w, src_h) = (src.tex, src.w, src.h);
                self.draw(gl, self.src.as_ref().unwrap());

                // 2. baseband
                let p = &mut self.p_base;
                gl.use_program(Some(p.id));
                p.tex(gl, "u_src", 0, src_tex);
                p.tex(gl, "u_lines", 1, self.lines_tex);
                p.f2(gl, "u_src_size", [src_w as f32, src_h as f32]);
                p.i1(gl, "u_field", u.field);
                p.i1(gl, "u_interlaced", u.interlaced);
                p.u1(gl, "u_frame", u.frame);
                p.i1(gl, "u_vcr", u.vcr);
                p.f1(gl, "u_luma_sigma", u.luma_sigma);
                p.f1(gl, "u_i_sigma", u.i_sigma);
                p.f1(gl, "u_q_sigma", u.q_sigma);
                p.f1(gl, "u_chroma_delay", u.chroma_delay);
                p.f1(gl, "u_sharp", u.vcr_sharp);
                p.f1(gl, "u_tape_noise", u.tape_noise);
                p.f1(gl, "u_chroma_noise", u.chroma_noise);
                p.f1(gl, "u_dropout", u.dropout);
                self.draw(gl, &self.base);

                // 3. channel
                let p = &mut self.p_chan;
                gl.use_program(Some(p.id));
                p.tex(gl, "u_base", 0, self.base.tex);
                p.i1(gl, "u_input", u.input);
                p.f1(gl, "u_field_phase", u.field_phase);
                p.u1(gl, "u_frame", u.frame);
                p.f1(gl, "u_time", u.time);
                p.f1(gl, "u_snow", u.snow);
                p.f1(gl, "u_noise", u.noise);
                p.f1(gl, "u_ghost_amp", u.ghost_amp);
                p.f1(gl, "u_ghost_delay", u.ghost_delay);
                p.f1(gl, "u_interf_amp", u.interf_amp);
                p.f1(gl, "u_interf_freq", u.interf_freq);
                p.f1(gl, "u_hum", u.hum);
                p.f1(gl, "u_impulse", u.impulse);
                p.f1(gl, "u_fine_tune", u.fine_tune);
                self.draw(gl, &self.chan);

                // 4. decode
                let p = &mut self.p_decode;
                gl.use_program(Some(p.id));
                p.tex(gl, "u_chan", 0, self.chan.tex);
                p.tex(gl, "u_lines", 1, self.lines_tex);
                p.i1(gl, "u_input", u.input);
                p.i1(gl, "u_comb", u.comb);
                p.f1(gl, "u_field_phase", u.field_phase);
                p.f1(gl, "u_contrast", u.contrast);
                p.f1(gl, "u_brightness", u.brightness);
                p.f1(gl, "u_color", u.color);
                p.f1(gl, "u_tint", u.tint);
                p.f1(gl, "u_sharp", u.sharp);
                p.f1(gl, "u_killer", u.killer);
                p.f1(gl, "u_chroma_sigma", u.rx_chroma_sigma);
                self.draw(gl, &self.drive);
                gl.bind_texture(glow::TEXTURE_2D, Some(self.drive.tex));
                gl.generate_mipmap(glow::TEXTURE_2D);

                // 5. burn-in
                if u.burn_rate > 0.0 || fi.burn_now {
                    let (prev, next) = (self.burn_cur, 1 - self.burn_cur);
                    let p = &mut self.p_burn;
                    gl.use_program(Some(p.id));
                    p.tex(gl, "u_prev", 0, self.burn[prev].tex);
                    p.tex(gl, "u_drive", 1, self.drive.tex);
                    p.f1(gl, "u_rate", if fi.burn_now { 0.35 } else { u.burn_rate });
                    p.f1(gl, "u_gamma", u.gamma);
                    p.f1(gl, "u_overscan", u.overscan);
                    self.draw(gl, &self.burn[next]);
                    self.burn_cur = next;
                }

                // 6. beam + mask
                let view = self.view.as_ref().unwrap();
                let burn_tex = self.burn[self.burn_cur].tex;
                let p = &mut self.p_beam;
                gl.use_program(Some(p.id));
                p.tex(gl, "u_drive", 0, self.drive.tex);
                p.tex(gl, "u_lines", 1, self.lines_tex);
                p.tex(gl, "u_burn", 2, burn_tex);
                p.f2(gl, "u_view_px", [vw as f32, vh as f32]);
                p.f3(gl, "u_view", u.view);
                p.f1(gl, "u_curv", u.curv);
                p.f1(gl, "u_corner", u.corner);
                p.f1(gl, "u_hsize", u.hsize);
                p.f1(gl, "u_vsize", u.vsize);
                p.f1(gl, "u_hpos", u.hpos);
                p.f1(gl, "u_vpos", u.vpos);
                p.f1(gl, "u_pin", u.pin);
                p.f1(gl, "u_breath", u.breath);
                p.f1(gl, "u_vphase", u.vphase);
                p.f1(gl, "u_field_shift", u.field_shift);
                p.f1(gl, "u_overscan", u.overscan);
                p.f1(gl, "u_ripple", u.ripple);
                p.f1(gl, "u_time", u.time);
                p.f2(gl, "u_conv_r", u.conv_r);
                p.f2(gl, "u_conv_b", u.conv_b);
                p.f1(gl, "u_conv_dyn", u.conv_dyn);
                p.f1(gl, "u_spot", u.spot);
                p.f1(gl, "u_spot_h", u.spot_h);
                p.f1(gl, "u_bloom", u.bloom);
                p.f1(gl, "u_gamma", u.gamma);
                p.f3(gl, "u_focus", u.focus);
                p.f3(gl, "u_emission", u.emission);
                p.f3(gl, "u_cutoff", u.cutoff);
                p.f3(gl, "u_imax", u.imax);
                p.i1(gl, "u_mask", u.mask);
                p.f1(gl, "u_pitch", u.pitch);
                p.f1(gl, "u_bm", u.bm);
                p.f1(gl, "u_mask_strength", u.mask_strength);
                p.f1(gl, "u_anti_moire", u.anti_moire);
                p.f1(gl, "u_wire", u.wire);
                p.f1(gl, "u_purity", u.purity);
                p.f2(gl, "u_purity_seed", u.purity_seed);
                p.f3(gl, "u_degauss", u.degauss);
                p.f1(gl, "u_burn_amt", u.burn_amt);
                p.f1(gl, "u_sat", u.sat);
                p.f4(gl, "u_collapse", u.collapse);
                p.f1(gl, "u_abl", u.abl);
                p.f1(gl, "u_maxlod", self.drive_levels as f32);
                // two field buffers: this field's energy and the previous one's
                let e_next = 1 - view.e_cur;
                self.draw(gl, &view.e[e_next]);
                self.view.as_mut().unwrap().e_cur = e_next;

                if u.scan == 0 {
                    // 7. phosphor afterglow, integrated per field
                    let view = self.view.as_ref().unwrap();
                    let e_tex = view.e[view.e_cur].tex;
                    let cur = view.cur;
                    let nxt = 1 - cur;
                    let p = &mut self.p_tail;
                    gl.use_program(Some(p.id));
                    p.tex(gl, "u_e", 0, e_tex);
                    p.tex(gl, "u_prev", 1, view.tail[cur].tex);
                    p.f3(gl, "u_decay", u.decay);
                    p.f3(gl, "u_frac", u.frac);
                    p.f1(gl, "u_reset", if fi.reset_trails { 1.0 } else { 0.0 });
                    self.draw(gl, &view.tail[nxt]);
                    let tail_tex = view.tail[nxt].tex;
                    let k = if fi.reset_trails { 1.0 } else { u.eye_k };
                    self.eye_and_glass(gl, u, e_tex, tail_tex, u.frac, 1.0, k, vw, vh);
                }
            }

            if u.scan != 0 {
                // 7b. rolling scan: light emitted during this displayed frame only
                let view = self.view.as_ref().unwrap();
                let (cur_e, prev_e) = (view.e[view.e_cur].tex, view.e[1 - view.e_cur].tex);
                let p = &mut self.p_scan;
                gl.use_program(Some(p.id));
                p.tex(gl, "u_cur", 0, cur_e);
                p.tex(gl, "u_prev", 1, prev_e);
                p.f1(gl, "u_phase", u.scan_phase);
                p.f1(gl, "u_w", u.scan_w);
                p.f3(gl, "u_tf", u.tau_fast);
                p.f3(gl, "u_ts", u.tau_slow);
                p.f3(gl, "u_frac", u.frac);
                p.f1(gl, "u_a", u.scan_a);
                self.draw(gl, &view.sc);
                let sc = view.sc.tex;
                let k = if fi.reset_trails { 1.0 } else { u.eye_k_frame };
                self.eye_and_glass(gl, u, sc, sc, [0.0; 3], 0.0, k, vw, vh);
            }

            // 10. composite into egui's framebuffer
            let view = self.view.as_ref().unwrap();
            let cur = view.cur;
            gl.bind_framebuffer(glow::FRAMEBUFFER, None);
            gl.viewport(vx, vy, vw, vh);
            let p = &mut self.p_comp;
            gl.use_program(Some(p.id));
            p.tex(gl, "u_eye", 0, view.eye[cur].tex);
            p.tex(gl, "u_bs", 1, view.q[0].tex);
            p.tex(gl, "u_bw", 2, view.q[2].tex);
            p.f2(gl, "u_view_px", [vw as f32, vh as f32]);
            p.f2(gl, "u_off", [vx as f32, vy as f32]);
            p.f3(gl, "u_view", u.view);
            p.f1(gl, "u_curv", u.curv);
            p.f1(gl, "u_corner", u.corner);
            p.m3(gl, "u_phos", &u.phos);
            p.f1(gl, "u_diffuse", u.diffuse);
            p.f1(gl, "u_halation", u.halation);
            p.f1(gl, "u_ambient", u.ambient);
            p.f1(gl, "u_reflect", u.reflect);
            p.f1(gl, "u_vignette", u.vignette);
            p.f1(gl, "u_exposure", u.exposure);
            p.f3(gl, "u_glass", u.glass);
            p.i1(gl, "u_bezel", u.bezel);
            p.i1(gl, "u_hdr", u.hdr);
            p.f1(gl, "u_hdr_paper", u.hdr_paper_white);
            p.f1(gl, "u_hdr_peak", u.hdr_peak_nits);
            p.u1(gl, "u_frame", u.frame);
            gl.draw_arrays(glow::TRIANGLES, 0, 3);

            gl.bind_vertex_array(None);
            gl.active_texture(glow::TEXTURE0);
            gl.bind_texture(glow::TEXTURE_2D, None);
            gl.use_program(None);
            Ok(())
        }
    }
}
