//! CPU-side simulation of the things a TV does once per line or field:
//! sync separation, horizontal AFC (phase-locked loop), the vertical
//! oscillator, VCR timebase / tracking, power sequencing and degaussing.

use crate::gpu::{FrameInput, NL, Uniforms};
use crate::settings::{Input, Settings, TapeSpeed};

#[derive(Default, Clone, Debug)]
struct ChaosState {
    // Current live drift offsets (only realistic analog signal / sync / tape / supply faults)
    signal_drop_db: f32,
    noise: f32,
    ghost: f32,
    tracking: f32,
    warble: f32,
    h_hold: f32,
    v_hold: f32,
    hum: f32,
    ripple: f32,
    // Target fault offsets
    t_signal_drop_db: f32,
    t_noise: f32,
    t_ghost: f32,
    t_tracking: f32,
    t_warble: f32,
    t_h_hold: f32,
    t_v_hold: f32,
    t_hum: f32,
    t_ripple: f32,
    // Countdown until the next cold-solder / signal glitch
    next_glitch_s: f32,
    whack_streak: u8,
}

impl ChaosState {
    fn clear(&mut self) {
        self.signal_drop_db = 0.0;
        self.noise = 0.0;
        self.ghost = 0.0;
        self.tracking = 0.0;
        self.warble = 0.0;
        self.h_hold = 0.0;
        self.v_hold = 0.0;
        self.hum = 0.0;
        self.ripple = 0.0;
        self.clear_targets();
    }

    fn clear_targets(&mut self) {
        self.t_signal_drop_db = 0.0;
        self.t_noise = 0.0;
        self.t_ghost = 0.0;
        self.t_tracking = 0.0;
        self.t_warble = 0.0;
        self.t_h_hold = 0.0;
        self.t_v_hold = 0.0;
        self.t_hum = 0.0;
        self.t_ripple = 0.0;
    }

    fn scale_all(&mut self, k: f32) {
        self.signal_drop_db *= k;
        self.noise *= k;
        self.ghost *= k;
        self.tracking *= k;
        self.warble *= k;
        self.h_hold *= k;
        self.v_hold *= k;
        self.hum *= k;
        self.ripple *= k;
        self.t_signal_drop_db *= k;
        self.t_noise *= k;
        self.t_ghost *= k;
        self.t_tracking *= k;
        self.t_warble *= k;
        self.t_h_hold *= k;
        self.t_v_hold *= k;
        self.t_hum *= k;
        self.t_ripple *= k;
    }

