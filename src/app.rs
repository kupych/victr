use crate::audio::AudioEngine;
use crate::capture::{self, CapturePlayer, WindowInfo};
use crate::channels_file::{self, ChannelsFileManager};
use crate::custom_provider::CustomProviderManager;
use crate::folders::FoldersManager;
use crate::gpu::{FrameInput, HdrStatus};
use crate::guide::{GuideState, GuideView};
use crate::jellyfin::JellyfinManager;
use crate::m3u::M3uManager;
use crate::mpv::{Player, StartOpts};
use crate::osd::TvOsd;
use crate::plex::PlexManager;
use crate::provider::{self, ActiveWatch, ArtworkImage, Channel, ChannelKind, MediaItem, TuneTarget};
use crate::retro::{self, RetroManager, RetroPlayer};
use crate::settings::{self, AppConfig, Input, LastWatched, Mask, Pattern, PhosphorSet, Settings, StartupDestination, StartupPreset, TapeSpeed, UserPreset};
use crate::sim::Sim;
use crate::splash::{SplashEvent, SplashState};
use crate::web_channel::{self, WebPlayer};
use egui::{Color32, RichText, Slider};
use std::path::PathBuf;
use std::sync::Arc;

/// Output of one UI & simulation frame, consumed by the native wgpu/winit host.
pub struct FrameOutput {
    pub crt_rect: egui::Rect,
    pub fi: FrameInput,
    pub want_hdr: bool,
    pub prefer_pq: bool,
    pub fullscreen_cmd: Option<bool>,
    pub focus_cmd: bool,
    pub close_cmd: bool,
    pub take_screenshot: Option<PathBuf>,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum PresetSel {
    BuiltIn(usize),
    User(usize),
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum SettingsTab {
    Channels,
    Games,
    Picture,
    CrtTube,
    Sound,
}

impl SettingsTab {
    const ALL: [SettingsTab; 5] = [
        SettingsTab::Channels,
        SettingsTab::Games,
        SettingsTab::Picture,
        SettingsTab::CrtTube,
        SettingsTab::Sound,
    ];

    fn label(self) -> &'static str {
        match self {
            SettingsTab::Channels => "Channels & Sources",
            SettingsTab::Games => "Games & Capture",
            SettingsTab::Picture => "Picture & Signal",
            SettingsTab::CrtTube => "CRT Tube & Room",
            SettingsTab::Sound => "Sound & System",
        }
    }
}

pub struct CrtApp {
    s: Settings,
    sim: Sim,
    hdr_status: HdrStatus,
    hdr_seeded_peak: bool,
    hdr_prefer_pq: bool,
    gpu_error: Option<String>,
    zoom: f32,
    pan: [f32; 2],
    last_fi: FrameInput,
    image: Option<(u64, Arc<(usize, usize, Vec<u8>)>)>,
    image_name: String,
    image_counter: u64,
    clear_burn: bool,
    burn_now: bool,
    reset_trails: bool,
    preset_sel: PresetSel,
    user_presets: Vec<UserPreset>,
    app_cfg: AppConfig,
    save_preset_open: bool,
    save_preset_focus: bool,
    save_as_default: bool,
    preset_name_input: String,
    preset_msg: String,
    confirm_delete_idx: Option<usize>,
    degauss_mag_start: Option<f32>,
    degauss_pinned_t: Option<f32>,
    // slide-in right quick menu & floating tabbed full-settings window
    sidebar_open: bool,
    sidebar_anim: f32,
    handle_anim: f32,
    handle_vis: f32,
    sidebar_width: f32,
    sidebar_resizing: bool,
    settings_open: bool,
    settings_tab: SettingsTab,
    settings_rect: Option<egui::Rect>,
    pixel_perfect_req: bool,
    last_vis_rect: egui::Rect,
    last_full_h_px: f32,
    status_toast: Option<(String, std::time::Instant)>,
    message: String,
    screenshot: Option<PathBuf>,
    frames: u32,
    shot_requested: bool,
    last_instant: std::time::Instant,
    fps: f32,
    scan_clock: ScanClock,
    // video through mpv, live web page channels, libretro console & external window/V4L2 capture
    player: Option<Player>,
    web_player: Option<WebPlayer>,
    retro_player: Option<RetroPlayer>,
    capture_player: Option<CapturePlayer>,
    video_image: Option<(u64, Arc<(usize, usize, Vec<u8>)>)>,
    video_frame_no: u64,
    video_name: String,
    video_url: String,
    crop: bool,
    seek_drag: Option<f64>,
    seek_drag_last: f64,
    vcr_shuttle_speed: f32,
    vcr_rw_last_step: std::time::Instant,
    // Multi-provider 90s cable guide & TV OSD
    jellyfin: JellyfinManager,
    plex: PlexManager,
    folders: FoldersManager,
    m3u: M3uManager,
    custom: CustomProviderManager,
    channels_file: ChannelsFileManager,
    retro: RetroManager,
    lineup: Vec<Channel>,
    active_watch: Option<ActiveWatch>,
    guide: GuideState,
    osd: TvOsd,
    splash: SplashState,
    startup_dest_pending: bool,
    current_channel: Option<u16>,
    pending_tune_channel: Option<u16>,
    open_guide_on_connect: bool,
    notify_guide_synced: bool,
    last_ended_item: Option<String>,
    muted: bool,
    show_api_key: bool,
    show_plex_token: bool,
    jf_username: String,
    jf_password: String,
    folder_path_input: String,
    m3u_ch_name: String,
    m3u_ch_url: String,
    m3u_ch_group: String,
    m3u_ch_logo: String,
    m3u_ch_is_web: bool,
    m3u_pl_url: String,
    custom_ep_name: String,
    custom_ep_url: String,
    custom_ep_auth: String,
    custom_msg: String,
    new_type_name: String,
    channels_file_msg: String,
    retro_selected_core: String,
    capture_windows: Vec<WindowInfo>,
    capture_monitors: Vec<String>,
    capture_v4l2: Vec<String>,
    capture_custom_query: String,
    panel_season_filter: Option<u32>,
    // sound
    audio: Option<AudioEngine>,
    // linking with the person's own mpv
    inbox: Option<crate::link::Inbox>,
    use_my_mpv: bool,
    link_msg: String,
    about_msg: String,
    tv_hovered: bool,
    fullscreen: bool,
    pending_fullscreen: Option<bool>,
    pending_close: bool,
    quitting_since: Option<std::time::Instant>,
    shutdown_flushes: Vec<std::thread::JoinHandle<()>>,
    fs_since: std::time::Instant,
    last_pointer_move: std::time::Instant,
}

impl Drop for CrtApp {
    fn drop(&mut self) {
        // Save final local video timestamp if watching a standalone video
        if self.current_channel.is_none() {
            if let (Some(p), Some(LastWatched::LocalVideo { src, .. })) = (&self.player, &self.app_cfg.last_watched) {
                let pos = p.shared.lock().unwrap().pos;
                if pos > 2.0 {
                    let src = src.clone();
                    self.app_cfg.last_watched = Some(LastWatched::LocalVideo { src, pos_secs: pos });
                    let _ = settings::save_app_config(&self.app_cfg);
                }
            }
        }
        // Flush local folder / custom provider watch progress if active
        if let Some(w) = self.active_watch.take() {
            if w.dwell_secs >= 10.0 && !w.marked_played {
                if w.provider_id == "folders" {
                    self.folders.save_resume(&w.item_id, w.last_pos, w.duration_secs);
                } else if w.provider_id == "custom" {
                    self.custom.save_resume(&w.item_id, w.last_pos, w.duration_secs);
                }
            }
        }
        // Stop mpv / web browser / retro console / external capture first.
        self.player = None;
        self.web_player = None;
        self.retro_player = None;
        self.capture_player = None;
        for h in self.shutdown_flushes.drain(..) {
            let _ = h.join();
        }
        self.jellyfin.finish_watch_sync();
        self.plex.finish_watch_sync();
        if self.inbox.is_some() {
            crate::link::cleanup();
        }
    }
}

/// Turns an egui key/text event into an mpv key name for `keypress`.
fn mpv_key_name(ev: &egui::Event) -> Option<String> {
    use egui::{Event, Key};
    match ev {
        // printable characters arrive as text (respects the keyboard layout and Shift)
        Event::Text(t) => {
            let c = t.chars().next()?;
            Some(match c {
                ' ' => "SPACE".into(),
                '#' => "SHARP".into(),
                _ => c.to_string(),
            })
        }
        Event::Key { key, pressed: true, modifiers, .. } => {
            let base = match key {
                Key::ArrowLeft => "LEFT",
                Key::ArrowRight => "RIGHT",
                Key::ArrowUp => "UP",
                Key::ArrowDown => "DOWN",
                Key::Enter => "ENTER",
                Key::Escape => "ESC",
                Key::Backspace => "BS",
                Key::Tab => "TAB",
                Key::Delete => "DEL",
                Key::Insert => "INS",
                Key::Home => "HOME",
                Key::End => "END",
                Key::PageUp => "PGUP",
                Key::PageDown => "PGDWN",
                Key::F1 => "F1",
                Key::F2 => "F2",
                Key::F3 => "F3",
                Key::F4 => "F4",
                Key::F5 => "F5",
                Key::F6 => "F6",
                Key::F7 => "F7",
                Key::F8 => "F8",
                Key::F9 => "F9",
                Key::F10 => "F10",
                Key::F11 => "F11",
                Key::F12 => "F12",
                _ => {
                    // letters/digits with Ctrl or Alt don't produce text events
                    if modifiers.ctrl || modifiers.alt || modifiers.command {
                        let n = key.name();
                        if n.chars().count() == 1 {
                            let mut s = String::new();
                            if modifiers.ctrl || modifiers.command {
                                s.push_str("Ctrl+");
                            }
                            if modifiers.alt {
                                s.push_str("Alt+");
                            }
                            let ch = n.to_lowercase();
                            s.push_str(&if modifiers.shift { ch.to_uppercase() } else { ch });
                            return Some(s);
                        }
                    }
                    return None;
                }
            };
            let mut s = String::new();
            if modifiers.ctrl {
                s.push_str("Ctrl+");
            }
            if modifiers.alt {
                s.push_str("Alt+");
            }
            if modifiers.shift {
                s.push_str("Shift+");
            }
            s.push_str(base);
            Some(s)
        }
        _ => None,
    }
}

impl CrtApp {
    pub fn new(
        egui_ctx: &egui::Context,
        screenshot: Option<PathBuf>,
        preset: Option<usize>,
        zoom: Option<f32>,
        pattern: Option<usize>,
        mpv_src: Option<(String, f64)>,
    ) -> Self {
        let mut visuals = egui::Visuals::dark();
        visuals.panel_fill = Color32::from_rgb(35, 33, 30);
        visuals.window_fill = Color32::from_rgb(40, 37, 33);
        visuals.window_stroke = egui::Stroke::new(1.0, Color32::from_rgb(138, 126, 106));
        visuals.extreme_bg_color = Color32::from_rgb(18, 17, 15);
        visuals.faint_bg_color = Color32::from_rgb(46, 43, 39);
        visuals.override_text_color = Some(Color32::from_rgb(232, 224, 206));
        visuals.selection.bg_fill = Color32::from_rgb(198, 108, 34);
        visuals.selection.stroke = egui::Stroke::new(1.0, Color32::from_rgb(245, 205, 145));
        visuals.widgets.noninteractive.bg_fill = Color32::from_rgb(36, 34, 31);
        visuals.widgets.noninteractive.bg_stroke = egui::Stroke::new(1.0, Color32::from_rgb(82, 76, 68));
        visuals.widgets.inactive.bg_fill = Color32::from_rgb(48, 45, 41);
        visuals.widgets.inactive.weak_bg_fill = Color32::from_rgb(44, 41, 37);
        visuals.widgets.inactive.bg_stroke = egui::Stroke::new(1.0, Color32::from_rgb(106, 98, 86));
        visuals.widgets.hovered.bg_fill = Color32::from_rgb(76, 60, 44);
        visuals.widgets.hovered.weak_bg_fill = Color32::from_rgb(70, 56, 42);
        visuals.widgets.hovered.bg_stroke = egui::Stroke::new(1.0, Color32::from_rgb(218, 158, 84));
        visuals.widgets.active.bg_fill = Color32::from_rgb(182, 98, 32);
        visuals.widgets.active.bg_stroke = egui::Stroke::new(1.0, Color32::from_rgb(246, 204, 138));
        egui_ctx.set_theme(egui::Theme::Dark);
        egui_ctx.set_visuals_of(egui::Theme::Dark, visuals);
        egui_ctx.all_styles_mut(|style| {
            style.spacing.slider_width = 136.0;
            style.spacing.item_spacing = egui::vec2(8.0, 5.0);
        });

        let user_presets = settings::load_user_presets();
        let app_cfg = settings::load_app_config();

        let (preset_sel, mut s) = if let Some(idx) = preset {
            let idx = idx.min(settings::PRESETS.len() - 1);
            (PresetSel::BuiltIn(idx), settings::preset(idx))
        } else {
            match &app_cfg.default_preset {
                Some(StartupPreset::User(file_name)) => {
                    if let Some((pos, p)) = user_presets.iter().enumerate().find(|(_, p)| p.file_name == *file_name) {
                        (PresetSel::User(pos), p.settings.clone())
                    } else {
                        (PresetSel::BuiltIn(0), settings::preset(0))
                    }
                }
                Some(StartupPreset::BuiltIn(idx)) => {
                    let idx = (*idx).min(settings::PRESETS.len() - 1);
                    (PresetSel::BuiltIn(idx), settings::preset(idx))
                }
                None => (PresetSel::BuiltIn(0), settings::preset(0)),
            }
        };
        if let Some(p) = pattern {
            s.pattern = Pattern::ALL[p.min(Pattern::ALL.len() - 1)];
        }

        let mut sim = Sim::new();
        if screenshot.is_some() || !app_cfg.cold_power_on {
            sim.warm = 1.0;
            sim.degauss_t = 10.0;
        } else {
            // Cold CRT power-on: starts dark, fires auto-degauss & HV crackle, and slowly warms up.
            sim.warm = 0.0;
            sim.degauss_t = 0.0;
        }
        let audio = if screenshot.is_none() { Some(AudioEngine::start()) } else { None };
        let screenshot_mode = screenshot.is_some();
        let cli_has_media = mpv_src.is_some() || pattern.is_some();
        let show_splash = app_cfg.splash_screen && !screenshot_mode && !cli_has_media;
        let startup_dest_pending =
            !screenshot_mode && !cli_has_media && app_cfg.startup_dest != StartupDestination::DefaultPattern;
        let mut app = CrtApp {
            s,
            sim,
            hdr_status: HdrStatus::default(),
            hdr_seeded_peak: false,
            hdr_prefer_pq: cfg!(target_os = "linux"),
            gpu_error: None,
            zoom: zoom.unwrap_or(1.0).clamp(1.0, 60.0),
            pan: [0.0, 0.0],
            last_fi: FrameInput::default(),
            image: None,
            image_name: String::new(),
            image_counter: 0,
            clear_burn: false,
            burn_now: preset_sel == PresetSel::BuiltIn(2),
            reset_trails: true,
            preset_sel,
            user_presets,
            app_cfg,
            save_preset_open: false,
            save_preset_focus: false,
            save_as_default: false,
            preset_name_input: String::new(),
            preset_msg: String::new(),
            confirm_delete_idx: None,
            degauss_mag_start: None,
            degauss_pinned_t: None,
            sidebar_open: false,
            sidebar_anim: 0.0,
            handle_anim: 0.0,
            handle_vis: 1.0,
            sidebar_width: 310.0,
            sidebar_resizing: false,
            settings_open: false,
            settings_tab: SettingsTab::Channels,
            settings_rect: None,
            pixel_perfect_req: false,
            last_vis_rect: egui::Rect::NOTHING,
            last_full_h_px: 0.0,
            status_toast: None,
            message: String::new(),
            screenshot,
            frames: 0,
            shot_requested: false,
            last_instant: std::time::Instant::now(),
            fps: 60.0,
            scan_clock: ScanClock::default(),
            player: None,
            web_player: None,
            retro_player: None,
            capture_player: None,
            video_image: None,
            video_frame_no: 0,
            video_name: String::new(),
            video_url: String::new(),
            crop: false,
            seek_drag: None,
            seek_drag_last: 0.0,
            vcr_shuttle_speed: 0.0,
            vcr_rw_last_step: std::time::Instant::now(),
            jellyfin: if screenshot_mode && !std::env::var_os("VICTR_GUIDE_CONNECT").is_some() {
                JellyfinManager::new_offline()
            } else {
                JellyfinManager::new()
            },
            plex: if screenshot_mode && !std::env::var_os("VICTR_GUIDE_CONNECT").is_some() {
                PlexManager::new_offline()
            } else {
                PlexManager::new()
            },
            folders: if screenshot_mode && !std::env::var_os("VICTR_GUIDE_CONNECT").is_some() {
                FoldersManager::new_offline()
            } else {
                FoldersManager::new()
            },
            m3u: if screenshot_mode && !std::env::var_os("VICTR_GUIDE_CONNECT").is_some() {
                M3uManager::new_offline()
            } else {
                M3uManager::new()
            },
            custom: if screenshot_mode && !std::env::var_os("VICTR_GUIDE_CONNECT").is_some() {
                CustomProviderManager::new_offline()
            } else {
                CustomProviderManager::new()
            },
            channels_file: if screenshot_mode && !std::env::var_os("VICTR_GUIDE_CONNECT").is_some() {
                ChannelsFileManager::new_offline()
            } else {
                ChannelsFileManager::new()
            },
            retro: if screenshot_mode && !std::env::var_os("VICTR_GUIDE_CONNECT").is_some() {
                RetroManager::new_offline()
            } else {
                RetroManager::new()
            },
            lineup: Vec::new(),
            active_watch: None,
            guide: GuideState::new(),
            osd: TvOsd::new(),
            splash: SplashState::new(show_splash),
            startup_dest_pending,
            current_channel: None,
            pending_tune_channel: None,
            open_guide_on_connect: false,
            notify_guide_synced: false,
            last_ended_item: None,
            muted: false,
            show_api_key: false,
            show_plex_token: false,
            jf_username: String::new(),
            jf_password: String::new(),
            folder_path_input: String::new(),
            m3u_ch_name: String::new(),
            m3u_ch_url: String::new(),
            m3u_ch_group: String::new(),
            m3u_ch_logo: String::new(),
            m3u_ch_is_web: false,
            m3u_pl_url: String::new(),
            custom_ep_name: String::new(),
            custom_ep_url: String::new(),
            custom_ep_auth: String::new(),
            custom_msg: String::new(),
            new_type_name: String::new(),
            channels_file_msg: String::new(),
            retro_selected_core: String::new(),
            capture_windows: Vec::new(),
            capture_monitors: Vec::new(),
            capture_v4l2: Vec::new(),
            capture_custom_query: String::new(),
            panel_season_filter: None,
            audio,
            inbox: if screenshot_mode { None } else { Some(crate::link::listen(egui_ctx.clone())) },
            use_my_mpv: true,
            link_msg: String::new(),
            about_msg: String::new(),
            tv_hovered: false,
            fullscreen: false,
            pending_fullscreen: None,
            pending_close: false,
            quitting_since: None,
            shutdown_flushes: Vec::new(),
            fs_since: std::time::Instant::now(),
            last_pointer_move: std::time::Instant::now(),
        };
        // Seed audio parameters immediately so cold power-on crackle & degauss play from frame 0.
        app.rebuild_lineup();
        app.update_audio();
        if let Some((src, start)) = mpv_src {
            app.start_video_at(src, start);
        }
        app
    }

    pub fn set_hdr_status(&mut self, status: HdrStatus) {
        if !self.hdr_seeded_peak {
            let os_confirmed_hdr = status.reported_peak_nits.is_some_and(|p| p > 350.0)
                || status.reported_edr_headroom.is_some_and(|h| h > 1.15);
            // Respect explicit HDR preference stored inside a user's default startup profile.
            if matches!(self.preset_sel, PresetSel::BuiltIn(_)) {
                if os_confirmed_hdr && status.supported {
                    self.s.hdr_mode = true;
                }
                if let Some(peak) = status.reported_peak_nits {
                    self.s.hdr_peak_nits = peak.clamp(300.0, 2000.0);
                    self.hdr_seeded_peak = true;
                }
                if let Some(sdr) = status.reported_sdr_nits {
                    self.s.hdr_ui_nits = sdr.clamp(80.0, 400.0);
                }
            } else {
                self.hdr_seeded_peak = true;
            }
        }
        self.hdr_status = status;
    }

    pub fn prefer_pq(&self) -> bool {
        self.hdr_prefer_pq
    }

    pub fn want_hdr(&self) -> bool {
        self.s.hdr_mode
    }

    pub fn fps(&self) -> f32 {
        self.fps
    }

    pub fn set_monitor_hz(&mut self, hz: Option<f32>) {
        self.scan_clock.set_monitor_hz(hz);
    }

    /// Fullscreen = only the TV: the window fills the screen and the settings are hidden.
    fn set_fullscreen(&mut self, on: bool) {
        if on == self.fullscreen {
            return;
        }
        self.fullscreen = on;
        if on {
            // Start fullscreen cleanly tucked closed; handle remains accessible on the right edge.
            self.sidebar_open = false;
        }
        self.fs_since = std::time::Instant::now();
        self.last_pointer_move = std::time::Instant::now();
        self.reset_trails = true;
        self.pending_fullscreen = Some(on);
    }

    pub fn wants_start_fullscreen(&self) -> bool {
        self.app_cfg.start_fullscreen
    }

    /// Starts in fullscreen (the --fullscreen option or Start in fullscreen preference).
    pub fn start_fullscreen(&mut self, real_window: bool) {
        if real_window {
            self.set_fullscreen(true);
        } else {
            self.fullscreen = true;
            self.fs_since = std::time::Instant::now();
        }
    }

    pub fn sync_window_fullscreen(&mut self, is_fullscreen: bool) {
        if self.fullscreen
            && self.screenshot.is_none()
            && self.fs_since.elapsed().as_secs_f32() > 1.0
            && !is_fullscreen
        {
            self.fullscreen = false;
        }
    }

    pub fn set_sidebar_locked(&mut self, open: bool) {
        self.sidebar_open = open;
        self.sidebar_anim = if open { 1.0 } else { 0.0 };
        self.handle_anim = if open { 1.0 } else { 0.0 };
        self.handle_vis = if open { 1.0 } else { 0.0 };
        self.reset_trails = true;
    }

    pub fn request_pixel_perfect(&mut self) {
        self.pixel_perfect_req = true;
    }

    pub fn request_open_guide(&mut self) {
        self.open_guide_on_connect = true;
    }

    /// Pins the degauss clock to a specific timestamp (used by `--degauss` for captures).
    pub fn set_degauss_time(&mut self, t: f32) {
        self.sim.degauss();
        self.sim.degauss_t = t;
        self.degauss_pinned_t = Some(t);
    }

    fn current_preset_label(&self) -> String {
        match self.preset_sel {
            PresetSel::BuiltIn(i) => settings::PRESETS.get(i).copied().unwrap_or("Custom").to_string(),
            PresetSel::User(i) => self.user_presets.get(i).map(|p| p.name.clone()).unwrap_or_else(|| "Custom".into()),
        }
    }

    fn find_preset_by_name(&self, name: &str) -> Option<PresetSel> {
        let q = name.trim();
        if q.is_empty() {
            return None;
        }
        if let Some(pos) = self
            .user_presets
            .iter()
            .position(|p| p.name.eq_ignore_ascii_case(q) || p.file_name.eq_ignore_ascii_case(q))
        {
            return Some(PresetSel::User(pos));
        }
        if let Some(pos) = settings::PRESETS.iter().position(|n| n.eq_ignore_ascii_case(q)) {
            return Some(PresetSel::BuiltIn(pos));
        }
        None
    }

    /// Automatically switches to the TV profile mapped for `channel_type` (or `profile_override`),
    /// preserving active video playback, master sound volume/mute, A/V sync, and HDR display preferences.
    fn apply_channel_profile(&mut self, channel_type: &str, profile_override: Option<&str>) {
        let Some(profile_name) = self.channels_file.resolve_profile(channel_type, profile_override) else {
            return;
        };
        let Some(sel) = self.find_preset_by_name(&profile_name) else {
            return;
        };
        if sel == self.preset_sel {
            return;
        }
        let saved_sound = self.s.sound;
        let saved_vol = self.s.volume;
        let saved_sync = self.s.audio_sync_ms;
        let saved_hdr = self.s.hdr_mode;
        let saved_pw = self.s.hdr_paper_white;
        let saved_peak = self.s.hdr_peak_nits;
        let saved_ui = self.s.hdr_ui_nits;
        self.apply_preset(sel);
        self.s.sound = saved_sound;
        self.s.volume = saved_vol;
        self.s.audio_sync_ms = saved_sync;
        self.s.hdr_mode = saved_hdr;
        self.s.hdr_paper_white = saved_pw;
        self.s.hdr_peak_nits = saved_peak;
        self.s.hdr_ui_nits = saved_ui;
        if let Some(p) = &self.player {
            p.volume(if self.muted { 0.0 } else { (self.s.volume * 100.0) as f64 });
            p.audio_delay(self.s.audio_sync_ms as f64 / 1000.0);
        }
        self.status_toast = Some((
            format!("TV Profile: {} ({channel_type})", self.current_preset_label()),
            std::time::Instant::now(),
        ));
    }

    fn apply_preset(&mut self, sel: PresetSel) {
        let old = self.s.clone();
        let next = match sel {
            PresetSel::BuiltIn(i) => settings::preset(i),
            PresetSel::User(i) => match self.user_presets.get(i) {
                Some(p) => p.settings.clone(),
                None => return,
            },
        };
        self.preset_sel = sel;
        self.s = next;
        if old.pattern == Pattern::Image || old.pattern == Pattern::Video {
            self.s.pattern = old.pattern;
        }
        // keep the user's sound, HDR, and chaos novelty preferences when switching built-in presets
        if matches!(sel, PresetSel::BuiltIn(_)) {
            self.s.sound = old.sound;
            self.s.volume = old.volume;
            self.s.tv_speaker = old.tv_speaker;
            self.s.mono = old.mono;
            self.s.whine = old.whine;
            self.s.test_tone = old.test_tone;
            self.s.audio_sync_ms = old.audio_sync_ms;
            self.s.hdr_mode = old.hdr_mode;
            self.s.hdr_paper_white = old.hdr_paper_white;
            self.s.hdr_peak_nits = old.hdr_peak_nits;
            self.s.hdr_ui_nits = old.hdr_ui_nits;
            self.s.chaos = old.chaos;
            self.s.chaos_rate = old.chaos_rate;
        } else if let Some(p) = &self.player {
            p.volume((self.s.volume * 100.0) as f64);
            p.audio_delay(self.s.audio_sync_ms as f64 / 1000.0);
        }
        self.degauss_mag_start = None;
        self.reset_trails = true;
        if sel == PresetSel::BuiltIn(2) {
            self.burn_now = true;
        }
    }

    fn startup_preset_for(&self, sel: PresetSel) -> Option<StartupPreset> {
        match sel {
            PresetSel::BuiltIn(0) => None,
            PresetSel::BuiltIn(i) => Some(StartupPreset::BuiltIn(i)),
            PresetSel::User(i) => self.user_presets.get(i).map(|p| StartupPreset::User(p.file_name.clone())),
        }
    }

    fn is_startup_default(&self, sel: PresetSel) -> bool {
        match (&self.app_cfg.default_preset, sel) {
            (None, PresetSel::BuiltIn(0)) => true,
            (Some(StartupPreset::BuiltIn(a)), PresetSel::BuiltIn(b)) => *a == b,
            (Some(StartupPreset::User(fname)), PresetSel::User(i)) => {
                self.user_presets.get(i).is_some_and(|p| p.file_name == *fname)
            }
            _ => false,
        }
    }

    fn startup_preset_label(&self) -> String {
        match &self.app_cfg.default_preset {
            None => settings::PRESETS[0].to_string(),
            Some(StartupPreset::BuiltIn(i)) => settings::PRESETS.get(*i).copied().unwrap_or(settings::PRESETS[0]).to_string(),
            Some(StartupPreset::User(fname)) => self
                .user_presets
                .iter()
                .find(|p| p.file_name == *fname)
                .map(|p| p.name.clone())
                .unwrap_or_else(|| settings::PRESETS[0].to_string()),
        }
    }

    fn set_startup_preset(&mut self, target: Option<StartupPreset>) {
        self.app_cfg.default_preset = target;
        match settings::save_app_config(&self.app_cfg) {
            Ok(()) => {
                self.preset_msg = format!("Startup profile set to \"{}\".", self.startup_preset_label());
            }
            Err(e) => {
                self.preset_msg = e;
            }
        }
    }

    fn open_save_preset_dialog(&mut self) {
        self.preset_name_input = match self.preset_sel {
            PresetSel::User(i) => self.user_presets.get(i).map(|p| p.name.clone()).unwrap_or_default(),
            PresetSel::BuiltIn(_) => String::new(),
        };
        self.save_as_default = self.is_startup_default(self.preset_sel);
        self.preset_msg.clear();
        self.save_preset_open = true;
        self.save_preset_focus = true;
    }

    fn save_current_preset(&mut self, raw_name: &str, make_default: bool) -> bool {
        match settings::save_user_preset(raw_name, &self.s) {
            Ok(saved) => {
                self.user_presets = settings::load_user_presets();
                if let Some(pos) = self.user_presets.iter().position(|p| p.file_name == saved.file_name) {
                    self.preset_sel = PresetSel::User(pos);
                }
                if make_default {
                    self.app_cfg.default_preset = Some(StartupPreset::User(saved.file_name.clone()));
                    let _ = settings::save_app_config(&self.app_cfg);
                    self.preset_msg = format!("Saved preset \"{}\" as startup default.", saved.name);
                } else {
                    self.preset_msg = format!("Saved preset \"{}\".", saved.name);
                }
                self.confirm_delete_idx = None;
                true
            }
            Err(e) => {
                self.preset_msg = e;
                false
            }
        }
    }

    fn delete_user_preset_at(&mut self, idx: usize) {
        let Some(target) = self.user_presets.get(idx).cloned() else { return };
        match settings::delete_user_preset(&target.file_name) {
            Ok(()) => {
                self.user_presets = settings::load_user_presets();
                self.preset_sel = match self.preset_sel {
                    PresetSel::User(cur) if cur == idx => PresetSel::BuiltIn(0),
                    PresetSel::User(cur) if cur > idx => PresetSel::User(cur - 1),
                    other => other,
                };
                if self.app_cfg.default_preset == Some(StartupPreset::User(target.file_name.clone())) {
                    self.app_cfg.default_preset = None;
                    let _ = settings::save_app_config(&self.app_cfg);
                }
                self.confirm_delete_idx = None;
                self.preset_msg = format!("Deleted preset \"{}\".", target.name);
            }
            Err(e) => {
                self.preset_msg = e;
            }
        }
    }

