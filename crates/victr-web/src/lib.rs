//! Thin WASM + WebGPU entry point for **ViCTR Web (`onvic.tr`)**.
//!
//! Wraps `victr_core` (`Gpu`, `Sim`, `Settings`, `TvOsd`, `TeletextRenderer`,
//! and `audio_dsp::Processor`) in a compact `wasm-bindgen` API driven by a
//! lightweight HTML/CSS/JS shell.

#![cfg(target_arch = "wasm32")]

use serde::{Deserialize, Serialize};
use std::sync::Arc;
use victr_core::audio_dsp::{self, AudioParams, AudioRing};
use victr_core::gpu::{FrameInput, Gpu};
use victr_core::osd::TvOsd;
use victr_core::settings::{self, Input, Mask, Pattern, Settings, TapeSpeed};
use victr_core::sim::Sim;
use victr_core::teletext::{TeletextInfo, TeletextMode, TeletextRenderer};
use wasm_bindgen::prelude::*;
use web_sys::{HtmlCanvasElement, HtmlImageElement, HtmlVideoElement, ImageBitmap};

#[derive(Serialize)]
struct SettingsSnapshot {
    preset_index: usize,
    preset_name: String,
    pattern: String,
    input: String,
    mask: String,
    vcr: bool,
    tape_speed: String,
    interlaced: bool,
    brightness: f32,
    contrast: f32,
    color: f32,
    tint: f32,
    sharpness: f32,
    curvature: f32,
    screen_inches: f32,
    show_bezel: bool,
    chaos: bool,
    signal_db: f32,
    sound: bool,
    volume: f32,
    muted: bool,
    tv_speaker: bool,
    mono: bool,
    whine: f32,
    crop_43: bool,
    power_on: bool,
    hdr_supported: bool,
    hdr_active: bool,
    backend: String,
}

#[derive(Deserialize, Default)]
struct SettingsPatch {
    input: Option<String>,
    mask: Option<String>,
    vcr: Option<bool>,
    tape_speed: Option<String>,
    interlaced: Option<bool>,
    brightness: Option<f32>,
    contrast: Option<f32>,
    color: Option<f32>,
    tint: Option<f32>,
    sharpness: Option<f32>,
    curvature: Option<f32>,
    screen_inches: Option<f32>,
    show_bezel: Option<bool>,
    chaos: Option<bool>,
    signal_db: Option<f32>,
    sound: Option<bool>,
    volume: Option<f32>,
    tv_speaker: Option<bool>,
    mono: Option<bool>,
    whine: Option<f32>,
}

struct TeletextMeta {
    active: bool,
    inactive: bool,
    channel_number: u16,
    callsign: String,
    channel_name: String,
    url: String,
    detail: String,
    elapsed_secs: f32,
}

impl Default for TeletextMeta {
    fn default() -> Self {
        Self {
            active: false,
            inactive: false,
            channel_number: 80,
            callsign: "LIVE".into(),
            channel_name: "Online Stream".into(),
            url: String::new(),
            detail: String::new(),
            elapsed_secs: 0.0,
        }
    }
}

#[wasm_bindgen]
pub struct VictrWeb {
    surface: wgpu::Surface<'static>,
    adapter: wgpu::Adapter,
    device: wgpu::Device,
    queue: wgpu::Queue,
    gpu: Gpu,
    width: u32,
    height: u32,

    s: Settings,
    preset_idx: usize,
    sim: Sim,
    last_fi: FrameInput,
    osd: TvOsd,
    teletext: TeletextRenderer,
    tt_meta: TeletextMeta,

    zoom: f32,
    pan: [f32; 2],
    crop_43: bool,
    muted: bool,
    reset_trails: bool,
    clear_burn: bool,
    has_video_media: bool,
    degauss_mag_start: Option<f32>,

    audio_ring: Arc<AudioRing>,
    audio_session: u64,
    audio_dsp: audio_dsp::Processor,
    audio_params: AudioParams,
}

fn view_half_at_zoom(width: u32, height: u32, show_bezel: bool, zoom: f32) -> f32 {
    let a = (width.max(1) as f32) / (height.max(1) as f32);
    let m: f32 = if show_bezel { 0.26 } else { 0.03 };
    let base = (1.0 + m).max((4.0 / 3.0 + m) / a);
    base / zoom.max(0.25)
}

