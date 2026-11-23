//! CPU-side simulation of the things a TV does once per line or field:
//! sync separation, horizontal AFC (phase-locked loop), the vertical
//! oscillator, VCR timebase / tracking, power sequencing and degaussing.

use crate::gpu::{FrameInput, NL, Uniforms};
use crate::settings::{Input, Settings, TapeSpeed};

pub struct Sim {
    pub fields: u64,
    acc: f64,
    pub time: f64,
    h_phase: f32,
    v_phase: f32,
    rng: u64,
    pub h_locked: bool,
    pub v_locked: bool,
    pub color_on: bool,
    pub snr_db: f32,
    pub power_on: bool,
    pub warm: f32,
    pub off_t: f32,
    pub degauss_t: f32,
    degauss_phase: f32,
    pub channel_switch_t: f32,
    lines: Vec<f32>,
    pub purity_seed: [f32; 2],
}

impl Sim {
    pub fn new() -> Self {
        Sim {
            fields: 0,
            acc: 0.0,
            time: 0.0,
            h_phase: 0.0,
            v_phase: 0.0,
            rng: 0x2545_f491_4f6c_dd1d,
            h_locked: true,
            v_locked: true,
            color_on: true,
            snr_db: 60.0,
            power_on: true,
            warm: 0.0,
            off_t: -1.0,
            degauss_t: 0.0,
            degauss_phase: 0.7,
            channel_switch_t: 10.0,
            lines: vec![0.0; (NL * 2 * 4) as usize],
            purity_seed: [3.7, 11.2],
        }
    }

    fn next(&mut self) -> f32 {
        self.rng ^= self.rng << 13;
        self.rng ^= self.rng >> 7;
        self.rng ^= self.rng << 17;
        ((self.rng >> 40) as f32) / ((1u64 << 24) as f32)
    }
    fn gauss(&mut self) -> f32 {
        let u1 = self.next().max(1e-7);
        let u2 = self.next();
        (-2.0 * u1.ln()).sqrt() * (std::f32::consts::TAU * u2).cos()
    }

    pub fn set_power(&mut self, on: bool) {
        if on == self.power_on {
            return;
        }
        self.power_on = on;
        if on {
            self.off_t = -1.0;
            self.warm = 0.0;
            self.degauss_t = 0.0; // sets auto-degauss when switched on
            self.degauss_phase = self.next() * std::f32::consts::TAU;
        } else {
            self.off_t = 0.0;
        }
    }

    pub fn degauss(&mut self) {
        self.degauss_t = 0.0;
        self.degauss_phase = self.next() * std::f32::consts::TAU;
    }

    pub fn switch_channel(&mut self) {
        self.channel_switch_t = 0.0;
    }

    /// Advances simulated time; returns true when a new field (1/59.94 s) is due.
    pub fn advance(&mut self, dt: f64) -> bool {
        let dt = dt.clamp(0.0, 0.25);
        self.time += dt;
        self.acc += dt;
        let fdt = 1.0 / 59.94;
        if self.acc >= fdt * 0.98 {
            self.acc = (self.acc - fdt).max(0.0).min(fdt);
            true
        } else {
            false
        }
    }