    fn preset_controls(&mut self, ui: &mut egui::Ui) {
        egui::CollapsingHeader::new(RichText::new("Presets & Startup").strong())
            .default_open(false)
            .show(ui, |ui| {
                // Startup profile selector
                let mut new_startup: Option<Option<StartupPreset>> = None;
                let cur_startup_lbl = self.startup_preset_label();
                ui.horizontal(|ui| {
                    ui.label("Startup profile");
                    egui::ComboBox::from_id_salt("startup_profile")
                        .selected_text(&cur_startup_lbl)
                        .width(178.0)
                        .show_ui(ui, |ui| {
                            for (i, n) in settings::PRESETS.iter().enumerate() {
                                let target = if i == 0 { None } else { Some(StartupPreset::BuiltIn(i)) };
                                let sel = self.app_cfg.default_preset == target;
                                if ui.selectable_label(sel, *n).clicked() {
                                    new_startup = Some(target);
                                }
                            }
                            if !self.user_presets.is_empty() {
                                ui.separator();
                                ui.label(RichText::new("Saved presets").weak().small());
                                for p in &self.user_presets {
                                    let target = Some(StartupPreset::User(p.file_name.clone()));
                                    let sel = self.app_cfg.default_preset == target;
                                    if ui.selectable_label(sel, &p.name).clicked() {
                                        new_startup = Some(target);
                                    }
                                }
                            }
                        });
                    if !self.is_startup_default(self.preset_sel)
                        && ui
                            .small_button("Use current")
                            .on_hover_text("Load the currently active preset whenever ViCTR starts")
                            .clicked()
                    {
                        new_startup = Some(self.startup_preset_for(self.preset_sel));
                    }
                });
                if let Some(target) = new_startup {
                    self.set_startup_preset(target);
                }
                if ui
                    .checkbox(&mut self.app_cfg.cold_power_on, "Cold CRT power-on warm-up")
                    .on_hover_text("Start from a cold picture tube with slow filament fade-in, auto-degauss hum, and high-voltage faceplate crackle.")
                    .changed()
                {
                    let _ = settings::save_app_config(&self.app_cfg);
                }
                ui.horizontal(|ui| {
                    if ui
                        .checkbox(&mut self.app_cfg.splash_screen, "Startup splash screen")
                        .on_hover_text("Boot into RF static, snap to the classic blue screen (\"ViCTR - Virtual CRT Television Receiver\"), then tune into your startup destination.")
                        .changed()
                    {
                        let _ = settings::save_app_config(&self.app_cfg);
                    }
                    if ui
                        .small_button("Replay")
                        .on_hover_text("Preview the startup static -> blue screen splash sequence")
                        .clicked()
                    {
                        self.guide.active = false;
                        self.player = None;
                        self.web_player = None;
                        self.retro_player = None;
                        self.capture_player = None;
                        self.splash = SplashState::new(true);
                        self.startup_dest_pending =
                            self.app_cfg.startup_dest != StartupDestination::DefaultPattern;
                        self.reset_trails = true;
                    }
                });
                if ui
                    .checkbox(&mut self.app_cfg.start_fullscreen, "Start in fullscreen")
                    .on_hover_text("Automatically launch ViCTR in borderless fullscreen (F11 or Esc to exit).")
                    .changed()
                {
                    let _ = settings::save_app_config(&self.app_cfg);
                }
                let mut dest = self.app_cfg.startup_dest;
                ui.horizontal(|ui| {
                    ui.label("On startup open");
                    egui::ComboBox::from_id_salt("startup_dest")
                        .selected_text(dest.label())
                        .width(178.0)
                        .show_ui(ui, |ui| {
                            for d in StartupDestination::ALL {
                                ui.selectable_value(&mut dest, d, d.label());
                            }
                        });
                });
                if dest != self.app_cfg.startup_dest {
                    self.app_cfg.startup_dest = dest;
                    let _ = settings::save_app_config(&self.app_cfg);
                }
                if self.app_cfg.startup_dest == StartupDestination::LastWatched {
                    let note = match &self.app_cfg.last_watched {
                        Some(lw) => format!("Last watched: {}", lw.label()),
                        None => "No last watched item yet — will fall back to Cable Guide.".to_string(),
                    };
                    ui.label(RichText::new(note).weak().small());
                }
                ui.separator();

                let exists = self
                    .user_presets
                    .iter()
                    .any(|p| p.name.eq_ignore_ascii_case(self.preset_name_input.trim()));
                ui.horizontal(|ui| {
                    let te = ui.add(
                        egui::TextEdit::singleline(&mut self.preset_name_input)
                            .hint_text("New preset name…")
                            .desired_width(250.0),
                    );
                    let btn_label = if exists { "Overwrite" } else { "Save" };
                    let submit = ui.button(btn_label).clicked()
                        || (te.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)));
                    if submit {
                        let name = self.preset_name_input.clone();
                        if self.save_current_preset(&name, false) {
                            self.preset_name_input.clear();
                        }
                    }
                });
                if !self.user_presets.is_empty() {
                    ui.add_space(4.0);
                    ui.label(RichText::new("Saved presets").weak().small());
                    let mut load_idx = None;
                    let mut delete_idx = None;
                    let mut toggle_default_fname: Option<(String, bool)> = None;
                    for i in 0..self.user_presets.len() {
                        let name = self.user_presets[i].name.clone();
                        let fname = self.user_presets[i].file_name.clone();
                        let active = self.preset_sel == PresetSel::User(i);
                        let is_def = self.app_cfg.default_preset == Some(StartupPreset::User(fname.clone()));
                        ui.horizontal(|ui| {
                            if ui.selectable_label(active, &name).clicked() {
                                load_idx = Some(i);
                            }
                            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                if self.confirm_delete_idx == Some(i) {
                                    if ui.small_button("Cancel").clicked() {
                                        self.confirm_delete_idx = None;
                                    }
                                    if ui
                                        .add(egui::Button::new(RichText::new("Delete").color(Color32::from_rgb(245, 110, 95))).small())
                                        .clicked()
                                    {
                                        delete_idx = Some(i);
                                    }
                                } else {
                                    if ui.small_button("×").on_hover_text("Delete this saved preset").clicked() {
                                        self.confirm_delete_idx = Some(i);
                                    }
                                    let star_txt = if is_def {
                                        RichText::new("★").color(Color32::from_rgb(245, 198, 72))
                                    } else {
                                        RichText::new("☆").weak()
                                    };
                                    let star_tip = if is_def {
                                        "Startup default profile (click to reset to factory default)"
                                    } else {
                                        "Set as default startup profile"
                                    };
                                    if ui.add(egui::Button::new(star_txt).small()).on_hover_text(star_tip).clicked() {
                                        toggle_default_fname = Some((fname, is_def));
                                    }
                                }
                            });
                        });
                    }
                    if let Some(i) = load_idx {
                        self.apply_preset(PresetSel::User(i));
                        self.confirm_delete_idx = None;
                    }
                    if let Some((fname, was_def)) = toggle_default_fname {
                        if was_def {
                            self.set_startup_preset(None);
                        } else {
                            self.set_startup_preset(Some(StartupPreset::User(fname)));
                        }
                    }
                    if let Some(i) = delete_idx {
                        self.delete_user_preset_at(i);
                    }
                } else {
                    ui.label(RichText::new("Save your current tube, signal, and VCR settings to reuse them later.").weak().small());
                }
                if !self.preset_msg.is_empty() {
                    ui.label(RichText::new(&self.preset_msg).small().color(Color32::from_rgb(235, 175, 100)));
                }
                if let Some(dir) = settings::presets_dir() {
                    ui.label(RichText::new(format!("Stored in {}", dir.display())).weak().small());
                }
            });
    }

    fn start_video(&mut self, src: String) {
        self.start_video_at(src, 0.0);
    }

    fn begin_quit(&mut self) {
        // Second Ctrl+Q during power-down collapse exits immediately.
        if self.quitting_since.is_some() {
            self.pending_close = true;
            return;
        }
        self.player = None;
        self.web_player = None;
        self.retro_player = None;
        self.capture_player = None;
        self.sidebar_open = false;
        self.save_preset_open = false;
        if let Some(w) = self.active_watch.take() {
            if w.dwell_secs >= 10.0 && !w.marked_played {
                if w.provider_id == "folders" {
                    self.folders.save_resume(&w.item_id, w.last_pos, w.duration_secs);
                } else if w.provider_id == "custom" {
                    self.custom.save_resume(&w.item_id, w.last_pos, w.duration_secs);
                }
            }
        }
        if self.shutdown_flushes.is_empty() {
            if let Some(h) = self.jellyfin.finish_watch_async_joinable() {
                self.shutdown_flushes.push(h);
            }
            if let Some(h) = self.plex.finish_watch_async_joinable() {
                self.shutdown_flushes.push(h);
            }
        }
        if self.sim.power_on {
            self.sim.set_power(false);
            self.quitting_since = Some(std::time::Instant::now());
        } else {
            self.pending_close = true;
        }
    }

    fn has_channels(&self) -> bool {
        self.lineup.len() > 1
    }

    fn any_source_busy(&self) -> bool {
        self.jellyfin.busy || self.plex.busy || self.folders.busy || self.m3u.busy || self.custom.busy
    }

    fn rebuild_lineup(&mut self) {
        let mut raw = Vec::with_capacity(
            self.jellyfin.channels.len()
                + self.plex.channels.len()
                + self.folders.channels.len()
                + self.m3u.channels.len()
                + self.custom.channels.len()
                + self.channels_file.channels.len()
                + 1,
        );
        raw.extend(self.channels_file.channels.iter().cloned());
        if let Some(rc) = &self.retro.channel {
            raw.push(rc.clone());
        }
        raw.extend(self.jellyfin.channels.iter().cloned());
        raw.extend(self.plex.channels.iter().cloned());
        raw.extend(self.folders.channels.iter().cloned());
        raw.extend(self.m3u.channels.iter().cloned());
        raw.extend(self.custom.channels.iter().cloned());
        let block_res = channels_file::build_block_channels(&self.channels_file.cfg, &raw);
        if !block_res.hidden_source_keys.is_empty() {
            raw.retain(|c| {
                !block_res
                    .hidden_source_keys
                    .iter()
                    .any(|(p, sk)| c.provider_id == *p && c.source_key == *sk)
            });
        }
        raw.extend(block_res.channels);
        for (prov, sid) in block_res.pending_episode_fetches {
            match prov.as_str() {
                "jellyfin" => self.jellyfin.ensure_series_episodes(&sid),
                "plex" => self.plex.ensure_series_episodes(&sid),
                _ => {}
            }
        }
        channels_file::apply_overrides(&mut raw, &self.channels_file.cfg);
        if raw.is_empty() {
            self.lineup.clear();
            return;
        }
        self.lineup = provider::merge_channel_lineup(raw);
        // Keep active_watch & current_channel synced if channel numbers or types shifted
        if let Some(w) = &mut self.active_watch {
            if let Some(ch) = self
                .lineup
                .iter()
                .find(|c| c.provider_id == w.provider_id && c.source_key == w.source_key)
            {
                w.channel_number = ch.number;
                w.channel_type = ch.effective_type().to_string();
                if self.current_channel.is_some() {
                    self.current_channel = Some(ch.number);
                }
            }
        }
    }

    fn visible_channels(&self) -> Vec<&Channel> {
        provider::filter_visible_channels(
            &self.lineup,
            self.jellyfin.cfg.favorites_only,
            &self.jellyfin.cfg.filter_text,
        )
    }

    fn channel_by_number(&self, number: u16) -> Option<&Channel> {
        self.lineup.iter().find(|c| c.number == number)
    }

    fn ensure_channel_episodes(&mut self, ch: &Channel) {
        if let ChannelKind::Series { series_id, loaded_episodes: false, .. } = &ch.kind {
            match ch.provider_id.as_str() {
                "jellyfin" => self.jellyfin.ensure_series_episodes(series_id),
                "plex" => self.plex.ensure_series_episodes(series_id),
                _ => {}
            }
        }
    }

    fn request_channel_artwork(&mut self, provider_id: &str, art_id: &str) -> Option<ArtworkImage> {
        if art_id.is_empty() {
            return None;
        }
        match provider_id {
            "jellyfin" => self.jellyfin.request_artwork(art_id),
            "plex" => self.plex.request_artwork(art_id),
            "folders" => self.folders.request_artwork(art_id),
            "m3u" => self.m3u.request_artwork(art_id),
            "custom" => self.custom.request_artwork(art_id),
            "channels" => self.channels_file.request_artwork(art_id),
            _ => None,
        }
    }

    fn set_channel_item_played(&mut self, channel_number: u16, item_id: &str, played: bool) {
        let provider_id = self
            .channel_by_number(channel_number)
            .map(|c| c.provider_id.clone())
            .unwrap_or_default();
        match provider_id.as_str() {
            "jellyfin" => self.jellyfin.set_item_played(item_id, played),
            "plex" => self.plex.set_item_played(item_id, played),
            "folders" => self.folders.set_item_played(item_id, played),
            "custom" => self.custom.set_item_played(item_id, played),
            _ => {}
        }
        if let Some(w) = &mut self.active_watch {
            if w.item_id == item_id {
                w.marked_played = played;
            }
        }
        self.rebuild_lineup();
    }

    fn finish_active_watch(&mut self) {
        let Some(w) = self.active_watch.take() else { return };
        match w.provider_id.as_str() {
            "jellyfin" => self.jellyfin.finish_watch(),
            "plex" => self.plex.finish_watch(),
            "folders" => {
                if w.dwell_secs >= 15.0 && !w.marked_played {
                    self.folders.save_resume(&w.item_id, w.last_pos, w.duration_secs);
                    self.rebuild_lineup();
                }
            }
            "custom" => {
                if w.dwell_secs >= 15.0 && !w.marked_played {
                    self.custom.save_resume(&w.item_id, w.last_pos, w.duration_secs);
                    self.rebuild_lineup();
                }
            }
            _ => {}
        }
    }

    fn refresh_all_sources(&mut self) {
        let mut started = false;
        if self.jellyfin.cfg.is_complete() && !self.jellyfin.busy {
            self.jellyfin.connect_and_load();
            started = true;
        }
        if self.plex.cfg.is_complete() && !self.plex.busy {
            self.plex.connect_and_load();
            started = true;
        }
        if self.folders.cfg.roots.iter().any(|r| r.enabled) && !self.folders.busy {
            self.folders.rescan_async();
            started = true;
        }
        if self.m3u.cfg.has_enabled_sources() && !self.m3u.busy {
            self.m3u.refresh_async();
            started = true;
        }
        if !self.custom.busy && (self.custom.has_enabled_sources() || crate::custom_provider::providers_dir().is_some_and(|d| d.is_dir())) {
            self.custom.refresh_async();
            started = true;
        }
        if self.channels_file.reload_from_disk() {
            self.rebuild_lineup();
            started = true;
        }
        self.retro.rescan();
        self.rebuild_lineup();
        if started {
            self.notify_guide_synced = true;
            self.osd.show_action("SYNCING GUIDE...");
        }
    }

    fn splash_waiting_for_sources(&self) -> bool {
        let jf_loading = !self.jellyfin.connected && self.jellyfin.cfg.is_complete() && self.jellyfin.busy;
        let px_loading = !self.plex.connected && self.plex.cfg.is_complete() && self.plex.busy;
        let dir_loading = self.folders.channels.is_empty()
            && self.folders.cfg.roots.iter().any(|r| r.enabled)
            && self.folders.busy;
        let m3u_loading = self.m3u.channels.is_empty() && self.m3u.cfg.has_enabled_sources() && self.m3u.busy;
        let cus_loading = self.custom.channels.is_empty() && self.custom.has_enabled_sources() && self.custom.busy;
        if !jf_loading && !px_loading && !dir_loading && !m3u_loading && !cus_loading {
            return false;
        }
        if self.open_guide_on_connect {
            return true;
        }
        if !self.startup_dest_pending {
            return false;
        }
        match self.app_cfg.startup_dest {
            StartupDestination::Guide => true,
            StartupDestination::LastWatched => !matches!(self.app_cfg.last_watched, Some(LastWatched::LocalVideo { .. })),
            StartupDestination::DefaultPattern => false,
        }
    }

    fn apply_startup_destination(&mut self) {
        let ready = self.has_channels();
        let still_loading = self.any_source_busy();
        if self.open_guide_on_connect {
            if ready {
                self.open_guide_on_connect = false;
                self.startup_dest_pending = false;
                self.guide.active = true;
                self.sim.switch_channel();
                self.reset_trails = true;
                self.osd.show_channel(1, "GUIDE", "", self.s.mono);
            }
            return;
        }
        if !self.startup_dest_pending {
            return;
        }
        match self.app_cfg.startup_dest {
            StartupDestination::DefaultPattern => {
                self.startup_dest_pending = false;
            }
            StartupDestination::Guide => {
                if ready && !still_loading {
                    self.startup_dest_pending = false;
                    self.guide.active = true;
                    self.sim.switch_channel();
                    self.reset_trails = true;
                    self.osd.show_channel(1, "GUIDE", "", self.s.mono);
                } else if !still_loading {
                    self.startup_dest_pending = false;
                }
            }
            StartupDestination::LastWatched => match self.app_cfg.last_watched.clone() {
                Some(LastWatched::LocalVideo { src, pos_secs }) => {
                    self.startup_dest_pending = false;
                    self.sim.switch_channel();
                    self.start_video_at(src, pos_secs);
                }
                Some(LastWatched::Channel { provider_id, source_key, .. }) => {
                    let target_ch = self
                        .lineup
                        .iter()
                        .find(|c| c.provider_id == provider_id && c.source_key == source_key)
                        .map(|c| c.number);
                    if let Some(ch_num) = target_ch {
                        self.startup_dest_pending = false;
                        self.tune_channel(ch_num);
                    } else if !still_loading {
                        self.startup_dest_pending = false;
                        if ready {
                            self.guide.active = true;
                            self.sim.switch_channel();
                            self.osd.show_channel(1, "GUIDE", "", self.s.mono);
                        }
                    }
                }
                Some(LastWatched::JellyfinSeries { series_id, .. }) => {
                    let target_ch = self.lineup.iter().find_map(|c| match &c.kind {
                        ChannelKind::Series { series_id: sid, .. } if sid == &series_id => Some(c.number),
                        _ => None,
                    });
                    if let Some(ch_num) = target_ch {
                        self.startup_dest_pending = false;
                        self.tune_channel(ch_num);
                    } else if !still_loading {
                        self.startup_dest_pending = false;
                        if ready {
                            self.guide.active = true;
                            self.sim.switch_channel();
                            self.osd.show_channel(1, "GUIDE", "", self.s.mono);
                        }
                    }
                }
                Some(LastWatched::JellyfinGenre { genre }) => {
                    let target_ch = self.lineup.iter().find_map(|c| match &c.kind {
                        ChannelKind::MovieGenre { genre: g, .. } if g.eq_ignore_ascii_case(&genre) => Some(c.number),
                        _ => None,
                    });
                    if let Some(ch_num) = target_ch {
                        self.startup_dest_pending = false;
                        self.tune_channel(ch_num);
                    } else if !still_loading {
                        self.startup_dest_pending = false;
                        if ready {
                            self.guide.active = true;
                            self.sim.switch_channel();
                            self.osd.show_channel(1, "GUIDE", "", self.s.mono);
                        }
                    }
                }
                None => {
                    if ready && !still_loading {
                        self.startup_dest_pending = false;
                        self.guide.active = true;
                        self.sim.switch_channel();
                        self.osd.show_channel(1, "GUIDE", "", self.s.mono);
                    } else if !still_loading {
                        self.startup_dest_pending = false;
                    }
                }
            },
        }
    }

    fn toggle_guide(&mut self) {
        if !self.has_channels() {
            return;
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
        } else if let Some(cur) = self.current_channel {
            self.guide.close_picker();
            if let Some(w) = &self.active_watch {
                let cs = w.callsign.clone();
                let title = w.title.clone();
                self.osd.show_channel(cur, &cs, &title, self.s.mono);
            }
        }
    }

    fn apply_tune_target(&mut self, target: TuneTarget) {
        self.finish_active_watch();
        self.pending_tune_channel = None;
        self.last_ended_item = None;
        self.current_channel = Some(target.channel_number);
        self.guide.active = false;
        self.guide.close_picker();
        self.sim.switch_channel();
        self.active_watch = Some(ActiveWatch {
            channel_number: target.channel_number,
            provider_id: target.provider_id.clone(),
            source_key: target.source_key.clone(),
            channel_type: target.channel_type.clone(),
            item_id: target.item.id.clone(),
            title: target.item.short_label(),
            callsign: target.callsign.clone(),
            duration_secs: target.item.runtime_secs,
            dwell_secs: 0.0,
            reported_start: false,
            marked_played: target.item.played,
            last_progress_secs: 0.0,
            last_pos: target.start_secs,
            last_paused: false,
        });
        if target.provider_id == "jellyfin" {
            self.jellyfin.begin_watch(&target);
        } else if target.provider_id == "plex" {
            self.plex.begin_watch(&target);
        }
        // Automatically switch TV profile / preset if this channel's type (or override) maps to one
        self.apply_channel_profile(&target.channel_type, target.profile_override.as_deref());
        // Remember this channel as "Last watched" for startup auto-resume
        if !target.provider_id.is_empty() && target.provider_id != "guide" {
            self.app_cfg.last_watched = Some(LastWatched::Channel {
                provider_id: target.provider_id.clone(),
                source_key: target.source_key.clone(),
                channel_name: target.channel_name.clone(),
            });
            let _ = settings::save_app_config(&self.app_cfg);
        }
        self.start_video_at(target.stream_url.clone(), target.start_secs);
        self.video_name = format!("CH {:02} {}", target.channel_number, target.channel_name);
        let sub = if target.is_live_clock {
            format!("LIVE · {}", target.item.short_label())
        } else {
            target.item.short_label()
        };
        self.osd.show_channel(target.channel_number, &target.callsign, &sub, self.s.mono);
    }

    fn tune_specific(&mut self, channel_number: u16, item: MediaItem, from_start: bool) {
        let Some(ch) = self.channel_by_number(channel_number).cloned() else { return };
        let target = ch.tune_specific_item(&item, from_start);
        self.apply_tune_target(target);
    }

    fn open_episode_picker(&mut self, channel_number: u16) {
        if !self.has_channels() || channel_number <= 1 {
            return;
        }
        let Some(ch) = self.channel_by_number(channel_number).cloned() else { return };
        if matches!(ch.kind, ChannelKind::LiveStream { .. } | ChannelKind::Guide) {
            return;
        }
        self.ensure_channel_episodes(&ch);
        let active_id = self.active_watch.as_ref().map(|w| w.item_id.as_str());
        let init_idx = ch.default_item_index(active_id);
        self.guide.active = true;
        self.guide.open_picker(channel_number, init_idx, ch.items().len());
        self.reset_trails = true;
    }

    fn step_episode(&mut self, delta: isize) {
        let Some(w) = self.active_watch.as_ref() else { return };
        let ch_num = w.channel_number;
        let item_id = w.item_id.clone();
        let Some(ch) = self.channel_by_number(ch_num).cloned() else { return };
        let target = if delta > 0 {
            ch.next_after(&item_id)
        } else {
            ch.prev_before(&item_id)
        };
        if let Some(t) = target {
            self.apply_tune_target(t);
            self.osd.show_action(if delta > 0 { "NEXT EPISODE" } else { "PREV EPISODE" });
        } else {
            self.osd.show_action(if delta > 0 { "LAST EPISODE" } else { "FIRST EPISODE" });
        }
    }

    fn toggle_current_watched(&mut self) {
        let Some(w) = self.active_watch.as_ref() else { return };
        let ch_num = w.channel_number;
        let item_id = w.item_id.clone();
        let new_played = !w.marked_played;
        self.set_channel_item_played(ch_num, &item_id, new_played);
        self.osd.show_action(if new_played { "MARKED WATCHED" } else { "MARKED UNWATCHED" });
    }

    fn tune_channel(&mut self, number: u16) {
        if !self.has_channels() {
            return;
        }
        if number == 1 {
            self.guide.active = true;
            self.sim.switch_channel();
            self.reset_trails = true;
            self.osd.show_channel(1, "GUIDE", "", self.s.mono);
            return;
        }
        let Some(ch) = self.channel_by_number(number).cloned() else {
            self.osd.show_action(&format!("NO CH {:02}", number));
            return;
        };
        if let ChannelKind::Series { ref episodes, loaded_episodes, .. } = ch.kind {
            if !loaded_episodes {
                self.ensure_channel_episodes(&ch);
                if episodes.is_empty() {
                    self.pending_tune_channel = Some(number);
                    self.sim.switch_channel();
                    self.osd.show_channel(number, &ch.callsign, "TUNING...", self.s.mono);
                    return;
                }
            }
        } else if let ChannelKind::Block { ref items, pending_loads, .. } = ch.kind {
            if items.is_empty() && pending_loads {
                self.pending_tune_channel = Some(number);
                self.sim.switch_channel();
                self.osd.show_channel(number, &ch.callsign, "LOADING BLOCK...", self.s.mono);
                return;
            }
        }
        let now_unix = chrono::Utc::now().timestamp();
        let Some(target) = ch.resolve_now(now_unix) else {
            self.osd.show_action(&format!("CH {:02} OFF AIR", number));
            return;
        };
        self.apply_tune_target(target);
    }

    fn surf_channel(&mut self, delta: isize) {
        if !self.has_channels() {
            return;
        }
        let nums: Vec<u16> = self.visible_channels().iter().map(|c| c.number).collect();
        if nums.is_empty() {
            return;
        }
        let cur = if self.guide.active { Some(1) } else { self.current_channel };
        let cur_idx = cur.and_then(|n| nums.iter().position(|&x| x == n)).unwrap_or(0);
        let len = nums.len() as isize;
        let next_idx = ((cur_idx as isize + delta) % len + len) as usize % nums.len();
        self.tune_channel(nums[next_idx]);
    }

    fn adjust_volume(&mut self, delta: f32) {
        self.s.volume = (self.s.volume + delta).clamp(0.0, 1.0);
        if delta > 0.0 && self.muted {
            self.muted = false;
        }
        if let Some(p) = &self.player {
            p.volume(if self.muted { 0.0 } else { (self.s.volume * 100.0) as f64 });
        }
        self.osd.show_volume(self.s.volume, self.muted);
    }

    fn toggle_mute(&mut self) {
        self.muted = !self.muted;
        if let Some(p) = &self.player {
            p.volume(if self.muted { 0.0 } else { (self.s.volume * 100.0) as f64 });
        }
        self.osd.show_volume(self.s.volume, self.muted);
    }

    fn show_info_osd(&mut self) {
        if self.guide.active {
            self.osd.show_channel(1, "GUIDE", "", self.s.mono);
            return;
        }
        if let Some(w) = &self.active_watch {
            let sub = if let Some(p) = &self.player {
                let sh = p.shared.lock().unwrap();
                if sh.duration > 1.0 {
                    format!("{} ({}/{})", w.title, hms(sh.pos), hms(sh.duration))
                } else {
                    w.title.clone()
                }
            } else {
                w.title.clone()
            };
            self.osd.show_channel(w.channel_number, &w.callsign, &sub, self.s.mono);
        } else if let Some(cur) = self.current_channel {
            self.osd.show_channel(cur, "TV", "", self.s.mono);
        } else {
            self.osd.show_action("VIDEO INPUT");
        }
        if let Some(p) = &self.player {
            let (dur, paused) = {
                let sh = p.shared.lock().unwrap();
                (sh.duration, sh.paused)
            };
            if dur > 1.0 {
                let lbl = if self.vcr_shuttle_speed != 0.0 {
                    Self::vcr_shuttle_label(self.vcr_shuttle_speed)
                } else if paused {
                    "PAUSE ||"
                } else {
                    "PLAY >"
                };
                self.show_video_progress_osd(lbl, None);
            }
        }
    }

    fn pick_folder(&mut self) {
        if let Some(path) = rfd::FileDialog::new().pick_folder() {
            self.folders.add_root(path.to_string_lossy().to_string());
        }
    }

    fn pick_m3u_playlist(&mut self) {
        if let Some(path) = rfd::FileDialog::new()
            .add_filter("M3U / M3U8 Playlists", &["m3u", "m3u8"])
            .add_filter("All files", &["*"])
            .pick_file()
        {
            let label = path.file_stem().and_then(|s| s.to_str()).unwrap_or("Playlist").to_string();
            self.m3u.add_playlist(path.to_string_lossy().to_string(), label);
        }
    }

    fn pick_rom_file(&mut self) {
        if let Some(path) = rfd::FileDialog::new()
            .add_filter("Game ROMs", retro::ROM_EXTS)
            .add_filter("All files", &["*"])
            .pick_file()
        {
            let p = path.to_string_lossy().to_string();
            let core_hint = if self.retro_selected_core.is_empty() {
                None
            } else {
                Some(self.retro_selected_core.clone())
            };
            self.launch_rom_on_console_channel(&p, core_hint.as_deref());
        }
    }

    fn pick_rom_folder(&mut self) {
        if let Some(path) = rfd::FileDialog::new().pick_folder() {
            self.retro.add_rom_dir(path.to_string_lossy().to_string());
            self.rebuild_lineup();
        }
    }

    fn launch_rom_on_console_channel(&mut self, rom_path: &str, core_hint: Option<&str>) {
        let ch_num = self.retro.cfg.channel_number.max(2);
        let url = retro::make_retro_url(rom_path, core_hint);
        let title = std::path::Path::new(rom_path)
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("Cartridge")
            .to_string();
        self.guide.active = false;
        self.guide.close_picker();
        self.current_channel = Some(ch_num);
        self.sim.switch_channel();
        self.apply_channel_profile("game", None);
        self.start_video_at(url, 0.0);
        self.video_name = format!("CH {:02} GAME · {title}", ch_num);
        self.osd.show_channel(ch_num, "GAME", &title, self.s.mono);
    }

    fn retro_gamepad_selector(&mut self, ui: &mut egui::Ui, id_salt: &str) {
        let cur = self.retro.cfg.gamepad_device.clone();
        let cur_label = if cur.is_empty() {
            "Auto (all gamepads)".to_string()
        } else {
            self.retro
                .gamepads
                .iter()
                .find(|g| g.id == cur || g.path == cur)
                .map(|g| g.display_label())
                .unwrap_or_else(|| format!("{cur} (disconnected)"))
        };
        let mut chosen: Option<String> = None;
        let mut refresh = false;
        ui.horizontal(|ui| {
            ui.label("Controller");
            let cb_w = (ui.available_width() - 34.0).clamp(115.0, 260.0);
            egui::ComboBox::from_id_salt(id_salt)
                .selected_text(cur_label)
                .width(cb_w)
                .show_ui(ui, |ui| {
                    if ui
                        .selectable_label(cur.is_empty(), "Auto (all gamepads)")
                        .on_hover_text("Automatically uses all connected gamepads and skips RGB/LED/mouse HID devices.")
                        .clicked()
                    {
                        chosen = Some(String::new());
                    }
                    let real_gps: Vec<_> = self.retro.gamepads.iter().filter(|g| g.is_gamepad).cloned().collect();
                    let other_gps: Vec<_> = self.retro.gamepads.iter().filter(|g| !g.is_gamepad).cloned().collect();
                    if !real_gps.is_empty() {
                        ui.separator();
                        ui.label(RichText::new("Detected gamepads").weak().small());
                        for g in &real_gps {
                            let sel = cur == g.id || cur == g.path;
                            if ui.selectable_label(sel, g.display_label()).clicked() {
                                chosen = Some(g.id.clone());
                            }
                        }
                    }
                    if !other_gps.is_empty() {
                        ui.separator();
                        ui.label(RichText::new("Other js* devices (non-gamepad)").weak().small());
                        for g in &other_gps {
                            let sel = cur == g.id || cur == g.path;
                            if ui.selectable_label(sel, g.display_label()).clicked() {
                                chosen = Some(g.id.clone());
                            }
                        }
                    }
                });
            if ui
                .small_button("↻")
                .on_hover_text("Rescan connected USB/Bluetooth controllers")
                .clicked()
            {
                refresh = true;
            }
        });
        if refresh {
            self.retro.refresh_gamepads();
        }
        if let Some(dev) = chosen {
            let osd_name = if dev.is_empty() {
                "AUTO GAMEPADS".to_string()
            } else {
                self.retro
                    .gamepads
                    .iter()
                    .find(|g| g.id == dev)
                    .map(|g| if g.name.is_empty() { g.id.clone() } else { g.name.clone() })
                    .unwrap_or_else(|| dev.clone())
            };
            self.retro.set_gamepad_device(dev.clone());
            if let Some(rp) = &self.retro_player {
                rp.set_gamepad(dev);
            }
            self.osd.show_action(&format!("PAD: {osd_name}"));
        }
    }

    fn retro_running_controls(&mut self, ui: &mut egui::Ui) {
        let Some(rp) = &self.retro_player else { return };
        let (gname, cname, status, active_gp, paused, running) = {
            let sh = rp.shared.lock().unwrap();
            (
                sh.game_name.clone(),
                sh.core_name.clone(),
                sh.status.clone(),
                sh.active_gamepad.clone(),
                sh.paused,
                sh.running,
            )
        };
        let hdr = if running {
            format!("Game Console · {gname}")
        } else {
            format!("Game Console (CH {:02})", self.retro.cfg.channel_number.max(2))
        };
        egui::CollapsingHeader::new(RichText::new(hdr).strong().color(Color32::from_rgb(138, 240, 178)))
            .id_salt("retro_running_menu")
            .default_open(true)
            .show(ui, |ui| {
                if !cname.is_empty() {
                    ui.label(RichText::new(format!("{status}")).weak().small());
                } else if !status.is_empty() {
                    ui.label(RichText::new(status).weak().small());
                }
                self.retro_gamepad_selector(ui, "retro_gp_running");
                if running && !active_gp.is_empty() {
                    ui.label(
                        RichText::new(format!("Active: {active_gp}"))
                            .small()
                            .color(Color32::from_rgb(156, 224, 186)),
                    );
                }
                let mut eject = false;
                if let Some(rp) = &self.retro_player {
                    ui.add_space(2.0);
                    ui.horizontal(|ui| {
                        if running {
                            if ui.small_button(if paused { "Resume" } else { "Pause" }).clicked() {
                                rp.toggle_pause();
                            }
                            if ui.small_button("Reset (F1)").clicked() {
                                rp.reset();
                                self.osd.show_action("CONSOLE RESET");
                            }
                            if ui.small_button("Save (F2)").clicked() {
                                rp.save_state();
                                self.osd.show_action("STATE SAVED");
                            }
                            if ui.small_button("Load (F4)").clicked() {
                                rp.load_state();
                                self.osd.show_action("STATE LOADED");
                            }
                            if ui.small_button("Eject").clicked() {
                                eject = true;
                            }
                        } else if ui.small_button("Load ROM…").clicked() {
                            self.message = "pickrom".into();
                        }
                    });
                }
                if eject {
                    self.retro_player = None;
                }
            });
        ui.separator();
    }

    fn open_settings_tab(&mut self, tab: SettingsTab) {
        self.settings_tab = tab;
        self.settings_open = true;
    }

    fn channels_sources_controls(&mut self, ui: &mut egui::Ui) {
        ui.label(
            RichText::new("Combine Jellyfin, Plex, nested local folders, M3U8 live streams, live web pages, and custom integrators into one 90s Prevue Cable Guide.")
                .weak()
                .small(),
        );

        // ---- Unified Guide & On-Air controls (shown whenever any source has channels)
        if self.has_channels() {
                    ui.add_space(4.0);
                    ui.horizontal(|ui| {
                        let lbl = if self.guide.active { "Exit Cable Guide (G)" } else { "Open Cable Guide (G)" };
                        if ui.button(lbl).clicked() {
                            self.toggle_guide();
                        }
                        if ui.small_button("Ch -").clicked() {
                            self.surf_channel(-1);
                        }
                        if ui.small_button("Ch +").clicked() {
                            self.surf_channel(1);
                        }
                        if ui
                            .add_enabled(!self.any_source_busy(), egui::Button::new("Refresh").small())
                            .on_hover_text("Reload channels from all enabled sources (F5 / R in guide)")
                            .clicked()
                        {
                            self.refresh_all_sources();
                        }
                    });
                    let mut cfg_changed = false;
                    if ui
                        .checkbox(&mut self.jellyfin.cfg.favorites_only, "Favorites & Continue Watching only")
                        .changed()
                    {
                        cfg_changed = true;
                    }
                    ui.horizontal(|ui| {
                        ui.label("Filter");
                        if ui
                            .add(
                                egui::TextEdit::singleline(&mut self.jellyfin.cfg.filter_text)
                                    .hint_text("Search channels or source…")
                                    .desired_width(210.0),
                            )
                            .changed()
                        {
                            cfg_changed = true;
                        }
                        if !self.jellyfin.cfg.filter_text.is_empty() && ui.small_button("×").clicked() {
                            self.jellyfin.cfg.filter_text.clear();
                            cfg_changed = true;
                        }
                    });
                    if cfg_changed {
                        let _ = crate::jellyfin::save_config(&self.jellyfin.cfg);
                    }

                    // Active playback quick actions (Mark Watched, Next/Prev Episode, Restart, Episode Picker)
                    if let Some(w) = &self.active_watch {
                        let ch_num = w.channel_number;
                        let title = w.title.clone();
                        let played = w.marked_played;
                        let badge = provider::source_badge(&w.provider_id);
                        let ch_type = w.channel_type.clone();
                        ui.separator();
                        ui.label(
                            RichText::new(format!("On air [{badge} · {ch_type}]: CH {:02} · {}", ch_num, title))
                                .strong()
                                .small()
                                .color(Color32::from_rgb(138, 240, 178)),
                        );
                        ui.horizontal(|ui| {
                            if ui.small_button("Prev Ep").on_hover_text("Previous episode (P)").clicked() {
                                self.step_episode(-1);
                            }
                            if ui.small_button("Next Ep").on_hover_text("Next episode (N)").clicked() {
                                self.step_episode(1);
                            }
                            let w_lbl = if played { "Mark Unwatched" } else { "Mark Watched" };
                            if ui.small_button(w_lbl).on_hover_text("Toggle watched status (W)").clicked() {
                                self.toggle_current_watched();
                            }
                        });
                        ui.horizontal(|ui| {
                            if ui.small_button("Pick Episode (E)").on_hover_text("Browse all episodes on the CRT guide (E)").clicked() {
                                self.open_episode_picker(ch_num);
                            }
                            if ui.small_button("Restart 0:00").on_hover_text("Restart from the beginning (R)").clicked() {
                                if let Some(p) = &self.player {
                                    p.seek_abs(0.0);
                                    self.osd.show_action("RESTART 0:00");
                                }
                            }
                        });
                    }

                    // Quick Episode / Movie browser for the active (or guide-selected) channel
                    let browse_ch_num = self
                        .guide
                        .picker_channel()
                        .or(self.current_channel)
                        .or_else(|| {
                            self.visible_channels()
                                .get(self.guide.selected_idx)
                                .filter(|c| c.number > 1)
                                .map(|c| c.number)
                        });
                    if let Some(bch_num) = browse_ch_num {
                        if let Some(bch) = self.channel_by_number(bch_num).cloned() {
                            if !matches!(bch.kind, ChannelKind::Guide | ChannelKind::LiveStream { .. }) {
                                self.ensure_channel_episodes(&bch);
                                let items = bch.items().to_vec();
                                let bch_remote_series = if bch.provider_id == "jellyfin" || bch.provider_id == "plex" {
                                    match &bch.kind {
                                        ChannelKind::Series { series_id, .. } => Some((bch.provider_id.clone(), series_id.clone())),
                                        _ => None,
                                    }
                                } else {
                                    None
                                };
                                let badge = provider::source_badge(&bch.provider_id);
                                let header = format!("Episodes / Movies — CH {:02} {} [{badge}] ({})", bch.number, bch.name, items.len());
                                let mut play_req: Option<(MediaItem, bool)> = None;
                                let mut watch_toggle: Option<(String, bool)> = None;
                                let mut reload_sid: Option<(String, String)> = None;
                                egui::CollapsingHeader::new(RichText::new(header).small().strong())
                                    .id_salt("ch_ep_browser")
                                    .default_open(false)
                                    .show(ui, |ui| {
                                        let mut seasons: Vec<u32> = items.iter().filter_map(|m| m.season).collect();
                                        seasons.sort_unstable();
                                        seasons.dedup();
                                        if let Some(sf) = self.panel_season_filter {
                                            if !seasons.contains(&sf) {
                                                self.panel_season_filter = None;
                                            }
                                        }
                                        ui.horizontal_wrapped(|ui| {
                                            if seasons.len() > 1 {
                                                if ui.selectable_label(self.panel_season_filter.is_none(), "All").clicked() {
                                                    self.panel_season_filter = None;
                                                }
                                                for s in &seasons {
                                                    let lbl = if *s == 0 { "Specials".to_string() } else { format!("S{s}") };
                                                    if ui
                                                        .selectable_label(self.panel_season_filter == Some(*s), lbl)
                                                        .clicked()
                                                    {
                                                        self.panel_season_filter = Some(*s);
                                                    }
                                                }
                                            }
                                            if let Some((prov, sid)) = &bch_remote_series {
                                                if ui.small_button("Reload list").on_hover_text("Re-fetch all episodes for this series from the server").clicked() {
                                                    reload_sid = Some((prov.clone(), sid.clone()));
                                                }
                                            }
                                        });
                                        let active_id = self.active_watch.as_ref().map(|w| w.item_id.as_str());
                                        egui::ScrollArea::vertical()
                                            .id_salt("ch_ep_scroll")
                                            .max_height(210.0)
                                            .show(ui, |ui| {
                                                for item in &items {
                                                    if let Some(sf) = self.panel_season_filter {
                                                        if item.season != Some(sf) {
                                                            continue;
                                                        }
                                                    }
                                                    let is_now = active_id == Some(item.id.as_str());
                                                    ui.horizontal(|ui| {
                                                        let mut checked = item.played;
                                                        if ui
                                                            .checkbox(&mut checked, "")
                                                            .on_hover_text("Toggle watched status")
                                                            .changed()
                                                        {
                                                            watch_toggle = Some((item.id.clone(), checked));
                                                        }
                                                        let lbl = item.short_label();
                                                        let r = ui.selectable_label(is_now, lbl);
                                                        if r.clicked() {
                                                            play_req = Some((item.clone(), false));
                                                        }
                                                    });
                                                }
                                            });
                                    });
                                if let Some((prov, sid)) = reload_sid {
                                    if prov == "jellyfin" {
                                        self.jellyfin.reload_series_episodes(&sid);
                                    } else if prov == "plex" {
                                        self.plex.reload_series_episodes(&sid);
                                    }
                                }
                                if let Some((iid, played)) = watch_toggle {
                                    self.set_channel_item_played(bch_num, &iid, played);
                                    self.osd.show_action(if played { "MARKED WATCHED" } else { "MARKED UNWATCHED" });
                                }
                                if let Some((item, from_start)) = play_req {
                                    self.tune_specific(bch_num, item, from_start);
                                }
                            }
                        }
                    }

                    ui.label(
                        RichText::new("Remote: G Guide · E Episodes · W Watched · N/P Next/Prev · 0-9 Dial · PgUp/Dn Ch · -/+ Vol · M Mute · R Restart · I Info")
                            .weak()
                            .small(),
                    );
                    ui.separator();
                }

                // ---- Editable channels.json & Channel Type -> TV Profile Automation
                let tp_count = self.channels_file.cfg.type_profiles.values().filter(|v| !v.trim().is_empty()).count();
                let cf_hdr = if tp_count > 0 || self.channels_file.file_exists {
                    format!("channels.json & TV Profiles ({tp_count} auto)")
                } else {
                    "channels.json & TV Profiles".to_string()
                };
                egui::CollapsingHeader::new(RichText::new(cf_hdr).strong().small())
                    .id_salt("src_channels_file")
                    .default_open(self.channels_file.file_exists || tp_count > 0)
                    .show(ui, |ui| {
                        ui.label(
                            RichText::new("Auto-switch TV presets by channel type (e.g. series -> Broadcast, movies -> VHS) and pin/define/override channels in channels.json (auto-reloads on save).")
                                .weak()
                                .small(),
                        );
                        ui.add_space(3.0);

                        // Build list of available profile names (saved user presets first, then built-in)
                        let mut preset_names: Vec<String> = self.user_presets.iter().map(|p| p.name.clone()).collect();
                        for b in settings::PRESETS {
                            preset_names.push(b.to_string());
                        }

                        // Always show the 5 standard types plus any custom types defined by the user
                        let mut display_types: Vec<String> = vec![
                            "series".into(),
                            "movies".into(),
                            "live".into(),
                            "web".into(),
                            "game".into(),
                        ];
                        for k in self.channels_file.cfg.type_profiles.keys() {
                            if !display_types.iter().any(|d| d.eq_ignore_ascii_case(k)) {
                                display_types.push(k.clone());
                            }
                        }

                        let mut set_mapping: Option<(String, String)> = None;
                        let mut remove_mapping: Option<String> = None;
                        for t_key in &display_types {
                            let cur_val = self
                                .channels_file
                                .cfg
                                .type_profiles
                                .iter()
                                .find(|(k, _)| k.eq_ignore_ascii_case(t_key))
                                .map(|(_, v)| v.clone())
                                .unwrap_or_default();
                            let sel_text = if cur_val.is_empty() {
                                "(No auto-switch)".to_string()
                            } else {
                                cur_val.clone()
                            };
                            let is_builtin = matches!(t_key.as_str(), "series" | "movies" | "live" | "web" | "game");
                            ui.horizontal(|ui| {
                                ui.label(RichText::new(format!("{t_key:<9}")).monospace().small());
                                egui::ComboBox::from_id_salt(format!("tp_combo_{t_key}"))
                                    .selected_text(&sel_text)
                                    .width(176.0)
                                    .show_ui(ui, |ui| {
                                        if ui.selectable_label(cur_val.is_empty(), "(No auto-switch)").clicked() {
                                            set_mapping = Some((t_key.clone(), String::new()));
                                        }
                                        if !self.user_presets.is_empty() {
                                            ui.separator();
                                            ui.label(RichText::new("Saved presets").weak().small());
                                            for p in &self.user_presets {
                                                if ui
                                                    .selectable_label(cur_val.eq_ignore_ascii_case(&p.name), &p.name)
                                                    .clicked()
                                                {
                                                    set_mapping = Some((t_key.clone(), p.name.clone()));
                                                }
                                            }
                                        }
                                        ui.separator();
                                        ui.label(RichText::new("Built-in presets").weak().small());
                                        for b in settings::PRESETS {
                                            if ui.selectable_label(cur_val.eq_ignore_ascii_case(b), b).clicked() {
                                                set_mapping = Some((t_key.clone(), b.to_string()));
                                            }
                                        }
                                    });
                                if !is_builtin && ui.small_button("×").on_hover_text("Remove custom type").clicked() {
                                    remove_mapping = Some(t_key.clone());
                                }
                            });
                        }

                        // Fallback default profile
                        let mut new_default_prof: Option<String> = None;
                        let def_cur = self.channels_file.cfg.default_profile.clone();
                        let def_lbl = if def_cur.is_empty() {
                            "(None)".to_string()
                        } else {
                            def_cur.clone()
                        };
                        ui.horizontal(|ui| {
                            ui.label(RichText::new("fallback ").monospace().small())
                                .on_hover_text("Applied to channels whose type has no mapping above");
                            egui::ComboBox::from_id_salt("tp_combo_fallback")
                                .selected_text(&def_lbl)
                                .width(176.0)
                                .show_ui(ui, |ui| {
                                    if ui.selectable_label(def_cur.is_empty(), "(None)").clicked() {
                                        new_default_prof = Some(String::new());
                                    }
                                    for name in &preset_names {
                                        if ui.selectable_label(def_cur.eq_ignore_ascii_case(name), name).clicked() {
                                            new_default_prof = Some(name.clone());
                                        }
                                    }
                                });
                        });

                        // Add custom channel type (e.g. "broadcast", "vhs", "weather")
                        ui.horizontal(|ui| {
                            let te = ui.add(
                                egui::TextEdit::singleline(&mut self.new_type_name)
                                    .hint_text("Add custom type (e.g. broadcast, vhs)…")
                                    .desired_width(214.0),
                            );
                            let add_t = ui.small_button("Add").clicked()
                                || (te.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)));
                            if add_t && !self.new_type_name.trim().is_empty() {
                                let k = std::mem::take(&mut self.new_type_name);
                                self.channels_file.set_type_profile(k, String::new());
                            }
                        });

                        if let Some((k, v)) = set_mapping {
                            self.channels_file.set_type_profile(k.clone(), v);
                            if let Some(w) = &self.active_watch {
                                if w.channel_type.eq_ignore_ascii_case(&k) {
                                    let ct = w.channel_type.clone();
                                    self.apply_channel_profile(&ct, None);
                                }
                            }
                        }
                        if let Some(k) = remove_mapping {
                            self.channels_file.remove_type_profile(&k);
                        }
                        if let Some(dp) = new_default_prof {
                            self.channels_file.cfg.default_profile = dp;
                            let _ = self.channels_file.save_to_disk();
                        }

                        ui.add_space(4.0);
                        ui.horizontal(|ui| {
                            if !self.channels_file.file_exists {
                                if ui
                                    .small_button("Create channels.json")
                                    .on_hover_text("Create a starter channels.json with WeatherStar 4000+, type->profile mappings, and override examples")
                                    .clicked()
                                {
                                    match self.channels_file.create_starter_file() {
                                        Ok(p) => {
                                            self.rebuild_lineup();
                                            self.channels_file_msg = format!("Created {}", p.display());
                                        }
                                        Err(e) => self.channels_file_msg = e,
                                    }
                                }
                            } else if ui.small_button("Reload").clicked() {
                                if self.channels_file.reload_from_disk() {
                                    self.rebuild_lineup();
                                    self.osd.show_action("CHANNELS.JSON RELOADED");
                                }
                            }
                            if let Some(path) = channels_file::config_path() {
                                if ui.small_button("Copy path").clicked() {
                                    ui.ctx().copy_text(path.display().to_string());
                                    self.channels_file_msg = "Copied channels.json path.".into();
                                }
                            }
                        });
                        if let Some(path) = channels_file::config_path() {
                            ui.label(RichText::new(format!("File: {}", path.display())).weak().small());
                        }
                        if !self.channels_file_msg.is_empty() {
                            ui.label(RichText::new(&self.channels_file_msg).small().color(Color32::from_rgb(235, 175, 100)));
                        }
                        if !self.channels_file.status.is_empty() {
                            ui.label(RichText::new(&self.channels_file.status).small().color(Color32::from_rgb(112, 236, 184)));
                        }
                        if !self.channels_file.last_error.is_empty() {
                            let mut clear_err = false;
                            ui.horizontal_wrapped(|ui| {
                                ui.label(
                                    RichText::new(&self.channels_file.last_error)
                                        .small()
                                        .color(Color32::from_rgb(248, 122, 108)),
                                );
                                if ui.small_button("×").on_hover_text("Dismiss error").clicked() {
                                    clear_err = true;
                                }
                            });
                            if clear_err {
                                self.channels_file.last_error.clear();
                            }
                        }
                    });

                // ---- Source 1: Local Nested Folders
                let dir_count = self.folders.channels.len();
                let dir_hdr = if dir_count > 0 {
                    format!("Local Folders ({dir_count} ch)")
                } else {
                    "Local Folders".to_string()
                };
                egui::CollapsingHeader::new(RichText::new(dir_hdr).strong().small())
                    .id_salt("src_local_folders")
                    .default_open(!self.jellyfin.connected)
                    .show(ui, |ui| {
                        ui.label(
                            RichText::new("Point at any media folder — nested TV shows (Show/Season 1/S01E01.mkv) and movie folders automatically become cable channels with local watched tracking.")
                                .weak()
                                .small(),
                        );
                        ui.add_space(3.0);
                        ui.horizontal(|ui| {
                            if ui.button("Add folder…").clicked() {
                                self.message = "pickfolder".into();
                            }
                            if !self.folders.cfg.roots.is_empty()
                                && ui
                                    .add_enabled(!self.folders.busy, egui::Button::new("Rescan"))
                                    .on_hover_text("Rescan all configured folders for new videos")
                                    .clicked()
                            {
                                self.folders.rescan_async();
                            }
                        });
                        ui.horizontal(|ui| {
                            let te = ui.add(
                                egui::TextEdit::singleline(&mut self.folder_path_input)
                                    .hint_text("/path/to/TV or Movies folder")
                                    .desired_width(235.0),
                            );
                            let add_clicked = ui.small_button("Add").clicked()
                                || (te.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)));
                            if add_clicked && !self.folder_path_input.trim().is_empty() {
                                let p = std::mem::take(&mut self.folder_path_input);
                                self.folders.add_root(p);
                            }
                        });
                        let mut remove_idx: Option<usize> = None;
                        let mut toggle_changed = false;
                        for (i, root) in self.folders.cfg.roots.iter_mut().enumerate() {
                            ui.horizontal(|ui| {
                                if ui.checkbox(&mut root.enabled, "").changed() {
                                    toggle_changed = true;
                                }
                                let display = if root.label.is_empty() { &root.path } else { &root.label };
                                ui.label(RichText::new(display).small()).on_hover_text(&root.path);
                                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                    if ui.small_button("×").on_hover_text("Remove folder").clicked() {
                                        remove_idx = Some(i);
                                    }
                                });
                            });
                        }
                        if toggle_changed {
                            let _ = crate::folders::save_config(&self.folders.cfg);
                            self.folders.rescan_async();
                        }
                        if let Some(idx) = remove_idx {
                            self.folders.remove_root(idx);
                        }
                        if !self.folders.status.is_empty() {
                            let col = if self.folders.busy {
                                Color32::from_rgb(235, 175, 100)
                            } else {
                                Color32::from_rgb(112, 236, 184)
                            };
                            ui.label(RichText::new(&self.folders.status).small().color(col));
                        }
                    });

                // ---- Source 2: Jellyfin Server
                let jf_hdr = if self.jellyfin.connected {
                    format!("Jellyfin Server ({} ch)", self.jellyfin.channels.len())
                } else {
                    "Jellyfin Server".to_string()
                };
                egui::CollapsingHeader::new(RichText::new(jf_hdr).strong().small())
                    .id_salt("src_jellyfin")
                    .default_open(self.jellyfin.connected || self.folders.cfg.roots.is_empty())
                    .show(ui, |ui| {
                        ui.horizontal(|ui| {
                            ui.label("Server URL");
                            ui.add(
                                egui::TextEdit::singleline(&mut self.jellyfin.cfg.server_url)
                                    .hint_text("http://192.168.1.10:8096")
                                    .desired_width(230.0),
                            );
                        });
                        ui.horizontal(|ui| {
                            ui.label("API Key / Token");
                            ui.add(
                                egui::TextEdit::singleline(&mut self.jellyfin.cfg.api_key)
                                    .password(!self.show_api_key)
                                    .hint_text("API Key or sign in below")
                                    .desired_width(166.0),
                            );
                            if ui.small_button(if self.show_api_key { "Hide" } else { "Show" }).clicked() {
                                self.show_api_key = !self.show_api_key;
                            }
                        });
                        egui::CollapsingHeader::new(RichText::new("Sign in with username & password (recommended for watched sync)").small())
                            .id_salt("jf_user_login")
                            .default_open(false)
                            .show(ui, |ui| {
                                ui.horizontal(|ui| {
                                    ui.label("Username");
                                    ui.add(
                                        egui::TextEdit::singleline(&mut self.jf_username)
                                            .hint_text("Jellyfin username")
                                            .desired_width(175.0),
                                    );
                                });
                                ui.horizontal(|ui| {
                                    ui.label("Password");
                                    let pw_te = ui.add(
                                        egui::TextEdit::singleline(&mut self.jf_password)
                                            .password(true)
                                            .hint_text("Password (not stored)")
                                            .desired_width(135.0),
                                    );
                                    let submit = ui
                                        .add_enabled(!self.jellyfin.busy, egui::Button::new("Sign in"))
                                        .clicked()
                                        || (pw_te.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)));
                                    if submit && !self.jellyfin.busy {
                                        let u = self.jf_username.clone();
                                        let p = std::mem::take(&mut self.jf_password);
                                        self.jellyfin.sign_in_async(u, p);
                                    }
                                });
                                ui.label(
                                    RichText::new("Gets a real Jellyfin user session token so watched status & resume progress always save cleanly. Your password is never saved.")
                                        .weak()
                                        .small(),
                                );
                            });
                        ui.horizontal(|ui| {
                            if ui.add_enabled(!self.jellyfin.busy, egui::Button::new("Connect / Fetch Users")).clicked() {
                                let _ = crate::jellyfin::save_config(&self.jellyfin.cfg);
                                self.jellyfin.fetch_users_async();
                            }
                            if self.jellyfin.connected
                                && ui
                                    .add_enabled(!self.jellyfin.busy, egui::Button::new("Refresh"))
                                    .on_hover_text("Reload channels, episodes, and watched states from Jellyfin")
                                    .clicked()
                            {
                                self.jellyfin.connect_and_load();
                            }
                            if self.jellyfin.connected
                                && ui
                                    .add_enabled(!self.jellyfin.busy, egui::Button::new("Rescan Server"))
                                    .on_hover_text("Ask Jellyfin to scan its media libraries for new files, then reload the guide")
                                    .clicked()
                            {
                                self.jellyfin.scan_server_library();
                            }
                            if self.jellyfin.connected && ui.button("Disconnect").clicked() {
                                self.jellyfin.disconnect();
                                self.rebuild_lineup();
                                if !self.has_channels() {
                                    self.guide.active = false;
                                    self.current_channel = None;
                                }
                            }
                        });

                        if !self.jellyfin.users.is_empty() {
                            let selected_name = if self.jellyfin.cfg.user_name.is_empty() {
                                "Select user…".to_string()
                            } else {
                                self.jellyfin.cfg.user_name.clone()
                            };
                            let mut chosen: Option<(String, String)> = None;
                            ui.horizontal(|ui| {
                                ui.label("User");
                                egui::ComboBox::from_id_salt("jellyfin_user")
                                    .selected_text(selected_name)
                                    .width(205.0)
                                    .show_ui(ui, |ui| {
                                        for u in &self.jellyfin.users {
                                            let sel = self.jellyfin.cfg.user_id == u.id;
                                            if ui.selectable_label(sel, &u.name).clicked() {
                                                chosen = Some((u.id.clone(), u.name.clone()));
                                            }
                                        }
                                    });
                            });
                            if let Some((uid, uname)) = chosen {
                                self.jellyfin.cfg.user_id = uid;
                                self.jellyfin.cfg.user_name = uname;
                                self.jellyfin.connect_and_load();
                            }
                        } else if !self.jellyfin.cfg.user_name.is_empty() {
                            ui.label(RichText::new(format!("User: {}", self.jellyfin.cfg.user_name)).small());
                        }

                        if self.jellyfin.connected {
                            let mut auto_changed = false;
                            ui.horizontal(|ui| {
                                ui.label("Auto-sync");
                                let cur_mins = self.jellyfin.cfg.auto_refresh_mins;
                                let lbl = if cur_mins == 0 {
                                    "Manual only".to_string()
                                } else {
                                    format!("Every {cur_mins} min")
                                };
                                egui::ComboBox::from_id_salt("jf_auto_sync")
                                    .selected_text(lbl)
                                    .width(126.0)
                                    .show_ui(ui, |ui| {
                                        for &(mins, text) in &[
                                            (0u32, "Manual only"),
                                            (5, "Every 5 min"),
                                            (10, "Every 10 min"),
                                            (15, "Every 15 min"),
                                            (30, "Every 30 min"),
                                            (60, "Every 60 min"),
                                        ] {
                                            if ui.selectable_label(self.jellyfin.cfg.auto_refresh_mins == mins, text).clicked() {
                                                self.jellyfin.cfg.auto_refresh_mins = mins;
                                                auto_changed = true;
                                            }
                                        }
                                    });
                            });
                            if auto_changed {
                                let _ = crate::jellyfin::save_config(&self.jellyfin.cfg);
                            }
                        }

                        if !self.jellyfin.status.is_empty() {
                            let col = if self.jellyfin.connected {
                                Color32::from_rgb(112, 236, 184)
                            } else {
                                Color32::from_rgb(235, 175, 100)
                            };
                            let status_line = if self.jellyfin.connected {
                                match self.jellyfin.last_sync_label() {
                                    Some(ago) => format!("{} (synced {ago})", self.jellyfin.status),
                                    None => self.jellyfin.status.clone(),
                                }
                            } else {
                                self.jellyfin.status.clone()
                            };
                            ui.label(RichText::new(status_line).small().color(col));
                        }
                        if !self.jellyfin.last_error.is_empty() {
                            let mut clear_err = false;
                            ui.horizontal_wrapped(|ui| {
                                ui.label(
                                    RichText::new(&self.jellyfin.last_error)
                                        .small()
                                        .color(Color32::from_rgb(248, 122, 108)),
                                );
                                if ui.small_button("×").on_hover_text("Dismiss error").clicked() {
                                    clear_err = true;
                                }
                            });
                            if clear_err {
                                self.jellyfin.last_error.clear();
                            }
                        }
                    });

                // ---- Source 3: Plex Media Server
                let px_hdr = if self.plex.connected {
                    format!("Plex Server ({} ch)", self.plex.channels.len())
                } else {
                    "Plex Server".to_string()
                };
                egui::CollapsingHeader::new(RichText::new(px_hdr).strong().small())
                    .id_salt("src_plex")
                    .default_open(self.plex.connected)
                    .show(ui, |ui| {
                        ui.horizontal(|ui| {
                            ui.label("Server URL");
                            ui.add(
                                egui::TextEdit::singleline(&mut self.plex.cfg.server_url)
                                    .hint_text("http://192.168.1.10:32400")
                                    .desired_width(230.0),
                            );
                        });
                        ui.horizontal(|ui| {
                            ui.label("X-Plex-Token");
                            ui.add(
                                egui::TextEdit::singleline(&mut self.plex.cfg.token)
                                    .password(!self.show_plex_token)
                                    .hint_text("Paste your Plex token")
                                    .desired_width(172.0),
                            )
                            .on_hover_text("Find your token in Plex Web: open any item -> Get Info -> View XML -> copy X-Plex-Token from the URL.");
                            if ui.small_button(if self.show_plex_token { "Hide" } else { "Show" }).clicked() {
                                self.show_plex_token = !self.show_plex_token;
                            }
                        });
                        ui.horizontal(|ui| {
                            let connect_lbl = if self.plex.connected { "Reconnect" } else { "Connect" };
                            if ui.add_enabled(!self.plex.busy, egui::Button::new(connect_lbl)).clicked() {
                                self.plex.connect_and_load();
                            }
                            if self.plex.connected
                                && ui
                                    .add_enabled(!self.plex.busy, egui::Button::new("Refresh"))
                                    .on_hover_text("Reload channels, episodes, and watched states from Plex")
                                    .clicked()
                            {
                                self.plex.connect_and_load();
                            }
                            if self.plex.connected
                                && ui
                                    .add_enabled(!self.plex.busy, egui::Button::new("Rescan Server"))
                                    .on_hover_text("Ask Plex to scan its libraries for new media, then reload the guide")
                                    .clicked()
                            {
                                self.plex.scan_server_library();
                            }
                            if self.plex.connected && ui.button("Disconnect").clicked() {
                                self.plex.disconnect();
                                self.rebuild_lineup();
                                if !self.has_channels() {
                                    self.guide.active = false;
                                    self.current_channel = None;
                                }
                            }
                        });
                        if self.plex.connected {
                            let mut auto_changed = false;
                            ui.horizontal(|ui| {
                                ui.label("Auto-sync");
                                let cur_mins = self.plex.cfg.auto_refresh_mins;
                                let lbl = if cur_mins == 0 {
                                    "Manual only".to_string()
                                } else {
                                    format!("Every {cur_mins} min")
                                };
                                egui::ComboBox::from_id_salt("plex_auto_sync")
                                    .selected_text(lbl)
                                    .width(126.0)
                                    .show_ui(ui, |ui| {
                                        for &(mins, text) in &[
                                            (0u32, "Manual only"),
                                            (5, "Every 5 min"),
                                            (10, "Every 10 min"),
                                            (15, "Every 15 min"),
                                            (30, "Every 30 min"),
                                            (60, "Every 60 min"),
                                        ] {
                                            if ui.selectable_label(self.plex.cfg.auto_refresh_mins == mins, text).clicked() {
                                                self.plex.cfg.auto_refresh_mins = mins;
                                                auto_changed = true;
                                            }
                                        }
                                    });
                            });
                            if auto_changed {
                                let _ = crate::plex::save_config(&self.plex.cfg);
                            }
                        }
                        if !self.plex.status.is_empty() {
                            let col = if self.plex.connected {
                                Color32::from_rgb(112, 236, 184)
                            } else {
                                Color32::from_rgb(235, 175, 100)
                            };
                            let status_line = if self.plex.connected {
                                match self.plex.last_sync_label() {
                                    Some(ago) => format!("{} (synced {ago})", self.plex.status),
                                    None => self.plex.status.clone(),
                                }
                            } else {
                                self.plex.status.clone()
                            };
                            ui.label(RichText::new(status_line).small().color(col));
                        }
                        if !self.plex.last_error.is_empty() {
                            let mut clear_err = false;
                            ui.horizontal_wrapped(|ui| {
                                ui.label(
                                    RichText::new(&self.plex.last_error)
                                        .small()
                                        .color(Color32::from_rgb(248, 122, 108)),
                                );
                                if ui.small_button("×").on_hover_text("Dismiss error").clicked() {
                                    clear_err = true;
                                }
                            });
                            if clear_err {
                                self.plex.last_error.clear();
                            }
                        }
                    });

                // ---- Source 4: M3U / M3U8 Live Streams & Arbitrary Online Channels
                let m3u_count = self.m3u.channels.len();
                let m3u_hdr = if m3u_count > 0 {
                    format!("Live Streams / M3U8 ({m3u_count} ch)")
                } else {
                    "Live Streams / M3U8".to_string()
                };
                egui::CollapsingHeader::new(RichText::new(m3u_hdr).strong().small())
                    .id_salt("src_m3u")
                    .default_open(m3u_count > 0)
                    .show(ui, |ui| {
                        ui.label(
                            RichText::new("Add arbitrary online .m3u8 live streams (auto-probing HLS metadata), live web channels (like WeatherStar 4000+), or .m3u/.m3u8 playlists onto CH 80+.")
                                .weak()
                                .small(),
                        );
                        ui.add_space(3.0);
                        ui.horizontal(|ui| {
                            let has_ws4k = self
                                .m3u
                                .cfg
                                .channels
                                .iter()
                                .any(|c| c.url.contains("4000.weatherstar.dev"));
                            if !has_ws4k
                                && ui
                                    .small_button("+ WeatherStar 4000+")
                                    .on_hover_text("Add the classic 1990s Weather Channel emulator (4000.weatherstar.dev) as a live CRT web channel")
                                    .clicked()
                            {
                                self.m3u.add_channel(
                                    "WeatherStar 4000+".into(),
                                    web_channel::WEATHERSTAR_PRESET_URL.into(),
                                    "Weather".into(),
                                    String::new(),
                                );
                            }
                        });
                        ui.label(RichText::new("Add a single live / web channel").small().strong());
                        ui.horizontal(|ui| {
                            ui.label("Name");
                            ui.add(
                                egui::TextEdit::singleline(&mut self.m3u_ch_name)
                                    .hint_text("e.g. WeatherStar 4000+")
                                    .desired_width(130.0),
                            );
                            ui.label("Group");
                            ui.add(
                                egui::TextEdit::singleline(&mut self.m3u_ch_group)
                                    .hint_text("Live")
                                    .desired_width(82.0),
                            );
                        });
                        ui.horizontal(|ui| {
                            ui.label("URL");
                            ui.add(
                                egui::TextEdit::singleline(&mut self.m3u_ch_url)
                                    .hint_text("https://.../stream.m3u8 or 4000.weatherstar.dev")
                                    .desired_width(256.0),
                            );
                        });
                        ui.horizontal(|ui| {
                            ui.checkbox(&mut self.m3u_ch_is_web, "Web page")
                                .on_hover_text("Render a live web page (e.g. 4000.weatherstar.dev) via headless Chromium at 640×480 straight into the CRT");
                            ui.add(
                                egui::TextEdit::singleline(&mut self.m3u_ch_logo)
                                    .hint_text("Optional logo URL")
                                    .desired_width(134.0),
                            );
                            if ui
                                .add_enabled(!self.m3u_ch_url.trim().is_empty(), egui::Button::new("Add Ch").small())
                                .clicked()
                            {
                                let n = std::mem::take(&mut self.m3u_ch_name);
                                let raw_u = std::mem::take(&mut self.m3u_ch_url);
                                let u = if self.m3u_ch_is_web {
                                    web_channel::ensure_web_prefix(&raw_u)
                                } else {
                                    raw_u
                                };
                                let g = std::mem::take(&mut self.m3u_ch_group);
                                let l = std::mem::take(&mut self.m3u_ch_logo);
                                self.m3u.add_channel(n, u, g, l);
                            }
                        });
                        let mut ch_toggle = false;
                        let mut ch_remove: Option<usize> = None;
                        for (i, ch) in self.m3u.cfg.channels.iter_mut().enumerate() {
                            ui.horizontal(|ui| {
                                if ui.checkbox(&mut ch.enabled, "").changed() {
                                    ch_toggle = true;
                                }
                                let lbl = if ch.name.is_empty() { &ch.url } else { &ch.name };
                                ui.label(RichText::new(lbl).small()).on_hover_text(&ch.url);
                                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                    if ui.small_button("×").on_hover_text("Remove channel").clicked() {
                                        ch_remove = Some(i);
                                    }
                                });
                            });
                        }
                        if ch_toggle {
                            let _ = crate::m3u::save_config(&self.m3u.cfg);
                            self.m3u.refresh_async();
                        }
                        if let Some(i) = ch_remove {
                            self.m3u.remove_channel(i);
                        }

                        ui.add_space(4.0);
                        ui.label(RichText::new("M3U / M3U8 Playlists (URL or file)").small().strong());
                        ui.horizontal(|ui| {
                            let te = ui.add(
                                egui::TextEdit::singleline(&mut self.m3u_pl_url)
                                    .hint_text("https://.../channels.m3u8 or local path")
                                    .desired_width(210.0),
                            );
                            let add = ui.small_button("Add").clicked()
                                || (te.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)));
                            if add && !self.m3u_pl_url.trim().is_empty() {
                                let u = std::mem::take(&mut self.m3u_pl_url);
                                self.m3u.add_playlist(u, String::new());
                            }
                            if ui.small_button("File…").on_hover_text("Browse for a local .m3u / .m3u8 file").clicked() {
                                self.message = "pickm3u".into();
                            }
                        });
                        let mut pl_toggle = false;
                        let mut pl_remove: Option<usize> = None;
                        for (i, pl) in self.m3u.cfg.playlists.iter_mut().enumerate() {
                            ui.horizontal(|ui| {
                                if ui.checkbox(&mut pl.enabled, "").changed() {
                                    pl_toggle = true;
                                }
                                let lbl = if pl.label.is_empty() { &pl.url_or_path } else { &pl.label };
                                ui.label(RichText::new(lbl).small()).on_hover_text(&pl.url_or_path);
                                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                    if ui.small_button("×").on_hover_text("Remove playlist").clicked() {
                                        pl_remove = Some(i);
                                    }
                                });
                            });
                        }
                        if pl_toggle {
                            let _ = crate::m3u::save_config(&self.m3u.cfg);
                            self.m3u.refresh_async();
                        }
                        if let Some(i) = pl_remove {
                            self.m3u.remove_playlist(i);
                        }

                        if self.m3u.cfg.has_enabled_sources() {
                            ui.horizontal(|ui| {
                                if ui
                                    .add_enabled(!self.m3u.busy, egui::Button::new("Refresh Streams").small())
                                    .clicked()
                                {
                                    self.m3u.refresh_async();
                                }
                            });
                        }
                        if !self.m3u.status.is_empty() {
                            let col = if self.m3u.busy {
                                Color32::from_rgb(235, 175, 100)
                            } else {
                                Color32::from_rgb(112, 236, 184)
                            };
                            ui.label(RichText::new(&self.m3u.status).small().color(col));
                        }
                        if !self.m3u.last_error.is_empty() {
                            let mut clear_err = false;
                            ui.horizontal_wrapped(|ui| {
                                ui.label(
                                    RichText::new(&self.m3u.last_error)
                                        .small()
                                        .color(Color32::from_rgb(248, 122, 108)),
                                );
                                if ui.small_button("×").on_hover_text("Dismiss error").clicked() {
                                    clear_err = true;
                                }
                            });
                            if clear_err {
                                self.m3u.last_error.clear();
                            }
                        }
                    });

                // ---- Source 5: Custom Integrators (Drop-in Scripts & HTTP JSON Feeds)
                let cus_count = self.custom.channels.len();
                let cus_hdr = if cus_count > 0 {
                    format!("Custom Integrators ({cus_count} ch)")
                } else {
                    "Custom Integrators".to_string()
                };
                egui::CollapsingHeader::new(RichText::new(cus_hdr).strong().small())
                    .id_salt("src_custom")
                    .default_open(cus_count > 0)
                    .show(ui, |ui| {
                        ui.label(
                            RichText::new("Plug in custom scripts/binaries (stdout JSON) or HTTP JSON feed endpoints using the ViCTR provider JSON schema.")
                                .weak()
                                .small(),
                        );
                        ui.add_space(3.0);
                        ui.horizontal(|ui| {
                            if ui
                                .add_enabled(!self.custom.busy, egui::Button::new("Rescan & Run").small())
                                .on_hover_text("Discover scripts in the providers folder and refresh all custom endpoints")
                                .clicked()
                            {
                                self.custom.refresh_async();
                            }
                            if ui
                                .small_button("Create Example Plugin")
                                .on_hover_text("Writes a documented README.md + example-channel.sh.disabled into the providers folder")
                                .clicked()
                            {
                                self.custom_msg = match crate::custom_provider::create_example_plugin() {
                                    Ok(dir) => format!("Created template in {}", dir.display()),
                                    Err(e) => e,
                                };
                            }
                        });
                        if let Some(dir) = crate::custom_provider::providers_dir() {
                            ui.label(RichText::new(format!("Plugins dir: {}", dir.display())).weak().small());
                        }
                        if !self.custom.scripts.is_empty() {
                            ui.label(RichText::new("Discovered plugin scripts").small().strong());
                            let scripts: Vec<String> = self.custom.scripts.iter().map(|s| s.file_name.clone()).collect();
                            let mut script_toggle: Option<(String, bool)> = None;
                            for fname in scripts {
                                let mut en = !self.custom.cfg.disabled_scripts.contains(&fname);
                                ui.horizontal(|ui| {
                                    if ui.checkbox(&mut en, "").changed() {
                                        script_toggle = Some((fname.clone(), en));
                                    }
                                    ui.label(RichText::new(&fname).monospace().small());
                                });
                            }
                            if let Some((fname, en)) = script_toggle {
                                self.custom.set_script_enabled(&fname, en);
                            }
                        }

                        ui.add_space(4.0);
                        ui.label(RichText::new("HTTP JSON feed endpoints").small().strong());
                        ui.horizontal(|ui| {
                            ui.label("Name");
                            ui.add(
                                egui::TextEdit::singleline(&mut self.custom_ep_name)
                                    .hint_text("My Feed")
                                    .desired_width(110.0),
                            );
                            ui.label("URL");
                            ui.add(
                                egui::TextEdit::singleline(&mut self.custom_ep_url)
                                    .hint_text("https://.../channels.json")
                                    .desired_width(142.0),
                            );
                        });
                        ui.horizontal(|ui| {
                            ui.label("Auth");
                            ui.add(
                                egui::TextEdit::singleline(&mut self.custom_ep_auth)
                                    .hint_text("Optional Authorization header")
                                    .desired_width(210.0),
                            );
                            if ui
                                .add_enabled(!self.custom_ep_url.trim().is_empty(), egui::Button::new("Add Feed").small())
                                .clicked()
                            {
                                let n = std::mem::take(&mut self.custom_ep_name);
                                let u = std::mem::take(&mut self.custom_ep_url);
                                let a = std::mem::take(&mut self.custom_ep_auth);
                                self.custom.add_endpoint(n, u, a);
                            }
                        });
                        let mut ep_toggle = false;
                        let mut ep_remove: Option<usize> = None;
                        for (i, ep) in self.custom.cfg.endpoints.iter_mut().enumerate() {
                            ui.horizontal(|ui| {
                                if ui.checkbox(&mut ep.enabled, "").changed() {
                                    ep_toggle = true;
                                }
                                ui.label(RichText::new(&ep.name).small()).on_hover_text(&ep.url);
                                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                    if ui.small_button("×").on_hover_text("Remove endpoint").clicked() {
                                        ep_remove = Some(i);
                                    }
                                });
                            });
                        }
                        if ep_toggle {
                            let _ = crate::custom_provider::save_config(&self.custom.cfg);
                            self.custom.refresh_async();
                        }
                        if let Some(i) = ep_remove {
                            self.custom.remove_endpoint(i);
                        }

                        if !self.custom_msg.is_empty() {
                            ui.label(RichText::new(&self.custom_msg).small().color(Color32::from_rgb(235, 175, 100)));
                        }
                        if !self.custom.status.is_empty() {
                            let col = if self.custom.busy {
                                Color32::from_rgb(235, 175, 100)
                            } else {
                                Color32::from_rgb(112, 236, 184)
                            };
                            ui.label(RichText::new(&self.custom.status).small().color(col));
                        }
                        if !self.custom.last_error.is_empty() {
                            let mut clear_err = false;
                            ui.horizontal_wrapped(|ui| {
                                ui.label(
                                    RichText::new(&self.custom.last_error)
                                        .small()
                                        .color(Color32::from_rgb(248, 122, 108)),
                                );
                                if ui.small_button("×").on_hover_text("Dismiss error").clicked() {
                                    clear_err = true;
                                }
                            });
                            if clear_err {
                                self.custom.last_error.clear();
                            }
                        }
                    });
    }

    fn games_capture_controls(&mut self, ui: &mut egui::Ui) {
        // ---- Native libretro Game Console Input (CH 03)
        let game_hdr = format!("Game Console (CH {:02} / libretro)", self.retro.cfg.channel_number.max(2));
        egui::CollapsingHeader::new(RichText::new(game_hdr).strong())
            .id_salt("tab_retro_console")
            .default_open(true)
            .show(ui, |ui| {
                ui.label(
                    RichText::new("Play NES, SNES, Genesis, GB/GBA, PC Engine & Arcade ROMs directly on CH 03 using your installed libretro cores — zero-lag 240p + analog CRT sound.")
                        .weak()
                        .small(),
                );
                ui.add_space(4.0);
                let mut retro_cfg_changed = false;
                ui.horizontal(|ui| {
                    if ui.checkbox(&mut self.retro.cfg.enabled, "Enable CH").changed() {
                        retro_cfg_changed = true;
                    }
                    let mut ch_n = self.retro.cfg.channel_number.max(2) as i32;
                    if ui.add(egui::DragValue::new(&mut ch_n).range(2..=99)).changed() {
                        self.retro.cfg.channel_number = ch_n as u16;
                        retro_cfg_changed = true;
                    }
                    if ui.small_button("Tune Now").clicked() {
                        let n = self.retro.cfg.channel_number.max(2);
                        self.tune_channel(n);
                    }
                });
                if retro_cfg_changed {
                    let _ = retro::save_config(&self.retro.cfg);
                    self.retro.rescan();
                    self.rebuild_lineup();
                }
                ui.horizontal(|ui| {
                    if ui.button("Load ROM…").clicked() {
                        self.message = "pickrom".into();
                    }
                    if ui.button("Add ROM Folder…").clicked() {
                        self.message = "pickromdir".into();
                    }
                    if ui.small_button("Rescan").clicked() {
                        self.retro.rescan();
                        self.rebuild_lineup();
                    }
                });
                if !self.retro.cores.is_empty() {
                    let cur_core_lbl = if self.retro_selected_core.is_empty() {
                        "Auto-detect by ROM extension".to_string()
                    } else {
                        self.retro_selected_core.clone()
                    };
                    ui.horizontal(|ui| {
                        ui.label("Core");
                        egui::ComboBox::from_id_salt("retro_core_sel")
                            .selected_text(cur_core_lbl)
                            .width(240.0)
                            .show_ui(ui, |ui| {
                                if ui
                                    .selectable_label(self.retro_selected_core.is_empty(), "Auto-detect by ROM extension")
                                    .clicked()
                                {
                                    self.retro_selected_core.clear();
                                }
                                for c in &self.retro.cores {
                                    if ui
                                        .selectable_label(self.retro_selected_core == c.name, &c.name)
                                        .clicked()
                                    {
                                        self.retro_selected_core = c.name.clone();
                                    }
                                }
                            });
                    });
                }
                self.retro_gamepad_selector(ui, "retro_gp_tab");
                if let Some(rp) = &self.retro_player {
                    let (gname, cname, status, active_gp, paused, running) = {
                        let sh = rp.shared.lock().unwrap();
                        (
                            sh.game_name.clone(),
                            sh.core_name.clone(),
                            sh.status.clone(),
                            sh.active_gamepad.clone(),
                            sh.paused,
                            sh.running,
                        )
                    };
                    ui.separator();
                    let title_line = if cname.is_empty() {
                        format!("Cartridge: {gname}")
                    } else {
                        format!("Cartridge: {gname} ({cname})")
                    };
                    ui.label(RichText::new(title_line).strong().color(Color32::from_rgb(138, 240, 178)));
                    ui.label(RichText::new(status).weak().small());
                    if running && !active_gp.is_empty() {
                        ui.label(RichText::new(format!("Active pad: {active_gp}")).small().color(Color32::from_rgb(156, 224, 186)));
                    }
                    let mut eject = false;
                    if running {
                        ui.horizontal(|ui| {
                            if ui.small_button(if paused { "Resume" } else { "Pause" }).clicked() {
                                rp.toggle_pause();
                            }
                            if ui.small_button("Reset (F1)").clicked() {
                                rp.reset();
                                self.osd.show_action("CONSOLE RESET");
                            }
                            if ui.small_button("Save (F2)").clicked() {
                                rp.save_state();
                                self.osd.show_action("STATE SAVED");
                            }
                            if ui.small_button("Load (F4)").clicked() {
                                rp.load_state();
                                self.osd.show_action("STATE LOADED");
                            }
                            if ui.small_button("Eject").clicked() {
                                eject = true;
                            }
                        });
                    }
                    if eject {
                        self.retro_player = None;
                    }
                }
                if !self.retro.cfg.rom_dirs.is_empty() {
                    ui.add_space(4.0);
                    ui.label(RichText::new("ROM Folders").strong().small());
                    let mut rm_dir: Option<usize> = None;
                    for (i, d) in self.retro.cfg.rom_dirs.iter().enumerate() {
                        ui.horizontal(|ui| {
                            let lbl = std::path::Path::new(d)
                                .file_name()
                                .and_then(|n| n.to_str())
                                .unwrap_or(d);
                            ui.label(RichText::new(lbl).small()).on_hover_text(d);
                            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                if ui.small_button("×").on_hover_text("Remove ROM folder").clicked() {
                                    rm_dir = Some(i);
                                }
                            });
                        });
                    }
                    if let Some(idx) = rm_dir {
                        self.retro.remove_rom_dir(idx);
                        self.rebuild_lineup();
                    }
                }
                if !self.retro.status.is_empty() {
                    ui.label(RichText::new(&self.retro.status).small().color(Color32::from_rgb(112, 236, 184)));
                }
                ui.label(
                    RichText::new("Keyboard: Arrows=D-Pad · Z/X=B/A · A/S=Y/X · Q/W=L/R · Enter=Start · Backspace=Select · F1=Reset · F2/F4=Save/Load.")
                        .weak()
                        .small(),
                );
            });

        ui.add_space(6.0);

        // ---- External Emulator Window / Monitor / USB Capture Card (AV 1)
        let cap_hdr = if self.capture_player.is_some() {
            "External Input / Capture (LIVE)".to_string()
        } else {
            "External Input / Capture (Dolphin, PCSX2, RetroArch…)".to_string()
        };
        egui::CollapsingHeader::new(RichText::new(cap_hdr).strong())
            .id_salt("tab_capture_input")
            .default_open(true)
            .show(ui, |ui| {
                ui.label(
                    RichText::new("Mirror any running emulator window (Dolphin, PCSX2, RetroArch, etc.), Wayland monitor, or USB HDMI/Composite capture card (/dev/video0) directly into the CRT.")
                        .weak()
                        .small(),
                );
                ui.add_space(4.0);
                ui.horizontal(|ui| {
                    if ui.button("Scan Windows & Devices").clicked() {
                        self.capture_windows = capture::list_open_windows();
                        self.capture_monitors = capture::list_monitors();
                        self.capture_v4l2 = capture::list_v4l2_devices();
                    }
                    if self.capture_player.is_some() && ui.button("Stop Capture").clicked() {
                        self.capture_player = None;
                    }
                });
                let mut cap_start: Option<(String, String)> = None;
                ui.horizontal_wrapped(|ui| {
                    for (lbl, q) in [
                        ("Dolphin", "capture://window/dolphin"),
                        ("PCSX2", "capture://window/pcsx2"),
                        ("RetroArch", "capture://window/retroarch"),
                    ] {
                        if ui
                            .button(lbl)
                            .on_hover_text(format!("Mirror any open {lbl} window onto the CRT (auto-waits until launched)"))
                            .clicked()
                        {
                            cap_start = Some((lbl.to_string(), q.to_string()));
                        }
                    }
                });
                ui.horizontal(|ui| {
                    let te = ui.add(
                        egui::TextEdit::singleline(&mut self.capture_custom_query)
                            .hint_text("Window title/class or capture:// URL")
                            .desired_width(280.0),
                    );
                    let go = ui.button("Mirror").clicked()
                        || (te.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)));
                    if go && !self.capture_custom_query.trim().is_empty() {
                        let raw = self.capture_custom_query.trim().to_string();
                        let url = if capture::is_capture_url(&raw) {
                            raw.clone()
                        } else {
                            format!("capture://window/{raw}")
                        };
                        cap_start = Some((raw, url));
                    }
                });
                if !self.capture_windows.is_empty() {
                    ui.add_space(3.0);
                    ui.label(RichText::new("Open windows (click to mirror)").small().strong());
                    let wins = self.capture_windows.clone();
                    for w in wins {
                        let lbl = w.label();
                        let url = format!("capture://window/{}", w.match_key());
                        if ui.selectable_label(false, &lbl).clicked() {
                            cap_start = Some((lbl, url));
                        }
                    }
                }
                if !self.capture_monitors.is_empty() {
                    ui.horizontal_wrapped(|ui| {
                        ui.label(RichText::new("Displays:").weak().small());
                        for m in self.capture_monitors.clone() {
                            if ui.small_button(&m).clicked() {
                                cap_start = Some((m.clone(), format!("capture://output/{m}")));
                            }
                        }
                    });
                }
                if !self.capture_v4l2.is_empty() {
                    ui.horizontal_wrapped(|ui| {
                        ui.label(RichText::new("Capture cards:").weak().small());
                        for d in self.capture_v4l2.clone() {
                            if ui.small_button(&d).clicked() {
                                cap_start = Some((d.clone(), format!("capture://v4l2{d}")));
                            }
                        }
                    });
                }
                if let Some((lbl, url)) = cap_start {
                    self.guide.active = false;
                    self.sim.switch_channel();
                    self.apply_channel_profile("game", None);
                    self.start_video_at(url, 0.0);
                    self.video_name = format!("AV 1 · {lbl}");
                    self.osd.show_channel(0, "AV1", &lbl, self.s.mono);
                }
                if let Some(cp) = &self.capture_player {
                    let st = cp.shared.lock().unwrap().status.clone();
                    ui.label(RichText::new(st).small().color(Color32::from_rgb(112, 236, 184)));
                }
                ui.label(
                    RichText::new("Tip: Pin Dolphin/PCSX2 to a channel in channels.json with kind:\"capture\" and stream_url:\"capture://window/dolphin\".")
                        .weak()
                        .small(),
                );
            });
    }

    fn start_video_at(&mut self, src: String, start: f64) {
        // Stop any previous player FIRST so teardown cannot race with the new audio/video session.
        self.stop_vcr_shuttle_silent();
        self.player = None;
        self.web_player = None;
        self.retro_player = None;
        self.capture_player = None;
        let _ = self.splash.skip();
        self.startup_dest_pending = false;
        if self.current_channel.is_none() && self.screenshot.is_none() {
            self.app_cfg.last_watched = Some(LastWatched::LocalVideo { src: src.clone(), pos_secs: start });
            let _ = settings::save_app_config(&self.app_cfg);
        }
        if capture::is_capture_url(&src) {
            self.capture_player = Some(CapturePlayer::start(&src));
            self.video_name = src.clone();
            self.video_frame_no = 0;
            self.s.pattern = Pattern::Video;
            self.reset_trails = true;
            return;
        }
        if retro::is_retro_url(&src) {
            let (rom_path, core_hint) = retro::parse_retro_url(&src);
            let ch_num = self.retro.cfg.channel_number.max(2);
            if rom_path.is_empty() || rom_path == "standby" || !std::path::Path::new(&rom_path).is_file() {
                let msg = if rom_path == "standby" || rom_path.is_empty() {
                    String::new()
                } else {
                    format!("ROM not found: {rom_path}")
                };
                self.retro_player = Some(RetroPlayer::start_standby(ch_num, &msg));
            } else {
                let hint = core_hint.as_deref().or_else(|| {
                    if self.retro_selected_core.is_empty() {
                        None
                    } else {
                        Some(self.retro_selected_core.as_str())
                    }
                });
                if let Some(core) = retro::select_core_for_rom(
                    std::path::Path::new(&rom_path),
                    &self.retro.cores,
                    hint,
                    &self.retro.cfg.preferred_cores,
                )
                .cloned()
                {
                    let ring = self.audio.as_ref().map(|a| a.ring.clone());
                    let gamepad_dev = self.retro.cfg.gamepad_device.clone();
                    self.retro_player = Some(RetroPlayer::start(
                        core.path,
                        std::path::PathBuf::from(&rom_path),
                        ring,
                        gamepad_dev,
                    ));
                    self.retro.set_last_rom(&rom_path);
                    self.rebuild_lineup();
                } else {
                    let msg = format!("No libretro core found for {rom_path}");
                    self.retro_player = Some(RetroPlayer::start_standby(ch_num, &msg));
                }
            }
            self.video_name = format!("CH {:02} GAME", ch_num);
            self.video_frame_no = 0;
            self.s.pattern = Pattern::Video;
            self.reset_trails = true;
            return;
        }
        if web_channel::is_web_channel_url(&src) {
            self.web_player = Some(WebPlayer::start(&src, self.muted || !self.s.sound));
            self.video_name = web_channel::normalize_web_target_url(&src);
            self.video_frame_no = 0;
            self.s.pattern = Pattern::Video;
            self.reset_trails = true;
            return;
        }
        let ring = self.audio.as_ref().map(|a| a.ring.clone());
        let opts = StartOpts {
            volume: (self.s.volume * 100.0) as f64,
            crop: self.crop,
            start,
            user_config: self.use_my_mpv,
            audio_delay: self.s.audio_sync_ms as f64 / 1000.0,
        };
        self.player = Some(Player::start(&src, opts, ring));
        self.video_name = if src.contains("://") {
            src.clone()
        } else {
            std::path::Path::new(&src).file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_else(|| src.clone())
        };
        self.video_frame_no = 0;
        self.s.pattern = Pattern::Video;
        self.reset_trails = true;
    }

    fn stop_vcr_shuttle_silent(&mut self) {
        if self.vcr_shuttle_speed != 0.0 {
            self.vcr_shuttle_speed = 0.0;
            self.sim.set_vcr_shuttle(0.0);
            if let Some(p) = &self.player {
                p.set_shuttle(0.0);
            }
        }
    }

    fn vcr_shuttle_label(speed: f32) -> &'static str {
        if speed >= 24.0 {
            "FF >>>> 32X"
        } else if speed >= 12.0 {
            "FF >>> 16X"
        } else if speed >= 6.0 {
            "FF >>> 8X"
        } else if speed > 0.1 {
            "FF >> 4X"
        } else if speed <= -24.0 {
            "REW <<<< 32X"
        } else if speed <= -12.0 {
            "REW <<< 16X"
        } else if speed <= -6.0 {
            "REW <<< 8X"
        } else if speed < -0.1 {
            "REW << 4X"
        } else {
            "PLAY >"
        }
    }

    fn show_video_progress_osd(&mut self, label: &str, override_pos: Option<f64>) {
        if let Some(p) = &self.player {
            let (pos, dur) = {
                let sh = p.shared.lock().unwrap();
                (sh.pos, sh.duration)
            };
            let p_eff = override_pos.unwrap_or(pos).clamp(0.0, if dur > 0.0 { dur } else { f64::MAX });
            self.osd.show_progress(label, p_eff, dur);
        }
    }

    fn set_vcr_shuttle(&mut self, speed: f32) {
        let was_shuttling = self.vcr_shuttle_speed != 0.0;
        self.vcr_shuttle_speed = speed;
        self.sim.set_vcr_shuttle(speed);
        let lbl = Self::vcr_shuttle_label(speed);
        if let Some(p) = &self.player {
            p.set_shuttle(speed as f64);
            if speed > 0.1 {
                if speed > 8.5 {
                    self.vcr_rw_last_step = std::time::Instant::now() - std::time::Duration::from_millis(250);
                }
                self.osd.show_action(lbl);
                self.show_video_progress_osd(lbl, None);
            } else if speed < -0.1 {
                self.vcr_rw_last_step = std::time::Instant::now() - std::time::Duration::from_millis(250);
                self.osd.show_action(lbl);
                self.show_video_progress_osd(lbl, None);
            } else if was_shuttling {
                self.osd.show_action("PLAY >");
                self.show_video_progress_osd("PLAY >", None);
            }
        }
    }

    fn cycle_vcr_shuttle(&mut self, dir: f32) {
        if self.player.is_none() {
            return;
        }
        let s = self.vcr_shuttle_speed;
        let next = if dir > 0.0 {
            if s > 0.1 && s < 6.0 {
                8.0
            } else if s >= 6.0 && s < 12.0 {
                16.0
            } else if s >= 12.0 && s < 24.0 {
                32.0
            } else {
                4.0
            }
        } else if s < -0.1 && s > -6.0 {
            -8.0
        } else if s <= -6.0 && s > -12.0 {
            -16.0
        } else if s <= -12.0 && s > -24.0 {
            -32.0
        } else {
            -4.0
        };
        self.set_vcr_shuttle(next);
    }

    fn seek_video_rel(&mut self, delta: f64) {
        self.stop_vcr_shuttle_silent();
        if let Some(p) = &self.player {
            let cur_pos = p.shared.lock().unwrap().pos;
            p.seek(delta);
            self.sim.trigger_seek_shuttle(delta as f32);
            let secs = delta.abs().round() as i64;
            let lbl = if delta >= 0.0 {
                format!("FF >> +{secs}S")
            } else {
                format!("REW << -{secs}S")
            };
            self.osd.show_action(&lbl);
            self.show_video_progress_osd(&lbl, Some((cur_pos + delta).max(0.0)));
        }
    }

    fn seek_video_abs(&mut self, target: f64, prev_pos: f64) {
        self.stop_vcr_shuttle_silent();
        if let Some(p) = &self.player {
            let delta = target - prev_pos;
            p.seek_abs(target);
            self.sim.trigger_seek_shuttle(if delta.abs() < 0.25 { 5.0 } else { delta as f32 });
            let lbl = if delta < 0.0 { "REW <<" } else { "FF >>" };
            self.osd.show_action(lbl);
            self.show_video_progress_osd(lbl, Some(target));
        }
    }

    fn pick_video(&mut self) {
        if let Some(path) = rfd::FileDialog::new()
            .add_filter("Videos", &["mp4", "mkv", "webm", "avi", "mov", "m4v", "mpg", "mpeg", "ts", "flv", "wmv", "ogv", "3gp", "vob"])
            .add_filter("All files", &["*"])
            .pick_file()
        {
            self.start_video(path.to_string_lossy().to_string());
        }
    }

    fn video_controls(&mut self, ui: &mut egui::Ui) {
        egui::CollapsingHeader::new(RichText::new("Video through mpv").strong())
            .default_open(true)
            .show(ui, |ui| {
                let mut stop = false;
                ui.horizontal(|ui| {
                    if ui.button("Open a video…").clicked() {
                        self.message = "pickvideo".into();
                    }
                    if (self.player.is_some()
                        || self.web_player.is_some()
                        || self.retro_player.is_some()
                        || self.capture_player.is_some())
                        && ui.button("Stop").clicked()
                    {
                        stop = true;
                    }
                });
                if stop {
                    self.stop_vcr_shuttle_silent();
                    self.player.take();
                    self.web_player.take();
                    self.retro_player.take();
                    self.capture_player.take();
                }
                ui.horizontal(|ui| {
                    let te = ui.add(
                        egui::TextEdit::singleline(&mut self.video_url).hint_text("file path or URL (mpv + yt-dlp)").desired_width(260.0),
                    );
                    let go = ui.button("Play").clicked() || (te.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)));
                    if go && !self.video_url.trim().is_empty() {
                        let u = self.video_url.trim().to_string();
                        self.start_video(u);
                    }
                });
                let mut toggle = false;
                let mut seek: Option<f64> = None;
                let mut seek_abs: Option<(f64, f64)> = None;
                let mut shuttle_cmd: Option<f32> = None; // 0 = play, -1 = cycle RW, +1 = cycle FF
                if let Some(p) = &self.player {
                    let sh = p.shared.lock().unwrap();
                    let (pos, dur, paused, running) = (sh.pos, sh.duration, sh.paused, sh.running);
                    let status = format!("{} — {}", self.video_name, sh.status);
                    let backend = sh.backend;
                    drop(sh);
                    ui.label(RichText::new(status).small());
                    if !backend.is_empty() {
                        ui.label(RichText::new(format!("via {backend}")).weak().small());
                    }
                    if running {
                        let rw_on = self.vcr_shuttle_speed < -0.1;
                        let ff_on = self.vcr_shuttle_speed > 0.1;
                        ui.horizontal_wrapped(|ui| {
                            let rw_lbl = if self.vcr_shuttle_speed <= -24.0 {
                                "<<<< 32×"
                            } else if self.vcr_shuttle_speed <= -12.0 {
                                "<<< 16×"
                            } else if self.vcr_shuttle_speed <= -6.0 {
                                "<<< 8×"
                            } else if rw_on {
                                "<< 4×"
                            } else {
                                "<< RW"
                            };
                            if ui
                                .add(egui::Button::new(rw_lbl).selected(rw_on))
                                .on_hover_text("VHS Rewind picture search (click to step 4× → 8× → 16× → 32×, Space or Play to resume)")
                                .clicked()
                            {
                                shuttle_cmd = Some(-1.0);
                            }
                            let play_lbl = if rw_on || ff_on || paused { "Play >" } else { "Pause ||" };
                            if ui.button(play_lbl).clicked() {
                                if rw_on || ff_on {
                                    shuttle_cmd = Some(0.0);
                                } else {
                                    toggle = true;
                                }
                            }
                            let ff_lbl = if self.vcr_shuttle_speed >= 24.0 {
                                "32× >>>>"
                            } else if self.vcr_shuttle_speed >= 12.0 {
                                "16× >>>"
                            } else if self.vcr_shuttle_speed >= 6.0 {
                                "8× >>>"
                            } else if ff_on {
                                "4× >>"
                            } else {
                                "FF >>"
                            };
                            if ui
                                .add(egui::Button::new(ff_lbl).selected(ff_on))
                                .on_hover_text("VHS Fast-Forward picture search (click to step 4× → 8× → 16× → 32×, Space or Play to resume)")
                                .clicked()
                            {
                                shuttle_cmd = Some(1.0);
                            }
                            if ui.button("−10 s").clicked() {
                                seek = Some(-10.0);
                            }
                            if ui.button("+10 s").clicked() {
                                seek = Some(10.0);
                            }
                            ui.label(RichText::new(format!("{} / {}", hms(pos), hms(dur))).monospace().small());
                        });
                        if dur > 0.0 {
                            let mut v = self.seek_drag.unwrap_or(pos);
                            let r = ui.add(Slider::new(&mut v, 0.0..=dur).show_value(false).text("position"));
                            if r.dragged() {
                                let dir = v - self.seek_drag_last;
                                if dir.abs() > 0.05 {
                                    self.sim.trigger_scrub_shuttle(dir as f32);
                                    self.seek_drag_last = v;
                                } else if self.seek_drag.is_none() {
                                    self.seek_drag_last = pos;
                                }
                                let lbl = if dir < 0.0 { "REW <<" } else { "FF >>" };
                                self.osd.show_progress(lbl, v, dur);
                                self.seek_drag = Some(v);
                            } else if r.drag_stopped() || r.clicked() {
                                seek_abs = Some((v, pos));
                                self.seek_drag = None;
                            }
                        }
                    }
                } else if let Some(wp) = &self.web_player {
                    let st = wp.shared.lock().unwrap().status.clone();
                    ui.label(RichText::new(format!("{} — {}", self.video_name, st)).small());
                    ui.label(RichText::new("via headless Chromium CDP (640×480)").weak().small());
                }
                if ui.checkbox(&mut self.crop, "Crop widescreen to fill 4:3").changed() {
                    if let Some(p) = &self.player {
                        p.crop(self.crop);
                    }
                }
                ui.checkbox(&mut self.use_my_mpv, "Use my mpv settings (mpv.conf, key bindings, scripts)")
                    .on_hover_text("Loads your own mpv configuration. Your mpv keys work while the mouse is over the TV.");
                ui.add_space(4.0);
                ui.label(RichText::new("Link with my mpv").strong().small());
                ui.horizontal(|ui| {
                    if ui
                        .button("Add Ctrl+T shortcut to mpv")
                        .on_hover_text("Installs a tiny mpv script. In mpv, press Ctrl+T to pause there and continue watching on this TV.")
                        .clicked()
                    {
                        self.link_msg = match crate::link::install_mpv_script() {
                            Ok(files) => format!(
                                "Done. Restart mpv, then press Ctrl+T while watching to send the video here.\nInstalled: {}",
                                files.iter().map(|f| f.display().to_string()).collect::<Vec<_>>().join(", ")
                            ),
                            Err(e) => format!("Couldn't install the mpv script: {e}"),
                        };
                    }
                });
                if !self.link_msg.is_empty() {
                    ui.label(RichText::new(&self.link_msg).small());
                }
                ui.label(
                    RichText::new(
                        "Needs mpv installed. You can also drop a video onto the window, or run: victr --mpv <file or URL> [--start seconds].",
                    )
                    .weak()
                    .small(),
                );
                if let Some(cmd) = shuttle_cmd {
                    if cmd == 0.0 {
                        self.set_vcr_shuttle(0.0);
                    } else {
                        self.cycle_vcr_shuttle(cmd);
                    }
                }
                if toggle {
                    if let Some(p) = &self.player {
                        p.toggle_pause();
                    }
                }
                if let Some(d) = seek {
                    self.seek_video_rel(d);
                }
                if let Some((t, prev)) = seek_abs {
                    self.seek_video_abs(t, prev);
                }
            });
    }

    fn update_audio(&mut self) {
        let Some(a) = &self.audio else { return };
        let s = &self.s;
        let mut p = a.params.lock().unwrap();
        p.enabled = s.sound;
        p.volume = if self.muted { 0.0 } else { s.volume };
        p.tv_speaker = s.tv_speaker;
        p.mono = s.mono;
        p.screen_inches = s.screen_inches;
        p.whine = s.whine;
        p.power_on = self.sim.power_on;
        p.warm = self.sim.warm;
        p.degauss_t = self.sim.degauss_t;
        p.channel_switch_t = self.sim.channel_switch_t;
        p.whack_t = self.sim.whack_t;
        p.splash_static = if self.splash.is_static() { 1.0 } else { 0.0 };
        p.test_tone = s.test_tone && s.pattern == Pattern::ColorBars && !self.splash.is_active();
        let (c_sig_drop, c_noise, c_ghost, c_track, c_warble, c_hum, c_ripple) = self.sim.chaos_audio_offsets();
        let (ch_ft, ch_tr) = self.sim.channel_aft_offsets();
        p.rf = s.input == Input::Rf;
        p.snr_db = (s.signal_db - c_sig_drop - ch_ft.abs() * 8.0).max(-25.0);
        p.fine_tune = (s.fine_tune + ch_ft).clamp(-1.0, 1.0);
        p.ghost = (s.ghost_amp + c_ghost).clamp(-0.6, 0.6);
        p.hum = (s.hum + c_hum).min(1.0);
        p.impulse = s.impulse;
        p.extra_noise = s.extra_noise + c_noise + ch_ft.abs() * 0.06;
        p.vcr = s.vcr;
        let base_bw = match s.tape_speed {
            TapeSpeed::Sp => 10.0,
            TapeSpeed::Lp => 7.0,
            TapeSpeed::Ep => 5.0,
        };
        p.tape_bw_khz = base_bw * (1.0 - 0.35 * s.tape_wear);
        p.tape_wear = s.tape_wear;
        p.tracking = (s.tracking + c_track + ch_tr).clamp(-1.0, 1.0);
        p.dropouts = s.dropouts;
        p.wow = s.timebase_jitter + c_warble;
        p.head_switch = s.head_switch;
        let (sh_amt, sh_spd) = self.sim.shuttle_audio_state();
        p.shuttle = sh_amt;
        p.shuttle_speed = sh_spd;
        p.age = (s.age_hours / 40000.0).clamp(0.0, 1.0);
        p.ripple = s.ripple + c_ripple;
    }

    fn sound_controls(&mut self, ui: &mut egui::Ui) {
        let status = self.audio.as_ref().map(|a| a.status.lock().unwrap().clone()).unwrap_or_default();
        let s = &mut self.s;
        let player = &self.player;
        egui::CollapsingHeader::new(RichText::new("Sound").strong())
            .default_open(true)
            .show(ui, |ui| {
                ui.checkbox(&mut s.sound, "Sound on").on_hover_text(
                    "The sound goes through the same signal path as the picture: antenna static, tuning, hum, tape wear, and the TV's own speaker.",
                );
                if sl(ui, &mut s.volume, 0.0..=1.0, "Volume", "").changed() {
                    if let Some(p) = player {
                        p.volume((s.volume * 100.0) as f64);
                    }
                }
                ui.checkbox(&mut s.tv_speaker, "Through the TV's speaker")
                    .on_hover_text("Small cabinet speaker: thin bass, boxy resonance, dull treble. Its size follows the screen size.");
                ui.checkbox(&mut s.mono, "Mono (most CRT TVs)");
                sl(ui, &mut s.whine, 0.0..=1.0, "Flyback whine", "The 15.7 kHz squeal of the line output transformer (not everyone can hear it).");
                if sl(
                    ui,
                    &mut s.audio_sync_ms,
                    -250.0..=250.0,
                    "A/V sync (ms)",
                    "Fine lip-sync offset on top of automatic DSP latency compensation. Negative makes sound earlier, positive makes sound later.",
                )
                .changed()
                {
                    if let Some(p) = player {
                        p.audio_delay(s.audio_sync_ms as f64 / 1000.0);
                    }
                }
                ui.checkbox(&mut s.test_tone, "1 kHz tone with colour bars");
                if !status.is_empty() {
                    ui.label(RichText::new(status).weak().small());
                }
            });
    }

    fn load_image_bytes(&mut self, name: String, bytes: &[u8]) {
        match image::load_from_memory(bytes) {
            Ok(img) => {
                let mut rgba = img.to_rgba8();
                let (w, h) = rgba.dimensions();
                let maxd = 2048u32;
                if w > maxd || h > maxd {
                    let k = maxd as f32 / w.max(h) as f32;
                    rgba = image::imageops::resize(
                        &rgba,
                        ((w as f32 * k) as u32).max(1),
                        ((h as f32 * k) as u32).max(1),
                        image::imageops::FilterType::Triangle,
                    );
                }
                let (w, h) = rgba.dimensions();
                self.image_counter += 1;
                self.image = Some((self.image_counter, Arc::new((w as usize, h as usize, rgba.into_raw()))));
                self.image_name = name;
                self.s.pattern = Pattern::Image;
                self.message.clear();
            }
            Err(e) => self.message = format!("Couldn't open that picture: {e}"),
        }
    }

    fn pick_image(&mut self) {
        if let Some(path) = rfd::FileDialog::new()
            .add_filter("Pictures", &["png", "jpg", "jpeg", "bmp", "gif"])
            .pick_file()
        {
            match std::fs::read(&path) {
                Ok(b) => {
                    let name = path.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
                    self.load_image_bytes(name, &b);
                }
                Err(e) => self.message = format!("Couldn't read that file: {e}"),
            }
        }
    }

    fn view_half_at_zoom(&self, rect: egui::Rect, zoom: f32) -> f32 {
        let a = rect.width() / rect.height().max(1.0);
        let m: f32 = if self.s.show_bezel { 0.26 } else { 0.03 };
        let base = (1.0 + m).max((4.0 / 3.0 + m) / a);
        base / zoom
    }

    fn view_half(&self, rect: egui::Rect) -> f32 {
        self.view_half_at_zoom(rect, self.zoom)
    }

    fn apply_pixel_perfect_screen_size(&mut self) {
        if self.last_vis_rect.width() <= 0.0 || self.last_full_h_px <= 0.0 {
            return;
        }
        let h_1x = self.view_half_at_zoom(self.last_vis_rect, 1.0);
        let inches = pixel_perfect_screen_inches(self.s.pitch_mm, self.s.mask, self.last_full_h_px, h_1x);
        self.s.screen_inches = inches;
        self.zoom = 1.0;
        self.pan = [0.0, 0.0];
        self.reset_trails = true;
        self.status_toast = Some((
            format!("Pixel-perfect 1× phosphors: screen size {:.2}\" ({:.2} mm pitch)", inches, self.s.pitch_mm),
            std::time::Instant::now(),
        ));
    }

    fn picture_signal_controls(&mut self, ui: &mut egui::Ui) {
        let s = &mut self.s;
        egui::CollapsingHeader::new(RichText::new("Picture source").strong())
            .default_open(true)
            .show(ui, |ui| {
                egui::ComboBox::from_id_salt("pattern")
                    .selected_text(s.pattern.label())
                    .width(230.0)
                    .show_ui(ui, |ui| {
                        for p in Pattern::ALL {
                            ui.selectable_value(&mut s.pattern, p, p.label());
                        }
                    });
                if s.pattern == Pattern::FlatField {
                    ui.horizontal(|ui| {
                        ui.label("Field colour");
                        ui.color_edit_button_rgb(&mut s.flat_color);
                        for (n, c) in [("W", [1.0, 1.0, 1.0]), ("R", [1.0, 0.0, 0.0]), ("G", [0.0, 1.0, 0.0]), ("B", [0.0, 0.0, 1.0])] {
                            if ui.small_button(n).clicked() {
                                s.flat_color = c;
                            }
                        }
                    });
                }
                ui.horizontal(|ui| {
                    if ui.button("Open a picture…").clicked() {
                        self.message = "pick".into();
                    }
                    if !self.image_name.is_empty() {
                        ui.label(RichText::new(&self.image_name).weak());
                    }
                });
                ui.label(RichText::new("You can also drop a picture onto the window.").weak().small());
                ui.horizontal(|ui| {
                    ui.label("Scan mode");
                    ui.radio_value(&mut s.interlaced, false, "240p (consoles)");
                    ui.radio_value(&mut s.interlaced, true, "480i (broadcast)");
                });
            });

        egui::CollapsingHeader::new(RichText::new("Signal & antenna").strong())
            .default_open(true)
            .show(ui, |ui| {
                egui::ComboBox::from_id_salt("input")
                    .selected_text(s.input.label())
                    .width(230.0)
                    .show_ui(ui, |ui| {
                        for p in Input::ALL {
                            ui.selectable_value(&mut s.input, p, p.label());
                        }
                    });
                let rf = s.input == Input::Rf;
                ui.add_enabled_ui(rf, |ui| {
                    sl(ui, &mut s.signal_db, -25.0..=50.0, "Signal strength (dB)", "Carrier-to-noise ratio at the tuner. Below ~25 dB snow appears, below ~12 dB sync and colour start to fail.");
                    sl(ui, &mut s.fine_tune, -1.0..=1.0, "Fine tuning", "Off-tuning moves the picture on the IF filter slope: one way loses colour and rings, the other adds a sound/colour beat pattern.");
                    sl(ui, &mut s.ghost_amp, -0.6..=0.6, "Ghost strength", "Multipath reflection (e.g. from a building) arriving later than the direct signal.");
                    sl(ui, &mut s.ghost_delay_us, 0.2..=8.0, "Ghost delay (µs)", "Extra path length of the reflection: 1 µs ≈ 300 m.");
                    sl(ui, &mut s.interference, 0.0..=0.3, "RF interference", "A stray carrier beating with the picture: diagonal herringbone bars.");
                    sl(ui, &mut s.interference_freq, 0.01..=0.5, "Interference frequency", "Beat frequency relative to the sample clock.");
                    sl(ui, &mut s.hum, 0.0..=1.0, "Hum bars", "Mains hum leaking into the signal: slow rolling bright/dark bands.");
                    sl(ui, &mut s.impulse, 0.0..=1.0, "Impulse noise", "Ignition and switch sparks: short bursts of white streaks.");
                });
                sl(ui, &mut s.extra_noise, 0.0..=0.3, "Cable / baseband noise", "Gaussian noise added to any input.");
            });

        egui::CollapsingHeader::new(RichText::new("VCR & tape tracking").strong())
            .default_open(false)
            .show(ui, |ui| {
                ui.checkbox(&mut s.vcr, "Play through a VCR");
                ui.add_enabled_ui(s.vcr, |ui| {
                    ui.horizontal(|ui| {
                        ui.label("Tape speed");
                        ui.radio_value(&mut s.tape_speed, TapeSpeed::Sp, "SP");
                        ui.radio_value(&mut s.tape_speed, TapeSpeed::Lp, "LP");
                        ui.radio_value(&mut s.tape_speed, TapeSpeed::Ep, "EP");
                    });
                    ui.horizontal(|ui| {
                        sl(ui, &mut s.tracking, -1.0..=1.0, "Tracking", "Misalignment between the video heads and the recorded tracks: noise bars that crawl up the picture.");
                        if ui.small_button("Auto").clicked() {
                            s.tracking = 0.0;
                        }
                    });
                    sl(ui, &mut s.tape_wear, 0.0..=1.0, "Tape wear", "Worn oxide: more luma noise and colour streaking.");
                    sl(ui, &mut s.dropouts, 0.0..=1.0, "Dropouts", "Missing oxide flakes give short white streaks.");
                    sl(ui, &mut s.head_switch, 0.0..=1.0, "Head-switch skew", "The jump in timing when the drum switches heads, bending the bottom lines.");
                    sl(ui, &mut s.timebase_jitter, 0.0..=1.5, "Timebase wobble", "Tape stretch and drum speed variation: wavy verticals.");
                    sl(ui, &mut s.chroma_delay, 0.0..=10.0, "Colour delay (samples)", "Colour-under recording lags colour behind luma.");
                    sl(ui, &mut s.edge_enhance, 0.0..=2.0, "Edge enhancement", "The VCR's sharpening circuit: bright halos on edges.");
                });
            });

        egui::CollapsingHeader::new(RichText::new("TV controls").strong())
            .default_open(true)
            .show(ui, |ui| {
                sl(ui, &mut s.brightness, -1.0..=1.0, "Brightness", "Shifts the gun cutoff (black level).");
                sl(ui, &mut s.contrast, 0.2..=2.0, "Contrast (picture)", "Video amplifier gain.");
                sl(ui, &mut s.color, 0.0..=2.5, "Colour", "Chroma gain.");
                sl(ui, &mut s.tint, -60.0..=60.0, "Tint (°)", "Rotates the decoded hue (NTSC hue control).");
                sl(ui, &mut s.sharpness, -0.5..=2.0, "Sharpness", "Luma peaking.");
                ui.horizontal(|ui| {
                    ui.label("Y/C separation");
                    ui.radio_value(&mut s.comb_filter, false, "Notch").on_hover_text("Older sets: blurrier, dot crawl on colour edges.");
                    ui.radio_value(&mut s.comb_filter, true, "Comb").on_hover_text("Later sets: sharper, hanging dots on vertical colour edges.");
                });
                ui.checkbox(&mut s.color_killer, "Colour killer").on_hover_text("Turns colour off when the colour burst is too noisy.");
                sl(ui, &mut s.h_hold, -1.0..=1.0, "Horizontal hold", "Free-running frequency of the line oscillator. Too far and the AFC can't pull it in: the picture tears into diagonal bars.");
                sl(ui, &mut s.v_hold, -1.0..=1.0, "Vertical hold", "Free-running frequency of the field oscillator. Too far and the picture rolls.");
                sl(ui, &mut s.afc_speed, 0.02..=1.0, "AFC speed", "How fast the line oscillator follows the incoming sync: slow = stable on noise but bends with VCR jumps.");
            });
    }

    fn crt_tube_controls(&mut self, ui: &mut egui::Ui) {
        let s = &mut self.s;
        egui::CollapsingHeader::new(RichText::new("Picture tube").strong())
            .default_open(true)
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    sl(ui, &mut s.screen_inches, 4.0..=36.0, "Screen size (in)", "Visible diagonal. Sets how big the mask pitch is relative to the picture.");
                    if ui
                        .small_button("1:1")
                        .on_hover_text("Set screen size so each phosphor is pixel-perfect at 1× zoom (Ctrl+1)")
                        .clicked()
                    {
                        self.pixel_perfect_req = true;
                    }
                });
                egui::ComboBox::from_id_salt("mask")
                    .selected_text(s.mask.label())
                    .width(230.0)
                    .show_ui(ui, |ui| {
                        for p in Mask::ALL {
                            ui.selectable_value(&mut s.mask, p, p.label());
                        }
                    });
                sl(ui, &mut s.pitch_mm, 0.2..=1.5, "Mask pitch (mm)", "Distance between phosphor triads (default 0.62 mm). Zoom in or press Ctrl+1 to see them.");
                sl(ui, &mut s.mask_strength, 0.0..=1.0, "Mask visibility", "1 = physically correct; lower blends the phosphor pattern away.");
                sl(ui, &mut s.anti_moire, 0.0..=1.0, "Anti-moiré", "Aligns the mask to display pixels around 1× zoom and pre-filters scanlines so curved glass stays free of moiré rings.");
                sl(ui, &mut s.black_matrix, 0.0..=0.6, "Black matrix", "Dark surround between phosphors (improves contrast, costs brightness).");
                sl(ui, &mut s.spot, 0.12..=1.0, "Beam spot height", "Electron spot size relative to the line pitch: small = visible scanlines.");
                sl(ui, &mut s.spot_h, 0.3..=4.0, "Beam spot width", "Horizontal spot size (in samples).");
                sl(ui, &mut s.bloom, 0.0..=2.0, "Spot blooming", "Higher beam current widens the spot: bright lines grow fatter.");
                sl(ui, &mut s.gamma, 1.8..=3.0, "Gun gamma", "Electron gun transfer curve (current ∝ voltage^γ).");
                sl(ui, &mut s.color_temp, 5000.0..=11000.0, "White point (K)", "6500 K is the broadcast standard; many sets shipped at 9300 K.");
                egui::ComboBox::from_id_salt("phosphor")
                    .selected_text(s.phosphor.label())
                    .width(230.0)
                    .show_ui(ui, |ui| {
                        for p in PhosphorSet::ALL {
                            ui.selectable_value(&mut s.phosphor, p, p.label());
                        }
                    });
                sl(ui, &mut s.persistence, 0.0..=0.9, "Phosphor afterglow", "Fraction of light emitted in the slow decay tail.");
                sl(ui, &mut s.persistence_ms, 1.0..=300.0, "Afterglow time (ms)", "Time constant of the decay tail.");
                sl(ui, &mut s.phosphor_sat, 0.0..=1.0, "Phosphor saturation", "Light output flattens at very high current density.");
                sl(ui, &mut s.curvature, 0.0..=0.12, "Screen curvature", "Bulge of the faceplate.");
                sl(ui, &mut s.corner, 0.0..=0.35, "Corner rounding", "");
                sl(ui, &mut s.overscan, -0.05..=0.2, "Overscan", "How much of the picture is hidden behind the bezel.");
                ui.horizontal(|ui| {
                    ui.add(Slider::new(&mut s.h_size, 0.7..=1.3).text("H size"));
                });
                ui.horizontal(|ui| {
                    ui.add(Slider::new(&mut s.v_size, 0.7..=1.3).text("V size"));
                });
                sl(ui, &mut s.h_pos, -1.0..=1.0, "H position", "");
                sl(ui, &mut s.v_pos, -1.0..=1.0, "V position", "");
                sl(ui, &mut s.pincushion, -1.0..=1.5, "Pincushion", "Uncorrected deflection geometry: sides bow in (or out when negative).");
                ui.label(RichText::new("Convergence (mm)").small());
                sl(ui, &mut s.conv_r[0], -2.0..=2.0, "Red horizontal", "");
                sl(ui, &mut s.conv_r[1], -2.0..=2.0, "Red vertical", "");
                sl(ui, &mut s.conv_b[0], -2.0..=2.0, "Blue horizontal", "");
                sl(ui, &mut s.conv_b[1], -2.0..=2.0, "Blue vertical", "");
                sl(ui, &mut s.conv_dyn, 0.0..=3.0, "Corner misconvergence", "Guns drift apart towards the corners.");
                sl(ui, &mut s.abl, 0.0..=1.0, "Auto beam limiter", "Bright scenes reduce contrast to protect the tube.");
                sl(ui, &mut s.breathing, 0.0..=1.0, "HV breathing", "Weak high-voltage regulation: the picture grows on bright scenes.");
            });

        egui::CollapsingHeader::new(RichText::new("Scanning & flicker").strong())
            .default_open(true)
            .show(ui, |ui| {
                ui.checkbox(&mut s.rolling_scan, "Rolling scan (the beam paints the picture)").on_hover_text(
                    "Each spot lights up when the beam sweeps over it, left to right and top to bottom, then fades. \
                     Every frame on your monitor shows only the light given off during that frame.",
                );
                ui.add_enabled_ui(s.rolling_scan, |ui| {
                    ui.add(Slider::new(&mut s.slow_mo, 1.0..=20000.0).logarithmic(true).text("Slow motion ×"))
                        .on_hover_text("1× is real time. Slow it down to watch the beam draw each line and the glow fade behind it.");
                    ui.horizontal(|ui| {
                        for (label, v) in [("Real", 1.0), ("100×", 100.0), ("1000×", 1000.0), ("10000×", 10000.0)] {
                            if ui.small_button(label).clicked() {
                                s.slow_mo = v;
                            }
                        }
                    });
                    ui.add(Slider::new(&mut s.decay_ms, 0.02..=8.0).logarithmic(true).text("Phosphor decay (ms)"))
                        .on_hover_text("How fast the green phosphor fades after the beam passes; red fades about 3× slower, blue about 3× faster.");
                    ui.add(Slider::new(&mut s.scan_exposure, 0.1..=10.0).logarithmic(true).text("Camera exposure"))
                        .on_hover_text("Brightness of the slow-motion view, like a high-speed camera's exposure.");
                    ui.label(
                        RichText::new(
                            "At 1× on a 60 Hz monitor the scan is too fast to show and the picture looks steady, like a CRT to your eye. \
                             On a 120 Hz+ monitor you see real flicker; in slow motion you can watch the beam.",
                        )
                        .weak()
                        .small(),
                    );
                });
            });

        egui::CollapsingHeader::new(RichText::new("Aging & wear").strong())
            .default_open(true)
            .show(ui, |ui| {
                let mut age = s.age_hours;
                if ui
                    .add(Slider::new(&mut age, 0.0..=40000.0).text("Tube age (hours)").step_by(100.0))
                    .on_hover_text("Sets all the wear values below at once.")
                    .changed()
                {
                    s.apply_age(age);
                }
                sl(ui, &mut s.wear[0], 0.0..=1.0, "Red cathode wear", "Low emission: dimmer, highlights smear and turn 'silvery'.");
                sl(ui, &mut s.wear[1], 0.0..=1.0, "Green cathode wear", "");
                sl(ui, &mut s.wear[2], 0.0..=1.0, "Blue cathode wear", "");
                sl(ui, &mut s.focus_loss, 0.0..=1.5, "Focus loss", "Defocused, wider electron spot.");
                sl(ui, &mut s.cutoff_drift, 0.0..=1.5, "Grey-scale drift", "Gun cutoffs drift apart: tinted shadows.");
                if sl(ui, &mut s.magnetization, 0.0..=1.0, "Magnetisation", "A magnetised mask bends the beams onto the wrong phosphors: colour blotches.").changed() {
                    self.degauss_mag_start = None;
                }
                sl(ui, &mut s.glass_yellow, 0.0..=1.0, "Glass browning", "Years of X-rays darken and yellow the faceplate.");
                sl(ui, &mut s.ripple, 0.0..=1.5, "Capacitor ripple", "Dried-out filter capacitors: a wobbling band rolls slowly through the raster.");
                sl(ui, &mut s.burn_speed, 0.0..=1.0, "Burn-in speed", "Speeds up phosphor wear from static pictures (time-lapse).");
                sl(ui, &mut s.burn_visible, 0.0..=1.0, "Burn-in visibility", "");
                ui.horizontal(|ui| {
                    if ui.button("Burn in this picture").clicked() {
                        self.burn_now = true;
                    }
                    if ui.button("New phosphor (clear)").clicked() {
                        self.clear_burn = true;
                    }
                });
                ui.separator();
                let faults = self.sim.chaos_active_faults();
                ui.checkbox(&mut s.chaos, "Chaos engine (signal & sync drift)")
                    .on_hover_text("Novelty mode: occasional weak signal/snow, multipath ghosting, VCR tracking drift, timebase warble, H/V-hold slip, or hum/ripple. Hit Whack! (or K) to knock it back into gear.");
                ui.add_enabled_ui(s.chaos, |ui| {
                    sl(ui, &mut s.chaos_rate, 0.1..=2.0, "Drift frequency", "How often the old chassis acts up.");
                    ui.horizontal(|ui| {
                        let whack_lbl = if faults.is_empty() {
                            RichText::new("Whack cabinet! (K)")
                        } else {
                            RichText::new("Whack cabinet! (K)").color(Color32::from_rgb(255, 210, 96)).strong()
                        };
                        if ui
                            .button(whack_lbl)
                            .on_hover_text("Percussive maintenance: slap the side of the CRT to knock drifting circuits back into lock (shortcut: K).")
                            .clicked()
                        {
                            self.sim.whack();
                        }
                        if faults.is_empty() {
                            ui.label(RichText::new("Chassis steady").weak().small());
                        } else {
                            ui.label(
                                RichText::new(format!("Drifting: {}", faults.join(", ")))
                                    .small()
                                    .color(Color32::from_rgb(245, 168, 86)),
                            );
                        }
                    });
                });
            });

        egui::CollapsingHeader::new(RichText::new("Room, glass & eye").strong())
            .default_open(false)
            .show(ui, |ui| {
                sl(ui, &mut s.ambient, 0.0..=1.5, "Room light", "Light falling on the screen: raises blacks and adds reflections.");
                sl(ui, &mut s.reflection, 0.0..=1.5, "Glass reflection", "");
                sl(ui, &mut s.halation, 0.0..=0.6, "Halation", "Light scattered inside the thick faceplate glass: wide glow around bright areas.");
                sl(ui, &mut s.diffusion, 0.0..=0.8, "Glass diffusion", "Short-range light spread in the glass and phosphor layer.");
                sl(ui, &mut s.vignette, 0.0..=1.0, "Corner falloff", "Beams hit the corners at a slant: less light there.");
                sl(ui, &mut s.exposure, 0.3..=3.0, "Viewing exposure", "");
                sl(ui, &mut s.eye_ms, 0.0..=60.0, "Eye / camera integration (ms)", "How long your eye (or a camera) blends successive fields. 0 = a fast shutter.");
                ui.checkbox(&mut s.show_bezel, "Show cabinet bezel");
                ui.separator();
                let hdr_active = self.hdr_status.active_mode.is_hdr();
                ui.checkbox(&mut s.hdr_mode, "Native HDR output (OLED / HDR)")
                    .on_hover_text("Requests a native 16-bit float scRGB / EDR or 10-bit HDR10 PQ swapchain via wgpu so individual CRT phosphors can hit 600–1000+ nits while the UI stays comfortable. Automatically falls back to SDR when the display/compositor is in SDR.");
                let status_col = if hdr_active {
                    Color32::from_rgb(112, 236, 184)
                } else {
                    Color32::from_rgb(184, 174, 156)
                };
                let status_lbl = ui.label(RichText::new(format!("Output: {}", self.hdr_status.summary())).monospace().small().color(status_col));
                if !self.hdr_status.adapter_name.is_empty() {
                    status_lbl.on_hover_text(format!("GPU: {}", self.hdr_status.adapter_name));
                }
                if s.hdr_mode && !self.hdr_status.supported {
                    ui.label(
                        RichText::new("No HDR swapchain reported by OS/compositor — running in SDR fallback.")
                            .weak()
                            .small(),
                    );
                }
                ui.add_enabled_ui(hdr_active, |ui| {
                    if self.hdr_status.has_scrgb && self.hdr_status.has_pq {
                        ui.checkbox(&mut self.hdr_prefer_pq, "Prefer 10-bit HDR10 PQ over scRGB")
                            .on_hover_text("Switches between 16-bit float scRGB and 10-bit BT.2020 ST.2084 PQ if your compositor/display prefers one over the other.");
                    }
                    sl(ui, &mut s.hdr_paper_white, 80.0..=500.0, "HDR paper white (nits)", "Average brightness of nominal white; individual lit phosphors peak 3–5× higher.");
                    sl(ui, &mut s.hdr_peak_nits, 300.0..=2000.0, "HDR peak luminance (nits)", "Peak brightness ceiling of your OLED/HDR display.");
                    sl(ui, &mut s.hdr_ui_nits, 80.0..=400.0, "HDR UI brightness (nits)", "Brightness of the ViCTR control bar and service panel in HDR mode.");
                });
                ui.label(RichText::new("Tip: set Room light to 0 for true 0.000 OLED pure blacks.").weak().small());
            });
    }

    fn about_controls(&mut self, ui: &mut egui::Ui) {
        egui::CollapsingHeader::new(RichText::new("About ViCTR").strong())
            .default_open(false)
            .show(ui, |ui| {
                ui.label(
                    RichText::new(
                        "ViCTR (Virtual CRT Television Receiver) is an open-source, physics-based CRT simulator modeling the analog NTSC signal chain, electron optics, phosphor persistence, rolling scan, and audio path.",
                    )
                    .small(),
                );
                let path = crate::about::app_dir()
                    .or_else(crate::about::exe_path)
                    .map(|p| p.display().to_string())
                    .unwrap_or_default();
                if !path.is_empty() {
                    ui.label(RichText::new(&path).monospace().small());
                }
                ui.horizontal(|ui| {
                    if !path.is_empty() && ui.button("Copy location").clicked() {
                        ui.ctx().copy_text(path.clone());
                        self.about_msg = "Copied.".into();
                    }
                    if ui.button("Add to my apps menu").on_hover_text("Adds a \"ViCTR\" entry with its own icon to your desktop's apps menu.").clicked() {
                        self.about_msg = match crate::about::install_menu_entry() {
                            Ok(m) => m,
                            Err(e) => e,
                        };
                    }
                });
                if !self.about_msg.is_empty() {
                    ui.label(RichText::new(&self.about_msg).small());
                }
                ui.separator();
                if ui
                    .checkbox(&mut self.app_cfg.start_fullscreen, "Start in fullscreen")
                    .on_hover_text("Automatically launch ViCTR in borderless fullscreen (F11 or Esc to exit).")
                    .changed()
                {
                    let _ = settings::save_app_config(&self.app_cfg);
                }
            });
    }

    fn sidebar_contents(&mut self, ui: &mut egui::Ui) {
        let full_rect = ui.max_rect();
        // Faux walnut woodgrain side rails + brushed dark-metal faceplate
        paint_wood_panel(ui.painter(), full_rect, false);
        let metal_rect = full_rect.shrink2(egui::vec2(7.0, 4.0));
        paint_brushed_metal(ui.painter(), metal_rect);
        ui.painter().rect_stroke(
            metal_rect,
            2.0,
            egui::Stroke::new(1.0, Color32::from_rgb(138, 126, 106)),
            egui::StrokeKind::Inside,
        );
        paint_screw(ui.painter(), metal_rect.left_top() + egui::vec2(7.0, 7.0), 2.5);
        paint_screw(ui.painter(), metal_rect.right_top() + egui::vec2(-7.0, 7.0), 2.5);
        paint_screw(ui.painter(), metal_rect.left_bottom() + egui::vec2(7.0, -7.0), 2.5);
        paint_screw(ui.painter(), metal_rect.right_bottom() + egui::vec2(-7.0, -7.0), 2.5);

        let inner_frame = egui::Frame::NONE.inner_margin(egui::Margin { left: 14, right: 14, top: 10, bottom: 10 });
        inner_frame.show(ui, |ui| {
            let content_w = (full_rect.width() - 28.0).max(200.0);
            ui.set_max_width(content_w);

            // Header row
            ui.horizontal(|ui| {
                ui.label(
                    RichText::new("QUICK MENU")
                        .strong()
                        .size(12.5)
                        .color(Color32::from_rgb(242, 184, 96)),
                );
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui
                        .small_button("Close >")
                        .on_hover_text("Slide the quick menu closed")
                        .clicked()
                    {
                        self.sidebar_open = false;
                    }
                    if ui
                        .add(egui::Button::new(RichText::new("Quit").color(Color32::from_rgb(236, 152, 132))).small())
                        .on_hover_text("Power down the CRT, save watch progress, and quit (Ctrl+Q)")
                        .clicked()
                    {
                        self.begin_quit();
                    }
                    if self.fullscreen
                        && ui
                            .small_button("Exit FS")
                            .on_hover_text("Leave fullscreen (F11 or Esc)")
                            .clicked()
                    {
                        self.message = "exitfs".into();
                    }
                });
            });
            ui.separator();

            egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
                ui.set_width((content_w - 6.0).max(190.0));
                ui.add_space(2.0);

                // ---- Active Game Console (when running on CH 03)
                self.retro_running_controls(ui);

                // ---- Active Video / Tape Transport (when playing a file or stream)
                if let Some(p) = &self.player {
                    let (pos, dur, paused, running) = {
                        let sh = p.shared.lock().unwrap();
                        (sh.pos, sh.duration, sh.paused, sh.running)
                    };
                    if running {
                        let rw_on = self.vcr_shuttle_speed < -0.1;
                        let ff_on = self.vcr_shuttle_speed > 0.1;
                        let mut q_shuttle: Option<f32> = None;
                        let mut q_toggle = false;
                        let mut q_seek: Option<f64> = None;
                        let mut q_seek_abs: Option<(f64, f64)> = None;
                        ui.horizontal(|ui| {
                            ui.label(RichText::new("TAPE / TRANSPORT").strong().small().color(Color32::from_rgb(232, 186, 102)));
                            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                if dur > 0.0 {
                                    ui.label(RichText::new(format!("{}/{}", hms(pos), hms(dur))).monospace().small().weak());
                                }
                            });
                        });
                        ui.horizontal_wrapped(|ui| {
                            let rw_lbl = if self.vcr_shuttle_speed <= -24.0 {
                                "<<< 32×"
                            } else if self.vcr_shuttle_speed <= -12.0 {
                                "<<< 16×"
                            } else if self.vcr_shuttle_speed <= -6.0 {
                                "<<< 8×"
                            } else if rw_on {
                                "<< 4×"
                            } else {
                                "<< RW"
                            };
                            if ui
                                .add(egui::Button::new(rw_lbl).selected(rw_on).small())
                                .on_hover_text("VHS Rewind picture search (click for 4× → 8× → 16× → 32×)")
                                .clicked()
                            {
                                q_shuttle = Some(-1.0);
                            }
                            let play_lbl = if rw_on || ff_on || paused { "Play >" } else { "Pause" };
                            if ui.small_button(play_lbl).clicked() {
                                if rw_on || ff_on {
                                    q_shuttle = Some(0.0);
                                } else {
                                    q_toggle = true;
                                }
                            }
                            let ff_lbl = if self.vcr_shuttle_speed >= 24.0 {
                                "32× >>>"
                            } else if self.vcr_shuttle_speed >= 12.0 {
                                "16× >>>"
                            } else if self.vcr_shuttle_speed >= 6.0 {
                                "8× >>>"
                            } else if ff_on {
                                "4× >>"
                            } else {
                                "FF >>"
                            };
                            if ui
                                .add(egui::Button::new(ff_lbl).selected(ff_on).small())
                                .on_hover_text("VHS Fast-Forward picture search (click for 4× → 8× → 16× → 32×)")
                                .clicked()
                            {
                                q_shuttle = Some(1.0);
                            }
                            if ui.small_button("-10s").clicked() {
                                q_seek = Some(-10.0);
                            }
                            if ui.small_button("+10s").clicked() {
                                q_seek = Some(10.0);
                            }
                        });
                        if dur > 0.0 {
                            let mut v = self.seek_drag.unwrap_or(pos);
                            let r = ui.add(
                                Slider::new(&mut v, 0.0..=dur)
                                    .show_value(false),
                            );
                            if r.dragged() {
                                let dir = v - self.seek_drag_last;
                                if dir.abs() > 0.05 {
                                    self.sim.trigger_scrub_shuttle(dir as f32);
                                    self.seek_drag_last = v;
                                } else if self.seek_drag.is_none() {
                                    self.seek_drag_last = pos;
                                }
                                let lbl = if dir < 0.0 { "REW <<" } else { "FF >>" };
                                self.osd.show_progress(lbl, v, dur);
                                self.seek_drag = Some(v);
                            } else if r.drag_stopped() || r.clicked() {
                                q_seek_abs = Some((v, pos));
                                self.seek_drag = None;
                            }
                        }
                        if let Some(cmd) = q_shuttle {
                            if cmd == 0.0 {
                                self.set_vcr_shuttle(0.0);
                            } else {
                                self.cycle_vcr_shuttle(cmd);
                            }
                        }
                        if q_toggle {
                            if let Some(p) = &self.player {
                                p.toggle_pause();
                            }
                        }
                        if let Some(d) = q_seek {
                            self.seek_video_rel(d);
                        }
                        if let Some((t, prev)) = q_seek_abs {
                            self.seek_video_abs(t, prev);
                        }
                        ui.separator();
                    }
                }

                // ---- Active External Capture badge (when mirroring Dolphin/PCSX2/etc.)
                if let Some(cp) = &self.capture_player {
                    let st = cp.shared.lock().unwrap().status.clone();
                    ui.horizontal(|ui| {
                        ui.label(RichText::new("AV 1 · Live Capture").strong().color(Color32::from_rgb(138, 240, 178)));
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if ui.small_button("Stop").clicked() {
                                self.capture_player = None;
                            }
                        });
                    });
                    ui.label(RichText::new(st).weak().small());
                    ui.separator();
                }

                // ---- Receiver quick actions
                ui.horizontal_wrapped(|ui| {
                    let pwr_label = if self.sim.power_on {
                        RichText::new("Power").color(Color32::WHITE).strong()
                    } else {
                        RichText::new("Power off").color(Color32::from_rgb(210, 140, 120))
                    };
                    if ui.add(egui::Button::new(pwr_label).selected(self.sim.power_on).small()).clicked() {
                        let on = !self.sim.power_on;
                        self.sim.set_power(on);
                    }
                    if ui.small_button("Degauss").clicked() {
                        self.sim.degauss();
                        if self.s.magnetization > 0.0 {
                            self.degauss_mag_start = Some(self.s.magnetization);
                        }
                    }
                    if self.s.chaos && ui.small_button("Whack!").clicked() {
                        self.sim.whack();
                    }
                    if ui.small_button("Reset view").clicked() {
                        self.zoom = 1.0;
                        self.pan = [0.0, 0.0];
                        self.reset_trails = true;
                    }
                    if !self.fullscreen && ui.small_button("Fullscreen").clicked() {
                        self.set_fullscreen(true);
                    }
                });

                ui.add_space(4.0);

                // ---- Quick TV Preset switcher
                let mut sel = self.preset_sel;
                let selected_label = self.current_preset_label();
                ui.horizontal(|ui| {
                    ui.label(RichText::new("Preset").small().weak());
                    egui::ComboBox::from_id_salt("quick_preset_sel")
                        .selected_text(selected_label)
                        .width((content_w - 110.0).max(110.0))
                        .show_ui(ui, |ui| {
                            for (i, n) in settings::PRESETS.iter().enumerate() {
                                ui.selectable_value(&mut sel, PresetSel::BuiltIn(i), *n);
                            }
                            if !self.user_presets.is_empty() {
                                ui.separator();
                                ui.label(RichText::new("Saved presets").weak().small());
                                for (i, p) in self.user_presets.iter().enumerate() {
                                    ui.selectable_value(&mut sel, PresetSel::User(i), &p.name);
                                }
                            }
                        });
                    if ui.small_button("Save…").clicked() {
                        self.open_save_preset_dialog();
                    }
                });
                if sel != self.preset_sel {
                    self.apply_preset(sel);
                }

                // ---- Quick Volume, Mute & HDR
                ui.horizontal(|ui| {
                    let mute_lbl = if self.muted { "Unmute" } else { "Mute" };
                    if ui.small_button(mute_lbl).clicked() {
                        self.toggle_mute();
                    }
                    if ui
                        .add(
                            Slider::new(&mut self.s.volume, 0.0..=1.0)
                                .show_value(false)
                                .text("Vol"),
                        )
                        .changed()
                    {
                        if let Some(p) = &self.player {
                            p.volume(if self.muted { 0.0 } else { (self.s.volume * 100.0) as f64 });
                        }
                    }
                    if self.hdr_status.supported {
                        let hdr_on = self.hdr_status.active_mode.is_hdr();
                        let hdr_lbl = if hdr_on {
                            RichText::new("HDR").color(Color32::from_rgb(255, 214, 96)).strong()
                        } else {
                            RichText::new("HDR").weak()
                        };
                        if ui.add(egui::Button::new(hdr_lbl).selected(hdr_on).small()).clicked() {
                            self.s.hdr_mode = !self.s.hdr_mode;
                        }
                    }
                });

                // ---- Quick Cable Remote (when any channel source is active)
                if self.has_channels() {
                    ui.separator();
                    if let Some(w) = &self.active_watch {
                        ui.label(
                            RichText::new(format!("CH {:02} {} · {}", w.channel_number, w.callsign, w.title))
                                .small()
                                .color(Color32::from_rgb(138, 240, 178)),
                        );
                    } else if let Some(ch) = self.current_channel {
                        ui.label(
                            RichText::new(format!("CH {:02}", ch))
                                .small()
                                .color(Color32::from_rgb(138, 240, 178)),
                        );
                    }
                    ui.horizontal_wrapped(|ui| {
                        let glbl = if self.guide.active { "Exit Guide (G)" } else { "Cable Guide (G)" };
                        if ui.add(egui::Button::new(glbl).selected(self.guide.active).small()).clicked() {
                            self.toggle_guide();
                        }
                        if ui.small_button("Ch -").clicked() {
                            self.surf_channel(-1);
                        }
                        if ui.small_button("Ch +").clicked() {
                            self.surf_channel(1);
                        }
                        if self.active_watch.is_some() {
                            if ui.small_button("Prev").clicked() {
                                self.step_episode(-1);
                            }
                            if ui.small_button("Next").clicked() {
                                self.step_episode(1);
                            }
                        }
                    });
                }

                // ---- Full Tabbed Settings Window Launchers
                ui.separator();
                ui.label(
                    RichText::new("FULL CONTROLS (TABBED WINDOW)")
                        .strong()
                        .small()
                        .color(Color32::from_rgb(232, 186, 102)),
                );
                ui.add_space(3.0);
                let btn_w = (content_w - 10.0).max(180.0);
                for (tab, subtitle) in [
                    (SettingsTab::Channels, "Guide, channels.json, Profiles, Jellyfin, Plex, M3U8, Web"),
                    (SettingsTab::Games, "CH 03 libretro ROMs, Gamepads, Dolphin/PCSX2 Capture"),
                    (SettingsTab::Picture, "Presets, Video, 240p/480i, RF/Composite/S-Video, VCR"),
                    (SettingsTab::CrtTube, "Mask, Phosphors, Rolling Scan, Burn-in, Chaos, Room/HDR"),
                    (SettingsTab::Sound, "Analog TV Speaker/Hum/Flyback DSP & App Info"),
                ] {
                    let active = self.settings_open && self.settings_tab == tab;
                    let label = if active {
                        RichText::new(format!("{}  (Open)", tab.label())).strong().color(Color32::from_rgb(255, 214, 112))
                    } else {
                        RichText::new(format!("{}…", tab.label()))
                    };
                    if ui
                        .add_sized([btn_w, 26.0], egui::Button::new(label).selected(active))
                        .on_hover_text(subtitle)
                        .clicked()
                    {
                        self.open_settings_tab(tab);
                    }
                }

                if !self.message.is_empty()
                    && self.message != "pick"
                    && self.message != "pickvideo"
                    && self.message != "pickfolder"
                    && self.message != "pickm3u"
                    && self.message != "pickrom"
                    && self.message != "pickromdir"
                    && self.message != "exitfs"
                {
                    ui.add_space(6.0);
                    ui.colored_label(Color32::from_rgb(230, 120, 90), &self.message);
                }

                ui.add_space(8.0);
                ui.label(
                    RichText::new("Scroll to zoom · Drag to pan · Double-click to reset · G for Cable Guide.")
                        .weak()
                        .small(),
                );
                ui.add_space(8.0);
            });
        });
    }

    fn show_settings_window(&mut self, ctx: &egui::Context) {
        if !self.settings_open {
            self.settings_rect = None;
            return;
        }
        let mut open = self.settings_open;
        let win_resp = egui::Window::new("ViCTR Service & Settings")
            .id(egui::Id::unique("victr_full_settings_win"))
            .open(&mut open)
            .collapsible(false)
            .resizable(true)
            .default_size([660.0, 620.0])
            .min_size([460.0, 360.0])
            .default_pos([64.0, 56.0])
            .show(ctx, |ui| {
                // Top horizontal tab bar
                ui.horizontal_wrapped(|ui| {
                    for tab in SettingsTab::ALL {
                        let sel = self.settings_tab == tab;
                        let lbl = if sel {
                            RichText::new(tab.label()).strong().color(Color32::from_rgb(255, 214, 112))
                        } else {
                            RichText::new(tab.label())
                        };
                        if ui.selectable_label(sel, lbl).clicked() {
                            self.settings_tab = tab;
                        }
                    }
                });
                ui.separator();

                egui::ScrollArea::vertical()
                    .id_salt(("settings_tab_scroll", self.settings_tab as u8))
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        let w = (ui.available_width() - 8.0).max(320.0);
                        ui.set_width(w);
                        match self.settings_tab {
                            SettingsTab::Channels => {
                                self.channels_sources_controls(ui);
                            }
                            SettingsTab::Games => {
                                self.games_capture_controls(ui);
                            }
                            SettingsTab::Picture => {
                                self.preset_controls(ui);
                                self.video_controls(ui);
                                self.picture_signal_controls(ui);
                            }
                            SettingsTab::CrtTube => {
                                self.crt_tube_controls(ui);
                            }
                            SettingsTab::Sound => {
                                self.sound_controls(ui);
                                self.about_controls(ui);
                            }
                        }
                        ui.add_space(12.0);
                    });
            });
        self.settings_rect = win_resp.map(|r| r.response.rect);
        self.settings_open = open;
    }
}

