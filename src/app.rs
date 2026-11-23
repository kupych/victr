use crate::audio::AudioEngine;
use crate::gpu::{FrameInput, HdrStatus};
use crate::mpv::{Player, StartOpts};
use crate::settings::{self, Input, Mask, Pattern, PhosphorSet, Settings, TapeSpeed, UserPreset};
use crate::sim::Sim;
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
    save_preset_open: bool,
    save_preset_focus: bool,
    preset_name_input: String,
    preset_msg: String,
    confirm_delete_idx: Option<usize>,
    degauss_mag_start: Option<f32>,
    degauss_pinned_t: Option<f32>,
    // slide-in right sidebar (click handle or Controls button to toggle open/closed)
    sidebar_open: bool,
    sidebar_anim: f32,
    handle_anim: f32,
    sidebar_width: f32,
    sidebar_resizing: bool,
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
    // video through mpv
    player: Option<Player>,
    video_image: Option<(u64, Arc<(usize, usize, Vec<u8>)>)>,
    video_frame_no: u64,
    video_name: String,
    video_url: String,
    crop: bool,
    seek_drag: Option<f64>,
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
    fs_since: std::time::Instant,
    last_pointer_move: std::time::Instant,
}

impl Drop for CrtApp {
    fn drop(&mut self) {
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

        let mut sim = Sim::new();
        sim.warm = if screenshot.is_some() { 1.0 } else { 0.35 };
        if screenshot.is_some() {
            sim.degauss_t = 10.0;
        }
        let preset_idx = preset.unwrap_or(0).min(settings::PRESETS.len() - 1);
        let mut s = settings::preset(preset_idx);
        if let Some(p) = pattern {
            s.pattern = Pattern::ALL[p.min(Pattern::ALL.len() - 1)];
        }
        let user_presets = settings::load_user_presets();
        let audio = if screenshot.is_none() { Some(AudioEngine::start()) } else { None };
        let screenshot_mode = screenshot.is_some();
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
            burn_now: preset_idx == 2,
            reset_trails: true,
            preset_sel: PresetSel::BuiltIn(preset_idx),
            user_presets,
            save_preset_open: false,
            save_preset_focus: false,
            preset_name_input: String::new(),
            preset_msg: String::new(),
            confirm_delete_idx: None,
            degauss_mag_start: None,
            degauss_pinned_t: None,
            sidebar_open: false,
            sidebar_anim: 0.0,
            handle_anim: 0.0,
            sidebar_width: 440.0,
            sidebar_resizing: false,
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
            video_image: None,
            video_frame_no: 0,
            video_name: String::new(),
            video_url: String::new(),
            crop: false,
            seek_drag: None,
            audio,
            inbox: if screenshot_mode { None } else { Some(crate::link::listen(egui_ctx.clone())) },
            use_my_mpv: true,
            link_msg: String::new(),
            about_msg: String::new(),
            tv_hovered: false,
            fullscreen: false,
            pending_fullscreen: None,
            fs_since: std::time::Instant::now(),
            last_pointer_move: std::time::Instant::now(),
        };
        if let Some((src, start)) = mpv_src {
            app.start_video_at(src, start);
        }
        app
    }

    pub fn set_hdr_status(&mut self, status: HdrStatus) {
        if !self.hdr_seeded_peak {
            let os_confirmed_hdr = status.reported_peak_nits.is_some_and(|p| p > 350.0)
                || status.reported_edr_headroom.is_some_and(|h| h > 1.15);
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

    /// Starts in fullscreen (the --fullscreen option).
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
        self.reset_trails = true;
    }

    pub fn request_pixel_perfect(&mut self) {
        self.pixel_perfect_req = true;
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
        // keep the user's sound and display HDR preferences when switching built-in presets
        if matches!(sel, PresetSel::BuiltIn(_)) {
            self.s.sound = old.sound;
            self.s.volume = old.volume;
            self.s.tv_speaker = old.tv_speaker;
            self.s.mono = old.mono;
            self.s.whine = old.whine;
            self.s.test_tone = old.test_tone;
            self.s.hdr_mode = old.hdr_mode;
            self.s.hdr_paper_white = old.hdr_paper_white;
            self.s.hdr_peak_nits = old.hdr_peak_nits;
            self.s.hdr_ui_nits = old.hdr_ui_nits;
        } else if let Some(p) = &self.player {
            p.volume((self.s.volume * 100.0) as f64);
        }
        self.degauss_mag_start = None;
        self.reset_trails = true;
        if sel == PresetSel::BuiltIn(2) {
            self.burn_now = true;
        }
    }

    fn open_save_preset_dialog(&mut self) {
        self.preset_name_input = match self.preset_sel {
            PresetSel::User(i) => self.user_presets.get(i).map(|p| p.name.clone()).unwrap_or_default(),
            PresetSel::BuiltIn(_) => String::new(),
        };
        self.preset_msg.clear();
        self.save_preset_open = true;
        self.save_preset_focus = true;
    }

    fn save_current_preset(&mut self, raw_name: &str) -> bool {
        match settings::save_user_preset(raw_name, &self.s) {
            Ok(saved) => {
                self.user_presets = settings::load_user_presets();
                if let Some(pos) = self.user_presets.iter().position(|p| p.file_name == saved.file_name) {
                    self.preset_sel = PresetSel::User(pos);
                }
                self.preset_msg = format!("Saved preset \"{}\".", saved.name);
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
                self.confirm_delete_idx = None;
                self.preset_msg = format!("Deleted preset \"{}\".", target.name);
            }
            Err(e) => {
                self.preset_msg = e;
            }
        }
    }

    fn preset_controls(&mut self, ui: &mut egui::Ui) {
        egui::CollapsingHeader::new(RichText::new("Presets").strong())
            .default_open(false)
            .show(ui, |ui| {
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
                        if self.save_current_preset(&name) {
                            self.preset_name_input.clear();
                        }
                    }
                });
                if !self.user_presets.is_empty() {
                    ui.add_space(4.0);
                    ui.label(RichText::new("Saved presets").weak().small());
                    let mut load_idx = None;
                    let mut delete_idx = None;
                    for i in 0..self.user_presets.len() {
                        let name = self.user_presets[i].name.clone();
                        let active = self.preset_sel == PresetSel::User(i);
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
                                } else if ui.small_button("×").on_hover_text("Delete this saved preset").clicked() {
                                    self.confirm_delete_idx = Some(i);
                                }
                            });
                        });
                    }
                    if let Some(i) = load_idx {
                        self.apply_preset(PresetSel::User(i));
                        self.confirm_delete_idx = None;
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

    fn start_video_at(&mut self, src: String, start: f64) {
        // replacing the player stops the previous one
        let ring = self.audio.as_ref().map(|a| a.ring.clone());
        let opts = StartOpts { volume: (self.s.volume * 100.0) as f64, crop: self.crop, start, user_config: self.use_my_mpv };
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
                    if self.player.is_some() && ui.button("Stop").clicked() {
                        stop = true;
                    }
                });
                if stop {
                    self.player.take();
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
                let mut seek_abs: Option<f64> = None;
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
                        ui.horizontal(|ui| {
                            if ui.button(if paused { "Play" } else { "Pause" }).clicked() {
                                toggle = true;
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
                                self.seek_drag = Some(v);
                            } else if r.drag_stopped() || r.clicked() {
                                seek_abs = Some(v);
                                self.seek_drag = None;
                            }
                        }
                    }
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
                if let Some(p) = &self.player {
                    if toggle {
                        p.toggle_pause();
                    }
                    if let Some(d) = seek {
                        p.seek(d);
                    }
                    if let Some(t) = seek_abs {
                        p.seek_abs(t);
                    }
                }
            });
    }

    fn update_audio(&mut self) {
        let Some(a) = &self.audio else { return };
        let s = &self.s;
        let mut p = a.params.lock().unwrap();
        p.enabled = s.sound;
        p.volume = s.volume;
        p.tv_speaker = s.tv_speaker;
        p.mono = s.mono;
        p.screen_inches = s.screen_inches;
        p.whine = s.whine;
        p.power_on = self.sim.power_on;
        p.warm = self.sim.warm;
        p.degauss_t = self.sim.degauss_t;
        p.test_tone = s.test_tone && s.pattern == Pattern::ColorBars;
        p.rf = s.input == Input::Rf;
        p.snr_db = s.signal_db;
        p.fine_tune = s.fine_tune;
        p.ghost = s.ghost_amp;
        p.hum = s.hum;
        p.impulse = s.impulse;
        p.extra_noise = s.extra_noise;
        p.vcr = s.vcr;
        let base_bw = match s.tape_speed {
            TapeSpeed::Sp => 10.0,
            TapeSpeed::Lp => 7.0,
            TapeSpeed::Ep => 5.0,
        };
        p.tape_bw_khz = base_bw * (1.0 - 0.35 * s.tape_wear);
        p.tape_wear = s.tape_wear;
        p.tracking = s.tracking;
        p.dropouts = s.dropouts;
        p.wow = s.timebase_jitter;
        p.head_switch = s.head_switch;
        p.age = (s.age_hours / 40000.0).clamp(0.0, 1.0);
        p.ripple = s.ripple;
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

    fn controls(&mut self, ui: &mut egui::Ui) {
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

        let inner_frame = egui::Frame::NONE.inner_margin(egui::Margin { left: 16, right: 18, top: 10, bottom: 10 });
        inner_frame.show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.label(
                    RichText::new("SERVICE PANEL")
                        .strong()
                        .size(12.5)
                        .color(Color32::from_rgb(242, 184, 96)),
                );
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui
                        .small_button("Close >")
                        .on_hover_text("Slide the service panel closed")
                        .clicked()
                    {
                        self.sidebar_open = false;
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
            if self.fullscreen {
                ui.add_space(4.0);
                ui.horizontal(|ui| {
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
                    if ui.small_button("Save preset…").clicked() {
                        self.open_save_preset_dialog();
                    }
                    if ui.small_button("Reset view").clicked() {
                        self.zoom = 1.0;
                        self.pan = [0.0, 0.0];
                        self.reset_trails = true;
                    }
                });
            }
            ui.separator();
            egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
                ui.set_width((ui.available_width() - 6.0).max(200.0));
                ui.add_space(2.0);
                self.preset_controls(ui);
                self.video_controls(ui);
                self.controls(ui);
                self.sound_controls(ui);
                self.about_controls(ui);
                if !self.message.is_empty() && self.message != "pick" && self.message != "pickvideo" {
                    ui.colored_label(Color32::from_rgb(230, 120, 90), &self.message);
                }
                ui.add_space(8.0);
                ui.label(RichText::new("Scroll on the screen to zoom into the phosphors, drag to pan, double-click to reset.").weak().small());
                ui.add_space(12.0);
            });
        });
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

        // dropped pictures
        let dropped: Vec<_> = ctx.input(|i| i.raw.dropped_files.clone());
        for f in dropped {
            let path = f.path().to_path_buf();
            let ext = path.extension().map(|e| e.to_string_lossy().to_lowercase()).unwrap_or_default();
            if !["png", "jpg", "jpeg", "bmp", "gif"].contains(&ext.as_str()) && !path.as_os_str().is_empty() {
                // anything that isn't a picture goes to mpv
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

        // fullscreen: F11 toggles, Esc leaves; Ctrl+1 snaps screen size to 1:1 pixel-perfect phosphors
        let f11 = ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::F11));
        let esc = (self.fullscreen || self.save_preset_open)
            && ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Escape));
        let ctrl_1 = !ctx.egui_wants_keyboard_input()
            && ctx.input_mut(|i| i.consume_key(egui::Modifiers::COMMAND, egui::Key::Num1));
        if f11 {
            self.set_fullscreen(!self.fullscreen);
        } else if esc {
            if self.save_preset_open {
                self.save_preset_open = false;
            } else {
                self.set_fullscreen(false);
            }
        }
        if ctrl_1 {
            self.pixel_perfect_req = true;
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

        // the person's mpv key bindings work while the pointer is over the TV
        if self.player.is_some()
            && self.s.pattern == Pattern::Video
            && !ctx.egui_wants_keyboard_input()
            && (self.tv_hovered || (self.fullscreen && !self.sidebar_open))
        {
            let keys: Vec<String> = ctx.input(|i| i.events.iter().filter_map(mpv_key_name).collect());
            if let Some(p) = &self.player {
                for k in keys {
                    p.key(&k);
                }
            }
        }

        // newest frame from mpv
        if let Some(p) = &self.player {
            let sh = p.shared.lock().unwrap();
            if sh.frame_no != self.video_frame_no {
                self.video_frame_no = sh.frame_no;
                if let Some(f) = &sh.frame {
                    self.image_counter += 1;
                    self.video_image = Some((self.image_counter, f.clone()));
                }
            }
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
                                        RichText::new("Controls >").color(Color32::WHITE).strong()
                                    } else {
                                        RichText::new("< Controls")
                                    };
                                    let btn = egui::Button::new(btn_label).selected(self.sidebar_open);
                                    if ui
                                        .add(btn)
                                        .on_hover_text("Toggle the right-side service panel open or closed (you can also click the right-edge handle).")
                                        .clicked()
                                    {
                                        self.sidebar_open = !self.sidebar_open;
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
                let max_w = (rect.width() - 260.0).max(380.0).min(620.0);
                let drawer_w = self.sidebar_width.clamp(380.0, max_w);
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
                let overlay_active = over_drawer || over_handle || self.sidebar_resizing || self.save_preset_open;

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

                // zoom & pan
                let h = self.view_half(vis_rect);
                let shift_x = (drawer_vis_w / rect.height().max(1.0)) * h;
                if self.tv_hovered {
                    let scroll = ctx.input(|i| i.smooth_scroll_delta.y);
                    if scroll.abs() > 0.0 {
                        if let Some(mp) = resp.hover_pos() {
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
                if resp.dragged() && !overlay_active {
                    let d = resp.drag_delta();
                    let k = 2.0 * h / rect.height();
                    self.pan[0] -= d.x * k;
                    self.pan[1] -= d.y * k;
                    self.reset_trails = true;
                }
                if resp.double_clicked() && !overlay_active {
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
                fi.image = if self.s.pattern == Pattern::Video { self.video_image.clone() } else { self.image.clone() };
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
                if !self.sim.power_on {
                    ui.painter().text(
                        rect.left_bottom() + egui::vec2(12.0, -12.0),
                        egui::Align2::LEFT_BOTTOM,
                        "Power is off — press Power on",
                        egui::FontId::proportional(13.0),
                        Color32::from_gray(120),
                    );
                }
            });

        // Slide-in right service panel & translucent pull handle (works in both windowed and fullscreen)
        if central_rect.width() > 0.0 && central_rect.height() > 0.0 {
            let max_w = (central_rect.width() - 260.0).max(380.0).min(620.0);
            let drawer_w = self.sidebar_width.clamp(380.0, max_w);
            let drawer_x = central_rect.right() - drawer_w * eased;

            let tab_w = 24.0;
            let tab_h = 92.0;
            let tab_top = central_rect.center().y - tab_h * 0.5;
            let tab_bottom = tab_top + tab_h;

            // Handle stays subtly visible at low opacity at rest and brightens smoothly
            // when moused over, near the right edge, or while the service panel is open.
            let near_handle = ptr_pos.is_some_and(|p| {
                p.x >= drawer_x - tab_w - 36.0
                    && p.x <= drawer_x + 8.0
                    && p.y >= central_rect.top()
                    && p.y <= central_rect.bottom()
            });
            let highlight_handle = self.sidebar_open || self.sidebar_anim > 0.01 || (near_handle && !self.save_preset_open);
            let h_target = if highlight_handle { 1.0 } else { 0.0 };
            let h_step = dt_ui / if highlight_handle { 0.14 } else { 0.24 };
            if self.handle_anim < h_target {
                self.handle_anim = (self.handle_anim + h_step).min(h_target);
            } else if self.handle_anim > h_target {
                self.handle_anim = (self.handle_anim - h_step).max(h_target);
            }
            let handle_eased = 1.0 - (1.0 - self.handle_anim).powi(3);
            let handle_alpha = 0.20 + 0.80 * handle_eased;

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
                                    self.sidebar_width = (self.sidebar_width - rresp.drag_delta().x).clamp(380.0, max_w);
                                    self.reset_trails = true;
                                }
                            });
                    }
                }
                self.sidebar_resizing = resizing;

                egui::Area::new(egui::Id::unique("controls_drawer"))
                    .order(egui::Order::Foreground)
                    .fixed_pos(drawer_rect.min)
                    .interactable(true)
                    .show(&ctx, |ui| {
                        ui.set_clip_rect(central_rect);
                        egui::Frame::NONE.show(ui, |ui| {
                            ui.set_min_size(egui::vec2(drawer_w, central_rect.height()));
                            ui.set_max_size(egui::vec2(drawer_w, central_rect.height()));
                            self.sidebar_contents(ui);
                        });
                    });
            } else {
                self.sidebar_resizing = false;
            }

            // Retro walnut & brushed-metal pull handle: always gently visible at low opacity,
            // brightens smoothly on mouseover or when open; clicking toggles open/closed.
            let tab_x = (drawer_x - tab_w).max(central_rect.left());
            let tab_pos = egui::pos2(tab_x, tab_top);
            egui::Area::new(egui::Id::unique("controls_pull_tab"))
                .order(egui::Order::Foreground)
                .fixed_pos(tab_pos)
                .interactable(true)
                .show(&ctx, |ui| {
                    ui.set_clip_rect(central_rect);
                    let (rect, resp) = ui.allocate_exact_size(egui::vec2(tab_w, tab_h), egui::Sense::click());
                    let hot = resp.hovered();
                    if hot {
                        ctx.set_cursor_icon(egui::CursorIcon::PointingHand);
                    }
                    let a = if hot { 1.0 } else { handle_alpha };
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
        } else {
            self.sidebar_resizing = false;
        }

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
                    if !self.preset_msg.is_empty() {
                        ui.colored_label(Color32::from_rgb(235, 130, 100), &self.preset_msg);
                    }
                    ui.add_space(8.0);
                    ui.horizontal(|ui| {
                        let enter = ui.input(|i| i.key_pressed(egui::Key::Enter));
                        let save_label = if exists { "Overwrite preset" } else { "Save preset" };
                        if ui.button(save_label).clicked() || enter {
                            let name = self.preset_name_input.clone();
                            if self.save_current_preset(&name) {
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
            close_cmd: false,
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
}

impl ScanClock {
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
                if self.recent.is_empty() {
                    1.0 / 60.0
                } else {
                    // Trimmed mean of recent frame times (ignores dropped frames and
                    // timing spikes), then snap to a standard refresh rate when close.
                    let mut v: Vec<f32> = self.recent.iter().copied().collect();
                    v.sort_by(|a, b| a.partial_cmp(b).unwrap());
                    let cut = v.len() / 10;
                    let keep = &v[cut..v.len() - cut];
                    let mean = keep.iter().sum::<f32>() / keep.len().max(1) as f32;
                    let est_hz = 1.0 / mean.clamp(1.0 / 1000.0, 0.1);
                    const RATES: [f32; 16] =
                        [50.0, 59.94, 60.0, 72.0, 75.0, 90.0, 100.0, 119.88, 120.0, 144.0, 165.0, 180.0, 200.0, 240.0, 360.0, 480.0];
                    // stay locked to the current standard rate unless timing clearly moved away
                    if let Some(l) = self.locked_hz {
                        if (est_hz / l - 1.0).abs() < 0.12 {
                            return self.advance(real_dt, 1.0 / l, slow, false);
                        }
                        self.locked_hz = None;
                    }
                    let mut best = est_hz;
                    let mut best_err = 0.06;
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
        // whole refresh periods only (more than one after a dropped frame)
        let steps = if forced { 1.0 } else { (real_dt / period).round().clamp(1.0, 8.0) };
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