    /// Runs the per-line/per-field model and builds everything the GPU needs.
    pub fn field(&mut self, s: &Settings, view: [f32; 3], real_dt: f32) -> FrameInput {
        self.fields += 1;
        let fdt = 1.0 / 59.94f32;
        let t = self.time as f32;

        // ---- power & degauss
        if self.power_on {
            self.warm = (self.warm + fdt / 3.0).min(1.0);
        } else if self.off_t >= 0.0 {
            self.off_t += fdt;
        }
        self.degauss_t += real_dt.max(fdt);
        self.channel_switch_t += real_dt.max(fdt);

        // Brief ~0.25s RF static burst + tuner sync hiccup when changing cable channels
        let ch_env = if self.channel_switch_t >= 0.0 && self.channel_switch_t < 0.26 {
            let k = 1.0 - self.channel_switch_t / 0.26;
            k * k
        } else {
            0.0
        };

        // ---- noise budget
        let rf_sigma = if s.input == Input::Rf { 10f32.powf(-s.signal_db / 20.0) } else { 0.0 } + 0.85 * ch_env;
        let extra_noise_eff = s.extra_noise + 0.65 * ch_env;
        let (tape_sigma, speed_bw) = if s.vcr {
            let k = match s.tape_speed {
                TapeSpeed::Sp => (1.0, 3.0),
                TapeSpeed::Lp => (1.4, 2.5),
                TapeSpeed::Ep => (1.9, 2.0),
            };
            (s.tape_wear * 0.08 * k.0 + 0.012, k.1)
        } else {
            (0.0, 4.2)
        };
        let sigma = (rf_sigma * rf_sigma + extra_noise_eff * extra_noise_eff + tape_sigma * tape_sigma).sqrt();
        self.snr_db = if sigma > 1e-5 { (-20.0 * sigma.log10()).min(60.0) } else { 60.0 };

        // probability that the sync separator finds a line's sync pulse
        let p_h = (1.6 - sigma * 4.5).clamp(0.0, 1.0);
        // vertical sync is integrated over several lines, so it survives more noise
        let p_v = (2.2 - sigma * 5.0).clamp(0.0, 1.0);

        // colour killer: burst too noisy -> colour off (with hysteresis)
        if s.color_killer && s.input != Input::Component {
            if self.color_on && sigma > 0.24 {
                self.color_on = false;
            } else if !self.color_on && sigma < 0.19 {
                self.color_on = true;
            }
        } else {
            self.color_on = true;
        }

        // ---- VCR tracking band
        let track_err = if s.vcr {
            s.tracking + 0.04 * s.tape_wear * (t * 0.31).sin()
        } else {
            0.0
        };
        let sev = ((track_err.abs() - 0.05) * 2.6).clamp(0.0, 1.0);
        let band_center = ((0.88 - track_err.abs() * 0.7 + 0.04 * (t * 0.5).sin()).rem_euclid(1.0)) * NL as f32;
        let band_half = 3.0 + 45.0 * track_err.abs();
        let band2 = track_err.abs() > 0.5;

        // ---- horizontal: AFC loop per line
        // free-running error of the line oscillator (a real one is never exactly on frequency)
        let detune = s.h_hold * 14.0 + 0.35;
        let k_afc = s.afc_speed.clamp(0.02, 1.0);
        let max_corr = 9.0;
        let mut err_acc = 0.0;
        let jit = if s.vcr { s.timebase_jitter } else { 0.0 };
        let hs_line = 256.0;
        let mut v_jit = 0.0;
        for l in 0..NL as usize {
            let lf = l as f32;
            // tracking noise amount for this line
            let mut tn = 0.0f32;
            if s.vcr && sev > 0.0 {
                let mut d = (lf - band_center).abs();
                d = d.min(NL as f32 - d);
                tn = (1.0 - (d / band_half)).clamp(0.0, 1.0).powf(0.7) * sev;
                if band2 {
                    let c2 = (band_center + NL as f32 * 0.45).rem_euclid(NL as f32);
                    let mut d2 = (lf - c2).abs();
                    d2 = d2.min(NL as f32 - d2);
                    tn = tn.max((1.0 - d2 / (band_half * 0.6)).clamp(0.0, 1.0) * sev);
                }
            }
            if s.vcr && (lf - hs_line).abs() < 1.5 {
                tn = tn.max(0.55 * s.head_switch);
            }
            // where the sync (and the picture) actually arrives on this line
            let mut e = 0.0f32;
            if s.vcr {
                e += jit * (2.5 * (lf * 0.071 + t * 2.3).sin() + 1.2 * (lf * 0.29 + t * 6.1).sin());
                e += jit * 0.6 * self.gauss();
                if lf >= hs_line {
                    e += 16.0 * s.head_switch;
                }
                e += tn * 30.0 * self.gauss();
            }
            self.h_phase += detune;
            let detected = self.next() < p_h * (1.0 - 0.85 * tn);
            if detected {
                let meas = e + self.gauss() * sigma * 22.0;
                let mut err = meas - self.h_phase;
                err = (err + 455.0).rem_euclid(910.0) - 455.0;
                self.h_phase += (err * k_afc).clamp(-max_corr, max_corr);
            }
            self.h_phase = (self.h_phase + 455.0).rem_euclid(910.0) - 455.0;
            let off = self.h_phase - e;
            err_acc += off.abs();
            v_jit += tn;

            let i0 = l * 4;
            self.lines[i0] = off;
            self.lines[i0 + 1] = tn;
            self.lines[i0 + 2] = 0.0;
            self.lines[i0 + 3] = 1.0;
            let i1 = (NL as usize + l) * 4;
            // burst phase noise (degrees of hue wobble per line)
            let burst_noise = sigma * 0.9 + if s.vcr { 0.03 * s.tape_wear } else { 0.0 };
            self.lines[i1] = self.gauss() * burst_noise;
            self.lines[i1 + 1] = tn * 6.0;
            self.lines[i1 + 2] = 0.0;
            self.lines[i1 + 3] = 0.0;
        }
        self.h_locked = err_acc / (NL as f32) < 2.5;

        // ---- vertical oscillator
        let half = NL as f32 * 0.5;
        // the field oscillator free-runs slightly slow; a sync pulse inside the
        // pull-in window triggers the retrace early and locks the picture
        self.v_phase += s.v_hold * 25.0 + 0.4;
        if self.next() < p_v {
            let w = (self.v_phase + half).rem_euclid(NL as f32) - half;
            if w.abs() < 10.0 {
                self.v_phase = w * 0.05;
            }
        }
        self.v_phase = (self.v_phase + half).rem_euclid(NL as f32) - half;
        self.v_locked = self.v_phase.abs() < 0.6;
        let mut vphase = self.v_phase;
        if s.vcr {
            vphase += self.gauss() * 0.06 * s.tape_wear + (v_jit / NL as f32) * self.gauss() * 4.0;
        }

        // ---- degauss coil (AC perimeter coil driven through a PTC thermistor)
        // Cold PTC passes a strong initial current surge, then heats past its
        // Curie point so resistance rises steeply and the field collapses cleanly.
        let dg_t = self.degauss_t;
        let degauss_amp = if dg_t >= 0.0 && dg_t < 2.6 {
            let rise = (dg_t / 0.025).min(1.0);
            let ptc = 1.0 / (1.0 + (dg_t / 0.36).powf(2.6));
            let tail = (-dg_t / 1.15).exp();
            rise * ptc * tail
        } else {
            0.0
        };
        let shake = degauss_amp * 0.004 * (dg_t * std::f32::consts::TAU * 13.0 + self.degauss_phase).sin();

        // ---- power sequencing
        let mut collapse = [1.0, 1.0, 0.0, 0.0];
        let emission_scale;
        if !self.power_on {
            let ot = self.off_t.max(0.0);
            let sx = (-ot / 0.10).exp().max(0.003);
            let sy = (-ot / 0.025).exp().max(0.003);
            let glow = (-ot / 1.1).exp() * 0.6;
            collapse = [sx, sy, glow, 1.0];
            emission_scale = 0.0;
        } else {
            let w = self.warm;
            // thermionic emission rises steeply once the heater is warm
            emission_scale = ((w - 0.15) / 0.85).clamp(0.0, 1.0).powf(2.2);
        }

        // ---- tube parameters
        let screen_h_mm = s.screen_inches * 25.4 * 0.6;
        let mm = 2.0 / screen_h_mm; // face units per mm
        let ct = colour_temp_gains(s.color_temp);
        let wear = s.wear;
        let emission = [
            ct[0] * (1.0 - 0.65 * wear[0]) * emission_scale,
            ct[1] * (1.0 - 0.65 * wear[1]) * emission_scale,
            ct[2] * (1.0 - 0.65 * wear[2]) * emission_scale,
        ];
        let imax = [
            8.0 * (1.0 - 0.88 * wear[0]).max(0.05) * emission_scale.max(0.02),
            8.0 * (1.0 - 0.88 * wear[1]).max(0.05) * emission_scale.max(0.02),
            8.0 * (1.0 - 0.88 * wear[2]).max(0.05) * emission_scale.max(0.02),
        ];
        let cd = s.cutoff_drift;
        let cutoff = [0.035 * cd, -0.015 * cd, 0.05 * cd];
        let focus = 1.0 + s.focus_loss * 1.4;
        let focus3 = [focus * 1.03, focus, focus * 1.06];

        let decay_t = (s.persistence_ms.max(0.1)) / 1000.0;
        // per-phosphor afterglow: the green and blue sulphide phosphors of P22 have the longest tails
        let tau = [decay_t * 0.6, decay_t * 1.25, decay_t * 1.0];
        let decay = [(-fdt / tau[0]).exp(), (-fdt / tau[1]).exp(), (-fdt / tau[2]).exp()];
        let pf = s.persistence.clamp(0.0, 0.95);
        let frac = [pf * 0.7, pf * 1.1, pf * 0.95].map(|v: f32| v.min(0.97));
        let eye_k = if s.eye_ms <= 0.5 { 1.0 } else { 1.0 - (-(fdt * 1000.0) / s.eye_ms).exp() };

        let px_per_face = 1.0 / (2.0 * view[2]); // multiplied by view height in app
        let _ = px_per_face;

        let glass = {
            let y = s.glass_yellow;
            [1.0 - 0.04 * y, 1.0 - 0.09 * y, 1.0 - 0.30 * y]
        };

        let interlaced = s.interlaced;
        let field = if interlaced { (self.fields & 1) as i32 } else { 0 };
        let field_phase = if interlaced { ((self.fields as f64 * 0.75) % 1.0) as f32 } else { 0.0 };

        let fs = Settings::samples_per_us();
        let gs = |mhz: f32| 0.1325 * fs / mhz.max(0.05);
        let (luma_bw, i_bw, q_bw) = if s.vcr { (speed_bw, 0.42, 0.42) } else { (4.2, 1.3, 0.6) };

        let u = Uniforms {
            time: t,
            frame: (self.fields & 0xffff_ffff) as u32,
            pattern: s.pattern.id(),
            flat: s.flat_color,
            src_w: if interlaced { 640 } else { 320 },
            src_h: if interlaced { 480 } else { 240 },
            field,
            interlaced: interlaced as i32,
            field_phase,
            vcr: s.vcr as i32,
            luma_sigma: gs(luma_bw),
            i_sigma: gs(i_bw),
            q_sigma: gs(q_bw),
            chroma_delay: if s.vcr { s.chroma_delay } else { 0.0 },
            vcr_sharp: s.edge_enhance,
            tape_noise: s.tape_wear
                * match s.tape_speed {
                    TapeSpeed::Sp => 0.6,
                    TapeSpeed::Lp => 0.9,
                    TapeSpeed::Ep => 1.3,
                },
            chroma_noise: s.tape_wear * 0.9,
            dropout: s.dropouts,
            input: s.input.id(),
            snow: rf_sigma,
            noise: extra_noise_eff,
            ghost_amp: s.ghost_amp,
            ghost_delay: s.ghost_delay_us * fs,
            interf_amp: s.interference,
            interf_freq: s.interference_freq,
            hum: s.hum,
            impulse: s.impulse,
            fine_tune: s.fine_tune,
            comb: s.comb_filter as i32,
            contrast: s.contrast,
            brightness: s.brightness * 0.3,
            color: s.color,
            tint: s.tint.to_radians(),
            sharp: s.sharpness,
            killer: if self.color_on { 0.0 } else { 1.0 },
            rx_chroma_sigma: gs(1.1),
            view,
            curv: s.curvature,
            corner: s.corner,
            hsize: s.h_size,
            vsize: s.v_size,
            hpos: s.h_pos * 0.1 + shake,
            vpos: s.v_pos * 0.1 + shake * 0.6,
            pin: s.pincushion,
            breath: s.breathing,
            vphase,
            field_shift: if interlaced { 0.5 * field as f32 } else { 0.0 },
            overscan: s.overscan,
            ripple: s.ripple,
            conv_r: [s.conv_r[0] * mm / 0.004, s.conv_r[1] * mm / 0.004],
            conv_b: [s.conv_b[0] * mm / 0.004, s.conv_b[1] * mm / 0.004],
            conv_dyn: s.conv_dyn * mm / 0.004 / 1.77,
            spot: s.spot,
            spot_h: s.spot_h,
            bloom: s.bloom,
            gamma: s.gamma,
            focus: focus3,
            emission,
            cutoff,
            imax,
            mask: s.mask.id(),
            pitch: s.pitch_mm * mm,
            bm: s.black_matrix,
            mask_strength: s.mask_strength,
            anti_moire: s.anti_moire,
            wire: 0.15 * mm,
            purity: s.magnetization,
            purity_seed: self.purity_seed,
            degauss: [degauss_amp, dg_t, self.degauss_phase],
            burn_amt: s.burn_visible,
            burn_rate: s.burn_speed * s.burn_speed * 0.002,
            sat: s.phosphor_sat,
            collapse,
            abl: s.abl,
            decay,
            frac,
            eye_k,
            scan: 0,
            scan_phase: 0.0,
            scan_w: 1.0,
            // fast decay per phosphor, in fields: the europium red is slowest, the silver-activated blue fastest
            tau_fast: [s.decay_ms * 3.0, s.decay_ms, s.decay_ms * 0.3].map(|ms| (ms / 1000.0 * 59.94).max(1e-5)),
            tau_slow: tau.map(|t| t * 59.94),
            scan_a: 1.0,
            eye_k_frame: eye_k,
            phos: s.phosphor.matrix(),
            diffuse: s.diffusion,
            halation: s.halation,
            diffuse_sigma_px: 0.0,
            halation_sigma_px: 0.0,
            ambient: s.ambient,
            reflect: s.reflection,
            vignette: s.vignette,
            exposure: s.exposure,
            glass,
            bezel: s.show_bezel as i32,
            hdr_paper_white: s.hdr_paper_white,
            hdr_peak_nits: s.hdr_peak_nits,
            hdr_ui_nits: s.hdr_ui_nits,
        };

        FrameInput {
            new_field: true,
            lines: self.lines.clone(),
            u,
            image: None,
            osd: None,
            clear_burn: false,
            burn_now: false,
            reset_trails: false,
        }
    }
}