/// Paints a procedural 1970s faux walnut woodgrain finish inside `rect`.
fn paint_wood_panel(painter: &egui::Painter, rect: egui::Rect, horizontal: bool) {
    if rect.width() <= 0.0 || rect.height() <= 0.0 {
        return;
    }
    painter.rect_filled(rect, 0.0, Color32::from_rgb(64, 38, 21));
    let step = 2.0f32;
    if horizontal {
        let mut y = rect.top();
        while y < rect.bottom() {
            let h = step.min(rect.bottom() - y);
            let t = y - rect.top();
            let g = 0.5
                + 0.30 * (t * 0.27 + (t * 0.07).sin() * 1.4).sin()
                + 0.14 * (t * 0.71).cos()
                + 0.06 * (t * 1.53).sin();
            let v = (y - rect.top()) / rect.height().max(1.0);
            let sheen = 1.08 - 0.24 * v;
            let r = ((44.0 + 50.0 * g) * sheen).clamp(0.0, 255.0) as u8;
            let gr = ((24.0 + 30.0 * g) * sheen).clamp(0.0, 255.0) as u8;
            let b = ((12.0 + 16.0 * g) * sheen).clamp(0.0, 255.0) as u8;
            painter.rect_filled(
                egui::Rect::from_min_size(egui::pos2(rect.left(), y), egui::vec2(rect.width(), h)),
                0.0,
                Color32::from_rgb(r, gr, b),
            );
            y += step;
        }
    } else {
        let mut x = rect.left();
        while x < rect.right() {
            let w = step.min(rect.right() - x);
            let t = x - rect.left();
            let g = 0.5
                + 0.32 * (t * 0.31 + (t * 0.09).sin() * 1.3).sin()
                + 0.12 * (t * 0.79).cos()
                + 0.06 * (t * 1.61).sin();
            let r = (42.0 + 48.0 * g).clamp(0.0, 255.0) as u8;
            let gr = (23.0 + 29.0 * g).clamp(0.0, 255.0) as u8;
            let b = (11.0 + 15.0 * g).clamp(0.0, 255.0) as u8;
            painter.rect_filled(
                egui::Rect::from_min_size(egui::pos2(x, rect.top()), egui::vec2(w, rect.height())),
                0.0,
                Color32::from_rgb(r, gr, b),
            );
            x += step;
        }
    }
    // Lacquer top highlight and recessed bottom edge
    painter.line_segment(
        [rect.left_top() + egui::vec2(0.0, 0.5), rect.right_top() + egui::vec2(0.0, 0.5)],
        egui::Stroke::new(1.0, Color32::from_rgba_unmultiplied(245, 200, 145, 38)),
    );
    painter.line_segment(
        [rect.left_bottom() - egui::vec2(0.0, 0.5), rect.right_bottom() - egui::vec2(0.0, 0.5)],
        egui::Stroke::new(1.0, Color32::from_rgba_unmultiplied(0, 0, 0, 140)),
    );
}