fn pixel_perfect_screen_inches(pitch_mm: f32, mask: Mask, view_h_px: f32, view_half_1x: f32) -> f32 {
    let target_px_per_pitch = match mask {
        Mask::ApertureGrille | Mask::SlotMask => 3.0,
        Mask::ShadowMask => 3.0 * 3.0f32.sqrt(),
    };
    let inches = (pitch_mm * view_h_px) / (15.24 * view_half_1x.max(1e-3) * target_px_per_pitch);
    inches.clamp(4.0, 36.0)
}

#[wasm_bindgen]
impl VictrWeb {
    /// Initializes WebGPU on the given `<canvas>` and builds the full CRT physics pipeline.
    pub async fn create(canvas: HtmlCanvasElement) -> Result<VictrWeb, JsValue> {
        let width = canvas.width().max(1);
        let height = canvas.height().max(1);

        let mut desc = wgpu::InstanceDescriptor::new_without_display_handle();
        desc.backends = wgpu::Backends::BROWSER_WEBGPU;
        let instance = wgpu::Instance::new(desc);
        let surface = instance
            .create_surface(wgpu::SurfaceTarget::Canvas(canvas))
            .map_err(|e| JsValue::from_str(&format!("WebGPU surface error: {e}")))?;

        let adapter = instance
            .request_adapter(&wgpu::RequestAdapterOptions {
                power_preference: wgpu::PowerPreference::HighPerformance,
                compatible_surface: Some(&surface),
                force_fallback_adapter: false,
                ..Default::default()
            })
            .await
            .map_err(|e| JsValue::from_str(&format!("WebGPU adapter unavailable: {e}")))?;

        let (device, queue) = adapter
            .request_device(&wgpu::DeviceDescriptor {
                label: Some("victr_web_device"),
                required_features: wgpu::Features::empty(),
                required_limits: wgpu::Limits::default(),
                experimental_features: wgpu::ExperimentalFeatures::disabled(),
                memory_hints: wgpu::MemoryHints::Performance,
                trace: wgpu::Trace::Off,
            })
            .await
            .map_err(|e| JsValue::from_str(&format!("WebGPU device error: {e}")))?;

        let s = settings::preset(0);
        let gpu = Gpu::new(&device, &queue, &adapter, &surface, width, height, s.hdr_mode, false);

        let mut sim = Sim::new();
        // Authentic quick warm-up + subtle degauss pulse on initial power-on
        sim.warm = 0.05;
        sim.degauss_t = 0.0;

        let audio_ring = AudioRing::new();
        let audio_session = audio_ring.begin_session();

        let mut engine = VictrWeb {
            surface,
            adapter,
            device,
            queue,
            gpu,
            width,
            height,
            s,
            preset_idx: 0,
            sim,
            last_fi: FrameInput::default(),
            osd: TvOsd::new(),
            teletext: TeletextRenderer::new(),
            tt_meta: TeletextMeta::default(),
            zoom: 1.0,
            pan: [0.0, 0.0],
            crop_43: false,
            muted: false,
            reset_trails: true,
            clear_burn: false,
            has_video_media: false,
            degauss_mag_start: None,
            audio_ring,
            audio_session,
            audio_dsp: audio_dsp::Processor::new(),
            audio_params: AudioParams::default(),
        };
        engine.sync_audio_params();
        Ok(engine)
    }

    /// Returns built-in preset names as a JSON array of strings.
    pub fn preset_names_json() -> String {
        serde_json::to_string(&settings::PRESETS).unwrap_or_else(|_| "[]".into())
    }

    pub fn resize(&mut self, width: u32, height: u32) {
        let w = width.max(1);
        let h = height.max(1);
        if w != self.width || h != self.height {
            self.width = w;
            self.height = h;
            self.gpu.reconfigure(
                &self.device,
                &self.adapter,
                &self.surface,
                w,
                h,
                self.s.hdr_mode,
                false,
            );
            self.reset_trails = true;
        }
    }

    pub fn set_preset(&mut self, idx: usize) {
        let i = idx.min(settings::PRESETS.len().saturating_sub(1));
        let keep_pat = self.s.pattern;
        let keep_vol = self.s.volume;
        let keep_sound = self.s.sound;
        self.s = settings::preset(i);
        self.s.pattern = keep_pat;
        self.s.volume = keep_vol;
        self.s.sound = keep_sound;
        self.preset_idx = i;
        self.reset_trails = true;
        self.osd.show_action(&settings::PRESETS[i].to_ascii_uppercase());
    }