/// Approximate per-gun drive gains for a white point at the given colour temperature
/// (normalised so 6500 K is neutral).
pub fn colour_temp_gains(k: f32) -> [f32; 3] {
    fn planck_rgb(t: f32) -> [f32; 3] {
        // Tanner Helland style fit, returns linear-ish sRGB.
        let t = t / 100.0;
        let r = if t <= 66.0 { 1.0 } else { (1.292_936_2 * (t - 60.0).powf(-0.133_204_76)).clamp(0.0, 1.0) };
        let g = if t <= 66.0 {
            (0.390_081_58 * t.ln() - 0.631_841_4).clamp(0.0, 1.0)
        } else {
            (1.129_890_9 * (t - 60.0).powf(-0.075_514_85)).clamp(0.0, 1.0)
        };
        let b = if t >= 66.0 {
            1.0
        } else if t <= 19.0 {
            0.0
        } else {
            (0.543_206_8 * (t - 10.0).ln() - 1.196_254_1).clamp(0.0, 1.0)
        };
        [r.powf(2.2), g.powf(2.2), b.powf(2.2)]
    }
    let a = planck_rgb(k);
    let n = planck_rgb(6500.0);
    let g = [a[0] / n[0].max(1e-3), a[1] / n[1].max(1e-3), a[2] / n[2].max(1e-3)];
    let m = g[0].max(g[1]).max(g[2]);
    [g[0] / m, g[1] / m, g[2] / m]
}