    fn active_faults(&self) -> Vec<&'static str> {
        let mut f = Vec::new();
        if self.signal_drop_db > 3.0 || self.noise > 0.018 { f.push("weak signal / snow"); }
        if self.ghost.abs() > 0.06 { f.push("multipath ghost"); }
        if self.tracking.abs() > 0.06 { f.push("tracking drift"); }
        if self.warble > 0.08 { f.push("timebase warble"); }
        if self.v_hold.abs() > 0.015 { f.push("V-hold roll"); }
        if self.h_hold.abs() > 0.025 { f.push("H-hold tear"); }
        if self.hum > 0.08 || self.ripple > 0.12 { f.push("hum / supply ripple"); }
        f
    }
}

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
    tuning_static: bool,
    ch_fine_tune_init: f32,
    ch_tracking_init: f32,
    ch_h_pull_init: f32,
    ch_v_pull_init: f32,
    pub whack_t: f32,
    // VHS FF / RW picture-search & seek shuttle state
    shuttle_target: f32, // 0 = normal play, +4/+8 = FF, -4/-8 = RW
    shuttle_vel: f32,    // slewed effective continuous shuttle speed
    shuttle_pulse: f32,  // transient directional burst on single seek or scrub
    shuttle_phase: f32,  // integrated helical noise-bar vertical crawl phase
    chaos: ChaosState,
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
            tuning_static: false,
            ch_fine_tune_init: 0.0,
            ch_tracking_init: 0.0,
            ch_h_pull_init: 0.0,
            ch_v_pull_init: 0.0,
            whack_t: 10.0,
            shuttle_target: 0.0,
            shuttle_vel: 0.0,
            shuttle_pulse: 0.0,
            shuttle_phase: 0.15,
            chaos: ChaosState { next_glitch_s: 8.0, ..Default::default() },
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
        // Classic analog tuner AFT (Automatic Fine Tuning) & sync pull-in:
        // start subtly off-frequency/off-tracking and smoothly drift to zero.
        let sign_ft = if self.next() > 0.5 { 1.0 } else { -1.0 };
        let sign_tr = if self.next() > 0.5 { 1.0 } else { -1.0 };
        let sign_h = if self.next() > 0.5 { 1.0 } else { -1.0 };
        let sign_v = if self.next() > 0.5 { 1.0 } else { -1.0 };
        self.ch_fine_tune_init = sign_ft * (0.26 + 0.20 * self.next());
        self.ch_tracking_init = sign_tr * (0.11 + 0.09 * self.next());
        self.ch_h_pull_init = sign_h * (0.015 + 0.014 * self.next());
        self.ch_v_pull_init = sign_v * (0.004 + 0.004 * self.next());
    }

    /// Holds authentic untuned CRT static snow on screen while a new channel/stream is
    /// still connecting and buffering its first frame, then cleanly pulls into lock on arrival.
    pub fn set_tuning_static(&mut self, active: bool) {
        if self.tuning_static && !active {
            // Transitioning from static placeholder -> first real frame: trigger crisp lock-in
            // with only a subtle vertical offset so sync settles quickly instead of rolling far.
            self.channel_switch_t = 0.04;
            let sign = if self.v_phase >= 0.0 { 1.0 } else { -1.0 };
            self.v_phase = sign * (12.0 + (self.v_phase.abs() * 0.08).min(10.0));
        }
        self.tuning_static = active;
    }

    /// Crisp input relay snap (e.g. RF static -> VIDEO 1 blue screen) with the mechanical
    /// switch click and brief static pop, without AFT tuner detune.
    pub fn snap_input(&mut self) {
        self.channel_switch_t = 0.0;
        self.ch_fine_tune_init = 0.0;
        self.ch_tracking_init = 0.0;
        self.ch_h_pull_init = 0.014;
        self.ch_v_pull_init = 0.008;
    }

    /// Live channel-switch AFT / tracking pull-in offsets: (fine_tune, tracking, h_pull, v_pull).
    fn channel_pull_in(&self) -> (f32, f32, f32, f32) {
        let ct = self.channel_switch_t;
        if !(0.0..2.2).contains(&ct) {
            return (0.0, 0.0, 0.0, 0.0);
        }
        // Damped AFT varactor servo curve: pulls smoothly through zero with a subtle ~6% overshoot then locks.
        let ft = self.ch_fine_tune_init * (-ct / 0.48).exp() * (ct * 2.0).cos();
        let tr = self.ch_tracking_init * (-ct / 0.46).exp();
        let hp = self.ch_h_pull_init * (-ct / 0.26).exp();
        let vp = self.ch_v_pull_init * (-ct / 0.20).exp();
        (
            if ft.abs() < 0.004 { 0.0 } else { ft },
            if tr.abs() < 0.004 { 0.0 } else { tr },
            if hp.abs() < 0.001 { 0.0 } else { hp },
            if vp.abs() < 0.001 { 0.0 } else { vp },
        )
    }

    /// Live channel-switch AFT fine-tune and tracking offsets for synchronized audio DSP.
    pub fn channel_aft_offsets(&self) -> (f32, f32) {
        let (ft, tr, _, _) = self.channel_pull_in();
        (ft, tr)
    }

    /// Sets continuous VCR FF/RW picture-search speed (`0.0` = Play, `±4/±8/±16/±32`).
    pub fn set_vcr_shuttle(&mut self, speed: f32) {
        self.shuttle_target = speed.clamp(-32.0, 32.0);
        if speed.abs() > 0.1 {
            self.shuttle_pulse = 0.0;
        }
    }

    /// Triggers a brief directional VHS helical-scan seek burst (e.g. `-10s`/`+10s` or key seek)
    /// that smoothly pulls back into clean capstan tracking over ~0.4 s.
    pub fn trigger_seek_shuttle(&mut self, delta_secs: f32) {
        if delta_secs.abs() < 0.01 {
            return;
        }
        let dir = delta_secs.signum();
        let mag = if delta_secs.abs() >= 30.0 { 7.5 } else { 4.8 };
        self.shuttle_pulse = dir * mag;
    }

    /// Keeps a gentle directional helical-scan shuttle active while dragging a seek slider.
    pub fn trigger_scrub_shuttle(&mut self, dir: f32) {
        let sign = if dir >= 0.0 { 1.0 } else { -1.0 };
        self.shuttle_pulse = sign * 5.2;
    }

    /// Current effective VHS shuttle state for synchronized audio DSP: `(intensity 0..1, speed_norm 0..1)`.
    pub fn shuttle_audio_state(&self) -> (f32, f32) {
        let v = if self.shuttle_vel.abs() >= self.shuttle_pulse.abs() {
            self.shuttle_vel
        } else {
            self.shuttle_pulse
        };
        let a = v.abs();
        if a <= 0.12 {
            (0.0, 0.0)
        } else {
            let intensity = ((a - 0.12) / 3.5).clamp(0.0, 1.0);
            let speed_norm = ((1.0 + a).ln() / 33.0f32.ln()).clamp(0.0, 1.0);
            (intensity, speed_norm)
        }
    }

    /// Percussive maintenance: slaps the side of the CRT cabinet to knock loose
    /// solder joints and drifting oscillators back into gear!
    pub fn whack(&mut self) -> bool {
        self.whack_t = 0.0;
        self.chaos.whack_streak = self.chaos.whack_streak.saturating_add(1);
        let had_faults = !self.chaos.active_faults().is_empty();
        // 85% chance of an instant clean fix on the first slap; 2nd slap ALWAYS locks 100%.
        let stubborn = had_faults && self.chaos.whack_streak == 1 && self.next() < 0.15;
        if stubborn {
            self.chaos.scale_all(0.32);
            self.chaos.next_glitch_s = self.chaos.next_glitch_s.max(6.0);
            false
        } else {
            self.chaos.clear();
            self.chaos.whack_streak = 0;
            self.chaos.next_glitch_s = 18.0 + self.next() * 20.0;
            true
        }
    }

    pub fn chaos_active_faults(&self) -> Vec<&'static str> {
        self.chaos.active_faults()
    }

    /// Live chaos offsets for synchronized audio DSP (signal_drop_db, noise, ghost, tracking, warble, hum, ripple).
    pub fn chaos_audio_offsets(&self) -> (f32, f32, f32, f32, f32, f32, f32) {
        (
            self.chaos.signal_drop_db,
            self.chaos.noise,
            self.chaos.ghost,
            self.chaos.tracking,
            self.chaos.warble,
            self.chaos.hum,
            self.chaos.ripple,
        )
    }

    fn step_chaos(&mut self, s: &Settings, enabled: bool, dt: f32) {
        if !enabled {
            self.chaos.clear();
            self.chaos.next_glitch_s = 8.0;
            return;
        }
        let rate = s.chaos_rate.clamp(0.1, 2.0);
        // Smoothly drift live values toward their fault targets
        let slew = (dt * 0.65).min(1.0);
        self.chaos.signal_drop_db += (self.chaos.t_signal_drop_db - self.chaos.signal_drop_db) * slew;
        self.chaos.noise += (self.chaos.t_noise - self.chaos.noise) * slew;
        self.chaos.ghost += (self.chaos.t_ghost - self.chaos.ghost) * slew;
        self.chaos.tracking += (self.chaos.t_tracking - self.chaos.tracking) * slew;
        self.chaos.warble += (self.chaos.t_warble - self.chaos.warble) * slew;
        self.chaos.h_hold += (self.chaos.t_h_hold - self.chaos.h_hold) * slew;
        self.chaos.v_hold += (self.chaos.t_v_hold - self.chaos.v_hold) * slew;
        self.chaos.hum += (self.chaos.t_hum - self.chaos.hum) * slew;
        self.chaos.ripple += (self.chaos.t_ripple - self.chaos.ripple) * slew;

        self.chaos.next_glitch_s -= dt * rate;
        if self.chaos.next_glitch_s <= 0.0 {
            self.chaos.whack_streak = 0;
            self.chaos.next_glitch_s = 10.0 + self.next() * 16.0;
            // Cap concurrent faults at 2 so it stays fun/watchable until whacked
            if self.chaos.active_faults().len() >= 2 {
                return;
            }
            let sign = if self.next() > 0.5 { 1.0 } else { -1.0 };
            let pick = (self.next() * 6.0) as u32;
            match pick {
                0 => {
                    // Weak / loose antenna signal strength drop & snow
                    self.chaos.t_signal_drop_db = 14.0 + 14.0 * self.next();
                    self.chaos.t_noise = 0.045 + 0.085 * self.next();
                }
                1 => {
                    // VCR tracking slip (when VCR on) or loose cable noise + slight multipath
                    if s.vcr {
                        self.chaos.t_tracking = sign * (0.18 + 0.24 * self.next());
                    } else if s.input == Input::Rf {
                        self.chaos.t_ghost = sign * (0.18 + 0.22 * self.next());
                        self.chaos.t_signal_drop_db = 8.0 + 10.0 * self.next();
                    } else {
                        self.chaos.t_noise = 0.04 + 0.07 * self.next();
                    }
                }
                2 => {
                    // Timebase warble (tape wow/flutter or unstable sync lines)
                    self.chaos.t_warble = 0.45 + 0.65 * self.next();
                }
                3 => {
                    // Classic vertical hold slip (slow picture roll)
                    self.chaos.t_v_hold = sign * (0.042 + 0.052 * self.next());
                }
                4 => {
                    // Horizontal hold AFC slip (diagonal tearing)
                    self.chaos.t_h_hold = sign * (0.055 + 0.075 * self.next());
                }
                _ => {
                    // Mains hum bar + dried-out capacitor supply ripple warble
                    self.chaos.t_hum = 0.22 + 0.34 * self.next();
                    self.chaos.t_ripple = 0.35 + 0.50 * self.next();
                }
            }
        }
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
            self.warm = (self.warm + fdt / 3.8).min(1.0);
        } else if self.off_t >= 0.0 {
            self.off_t += fdt;
        }
        self.degauss_t += real_dt.max(fdt);
        self.channel_switch_t += real_dt.max(fdt);
        self.whack_t += real_dt.max(fdt);
        self.step_chaos(s, s.chaos && self.power_on, real_dt.max(fdt));

        // Brief ~0.25s RF static burst + ~1.8s AFT fine-tuning & tracking pull-in when changing channels
        let ch_env = if self.channel_switch_t >= 0.0 && self.channel_switch_t < 0.26 {
            let k = 1.0 - self.channel_switch_t / 0.26;
            k * k
        } else {
            0.0
        };
        let (ch_ft, ch_tr, ch_h_pull, ch_v_pull) = self.channel_pull_in();
        let ch_aft_noise = ch_ft.abs() * 0.085;
        // Mechanical cabinet jolt + tiny contact static pop when whacking the TV
        let whack_env = if self.whack_t >= 0.0 && self.whack_t < 0.32 {
            (-self.whack_t * 12.0).exp()
        } else {
            0.0
        };
        let whack_x = whack_env * 0.036 * (self.whack_t * 92.0).sin();
        let whack_y = whack_env * 0.024 * (self.whack_t * 78.0 + 1.1).sin();
        let whack_pop = if self.whack_t >= 0.0 && self.whack_t < 0.07 {
            (1.0 - self.whack_t / 0.07) * 0.22
        } else {
            0.0
        };

        // ---- noise budget
        let eff_signal_db = (s.signal_db - self.chaos.signal_drop_db - ch_ft.abs() * 8.0).max(-25.0);
        let mut rf_sigma = if s.input == Input::Rf { 10f32.powf(-eff_signal_db / 20.0) } else { 0.0 }
            + 0.85 * ch_env
            + ch_aft_noise
            + self.chaos.noise * 1.4
            + whack_pop;
        let mut extra_noise_eff = s.extra_noise + 0.65 * ch_env + ch_aft_noise * 0.75 + self.chaos.noise + whack_pop;
        if self.tuning_static {
            rf_sigma = rf_sigma.max(1.9);
            extra_noise_eff = extra_noise_eff.max(1.2);
        }
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

        // ---- VHS FF / RW helical-scan picture-search & seek shuttle
        let slew_tau = if self.shuttle_target.abs() > self.shuttle_vel.abs() { 0.065 } else { 0.24 };
        let slew_k = 1.0 - (-fdt / slew_tau).exp();
        self.shuttle_vel += (self.shuttle_target - self.shuttle_vel) * slew_k;
        if self.shuttle_target == 0.0 && self.shuttle_vel.abs() < 0.06 {
            self.shuttle_vel = 0.0;
        }
        self.shuttle_pulse *= (-fdt / 0.22).exp();
        if self.shuttle_pulse.abs() < 0.08 {
            self.shuttle_pulse = 0.0;
        }
        let sh_v = if self.shuttle_vel.abs() >= self.shuttle_pulse.abs() {
            self.shuttle_vel
        } else {
            self.shuttle_pulse
        };
        let sh_abs = sh_v.abs();
        let sh_mag = if sh_abs > 0.12 { ((sh_abs - 0.12) / 5.5).clamp(0.0, 1.0) } else { 0.0 };
        let sh_dir = if sh_v >= 0.0 { 1.0 } else { -1.0 };
        if sh_mag > 0.0 {
            // FF crawls upward (-phase), RW crawls downward (+phase); scales smoothly up to 32x
            let crawl = 0.28 + 0.34 * (1.0 + sh_abs).ln();
            self.shuttle_phase = (self.shuttle_phase - sh_dir * crawl * fdt).rem_euclid(1.0);
        }
        // Fewer, cleaner helical-scan noise bars so the picture stays readable:
        // 1 crisp crawling bar for seeks & 4x, 2 bars for 8x-32x high-speed shuttle.
        let sh_bars: usize = if sh_abs > 6.5 { 2 } else { 1 };
        let sh_bar_half = 5.5 + 7.5 * sh_mag;
        let vcr_active = s.vcr || sh_mag > 0.01;

        // ---- VCR tracking band
        let track_err = if s.vcr {
            s.tracking + self.chaos.tracking + ch_tr + 0.04 * s.tape_wear * (t * 0.31).sin()
        } else {
            0.0
        };
        let sev = ((track_err.abs() - 0.05) * 2.6).clamp(0.0, 1.0);
        let band_center = ((0.88 - track_err.abs() * 0.7 + 0.04 * (t * 0.5).sin()).rem_euclid(1.0)) * NL as f32;
        let band_half = 3.0 + 45.0 * track_err.abs();
        let band2 = track_err.abs() > 0.5;

        // ---- horizontal: AFC loop per line
        // free-running error of the line oscillator (a real one is never exactly on frequency)
        let detune = (s.h_hold + self.chaos.h_hold + ch_h_pull) * 14.0 + 0.35;
        let k_afc = s.afc_speed.clamp(0.02, 1.0);
        let max_corr = 9.0;
        let mut err_acc = 0.0;
        let jit = (if s.vcr { s.timebase_jitter } else { 0.0 }) + self.chaos.warble + sh_mag * 0.65;
        let hs_line = 256.0;
        let hs_eff = if s.vcr { s.head_switch.max(sh_mag * 0.85) } else { sh_mag * 0.85 };
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
            // Helical-scan track-crossing snow bars during VHS FF/RW or seeking
            let mut sh_bar_env = 0.0f32;
            if sh_mag > 0.01 {
                for b in 0..sh_bars {
                    let bc = ((self.shuttle_phase + b as f32 / sh_bars as f32).rem_euclid(1.0)) * NL as f32;
                    let mut db = (lf - bc).abs();
                    db = db.min(NL as f32 - db);
                    let flutter = 1.0 + 0.20 * (lf * 0.31 + t * 23.0 + b as f32 * 1.9).sin();
                    let env = (1.0 - (db / (sh_bar_half * flutter))).clamp(0.0, 1.0).powf(0.65);
                    if env > sh_bar_env {
                        sh_bar_env = env;
                    }
                }
                tn = tn.max(sh_bar_env * (0.42 + 0.58 * sh_mag));
            }
            if vcr_active && (lf - hs_line).abs() < 1.5 {
                tn = tn.max(0.55 * hs_eff);
            }
            // where the sync (and the picture) actually arrives on this line
            let mut e = 0.0f32;
            if jit > 0.0 {
                e += jit * (2.5 * (lf * 0.071 + t * 2.3).sin() + 1.2 * (lf * 0.29 + t * 6.1).sin());
                e += jit * 0.6 * self.gauss();
            }
            if vcr_active {
                if lf >= hs_line {
                    e += 16.0 * hs_eff;
                }
                if sh_bar_env > 0.01 {
                    // Coherent helical-scan tearing shear right across each guard-band crossing
                    e += sh_dir * sh_bar_env * 13.0 * sh_mag;
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
            let burst_noise = sigma * 0.9 + if s.vcr { 0.03 * s.tape_wear } else { 0.0 } + sh_mag * 0.14;
            self.lines[i1] = self.gauss() * burst_noise;
            self.lines[i1 + 1] = tn * 6.0;
            self.lines[i1 + 2] = 0.0;
            self.lines[i1 + 3] = 0.0;
        }
        self.h_locked = err_acc / (NL as f32) < 2.5;

        // ---- vertical oscillator
        let half = NL as f32 * 0.5;
        let v_hold_err = s.v_hold + self.chaos.v_hold;
        // the field oscillator free-runs slightly slow; a sync pulse inside the
        // pull-in window triggers the retrace early and locks the picture
        self.v_phase += (v_hold_err + ch_v_pull) * 25.0 + 0.4;
        if self.next() < p_v {
            let w = (self.v_phase + half).rem_euclid(NL as f32) - half;
            if w.abs() < 10.0 {
                self.v_phase = w * 0.05;
            } else if v_hold_err.abs() < 0.05 {
                // With V-hold centered and a clean V-sync pulse present, pull back
                // toward 0 along the shortest wrap-around path instead of drifting
                // slowly all the way around the frame for ~8 seconds.
                let step = (1.5 + w.abs() * 0.035).min(w.abs());
                self.v_phase = w - w.signum() * step;
            }
        }
        self.v_phase = (self.v_phase + half).rem_euclid(NL as f32) - half;
        self.v_locked = self.v_phase.abs() < 0.6;
        let mut vphase = self.v_phase;
        if vcr_active {
            let wear_v = if s.vcr { s.tape_wear } else { 0.0 };
            vphase += self.gauss() * (0.06 * wear_v + 0.22 * sh_mag) + (v_jit / NL as f32) * self.gauss() * 4.0;
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
        let emission_rgb: [f32; 3];
        let warm_size: f32;
        let warm_focus: f32;
        if !self.power_on {
            let ot = self.off_t.max(0.0);
            let sx = (-ot / 0.10).exp().max(0.003);
            let sy = (-ot / 0.025).exp().max(0.003);
            let glow = (-ot / 1.1).exp() * 0.6;
            collapse = [sx, sy, glow, 1.0];
            emission_rgb = [0.0, 0.0, 0.0];
            warm_size = 1.0;
            warm_focus = 0.0;
        } else {
            let w = self.warm;
            // Thermionic emission rises steeply once the cathode heaters glow up,
            // with tiny per-gun thermal-mass stagger that settles to exact 1.0 at full warm-up.
            let warm_curve = |thresh: f32| -> f32 {
                ((w - thresh) / (1.0 - thresh)).clamp(0.0, 1.0).powf(2.4)
            };
            emission_rgb = [warm_curve(0.14), warm_curve(0.165), warm_curve(0.195)];
            // Deflection & HV expand gently into full raster size and focus locks in as EHT stabilizes.
            let sz_t = ((w - 0.12) / 0.63).clamp(0.0, 1.0);
            let sz_s = sz_t * sz_t * (3.0 - 2.0 * sz_t);
            warm_size = 0.94 + 0.06 * sz_s;
            let fc_t = ((w - 0.15) / 0.70).clamp(0.0, 1.0);
            let fc_s = fc_t * fc_t * (3.0 - 2.0 * fc_t);
            warm_focus = (1.0 - fc_s) * 0.55;
        }

        // ---- tube parameters
        let screen_h_mm = s.screen_inches * 25.4 * 0.6;
        let mm = 2.0 / screen_h_mm; // face units per mm
        let ct = colour_temp_gains(s.color_temp);
        let wear = s.wear;
        let emission = [
            ct[0] * (1.0 - 0.65 * wear[0]) * emission_rgb[0],
            ct[1] * (1.0 - 0.65 * wear[1]) * emission_rgb[1],
            ct[2] * (1.0 - 0.65 * wear[2]) * emission_rgb[2],
        ];
        let imax = [
            8.0 * (1.0 - 0.88 * wear[0]).max(0.05) * emission_rgb[0].max(0.02),
            8.0 * (1.0 - 0.88 * wear[1]).max(0.05) * emission_rgb[1].max(0.02),
            8.0 * (1.0 - 0.88 * wear[2]).max(0.05) * emission_rgb[2].max(0.02),
        ];
        let cd = s.cutoff_drift;
        let cutoff = [0.035 * cd, -0.015 * cd, 0.05 * cd];
        let focus = 1.0 + s.focus_loss * 1.4 + warm_focus;
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
            vcr: vcr_active as i32,
            luma_sigma: gs(luma_bw),
            i_sigma: gs(i_bw),
            q_sigma: gs(q_bw),
            chroma_delay: if s.vcr { s.chroma_delay } else { sh_mag * 1.4 },
            vcr_sharp: if s.vcr { s.edge_enhance } else { 0.0 },
            tape_noise: (if s.vcr {
                s.tape_wear
                    * match s.tape_speed {
                        TapeSpeed::Sp => 0.6,
                        TapeSpeed::Lp => 0.9,
                        TapeSpeed::Ep => 1.3,
                    }
            } else {
                0.0
            }) + sh_mag * 0.32,
            chroma_noise: (if s.vcr { s.tape_wear * 0.9 } else { 0.0 }) + sh_mag * 0.42,
            dropout: ((if s.vcr { s.dropouts } else { 0.0 }) + sh_mag * 0.55).min(1.0),
            input: if self.tuning_static { Input::Rf.id() } else { s.input.id() },
            snow: rf_sigma,
            noise: extra_noise_eff,
            ghost_amp: (s.ghost_amp + self.chaos.ghost).clamp(-0.6, 0.6),
            ghost_delay: s.ghost_delay_us * fs,
            interf_amp: s.interference,
            interf_freq: s.interference_freq,
            hum: (s.hum + self.chaos.hum).min(1.0),
            impulse: s.impulse,
            fine_tune: ((if s.input == Input::Rf { s.fine_tune } else { 0.0 }) + ch_ft).clamp(-1.0, 1.0),
            comb: s.comb_filter as i32,
            contrast: s.contrast,
            brightness: s.brightness * 0.3,
            color: s.color,
            tint: s.tint.to_radians(),
            sharp: s.sharpness,
            killer: if self.color_on && !self.tuning_static { 0.0 } else { 1.0 },
            rx_chroma_sigma: gs(1.1),
            view,
            curv: s.curvature,
            corner: s.corner,
            hsize: s.h_size * warm_size,
            vsize: s.v_size * warm_size,
            hpos: s.h_pos * 0.1 + shake + whack_x,
            vpos: s.v_pos * 0.1 + shake * 0.6 + whack_y,
            pin: s.pincushion,
            breath: s.breathing,
            vphase,
            field_shift: if interlaced { 0.5 * field as f32 } else { 0.0 },
            overscan: s.overscan,
            ripple: s.ripple + self.chaos.ripple,
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
            crop_43: false,
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chaos_drifts_and_whack_knocks_back_into_gear() {
        let mut sim = Sim::new();
        let mut s = Settings::default();
        s.chaos = true;
        s.chaos_rate = 2.0;

        // Advance simulated time until at least one fault drifts in
        for _ in 0..600 {
            sim.field(&s, [0.0, 0.0, 1.0], 1.0 / 30.0);
        }
        assert!(!sim.chaos_active_faults().is_empty(), "chaos engine should introduce at least one drift fault");

        // Two whacks always guarantee 100% clean lock
        sim.whack();
        sim.whack();
        assert!(sim.chaos_active_faults().is_empty(), "whacking should knock faults back into gear");
        assert!(sim.whack_t < 0.05);

        // Turning chaos off clears all drift immediately
        for _ in 0..600 {
            sim.field(&s, [0.0, 0.0, 1.0], 1.0 / 30.0);
        }
        s.chaos = false;
        sim.field(&s, [0.0, 0.0, 1.0], 1.0 / 60.0);
        assert!(sim.chaos_active_faults().is_empty());
    }

    #[test]
    fn cold_power_on_fades_in_from_dark_to_full_emission() {
        let mut sim = Sim::new();
        let s = Settings::default();
        assert_eq!(sim.warm, 0.0);

        // First field from a cold tube: cathode heaters below emission threshold -> dark
        let first = sim.field(&s, [0.0, 0.0, 1.0], 1.0 / 60.0);
        assert_eq!(first.u.emission, [0.0, 0.0, 0.0]);
        assert!(first.u.hsize < s.h_size, "cold raster should gently expand into full size");

        // Midway through warm-up (~1.8s): visibly warming up, still below full brightness
        for _ in 0..108 {
            sim.field(&s, [0.0, 0.0, 1.0], 1.0 / 60.0);
        }
        let mid = sim.field(&s, [0.0, 0.0, 1.0], 1.0 / 60.0);
        assert!(mid.u.emission[0] > 0.05 && mid.u.emission[0] < 0.85);

        // After ~4.2s: fully warmed to calibrated 1.0 emission & exact geometry
        for _ in 0..160 {
            sim.field(&s, [0.0, 0.0, 1.0], 1.0 / 60.0);
        }
        let hot = sim.field(&s, [0.0, 0.0, 1.0], 1.0 / 60.0);
        assert!((sim.warm - 1.0).abs() < 1e-5);
        assert!((hot.u.hsize - s.h_size).abs() < 1e-5);
        assert!(hot.u.emission[0] > 0.95 && hot.u.emission[1] > 0.95 && hot.u.emission[2] > 0.95);
    }

    #[test]
    fn channel_switch_drifts_fine_tuning_and_tracking_to_zero() {
        let mut sim = Sim::new();
        sim.warm = 1.0;
        let s = Settings::default();

        sim.switch_channel();
        let (ft0, tr0) = sim.channel_aft_offsets();
        assert!(ft0.abs() > 0.2, "channel switch should start fine tuning off-center: {ft0}");
        assert!(tr0.abs() > 0.1, "channel switch should start tracking off-center: {tr0}");

        let f0 = sim.field(&s, [0.0, 0.0, 1.0], 1.0 / 60.0);
        assert!(f0.u.fine_tune.abs() > 0.2);

        // Midway (~0.45s): pulled noticeably closer to zero
        for _ in 0..27 {
            sim.field(&s, [0.0, 0.0, 1.0], 1.0 / 60.0);
        }
        let (ft_mid, tr_mid) = sim.channel_aft_offsets();
        assert!(ft_mid.abs() < ft0.abs() * 0.6);
        assert!(tr_mid.abs() < tr0.abs() * 0.6);

        // After ~2.4s: cleanly locked at exact 0.0
        for _ in 0..120 {
            sim.field(&s, [0.0, 0.0, 1.0], 1.0 / 60.0);
        }
        let (ft_end, tr_end) = sim.channel_aft_offsets();
        assert_eq!(ft_end, 0.0);
        assert_eq!(tr_end, 0.0);
        let f_end = sim.field(&s, [0.0, 0.0, 1.0], 1.0 / 60.0);
        assert_eq!(f_end.u.fine_tune, 0.0);
    }

    #[test]
    fn channel_switch_vertical_sync_pulls_short_way_and_locks_quickly() {
        let mut sim = Sim::new();
        sim.warm = 1.0;
        let s = Settings::default();

        // Simulate 1.2s of tuning static while connecting to a stream
        sim.switch_channel();
        sim.set_tuning_static(true);
        for _ in 0..72 {
            sim.field(&s, [0.0, 0.0, 1.0], 1.0 / 60.0);
        }
        // First real frame arrives: offset should be modest and lock within 0.5s (30 fields)
        // moving strictly toward 0 along the shortest path, never cycling all the way around.
        sim.set_tuning_static(false);
        assert!(sim.v_phase.abs() <= 23.0, "initial v_phase offset should be modest: {}", sim.v_phase);
        let mut locked = false;
        for _ in 0..30 {
            sim.field(&s, [0.0, 0.0, 1.0], 1.0 / 60.0);
            assert!(sim.v_phase.abs() < 30.0, "v_phase should pull short way, not wrap around: {}", sim.v_phase);
            if sim.v_locked {
                locked = true;
                break;
            }
        }
        assert!(locked, "vertical sync should lock cleanly within 0.5s after static clears");
    }

    #[test]
    fn vhs_shuttle_creates_helical_tracking_bars_even_when_vcr_off_and_settles_cleanly() {
        let mut sim = Sim::new();
        sim.warm = 1.0;
        let mut s = Settings::default();
        s.vcr = false; // broadcast profile — shuttle should still engage helical bars during seek/FF/RW

        // Idle broadcast: no tracking noise and vcr flag off
        let idle = sim.field(&s, [0.0, 0.0, 1.0], 1.0 / 60.0);
        assert_eq!(idle.u.vcr, 0);
        let idle_max_tn = idle.lines.chunks_exact(4).take(NL as usize).map(|c| c[1]).fold(0.0f32, f32::max);
        assert_eq!(idle_max_tn, 0.0);

        // Trigger a +10s FF seek burst
        sim.trigger_seek_shuttle(10.0);
        let (sh_amt, sh_spd) = sim.shuttle_audio_state();
        assert!(sh_amt > 0.5 && sh_spd > 0.4);
        let ff = sim.field(&s, [0.0, 0.0, 1.0], 1.0 / 60.0);
        assert_eq!(ff.u.vcr, 1, "shuttle should engage helical tracking pass while seeking");
        let ff_max_tn = ff.lines.chunks_exact(4).take(NL as usize).map(|c| c[1]).fold(0.0f32, f32::max);
        assert!(ff_max_tn > 0.5, "expected helical noise bars during FF seek, got {ff_max_tn}");

        // After ~1.5s, pulse should decay completely back to clean broadcast
        for _ in 0..90 {
            sim.field(&s, [0.0, 0.0, 1.0], 1.0 / 60.0);
        }
        let settled = sim.field(&s, [0.0, 0.0, 1.0], 1.0 / 60.0);
        assert_eq!(settled.u.vcr, 0);
        assert_eq!(sim.shuttle_audio_state(), (0.0, 0.0));
    }
}