    /// Matches a preset by slug or case-insensitive substring (e.g. "vhs", "trinitron", "pvm", "rf").
    pub fn set_preset_by_name(&mut self, query: &str) -> bool {
        let q = query.trim().to_ascii_lowercase();
        if q.is_empty() {
            return false;
        }
        // Short friendly aliases for shareable onvic.tr URLs
        let alias_idx: Option<usize> = match q.as_str() {
            "living" | "90s" | "default" | "tv" => Some(0),
            "pvm" | "sony" | "trinitron" | "studio" | "rgb" => Some(1),
            "arcade" | "cabinet" => Some(2),
            "vhs" | "tape" | "rental" => Some(3),
            "rf" | "antenna" | "snow" | "weak" => Some(4),
            "portable" | "kitchen" | "small" => Some(5),
            "bw" | "mono" | "1950s" => Some(6),
            "slowmo" | "scan" => Some(7),
            _ => None,
        };
        if let Some(i) = alias_idx {
            if i < settings::PRESETS.len() {
                self.set_preset(i);
                return true;
            }
        }
        if let Ok(num) = q.parse::<usize>() {
            if num < settings::PRESETS.len() {
                self.set_preset(num);
                return true;
            }
        }
        if let Some(i) = settings::PRESETS
            .iter()
            .position(|p| p.to_ascii_lowercase().contains(&q))
        {
            self.set_preset(i);
            return true;
        }
        false
    }

    pub fn get_settings_json(&self) -> String {
        let hdr_st = self.gpu.hdr_status();
        let snap = SettingsSnapshot {
            preset_index: self.preset_idx,
            preset_name: settings::PRESETS
                .get(self.preset_idx)
                .copied()
                .unwrap_or("Custom")
                .to_string(),
            pattern: self.s.pattern.as_key().into(),
            input: self.s.input.as_key().into(),
            mask: self.s.mask.as_key().into(),
            vcr: self.s.vcr,
            tape_speed: self.s.tape_speed.as_key().into(),
            interlaced: self.s.interlaced,
            brightness: self.s.brightness,
            contrast: self.s.contrast,
            color: self.s.color,
            tint: self.s.tint,
            sharpness: self.s.sharpness,
            curvature: self.s.curvature,
            screen_inches: self.s.screen_inches,
            show_bezel: self.s.show_bezel,
            chaos: self.s.chaos,
            signal_db: self.s.signal_db,
            sound: self.s.sound,
            volume: self.s.volume,
            muted: self.muted,
            tv_speaker: self.s.tv_speaker,
            mono: self.s.mono,
            whine: self.s.whine,
            crop_43: self.crop_43,
            power_on: self.sim.power_on,
            hdr_supported: hdr_st.supported,
            hdr_active: hdr_st.active_mode.is_hdr(),
            backend: hdr_st.backend,
        };
        serde_json::to_string(&snap).unwrap_or_else(|_| "{}".into())
    }

    pub fn apply_settings_patch(&mut self, json: &str) {
        let Ok(p) = serde_json::from_str::<SettingsPatch>(json) else { return };
        if let Some(v) = p.input.as_deref().and_then(Input::from_key) {
            self.s.input = v;
        }
        if let Some(v) = p.mask.as_deref().and_then(Mask::from_key) {
            self.s.mask = v;
        }
        if let Some(v) = p.vcr {
            self.s.vcr = v;
        }
        if let Some(v) = p.tape_speed.as_deref().and_then(TapeSpeed::from_key) {
            self.s.tape_speed = v;
        }
        if let Some(v) = p.interlaced {
            self.s.interlaced = v;
        }
        if let Some(v) = p.brightness {
            self.s.brightness = v.clamp(-1.0, 1.0);
        }
        if let Some(v) = p.contrast {
            self.s.contrast = v.clamp(0.2, 2.0);
        }
        if let Some(v) = p.color {
            self.s.color = v.clamp(0.0, 2.0);
        }
        if let Some(v) = p.tint {
            self.s.tint = v.clamp(-1.0, 1.0);
        }
        if let Some(v) = p.sharpness {
            self.s.sharpness = v.clamp(0.0, 1.0);
        }
        if let Some(v) = p.curvature {
            self.s.curvature = v.clamp(0.0, 0.35);
        }
        if let Some(v) = p.screen_inches {
            self.s.screen_inches = v.clamp(4.0, 36.0);
        }
        if let Some(v) = p.show_bezel {
            self.s.show_bezel = v;
            self.reset_trails = true;
        }
        if let Some(v) = p.chaos {
            self.s.chaos = v;
        }
        if let Some(v) = p.signal_db {
            self.s.signal_db = v.clamp(-8.0, 45.0);
        }
        if let Some(v) = p.sound {
            self.s.sound = v;
        }
        if let Some(v) = p.volume {
            self.s.volume = v.clamp(0.0, 1.0);
        }
        if let Some(v) = p.tv_speaker {
            self.s.tv_speaker = v;
        }
        if let Some(v) = p.mono {
            self.s.mono = v;
        }
        if let Some(v) = p.whine {
            self.s.whine = v.clamp(0.0, 1.0);
        }
    }