/// Paints a brushed dark gunmetal / anodized aluminum receiver faceplate inside `rect`.
fn paint_brushed_metal(painter: &egui::Painter, rect: egui::Rect) {
    if rect.width() <= 0.0 || rect.height() <= 0.0 {
        return;
    }
    painter.rect_filled(rect, 2.0, Color32::from_rgb(38, 36, 33));
    let bands = 10usize;
    let bh = rect.height() / bands as f32;
    for i in 0..bands {
        let t = i as f32 / (bands - 1).max(1) as f32;
        // Subtle metallic satin reflection across the faceplate height
        let luma = 47.0 - 15.0 * t + 5.0 * (t * std::f32::consts::PI * 2.0).sin();
        let col = Color32::from_rgb(luma as u8, (luma * 0.96) as u8, (luma * 0.90) as u8);
        let y0 = rect.top() + i as f32 * bh;
        painter.rect_filled(
            egui::Rect::from_min_size(egui::pos2(rect.left(), y0), egui::vec2(rect.width(), bh + 0.5)),
            0.0,
            col,
        );
    }
    // Fine horizontal hairline brush striations
    let mut y = rect.top() + 1.0;
    let mut idx = 0u32;
    while y < rect.bottom() - 1.0 {
        let col = if idx % 3 == 0 {
            Color32::from_rgba_unmultiplied(255, 246, 228, 7)
        } else {
            Color32::from_rgba_unmultiplied(0, 0, 0, 12)
        };
        painter.line_segment([egui::pos2(rect.left() + 1.0, y), egui::pos2(rect.right() - 1.0, y)], egui::Stroke::new(1.0, col));
        y += 2.0;
        idx += 1;
    }
    // Top specular metal bevel and bottom shadow
    painter.line_segment(
        [rect.left_top() + egui::vec2(1.0, 1.0), rect.right_top() + egui::vec2(-1.0, 1.0)],
        egui::Stroke::new(1.0, Color32::from_rgba_unmultiplied(255, 245, 224, 34)),
    );
    painter.line_segment(
        [rect.left_bottom() + egui::vec2(1.0, -1.0), rect.right_bottom() + egui::vec2(-1.0, -1.0)],
        egui::Stroke::new(1.0, Color32::from_rgba_unmultiplied(0, 0, 0, 120)),
    );
}

