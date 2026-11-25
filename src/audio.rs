//! The TV's sound path, driven by the same knobs as the picture:
//! source audio (from mpv) -> VCR linear audio track -> RF FM sound carrier
//! (hiss, threshold clicks, static, mistuning, buzz, hum) -> TV amplifier and
//! cabinet speaker -> flyback whine, degauss buzz, power crackle -> output.
//!
//! Output goes to the first sound player found (pacat / pw-cat / aplay / mpv),
//! fed raw 48 kHz stereo 16-bit samples over a pipe.

use std::collections::VecDeque;
use std::io::Write;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

pub const RATE: f32 = 48000.0;
const BLOCK: usize = 256;

/// Ring buffer that mpv's decoded audio is written into (interleaved stereo f32).
pub struct AudioRing {
    buf: Mutex<VecDeque<f32>>,
    cap: usize,
    session: AtomicU64,
}

impl AudioRing {
    pub fn new() -> Arc<Self> {
        // 45 ms stereo ring (4,320 floats = 2,160 frames): generous enough to avoid
        // startup/HTTP underruns while keeping DSP lip-sync latency tight.
        let cap = (RATE * 0.045) as usize * 2;
        Arc::new(AudioRing {
            buf: Mutex::new(VecDeque::with_capacity(cap + 1024)),
            cap,
            session: AtomicU64::new(0),
        })
    }
    /// Claims exclusive ownership of the ring for a new player instance and clears
    /// any leftover samples from the previous player so teardown can never wipe
    /// the new stream's audio.
    pub fn begin_session(&self) -> u64 {
        let id = self.session.fetch_add(1, Ordering::SeqCst) + 1;
        self.buf.lock().unwrap().clear();
        id
    }
    /// Blocks while the ring is full (this is what paces mpv to real time).
    #[cfg(test)]
    pub fn push_s16(&self, bytes: &[u8], stop: &AtomicBool) {
        let cur = self.session.load(Ordering::Relaxed);
        self.push_s16_session(cur, bytes, stop);
    }
    /// Like `push_s16`, but aborts immediately if a newer player session has started.
    pub fn push_s16_session(&self, session: u64, bytes: &[u8], stop: &AtomicBool) {
        let mut i = 0;
        while i + 1 < bytes.len() {
            loop {
                if stop.load(Ordering::Relaxed) || self.session.load(Ordering::Relaxed) != session {
                    return;
                }
                if self.buf.lock().unwrap().len() < self.cap {
                    break;
                }
                std::thread::sleep(Duration::from_millis(1));
            }
            let mut b = self.buf.lock().unwrap();
            if self.session.load(Ordering::Relaxed) != session {
                return;
            }
            while i + 1 < bytes.len() && b.len() < self.cap + 512 {
                b.push_back(i16::from_le_bytes([bytes[i], bytes[i + 1]]) as f32 / 32768.0);
                i += 2;
            }
        }
    }
    #[allow(dead_code)]
    pub fn len(&self) -> usize {
        self.buf.lock().unwrap().len()
    }
    pub fn clear_session(&self, session: u64) {
        let mut b = self.buf.lock().unwrap();
        if self.session.load(Ordering::Relaxed) == session {
            b.clear();
        }
    }
    fn pop_frame(&self, b: &mut std::sync::MutexGuard<'_, VecDeque<f32>>) -> Option<(f32, f32)> {
        if b.len() >= 2 {
            let l = b.pop_front().unwrap();
            let r = b.pop_front().unwrap();
            Some((l, r))
        } else {
            None
        }
    }
}

#[derive(Clone, Debug)]
pub struct AudioParams {
    pub enabled: bool,
    pub volume: f32,
    pub tv_speaker: bool,
    pub mono: bool,
    pub screen_inches: f32,
    pub whine: f32,
    pub power_on: bool,
    pub warm: f32,
    pub degauss_t: f32,
    pub channel_switch_t: f32,
    pub whack_t: f32,
    pub test_tone: bool,
    // RF
    pub rf: bool,
    pub snr_db: f32,
    pub fine_tune: f32,
    pub ghost: f32,
    pub hum: f32,
    pub impulse: f32,
    pub extra_noise: f32,
    // VCR
    pub vcr: bool,
    pub tape_bw_khz: f32,
    pub tape_wear: f32,
    pub tracking: f32,
    pub dropouts: f32,
    pub wow: f32,
    pub head_switch: f32,
    // aging
    pub age: f32,
    pub ripple: f32,
}