    pub fn set_pattern(&mut self, key: &str) {
        if let Some(p) = Pattern::from_key(key) {
            self.s.pattern = p;
            if p != Pattern::Video && p != Pattern::Image {
                self.has_video_media = false;
                self.tt_meta.active = false;
            }
            self.reset_trails = true;
        }
    }

    pub fn set_crop_43(&mut self, crop: bool) {
        self.crop_43 = crop;
        self.osd.show_action(if crop { "4:3 CROP ON" } else { "4:3 CROP OFF" });
    }

    pub fn toggle_crop_43(&mut self) -> bool {
        let next = !self.crop_43;
        self.set_crop_43(next);
        next
    }

    pub fn snap_pixel_perfect(&mut self) -> f32 {
        let h_1x = view_half_at_zoom(self.width, self.height, self.s.show_bezel, 1.0);
        let inches = pixel_perfect_screen_inches(self.s.pitch_mm, self.s.mask, self.height as f32, h_1x);
        self.s.screen_inches = inches;
        self.zoom = 1.0;
        self.pan = [0.0, 0.0];
        self.reset_trails = true;
        self.osd.show_action(&format!("1:1 PIXEL {:.1}\"", inches));
        inches
    }

    pub fn set_power(&mut self, on: bool) {
        self.sim.set_power(on);
    }

    pub fn toggle_power(&mut self) -> bool {
        let next = !self.sim.power_on;
        self.sim.set_power(next);
        next
    }

    pub fn degauss(&mut self) {
        self.sim.degauss();
        if self.s.magnetization > 0.0 {
            self.degauss_mag_start = Some(self.s.magnetization);
        }
        self.osd.show_action("DEGAUSS");
    }

    pub fn whack(&mut self) {
        self.sim.whack();
        self.osd.show_action("WHACK!");
    }

    pub fn switch_channel_effect(&mut self) {
        self.sim.switch_channel();
        self.audio_session = self.audio_ring.begin_session();
    }

    pub fn clear_burn_in(&mut self) {
        self.clear_burn = true;
        self.osd.show_action("BURN-IN CLEARED");
    }

    pub fn zoom_by(&mut self, factor: f32) {
        self.zoom = (self.zoom * factor).clamp(1.0, 40.0);
        if self.zoom <= 1.001 {
            self.zoom = 1.0;
            self.pan = [0.0, 0.0];
        }
    }

    pub fn pan_by(&mut self, dx_px: f32, dy_px: f32) {
        if self.zoom <= 1.01 {
            return;
        }
        let h = view_half_at_zoom(self.width, self.height, self.s.show_bezel, self.zoom);
        let scale = (2.0 * h) / (self.height.max(1) as f32);
        self.pan[0] = (self.pan[0] - dx_px * scale).clamp(-1.5, 1.5);
        self.pan[1] = (self.pan[1] + dy_px * scale).clamp(-1.2, 1.2);
    }

    pub fn reset_view(&mut self) {
        self.zoom = 1.0;
        self.pan = [0.0, 0.0];
        self.reset_trails = true;
    }

    pub fn adjust_volume(&mut self, delta: f32) -> f32 {
        self.s.volume = (self.s.volume + delta).clamp(0.0, 1.0);
        if delta > 0.0 && self.muted {
            self.muted = false;
        }
        self.osd.show_volume(self.s.volume, self.muted);
        self.s.volume
    }

    pub fn set_volume(&mut self, vol: f32) {
        self.s.volume = vol.clamp(0.0, 1.0);
        if self.s.volume > 0.0 && self.muted {
            self.muted = false;
        }
        self.osd.show_volume(self.s.volume, self.muted);
    }

    pub fn toggle_mute(&mut self) -> bool {
        self.muted = !self.muted;
        self.osd.show_volume(self.s.volume, self.muted);
        self.muted
    }

    pub fn show_osd_channel(&mut self, ch_num: u16, callsign: &str, title: &str) {
        self.osd.show_channel(ch_num, callsign, title, self.s.mono);
    }