/// Paints a small vintage slotted brushed-steel faceplate screw.
fn paint_screw(painter: &egui::Painter, center: egui::Pos2, r: f32) {
    painter.circle_filled(center, r + 0.8, Color32::from_rgba_unmultiplied(0, 0, 0, 150));
    painter.circle_filled(center, r, Color32::from_rgb(112, 106, 96));
    painter.circle_stroke(center, r, egui::Stroke::new(0.7, Color32::from_rgb(168, 158, 140)));
    let d = r * 0.65;
    painter.line_segment(
        [center + egui::vec2(-d, -d * 0.6), center + egui::vec2(d, d * 0.6)],
        egui::Stroke::new(1.0, Color32::from_rgb(28, 26, 24)),
    );
}

/// Draws the retro ViCTR brushed-metal nameplate logo with miniature glowing CRT emblem.
fn draw_victr_logo(ui: &mut egui::Ui) {
    let badge_size = egui::vec2(182.0, 26.0);
    let (rect, resp) = ui.allocate_exact_size(badge_size, egui::Sense::hover());
    let p = ui.painter();

    // Recessed brushed-aluminum badge plate
    p.rect_filled(rect, 3.0, Color32::from_rgb(26, 25, 23));
    p.rect_filled(rect.shrink(1.0), 2.0, Color32::from_rgb(34, 32, 29));
    p.rect_stroke(rect, 3.0, egui::Stroke::new(1.1, Color32::from_rgb(168, 152, 122)), egui::StrokeKind::Inside);
    // Tiny mounting rivets
    paint_screw(p, egui::pos2(rect.left() + 5.5, rect.center().y), 1.6);
    paint_screw(p, egui::pos2(rect.right() - 5.5, rect.center().y), 1.6);

    // Miniature glowing retro CRT emblem
    let tv_rect = egui::Rect::from_min_size(egui::pos2(rect.left() + 12.0, rect.top() + 4.5), egui::vec2(22.0, 17.0));
    p.rect_filled(tv_rect, 3.5, Color32::from_rgb(78, 46, 24));
    p.rect_stroke(tv_rect, 3.5, egui::Stroke::new(0.9, Color32::from_rgb(190, 168, 128)), egui::StrokeKind::Inside);
    let glass = tv_rect.shrink(2.3);
    p.rect_filled(glass, 2.5, Color32::from_rgb(14, 24, 22));
    // Glowing RGB / amber scanlines inside the mini tube
    for (idx, col) in [
        Color32::from_rgb(245, 110, 75),
        Color32::from_rgb(110, 235, 150),
        Color32::from_rgb(85, 195, 255),
        Color32::from_rgb(248, 186, 86),
    ]
    .iter()
    .enumerate()
    {
        let y = glass.top() + 2.2 + idx as f32 * 2.7;
        p.line_segment(
            [egui::pos2(glass.left() + 1.5, y), egui::pos2(glass.right() - 1.5, y)],
            egui::Stroke::new(1.3, *col),
        );
    }
    // Tiny glass glint
    p.circle_filled(glass.left_top() + egui::vec2(3.0, 2.5), 1.1, Color32::from_rgba_unmultiplied(255, 255, 255, 160));

    // "ViCTR" metallic + amber-gold retro logotype
    let vi_pos = egui::pos2(tv_rect.right() + 6.0, rect.center().y - 0.5);
    let vi_galley = p.layout_no_wrap("Vi".into(), egui::FontId::proportional(15.5), Color32::from_rgb(232, 224, 206));
    let vi_w = vi_galley.size().x;
    p.galley(egui::pos2(vi_pos.x, rect.center().y - vi_galley.size().y * 0.5 - 1.0), vi_galley, Color32::WHITE);

    let ctr_galley = p.layout_no_wrap("CTR".into(), egui::FontId::proportional(15.5), Color32::from_rgb(246, 178, 74));
    let ctr_w = ctr_galley.size().x;
    p.galley(egui::pos2(vi_pos.x + vi_w - 0.5, rect.center().y - ctr_galley.size().y * 0.5 - 1.0), ctr_galley, Color32::WHITE);

    // Warm copper accent rule beneath logotype
    let rule_y = rect.bottom() - 4.0;
    p.line_segment(
        [egui::pos2(vi_pos.x, rule_y), egui::pos2(vi_pos.x + vi_w + ctr_w, rule_y)],
        egui::Stroke::new(1.2, Color32::from_rgb(212, 128, 48)),
    );

    // Vertical divider + stacked "VIRTUAL CRT / TV RECEIVER" engraved caption
    let div_x = vi_pos.x + vi_w + ctr_w + 6.0;
    p.line_segment(
        [egui::pos2(div_x, rect.top() + 5.0), egui::pos2(div_x, rect.bottom() - 5.0)],
        egui::Stroke::new(0.9, Color32::from_rgb(104, 96, 84)),
    );
    p.text(
        egui::pos2(div_x + 5.0, rect.top() + 8.5),
        egui::Align2::LEFT_CENTER,
        "VIRTUAL CRT",
        egui::FontId::monospace(7.5),
        Color32::from_rgb(188, 176, 152),
    );
    p.text(
        egui::pos2(div_x + 5.0, rect.bottom() - 8.5),
        egui::Align2::LEFT_CENTER,
        "TV RECEIVER",
        egui::FontId::monospace(7.5),
        Color32::from_rgb(154, 144, 126),
    );

    resp.on_hover_text("ViCTR — Virtual CRT Television Receiver");
}

