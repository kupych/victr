//! Every tunable knob of the simulated television, plus presets.

use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Pattern {
    DemoScene,
    ColorBars,
    TestChart,
    Crosshatch,
    GrayRamp,
    MotionTest,
    FlatField,
    Image,
    Video,
}

impl Pattern {
    pub const ALL: [Pattern; 9] = [
        Pattern::DemoScene,
        Pattern::ColorBars,
        Pattern::TestChart,
        Pattern::Crosshatch,
        Pattern::GrayRamp,
        Pattern::MotionTest,
        Pattern::FlatField,
        Pattern::Image,
        Pattern::Video,
    ];
    pub fn label(self) -> &'static str {
        match self {
            Pattern::DemoScene => "Animated demo scene",
            Pattern::ColorBars => "Color bars",
            Pattern::TestChart => "Resolution test chart",
            Pattern::Crosshatch => "Convergence crosshatch",
            Pattern::GrayRamp => "Gray ramp + PLUGE",
            Pattern::MotionTest => "Motion / persistence test",
            Pattern::FlatField => "Flat field (purity)",
            Pattern::Image => "Your picture",
            Pattern::Video => "Video through mpv",
        }
    }
    pub fn id(self) -> i32 {
        match self {
            Pattern::DemoScene => 3,
            Pattern::ColorBars => 0,
            Pattern::TestChart => 2,
            Pattern::Crosshatch => 1,
            Pattern::GrayRamp => 5,
            Pattern::MotionTest => 4,
            Pattern::FlatField => 6,
            Pattern::Image => 7,
            Pattern::Video => 7,
        }
    }
    pub fn as_key(self) -> &'static str {
        match self {
            Pattern::DemoScene => "demo_scene",
            Pattern::ColorBars => "color_bars",
            Pattern::TestChart => "test_chart",
            Pattern::Crosshatch => "crosshatch",
            Pattern::GrayRamp => "gray_ramp",
            Pattern::MotionTest => "motion_test",
            Pattern::FlatField => "flat_field",
            Pattern::Image => "image",
            Pattern::Video => "video",
        }
    }
    pub fn from_key(s: &str) -> Option<Self> {
        match s {
            "demo_scene" => Some(Pattern::DemoScene),
            "color_bars" => Some(Pattern::ColorBars),
            "test_chart" => Some(Pattern::TestChart),
            "crosshatch" => Some(Pattern::Crosshatch),
            "gray_ramp" => Some(Pattern::GrayRamp),
            "motion_test" => Some(Pattern::MotionTest),
            "flat_field" => Some(Pattern::FlatField),
            "image" => Some(Pattern::Image),
            "video" => Some(Pattern::Video),
            _ => None,
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Input {
    Rf,
    Composite,
    SVideo,
    Component,
}

impl Input {
    pub const ALL: [Input; 4] = [Input::Rf, Input::Composite, Input::SVideo, Input::Component];
    pub fn label(self) -> &'static str {
        match self {
            Input::Rf => "RF antenna (broadcast)",
            Input::Composite => "Composite video",
            Input::SVideo => "S-Video (Y/C)",
            Input::Component => "Component / RGB",
        }
    }
    pub fn id(self) -> i32 {
        match self {
            Input::Rf => 0,
            Input::Composite => 1,
            Input::SVideo => 2,
            Input::Component => 3,
        }
    }
    pub fn as_key(self) -> &'static str {
        match self {
            Input::Rf => "rf",
            Input::Composite => "composite",
            Input::SVideo => "svideo",
            Input::Component => "component",
        }
    }
    pub fn from_key(s: &str) -> Option<Self> {
        match s {
            "rf" => Some(Input::Rf),
            "composite" => Some(Input::Composite),
            "svideo" => Some(Input::SVideo),
            "component" => Some(Input::Component),
            _ => None,
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Mask {
    ApertureGrille,
    SlotMask,
    ShadowMask,
}

impl Mask {
    pub const ALL: [Mask; 3] = [Mask::ShadowMask, Mask::SlotMask, Mask::ApertureGrille];
    pub fn label(self) -> &'static str {
        match self {
            Mask::ApertureGrille => "Aperture grille (stripes)",
            Mask::SlotMask => "Slot mask",
            Mask::ShadowMask => "Shadow mask (dot triads)",
        }
    }
    pub fn id(self) -> i32 {
        match self {
            Mask::ApertureGrille => 0,
            Mask::SlotMask => 1,
            Mask::ShadowMask => 2,
        }
    }
    pub fn as_key(self) -> &'static str {
        match self {
            Mask::ApertureGrille => "aperture_grille",
            Mask::SlotMask => "slot_mask",
            Mask::ShadowMask => "shadow_mask",
        }
    }
    pub fn from_key(s: &str) -> Option<Self> {
        match s {
            "aperture_grille" => Some(Mask::ApertureGrille),
            "slot_mask" => Some(Mask::SlotMask),
            "shadow_mask" => Some(Mask::ShadowMask),
            _ => None,
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum TapeSpeed {
    Sp,
    Lp,
    Ep,
}

impl TapeSpeed {
    pub fn as_key(self) -> &'static str {
        match self {
            TapeSpeed::Sp => "sp",
            TapeSpeed::Lp => "lp",
            TapeSpeed::Ep => "ep",
        }
    }
    pub fn from_key(s: &str) -> Option<Self> {
        match s {
            "sp" => Some(TapeSpeed::Sp),
            "lp" => Some(TapeSpeed::Lp),
            "ep" => Some(TapeSpeed::Ep),
            _ => None,
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum PhosphorSet {
    P22,
    Ebu,
    Vivid,
}

impl PhosphorSet {
    pub const ALL: [PhosphorSet; 3] = [PhosphorSet::P22, PhosphorSet::Ebu, PhosphorSet::Vivid];
    pub fn label(self) -> &'static str {
        match self {
            PhosphorSet::P22 => "P22 (North American TV)",
            PhosphorSet::Ebu => "EBU (European)",
            PhosphorSet::Vivid => "Vivid (late-model, saturated)",
        }
    }
    pub fn as_key(self) -> &'static str {
        match self {
            PhosphorSet::P22 => "p22",
            PhosphorSet::Ebu => "ebu",
            PhosphorSet::Vivid => "vivid",
        }
    }
    pub fn from_key(s: &str) -> Option<Self> {
        match s {
            "p22" => Some(PhosphorSet::P22),
            "ebu" => Some(PhosphorSet::Ebu),
            "vivid" => Some(PhosphorSet::Vivid),
            _ => None,
        }
    }
    /// Columns: emission of the red, green and blue phosphor in linear sRGB.
    pub fn matrix(self) -> [f32; 9] {
        match self {
            // SMPTE-C / P22-like primaries expressed in linear sRGB.
            PhosphorSet::P22 => [
                0.9395, 0.0178, -0.0016, // red phosphor
                0.0502, 0.9658, -0.0044, // green phosphor
                0.0102, 0.0164, 1.0060, // blue phosphor
            ],
            PhosphorSet::Ebu => [
                1.0440, -0.0000, -0.0000, //
                -0.0440, 1.0000, 0.0118, //
                0.0000, 0.0000, 0.9882,
            ],
            PhosphorSet::Vivid => [
                1.12, -0.04, -0.01, //
                -0.10, 1.08, -0.05, //
                -0.02, -0.04, 1.06,
            ],
        }
    }
}

#[derive(Clone, Debug)]
pub struct Settings {
    // ---- source
    pub pattern: Pattern,
    pub flat_color: [f32; 3],
    pub interlaced: bool,

    // ---- signal path
    pub input: Input,
    pub signal_db: f32,
    pub fine_tune: f32,
    pub ghost_amp: f32,
    pub ghost_delay_us: f32,
    pub interference: f32,
    pub interference_freq: f32,
    pub hum: f32,
    pub impulse: f32,
    pub extra_noise: f32,

    // ---- VCR
    pub vcr: bool,
    pub tape_speed: TapeSpeed,
    pub tracking: f32,
    pub tape_wear: f32,
    pub dropouts: f32,
    pub head_switch: f32,
    pub timebase_jitter: f32,
    pub chroma_delay: f32,
    pub edge_enhance: f32,

    // ---- receiver
    pub brightness: f32,
    pub contrast: f32,
    pub color: f32,
    pub tint: f32,
    pub sharpness: f32,
    pub comb_filter: bool,
    pub h_hold: f32,
    pub v_hold: f32,
    pub afc_speed: f32,
    pub color_killer: bool,

    // ---- tube
    pub screen_inches: f32,
    pub mask: Mask,
    pub pitch_mm: f32,
    pub mask_strength: f32,
    pub anti_moire: f32,
    pub black_matrix: f32,
    pub curvature: f32,
    pub corner: f32,
    pub overscan: f32,
    pub h_size: f32,
    pub v_size: f32,
    pub h_pos: f32,
    pub v_pos: f32,
    pub pincushion: f32,
    pub spot: f32,
    pub spot_h: f32,
    pub bloom: f32,
    pub gamma: f32,
    pub color_temp: f32,
    pub phosphor: PhosphorSet,
    pub persistence: f32,
    pub persistence_ms: f32,
    pub phosphor_sat: f32,
    pub conv_r: [f32; 2],
    pub conv_b: [f32; 2],
    pub conv_dyn: f32,
    pub abl: f32,
    pub breathing: f32,

    // ---- aging
    pub age_hours: f32,
    pub wear: [f32; 3],
    pub focus_loss: f32,
    pub cutoff_drift: f32,
    pub magnetization: f32,
    pub glass_yellow: f32,
    pub ripple: f32,
    pub burn_speed: f32,
    pub burn_visible: f32,
    pub chaos: bool,
    pub chaos_rate: f32,

    // ---- room / glass / eye
    pub ambient: f32,
    pub reflection: f32,
    pub halation: f32,
    pub diffusion: f32,
    pub vignette: f32,
    pub exposure: f32,
    pub eye_ms: f32,
    pub show_bezel: bool,
    pub hdr_mode: bool,
    pub hdr_paper_white: f32,
    pub hdr_peak_nits: f32,
    pub hdr_ui_nits: f32,

    // ---- rolling scan / flicker
    pub rolling_scan: bool,
    pub slow_mo: f32,
    pub decay_ms: f32,
    pub scan_exposure: f32,

    // ---- sound
    pub sound: bool,
    pub volume: f32,
    pub tv_speaker: bool,
    pub mono: bool,
    pub whine: f32,
    pub test_tone: bool,
    pub audio_sync_ms: f32,
}

impl Default for Settings {
    fn default() -> Self {
        let mut s = Settings {
            pattern: Pattern::DemoScene,
            flat_color: [1.0, 1.0, 1.0],
            interlaced: false,

            input: Input::Composite,
            signal_db: 42.0,
            fine_tune: 0.0,
            ghost_amp: 0.0,
            ghost_delay_us: 1.8,
            interference: 0.0,
            interference_freq: 0.37,
            hum: 0.0,
            impulse: 0.0,
            extra_noise: 0.004,

            vcr: false,
            tape_speed: TapeSpeed::Sp,
            tracking: 0.0,
            tape_wear: 0.3,
            dropouts: 0.2,
            head_switch: 0.6,
            timebase_jitter: 0.4,
            chroma_delay: 3.0,
            edge_enhance: 0.5,

            brightness: 0.0,
            contrast: 1.0,
            color: 1.0,
            tint: 0.0,
            sharpness: 0.25,
            comb_filter: false,
            h_hold: 0.0,
            v_hold: 0.0,
            afc_speed: 0.25,
            color_killer: true,

            screen_inches: 14.0,
            mask: Mask::SlotMask,
            pitch_mm: 0.62,
            mask_strength: 1.0,
            anti_moire: 1.0,
            black_matrix: 0.25,
            curvature: 0.035,
            corner: 0.10,
            overscan: 0.06,
            h_size: 1.0,
            v_size: 1.0,
            h_pos: 0.0,
            v_pos: 0.0,
            pincushion: 0.15,
            spot: 0.26,
            spot_h: 1.2,
            bloom: 0.35,
            gamma: 2.4,
            color_temp: 6500.0,
            phosphor: PhosphorSet::P22,
            persistence: 0.12,
            persistence_ms: 18.0,
            phosphor_sat: 0.08,
            conv_r: [0.0, 0.0],
            conv_b: [0.0, 0.0],
            conv_dyn: 0.0,
            abl: 0.3,
            breathing: 0.15,

            age_hours: 0.0,
            wear: [0.0; 3],
            focus_loss: 0.0,
            cutoff_drift: 0.0,
            magnetization: 0.0,
            glass_yellow: 0.0,
            ripple: 0.0,
            burn_speed: 0.0,
            burn_visible: 1.0,
            chaos: false,
            chaos_rate: 0.5,

            ambient: 0.25,
            reflection: 0.5,
            halation: 0.10,
            diffusion: 0.18,
            vignette: 0.35,
            exposure: 1.0,
            eye_ms: 9.0,
            show_bezel: true,
            hdr_mode: false,
            hdr_paper_white: 200.0,
            hdr_peak_nits: 1000.0,
            hdr_ui_nits: 160.0,

            rolling_scan: true,
            slow_mo: 1.0,
            decay_ms: 0.25,
            scan_exposure: 1.0,

            sound: true,
            volume: 0.5,
            tv_speaker: true,
            mono: true,
            whine: 0.25,
            test_tone: false,
            audio_sync_ms: 0.0,
        };
        s.apply_age(2000.0);
        s
    }
}

impl Settings {
    /// Sets all the individual wear parameters from a single "hours of use" figure.
    pub fn apply_age(&mut self, hours: f32) {
        self.age_hours = hours;
        let a = (hours / 40000.0).clamp(0.0, 1.0);
        // Cathodes wear unevenly: red cathodes typically run hardest in TV use.
        self.wear = [a * 0.85, a * 0.55, a * 0.70];
        self.focus_loss = a * 0.9;
        self.cutoff_drift = a * 0.6;
        self.glass_yellow = a * 0.7;
        self.ripple = (a - 0.4).max(0.0) * 1.2;
        self.conv_dyn = a * 1.4;
        self.conv_r = [a * 0.7, -a * 0.3];
        self.conv_b = [-a * 0.5, a * 0.35];
    }

    pub fn samples_per_us() -> f32 {
        14.318_18
    }
}

pub const PRESETS: [&str; 9] = [
    "Retro console on a home TV",
    "Studio broadcast monitor",
    "Arcade cabinet (burned in)",
    "Weak rural antenna",
    "Worn VHS rental",
    "Dying old set",
    "No signal (snow)",
    "Late-90s aperture-grille TV",
    "Slow-motion beam (high-speed camera)",
];

pub fn preset(index: usize) -> Settings {
    let mut s = Settings::default();
    match index {
        0 => {}
        1 => {
            s.input = Input::Component;
            s.pattern = Pattern::ColorBars;
            s.interlaced = true;
            s.mask = Mask::ApertureGrille;
            s.pitch_mm = 0.30;
            s.screen_inches = 14.0;
            s.curvature = 0.012;
            s.corner = 0.03;
            s.pincushion = 0.0;
            s.overscan = 0.0;
            s.spot = 0.26;
            s.bloom = 0.2;
            s.color_temp = 6500.0;
            s.phosphor = PhosphorSet::Ebu;
            s.sharpness = 0.0;
            s.extra_noise = 0.0;
            s.apply_age(300.0);
            s.ambient = 0.1;
            s.halation = 0.06;
            s.abl = 0.1;
            s.breathing = 0.03;
        }
        2 => {
            s.input = Input::Component;
            s.mask = Mask::ShadowMask;
            s.pitch_mm = 0.75;
            s.screen_inches = 19.0;
            s.curvature = 0.05;
            s.corner = 0.12;
            s.spot = 0.30;
            s.bloom = 0.6;
            s.apply_age(22000.0);
            s.burn_speed = 0.0;
            s.burn_visible = 1.0;
            s.color_temp = 6800.0;
            s.halation = 0.16;
            s.ambient = 0.08;
        }
        3 => {
            s.input = Input::Rf;
            s.interlaced = true;
            s.pattern = Pattern::TestChart;
            s.signal_db = 17.0;
            s.ghost_amp = 0.28;
            s.ghost_delay_us = 2.4;
            s.interference = 0.04;
            s.hum = 0.25;
            s.impulse = 0.15;
            s.fine_tune = 0.1;
            s.screen_inches = 19.0;
            s.mask = Mask::ShadowMask;
            s.pitch_mm = 0.7;
            s.apply_age(9000.0);
        }
        4 => {
            s.input = Input::Composite;
            s.interlaced = true;
            s.vcr = true;
            s.tape_speed = TapeSpeed::Ep;
            s.tracking = 0.28;
            s.tape_wear = 0.75;
            s.dropouts = 0.6;
            s.head_switch = 1.0;
            s.timebase_jitter = 0.9;
            s.chroma_delay = 4.0;
            s.edge_enhance = 0.9;
            s.screen_inches = 20.0;
            s.apply_age(6000.0);
        }
        5 => {
            s.input = Input::Rf;
            s.signal_db = 30.0;
            s.screen_inches = 20.0;
            s.mask = Mask::ShadowMask;
            s.pitch_mm = 0.72;
            s.apply_age(38000.0);
            s.magnetization = 0.55;
            s.v_hold = 0.06;
            s.h_hold = 0.0;
            s.ripple = 0.8;
            s.bloom = 0.8;
            s.ghost_amp = 0.1;
            s.color_temp = 6000.0;
        }
        6 => {
            s.input = Input::Rf;
            s.signal_db = -25.0;
            s.interlaced = true;
            s.ghost_amp = 0.0;
        }
        7 => {
            s.input = Input::SVideo;
            s.interlaced = false;
            s.mask = Mask::ApertureGrille;
            s.pitch_mm = 0.5;
            s.screen_inches = 25.0;
            s.curvature = 0.015;
            s.corner = 0.04;
            s.comb_filter = true;
            s.color_temp = 9300.0;
            s.phosphor = PhosphorSet::Vivid;
            s.apply_age(1500.0);
            s.spot = 0.3;
        }
        8 => {
            s.rolling_scan = true;
            s.slow_mo = 120.0;
            s.decay_ms = 0.6;
            s.eye_ms = 0.0;
            s.pattern = Pattern::ColorBars;
        }
        _ => {}
    }
    s
}

// ---------------------------------------------------------------- user presets

#[derive(Clone, Debug)]
pub struct UserPreset {
    pub name: String,
    pub file_name: String,
    pub settings: Settings,
}

fn parse_bool(s: &str) -> Option<bool> {
    match s {
        "true" | "1" | "yes" => Some(true),
        "false" | "0" | "no" => Some(false),
        _ => None,
    }
}

fn parse_f32_2(s: &str) -> Option<[f32; 2]> {
    let mut it = s.split(',').map(|p| p.trim().parse::<f32>().ok());
    Some([it.next()??, it.next()??])
}

fn parse_f32_3(s: &str) -> Option<[f32; 3]> {
    let mut it = s.split(',').map(|p| p.trim().parse::<f32>().ok());
    Some([it.next()??, it.next()??, it.next()??])
}

impl Settings {
    /// Serialises all knobs to a human-readable key=value preset file.
    pub fn to_preset_text(&self, name: &str) -> String {
        let clean_name = name.trim().replace(['\r', '\n'], " ");
        let mut out = String::with_capacity(2048);
        use std::fmt::Write;
        let _ = writeln!(out, "# ViCTR preset");
        let _ = writeln!(out, "name = {clean_name}");
        let _ = writeln!(out, "pattern = {}", self.pattern.as_key());
        let _ = writeln!(out, "flat_color = {}, {}, {}", self.flat_color[0], self.flat_color[1], self.flat_color[2]);
        let _ = writeln!(out, "interlaced = {}", self.interlaced);
        let _ = writeln!(out, "input = {}", self.input.as_key());
        let _ = writeln!(out, "signal_db = {}", self.signal_db);
        let _ = writeln!(out, "fine_tune = {}", self.fine_tune);
        let _ = writeln!(out, "ghost_amp = {}", self.ghost_amp);
        let _ = writeln!(out, "ghost_delay_us = {}", self.ghost_delay_us);
        let _ = writeln!(out, "interference = {}", self.interference);
        let _ = writeln!(out, "interference_freq = {}", self.interference_freq);
        let _ = writeln!(out, "hum = {}", self.hum);
        let _ = writeln!(out, "impulse = {}", self.impulse);
        let _ = writeln!(out, "extra_noise = {}", self.extra_noise);
        let _ = writeln!(out, "vcr = {}", self.vcr);
        let _ = writeln!(out, "tape_speed = {}", self.tape_speed.as_key());
        let _ = writeln!(out, "tracking = {}", self.tracking);
        let _ = writeln!(out, "tape_wear = {}", self.tape_wear);
        let _ = writeln!(out, "dropouts = {}", self.dropouts);
        let _ = writeln!(out, "head_switch = {}", self.head_switch);
        let _ = writeln!(out, "timebase_jitter = {}", self.timebase_jitter);
        let _ = writeln!(out, "chroma_delay = {}", self.chroma_delay);
        let _ = writeln!(out, "edge_enhance = {}", self.edge_enhance);
        let _ = writeln!(out, "brightness = {}", self.brightness);
        let _ = writeln!(out, "contrast = {}", self.contrast);
        let _ = writeln!(out, "color = {}", self.color);
        let _ = writeln!(out, "tint = {}", self.tint);
        let _ = writeln!(out, "sharpness = {}", self.sharpness);
        let _ = writeln!(out, "comb_filter = {}", self.comb_filter);
        let _ = writeln!(out, "h_hold = {}", self.h_hold);
        let _ = writeln!(out, "v_hold = {}", self.v_hold);
        let _ = writeln!(out, "afc_speed = {}", self.afc_speed);
        let _ = writeln!(out, "color_killer = {}", self.color_killer);
        let _ = writeln!(out, "screen_inches = {}", self.screen_inches);
        let _ = writeln!(out, "mask = {}", self.mask.as_key());
        let _ = writeln!(out, "pitch_mm = {}", self.pitch_mm);
        let _ = writeln!(out, "mask_strength = {}", self.mask_strength);
        let _ = writeln!(out, "anti_moire = {}", self.anti_moire);
        let _ = writeln!(out, "black_matrix = {}", self.black_matrix);
        let _ = writeln!(out, "curvature = {}", self.curvature);
        let _ = writeln!(out, "corner = {}", self.corner);
        let _ = writeln!(out, "overscan = {}", self.overscan);
        let _ = writeln!(out, "h_size = {}", self.h_size);
        let _ = writeln!(out, "v_size = {}", self.v_size);
        let _ = writeln!(out, "h_pos = {}", self.h_pos);
        let _ = writeln!(out, "v_pos = {}", self.v_pos);
        let _ = writeln!(out, "pincushion = {}", self.pincushion);
        let _ = writeln!(out, "spot = {}", self.spot);
        let _ = writeln!(out, "spot_h = {}", self.spot_h);
        let _ = writeln!(out, "bloom = {}", self.bloom);
        let _ = writeln!(out, "gamma = {}", self.gamma);
        let _ = writeln!(out, "color_temp = {}", self.color_temp);
        let _ = writeln!(out, "phosphor = {}", self.phosphor.as_key());
        let _ = writeln!(out, "persistence = {}", self.persistence);
        let _ = writeln!(out, "persistence_ms = {}", self.persistence_ms);
        let _ = writeln!(out, "phosphor_sat = {}", self.phosphor_sat);
        let _ = writeln!(out, "conv_r = {}, {}", self.conv_r[0], self.conv_r[1]);
        let _ = writeln!(out, "conv_b = {}, {}", self.conv_b[0], self.conv_b[1]);
        let _ = writeln!(out, "conv_dyn = {}", self.conv_dyn);
        let _ = writeln!(out, "abl = {}", self.abl);
        let _ = writeln!(out, "breathing = {}", self.breathing);
        let _ = writeln!(out, "age_hours = {}", self.age_hours);
        let _ = writeln!(out, "wear = {}, {}, {}", self.wear[0], self.wear[1], self.wear[2]);
        let _ = writeln!(out, "focus_loss = {}", self.focus_loss);
        let _ = writeln!(out, "cutoff_drift = {}", self.cutoff_drift);
        let _ = writeln!(out, "magnetization = {}", self.magnetization);
        let _ = writeln!(out, "glass_yellow = {}", self.glass_yellow);
        let _ = writeln!(out, "ripple = {}", self.ripple);
        let _ = writeln!(out, "burn_speed = {}", self.burn_speed);
        let _ = writeln!(out, "burn_visible = {}", self.burn_visible);
        let _ = writeln!(out, "chaos = {}", self.chaos);
        let _ = writeln!(out, "chaos_rate = {}", self.chaos_rate);
        let _ = writeln!(out, "ambient = {}", self.ambient);
        let _ = writeln!(out, "reflection = {}", self.reflection);
        let _ = writeln!(out, "halation = {}", self.halation);
        let _ = writeln!(out, "diffusion = {}", self.diffusion);
        let _ = writeln!(out, "vignette = {}", self.vignette);
        let _ = writeln!(out, "exposure = {}", self.exposure);
        let _ = writeln!(out, "eye_ms = {}", self.eye_ms);
        let _ = writeln!(out, "show_bezel = {}", self.show_bezel);
        let _ = writeln!(out, "hdr_mode = {}", self.hdr_mode);
        let _ = writeln!(out, "hdr_paper_white = {}", self.hdr_paper_white);
        let _ = writeln!(out, "hdr_peak_nits = {}", self.hdr_peak_nits);
        let _ = writeln!(out, "hdr_ui_nits = {}", self.hdr_ui_nits);
        let _ = writeln!(out, "rolling_scan = {}", self.rolling_scan);
        let _ = writeln!(out, "slow_mo = {}", self.slow_mo);
        let _ = writeln!(out, "decay_ms = {}", self.decay_ms);
        let _ = writeln!(out, "scan_exposure = {}", self.scan_exposure);
        let _ = writeln!(out, "sound = {}", self.sound);
        let _ = writeln!(out, "volume = {}", self.volume);
        let _ = writeln!(out, "tv_speaker = {}", self.tv_speaker);
        let _ = writeln!(out, "mono = {}", self.mono);
        let _ = writeln!(out, "whine = {}", self.whine);
        let _ = writeln!(out, "test_tone = {}", self.test_tone);
        let _ = writeln!(out, "audio_sync_ms = {}", self.audio_sync_ms);
        out
    }

    /// Parses a key=value preset file. Missing keys fall back to defaults.
    pub fn from_preset_text(text: &str) -> Option<(String, Settings)> {
        let mut s = Settings::default();
        let mut name: Option<String> = None;
        for raw_line in text.lines() {
            let line = raw_line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let Some((k, v)) = line.split_once('=') else { continue };
            let k = k.trim();
            let v = v.trim();
            match k {
                "name" => {
                    if !v.is_empty() {
                        name = Some(v.to_string());
                    }
                }
                "pattern" => if let Some(x) = Pattern::from_key(v) { s.pattern = x; },
                "flat_color" => if let Some(x) = parse_f32_3(v) { s.flat_color = x; },
                "interlaced" => if let Some(x) = parse_bool(v) { s.interlaced = x; },
                "input" => if let Some(x) = Input::from_key(v) { s.input = x; },
                "signal_db" => if let Ok(x) = v.parse() { s.signal_db = x; },
                "fine_tune" => if let Ok(x) = v.parse() { s.fine_tune = x; },
                "ghost_amp" => if let Ok(x) = v.parse() { s.ghost_amp = x; },
                "ghost_delay_us" => if let Ok(x) = v.parse() { s.ghost_delay_us = x; },
                "interference" => if let Ok(x) = v.parse() { s.interference = x; },
                "interference_freq" => if let Ok(x) = v.parse() { s.interference_freq = x; },
                "hum" => if let Ok(x) = v.parse() { s.hum = x; },
                "impulse" => if let Ok(x) = v.parse() { s.impulse = x; },
                "extra_noise" => if let Ok(x) = v.parse() { s.extra_noise = x; },
                "vcr" => if let Some(x) = parse_bool(v) { s.vcr = x; },
                "tape_speed" => if let Some(x) = TapeSpeed::from_key(v) { s.tape_speed = x; },
                "tracking" => if let Ok(x) = v.parse() { s.tracking = x; },
                "tape_wear" => if let Ok(x) = v.parse() { s.tape_wear = x; },
                "dropouts" => if let Ok(x) = v.parse() { s.dropouts = x; },
                "head_switch" => if let Ok(x) = v.parse() { s.head_switch = x; },
                "timebase_jitter" => if let Ok(x) = v.parse() { s.timebase_jitter = x; },
                "chroma_delay" => if let Ok(x) = v.parse() { s.chroma_delay = x; },
                "edge_enhance" => if let Ok(x) = v.parse() { s.edge_enhance = x; },
                "brightness" => if let Ok(x) = v.parse() { s.brightness = x; },
                "contrast" => if let Ok(x) = v.parse() { s.contrast = x; },
                "color" => if let Ok(x) = v.parse() { s.color = x; },
                "tint" => if let Ok(x) = v.parse() { s.tint = x; },
                "sharpness" => if let Ok(x) = v.parse() { s.sharpness = x; },
                "comb_filter" => if let Some(x) = parse_bool(v) { s.comb_filter = x; },
                "h_hold" => if let Ok(x) = v.parse() { s.h_hold = x; },
                "v_hold" => if let Ok(x) = v.parse() { s.v_hold = x; },
                "afc_speed" => if let Ok(x) = v.parse() { s.afc_speed = x; },
                "color_killer" => if let Some(x) = parse_bool(v) { s.color_killer = x; },
                "screen_inches" => if let Ok(x) = v.parse() { s.screen_inches = x; },
                "mask" => if let Some(x) = Mask::from_key(v) { s.mask = x; },
                "pitch_mm" => if let Ok(x) = v.parse() { s.pitch_mm = x; },
                "mask_strength" => if let Ok(x) = v.parse() { s.mask_strength = x; },
                "anti_moire" => if let Ok(x) = v.parse() { s.anti_moire = x; },
                "black_matrix" => if let Ok(x) = v.parse() { s.black_matrix = x; },
                "curvature" => if let Ok(x) = v.parse() { s.curvature = x; },
                "corner" => if let Ok(x) = v.parse() { s.corner = x; },
                "overscan" => if let Ok(x) = v.parse() { s.overscan = x; },
                "h_size" => if let Ok(x) = v.parse() { s.h_size = x; },
                "v_size" => if let Ok(x) = v.parse() { s.v_size = x; },
                "h_pos" => if let Ok(x) = v.parse() { s.h_pos = x; },
                "v_pos" => if let Ok(x) = v.parse() { s.v_pos = x; },
                "pincushion" => if let Ok(x) = v.parse() { s.pincushion = x; },
                "spot" => if let Ok(x) = v.parse() { s.spot = x; },
                "spot_h" => if let Ok(x) = v.parse() { s.spot_h = x; },
                "bloom" => if let Ok(x) = v.parse() { s.bloom = x; },
                "gamma" => if let Ok(x) = v.parse() { s.gamma = x; },
                "color_temp" => if let Ok(x) = v.parse() { s.color_temp = x; },
                "phosphor" => if let Some(x) = PhosphorSet::from_key(v) { s.phosphor = x; },
                "persistence" => if let Ok(x) = v.parse() { s.persistence = x; },
                "persistence_ms" => if let Ok(x) = v.parse() { s.persistence_ms = x; },
                "phosphor_sat" => if let Ok(x) = v.parse() { s.phosphor_sat = x; },
                "conv_r" => if let Some(x) = parse_f32_2(v) { s.conv_r = x; },
                "conv_b" => if let Some(x) = parse_f32_2(v) { s.conv_b = x; },
                "conv_dyn" => if let Ok(x) = v.parse() { s.conv_dyn = x; },
                "abl" => if let Ok(x) = v.parse() { s.abl = x; },
                "breathing" => if let Ok(x) = v.parse() { s.breathing = x; },
                "age_hours" => if let Ok(x) = v.parse() { s.age_hours = x; },
                "wear" => if let Some(x) = parse_f32_3(v) { s.wear = x; },
                "focus_loss" => if let Ok(x) = v.parse() { s.focus_loss = x; },
                "cutoff_drift" => if let Ok(x) = v.parse() { s.cutoff_drift = x; },
                "magnetization" => if let Ok(x) = v.parse() { s.magnetization = x; },
                "glass_yellow" => if let Ok(x) = v.parse() { s.glass_yellow = x; },
                "ripple" => if let Ok(x) = v.parse() { s.ripple = x; },
                "burn_speed" => if let Ok(x) = v.parse() { s.burn_speed = x; },
                "burn_visible" => if let Ok(x) = v.parse() { s.burn_visible = x; },
                "chaos" => if let Some(x) = parse_bool(v) { s.chaos = x; },
                "chaos_rate" => if let Ok(x) = v.parse() { s.chaos_rate = x; },
                "ambient" => if let Ok(x) = v.parse() { s.ambient = x; },
                "reflection" => if let Ok(x) = v.parse() { s.reflection = x; },
                "halation" => if let Ok(x) = v.parse() { s.halation = x; },
                "diffusion" => if let Ok(x) = v.parse() { s.diffusion = x; },
                "vignette" => if let Ok(x) = v.parse() { s.vignette = x; },
                "exposure" => if let Ok(x) = v.parse() { s.exposure = x; },
                "eye_ms" => if let Ok(x) = v.parse() { s.eye_ms = x; },
                "show_bezel" => if let Some(x) = parse_bool(v) { s.show_bezel = x; },
                "hdr_mode" => if let Some(x) = parse_bool(v) { s.hdr_mode = x; },
                "hdr_paper_white" => if let Ok(x) = v.parse() { s.hdr_paper_white = x; },
                "hdr_peak_nits" => if let Ok(x) = v.parse() { s.hdr_peak_nits = x; },
                "hdr_ui_nits" => if let Ok(x) = v.parse() { s.hdr_ui_nits = x; },
                "rolling_scan" => if let Some(x) = parse_bool(v) { s.rolling_scan = x; },
                "slow_mo" => if let Ok(x) = v.parse() { s.slow_mo = x; },
                "decay_ms" => if let Ok(x) = v.parse() { s.decay_ms = x; },
                "scan_exposure" => if let Ok(x) = v.parse() { s.scan_exposure = x; },
                "sound" => if let Some(x) = parse_bool(v) { s.sound = x; },
                "volume" => if let Ok(x) = v.parse() { s.volume = x; },
                "tv_speaker" => if let Some(x) = parse_bool(v) { s.tv_speaker = x; },
                "mono" => if let Some(x) = parse_bool(v) { s.mono = x; },
                "whine" => if let Ok(x) = v.parse() { s.whine = x; },
                "test_tone" => if let Some(x) = parse_bool(v) { s.test_tone = x; },
                "audio_sync_ms" => if let Ok(x) = v.parse() { s.audio_sync_ms = x; },
                _ => {}
            }
        }
        Some((name?, s))
    }
}

pub fn presets_dir() -> Option<std::path::PathBuf> {
    #[cfg(target_os = "windows")]
    {
        return std::env::var_os("APPDATA").map(|p| std::path::PathBuf::from(p).join("victr").join("presets"));
    }
    #[cfg(target_os = "macos")]
    {
        return std::env::var_os("HOME").map(|p| std::path::PathBuf::from(p).join("Library/Application Support/victr/presets"));
    }
    #[cfg(not(any(target_os = "windows", target_os = "macos")))]
    {
        if let Some(xdg) = std::env::var_os("XDG_CONFIG_HOME").filter(|v| !v.is_empty()) {
            return Some(std::path::PathBuf::from(xdg).join("victr").join("presets"));
        }
        std::env::var_os("HOME").map(|p| std::path::PathBuf::from(p).join(".config/victr/presets"))
    }
}

fn slugify(name: &str) -> String {
    let mut out = String::new();
    let mut last_dash = false;
    for ch in name.chars() {
        if ch.is_ascii_alphanumeric() {
            out.push(ch.to_ascii_lowercase());
            last_dash = false;
        } else if !last_dash && !out.is_empty() {
            out.push('-');
            last_dash = true;
        }
    }
    let trimmed = out.trim_matches('-');
    if trimmed.is_empty() { "preset".to_string() } else { trimmed.to_string() }
}

pub fn load_user_presets_from(dir: &std::path::Path) -> Vec<UserPreset> {
    let Ok(entries) = std::fs::read_dir(dir) else { return Vec::new() };
    let mut out = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("preset") {
            continue;
        }
        let Some(file_name) = path.file_name().and_then(|n| n.to_str()).map(|s| s.to_string()) else {
            continue;
        };
        let Ok(text) = std::fs::read_to_string(&path) else { continue };
        if let Some((name, settings)) = Settings::from_preset_text(&text) {
            out.push(UserPreset { name, file_name, settings });
        }
    }
    out.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()));
    out
}

pub fn load_user_presets() -> Vec<UserPreset> {
    presets_dir().map(|d| load_user_presets_from(&d)).unwrap_or_default()
}

pub fn save_user_preset_in(dir: &std::path::Path, name: &str, settings: &Settings) -> Result<UserPreset, String> {
    let clean = name.trim().replace(['\r', '\n'], " ");
    if clean.is_empty() {
        return Err("Enter a name for the preset.".into());
    }
    std::fs::create_dir_all(dir).map_err(|e| format!("Couldn't create presets folder: {e}"))?;

    // If a user preset with the same name already exists, update its file in place.
    let existing = load_user_presets_from(dir);
    let file_name = if let Some(prev) = existing.iter().find(|p| p.name.eq_ignore_ascii_case(&clean)) {
        prev.file_name.clone()
    } else {
        let base = slugify(&clean);
        let mut candidate = format!("{base}.preset");
        let mut n = 2u32;
        while dir.join(&candidate).exists() {
            candidate = format!("{base}-{n}.preset");
            n += 1;
        }
        candidate
    };

    let text = settings.to_preset_text(&clean);
    std::fs::write(dir.join(&file_name), text).map_err(|e| format!("Couldn't save preset: {e}"))?;
    Ok(UserPreset { name: clean, file_name, settings: settings.clone() })
}

pub fn save_user_preset(name: &str, settings: &Settings) -> Result<UserPreset, String> {
    let dir = presets_dir().ok_or_else(|| "Couldn't locate a config folder for presets.".to_string())?;
    save_user_preset_in(&dir, name, settings)
}

pub fn delete_user_preset_in(dir: &std::path::Path, file_name: &str) -> Result<(), String> {
    if file_name.contains('/') || file_name.contains('\\') || file_name.contains("..") {
        return Err("Invalid preset file name.".into());
    }
    let path = dir.join(file_name);
    if path.exists() {
        std::fs::remove_file(&path).map_err(|e| format!("Couldn't delete preset: {e}"))?;
    }
    Ok(())
}

pub fn delete_user_preset(file_name: &str) -> Result<(), String> {
    let dir = presets_dir().ok_or_else(|| "Couldn't locate a config folder for presets.".to_string())?;
    delete_user_preset_in(&dir, file_name)
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "value")]
pub enum StartupPreset {
    BuiltIn(usize),
    User(String),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum StartupDestination {
    Guide,
    LastWatched,
    DefaultPattern,
}

impl Default for StartupDestination {
    fn default() -> Self {
        Self::Guide
    }
}

impl StartupDestination {
    pub const ALL: [StartupDestination; 3] = [
        StartupDestination::Guide,
        StartupDestination::LastWatched,
        StartupDestination::DefaultPattern,
    ];
    pub fn label(self) -> &'static str {
        match self {
            StartupDestination::Guide => "90s Cable Guide",
            StartupDestination::LastWatched => "Last watched",
            StartupDestination::DefaultPattern => "Default test pattern",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind")]
pub enum LastWatched {
    JellyfinSeries {
        series_id: String,
        channel_name: String,
        #[serde(default)]
        item_id: String,
    },
    JellyfinGenre {
        genre: String,
    },
    LocalVideo {
        src: String,
        #[serde(default)]
        pos_secs: f64,
    },
}

impl LastWatched {
    pub fn label(&self) -> String {
        match self {
            LastWatched::JellyfinSeries { channel_name, .. } => channel_name.clone(),
            LastWatched::JellyfinGenre { genre } => format!("{genre} Movies"),
            LastWatched::LocalVideo { src, .. } => std::path::Path::new(src)
                .file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_else(|| src.clone()),
        }
    }
}

fn default_true() -> bool {
    true
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AppConfig {
    #[serde(default)]
    pub default_preset: Option<StartupPreset>,
    #[serde(default = "default_true")]
    pub cold_power_on: bool,
    #[serde(default = "default_true")]
    pub splash_screen: bool,
    #[serde(default)]
    pub startup_dest: StartupDestination,
    #[serde(default)]
    pub last_watched: Option<LastWatched>,
}

impl Default for AppConfig {
    fn default() -> Self {
        Self {
            default_preset: None,
            cold_power_on: true,
            splash_screen: true,
            startup_dest: StartupDestination::Guide,
            last_watched: None,
        }
    }
}

pub fn app_config_path() -> Option<std::path::PathBuf> {
    presets_dir().and_then(|d| d.parent().map(|p| p.join("app.json")))
}

pub fn load_app_config_from(path: &std::path::Path) -> AppConfig {
    let Ok(text) = std::fs::read_to_string(path) else { return AppConfig::default() };
    serde_json::from_str(&text).unwrap_or_default()
}

pub fn save_app_config_to(path: &std::path::Path, cfg: &AppConfig) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("Couldn't create config folder: {e}"))?;
    }
    let json = serde_json::to_string_pretty(cfg).map_err(|e| e.to_string())?;
    std::fs::write(path, json).map_err(|e| format!("Couldn't save app config: {e}"))
}

pub fn load_app_config() -> AppConfig {
    app_config_path().map(|p| load_app_config_from(&p)).unwrap_or_default()
}

pub fn save_app_config(cfg: &AppConfig) -> Result<(), String> {
    let path = app_config_path().ok_or_else(|| "Couldn't locate config folder.".to_string())?;
    save_app_config_to(&path, cfg)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_all_settings_fields() {
        let mut s = preset(4);
        s.flat_color = [0.25, 0.5, 0.75];
        s.mask = Mask::ApertureGrille;
        s.phosphor = PhosphorSet::Vivid;
        s.conv_r = [0.42, -0.31];
        s.conv_b = [-0.18, 0.64];
        s.wear = [0.11, 0.22, 0.33];
        s.magnetization = 0.45;
        s.slow_mo = 250.0;
        s.show_bezel = false;

        let text = s.to_preset_text("My Custom VHS");
        let (name, parsed) = Settings::from_preset_text(&text).expect("preset should parse");
        assert_eq!(name, "My Custom VHS");
        assert_eq!(parsed.pattern, s.pattern);
        assert_eq!(parsed.flat_color, s.flat_color);
        assert_eq!(parsed.interlaced, s.interlaced);
        assert_eq!(parsed.input, s.input);
        assert_eq!(parsed.vcr, s.vcr);
        assert_eq!(parsed.tape_speed, s.tape_speed);
        assert_eq!(parsed.mask, s.mask);
        assert_eq!(parsed.phosphor, s.phosphor);
        assert_eq!(parsed.conv_r, s.conv_r);
        assert_eq!(parsed.conv_b, s.conv_b);
        assert_eq!(parsed.wear, s.wear);
        assert_eq!(parsed.magnetization, s.magnetization);
        assert_eq!(parsed.slow_mo, s.slow_mo);
        assert_eq!(parsed.show_bezel, s.show_bezel);
        assert_eq!(parsed.to_preset_text(&name), text);
    }

    #[test]
    fn save_load_overwrite_delete_presets() {
        let dir = std::env::temp_dir().join(format!("phosphor-presets-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);

        let mut s1 = preset(1);
        s1.color_temp = 8200.0;
        let saved = save_user_preset_in(&dir, "Sony PVM Warm", &s1).unwrap();
        assert_eq!(saved.file_name, "sony-pvm-warm.preset");

        let mut s2 = preset(5);
        s2.magnetization = 0.8;
        let saved2 = save_user_preset_in(&dir, "Attic Set", &s2).unwrap();
        assert_eq!(saved2.file_name, "attic-set.preset");

        let loaded = load_user_presets_from(&dir);
        assert_eq!(loaded.len(), 2);
        assert_eq!(loaded[0].name, "Attic Set");
        assert_eq!(loaded[1].name, "Sony PVM Warm");
        assert!((loaded[1].settings.color_temp - 8200.0).abs() < 1e-3);

        // Overwrite existing by name (case-insensitive)
        s1.color_temp = 6500.0;
        let updated = save_user_preset_in(&dir, "sony pvm warm", &s1).unwrap();
        assert_eq!(updated.file_name, "sony-pvm-warm.preset");
        let loaded = load_user_presets_from(&dir);
        assert_eq!(loaded.len(), 2);
        assert!((loaded[1].settings.color_temp - 6500.0).abs() < 1e-3);

        // Delete one
        delete_user_preset_in(&dir, &saved2.file_name).unwrap();
        let loaded = load_user_presets_from(&dir);
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].file_name, "sony-pvm-warm.preset");

        // Reject empty names
        assert!(save_user_preset_in(&dir, "   ", &s1).is_err());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn app_config_roundtrip() {
        let dir = std::env::temp_dir().join(format!("victr-appcfg-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let path = dir.join("app.json");

        let def = load_app_config_from(&path);
        assert!(def.default_preset.is_none());
        assert!(def.cold_power_on);

        let cfg = AppConfig {
            default_preset: Some(StartupPreset::User("sony-pvm-warm.preset".into())),
            cold_power_on: false,
            splash_screen: true,
            startup_dest: StartupDestination::LastWatched,
            last_watched: Some(LastWatched::JellyfinSeries {
                series_id: "simpsons-id".into(),
                channel_name: "The Simpsons".into(),
                item_id: "ep1".into(),
            }),
        };
        save_app_config_to(&path, &cfg).unwrap();
        let loaded = load_app_config_from(&path);
        assert_eq!(loaded.default_preset, Some(StartupPreset::User("sony-pvm-warm.preset".into())));
        assert!(!loaded.cold_power_on);
        assert!(loaded.splash_screen);
        assert_eq!(loaded.startup_dest, StartupDestination::LastWatched);
        assert_eq!(loaded.last_watched.as_ref().map(|w| w.label()), Some("The Simpsons".into()));

        let cfg2 = AppConfig {
            default_preset: Some(StartupPreset::BuiltIn(4)),
            cold_power_on: true,
            splash_screen: false,
            startup_dest: StartupDestination::DefaultPattern,
            last_watched: None,
        };
        save_app_config_to(&path, &cfg2).unwrap();
        let loaded2 = load_app_config_from(&path);
        assert_eq!(loaded2.default_preset, Some(StartupPreset::BuiltIn(4)));
        assert!(loaded2.cold_power_on);
        assert!(!loaded2.splash_screen);
        assert_eq!(loaded2.startup_dest, StartupDestination::DefaultPattern);

        let _ = std::fs::remove_dir_all(&dir);
    }
}