    pub fn show_osd_action(&mut self, msg: &str) {
        self.osd.show_action(msg);
    }

    pub fn set_teletext(
        &mut self,
        active: bool,
        inactive: bool,
        ch_num: u16,
        callsign: &str,
        channel_name: &str,
        url: &str,
        detail: &str,
    ) {
        let was_active = self.tt_meta.active;
        self.tt_meta.active = active;
        self.tt_meta.inactive = inactive;
        self.tt_meta.channel_number = ch_num;
        self.tt_meta.callsign = callsign.to_string();
        self.tt_meta.channel_name = channel_name.to_string();
        self.tt_meta.url = url.to_string();
        self.tt_meta.detail = detail.to_string();
        if active && !was_active {
            self.tt_meta.elapsed_secs = 0.0;
        }
        if active {
            self.s.pattern = Pattern::Video;
        }
    }

    /// Zero-copy GPU upload of the current HTML `<video>` frame directly into WebGPU.
    pub fn upload_video_frame(&mut self, video: &HtmlVideoElement) -> bool {
        let w = video.video_width();
        let h = video.video_height();
        if w == 0 || h == 0 {
            return false;
        }
        self.gpu.upload_external_image(
            &self.device,
            &self.queue,
            wgpu::ExternalImageSource::HTMLVideoElement(video.clone()),
            w,
            h,
        );
        self.s.pattern = Pattern::Video;
        self.has_video_media = true;
        self.tt_meta.active = false;
        true
    }

    /// Zero-copy GPU upload of a loaded `<img>` element.
    pub fn upload_image_element(&mut self, img: &HtmlImageElement) -> bool {
        let w = img.natural_width();
        let h = img.natural_height();
        if w == 0 || h == 0 {
            return false;
        }
        self.gpu.upload_external_image(
            &self.device,
            &self.queue,
            wgpu::ExternalImageSource::HTMLImageElement(img.clone()),
            w,
            h,
        );
        self.s.pattern = Pattern::Image;
        self.has_video_media = true;
        self.tt_meta.active = false;
        self.reset_trails = true;
        true
    }

    /// Zero-copy GPU upload of an `ImageBitmap`.
    pub fn upload_image_bitmap(&mut self, bmp: &ImageBitmap) -> bool {
        let w = bmp.width();
        let h = bmp.height();
        if w == 0 || h == 0 {
            return false;
        }
        self.gpu.upload_external_image(
            &self.device,
            &self.queue,
            wgpu::ExternalImageSource::ImageBitmap(bmp.clone()),
            w,
            h,
        );
        self.s.pattern = Pattern::Image;
        self.has_video_media = true;
        self.tt_meta.active = false;
        self.reset_trails = true;
        true
    }

    pub fn clear_media(&mut self) {
        self.gpu.clear_image();
        self.has_video_media = false;
    }

    /// Processes one WebAudio stereo block through ViCTR's analog TV/RF/VCR audio DSP.
    pub fn process_audio(&mut self, in_l: &[f32], in_r: &[f32], out_l: &mut [f32], out_r: &mut [f32]) {
        let n = in_l.len().min(in_r.len()).min(out_l.len()).min(out_r.len());
        if n == 0 {
            return;
        }
        let mut interleaved = Vec::with_capacity(n * 2);
        for i in 0..n {
            interleaved.push(in_l[i]);
            interleaved.push(in_r[i]);
        }
        self.audio_ring.push_f32_session_realtime(self.audio_session, &interleaved);
        self.audio_dsp
            .process_f32(&self.audio_params, &self.audio_ring, &mut out_l[..n], &mut out_r[..n]);
    }