fn sl(ui: &mut egui::Ui, v: &mut f32, r: std::ops::RangeInclusive<f32>, label: &str, tip: &str) -> egui::Response {
    let resp = ui.add(Slider::new(v, r).text(label));
    if tip.is_empty() { resp } else { resp.on_hover_text(tip) }
}

fn lamp(ui: &mut egui::Ui, on: bool, label: &str, tip: &str) {
    let (rect, _) = ui.allocate_exact_size(egui::vec2(12.0, 12.0), egui::Sense::hover());
    let c = rect.center();
    let p = ui.painter();
    // Recessed chrome LED bezel
    p.circle_filled(c, 5.5, Color32::from_rgb(96, 90, 80));
    p.circle_filled(c, 4.4, Color32::from_rgb(18, 16, 15));
    let (core, halo) = if on {
        (Color32::from_rgb(76, 236, 124), Color32::from_rgba_unmultiplied(76, 236, 124, 58))
    } else {
        (Color32::from_rgb(228, 66, 48), Color32::from_rgba_unmultiplied(228, 66, 48, 48))
    };
    p.circle_filled(c, 6.2, halo);
    p.circle_filled(c, 3.6, core);
    // Tiny jeweled glass highlight
    p.circle_filled(c + egui::vec2(-1.1, -1.1), 1.0, Color32::from_rgba_unmultiplied(255, 255, 255, 195));
    ui.label(RichText::new(label).small().color(Color32::from_rgb(218, 208, 188))).on_hover_text(tip);
}