impl Default for AudioParams {
    fn default() -> Self {
        AudioParams {
            enabled: true,
            volume: 0.5,
            tv_speaker: true,
            mono: true,
            screen_inches: 14.0,
            whine: 0.2,
            power_on: true,
            warm: 1.0,
            degauss_t: 10.0,
            channel_switch_t: 10.0,
            whack_t: 10.0,
            test_tone: false,
            rf: false,
            snr_db: 60.0,
            fine_tune: 0.0,
            ghost: 0.0,
            hum: 0.0,
            impulse: 0.0,
            extra_noise: 0.0,
            vcr: false,
            tape_bw_khz: 10.0,
            tape_wear: 0.0,
            tracking: 0.0,
            dropouts: 0.0,
            wow: 0.0,
            head_switch: 0.0,
            age: 0.0,
            ripple: 0.0,
        }
    }
}

// ------------------------------------------------------------------ DSP blocks

#[derive(Clone, Copy, Default)]
struct Biquad {
    b0: f32,
    b1: f32,
    b2: f32,
    a1: f32,
    a2: f32,
    z1: f32,
    z2: f32,
}

impl Biquad {
    fn set(&mut self, kind: u8, f: f32, q: f32, gain_db: f32) {
        let f = f.clamp(10.0, RATE * 0.45);
        let w = std::f32::consts::TAU * f / RATE;
        let (sn, cs) = (w.sin(), w.cos());
        let alpha = sn / (2.0 * q);
        let a = 10f32.powf(gain_db / 40.0);
        let (b0, b1, b2, a0, a1, a2) = match kind {
            0 => ((1.0 - cs) / 2.0, 1.0 - cs, (1.0 - cs) / 2.0, 1.0 + alpha, -2.0 * cs, 1.0 - alpha), // low-pass
            1 => ((1.0 + cs) / 2.0, -(1.0 + cs), (1.0 + cs) / 2.0, 1.0 + alpha, -2.0 * cs, 1.0 - alpha), // high-pass
            _ => (1.0 + alpha * a, -2.0 * cs, 1.0 - alpha * a, 1.0 + alpha / a, -2.0 * cs, 1.0 - alpha / a), // peak
        };
        self.b0 = b0 / a0;
        self.b1 = b1 / a0;
        self.b2 = b2 / a0;
        self.a1 = a1 / a0;
        self.a2 = a2 / a0;
    }
    #[inline]
    fn run(&mut self, x: f32) -> f32 {
        let y = self.b0 * x + self.z1;
        self.z1 = self.b1 * x - self.a1 * y + self.z2;
        self.z2 = self.b2 * x - self.a2 * y;
        y
    }
}

struct OnePole {
    a: f32,
    y: f32,
}
impl OnePole {
    fn new(fc: f32) -> Self {
        OnePole { a: 1.0 - (-std::f32::consts::TAU * fc / RATE).exp(), y: 0.0 }
    }
    #[inline]
    fn run(&mut self, x: f32) -> f32 {
        self.y += self.a * (x - self.y);
        self.y
    }
}

struct Processor {
    t: f64,
    rng: u64,
    // VCR
    delay: Vec<f32>,
    dpos: usize,
    tape_lp: [Biquad; 2],
    tape_hp: Biquad,
    hiss_hp: Biquad,
    dropout_env: f32,
    dropout_left: usize,
    // RF
    deemph: OnePole,
    static_lp: OnePole,
    click_env: f32,
    click_sign: f32,
    buzz_lp: OnePole,
    // speaker
    spk_hp: [Biquad; 2],
    spk_peak: Biquad,
    spk_lp: [Biquad; 2],
    cfg_key: (i32, i32, i32),
    // power, degauss & electrostatic crackle
    inited: bool,
    gain: f32,
    was_on: bool,
    last_degauss_t: f32,
    crackle: f32,
    spark_env_l: f32,
    spark_env_r: f32,
    spark_decay: f32,
    crackle_hp_l: Biquad,
    crackle_hp_r: Biquad,
}

