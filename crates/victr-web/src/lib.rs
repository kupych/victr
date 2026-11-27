//! Thin WASM + WebGPU entry point for **ViCTR Web (`onvic.tr`)**.
//!
//! Wraps `victr_core` (`Gpu`, `Sim`, `Settings`, `GuideState`, `TvOsd`,
//! `TeletextRenderer`, `OpenChannelWatchState`, and `audio_dsp::Processor`)
//! in a compact `wasm-bindgen` API driven by a lightweight HTML/CSS/JS shell.

#![cfg(target_arch = "wasm32")]

use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use std::sync::Arc;
use victr_core::audio_dsp::{self, AudioParams, AudioRing};
use victr_core::gpu::{FrameInput, Gpu};
use victr_core::guide::{GuideState, GuideView};
use victr_core::osd::TvOsd;
use victr_core::provider::{
    self, Channel, ChannelKind, MediaItem, OpenChannelWatchState, TuneTarget, make_callsign,
};
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
    guide_active: bool,
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

/// Open channel JSON definition accepted by `set_lineup_json` (compatible with `channels.json`).
#[derive(Deserialize, Clone, Debug)]
struct WebChannelDef {
    #[serde(default)]
    number: Option<u16>,
    #[serde(default)]
    id: String,
    name: String,
    #[serde(default)]
    callsign: String,
    /// `"live"` (default), `"series"` / `"collection"`, `"movies"`, or `"block"`.
    #[serde(default)]
    kind: String,
    #[serde(rename = "type", default)]
    channel_type: String,
    #[serde(default)]
    profile: Option<String>,
    #[serde(default)]
    stream_url: String,
    #[serde(default)]
    group: String,
    #[serde(default)]
    now_title: String,
    #[serde(default)]
    overview: String,
    #[serde(default)]
    live: Option<bool>,
    #[serde(default)]
    items: Vec<WebMediaItemDef>,
}

#[derive(Deserialize, Clone, Debug)]
struct WebMediaItemDef {
    #[serde(default)]
    id: String,
    title: String,
    #[serde(default)]
    season: Option<u32>,
    #[serde(default)]
    episode: Option<u32>,
    #[serde(default)]
    overview: String,
    #[serde(default)]
    runtime_secs: Option<f64>,
    stream_url: String,
}

#[derive(Serialize, Deserialize, Default)]
struct PersistedWebState {
    #[serde(default)]
    watch_state: OpenChannelWatchState,
    #[serde(default)]
    favorite_items: BTreeSet<String>,
    #[serde(default)]
    favorite_channels: BTreeSet<String>,
}

#[derive(Serialize)]
struct JsTuneTarget {
    channel_number: u16,
    source_key: String,
    callsign: String,
    channel_name: String,
    profile_override: Option<String>,
    stream_url: String,
    start_secs: f64,
    item_id: String,
    item_title: String,
    item_played: bool,
    item_favorite: bool,
    is_live_clock: bool,
    has_items: bool,
}

#[derive(Serialize)]
struct GuideActionResult {
    action: &'static str,
    target: Option<JsTuneTarget>,
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