impl CrtApp {
    pub fn run_ui(&mut self, ui: &mut egui::Ui) -> FrameOutput {
        let ctx = ui.ctx().clone();
        let now = std::time::Instant::now();
        let real_dt = (now - self.last_instant).as_secs_f32();
        self.last_instant = now;
        if real_dt > 0.0 {
            self.fps = self.fps * 0.95 + (1.0 / real_dt).min(1000.0) * 0.05;
        }

        // dropped pictures, ROM cartridges, or videos
        let dropped: Vec<_> = ctx.input(|i| i.raw.dropped_files.clone());
        for f in dropped {
            let path = f.path().to_path_buf();
            let ext = path.extension().map(|e| e.to_string_lossy().to_lowercase()).unwrap_or_default();
            if retro::is_rom_file(&path) && !path.as_os_str().is_empty() {
                self.launch_rom_on_console_channel(&path.to_string_lossy(), None);
                continue;
            }
            if !["png", "jpg", "jpeg", "bmp", "gif"].contains(&ext.as_str()) && !path.as_os_str().is_empty() {
                // anything that isn't a picture or ROM goes to mpv
                self.start_video(path.to_string_lossy().to_string());
                continue;
            }
            if let Ok(b) = f.bytes() {
                let name = f.path().file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
                self.load_image_bytes(name, &b);
            }
        }

        // videos sent from the person's mpv (Ctrl+T shortcut / second launch)
        let mut focus_cmd = false;
        let requests: Vec<(String, f64)> = self.inbox.as_ref().map(|i| std::mem::take(&mut *i.lock().unwrap())).unwrap_or_default();
        for (src, start) in requests {
            self.start_video_at(src, start);
            focus_cmd = true;
        }

        // Poll background media providers & remote digit-entry commit timer
        let prev_jf_err = self.jellyfin.last_error.clone();
        let prev_px_err = self.plex.last_error.clone();
        let jf_changed = self.jellyfin.poll();
        let px_changed = self.plex.poll();
        let dir_changed = self.folders.poll();
        let m3u_changed = self.m3u.poll();
        let cus_changed = self.custom.poll();
        let (cf_changed, cf_hot) = self.channels_file.poll();
        let any_changed = jf_changed || px_changed || dir_changed || m3u_changed || cus_changed || cf_changed;
        if any_changed {
            self.rebuild_lineup();
        }
        if cf_hot {
            self.osd.show_action("CHANNELS.JSON RELOADED");
            if let Some((ct, ov)) = self.active_watch.as_ref().and_then(|w| {
                self.channel_by_number(w.channel_number)
                    .map(|ch| (ch.effective_type().to_string(), ch.profile_override.clone()))
            }) {
                self.apply_channel_profile(&ct, ov.as_deref());
            }
        }
        if !self.jellyfin.last_error.is_empty() && self.jellyfin.last_error != prev_jf_err {
            self.osd.show_action("JELLYFIN SYNC ERR");
        }
        if !self.plex.last_error.is_empty() && self.plex.last_error != prev_px_err {
            self.osd.show_action("PLEX SYNC ERR");
        }
        if any_changed && self.notify_guide_synced && !self.any_source_busy() {
            self.notify_guide_synced = false;
            if self.has_channels() {
                self.osd.show_action("GUIDE SYNCED");
            }
        }
        // Step startup splash screen (RF static -> blue screen -> configured startup destination)
        if self.splash.is_active() {
            let skip_key = !ctx.egui_wants_keyboard_input()
                && !self.save_preset_open
                && ctx.input_mut(|i| {
                    i.consume_key(egui::Modifiers::NONE, egui::Key::Enter)
                        || i.consume_key(egui::Modifiers::NONE, egui::Key::Space)
                });
            let ev = if skip_key {
                self.splash.skip();
                SplashEvent::Finished
            } else {
                self.splash.step(real_dt, self.splash_waiting_for_sources())
            };
            match ev {
                SplashEvent::None => {}
                SplashEvent::SnappedToBlue => {
                    self.sim.snap_input();
                    self.reset_trails = true;
                }
                SplashEvent::Finished => {
                    self.reset_trails = true;
                    if self.startup_dest_pending || self.open_guide_on_connect {
                        self.apply_startup_destination();
                    } else {
                        self.sim.switch_channel();
                    }
                }
            }
        } else if self.startup_dest_pending || self.open_guide_on_connect {
            self.apply_startup_destination();
        }
        if let Some(pending) = self.pending_tune_channel {
            if let Some(ch) = self.channel_by_number(pending) {
                let ready = match &ch.kind {
                    ChannelKind::Series { loaded_episodes: true, .. } => true,
                    ChannelKind::Block { items, pending_loads, .. } => !items.is_empty() || !pending_loads,
                    _ => false,
                };
                if ready {
                    self.pending_tune_channel = None;
                    self.tune_channel(pending);
                }
            }
        }
        if let Some(ch_num) = self.osd.poll_digit_commit() {
            if self.has_channels() {
                self.tune_channel(ch_num);
            }
        }

        // fullscreen: F11 toggles, Esc leaves; Ctrl+1 snaps screen size; Ctrl+Q cleanly saves progress & quits
        let f11 = ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::F11));
        let esc = (self.fullscreen || self.save_preset_open || self.settings_open || self.guide.active)
            && ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Escape));
        let ctrl_1 = !ctx.egui_wants_keyboard_input()
            && ctx.input_mut(|i| i.consume_key(egui::Modifiers::COMMAND, egui::Key::Num1));
        let ctrl_q = ctx.input_mut(|i| i.consume_key(egui::Modifiers::COMMAND, egui::Key::Q));
        if ctrl_q {
            self.begin_quit();
        }
        if let Some(t0) = self.quitting_since {
            // Exit cleanly once the CRT raster dot-collapse & phosphor fade completes (~1.25 s).
            if t0.elapsed() >= std::time::Duration::from_millis(1250) {
                self.pending_close = true;
            }
        }
        if f11 {
            self.set_fullscreen(!self.fullscreen);
        } else if esc {
            if self.save_preset_open {
                self.save_preset_open = false;
            } else if self.settings_open {
                self.settings_open = false;
            } else if self.guide.active {
                if self.guide.picker_channel().is_some() {
                    self.guide.close_picker();
                } else {
                    self.toggle_guide();
                }
            } else {
                self.set_fullscreen(false);
            }
        }
        if ctrl_1 {
            self.pixel_perfect_req = true;
        }

        let retro_playing = self.retro_player.as_ref().is_some_and(|rp| rp.shared.lock().unwrap().running);
        if let Some(rp) = &self.retro_player {
            if !self.guide.active && !ctx.egui_wants_keyboard_input() && !self.save_preset_open {
                let mask = ctx.input(|i| {
                    let mut m: u16 = 0;
                    if i.key_down(egui::Key::ArrowUp) { m |= retro::BTN_UP; }
                    if i.key_down(egui::Key::ArrowDown) { m |= retro::BTN_DOWN; }
                    if i.key_down(egui::Key::ArrowLeft) { m |= retro::BTN_LEFT; }
                    if i.key_down(egui::Key::ArrowRight) { m |= retro::BTN_RIGHT; }
                    if i.key_down(egui::Key::Z) { m |= retro::BTN_B; }
                    if i.key_down(egui::Key::X) { m |= retro::BTN_A; }
                    if i.key_down(egui::Key::A) { m |= retro::BTN_Y; }
                    if i.key_down(egui::Key::S) { m |= retro::BTN_X; }
                    if i.key_down(egui::Key::Q) { m |= retro::BTN_L; }
                    if i.key_down(egui::Key::W) { m |= retro::BTN_R; }
                    if i.key_down(egui::Key::Enter) { m |= retro::BTN_START; }
                    if i.key_down(egui::Key::Backspace) || i.key_down(egui::Key::Tab) { m |= retro::BTN_SELECT; }
                    m
                });
                rp.key_mask.store(mask, std::sync::atomic::Ordering::Relaxed);
                if ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::F1)) {
                    rp.reset();
                    self.osd.show_action("CONSOLE RESET");
                }
                if ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::F2)) {
                    rp.save_state();
                    self.osd.show_action("STATE SAVED");
                }
                if ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::F4)) {
                    rp.load_state();
                    self.osd.show_action("STATE LOADED");
                }
            } else {
                rp.key_mask.store(0, std::sync::atomic::Ordering::Relaxed);
            }
        }

        // TV remote & 90s Cable Guide keyboard controls (when not typing in a text field)
        let mut remote_consumed = false;
        if !ctx.egui_wants_keyboard_input() && !self.save_preset_open {
            // Volume (- / +) and Mute (M) always work like a real TV remote
            if ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Minus)) {
                self.adjust_volume(-0.05);
                remote_consumed = true;
            }
            if ctx.input_mut(|i| {
                i.consume_key(egui::Modifiers::NONE, egui::Key::Equals)
                    || i.consume_key(egui::Modifiers::NONE, egui::Key::Plus)
                    || i.consume_key(egui::Modifiers::SHIFT, egui::Key::Equals)
            }) {
                self.adjust_volume(0.05);
                remote_consumed = true;
            }
            if ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::M)) {
                self.toggle_mute();
                remote_consumed = true;
            }
            if self.s.chaos && ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::K)) {
                self.sim.whack();
                remote_consumed = true;
            }

            if self.has_channels() {
                if ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::G)) {
                    self.toggle_guide();
                    remote_consumed = true;
                }
                if ctx.input_mut(|i| {
                    i.consume_key(egui::Modifiers::NONE, egui::Key::PageUp)
                        || i.consume_key(egui::Modifiers::NONE, egui::Key::CloseBracket)
                }) {
                    self.surf_channel(1);
                    remote_consumed = true;
                }
                if ctx.input_mut(|i| {
                    i.consume_key(egui::Modifiers::NONE, egui::Key::PageDown)
                        || i.consume_key(egui::Modifiers::NONE, egui::Key::OpenBracket)
                }) {
                    self.surf_channel(-1);
                    remote_consumed = true;
                }
                if ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::I)) {
                    self.show_info_osd();
                    remote_consumed = true;
                }
                if ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::E)) {
                    remote_consumed = true;
                    if self.guide.active {
                        if self.guide.picker_channel().is_some() {
                            self.guide.close_picker();
                        } else {
                            let sel_ch = self
                                .visible_channels()
                                .get(self.guide.selected_idx)
                                .map(|c| c.number);
                            if let Some(n) = sel_ch.filter(|&n| n > 1) {
                                self.open_episode_picker(n);
                            }
                        }
                    } else if let Some(cur) = self.current_channel.filter(|&n| n > 1) {
                        self.open_episode_picker(cur);
                    }
                }
                if (!retro_playing || self.guide.active) && ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::W)) {
                    remote_consumed = true;
                    if let (true, Some((ch_num, idx))) = (self.guide.active, self.guide.picker_selected()) {
                        if let Some((iid, played)) = self
                            .channel_by_number(ch_num)
                            .and_then(|c| c.items().get(idx))
                            .map(|m| (m.id.clone(), !m.played))
                        {
                            self.set_channel_item_played(ch_num, &iid, played);
                            self.osd.show_action(if played { "MARKED WATCHED" } else { "MARKED UNWATCHED" });
                        }
                    } else if self.active_watch.is_some() {
                        self.toggle_current_watched();
                    }
                }
                if !self.guide.active && ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::N)) {
                    self.step_episode(1);
                    remote_consumed = true;
                }
                if !self.guide.active && ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::P)) {
                    self.step_episode(-1);
                    remote_consumed = true;
                }
                if ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::F5)) {
                    self.refresh_all_sources();
                    remote_consumed = true;
                }
                if ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::R)) {
                    if self.guide.active && self.guide.picker_selected().is_none() {
                        self.refresh_all_sources();
                        remote_consumed = true;
                    } else if let (true, Some((ch_num, idx))) = (self.guide.active, self.guide.picker_selected()) {
                        if let Some(item) = self.channel_by_number(ch_num).and_then(|c| c.items().get(idx)).cloned() {
                            self.tune_specific(ch_num, item, true);
                            self.osd.show_action("RESTART 0:00");
                            remote_consumed = true;
                        }
                    } else if let Some(p) = &self.player {
                        p.seek_abs(0.0);
                        self.osd.show_action("RESTART 0:00");
                        remote_consumed = true;
                    }
                }
                // Direct 0-9 channel number entry
                let digit_keys = [
                    (egui::Key::Num0, '0'),
                    (egui::Key::Num1, '1'),
                    (egui::Key::Num2, '2'),
                    (egui::Key::Num3, '3'),
                    (egui::Key::Num4, '4'),
                    (egui::Key::Num5, '5'),
                    (egui::Key::Num6, '6'),
                    (egui::Key::Num7, '7'),
                    (egui::Key::Num8, '8'),
                    (egui::Key::Num9, '9'),
                ];
                for (k, ch) in digit_keys {
                    if ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, k)) {
                        remote_consumed = true;
                        if let Some(ch_num) = self.osd.push_digit(ch) {
                            self.tune_channel(ch_num);
                        }
                    }
                }
                if self.guide.active {
                    if let Some((pch_num, sel_idx)) = self.guide.picker_selected() {
                        let items: Vec<MediaItem> = self
                            .channel_by_number(pch_num)
                            .map(|c| c.items().to_vec())
                            .unwrap_or_default();
                        if ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Backspace)) {
                            self.guide.close_picker();
                            remote_consumed = true;
                        }
                        if ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::ArrowUp)) {
                            self.guide.picker_move(-1, items.len());
                            remote_consumed = true;
                        }
                        if ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::ArrowDown)) {
                            self.guide.picker_move(1, items.len());
                            remote_consumed = true;
                        }
                        if ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::ArrowLeft)) {
                            self.guide.picker_jump_season(-1, &items);
                            remote_consumed = true;
                        }
                        if ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::ArrowRight)) {
                            self.guide.picker_jump_season(1, &items);
                            remote_consumed = true;
                        }
                        if ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Enter)) {
                            remote_consumed = true;
                            if let Some(ch_num) = self.osd.commit_digits() {
                                self.tune_channel(ch_num);
                            } else if let Some(item) = items.get(sel_idx).cloned() {
                                self.tune_specific(pch_num, item, false);
                            }
                        }
                    } else {
                        let vis_len = self.visible_channels().len();
                        if ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::ArrowUp)) {
                            self.guide.move_selection(-1, vis_len);
                            remote_consumed = true;
                        }
                        if ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::ArrowDown)) {
                            self.guide.move_selection(1, vis_len);
                            remote_consumed = true;
                        }
                        if ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::ArrowLeft)) {
                            self.guide.shift_time(-1);
                            remote_consumed = true;
                        }
                        if ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::ArrowRight)) {
                            self.guide.shift_time(1);
                            remote_consumed = true;
                        }
                        if ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Enter)) {
                            remote_consumed = true;
                            if let Some(ch_num) = self.osd.commit_digits() {
                                self.tune_channel(ch_num);
                            } else {
                                let target_ch = self
                                    .visible_channels()
                                    .get(self.guide.selected_idx)
                                    .map(|c| c.number);
                                if let Some(n) = target_ch {
                                    if n == 1 {
                                        self.toggle_guide();
                                    } else {
                                        self.tune_channel(n);
                                    }
                                }
                            }
                        }
                    }
                } else if self.osd.has_pending_digits()
                    && ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Enter))
                {
                    remote_consumed = true;
                    if let Some(ch_num) = self.osd.commit_digits() {
                        self.tune_channel(ch_num);
                    }
                }
            }
        }
        // pointer activity: shows the hint, hides the idle pointer in fullscreen
        if ctx.input(|i| i.pointer.delta() != egui::Vec2::ZERO || i.pointer.any_down()) {
            self.last_pointer_move = std::time::Instant::now();
        }

        // Smoothly demagnetise the shadow mask as the degauss coil sweeps through it
        if let Some(start_mag) = self.degauss_mag_start {
            let progress = (self.sim.degauss_t / 0.85).clamp(0.0, 1.0);
            self.s.magnetization = start_mag * (1.0 - progress).powi(2);
            if progress >= 1.0 {
                self.s.magnetization = 0.0;
                self.degauss_mag_start = None;
            }
        }
        if let Some(t) = self.degauss_pinned_t {
            self.sim.degauss_t = t;
        }

        // the person's mpv key bindings work while the pointer is over the TV (paused while browsing Guide)
        // Plus automatic VHS RW/FF helical-scan effects on seek keys and Shift+Left/Right (or , / .) shuttle!
        if self.player.is_some()
            && self.s.pattern == Pattern::Video
            && !self.guide.active
            && !remote_consumed
            && !ctx.egui_wants_keyboard_input()
            && (self.tv_hovered || (self.fullscreen && !self.sidebar_open))
        {
            let keys: Vec<String> = ctx.input(|i| i.events.iter().filter_map(mpv_key_name).collect());
            for k in keys {
                match k.as_str() {
                    "Shift+LEFT" | "," => {
                        self.cycle_vcr_shuttle(-1.0);
                    }
                    "Shift+RIGHT" | "." => {
                        self.cycle_vcr_shuttle(1.0);
                    }
                    "SPACE" if self.vcr_shuttle_speed != 0.0 => {
                        self.set_vcr_shuttle(0.0);
                    }
                    "SPACE" => {
                        let will_pause = self.player.as_ref().is_some_and(|p| !p.shared.lock().unwrap().paused);
                        if let Some(p) = &self.player {
                            p.key(&k);
                        }
                        let lbl = if will_pause { "PAUSE ||" } else { "PLAY >" };
                        self.osd.show_action(lbl);
                        self.show_video_progress_osd(lbl, None);
                    }
                    "LEFT" => {
                        self.stop_vcr_shuttle_silent();
                        let cur_pos = self.player.as_ref().map(|p| p.shared.lock().unwrap().pos).unwrap_or(0.0);
                        if let Some(p) = &self.player {
                            p.key(&k);
                        }
                        self.sim.trigger_seek_shuttle(-5.0);
                        self.osd.show_action("REW <<");
                        self.show_video_progress_osd("REW <<", Some((cur_pos - 5.0).max(0.0)));
                    }
                    "RIGHT" => {
                        self.stop_vcr_shuttle_silent();
                        let cur_pos = self.player.as_ref().map(|p| p.shared.lock().unwrap().pos).unwrap_or(0.0);
                        if let Some(p) = &self.player {
                            p.key(&k);
                        }
                        self.sim.trigger_seek_shuttle(5.0);
                        self.osd.show_action("FF >>");
                        self.show_video_progress_osd("FF >>", Some(cur_pos + 5.0));
                    }
                    "DOWN" => {
                        self.stop_vcr_shuttle_silent();
                        let cur_pos = self.player.as_ref().map(|p| p.shared.lock().unwrap().pos).unwrap_or(0.0);
                        if let Some(p) = &self.player {
                            p.key(&k);
                        }
                        self.sim.trigger_seek_shuttle(-60.0);
                        self.osd.show_action("REW <<<");
                        self.show_video_progress_osd("REW <<<", Some((cur_pos - 60.0).max(0.0)));
                    }
                    "UP" => {
                        self.stop_vcr_shuttle_silent();
                        let cur_pos = self.player.as_ref().map(|p| p.shared.lock().unwrap().pos).unwrap_or(0.0);
                        if let Some(p) = &self.player {
                            p.key(&k);
                        }
                        self.sim.trigger_seek_shuttle(60.0);
                        self.osd.show_action("FF >>>");
                        self.show_video_progress_osd("FF >>>", Some(cur_pos + 60.0));
                    }
                    _ => {
                        if let Some(p) = &self.player {
                            p.key(&k);
                        }
                    }
                }
            }
        }

        // Drive continuous VHS RW (and 16x/32x FF) stepped picture-search & keep OSD progress bar live!
        if self.vcr_shuttle_speed != 0.0 {
            let mut cancel_shuttle = false;
            let spd = self.vcr_shuttle_speed;
            let lbl = Self::vcr_shuttle_label(spd);
            let mut live_osd: Option<(f64, f64)> = None;
            if let Some(p) = &self.player {
                let (pos, dur, ended, running, seeking) = {
                    let sh = p.shared.lock().unwrap();
                    (sh.pos, sh.duration, sh.ended, sh.running, sh.seeking)
                };
                let step_ready = (!seeking && self.vcr_rw_last_step.elapsed() >= std::time::Duration::from_millis(125))
                    || self.vcr_rw_last_step.elapsed() >= std::time::Duration::from_millis(550);
                if !running || ended {
                    cancel_shuttle = true;
                } else if spd < -0.1 {
                    if pos <= 0.6 {
                        p.seek_abs(0.0);
                        cancel_shuttle = true;
                    } else if step_ready {
                        let dt_s = self.vcr_rw_last_step.elapsed().as_secs_f64().clamp(0.10, 0.35);
                        self.vcr_rw_last_step = std::time::Instant::now();
                        let step = spd as f64 * dt_s;
                        p.seek_fast(step);
                        self.osd.show_action(lbl);
                    }
                    live_osd = Some((pos, dur));
                } else if spd > 8.5 {
                    if dur > 1.0 && pos >= dur - 1.5 {
                        cancel_shuttle = true;
                    } else if step_ready {
                        let dt_s = self.vcr_rw_last_step.elapsed().as_secs_f64().clamp(0.10, 0.35);
                        self.vcr_rw_last_step = std::time::Instant::now();
                        let step = spd as f64 * dt_s;
                        p.seek_fast(step);
                        self.osd.show_action(lbl);
                    }
                    live_osd = Some((pos, dur));
                } else if spd > 0.1 {
                    if self.vcr_rw_last_step.elapsed() >= std::time::Duration::from_millis(550) {
                        self.vcr_rw_last_step = std::time::Instant::now();
                        self.osd.show_action(lbl);
                    }
                    live_osd = Some((pos, dur));
                }
            } else {
                cancel_shuttle = true;
            }
            if let Some((pos, dur)) = live_osd {
                self.osd.show_progress(lbl, pos, dur);
            }
            if cancel_shuttle {
                self.set_vcr_shuttle(0.0);
            }
        }

        // newest frame from mpv + multi-provider watch-progress sync & episode auto-advance
        let mut auto_next_target = None;
        if let Some(p) = &self.player {
            let sh = p.shared.lock().unwrap();
            let (pos, dur, paused, ended) = (sh.pos, sh.duration, sh.paused, sh.ended);
            if sh.frame_no != self.video_frame_no {
                self.video_frame_no = sh.frame_no;
                if let Some(f) = &sh.frame {
                    self.image_counter += 1;
                    self.video_image = Some((self.image_counter, f.clone()));
                }
            }
            drop(sh);
            let mut rebuild_after_tick = false;
            if let Some(w) = self.active_watch.as_mut() {
                if !paused {
                    w.dwell_secs += real_dt.clamp(0.0, 0.25);
                }
                w.last_pos = pos;
                w.last_paused = paused;
                if dur > 10.0 {
                    w.duration_secs = dur;
                }
                if w.provider_id == "jellyfin" {
                    if self.jellyfin.tick_watch(real_dt, pos, dur, paused) {
                        w.marked_played = true;
                        rebuild_after_tick = true;
                    }
                } else if w.provider_id == "plex" {
                    if self.plex.tick_watch(real_dt, pos, dur, paused) {
                        w.marked_played = true;
                        rebuild_after_tick = true;
                    }
                } else if (w.provider_id == "folders" || w.provider_id == "custom") && w.dwell_secs >= 20.0 {
                    let hit_90 = !w.marked_played && w.duration_secs > 30.0 && pos >= w.duration_secs * 0.90;
                    if hit_90 || w.dwell_secs - w.last_progress_secs >= 15.0 {
                        w.last_progress_secs = w.dwell_secs;
                        let iid = w.item_id.clone();
                        let d = w.duration_secs;
                        let prov = w.provider_id.clone();
                        if hit_90 {
                            w.marked_played = true;
                        }
                        if prov == "folders" {
                            self.folders.save_resume(&iid, pos, d);
                        } else {
                            self.custom.save_resume(&iid, pos, d);
                        }
                        if hit_90 {
                            rebuild_after_tick = true;
                        }
                    }
                }
            }
            if rebuild_after_tick {
                self.rebuild_lineup();
            }
            if ended {
                if let Some(w) = &self.active_watch {
                    if self.last_ended_item.as_deref() != Some(w.item_id.as_str()) {
                        let ch_num = w.channel_number;
                        let item_id = w.item_id.clone();
                        self.last_ended_item = Some(item_id.clone());
                        if let Some(ch) = self.channel_by_number(ch_num) {
                            auto_next_target = ch.next_after(&item_id);
                        }
                    }
                }
            }
        } else if let Some(wp) = &self.web_player {
            let sh = wp.shared.lock().unwrap();
            if sh.frame_no != self.video_frame_no {
                self.video_frame_no = sh.frame_no;
                if let Some(f) = &sh.frame {
                    self.image_counter += 1;
                    self.video_image = Some((self.image_counter, f.clone()));
                }
            }
        } else if let Some(rp) = &self.retro_player {
            let sh = rp.shared.lock().unwrap();
            if sh.frame_no != self.video_frame_no {
                self.video_frame_no = sh.frame_no;
                if let Some(f) = &sh.frame {
                    self.image_counter += 1;
                    self.video_image = Some((self.image_counter, f.clone()));
                }
            }
        } else if let Some(cp) = &self.capture_player {
            let sh = cp.shared.lock().unwrap();
            if sh.frame_no != self.video_frame_no {
                self.video_frame_no = sh.frame_no;
                if let Some(f) = &sh.frame {
                    self.image_counter += 1;
                    self.video_image = Some((self.image_counter, f.clone()));
                }
            }
        }
        if let Some(next) = auto_next_target {
            self.apply_tune_target(next);
            self.osd.show_action("NEXT UP");
        }
        self.update_audio();

        let mut go_fullscreen = false;
        if !self.fullscreen {
            egui::Panel::top("top")
                .frame(egui::Frame::NONE.inner_margin(egui::Margin::symmetric(0, 0)))
                .show(ui, |ui| {
                    let bar_rect = ui.max_rect();
                    // Outer faux walnut woodgrain fascia with inset brushed-metal receiver control deck
                    paint_wood_panel(ui.painter(), bar_rect, true);
                    let deck_rect = bar_rect.shrink2(egui::vec2(10.0, 4.0));
                    paint_brushed_metal(ui.painter(), deck_rect);
                    ui.painter().rect_stroke(
                        deck_rect,
                        2.0,
                        egui::Stroke::new(1.0, Color32::from_rgb(146, 134, 112)),
                        egui::StrokeKind::Inside,
                    );
                    paint_screw(ui.painter(), egui::pos2(deck_rect.left() + 5.5, deck_rect.center().y), 2.0);
                    paint_screw(ui.painter(), egui::pos2(deck_rect.right() - 5.5, deck_rect.center().y), 2.0);

                    egui::Frame::NONE
                        .inner_margin(egui::Margin { left: 20, right: 20, top: 8, bottom: 8 })
                        .show(ui, |ui| {
                            ui.horizontal(|ui| {
                                draw_victr_logo(ui);
                                ui.separator();
                                let pwr_label = if self.sim.power_on {
                                    RichText::new("Power").color(Color32::WHITE).strong()
                                } else {
                                    RichText::new("Power off").color(Color32::from_rgb(210, 140, 120))
                                };
                                if ui
                                    .add(egui::Button::new(pwr_label).selected(self.sim.power_on))
                                    .on_hover_text("Toggle main TV receiver power (with CRT warm-up & collapse)")
                                    .clicked()
                                {
                                    let on = !self.sim.power_on;
                                    self.sim.set_power(on);
                                }
                                if ui
                                    .button("Degauss")
                                    .on_hover_text("Fire the perimeter degaussing coil: clears magnetisation with the classic AC magnetic warp and colour rings.")
                                    .clicked()
                                {
                                    self.sim.degauss();
                                    if self.s.magnetization > 0.0 {
                                        self.degauss_mag_start = Some(self.s.magnetization);
                                    }
                                }
                                if self.s.chaos {
                                    let drifting = !self.sim.chaos_active_faults().is_empty();
                                    let lbl = if drifting {
                                        RichText::new("Whack!").color(Color32::from_rgb(255, 212, 92)).strong()
                                    } else {
                                        RichText::new("Whack!")
                                    };
                                    if ui
                                        .add(egui::Button::new(lbl).selected(drifting))
                                        .on_hover_text("Percussive maintenance: slap the side of the CRT to knock drifting circuits back into gear (shortcut: K).")
                                        .clicked()
                                    {
                                        self.sim.whack();
                                    }
                                }
                                ui.separator();
                                ui.label(RichText::new("Preset").color(Color32::from_rgb(210, 198, 176)).small());
                                let mut sel = self.preset_sel;
                                let selected_label = self.current_preset_label();
                                egui::ComboBox::from_id_salt("preset")
                                    .selected_text(selected_label)
                                    .width(182.0)
                                    .show_ui(ui, |ui| {
                                        for (i, n) in settings::PRESETS.iter().enumerate() {
                                            ui.selectable_value(&mut sel, PresetSel::BuiltIn(i), *n);
                                        }
                                        if !self.user_presets.is_empty() {
                                            ui.separator();
                                            ui.label(RichText::new("Saved presets").weak().small());
                                            for (i, p) in self.user_presets.iter().enumerate() {
                                                ui.selectable_value(&mut sel, PresetSel::User(i), &p.name);
                                            }
                                        }
                                    });
                                if sel != self.preset_sel {
                                    self.apply_preset(sel);
                                }
                                if ui
                                    .button("Save…")
                                    .on_hover_text("Save the current CRT settings as a new preset.")
                                    .clicked()
                                {
                                    self.open_save_preset_dialog();
                                }
                                if ui.button("Reset view").clicked() {
                                    self.zoom = 1.0;
                                    self.pan = [0.0, 0.0];
                                    self.reset_trails = true;
                                }
                                if ui.button("Fullscreen").on_hover_text("Just the TV, no settings (F11). Press F11 or Esc to come back.").clicked() {
                                    go_fullscreen = true;
                                }
                                ui.separator();
                                lamp(ui, self.sim.h_locked, "H", "Line oscillator locked to the incoming sync.");
                                lamp(ui, self.sim.v_locked, "V", "Field oscillator locked (picture not rolling).");
                                lamp(ui, self.sim.color_on, "CLR", "Colour killer off: colour is being decoded.");

                                // Recessed vacuum-fluorescent telemetry readout window
                                egui::Frame::new()
                                    .fill(Color32::from_rgb(13, 17, 15))
                                    .stroke(egui::Stroke::new(1.0, Color32::from_rgb(98, 90, 78)))
                                    .corner_radius(3.0)
                                    .inner_margin(egui::Margin::symmetric(7, 2))
                                    .show(ui, |ui| {
                                        ui.horizontal(|ui| {
                                            ui.spacing_mut().item_spacing.x = 7.0;
                                            ui.label(
                                                RichText::new(format!("SNR {:>2.0}dB", self.sim.snr_db))
                                                    .small()
                                                    .monospace()
                                                    .color(Color32::from_rgb(112, 236, 184)),
                                            );
                                            ui.label(
                                                RichText::new(format!("{:>3.0}fps", self.fps))
                                                    .small()
                                                    .monospace()
                                                    .color(Color32::from_rgb(232, 186, 102)),
                                            );
                                            ui.label(
                                                RichText::new(format!("{:.1}×", self.zoom))
                                                    .small()
                                                    .monospace()
                                                    .color(Color32::from_rgb(206, 174, 118)),
                                            );
                                            if self.hdr_status.active_mode.is_hdr() {
                                                ui.label(
                                                    RichText::new("HDR")
                                                        .small()
                                                        .strong()
                                                        .monospace()
                                                        .color(Color32::from_rgb(255, 206, 92)),
                                                );
                                            }
                                        });
                                    });

                                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                    if self.has_channels() {
                                        let guide_lbl = if self.guide.active {
                                            RichText::new("Guide").color(Color32::from_rgb(248, 206, 72)).strong()
                                        } else if let Some(ch) = self.current_channel {
                                            RichText::new(format!("CH {:02}", ch)).color(Color32::from_rgb(112, 236, 152)).strong()
                                        } else {
                                            RichText::new("Guide")
                                        };
                                        if ui
                                            .add(egui::Button::new(guide_lbl).selected(self.guide.active))
                                            .on_hover_text("Toggle 90s Prevue Cable Guide (G)")
                                            .clicked()
                                        {
                                            self.toggle_guide();
                                        }
                                    }
                                    if self.hdr_status.supported {
                                        let hdr_on = self.hdr_status.active_mode.is_hdr();
                                        let hdr_lbl = if hdr_on {
                                            RichText::new("HDR").color(Color32::from_rgb(255, 214, 96)).strong()
                                        } else {
                                            RichText::new("HDR").color(Color32::from_rgb(176, 168, 152))
                                        };
                                        if ui
                                            .add(egui::Button::new(hdr_lbl).selected(hdr_on))
                                            .on_hover_text(format!("Toggle native HDR output ({})", self.hdr_status.summary()))
                                            .clicked()
                                        {
                                            self.s.hdr_mode = !self.s.hdr_mode;
                                        }
                                    }
                                    let btn_label = if self.sidebar_open {
                                        RichText::new("Quick >").color(Color32::WHITE).strong()
                                    } else {
                                        RichText::new("< Quick")
                                    };
                                    let btn = egui::Button::new(btn_label).selected(self.sidebar_open);
                                    if ui
                                        .add(btn)
                                        .on_hover_text("Toggle the slim Quick Menu drawer (or click the right-edge pull handle).")
                                        .clicked()
                                    {
                                        self.sidebar_open = !self.sidebar_open;
                                    }
                                    let set_lbl = if self.settings_open {
                                        RichText::new("Settings").color(Color32::from_rgb(255, 214, 112)).strong()
                                    } else {
                                        RichText::new("Settings")
                                    };
                                    if ui
                                        .add(egui::Button::new(set_lbl).selected(self.settings_open))
                                        .on_hover_text("Open the full tabbed Service & Settings window")
                                        .clicked()
                                    {
                                        self.settings_open = !self.settings_open;
                                    }
                                });
                            });
                        });
                });
        }
        if go_fullscreen {
            self.set_fullscreen(true);
        }
        if self.message == "pick" {
            self.message.clear();
            self.pick_image();
        }
        if self.message == "pickvideo" {
            self.message.clear();
            self.pick_video();
        }
        if self.message == "pickfolder" {
            self.message.clear();
            self.pick_folder();
        }
        if self.message == "pickm3u" {
            self.message.clear();
            self.pick_m3u_playlist();
        }
        if self.message == "pickrom" {
            self.message.clear();
            self.pick_rom_file();
        }
        if self.message == "pickromdir" {
            self.message.clear();
            self.pick_rom_folder();
        }
        if self.message == "exitfs" {
            self.message.clear();
            self.set_fullscreen(false);
        }

        // Advance drawer & right-edge handle animations BEFORE rendering the CRT,
        // so the TV smoothly glides left to stay completely unobstructed beside the
        // sliding panel with zero OpenGL FBO reallocations (in both windowed and fullscreen).
        let dt_ui = real_dt.clamp(0.0, 0.05);
        let prev_anim = self.sidebar_anim;
        let target = if self.sidebar_open { 1.0 } else { 0.0 };
        let step = dt_ui / 0.20;
        if self.sidebar_anim < target {
            self.sidebar_anim = (self.sidebar_anim + step).min(target);
        } else if self.sidebar_anim > target {
            self.sidebar_anim = (self.sidebar_anim - step).max(target);
        }
        if (self.sidebar_anim - prev_anim).abs() > 1e-5 {
            self.reset_trails = true;
        }

        let mut central_rect = egui::Rect::NOTHING;
        let mut frame_fi = FrameInput::default();
        let ptr_pos = ctx.input(|i| i.pointer.hover_pos());
        let eased = 1.0 - (1.0 - self.sidebar_anim).powi(3);

        egui::CentralPanel::default()
            .frame(egui::Frame::NONE)
            .show(ui, |ui| {
                let rect = ui.available_rect_before_wrap();
                central_rect = rect;
                let max_w = (rect.width() - 260.0).max(270.0).min(420.0);
                let drawer_w = self.sidebar_width.clamp(270.0, max_w);
                let drawer_vis_w = drawer_w * eased;
                let drawer_x = rect.right() - drawer_vis_w;
                let tab_w = 24.0;
                let over_handle = ptr_pos.is_some_and(|p| {
                    let tab_left = drawer_x - tab_w;
                    let cy = rect.center().y;
                    p.x >= tab_left - 2.0 && p.x <= drawer_x + 4.0 && (p.y - cy).abs() <= 50.0
                });
                let over_drawer = self.sidebar_anim > 0.01
                    && ptr_pos.is_some_and(|p| p.x >= drawer_x - 6.0 && p.y >= rect.top());
                let over_settings = self.settings_open
                    && self
                        .settings_rect
                        .is_some_and(|r| ptr_pos.is_some_and(|p| r.expand(6.0).contains(p)));
                let overlay_active = over_drawer || over_handle || self.sidebar_resizing || self.save_preset_open || over_settings;

                let resp = ui.allocate_rect(rect, egui::Sense::click_and_drag());
                self.tv_hovered = resp.hovered() && !overlay_active;

                // Visible TV region to the left of the sliding service panel:
                // the TV smoothly shifts & fits into this rect with zero FBO churn.
                let vis_rect = egui::Rect::from_min_max(
                    rect.min,
                    egui::pos2((rect.right() - drawer_vis_w).max(rect.left() + 120.0), rect.bottom()),
                );
                let ppp = ctx.pixels_per_point();
                self.last_vis_rect = vis_rect;
                self.last_full_h_px = (rect.height() * ppp).round();
                if self.pixel_perfect_req {
                    self.pixel_perfect_req = false;
                    self.apply_pixel_perfect_screen_size();
                }

                // zoom, pan & direct mouse interaction on the 90s Cable Guide
                let h = self.view_half(vis_rect);
                let shift_x = (drawer_vis_w / rect.height().max(1.0)) * h;
                let ctrl_down = ctx.input(|i| i.modifiers.ctrl || i.modifiers.command);
                if self.tv_hovered {
                    let scroll = ctx.input(|i| i.smooth_scroll_delta.y);
                    if scroll.abs() > 0.0 {
                        if self.guide.active && self.has_channels() && !ctrl_down {
                            let step = if scroll > 0.0 { -1 } else { 1 };
                            if let Some(pch_num) = self.guide.picker_channel() {
                                let len = self.channel_by_number(pch_num).map(|c| c.items().len()).unwrap_or(0);
                                self.guide.picker_scroll(step, len);
                            } else {
                                let vis_len = self.visible_channels().len();
                                self.guide.scroll_rows(step, vis_len);
                            }
                        } else if let Some(mp) = resp.hover_pos() {
                            let nx = (mp.x - rect.left()) / rect.width() * 2.0 - 1.0;
                            let ny = (mp.y - rect.top()) / rect.height() * 2.0 - 1.0;
                            let a = rect.width() / rect.height();
                            let px = nx * a * h + (self.pan[0] + shift_x);
                            let py = ny * h + self.pan[1];
                            self.zoom = (self.zoom * (scroll * 0.004).exp()).clamp(1.0, 60.0);
                            let h2 = self.view_half(vis_rect);
                            let shift_x2 = (drawer_vis_w / rect.height().max(1.0)) * h2;
                            self.pan = [px - nx * a * h2 - shift_x2, py - ny * h2];
                            self.reset_trails = true;
                        }
                    }
                }
                let mut guide_clicked_tune: Option<u16> = None;
                let mut guide_clicked_picker: Option<u16> = None;
                let mut picker_clicked_play: Option<(u16, MediaItem)> = None;
                if self.guide.active
                    && self.has_channels()
                    && !overlay_active
                    && (resp.clicked() || resp.double_clicked() || resp.secondary_clicked())
                {
                    if let Some(mp) = resp.interact_pointer_pos() {
                        let nx = (mp.x - rect.left()) / rect.width().max(1.0) * 2.0 - 1.0;
                        let ny = (mp.y - rect.top()) / rect.height().max(1.0) * 2.0 - 1.0;
                        let a = rect.width() / rect.height().max(1.0);
                        let fx = nx * a * h + (self.pan[0] + shift_x);
                        let fy = ny * h + self.pan[1];
                        let gu = (fx / (4.0 / 3.0)) * 0.5 + 0.5;
                        let gv = fy * 0.5 + 0.5;
                        if let Some((pch_num, cur_sel)) = self.guide.picker_selected() {
                            if resp.secondary_clicked() {
                                self.guide.close_picker();
                            } else if let Some(ch) = self.channel_by_number(pch_num).cloned() {
                                let items = ch.items();
                                if let Some(idx) = self.guide.picker_hit_test(gu, gv, items.len()) {
                                    let already = cur_sel == idx;
                                    if let GuideView::ItemPicker { selected_item, .. } = &mut self.guide.view {
                                        *selected_item = idx;
                                    }
                                    self.guide.clamp_picker(items.len());
                                    if resp.double_clicked() || already {
                                        if let Some(item) = items.get(idx).cloned() {
                                            picker_clicked_play = Some((pch_num, item));
                                        }
                                    }
                                }
                            }
                        } else {
                            let vis_nums: Vec<u16> = self.visible_channels().iter().map(|c| c.number).collect();
                            if let Some(row_idx) = self.guide.hit_test_row(gu, gv, vis_nums.len()) {
                                let already = self.guide.selected_idx == row_idx;
                                self.guide.selected_idx = row_idx;
                                self.guide.clamp_selection(vis_nums.len());
                                if resp.secondary_clicked() {
                                    guide_clicked_picker = vis_nums.get(row_idx).copied().filter(|&n| n > 1);
                                } else if resp.double_clicked() || already {
                                    guide_clicked_tune = vis_nums.get(row_idx).copied();
                                }
                            }
                        }
                    }
                }
                if let Some((pch_num, item)) = picker_clicked_play {
                    self.tune_specific(pch_num, item, false);
                } else if let Some(n) = guide_clicked_picker {
                    self.open_episode_picker(n);
                } else if let Some(n) = guide_clicked_tune {
                    if n == 1 {
                        self.toggle_guide();
                    } else {
                        self.tune_channel(n);
                    }
                }
                if resp.dragged() && !overlay_active {
                    let d = resp.drag_delta();
                    let k = 2.0 * h / rect.height();
                    self.pan[0] -= d.x * k;
                    self.pan[1] -= d.y * k;
                    self.reset_trails = true;
                }
                if resp.double_clicked() && !overlay_active && !self.guide.active {
                    self.zoom = 1.0;
                    self.pan = [0.0, 0.0];
                    self.reset_trails = true;
                }
                if self.zoom <= 1.0001 {
                    self.pan = [0.0, 0.0];
                }
                // keep the visible window on the tube
                let lim = (1.0 - 1.0 / self.zoom).max(0.0);
                self.pan[0] = self.pan[0].clamp(-1.45 * lim, 1.45 * lim);
                self.pan[1] = self.pan[1].clamp(-1.1 * lim, 1.1 * lim);
                let h = self.view_half(vis_rect);
                let shift_x = (drawer_vis_w / rect.height().max(1.0)) * h;
                let view = [self.pan[0] + shift_x, self.pan[1], h];

                // simulation step
                // In slow motion the whole TV (beam, signal, animation) runs on a slowed clock.
                let slow = if self.s.rolling_scan { self.s.slow_mo.max(1.0) } else { 1.0 };
                let mut scan_step = (0.0f32, 1.0f32, 1.0f32 / 60.0); // (phase, window, frame time)
                let new_field = if self.s.rolling_scan {
                    // Beam clock locked to the display's refresh: every displayed frame
                    // advances the beam by exactly the slice it shows, so slices tile
                    // seamlessly (no gaps or double-lit bands).
                    let forced = if self.screenshot.is_some() { Some(1.0 / 60.0) } else { None };
                    let (nf, phase, w, frame_dt) = self.scan_clock.step(real_dt, slow, forced);
                    self.sim.advance(frame_dt as f64 / slow as f64);
                    scan_step = (phase, w, frame_dt);
                    nf || self.frames < 2
                } else {
                    self.sim.advance(real_dt as f64) || self.frames < 2
                };
                let mut fi = if new_field {
                    self.sim.field(&self.s, view, real_dt)
                } else {
                    let mut f = self.last_fi.clone();
                    f.new_field = false;
                    f.u.view = view;
                    f
                };
                // glass blur radii in quarter-resolution pixels
                let px_per_face = rect.height() * ppp / (2.0 * h);
                let mm = 2.0 / (self.s.screen_inches * 25.4 * 0.6);
                fi.u.diffuse_sigma_px = (0.9 * mm * px_per_face / 4.0).clamp(0.3, 30.0);
                fi.u.halation_sigma_px = (9.0 * mm * px_per_face / 4.0).clamp(0.8, 60.0);
                // rolling scan: how much beam time this displayed frame covers
                fi.u.scan = self.s.rolling_scan as i32;
                if self.s.rolling_scan {
                    let (phase, w, frame_dt) = scan_step;
                    fi.u.scan_phase = phase;
                    fi.u.scan_w = w;
                    fi.u.tau_fast = [self.s.decay_ms * 3.0, self.s.decay_ms, self.s.decay_ms * 0.3]
                        .map(|ms| (ms / 1000.0 * 59.94).max(1e-5));
                    // auto exposure like a camera: short frames are brightened up to the green decay time
                    fi.u.scan_a = (w.max(fi.u.tau_fast[1] * 1.5)).min(1.0) * self.s.scan_exposure;
                    fi.u.eye_k_frame = if self.s.eye_ms <= 0.5 { 1.0 } else { 1.0 - (-(frame_dt * 1000.0) / self.s.eye_ms).exp() };
                }
                if self.splash.is_active() {
                    if resp.clicked() && !overlay_active {
                        self.splash.skip();
                        self.reset_trails = true;
                        if self.startup_dest_pending || self.open_guide_on_connect {
                            self.apply_startup_destination();
                        } else {
                            self.sim.switch_channel();
                        }
                    }
                }
                if self.splash.is_active() {
                    let hint = if self.splash_waiting_for_sources() {
                        match self.app_cfg.startup_dest {
                            StartupDestination::LastWatched => Some("TUNING LAST WATCHED..."),
                            _ => Some("LOADING GUIDE..."),
                        }
                    } else {
                        None
                    };
                    fi.image = Some(self.splash.render(hint));
                    fi.u.pattern = Pattern::Image.id();
                } else if self.guide.active && self.has_channels() {
                    // Ensure episodes for all visible rows & artwork for the highlighted channel are fetched
                    let (needed_sids, sel_art_provider, sel_art_id) =
                        if let Some((pch_num, sel_idx)) = self.guide.picker_selected() {
                            if let Some(pch) = self.channel_by_number(pch_num).cloned() {
                                let len = pch.items().len();
                                self.guide.clamp_picker(len);
                                let mut sids = Vec::new();
                                if pch.provider_id == "jellyfin" || pch.provider_id == "plex" {
                                    if let ChannelKind::Series { series_id, loaded_episodes: false, .. } = &pch.kind {
                                        sids.push((pch.provider_id.clone(), series_id.clone()));
                                    }
                                }
                                let art = pch
                                    .items()
                                    .get(sel_idx)
                                    .map(|m| m.artwork_id.clone())
                                    .filter(|a| !a.is_empty())
                                    .unwrap_or_else(|| pch.artwork_id.clone());
                                (sids, pch.provider_id.clone(), art)
                            } else {
                                self.guide.close_picker();
                                (Vec::new(), String::new(), String::new())
                            }
                        } else {
                            let vis_len = self.visible_channels().len();
                            self.guide.clamp_selection(vis_len);
                            let vis = provider::filter_visible_channels(
                                &self.lineup,
                                self.jellyfin.cfg.favorites_only,
                                &self.jellyfin.cfg.filter_text,
                            );
                            let mut sids = Vec::new();
                            for row in 0..6 {
                                if let Some(ch) = vis.get(self.guide.scroll_row + row) {
                                    if ch.provider_id == "jellyfin" || ch.provider_id == "plex" {
                                        if let ChannelKind::Series { series_id, loaded_episodes: false, .. } = &ch.kind {
                                            sids.push((ch.provider_id.clone(), series_id.clone()));
                                        }
                                    }
                                }
                            }
                            let (prov, art) = vis
                                .get(self.guide.selected_idx)
                                .map(|ch| (ch.provider_id.clone(), ch.preview_info(chrono::Utc::now().timestamp()).3))
                                .unwrap_or_default();
                            (sids, prov, art)
                        };
                    for (prov, sid) in needed_sids {
                        if prov == "jellyfin" {
                            self.jellyfin.ensure_series_episodes(&sid);
                        } else if prov == "plex" {
                            self.plex.ensure_series_episodes(&sid);
                        }
                    }
                    let art_img = self.request_channel_artwork(&sel_art_provider, &sel_art_id);
                    let now_lbl = self.active_watch.as_ref().map(|w| {
                        format!("NOW: CH {:02} {}", w.channel_number, w.callsign)
                    });
                    let active_id = self.active_watch.as_ref().map(|w| w.item_id.clone());
                    let picker_ch = self.guide.picker_channel().and_then(|n| self.channel_by_number(n).cloned());
                    let vis = provider::filter_visible_channels(
                        &self.lineup,
                        self.jellyfin.cfg.favorites_only,
                        &self.jellyfin.cfg.filter_text,
                    );
                    let live_vid = self.video_image.as_ref().map(|(_, f)| f);
                    let guide_frame = self.guide.render(
                        &vis,
                        picker_ch.as_ref(),
                        active_id.as_deref(),
                        live_vid,
                        now_lbl.as_deref(),
                        art_img.as_ref(),
                    );
                    fi.image = Some(guide_frame);
                    fi.u.pattern = Pattern::Image.id();
                } else {
                    fi.image = if self.s.pattern == Pattern::Video { self.video_image.clone() } else { self.image.clone() };
                }
                fi.osd = self.osd.render();
                if new_field {
                    fi.clear_burn = std::mem::take(&mut self.clear_burn);
                    fi.burn_now = std::mem::take(&mut self.burn_now);
                    fi.reset_trails = std::mem::take(&mut self.reset_trails);
                }
                self.last_fi = fi.clone();
                self.last_fi.clear_burn = false;
                self.last_fi.burn_now = false;
                self.last_fi.reset_trails = false;
                frame_fi = fi;

                if let Some(e) = &self.gpu_error {
                    ui.painter().text(
                        rect.center(),
                        egui::Align2::CENTER_CENTER,
                        format!("Graphics problem:\n{e}"),
                        egui::FontId::monospace(12.0),
                        Color32::from_rgb(240, 120, 100),
                    );
                }
                if self.fullscreen {
                    let idle = self.last_pointer_move.elapsed().as_secs_f32();
                    let since = self.fs_since.elapsed().as_secs_f32();
                    // short hint after entering, and whenever the mouse moves
                    let show = since < 3.0 || idle < 2.0;
                    if show {
                        let a = (1.0 - (idle.min(since) - 1.5).max(0.0) / 1.5).clamp(0.0, 1.0);
                        let text = "F11 or Esc: leave fullscreen   ·   scroll: zoom   ·   double-click: reset";
                        let pos = vis_rect.center_bottom() + egui::vec2(0.0, -24.0);
                        let galley = ui.painter().layout_no_wrap(text.to_string(), egui::FontId::proportional(14.0), Color32::from_gray(220));
                        let bg = egui::Rect::from_center_size(pos, galley.size() + egui::vec2(24.0, 12.0));
                        ui.painter().rect_filled(bg, 8.0, Color32::from_black_alpha((170.0 * a) as u8));
                        ui.painter().text(
                            pos,
                            egui::Align2::CENTER_CENTER,
                            text,
                            egui::FontId::proportional(14.0),
                            Color32::from_white_alpha((220.0 * a) as u8),
                        );
                    }
                    if idle > 2.0 && self.tv_hovered {
                        ctx.set_cursor_icon(egui::CursorIcon::None);
                    }
                }
                if let Some((ref msg, when)) = self.status_toast {
                    let age = when.elapsed().as_secs_f32();
                    if age < 2.6 {
                        let a = (1.0 - (age - 1.8).max(0.0) / 0.8).clamp(0.0, 1.0);
                        let pos = vis_rect.center_top() + egui::vec2(0.0, 26.0);
                        let galley = ui.painter().layout_no_wrap(msg.clone(), egui::FontId::proportional(13.5), Color32::from_rgb(245, 210, 145));
                        let bg = egui::Rect::from_center_size(pos, galley.size() + egui::vec2(22.0, 10.0));
                        ui.painter().rect_filled(bg, 6.0, Color32::from_rgba_unmultiplied(24, 22, 19, (215.0 * a) as u8));
                        ui.painter().rect_stroke(
                            bg,
                            6.0,
                            egui::Stroke::new(1.0, Color32::from_rgba_unmultiplied(186, 142, 82, (200.0 * a) as u8)),
                            egui::StrokeKind::Inside,
                        );
                        ui.painter().text(
                            pos,
                            egui::Align2::CENTER_CENTER,
                            msg,
                            egui::FontId::proportional(13.5),
                            Color32::from_rgba_unmultiplied(245, 220, 170, (245.0 * a) as u8),
                        );
                    } else {
                        self.status_toast = None;
                    }
                }
                if !self.sim.power_on && self.quitting_since.is_none() {
                    ui.painter().text(
                        rect.left_bottom() + egui::vec2(12.0, -12.0),
                        egui::Align2::LEFT_BOTTOM,
                        "Power is off — press Power on",
                        egui::FontId::proportional(13.0),
                        Color32::from_gray(120),
                    );
                }
            });

        // Slide-in right quick menu & translucent pull handle (works in both windowed and fullscreen)
        if central_rect.width() > 0.0 && central_rect.height() > 0.0 {
            let max_w = (central_rect.width() - 260.0).max(270.0).min(420.0);
            let drawer_w = self.sidebar_width.clamp(270.0, max_w);
            let drawer_x = central_rect.right() - drawer_w * eased;

            let tab_w = 24.0;
            let tab_h = 92.0;
            let tab_top = central_rect.center().y - tab_h * 0.5;
            let tab_bottom = tab_top + tab_h;

            // Handle fades in smoothly when the mouse moves (or while the service panel is open)
            // and slowly fades completely away once the mouse goes idle.
            let pointer_idle = self.last_pointer_move.elapsed().as_secs_f32();
            let drawer_visible = self.sidebar_open || self.sidebar_anim > 0.01;
            let show_handle = drawer_visible || (pointer_idle < 2.0 && !self.save_preset_open);
            let vis_target = if show_handle { 1.0 } else { 0.0 };
            let vis_step = dt_ui / if show_handle { 0.18 } else { 1.4 };
            if self.handle_vis < vis_target {
                self.handle_vis = (self.handle_vis + vis_step).min(vis_target);
            } else if self.handle_vis > vis_target {
                self.handle_vis = (self.handle_vis - vis_step).max(vis_target);
            }
            let vis_eased = self.handle_vis * self.handle_vis * (3.0 - 2.0 * self.handle_vis);

            let near_handle = ptr_pos.is_some_and(|p| {
                p.x >= drawer_x - tab_w - 36.0
                    && p.x <= drawer_x + 8.0
                    && p.y >= central_rect.top()
                    && p.y <= central_rect.bottom()
            });
            let highlight_handle = drawer_visible || (near_handle && show_handle && !self.save_preset_open);
            let h_target = if highlight_handle { 1.0 } else { 0.0 };
            let h_step = dt_ui / if highlight_handle { 0.14 } else { 0.24 };
            if self.handle_anim < h_target {
                self.handle_anim = (self.handle_anim + h_step).min(h_target);
            } else if self.handle_anim > h_target {
                self.handle_anim = (self.handle_anim - h_step).max(h_target);
            }
            let handle_eased = 1.0 - (1.0 - self.handle_anim).powi(3);
            let handle_alpha = (0.35 + 0.65 * handle_eased) * vis_eased;

            // Animated service panel drawer
            if self.sidebar_anim > 0.005 {
                let drawer_rect = egui::Rect::from_min_size(egui::pos2(drawer_x, central_rect.top()), egui::vec2(drawer_w, central_rect.height()));
                // Subtle drop shadow along the left edge of the drawer
                let shadow_layer = ctx.layer_painter(egui::LayerId::new(egui::Order::Foreground, egui::Id::unique("controls_drawer_shadow")));
                for i in 0..6 {
                    let w = 4.0;
                    let x0 = drawer_rect.left() - (i as f32 + 1.0) * w;
                    let alpha = ((6 - i) as f32 * 15.0 * eased) as u8;
                    shadow_layer.rect_filled(
                        egui::Rect::from_min_size(egui::pos2(x0, drawer_rect.top()), egui::vec2(w, drawer_rect.height())),
                        0.0,
                        Color32::from_black_alpha(alpha),
                    );
                }

                // Left-edge resize grip (split above & below the pull handle so clicks never collide)
                let mut resizing = false;
                for (tag, y0, y1) in [
                    ("controls_drawer_resize_top", drawer_rect.top(), (tab_top - 4.0).max(drawer_rect.top())),
                    ("controls_drawer_resize_bot", (tab_bottom + 4.0).min(drawer_rect.bottom()), drawer_rect.bottom()),
                ] {
                    if y1 > y0 + 2.0 {
                        egui::Area::new(egui::Id::unique(tag))
                            .order(egui::Order::Tooltip)
                            .fixed_pos(egui::pos2(drawer_rect.left() - 3.0, y0))
                            .interactable(true)
                            .show(&ctx, |ui| {
                                let (_, rresp) = ui.allocate_exact_size(egui::vec2(7.0, y1 - y0), egui::Sense::drag());
                                if rresp.hovered() || rresp.dragged() {
                                    ctx.set_cursor_icon(egui::CursorIcon::ResizeHorizontal);
                                }
                                if rresp.dragged() {
                                    resizing = true;
                                    self.sidebar_width = (self.sidebar_width - rresp.drag_delta().x).clamp(270.0, max_w);
                                    self.reset_trails = true;
                                }
                            });
                    }
                }
                self.sidebar_resizing = resizing;

                let visible_drawer_rect = drawer_rect.intersect(central_rect);
                egui::Area::new(egui::Id::unique("controls_drawer"))
                    .order(egui::Order::Foreground)
                    .fixed_pos(drawer_rect.min)
                    .constrain(false)
                    .interactable(true)
                    .show(&ctx, |ui| {
                        ui.set_clip_rect(visible_drawer_rect);
                        ui.scope_builder(egui::UiBuilder::new().max_rect(drawer_rect), |ui| {
                            self.sidebar_contents(ui);
                        });
                    });
            } else {
                self.sidebar_resizing = false;
            }

            // Retro walnut & brushed-metal pull handle: fades in on mouse movement or while open,
            // and melts completely away once the mouse idles; clicking toggles open/closed.
            // Drawn on Order::Tooltip so clicking inside the drawer can never occlude the handle!
            if vis_eased > 0.004 {
                let tab_x = (drawer_x - tab_w).max(central_rect.left());
                let tab_pos = egui::pos2(tab_x, tab_top);
                egui::Area::new(egui::Id::unique("controls_pull_tab"))
                    .order(egui::Order::Tooltip)
                    .fixed_pos(tab_pos)
                    .constrain(false)
                    .interactable(true)
                    .show(&ctx, |ui| {
                        ui.set_clip_rect(central_rect);
                        let (rect, resp) = ui.allocate_exact_size(egui::vec2(tab_w, tab_h), egui::Sense::click());
                        let hot = resp.hovered() && show_handle;
                        if hot {
                            ctx.set_cursor_icon(egui::CursorIcon::PointingHand);
                        }
                        let a = if hot { vis_eased } else { handle_alpha };
                        let fade = |c: Color32| {
                            Color32::from_rgba_unmultiplied(c.r(), c.g(), c.b(), (c.a() as f32 * a) as u8)
                        };
                        let rounding = egui::CornerRadius { nw: 7, sw: 7, ne: 0, se: 0 };
                        let wood_bg = fade(if hot {
                            Color32::from_rgb(92, 54, 30)
                        } else {
                            Color32::from_rgb(66, 39, 21)
                        });
                        let stroke = fade(if hot {
                            Color32::from_rgb(232, 168, 86)
                        } else {
                            Color32::from_rgb(142, 128, 106)
                        });
                        ui.painter().rect_filled(rect, rounding, wood_bg);
                        let plate = egui::Rect::from_min_max(
                            rect.min + egui::vec2(3.0, 4.0),
                            rect.max - egui::vec2(0.0, 4.0),
                        );
                        ui.painter().rect_filled(
                            plate,
                            egui::CornerRadius { nw: 4, sw: 4, ne: 0, se: 0 },
                            fade(if hot { Color32::from_rgb(56, 52, 46) } else { Color32::from_rgb(40, 38, 35) }),
                        );
                        ui.painter().rect_stroke(rect, rounding, egui::Stroke::new(1.1, stroke), egui::StrokeKind::Inside);
                        let c = plate.center();
                        let ridge_col = fade(Color32::from_rgb(112, 104, 90));
                        for &dy in &[-24.0f32, -19.0, 19.0, 24.0] {
                            ui.painter().line_segment(
                                [egui::pos2(plate.left() + 4.0, c.y + dy), egui::pos2(plate.right() - 4.0, c.y + dy)],
                                egui::Stroke::new(1.0, ridge_col),
                            );
                        }
                        let arrow_col = fade(if hot { Color32::from_rgb(248, 186, 92) } else { Color32::from_rgb(214, 200, 176) });
                        let arrow_stroke = egui::Stroke::new(2.0, arrow_col);
                        if self.sidebar_open {
                            // ">" chevron to close
                            ui.painter().line_segment([c + egui::vec2(-3.0, -6.0), c + egui::vec2(3.0, 0.0)], arrow_stroke);
                            ui.painter().line_segment([c + egui::vec2(3.0, 0.0), c + egui::vec2(-3.0, 6.0)], arrow_stroke);
                        } else {
                            // "<" chevron to open
                            ui.painter().line_segment([c + egui::vec2(3.0, -6.0), c + egui::vec2(-3.0, 0.0)], arrow_stroke);
                            ui.painter().line_segment([c + egui::vec2(-3.0, 0.0), c + egui::vec2(3.0, 6.0)], arrow_stroke);
                        }
                        let tip = if self.sidebar_open {
                            "Close service panel"
                        } else {
                            "Open service panel"
                        };
                        if resp.on_hover_text(tip).clicked() {
                            self.sidebar_open = !self.sidebar_open;
                        }
                    });
            }
        } else {
            self.sidebar_resizing = false;
        }

        // Floating tabbed full-controls window
        self.show_settings_window(&ctx);

        // Save-preset modal dialog
        if self.save_preset_open {
            let mut open = self.save_preset_open;
            let mut close_now = false;
            egui::Window::new("Save preset")
                .open(&mut open)
                .collapsible(false)
                .resizable(false)
                .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
                .show(&ctx, |ui| {
                    ui.set_min_width(300.0);
                    ui.label("Save your current CRT, signal, VCR, and room settings as a reusable preset.");
                    ui.add_space(6.0);
                    let te = ui.add(
                        egui::TextEdit::singleline(&mut self.preset_name_input)
                            .hint_text("e.g. Sony Trinitron RGB")
                            .desired_width(290.0),
                    );
                    if self.save_preset_focus {
                        te.request_focus();
                        self.save_preset_focus = false;
                    }
                    let exists = self
                        .user_presets
                        .iter()
                        .any(|p| p.name.eq_ignore_ascii_case(self.preset_name_input.trim()));
                    if exists {
                        ui.label(RichText::new("A saved preset with this name already exists and will be updated.").small().color(Color32::from_rgb(235, 175, 100)));
                    }
                    ui.add_space(4.0);
                    ui.checkbox(&mut self.save_as_default, "Set as default startup profile");
                    if !self.preset_msg.is_empty() {
                        ui.colored_label(Color32::from_rgb(235, 130, 100), &self.preset_msg);
                    }
                    ui.add_space(8.0);
                    ui.horizontal(|ui| {
                        let enter = ui.input(|i| i.key_pressed(egui::Key::Enter));
                        let save_label = if exists { "Overwrite preset" } else { "Save preset" };
                        if ui.button(save_label).clicked() || enter {
                            let name = self.preset_name_input.clone();
                            let make_def = self.save_as_default;
                            if self.save_current_preset(&name, make_def) {
                                close_now = true;
                            }
                        }
                        if ui.button("Cancel").clicked() {
                            close_now = true;
                        }
                    });
                });
            self.save_preset_open = open && !close_now;
        }

        // screenshot mode
        self.frames += 1;
        let mut take_screenshot = None;
        if let Some(path) = self.screenshot.clone() {
            if self.frames >= 120 && !self.shot_requested {
                self.shot_requested = true;
                take_screenshot = Some(path);
            }
        }
        ctx.request_repaint();

        FrameOutput {
            crt_rect: central_rect,
            fi: frame_fi,
            want_hdr: self.s.hdr_mode,
            prefer_pq: self.hdr_prefer_pq,
            fullscreen_cmd: self.pending_fullscreen.take(),
            focus_cmd,
            close_cmd: self.pending_close,
            take_screenshot,
        }
    }
}