impl Processor {
    fn new() -> Self {
        let mut crackle_hp_l = Biquad::default();
        let mut crackle_hp_r = Biquad::default();
        crackle_hp_l.set(1, 3800.0, 0.8, 0.0);
        crackle_hp_r.set(1, 3800.0, 0.8, 0.0);
        Processor {
            t: 0.0,
            rng: 0x9e37_79b9_7f4a_7c15,
            delay: vec![0.0; 4096],
            dpos: 0,
            tape_lp: [Biquad::default(); 2],
            tape_hp: Biquad::default(),
            hiss_hp: Biquad::default(),
            dropout_env: 1.0,
            dropout_left: 0,
            deemph: OnePole::new(2122.0), // 75 µs de-emphasis
            static_lp: OnePole::new(6000.0),
            click_env: 0.0,
            click_sign: 1.0,
            buzz_lp: OnePole::new(3000.0),
            spk_hp: [Biquad::default(); 2],
            spk_peak: Biquad::default(),
            spk_lp: [Biquad::default(); 2],
            cfg_key: (-1, -1, -1),
            inited: false,
            gain: 0.0,
            was_on: false,
            last_degauss_t: 10.0,
            crackle: 0.0,
            spark_env_l: 0.0,
            spark_env_r: 0.0,
            spark_decay: 0.94,
            crackle_hp_l,
            crackle_hp_r,
        }
    }

    #[inline]
    fn rnd(&mut self) -> f32 {
        self.rng ^= self.rng << 13;
        self.rng ^= self.rng >> 7;
        self.rng ^= self.rng << 17;
        ((self.rng >> 40) as f32) / ((1u64 << 24) as f32)
    }
    #[inline]
    fn noise(&mut self) -> f32 {
        // approximately gaussian (sum of uniforms)
        (self.rnd() + self.rnd() + self.rnd() + self.rnd() - 2.0) * 1.2
    }

    fn configure(&mut self, p: &AudioParams) {
        let key = ((p.tape_bw_khz * 10.0) as i32, p.screen_inches as i32, p.tv_speaker as i32);
        if key == self.cfg_key {
            return;
        }
        self.cfg_key = key;
        let bw = p.tape_bw_khz * 1000.0;
        self.tape_lp[0].set(0, bw, 0.54, 0.0);
        self.tape_lp[1].set(0, bw, 1.31, 0.0);
        self.tape_hp.set(1, 90.0, 0.7, 0.0);
        self.hiss_hp.set(1, 2500.0, 0.7, 0.0);
        // A bigger cabinet holds a bigger speaker: lower bass cutoff, wider treble.
        let size = ((p.screen_inches - 9.0) / 27.0).clamp(0.0, 1.0);
        let f_low = 220.0 - 150.0 * size;
        let f_high = 5500.0 + 6500.0 * size;
        self.spk_hp[0].set(1, f_low, 0.7, 0.0);
        self.spk_hp[1].set(1, f_low * 0.8, 0.9, 0.0);
        self.spk_peak.set(2, f_low * 1.4, 1.6, 5.0 - 2.0 * size);
        self.spk_lp[0].set(0, f_high, 0.7, 0.0);
        self.spk_lp[1].set(0, f_high * 1.2, 0.9, 0.0);
    }