    fn sync_audio_params(&mut self) {
        let s = &self.s;
        let sim = &self.sim;
        let active_audio = s.sound && !self.muted;
        let is_video = s.pattern == Pattern::Video && self.has_video_media && !self.tt_meta.active;
        self.audio_params = AudioParams {
            enabled: active_audio,
            power_on: sim.power_on,
            warm: sim.warm,
            volume: if self.muted { 0.0 } else { s.volume },
            tv_speaker: s.tv_speaker,
            screen_inches: s.screen_inches,
            mono: s.mono,
            whine: s.whine,
            age: (s.age_hours / 30_000.0).clamp(0.0, 1.0),
            test_tone: s.test_tone && !is_video && !self.tt_meta.active,
            rf: s.input == Input::Rf,
            snr_db: sim.snr_db,
            fine_tune: s.fine_tune,
            ghost: s.ghost_amp,
            hum: s.hum,
            impulse: s.impulse,
            extra_noise: s.extra_noise,
            vcr: s.vcr,
            tape_bw_khz: match s.tape_speed {
                TapeSpeed::Sp => 10.0,
                TapeSpeed::Lp => 7.0,
                TapeSpeed::Ep => 4.8,
            },
            wow: s.timebase_jitter,
            tape_wear: s.tape_wear,
            tracking: s.tracking,
            dropouts: s.dropouts,
            head_switch: s.head_switch,
            ripple: s.ripple,
            degauss_t: sim.degauss_t,
            splash_static: 0.0,
            channel_switch_t: sim.channel_switch_t,
            whack_t: sim.whack_t,
            shuttle: 0.0,
            shuttle_speed: 0.0,
        };
    }

    /// Steps CRT physics and renders one frame to the WebGPU `<canvas>`.
    pub fn tick_and_render(&mut self, dt_secs: f32) -> Result<(), JsValue> {
        let dt = dt_secs.clamp(0.001, 0.1);
        if self.tt_meta.active {
            self.tt_meta.elapsed_secs += dt;
        }
        if let Some(start_mag) = self.degauss_mag_start {
            let progress = (self.sim.degauss_t / 0.85).clamp(0.0, 1.0);
            self.s.magnetization = start_mag * (1.0 - progress).powi(2);
            if progress >= 1.0 {
                self.s.magnetization = 0.0;
                self.degauss_mag_start = None;
            }
        }

        let view_h = view_half_at_zoom(self.width, self.height, self.s.show_bezel, self.zoom);
        let view_vec = [self.pan[0], self.pan[1], view_h];
        let new_field = self.sim.advance(dt as f64) || self.last_fi.lines.is_empty();
        if new_field {
            self.last_fi = self.sim.field(&self.s, view_vec, dt);
        } else {
            self.last_fi.new_field = false;
            self.last_fi.u.view = view_vec;
        }
        self.sync_audio_params();

        let tt_image = if self.tt_meta.active {
            let host = self
                .tt_meta
                .url
                .trim_start_matches("https://")
                .trim_start_matches("http://")
                .split('/')
                .next()
                .unwrap_or("onvic.tr");
            let info = TeletextInfo {
                mode: if self.tt_meta.inactive {
                    TeletextMode::Inactive
                } else {
                    TeletextMode::Loading
                },
                channel_number: (self.tt_meta.channel_number > 0).then_some(self.tt_meta.channel_number),
                callsign: &self.tt_meta.callsign,
                title: &self.tt_meta.channel_name,
                subtitle: "ONVIC.TR WEB RECEIVER",
                source_kind: "HLS / WEB",
                source_host: host,
                status_line: &self.tt_meta.detail,
                elapsed_secs: self.tt_meta.elapsed_secs,
            };
            Some(self.teletext.render(&info))
        } else {
            None
        };

        let mut fi = self.last_fi.clone();
        if tt_image.is_some() {
            fi.u.pattern = Pattern::Video.id();
        }
        fi.image = tt_image;
        fi.osd = self.osd.render();
        fi.crop_43 = self.crop_43 && !self.tt_meta.active;
        fi.clear_burn = std::mem::take(&mut self.clear_burn);
        fi.reset_trails = std::mem::take(&mut self.reset_trails);

        let frame = match self.surface.get_current_texture() {
            wgpu::CurrentSurfaceTexture::Success(tex) | wgpu::CurrentSurfaceTexture::Suboptimal(tex) => tex,
            wgpu::CurrentSurfaceTexture::Outdated | wgpu::CurrentSurfaceTexture::Lost => {
                self.gpu.reconfigure(
                    &self.device,
                    &self.adapter,
                    &self.surface,
                    self.width,
                    self.height,
                    self.s.hdr_mode,
                    false,
                );
                return Ok(());
            }
            _ => return Ok(()),
        };
        let view = frame
            .texture
            .create_view(&wgpu::TextureViewDescriptor::default());
        let mut encoder = self.device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("victr_web_encoder"),
        });

        let crt_rect = [0.0, 0.0, self.width as f32, self.height as f32];
        self.gpu.render_crt(&self.device, &self.queue, &mut encoder, &fi, crt_rect);
        self.gpu.present(&self.device, &mut encoder, &view);
        self.queue.submit(std::iter::once(encoder.finish()));
        self.queue.present(frame);
        Ok(())
    }
}