    // Multi-channel lineup, 90s Prevue Cable Guide & persistent watch state
    raw_defs: Vec<WebChannelDef>,
    lineup: Vec<Channel>,
    guide: GuideState,
    persisted: PersistedWebState,
    current_channel: Option<u16>,
    active_item_id: Option<String>,
    now_playing_label: Option<String>,

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

fn now_unix_secs() -> i64 {
    (js_sys::Date::now() / 1000.0) as i64
}

fn build_channels_from_defs(defs: &[WebChannelDef]) -> Vec<Channel> {
    let mut out = Vec::with_capacity(defs.len());
    for (idx, def) in defs.iter().enumerate() {
        let name = def.name.trim().to_string();
        if name.is_empty() {
            continue;
        }
        let source_key = if def.id.trim().is_empty() {
            format!("web:{idx}:{name}")
        } else {
            format!("web:{}", def.id.trim())
        };
        let callsign = if !def.callsign.trim().is_empty() {
            def.callsign.trim().chars().take(5).collect::<String>().to_ascii_uppercase()
        } else {
            make_callsign(&name)
        };
        let kind_norm = def.kind.trim().to_ascii_lowercase();
        let items: Vec<MediaItem> = def
            .items
            .iter()
            .enumerate()
            .filter(|(_, m)| !m.stream_url.trim().is_empty())
            .map(|(mi, m)| MediaItem {
                id: if m.id.trim().is_empty() {
                    format!("{source_key}:item:{mi}:{}", m.title.trim())
                } else {
                    m.id.trim().to_string()
                },
                title: m.title.trim().to_string(),
                season: m.season,
                episode: m.episode,
                overview: m.overview.trim().to_string(),
                runtime_secs: m.runtime_secs.unwrap_or(0.0),
                played: false,
                resume_secs: 0.0,
                artwork_id: String::new(),
                stream_url: m.stream_url.trim().to_string(),
            })
            .collect();

        let ch_kind = if !items.is_empty() {
            if kind_norm == "movies" {
                ChannelKind::MovieGenre {
                    genre: if def.group.trim().is_empty() { name.clone() } else { def.group.trim().to_string() },
                    movies: items,
                }
            } else if kind_norm == "block" || kind_norm == "mix" || def.live == Some(true) {
                ChannelKind::Block {
                    overview: def.overview.trim().to_string(),
                    is_favorite: false,
                    live_clock: def.live.unwrap_or(true),
                    mode: "alternate".into(),
                    pending_loads: false,
                    items,
                }
            } else {
                let first_id = items.first().map(|m| m.id.clone());
                ChannelKind::Series {
                    series_id: source_key.clone(),
                    overview: def.overview.trim().to_string(),
                    is_continue: false,
                    is_favorite: false,
                    episodes: items,
                    next_up_episode_id: first_id,
                    loaded_episodes: true,
                }
            }
        } else if !def.stream_url.trim().is_empty() {
            ChannelKind::LiveStream {
                stream_url: def.stream_url.trim().to_string(),
                group: if def.group.trim().is_empty() { "Live".into() } else { def.group.trim().to_string() },
                now_title: if def.now_title.trim().is_empty() { name.clone() } else { def.now_title.trim().to_string() },
                overview: def.overview.trim().to_string(),
            }
        } else {
            continue;
        };

        let ch_type = if def.channel_type.trim().is_empty() {
            provider::default_channel_type(&ch_kind).to_string()
        } else {
            def.channel_type.trim().to_string()
        };

        out.push(Channel {
            number: def.number.filter(|&n| n >= 2).unwrap_or(0),
            provider_id: "web".into(),
            source_key,
            callsign,
            name,
            artwork_id: String::new(),
            channel_type: ch_type,
            profile_override: def.profile.clone().filter(|s| !s.trim().is_empty()),
            kind: ch_kind,
        });
    }
    out
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
            raw_defs: Vec::new(),
            lineup: Vec::new(),
            guide: GuideState::new(),
            persisted: PersistedWebState::default(),
            current_channel: None,
            active_item_id: None,
            now_playing_label: None,
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

    pub fn preset_names_json() -> String {
        serde_json::to_string(&settings::PRESETS).unwrap_or_else(|_| "[]".into())
    }

    fn rebuild_lineup(&mut self) {
        let mut raw = build_channels_from_defs(&self.raw_defs);
        provider::apply_open_watch_state(
            &mut raw,
            &self.persisted.watch_state,
            &self.persisted.favorite_items,
        );
        if raw.is_empty() {
            self.lineup.clear();
        } else {
            self.lineup = provider::merge_channel_lineup(raw);
        }
    }

    fn channel_by_number(&self, number: u16) -> Option<&Channel> {
        self.lineup.iter().find(|c| c.number == number)
    }

    fn visible_channels(&self) -> Vec<&Channel> {
        self.lineup.iter().collect()
    }

    fn guide_visible_channels(&self) -> Vec<&Channel> {
        let base = self.visible_channels();
        self.guide.filter_channels(&base, &self.persisted.favorite_channels)
    }

    fn guide_picker_items(&self, ch_num: u16) -> Vec<MediaItem> {
        self.channel_by_number(ch_num)
            .map(|c| {
                self.guide
                    .filter_items(c.items(), &self.persisted.favorite_items)
                    .into_iter()
                    .cloned()
                    .collect()
            })
            .unwrap_or_default()
    }

    fn wrap_target(&self, t: TuneTarget) -> JsTuneTarget {
        let has_items = self
            .channel_by_number(t.channel_number)
            .is_some_and(|c| !matches!(c.kind, ChannelKind::LiveStream { .. } | ChannelKind::Guide));
        let item_favorite = self.persisted.favorite_items.contains(&t.item.id);
        JsTuneTarget {
            channel_number: t.channel_number,
            source_key: t.source_key,
            callsign: t.callsign,
            channel_name: t.channel_name,
            profile_override: t.profile_override,
            stream_url: t.stream_url,
            start_secs: t.start_secs,
            item_id: t.item.id.clone(),
            item_title: t.item.short_label(),
            item_played: t.item.played,
            item_favorite,
            is_live_clock: t.is_live_clock,
            has_items,
        }
    }

    // ---- Lineup & Persistent Watch State API

    pub fn set_lineup_json(&mut self, json: &str) -> usize {
        if let Ok(defs) = serde_json::from_str::<Vec<WebChannelDef>>(json) {
            self.raw_defs = defs;
            self.rebuild_lineup();
        }
        self.lineup.len()
    }

    pub fn import_watch_state_json(&mut self, json: &str) {
        if let Ok(st) = serde_json::from_str::<PersistedWebState>(json) {
            self.persisted = st;
            self.rebuild_lineup();
        }
    }

    pub fn export_watch_state_json(&self) -> String {
        serde_json::to_string(&self.persisted).unwrap_or_else(|_| "{}".into())
    }

    pub fn set_active_context(&mut self, ch_num: u16, source_key: &str, item_id: &str, label: &str) {
        self.current_channel = (ch_num > 0).then_some(ch_num);
        self.active_item_id = (!item_id.is_empty()).then(|| item_id.to_string());
        self.now_playing_label = (!label.is_empty()).then(|| label.to_string());
        if !source_key.is_empty() && !item_id.is_empty() && item_id != source_key {
            self.persisted.watch_state.record_last_item(source_key, item_id);
            self.rebuild_lineup();
        }
    }

    pub fn toggle_item_watched(&mut self, item_id: &str) -> bool {
        if item_id.trim().is_empty() {
            return false;
        }
        let cur = self
            .persisted
            .watch_state
            .items
            .get(item_id)
            .is_some_and(|e| e.played);
        let next = !cur;
        self.persisted.watch_state.set_played(item_id, next);
        self.rebuild_lineup();
        self.osd
            .show_action(if next { "MARKED WATCHED" } else { "MARKED UNWATCHED" });
        next
    }

    pub fn toggle_item_favorite(&mut self, item_id: &str, title: &str) -> bool {
        if item_id.trim().is_empty() {
            return false;
        }
        let added = if self.persisted.favorite_items.contains(item_id) {
            self.persisted.favorite_items.remove(item_id);
            false
        } else {
            self.persisted.favorite_items.insert(item_id.to_string());
            true
        };
        self.rebuild_lineup();
        let short: String = title.chars().take(18).collect();
        self.osd.show_action(&format!(
            "{} {}",
            if added { "FAV *" } else { "UNFAV" },
            short.to_ascii_uppercase()
        ));
        added
    }

    pub fn save_item_resume(&mut self, item_id: &str, pos_secs: f64, duration_secs: f64) {
        self.persisted
            .watch_state
            .save_resume(item_id, pos_secs, duration_secs);
        self.rebuild_lineup();
    }

    pub fn resolve_channel_json(&mut self, ch_num: u16) -> String {
        if ch_num == 1 {
            self.guide.active = true;
            self.guide.close_picker();
            self.sim.switch_channel();
            self.reset_trails = true;
            self.osd.show_channel(1, "GUIDE", "", self.s.mono);
            return serde_json::to_string(&GuideActionResult {
                action: "guide",
                target: None,
            })
            .unwrap_or_else(|_| "{}".into());
        }
        let Some(ch) = self.channel_by_number(ch_num).cloned() else {
            self.osd.show_action(&format!("NO CH {:02}", ch_num));
            return "{}".into();
        };
        let Some(t) = ch.resolve_now(now_unix_secs()) else {
            self.osd.show_action(&format!("CH {:02} OFF AIR", ch_num));
            return "{}".into();
        };
        self.guide.active = false;
        self.guide.close_picker();
        serde_json::to_string(&GuideActionResult {
            action: "tune",
            target: Some(self.wrap_target(t)),
        })
        .unwrap_or_else(|_| "{}".into())
    }

    pub fn step_episode_json(&self, ch_num: u16, current_item_id: &str, delta: i32) -> String {
        let Some(ch) = self.channel_by_number(ch_num) else {
            return "{}".into();
        };
        let target = if delta >= 0 {
            ch.next_after(current_item_id)
        } else {
            ch.prev_before(current_item_id)
        };
        match target {
            Some(t) => serde_json::to_string(&self.wrap_target(t)).unwrap_or_else(|_| "{}".into()),
            None => "{}".into(),
        }
    }

    pub fn surf_channel_num(&self, delta: i32) -> u16 {
        let nums: Vec<u16> = self.lineup.iter().map(|c| c.number).collect();
        if nums.is_empty() {
            return 0;
        }
        let cur = if self.guide.active { Some(1) } else { self.current_channel };
        let cur_idx = cur.and_then(|n| nums.iter().position(|&x| x == n)).unwrap_or(0);
        let len = nums.len() as isize;
        let next_idx = ((cur_idx as isize + delta as isize) % len + len) as usize % nums.len();
        nums[next_idx]
    }

    // ---- 90s Prevue Cable Guide Controls

    pub fn toggle_guide(&mut self) -> bool {
        if self.lineup.is_empty() {
            return false;
        }
        self.guide.active = !self.guide.active;
        self.sim.switch_channel();
        self.reset_trails = true;
        if self.guide.active {
            self.guide.close_picker();
            if let Some(cur) = self.current_channel {
                let (pos, len) = {
                    let vis = self.visible_channels();
                    (vis.iter().position(|c| c.number == cur), vis.len())
                };
                if let Some(pos) = pos {
                    self.guide.selected_idx = pos;
                    self.guide.clamp_selection(len);
                }
            }
            self.osd.show_channel(1, "GUIDE", "", self.s.mono);
        } else {
            self.guide.close_picker();
        }
        self.guide.active
    }

    pub fn is_guide_active(&self) -> bool {
        self.guide.active
    }

    pub fn is_guide_searching(&self) -> bool {
        self.guide.active && self.guide.search_active
    }

    pub fn guide_open_picker(&mut self) -> bool {
        let target_ch = if self.guide.active {
            if self.guide.picker_channel().is_some() {
                self.guide.close_picker();
                return false;
            }
            self.guide_visible_channels()
                .get(self.guide.selected_idx)
                .map(|c| c.number)
                .filter(|&n| n > 1)
        } else {
            self.current_channel.filter(|&n| n > 1)
        };
        let Some(ch_num) = target_ch else { return false };
        let Some(ch) = self.channel_by_number(ch_num).cloned() else { return false };
        if matches!(ch.kind, ChannelKind::LiveStream { .. } | ChannelKind::Guide) {
            return false;
        }
        let init_idx = ch.default_item_index(self.active_item_id.as_deref());
        self.guide.active = true;
        self.guide.open_picker(ch_num, init_idx, ch.items().len());
        self.reset_trails = true;
        true
    }

    pub fn guide_close_or_back(&mut self) -> bool {
        if !self.guide.active {
            return false;
        }
        if self.guide.search_active || !self.guide.search_query.is_empty() {
            self.guide.clear_search();
        } else if self.guide.picker_channel().is_some() {
            self.guide.close_picker();
        } else {
            self.toggle_guide();
        }
        true
    }

    pub fn guide_move(&mut self, delta: i32) {
        if !self.guide.active {
            return;
        }
        if let Some((pch, _)) = self.guide.picker_selected() {
            let len = self.guide_picker_items(pch).len();
            self.guide.picker_move(delta as isize, len);
        } else {
            let len = self.guide_visible_channels().len();
            self.guide.move_selection(delta as isize, len);
        }
    }

    pub fn guide_shift_h(&mut self, delta: i32) {
        if !self.guide.active {
            return;
        }
        if let Some((pch, _)) = self.guide.picker_selected() {
            let items = self.guide_picker_items(pch);
            self.guide.picker_jump_season(delta as isize, &items);
        } else {
            self.guide.shift_time(delta);
        }
    }

    pub fn guide_search_start(&mut self) {
        if self.guide.active {
            self.guide.start_search();
        }
    }

    pub fn guide_search_push(&mut self, ch: &str) {
        if let Some(c) = ch.chars().next() {
            self.guide.push_search_char(c);
        }
    }

    pub fn guide_search_pop(&mut self) {
        self.guide.pop_search_char();
    }

    pub fn guide_toggle_selected_watched(&mut self) -> bool {
        if let Some((pch, idx)) = self.guide.picker_selected() {
            if let Some(item) = self.guide_picker_items(pch).get(idx).cloned() {
                return self.toggle_item_watched(&item.id);
            }
        }
        false
    }

    pub fn guide_toggle_selected_favorite(&mut self) -> bool {
        if !self.guide.active {
            return false;
        }
        if let Some((pch, idx)) = self.guide.picker_selected() {
            if let Some(item) = self.guide_picker_items(pch).get(idx).cloned() {
                return self.toggle_item_favorite(&item.id, &item.short_label());
            }
        } else if let Some(ch) = self.guide_visible_channels().get(self.guide.selected_idx).cloned().cloned() {
            if ch.number > 1 {
                let key = GuideState::channel_fav_key(&ch);
                let added = if self.persisted.favorite_channels.contains(&key) {
                    self.persisted.favorite_channels.remove(&key);
                    false
                } else {
                    self.persisted.favorite_channels.insert(key);
                    true
                };
                self.osd.show_action(&format!(
                    "{} CH {:02} {}",
                    if added { "FAV *" } else { "UNFAV" },
                    ch.number,
                    ch.callsign
                ));
                return added;
            }
        }
        false
    }

    pub fn guide_confirm_json(&mut self) -> String {
        if !self.guide.active {
            return "{}".into();
        }
        self.guide.search_active = false;
        if let Some((pch, idx)) = self.guide.picker_selected() {
            if let Some(item) = self.guide_picker_items(pch).get(idx).cloned() {
                if let Some(ch) = self.channel_by_number(pch).cloned() {
                    let target = ch.tune_specific_item(&item, false);
                    self.guide.active = false;
                    self.guide.close_picker();
                    return serde_json::to_string(&GuideActionResult {
                        action: "tune",
                        target: Some(self.wrap_target(target)),
                    })
                    .unwrap_or_else(|_| "{}".into());
                }
            }
            return "{}".into();
        }
        let target_ch = self
            .guide_visible_channels()
            .get(self.guide.selected_idx)
            .map(|c| c.number);
        let Some(n) = target_ch else { return "{}".into() };
        if n == 1 {
            self.toggle_guide();
            return serde_json::to_string(&GuideActionResult {
                action: "close_guide",
                target: None,
            })
            .unwrap_or_else(|_| "{}".into());
        }
        self.resolve_channel_json(n)
    }

    /// Handles a click on the canvas at normalized `[0..1, 0..1]` coordinates while Guide is open.
    pub fn guide_click_json(&mut self, norm_x: f32, norm_y: f32) -> String {
        if !self.guide.active {
            return "{}".into();
        }
        // Map normalized canvas coords into CRT picture [0..1] accounting for 4:3 bezel framing
        let a = (self.width.max(1) as f32) / (self.height.max(1) as f32);
        let h = view_half_at_zoom(self.width, self.height, self.s.show_bezel, self.zoom);
        let fx = (norm_x * 2.0 - 1.0) * h * a + self.pan[0];
        let fy = (norm_y * 2.0 - 1.0) * h + self.pan[1];
        let u = (fx / (4.0 / 3.0)) * 0.5 + 0.5;
        let v = fy * 0.5 + 0.5;
        if !(0.0..=1.0).contains(&u) || !(0.0..=1.0).contains(&v) {
            return "{}".into();
        }
        if let Some((pch, _)) = self.guide.picker_selected() {
            let items = self.guide_picker_items(pch);
            if let Some(hit) = self.guide.picker_hit_test(u, v, items.len()) {
                if let GuideView::ItemPicker { selected_item, .. } = &mut self.guide.view {
                    *selected_item = hit;
                }
                self.guide.clamp_picker(items.len());
                return self.guide_confirm_json();
            }
        } else {
            let vis_len = self.guide_visible_channels().len();
            if let Some(hit) = self.guide.hit_test_row(u, v, vis_len) {
                self.guide.selected_idx = hit;
                return self.guide_confirm_json();
            }
        }
        "{}".into()
    }

    pub fn push_digit(&mut self, digit: &str) -> i32 {
        if let Some(ch) = digit.chars().next() {
            if let Some(ch_num) = self.osd.push_digit(ch) {
                return ch_num as i32;
            }
        }
        -1
    }

    pub fn poll_digit_commit(&mut self) -> i32 {
        self.osd.poll_digit_commit().map(|n| n as i32).unwrap_or(-1)
    }

    pub fn commit_digits(&mut self) -> i32 {
        self.osd.commit_digits().map(|n| n as i32).unwrap_or(-1)
    }

    // ---- CRT Hardware, Presets & Media API

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

    pub fn set_preset_by_name(&mut self, query: &str) -> bool {
        let q = query.trim().to_ascii_lowercase();
        if q.is_empty() {
            return false;
        }
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
            guide_active: self.guide.active,
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
        let is_video = s.pattern == Pattern::Video && self.has_video_media && !self.tt_meta.active && !self.guide.active;
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
            test_tone: s.test_tone && !is_video && !self.tt_meta.active && !self.guide.active,
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

    pub fn tick_and_render(&mut self, dt_secs: f32) -> Result<(), JsValue> {
        let dt = dt_secs.clamp(0.001, 0.1);
        if self.tt_meta.active && !self.guide.active {
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

        // Priority: 1. 90s Prevue Cable Guide (G) -> 2. 1980s Teletext carrier screen -> 3. Live Video / Pattern
        let override_image = if self.guide.active && !self.lineup.is_empty() {
            let picker_ch = self
                .guide
                .picker_channel()
                .and_then(|n| self.channel_by_number(n))
                .cloned();
            let channels: Vec<&Channel> = self.lineup.iter().collect();
            Some(self.guide.render(
                &channels,
                picker_ch.as_ref(),
                self.active_item_id.as_deref(),
                None,
                self.now_playing_label.as_deref(),
                None,
                &self.persisted.favorite_channels,
                &self.persisted.favorite_items,
            ))
        } else if self.tt_meta.active {
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
        let showing_overlay = override_image.is_some();
        if showing_overlay {
            fi.u.pattern = Pattern::Video.id();
        }
        fi.image = override_image;
        fi.osd = self.osd.render();
        fi.crop_43 = self.crop_43 && !showing_overlay;
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