    fn process(&mut self, p: &AudioParams, ring: &AudioRing, out: &mut [i16]) {
        self.configure(p);
        let n = out.len() / 2;
        let dt = 1.0 / RATE as f64;

        // derived per-block quantities
        let snr = p.snr_db;
        // FM sound: ~20 dB of FM improvement above the threshold (~10 dB CNR)
        let hiss = if p.rf { (10f32.powf(-(snr + 18.0) / 20.0) * 0.8).min(0.02) } else { 0.0 };
        let sig_gain = if p.rf { smooth(-10.0, 6.0, snr) } else { 1.0 };
        let static_amp = if p.rf { 0.6 * (1.0 - smooth(-6.0, 9.0, snr)) } else { 0.0 };
        let click_rate = if p.rf { 250.0 * (-((snr - 4.0) / 5.0).powi(2)).exp() } else { 0.0 } + p.impulse * 18.0;
        let buzz_amp = if p.rf { 0.045 * p.fine_tune.abs() + 0.025 * (1.0 - smooth(5.0, 25.0, snr)) } else { 0.0 };
        let detune_dist = p.fine_tune * 0.8;
        let track_err = if p.vcr { p.tracking.abs() } else { 0.0 };
        let track_noise = ((track_err - 0.05) * 2.6).clamp(0.0, 1.0);
        let off_target = if p.power_on && p.enabled { smooth(0.14, 0.38, p.warm) } else { 0.0 };

        if !self.inited {
            self.inited = true;
            self.was_on = p.power_on;
            self.last_degauss_t = p.degauss_t;
            if p.power_on && p.degauss_t < 0.08 {
                self.crackle = 0.85;
            }
        } else {
            // Static electricity discharge/charge crackle on power switch or fresh degauss
            if p.power_on != self.was_on {
                self.was_on = p.power_on;
                self.crackle = 1.0;
            }
            if p.degauss_t < self.last_degauss_t - 0.05 || (p.degauss_t < 0.05 && self.last_degauss_t > 0.2) {
                self.crackle = self.crackle.max(0.75);
            }
            self.last_degauss_t = p.degauss_t;
        }

        let mut b = ring.buf.lock().unwrap();
        for i in 0..n {
            self.t += dt;
            let t = self.t as f32;
            // ---- source
            let (mut l, mut r) = ring.pop_frame(&mut b).unwrap_or((0.0, 0.0));
            if p.test_tone {
                let s = 0.12 * (std::f32::consts::TAU * 1000.0 * t).sin();
                l += s;
                r += s;
            }

            // ---- VCR linear audio track (mono)
            if p.vcr {
                let m = 0.5 * (l + r);
                // wow (capstan ~0.5 Hz) and flutter (~9 Hz) as a modulated delay line
                let d_ms = 6.0
                    + p.wow * (1.6 * (std::f32::consts::TAU * 0.55 * t).sin() + 0.25 * (std::f32::consts::TAU * 9.3 * t).sin());
                let d = (d_ms * 0.001 * RATE).clamp(1.0, 4000.0);
                let len = self.delay.len();
                self.delay[self.dpos] = m;
                let rp = (self.dpos as f32 - d + len as f32) % len as f32;
                let i0 = rp.floor() as usize;
                let fr = rp - i0 as f32;
                let mut x = self.delay[i0 % len] * (1.0 - fr) + self.delay[(i0 + 1) % len] * fr;
                self.dpos = (self.dpos + 1) % len;
                x = self.tape_hp.run(x);
                x = self.tape_lp[0].run(x);
                x = self.tape_lp[1].run(x);
                // tape saturation
                x = (x * 1.4).tanh() / 1.4;
                // dropouts: oxide gaps make the level dip
                if self.dropout_left == 0 && self.rnd() < p.dropouts * (1.0 + 6.0 * track_noise) * 1.5 / RATE {
                    self.dropout_left = (RATE * (0.015 + 0.06 * self.rnd())) as usize;
                }
                let target = if self.dropout_left > 0 {
                    self.dropout_left -= 1;
                    0.15
                } else {
                    1.0
                };
                self.dropout_env += (target - self.dropout_env) * 0.01;
                x *= self.dropout_env;
                // tape hiss
                let h = self.noise();
                x += self.hiss_hp.run(h) * (0.004 + 0.025 * p.tape_wear);
                // mistracking: noise bursts buzzing at the head-switching rate (29.97 Hz)
                if track_noise > 0.0 {
                    let gate = ((std::f32::consts::TAU * 29.97 * t).sin() * 0.5 + 0.5).powf(6.0);
                    x += self.noise() * track_noise * 0.25 * gate;
                    x *= 1.0 - 0.6 * track_noise * gate;
                }
                if p.head_switch > 0.0 {
                    let ph = (t * 29.97).fract();
                    if ph < 0.004 {
                        x += p.head_switch * 0.01 * self.noise();
                    }
                }
                l = x;
                r = x;
            }

            // ---- RF: FM sound carrier
            if p.rf {
                let mut m = [l, r];
                for v in m.iter_mut() {
                    // mistuned FM detector: off the linear part of the S-curve
                    *v += detune_dist * *v * v.abs() * 0.8;
                    *v = (*v * (1.0 + detune_dist.abs())).tanh() / (1.0 + detune_dist.abs());
                    *v *= sig_gain;
                }
                // hiss and full static, de-emphasised like a real FM receiver
                let n0 = self.noise();
                let hs = self.deemph.run(n0) * (hiss * 3.0);
                let n1 = self.noise();
                let st = self.static_lp.run(n1) * static_amp;
                // threshold "popcorn" clicks
                if self.rnd() < click_rate / RATE {
                    self.click_env = 0.5 + 0.5 * self.rnd();
                    self.click_sign = if self.rnd() > 0.5 { 1.0 } else { -1.0 };
                }
                let click = self.click_env * self.click_sign;
                self.click_env *= 0.93;
                // intercarrier / sync buzz at the field rate (only when mistuned or weak RF signal)
                let buzz = if buzz_amp > 1e-5 {
                    let fph = (t * 59.94).fract();
                    let pulse = if fph < 0.06 { 1.0 } else { 0.0 };
                    self.buzz_lp.run(pulse - 0.06) * buzz_amp * 4.5
                } else {
                    0.0
                };
                // mains hum
                let hum = p.hum
                    * 0.05
                    * ((std::f32::consts::TAU * 60.0 * t).sin() + 0.5 * (std::f32::consts::TAU * 120.0 * t).sin());
                let common = hs + st + click * 0.35 + buzz + hum;
                l = m[0] + common;
                r = m[1] + common;
                // multipath: a delayed copy of the FM carrier distorts the demodulated sound
                if p.ghost.abs() > 0.01 {
                    l += p.ghost.abs() * 0.3 * (l * 3.0).sin() * 0.1;
                    r += p.ghost.abs() * 0.3 * (r * 3.0).sin() * 0.1;
                }
            }
            if p.extra_noise > 0.0 {
                let e = self.noise() * p.extra_noise * 0.3;
                l += e;
                r += e;
            }
            // Brief RF static burst when snapping to a new cable channel
            if p.channel_switch_t >= 0.0 && p.channel_switch_t < 0.25 {
                let ct = p.channel_switch_t + i as f32 / RATE;
                if ct < 0.25 {
                    let env = (1.0 - ct / 0.25).powi(2);
                    let burst = self.noise() * 0.30 * env;
                    l = l * (1.0 - 0.75 * env) + burst;
                    r = r * (1.0 - 0.75 * env) + burst;
                }
            }

            // ---- TV amplifier and cabinet speaker
            if p.mono {
                let m = 0.5 * (l + r);
                l = m;
                r = m;
            }
            let mut y = 0.5 * (l + r);
            let side = 0.5 * (l - r);
            if p.tv_speaker {
                y = self.spk_hp[0].run(y);
                y = self.spk_hp[1].run(y);
                y = self.spk_peak.run(y);
                y = self.spk_lp[0].run(y);
                y = self.spk_lp[1].run(y);
            }
            // supply ripple from dried-out capacitors leaks into the audio amp
            if p.ripple > 0.0 {
                y += p.ripple * 0.01 * (std::f32::consts::TAU * 120.0 * t).sin().abs();
            }
            // ---- Chassis & picture-tube acoustic sounds (bypass the warming audio amp)
            let mut chassis_c = 0.0f32;
            // flyback transformer whine at the line rate (15.734 kHz)
            if p.power_on && p.warm > 0.1 {
                chassis_c += p.whine * (0.004 + 0.006 * p.age) * (std::f32::consts::TAU * 15734.26 * t).sin();
            }
            // degauss coil: loud 60 Hz buzz (120 Hz force ripple) + initial relay/coil thump,
            // decaying along the same PTC thermistor envelope as the visual warp.
            if p.degauss_t >= 0.0 && p.degauss_t < 2.5 {
                let dg = p.degauss_t + i as f32 / RATE;
                let rise = (dg / 0.02).min(1.0);
                let ptc = 1.0 / (1.0 + (dg / 0.36).powf(2.6));
                let a = rise * ptc * (-dg / 1.15).exp();
                let buzz = (std::f32::consts::TAU * 60.0 * t).sin().abs() - 0.63;
                let harm = 0.25 * (std::f32::consts::TAU * 120.0 * t).sin();
                chassis_c += 0.36 * a * (buzz + harm);
                if dg < 0.075 {
                    let k = 1.0 - dg / 0.075;
                    chassis_c += 0.42 * k * k * (std::f32::consts::TAU * 48.0 * dg).sin();
                }
            }
            // Crisp electronic/mechanical tuner click on channel change
            if p.channel_switch_t >= 0.0 && p.channel_switch_t < 0.020 {
                let ct = p.channel_switch_t + i as f32 / RATE;
                if ct < 0.018 {
                    let k = 1.0 - ct / 0.018;
                    chassis_c += 0.34 * k * k * (std::f32::consts::TAU * 880.0 * ct).sin();
                }
            }
            // Percussive maintenance: hollow walnut CRT cabinet THWACK + internal chassis rattle
            if p.whack_t >= 0.0 && p.whack_t < 0.16 {
                let wt = p.whack_t + i as f32 / RATE;
                if wt < 0.15 {
                    let body = (-wt * 26.0).exp()
                        * (0.58 * (std::f32::consts::TAU * 108.0 * wt).sin()
                            + 0.34 * (std::f32::consts::TAU * 172.0 * wt).sin());
                    let slap = if wt < 0.018 {
                        let k = 1.0 - wt / 0.018;
                        k * k * (0.45 * self.noise() + 0.35 * (std::f32::consts::TAU * 390.0 * wt).sin())
                    } else {
                        0.0
                    };
                    let rattle = (-wt * 38.0).exp() * 0.14 * self.noise() * (std::f32::consts::TAU * 62.0 * wt).sin();
                    chassis_c += 0.72 * (body + slap + rattle);
                }
            }
            // Subtle high-voltage electrostatic faceplate crackle on power switch / degauss:
            // delicate high-passed micro-sparks & soft corona fizz across the glass.
            let mut crackle_l = 0.0f32;
            let mut crackle_r = 0.0f32;
            if self.crackle > 0.002 || self.spark_env_l > 0.001 || self.spark_env_r > 0.001 {
                let rate = 0.0028 * (0.18 + 0.82 * self.crackle);
                if self.crackle > 0.002 && self.rnd() < rate {
                    let u = self.rnd();
                    let amp = (0.22 + 0.78 * u * u) * self.crackle;
                    let pan = 0.22 + 0.56 * self.rnd();
                    self.spark_env_l = (self.spark_env_l + amp * (1.0 - pan)).min(1.0);
                    self.spark_env_r = (self.spark_env_r + amp * pan).min(1.0);
                    self.spark_decay = 0.91 + 0.065 * self.rnd();
                }
                let fizz = 0.028 * self.crackle * self.crackle;
                let raw_l = (self.rnd() * 2.0 - 1.0) * (self.spark_env_l + fizz);
                let raw_r = (self.rnd() * 2.0 - 1.0) * (self.spark_env_r + fizz);
                crackle_l = self.crackle_hp_l.run(raw_l) * 0.22;
                crackle_r = self.crackle_hp_r.run(raw_r) * 0.22;
                self.spark_env_l *= self.spark_decay;
                self.spark_env_r *= self.spark_decay;
                self.crackle *= 0.999952;
            }

            self.gain += (off_target - self.gain) * 0.0004;
            let vol = p.volume * p.volume * 1.6;
            let amp_g = self.gain * vol;
            let chassis_g = if p.enabled { vol } else { 0.0 };
            let mut ol = (y + side) * amp_g + (chassis_c + crackle_l) * chassis_g;
            let mut or = (y - side) * amp_g + (chassis_c + crackle_r) * chassis_g;
            ol = soft_clip(ol);
            or = soft_clip(or);
            out[2 * i] = (ol * 32767.0) as i16;
            out[2 * i + 1] = (or * 32767.0) as i16;
        }
    }
}