fn hms(t: f64) -> String {
    if !t.is_finite() || t <= 0.0 {
        return "0:00".into();
    }
    let t = t as u64;
    if t >= 3600 { format!("{}:{:02}:{:02}", t / 3600, t / 60 % 60, t % 60) } else { format!("{}:{:02}", t / 60, t % 60) }
}

/// Computes the CRT diagonal (in inches) where each phosphor maps 1:1 onto
/// physical display pixels at 1× zoom.
pub fn pixel_perfect_screen_inches(pitch_mm: f32, mask: Mask, view_h_px: f32, view_half_1x: f32) -> f32 {
    let target_px_per_pitch = match mask {
        Mask::ApertureGrille | Mask::SlotMask => 3.0,
        Mask::ShadowMask => 3.0 * 3.0f32.sqrt(),
    };
    let inches = (pitch_mm * view_h_px) / (15.24 * view_half_1x.max(1e-3) * target_px_per_pitch);
    inches.clamp(4.0, 36.0)
}

/// Beam clock for the rolling scan, locked to the display refresh.
///
/// Raw frame times measured by the app are noisy (compositor and vsync jitter),
/// while the monitor actually shows frames at a steady rate. Using the noisy
/// times made each frame's slice of the sweep a different size than the slice
/// the beam really advanced, so bands were occasionally lit twice or skipped:
/// a periodic flicker. Here every frame advances the beam by a whole number of
/// refresh periods (normally one; more after a dropped frame), and the slice
/// shown is exactly that advance, carried across field boundaries without loss.
#[derive(Default)]
pub struct ScanClock {
    phase: f64,
    recent: std::collections::VecDeque<f32>,
    locked_hz: Option<f32>,
    monitor_hz: Option<f32>,
}

impl ScanClock {
    pub fn set_monitor_hz(&mut self, hz: Option<f32>) {
        self.monitor_hz = hz.filter(|h| h.is_finite() && *h >= 24.0 && *h <= 1000.0);
    }

    /// Returns (new field started, beam phase 0..1, slice width in fields, frame time).
    pub fn step(&mut self, real_dt: f32, slow: f32, forced_period: Option<f32>) -> (bool, f32, f32, f32) {
        let period = match forced_period {
            Some(p) => p,
            None => {
                if real_dt > 0.0005 && real_dt < 0.1 {
                    self.recent.push_back(real_dt);
                    if self.recent.len() > 121 {
                        self.recent.pop_front();
                    }
                }
                if self.recent.len() < 8 {
                    // Use the OS-reported monitor refresh rate immediately on startup
                    1.0 / self.monitor_hz.unwrap_or(60.0)
                } else {
                    // Trimmed mean of recent frame times (ignores dropped frames and
                    // timing spikes), then snap to the monitor rate or a standard refresh rate.
                    let mut v: Vec<f32> = self.recent.iter().copied().collect();
                    v.sort_by(|a, b| a.partial_cmp(b).unwrap());
                    let cut = v.len() / 10;
                    let keep = &v[cut..v.len() - cut];
                    let mean = keep.iter().sum::<f32>() / keep.len().max(1) as f32;
                    let est_hz = 1.0 / mean.clamp(1.0 / 1000.0, 0.1);
                    const RATES: [f32; 18] = [
                        50.0, 59.94, 60.0, 72.0, 75.0, 90.0, 100.0, 119.88, 120.0, 138.0, 144.0, 165.0, 175.0, 180.0,
                        200.0, 240.0, 360.0, 480.0,
                    ];
                    // stay locked to the current standard rate unless timing clearly moved away
                    if let Some(l) = self.locked_hz {
                        if (est_hz / l - 1.0).abs() < 0.12 {
                            return self.advance(real_dt, 1.0 / l, slow, false);
                        }
                        self.locked_hz = None;
                    }
                    let mut best = est_hz;
                    let mut best_err = 0.06;
                    if let Some(mh) = self.monitor_hz {
                        let e = (est_hz / mh - 1.0).abs();
                        if e < best_err {
                            best_err = e;
                            best = mh;
                            self.locked_hz = Some(mh);
                        }
                    }
                    for r in RATES {
                        let e = (est_hz / r - 1.0).abs();
                        if e < best_err {
                            best_err = e;
                            best = r;
                            self.locked_hz = Some(r);
                        }
                    }
                    1.0 / best
                }
            }
        };
        self.advance(real_dt, period, slow, forced_period.is_some())
    }

    fn advance(&mut self, real_dt: f32, period: f32, slow: f32, forced: bool) -> (bool, f32, f32, f32) {
        // Whole refresh periods only: hold steady at 1 period across normal compositor
        // jitter (up to 1.65x period), and catch up cleanly on genuine dropped frames.
        let steps = if forced || real_dt < period * 1.65 {
            1.0
        } else {
            (real_dt / period).round().clamp(1.0, 8.0)
        };
        let frame_dt = steps * period;
        let adv = frame_dt as f64 * 59.94 / slow as f64;
        self.phase += adv;
        let mut new_field = false;
        if self.phase >= 1.0 {
            self.phase = self.phase.fract();
            new_field = true;
        }
        (new_field, self.phase as f32, adv.min(1.0) as f32, frame_dt)
    }
}

#[cfg(test)]
mod scan_clock_tests {
    use super::ScanClock;

    #[test]
    fn slices_tile_under_jitter_at_240hz() {
        let mut c = ScanClock::default();
        let mut seed = 12345u64;
        let mut prev_phase = 0.0f64;
        let mut total_w = 0.0f64;
        let mut fields = 0u32;
        let mut widths = Vec::new();
        for i in 0..2400 {
            seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            let jitter = ((seed >> 33) as f32 / (1u64 << 31) as f32 - 0.5) * 0.0025; // ±1.25 ms noise
            let dt = 1.0 / 240.0 + jitter;
            let (nf, phase, w, _) = c.step(dt, 1.0, None);
            if i > 200 {
                // the slice must start exactly where the previous one ended
                let start = (phase as f64 - w as f64).rem_euclid(1.0);
                let gap = (start - prev_phase).rem_euclid(1.0);
                assert!(gap < 1e-4 || gap > 1.0 - 1e-4, "gap {gap} at frame {i}");
                total_w += w as f64;
                widths.push(w);
                if nf {
                    fields += 1;
                }
            }
            prev_phase = phase as f64;
        }
        // every frame shows the same slice size despite the noisy timings
        let mut uniq: Vec<i32> = widths.iter().map(|w| (w * 100000.0) as i32).collect(); uniq.sort(); uniq.dedup(); println!("distinct widths x1e5: {:?} first-bad {:?}", uniq, widths.iter().position(|w| (*w - 0.24975).abs() > 1e-3));
        let wmin = widths.iter().cloned().fold(1.0f32, f32::min);
        let wmax = widths.iter().cloned().fold(0.0f32, f32::max);
        assert!(wmax - wmin < 1e-3, "slice sizes vary: {wmin}..{wmax}");
        assert!((total_w - fields as f64).abs() < 1.5);
    }

    #[test]
    fn pixel_perfect_screen_size_matches_exact_triad_pitch() {
        use super::pixel_perfect_screen_inches;
        use crate::settings::Mask;
        let view_h_px = 1080.0f32;
        let view_half_1x = 1.26f32;
        let pitch_mm = 0.62f32;
        let inches = pixel_perfect_screen_inches(pitch_mm, Mask::SlotMask, view_h_px, view_half_1x);
        let pitch_face = pitch_mm * (2.0 / (inches * 25.4 * 0.6));
        let pxw = 2.0 * view_half_1x / view_h_px;
        let px_per_triad = pitch_face / pxw;
        assert!((px_per_triad - 3.0).abs() < 1e-4, "expected 3.0 px/triad, got {px_per_triad}");

        let inches_sm = pixel_perfect_screen_inches(pitch_mm, Mask::ShadowMask, view_h_px, view_half_1x);
        let pitch_face_sm = pitch_mm * (2.0 / (inches_sm * 25.4 * 0.6));
        let d_px = (pitch_face_sm / 3.0f32.sqrt()) / pxw;
        assert!((d_px - 3.0).abs() < 1e-4, "expected 3.0 px/dot spacing, got {d_px}");
    }
}