fn smooth(e0: f32, e1: f32, x: f32) -> f32 {
    let t = ((x - e0) / (e1 - e0)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

fn soft_clip(x: f32) -> f32 {
    if x.abs() < 0.8 { x } else { x.signum() * (0.8 + 0.2 * ((x.abs() - 0.8) / 0.2).tanh()) }
}

// ------------------------------------------------------------------ output

pub struct AudioEngine {
    pub params: Arc<Mutex<AudioParams>>,
    pub ring: Arc<AudioRing>,
    pub status: Arc<Mutex<String>>,
    stop: Arc<AtomicBool>,
}

impl AudioEngine {
    pub fn start() -> AudioEngine {
        let params = Arc::new(Mutex::new(AudioParams::default()));
        let ring = AudioRing::new();
        let status = Arc::new(Mutex::new("Looking for a sound output…".to_string()));
        let stop = Arc::new(AtomicBool::new(false));
        let (p2, r2, s2, st2) = (params.clone(), ring.clone(), status.clone(), stop.clone());
        std::thread::spawn(move || run_output(p2, r2, s2, st2));
        AudioEngine { params, ring, status, stop }
    }
}

impl Drop for AudioEngine {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
    }
}

fn sinks() -> Vec<(&'static str, Vec<&'static str>)> {
    vec![
        (
            "pacat",
            vec![
                "--playback",
                "--raw",
                "--format=s16le",
                "--rate=48000",
                "--channels=2",
                "--latency-msec=35",
                "--client-name=ViCTR",
            ],
        ),
        ("pw-cat", vec!["--playback", "--format", "s16", "--rate", "48000", "--channels", "2", "--latency", "35ms", "-"]),
        ("aplay", vec!["-q", "-f", "S16_LE", "-r", "48000", "-c", "2", "-t", "raw", "--buffer-time=40000"]),
        (
            "mpv",
            vec![
                "--no-config",
                "--really-quiet",
                "--no-terminal",
                "--no-video",
                "--demuxer=rawaudio",
                "--demuxer-rawaudio-format=s16le",
                "--demuxer-rawaudio-rate=48000",
                "--demuxer-rawaudio-channels=2",
                "--cache=no",
                "--audio-buffer=0.025",
                "--stream-buffer-size=4k",
                "--demuxer-readahead-secs=0",
                "-",
            ],
        ),
        (
            "ffplay",
            vec![
                "-nodisp",
                "-autoexit",
                "-loglevel",
                "quiet",
                "-f",
                "s16le",
                "-ar",
                "48000",
                "-ch_layout",
                "stereo",
                "-i",
                "pipe:0",
            ],
        ),
    ]
}

#[cfg(target_os = "linux")]
fn shrink_pipe(child: &Child) {
    use std::os::unix::io::AsRawFd;
    unsafe extern "C" {
        fn fcntl(fd: i32, cmd: i32, ...) -> i32;
    }
    if let Some(stdin) = child.stdin.as_ref() {
        // F_SETPIPE_SZ: 4096 B (minimum page size = ~21 ms at 48 kHz stereo s16) keeps lip-sync tight
        unsafe { fcntl(stdin.as_raw_fd(), 1031, 4096i32) };
    }
}
#[cfg(not(target_os = "linux"))]
fn shrink_pipe(_child: &Child) {}

fn run_output(params: Arc<Mutex<AudioParams>>, ring: Arc<AudioRing>, status: Arc<Mutex<String>>, stop: Arc<AtomicBool>) {
    let mut proc = Processor::new();
    let mut buf = vec![0i16; BLOCK * 2];
    let mut bytes = vec![0u8; BLOCK * 4];
    for (cmd, args) in sinks() {
        if stop.load(Ordering::SeqCst) {
            return;
        }
        let child = Command::new(cmd).args(&args).stdin(Stdio::piped()).stdout(Stdio::null()).stderr(Stdio::null()).spawn();
        let mut child: Child = match child {
            Ok(c) => c,
            Err(_) => continue,
        };
        shrink_pipe(&child);
        let mut stdin = child.stdin.take().unwrap();
        *status.lock().unwrap() = format!("Sound out through {cmd}");
        let mut written = 0usize;
        let ok = loop {
            if stop.load(Ordering::SeqCst) {
                let _ = child.kill();
                return;
            }
            let p = params.lock().unwrap().clone();
            proc.process(&p, &ring, &mut buf);
            for (k, s) in buf.iter().enumerate() {
                let b = s.to_le_bytes();
                bytes[2 * k] = b[0];
                bytes[2 * k + 1] = b[1];
            }
            if stdin.write_all(&bytes).is_err() {
                break written > (RATE as usize) * 4; // ran for a while: it was a real sink
            }
            written += bytes.len();
        };
        let _ = child.kill();
        let _ = child.wait();
        if ok {
            *status.lock().unwrap() = format!("Sound output ({cmd}) stopped.");
            return;
        }
    }
    *status.lock().unwrap() = "No sound output found (needs pacat, pw-cat, aplay or mpv).".to_string();
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rms(p: &AudioParams, feed_tone: bool, seconds: f32) -> (f32, bool) {
        let ring = AudioRing::new();
        let mut proc = Processor::new();
        let mut out = vec![0i16; BLOCK * 2];
        let stop = AtomicBool::new(false);
        let mut acc = 0f64;
        let mut n = 0usize;
        let mut t = 0usize;
        let blocks = (seconds * RATE / BLOCK as f32) as usize;
        for _ in 0..blocks {
            if feed_tone {
                let mut bytes = Vec::with_capacity(BLOCK * 4);
                for _ in 0..BLOCK {
                    let v = (8000.0 * (std::f32::consts::TAU * 440.0 * t as f32 / RATE).sin()) as i16;
                    bytes.extend_from_slice(&v.to_le_bytes());
                    bytes.extend_from_slice(&v.to_le_bytes());
                    t += 1;
                }
                ring.push_s16(&bytes, &stop);
            }
            proc.process(p, &ring, &mut out);
            for s in &out {
                acc += (*s as f64 / 32768.0).powi(2);
                n += 1;
            }
        }
        let r = (acc / n as f64).sqrt() as f32;
        (r, r.is_finite())
    }

    #[test]
    fn signal_chain_levels() {
        let mut p = AudioParams { whine: 0.0, degauss_t: 10.0, ..Default::default() };
        let (clean, ok) = rms(&p, true, 3.0);
        assert!(ok && clean > 0.02, "clean tone should be audible: {clean}");

        p.rf = true;
        p.snr_db = -25.0;
        let (snow, ok) = rms(&p, false, 3.0);
        assert!(ok && snow > 0.03, "no-signal static should be loud: {snow}");

        p.snr_db = 45.0;
        let (quiet, _) = rms(&p, false, 3.0);
        assert!(quiet < snow * 0.2, "strong signal should be quiet without programme: {quiet} vs {snow}");

        p.power_on = false;
        let (off, _) = rms(&p, true, 3.0);
        assert!(off < 0.01, "power off should be silent: {off}");

        let mut v = AudioParams { whine: 0.0, degauss_t: 10.0, vcr: true, tape_wear: 1.0, tracking: 0.6, dropouts: 1.0, wow: 1.0, ..Default::default() };
        v.tape_bw_khz = 5.0;
        let (tape, ok) = rms(&v, true, 3.0);
        assert!(ok && tape > 0.01, "tape path should pass sound: {tape}");
        println!("clean {clean:.3} static {snow:.3} strong {quiet:.4} off {off:.4} tape {tape:.3}");
    }

    #[test]
    fn static_crackle_on_power_and_degauss() {
        let ring = AudioRing::new();
        let mut proc = Processor::new();
        let mut out = vec![0i16; BLOCK * 2];

        // Start powered off and idle -> silent
        let mut p = AudioParams { whine: 0.0, degauss_t: 10.0, power_on: false, warm: 0.0, ..Default::default() };
        proc.process(&p, &ring, &mut out);
        assert!(out.iter().all(|&s| s == 0), "idle powered-off TV should be silent");

        // Switch power on (cold heater, warm = 0.0, no degauss buzz yet) -> subtle static crackle audible
        p.power_on = true;
        let mut energy = 0f64;
        for _ in 0..80 {
            proc.process(&p, &ring, &mut out);
            energy += out.iter().map(|&s| (s as f64 / 32768.0).powi(2)).sum::<f64>();
        }
        let pwr_rms = (energy / (80 * BLOCK * 2) as f64).sqrt();
        assert!(pwr_rms > 0.0005 && pwr_rms < 0.08, "power-on crackle should be subtle and audible: {pwr_rms}");

        // Let crackle decay, then trigger a degauss -> crackle re-triggers cleanly
        for _ in 0..400 {
            proc.process(&p, &ring, &mut out);
        }
        assert!(proc.crackle < 0.01, "crackle should decay cleanly");
        p.degauss_t = 0.0;
        proc.process(&p, &ring, &mut out);
        assert!(proc.crackle >= 0.7, "degauss should trigger subtle static crackle: {}", proc.crackle);
    }

    #[test]
    fn old_session_cannot_wipe_new_session_audio() {
        let ring = AudioRing::new();
        let stop = AtomicBool::new(false);
        let s1 = ring.begin_session();
        let s2 = ring.begin_session();

        let bytes = [0x10u8, 0x20, 0x30, 0x40].repeat(64);
        // Stale push from s1 is ignored
        ring.push_s16_session(s1, &bytes, &stop);
        assert_eq!(ring.len(), 0);

        // Active push from s2 lands normally
        ring.push_s16_session(s2, &bytes, &stop);
        assert_eq!(ring.len(), 128);

        // Stale clear from s1 teardown does NOT wipe s2's buffered audio
        ring.clear_session(s1);
        assert_eq!(ring.len(), 128);

        // Clearing s2 itself works
        ring.clear_session(s2);
        assert_eq!(ring.len(), 0);
    }

    #[test]
    fn engine_writes_to_sink() {
        let fake = "/tmp/crtfake";
        if !std::path::Path::new(&format!("{fake}/pacat")).exists() {
            return;
        }
        let _ = std::fs::remove_file(format!("{fake}/sound.raw"));
        let path = std::env::var("PATH").unwrap_or_default();
        unsafe { std::env::set_var("PATH", format!("{fake}:{path}")) };
        let eng = AudioEngine::start();
        {
            let mut p = eng.params.lock().unwrap();
            p.rf = true;
            p.snr_db = -25.0;
        }
        std::thread::sleep(Duration::from_millis(1500));
        let st = eng.status.lock().unwrap().clone();
        drop(eng);
        std::thread::sleep(Duration::from_millis(200));
        let len = std::fs::metadata(format!("{fake}/sound.raw")).map(|m| m.len()).unwrap_or(0);
        println!("status: {st}, bytes: {len}");
        assert!(st.contains("pacat"));
        assert!(len > 10000);
    }
}
