//! Native libretro game console host for ViCTR (`CH 03` Game Input).
//!
//! Dynamically loads installed libretro cores (e.g. `/usr/lib/libretro/*.so` or
//! `~/.config/retroarch/cores/*`) with zero build-time C dependencies, runs the core
//! at native console FPS, feeds raw 240p/480i frames straight into ViCTR's CRT physics
//! pipeline, and streams resampled PCM audio directly through ViCTR's analog TV DSP.
//!
//! Features:
//! - Auto-detects installed libretro cores and maps ROM extensions (NES, SNES, Genesis/MD,
//!   Master System, Game Gear, GB/GBC/GBA, PC Engine, PlayStation, Arcade)
//! - Keyboard + native Linux `/dev/input/js0` gamepad RetroPad input (bitmask + button API)
//! - Automatic SRAM battery saves (`.srm`) and Quick Save/Load States (`F2` / `F4`)
//! - ROM folder scanner exposing cartridges on `CH 03` (`GAME`) in the 90s Prevue Cable Guide
//! - `retro://<rom_path>` URL scheme for `channels.json` per-game / per-console pinned channels

use crate::audio::{AudioRing, RATE as AUDIO_RATE};
use crate::osd::Canvas;
use crate::provider::{Channel, ChannelKind, MediaItem};
use serde::{Deserialize, Serialize};
use std::cell::RefCell;
use std::collections::HashMap;
use std::ffi::{CStr, CString, c_char, c_uint, c_void};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU16, Ordering};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

// ------------------------------------------------------------------ ROM Extensions & URL Scheme

pub const ROM_EXTS: &[&str] = &[
    "nes", "fds", "unif", "sfc", "smc", "fig", "swc", "md", "gen", "smd", "sms", "gg", "sg",
    "gba", "gb", "gbc", "pce", "chd", "cue", "iso", "zip",
];

pub fn is_rom_file(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|ext| {
            // Exclude `.zip` from drag-and-drop ambiguity unless explicitly picked, keep console extensions
            let low = ext.to_ascii_lowercase();
            matches!(
                low.as_str(),
                "nes" | "fds" | "unif" | "sfc" | "smc" | "fig" | "swc"
                    | "md" | "gen" | "smd" | "sms" | "gg" | "sg"
                    | "gba" | "gb" | "gbc" | "pce"
            )
        })
}

pub fn is_retro_url(src: &str) -> bool {
    let s = src.trim();
    s.len() >= 8 && s[..8].eq_ignore_ascii_case("retro://")
}

/// Parses `retro:///path/to/game.sfc?core=bsnes` into `(rom_path, optional_core_hint)`.
pub fn parse_retro_url(src: &str) -> (String, Option<String>) {
    let s = src.trim();
    let rest = if is_retro_url(s) { &s[8..] } else { s };
    if let Some((path, query)) = rest.split_once("?core=") {
        let core = query.split('&').next().unwrap_or("").trim();
        let hint = if core.is_empty() { None } else { Some(core.to_string()) };
        (path.to_string(), hint)
    } else {
        (rest.to_string(), None)
    }
}

pub fn make_retro_url(rom_path: &str, core_hint: Option<&str>) -> String {
    match core_hint.map(str::trim).filter(|s| !s.is_empty()) {
        Some(c) => format!("retro://{rom_path}?core={c}"),
        None => format!("retro://{rom_path}"),
    }
}

// ------------------------------------------------------------------ Config & Persistence

fn default_channel_num() -> u16 {
    3
}

fn default_true() -> bool {
    true
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RetroConfig {
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default = "default_channel_num")]
    pub channel_number: u16,
    #[serde(default)]
    pub rom_dirs: Vec<String>,
    #[serde(default)]
    pub custom_core_dir: String,
    #[serde(default)]
    pub preferred_cores: HashMap<String, String>,
    #[serde(default)]
    pub last_rom: String,
    /// Selected Linux joystick device (`""` = Auto all real gamepads, or `"js1"` / `"/dev/input/js1"`).
    #[serde(default)]
    pub gamepad_device: String,
}

impl Default for RetroConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            channel_number: default_channel_num(),
            rom_dirs: Vec::new(),
            custom_core_dir: String::new(),
            preferred_cores: HashMap::new(),
            last_rom: String::new(),
            gamepad_device: String::new(),
        }
    }
}

pub fn config_path() -> Option<PathBuf> {
    crate::settings::presets_dir().and_then(|d| d.parent().map(|p| p.join("retro.json")))
}

pub fn retro_saves_dir() -> PathBuf {
    crate::settings::presets_dir()
        .and_then(|d| d.parent().map(|p| p.join("retro").join("saves")))
        .unwrap_or_else(|| std::env::temp_dir().join("victr-retro-saves"))
}

pub fn retro_system_dir() -> PathBuf {
    // Prefer user's existing RetroArch system dir if present so BIOS files work out of the box!
    if let Some(home) = std::env::var_os("HOME") {
        let ra = PathBuf::from(home).join(".config/retroarch/system");
        if ra.is_dir() {
            return ra;
        }
    }
    crate::settings::presets_dir()
        .and_then(|d| d.parent().map(|p| p.join("retro").join("system")))
        .unwrap_or_else(|| std::env::temp_dir().join("victr-retro-system"))
}

pub fn load_config() -> RetroConfig {
    let Some(path) = config_path() else { return RetroConfig::default() };
    let Ok(text) = std::fs::read_to_string(path) else { return RetroConfig::default() };
    serde_json::from_str(&text).unwrap_or_default()
}

pub fn save_config(cfg: &RetroConfig) -> Result<(), String> {
    let path = config_path().ok_or_else(|| "Couldn't locate config folder.".to_string())?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("Couldn't create config folder: {e}"))?;
    }
    let json = serde_json::to_string_pretty(cfg).map_err(|e| e.to_string())?;
    std::fs::write(path, json).map_err(|e| format!("Couldn't save retro config: {e}"))
}

// ------------------------------------------------------------------ Core Discovery & Matching

#[derive(Clone, Debug)]
pub struct CoreInfo {
    pub name: String,
    pub path: PathBuf,
}

fn core_file_ext() -> &'static str {
    #[cfg(target_os = "windows")]
    { "dll" }
    #[cfg(target_os = "macos")]
    { "dylib" }
    #[cfg(not(any(target_os = "windows", target_os = "macos")))]
    { "so" }
}

pub fn discover_cores(custom_dir: &str) -> Vec<CoreInfo> {
    let mut dirs: Vec<PathBuf> = Vec::new();
    if !custom_dir.trim().is_empty() {
        dirs.push(PathBuf::from(custom_dir.trim()));
    }
    if let Some(home) = std::env::var_os("HOME") {
        let h = PathBuf::from(home);
        dirs.push(h.join(".config/retroarch/cores"));
        dirs.push(h.join(".local/share/retroarch/cores"));
    }
    #[cfg(not(target_os = "windows"))]
    {
        dirs.push(PathBuf::from("/usr/lib/libretro"));
        dirs.push(PathBuf::from("/usr/lib64/libretro"));
        dirs.push(PathBuf::from("/usr/local/lib/libretro"));
        dirs.push(PathBuf::from("/usr/lib/x86_64-linux-gnu/libretro"));
    }
    #[cfg(target_os = "windows")]
    {
        if let Some(appdata) = std::env::var_os("APPDATA") {
            dirs.push(PathBuf::from(appdata).join("RetroArch\\cores"));
        }
        dirs.push(PathBuf::from(r"C:\RetroArch-Win64\cores"));
    }

    let ext_want = core_file_ext();
    let mut seen = std::collections::HashSet::new();
    let mut out = Vec::new();
    for dir in dirs {
        let Ok(entries) = std::fs::read_dir(&dir) else { continue };
        for entry in entries.flatten() {
            let p = entry.path();
            if !p.is_file() {
                continue;
            }
            if !p
                .extension()
                .and_then(|e| e.to_str())
                .is_some_and(|e| e.eq_ignore_ascii_case(ext_want))
            {
                continue;
            }
            let Some(stem) = p.file_stem().and_then(|s| s.to_str()) else { continue };
            if !stem.contains("libretro") {
                continue;
            }
            let clean = stem.strip_suffix("_libretro").unwrap_or(stem).to_string();
            if seen.insert(clean.to_ascii_lowercase()) {
                out.push(CoreInfo { name: clean, path: p });
            }
        }
    }
    out.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()));
    out
}

/// Picks the best installed libretro core for a ROM file (respecting optional hint & user overrides).
pub fn select_core_for_rom<'a>(
    rom_path: &Path,
    cores: &'a [CoreInfo],
    core_hint: Option<&str>,
    preferred: &HashMap<String, String>,
) -> Option<&'a CoreInfo> {
    if let Some(hint) = core_hint.map(str::trim).filter(|s| !s.is_empty()) {
        let h_low = hint.to_ascii_lowercase();
        if let Some(found) = cores.iter().find(|c| {
            c.name.eq_ignore_ascii_case(&h_low)
                || c.name.to_ascii_lowercase().contains(&h_low)
                || c.path.to_string_lossy().to_ascii_lowercase().contains(&h_low)
        }) {
            return Some(found);
        }
    }

    let ext = rom_path
        .extension()
        .and_then(|e| e.to_str())
        .map(|s| s.to_ascii_lowercase())
        .unwrap_or_default();

    if let Some(pref) = preferred.get(&ext) {
        if let Some(found) = cores
            .iter()
            .find(|c| c.name.eq_ignore_ascii_case(pref) || c.path.to_string_lossy().contains(pref))
        {
            return Some(found);
        }
    }

    let candidates: &[&str] = match ext.as_str() {
        "nes" | "fds" | "unif" => &["mesen", "fceumm", "nestopia", "quicknes"],
        "sfc" | "smc" | "fig" | "swc" => &["bsnes", "snes9x", "mesen-s", "bsnes_mercury"],
        "md" | "gen" | "smd" | "sms" | "gg" | "sg" => &["genesis_plus_gx", "picodrive", "blastem"],
        "gba" => &["mgba", "vba_next", "vbam", "gpsp"],
        "gb" | "gbc" => &["mgba", "gambatte", "sameboy", "tgbdual"],
        "pce" => &["mednafen_pce", "mednafen_pce_fast", "mednafen_supergrafx"],
        "chd" | "cue" | "iso" => &["mednafen_psx", "mednafen_psx_hw", "pcsx_rearmed", "swanstation", "genesis_plus_gx"],
        "zip" => &["mame", "fbneo", "fbalpha", "snes9x", "mesen"],
        _ => &[],
    };

    for want in candidates {
        if let Some(c) = cores.iter().find(|c| c.name.eq_ignore_ascii_case(want)) {
            return Some(c);
        }
    }
    for want in candidates {
        if let Some(c) = cores.iter().find(|c| c.name.to_ascii_lowercase().contains(want)) {
            return Some(c);
        }
    }
    None
}

fn console_label_for_ext(ext: &str) -> &'static str {
    match ext {
        "nes" | "fds" | "unif" => "NES",
        "sfc" | "smc" | "fig" | "swc" => "SNES",
        "md" | "gen" | "smd" => "GENESIS",
        "sms" => "SMS",
        "gg" => "GAME GEAR",
        "gba" => "GBA",
        "gb" => "GB",
        "gbc" => "GBC",
        "pce" => "PC ENGINE",
        "chd" | "cue" | "iso" => "DISC",
        _ => "ARCADE",
    }
}

// ------------------------------------------------------------------ ROM Scanner & Standby Screen

fn scan_roms_recursive(dir: &Path, depth: usize, out: &mut Vec<PathBuf>) {
    if depth > 4 {
        return;
    }
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    for entry in entries.flatten() {
        let p = entry.path();
        let name = p.file_name().and_then(|n| n.to_str()).unwrap_or("");
        if name.starts_with('.') {
            continue;
        }
        if p.is_file() {
            if let Some(ext) = p.extension().and_then(|e| e.to_str()) {
                if ROM_EXTS.iter().any(|r| r.eq_ignore_ascii_case(ext)) {
                    out.push(p);
                }
            }
        } else if p.is_dir() {
            scan_roms_recursive(&p, depth + 1, out);
        }
    }
}

pub fn build_console_channel(cfg: &RetroConfig, cores: &[CoreInfo]) -> Option<Channel> {
    if !cfg.enabled || cores.is_empty() {
        return None;
    }
    let mut rom_files = Vec::new();
    for dir in &cfg.rom_dirs {
        let p = Path::new(dir.trim());
        if p.is_dir() {
            scan_roms_recursive(p, 0, &mut rom_files);
        }
    }
    rom_files.sort();

    let mut items: Vec<MediaItem> = rom_files
        .into_iter()
        .filter_map(|p| {
            let core = select_core_for_rom(&p, cores, None, &cfg.preferred_cores)?;
            let ext = p.extension().and_then(|e| e.to_str()).unwrap_or("").to_ascii_lowercase();
            let sys = console_label_for_ext(&ext);
            let stem = p.file_stem().and_then(|s| s.to_str()).unwrap_or("Cartridge");
            let path_str = p.to_string_lossy().to_string();
            Some(MediaItem {
                id: path_str.clone(),
                title: format!("[{sys}] {stem}"),
                season: None,
                episode: None,
                overview: format!("{sys} cartridge · core: {}", core.name),
                runtime_secs: 3600.0,
                played: false,
                resume_secs: 0.0,
                artwork_id: String::new(),
                stream_url: make_retro_url(&path_str, Some(&core.name)),
            })
        })
        .collect();

    // If no ROM dirs scanned yet, include last_rom or a standby sentinel so CH 03 is always tunable!
    if items.is_empty() {
        if !cfg.last_rom.trim().is_empty() && Path::new(cfg.last_rom.trim()).is_file() {
            let p = Path::new(cfg.last_rom.trim());
            let stem = p.file_stem().and_then(|s| s.to_str()).unwrap_or("Cartridge");
            let url = make_retro_url(cfg.last_rom.trim(), None);
            items.push(MediaItem {
                id: cfg.last_rom.clone(),
                title: stem.to_string(),
                season: None,
                episode: None,
                overview: "Last played cartridge".into(),
                runtime_secs: 3600.0,
                played: false,
                resume_secs: 0.0,
                artwork_id: String::new(),
                stream_url: url,
            });
        } else {
            items.push(MediaItem {
                id: "retro:standby".into(),
                title: "INSERT CARTRIDGE".into(),
                season: None,
                episode: None,
                overview: "Drag & drop a ROM onto the TV or add ROM folders in Channels & Sources -> Game Console.".into(),
                runtime_secs: 3600.0,
                played: false,
                resume_secs: 0.0,
                artwork_id: String::new(),
                stream_url: "retro://standby".into(),
            });
        }
    }

    let next_up = if !cfg.last_rom.is_empty() && items.iter().any(|i| i.id == cfg.last_rom) {
        Some(cfg.last_rom.clone())
    } else {
        items.first().map(|i| i.id.clone())
    };
    let core_list: String = cores
        .iter()
        .take(5)
        .map(|c| c.name.as_str())
        .collect::<Vec<_>>()
        .join(", ");

    Some(Channel {
        number: cfg.channel_number.max(2),
        provider_id: "retro".into(),
        source_key: "retro:console".into(),
        callsign: "GAME".into(),
        name: "Game Console".into(),
        artwork_id: String::new(),
        channel_type: "game".into(),
        profile_override: None,
        kind: ChannelKind::Series {
            series_id: "retro:console".into(),
            overview: format!("CH {:02} RF/AV Game Input · Cores: {core_list}", cfg.channel_number.max(2)),
            is_continue: false,
            is_favorite: true,
            episodes: items,
            next_up_episode_id: next_up,
            loaded_episodes: true,
        },
    })
}

/// Renders a crisp 640×480 90s blue console-standby screen ("CH 03 GAME INPUT — INSERT CARTRIDGE").
pub fn render_standby_frame(ch_num: u16, msg: &str) -> Arc<(usize, usize, Vec<u8>)> {
    let mut c = Canvas::new(640, 480);
    c.fill_vgrad(0, 0, 640, 480, [14, 40, 152, 255], [10, 26, 114, 255]);
    let badge = format!("CH {:02}  GAME INPUT", ch_num);
    c.draw_text(44, 40, &badge, 2, 2, [96, 255, 134, 255], Some([6, 14, 48, 255]));
    c.draw_text(136, 184, "INSERT CARTRIDGE", 3, 3, [248, 212, 74, 255], Some([6, 14, 48, 255]));
    c.draw_text(
        82,
        244,
        "DRAG & DROP A ROM ONTO THE TV, PRESS E FOR ROMS,",
        1,
        2,
        [236, 244, 255, 255],
        None,
    );
    c.draw_text(
        106,
        270,
        "OR OPEN CHANNELS & SOURCES -> GAME CONSOLE",
        1,
        2,
        [182, 214, 255, 255],
        None,
    );
    if !msg.is_empty() {
        c.draw_text_clipped(60, 326, msg, 520, 1, 2, [255, 168, 132, 255], None);
    }
    c.draw_text(
        64,
        408,
        "KEYS: ARROWS=DPAD  Z/X=B/A  A/S=Y/X  Q/W=L/R  ENTER=START",
        1,
        2,
        [152, 212, 255, 255],
        None,
    );
    Arc::new((640, 480, c.rgba))
}

// ------------------------------------------------------------------ RetroPad Input Bitmask

pub const BTN_B: u16 = 1 << 0;
pub const BTN_Y: u16 = 1 << 1;
pub const BTN_SELECT: u16 = 1 << 2;
pub const BTN_START: u16 = 1 << 3;
pub const BTN_UP: u16 = 1 << 4;
pub const BTN_DOWN: u16 = 1 << 5;
pub const BTN_LEFT: u16 = 1 << 6;
pub const BTN_RIGHT: u16 = 1 << 7;
pub const BTN_A: u16 = 1 << 8;
pub const BTN_X: u16 = 1 << 9;
pub const BTN_L: u16 = 1 << 10;
pub const BTN_R: u16 = 1 << 11;

// ------------------------------------------------------------------ Native Linux `/dev/input/js*` Gamepad Discovery & Reader

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GamepadInfo {
    pub id: String,
    pub path: String,
    pub name: String,
    pub is_gamepad: bool,
}

impl GamepadInfo {
    pub fn display_label(&self) -> String {
        if self.name.is_empty() {
            self.id.clone()
        } else {
            format!("{} — {}", self.id, self.name)
        }
    }
}

fn parse_sysfs_bitset(raw: &str) -> Vec<u64> {
    raw.split_whitespace()
        .rev()
        .map(|w| u64::from_str_radix(w, 16).unwrap_or(0))
        .collect()
}

fn bitset_has(words: &[u64], bit: usize) -> bool {
    let wi = bit / 64;
    let bi = bit % 64;
    words.get(wi).is_some_and(|w| (w & (1u64 << bi)) != 0)
}

fn looks_like_non_gamepad(name: &str) -> bool {
    let low = name.to_ascii_lowercase();
    [
        "led controller",
        "rgb",
        "aura",
        "mystic light",
        "keyboard",
        "mouse",
        "touchpad",
        "trackpad",
        "accelerometer",
        "gyro",
        "sensor",
        "consumer control",
        "system control",
    ]
    .iter()
    .any(|kw| low.contains(kw))
}

fn classify_gamepad(name: &str, key_words: &[u64]) -> bool {
    if looks_like_non_gamepad(name) {
        return false;
    }
    // BTN_SOUTH / BTN_GAMEPAD (0x130) or BTN_JOYSTICK (0x120)
    bitset_has(key_words, 0x130) || bitset_has(key_words, 0x120)
}

fn is_nintendo_layout(name: &str) -> bool {
    let low = name.to_ascii_lowercase();
    low.contains("nintendo") || low.contains("pro controller") || low.contains("joy-con") || low.contains("switch")
}

/// Replicates Linux `drivers/input/joydev.c` button numbering from `capabilities/key`.
fn build_joydev_button_map(key_words: &[u64], nintendo_layout: bool) -> Vec<u16> {
    if key_words.is_empty() {
        return vec![BTN_B, BTN_A, BTN_Y, BTN_X, BTN_L, BTN_R, BTN_SELECT, BTN_START];
    }
    let map_code = |code: usize| -> u16 {
        match code {
            0x130 => BTN_B, // BTN_SOUTH
            0x131 => BTN_A, // BTN_EAST
            0x133 => {
                if nintendo_layout { BTN_X } else { BTN_Y } // BTN_NORTH (Nintendo X / Xbox X)
            }
            0x134 => {
                if nintendo_layout { BTN_Y } else { BTN_X } // BTN_WEST (Nintendo Y / Xbox Y)
            }
            0x136 | 0x138 => BTN_L, // BTN_TL / BTN_TL2
            0x137 | 0x139 => BTN_R, // BTN_TR / BTN_TR2
            0x13a => BTN_SELECT,    // BTN_SELECT
            0x13b => BTN_START,     // BTN_START
            0x220 => BTN_UP,        // BTN_DPAD_UP
            0x221 => BTN_DOWN,      // BTN_DPAD_DOWN
            0x222 => BTN_LEFT,      // BTN_DPAD_LEFT
            0x223 => BTN_RIGHT,     // BTN_DPAD_RIGHT
            _ => 0,
        }
    };
    let max_bit = (key_words.len() * 64).min(0x300);
    let mut out = Vec::new();
    for code in 0x120..max_bit {
        if bitset_has(key_words, code) {
            out.push(map_code(code));
        }
    }
    for code in 0x100..0x120.min(max_bit) {
        if bitset_has(key_words, code) {
            out.push(map_code(code));
        }
    }
    out
}

/// Replicates Linux `drivers/input/joydev.c` axis numbering from `capabilities/abs`.
fn build_joydev_axis_map(abs_words: &[u64]) -> Vec<u8> {
    let mut out = Vec::new();
    let max_bit = (abs_words.len() * 64).min(64);
    for code in 0..max_bit {
        if bitset_has(abs_words, code) {
            out.push(code as u8);
        }
    }
    out
}

#[cfg(target_os = "linux")]
pub fn discover_gamepads() -> Vec<GamepadInfo> {
    let mut found: Vec<(u32, GamepadInfo)> = Vec::new();
    let Ok(rd) = std::fs::read_dir("/sys/class/input") else {
        return Vec::new();
    };
    for entry in rd.flatten() {
        let id = entry.file_name().to_string_lossy().into_owned();
        let Some(num_str) = id.strip_prefix("js") else { continue };
        let Ok(idx) = num_str.parse::<u32>() else { continue };
        let dev_path = format!("/dev/input/{id}");
        if !Path::new(&dev_path).exists() {
            continue;
        }
        let sys_dev = entry.path().join("device");
        let name = std::fs::read_to_string(sys_dev.join("name"))
            .unwrap_or_default()
            .trim()
            .to_string();
        let key_words = std::fs::read_to_string(sys_dev.join("capabilities/key"))
            .map(|s| parse_sysfs_bitset(&s))
            .unwrap_or_default();
        let is_gamepad = classify_gamepad(&name, &key_words);
        found.push((
            idx,
            GamepadInfo {
                id,
                path: dev_path,
                name,
                is_gamepad,
            },
        ));
    }
    found.sort_by_key(|(idx, _)| *idx);
    found.into_iter().map(|(_, info)| info).collect()
}

#[cfg(not(target_os = "linux"))]
pub fn discover_gamepads() -> Vec<GamepadInfo> {
    Vec::new()
}

#[cfg(target_os = "linux")]
struct OpenGamepad {
    id: String,
    name: String,
    file: std::fs::File,
    btn_map: Vec<u16>,
    axis_map: Vec<u8>,
    buttons: u16,
    stick_bits: u16,
    hat_bits: u16,
}

#[cfg(target_os = "linux")]
impl OpenGamepad {
    fn open(info: &GamepadInfo) -> Option<Self> {
        use std::os::unix::fs::OpenOptionsExt;
        let file = std::fs::OpenOptions::new()
            .read(true)
            .custom_flags(0x800) // O_NONBLOCK
            .open(&info.path)
            .ok()?;
        let sys_dev = PathBuf::from("/sys/class/input").join(&info.id).join("device");
        let key_words = std::fs::read_to_string(sys_dev.join("capabilities/key"))
            .map(|s| parse_sysfs_bitset(&s))
            .unwrap_or_default();
        let abs_words = std::fs::read_to_string(sys_dev.join("capabilities/abs"))
            .map(|s| parse_sysfs_bitset(&s))
            .unwrap_or_default();
        let btn_map = build_joydev_button_map(&key_words, is_nintendo_layout(&info.name));
        let axis_map = build_joydev_axis_map(&abs_words);
        Some(Self {
            id: info.id.clone(),
            name: if info.name.is_empty() { info.id.clone() } else { info.name.clone() },
            file,
            btn_map,
            axis_map,
            buttons: 0,
            stick_bits: 0,
            hat_bits: 0,
        })
    }

    fn poll(&mut self) -> Result<u16, ()> {
        use std::io::Read;
        let mut ev = [0u8; 8];
        loop {
            match self.file.read(&mut ev) {
                Ok(8) => {
                    let value = i16::from_le_bytes([ev[4], ev[5]]);
                    let kind = ev[6] & !0x80; // strip JS_EVENT_INIT
                    let number = ev[7] as usize;
                    if kind == 1 {
                        let bit = self.btn_map.get(number).copied().unwrap_or(0);
                        if value != 0 {
                            self.buttons |= bit;
                        } else {
                            self.buttons &= !bit;
                        }
                    } else if kind == 2 {
                        const DEAD: i16 = 12000;
                        let abs_code = self.axis_map.get(number).copied().unwrap_or_else(|| match number {
                            0 => 0,
                            1 => 1,
                            6 => 16,
                            7 => 17,
                            _ => 255,
                        });
                        match abs_code {
                            0 => {
                                // ABS_X (left stick horizontal)
                                self.stick_bits &= !(BTN_LEFT | BTN_RIGHT);
                                if value < -DEAD {
                                    self.stick_bits |= BTN_LEFT;
                                } else if value > DEAD {
                                    self.stick_bits |= BTN_RIGHT;
                                }
                            }
                            1 => {
                                // ABS_Y (left stick vertical)
                                self.stick_bits &= !(BTN_UP | BTN_DOWN);
                                if value < -DEAD {
                                    self.stick_bits |= BTN_UP;
                                } else if value > DEAD {
                                    self.stick_bits |= BTN_DOWN;
                                }
                            }
                            16 => {
                                // ABS_HAT0X (D-pad horizontal)
                                self.hat_bits &= !(BTN_LEFT | BTN_RIGHT);
                                if value < -DEAD {
                                    self.hat_bits |= BTN_LEFT;
                                } else if value > DEAD {
                                    self.hat_bits |= BTN_RIGHT;
                                }
                            }
                            17 => {
                                // ABS_HAT0Y (D-pad vertical)
                                self.hat_bits &= !(BTN_UP | BTN_DOWN);
                                if value < -DEAD {
                                    self.hat_bits |= BTN_UP;
                                } else if value > DEAD {
                                    self.hat_bits |= BTN_DOWN;
                                }
                            }
                            _ => {}
                        }
                    }
                }
                Ok(_) => break,
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => break,
                Err(_) => return Err(()),
            }
        }
        Ok(self.buttons | self.stick_bits | self.hat_bits)
    }
}

#[cfg(target_os = "linux")]
pub(crate) struct LinuxGamepad {
    selected: String, // "" = auto (all real gamepads), or "js1" / "/dev/input/js1"
    devices: Vec<OpenGamepad>,
    last_probe: Instant,
    changed: bool,
}

#[cfg(target_os = "linux")]
impl LinuxGamepad {
    pub(crate) fn new(selected: String) -> Self {
        let mut gp = Self {
            selected,
            devices: Vec::new(),
            last_probe: Instant::now() - Duration::from_secs(5),
            changed: true,
        };
        gp.refresh_devices();
        gp
    }

    pub(crate) fn set_selected(&mut self, selected: String) {
        self.selected = selected;
        self.devices.clear();
        self.last_probe = Instant::now() - Duration::from_secs(5);
        self.changed = true;
        self.refresh_devices();
    }

    pub(crate) fn active_summary(&self) -> String {
        if self.devices.is_empty() {
            if self.selected.is_empty() {
                "None (keyboard only)".into()
            } else {
                format!("{} not connected (keyboard only)", self.selected)
            }
        } else {
            self.devices
                .iter()
                .map(|d| format!("{} ({})", d.name, d.id))
                .collect::<Vec<_>>()
                .join(", ")
        }
    }

    pub(crate) fn take_changed_summary(&mut self) -> Option<String> {
        if self.changed {
            self.changed = false;
            Some(self.active_summary())
        } else {
            None
        }
    }

    fn refresh_devices(&mut self) {
        if self.last_probe.elapsed() < Duration::from_secs(2) {
            return;
        }
        self.last_probe = Instant::now();
        let all = discover_gamepads();
        let sel = self.selected.trim();
        let wanted: Vec<&GamepadInfo> = if sel.is_empty() {
            all.iter().filter(|g| g.is_gamepad).collect()
        } else {
            all.iter().filter(|g| g.id == sel || g.path == sel).collect()
        };

        // Remove any currently-open devices no longer in `wanted`
        let before = self.devices.len();
        self.devices.retain(|d| wanted.iter().any(|w| w.id == d.id));
        if self.devices.len() != before {
            self.changed = true;
        }
        // Open any newly attached wanted devices
        for info in wanted {
            if !self.devices.iter().any(|d| d.id == info.id) {
                if let Some(opened) = OpenGamepad::open(info) {
                    self.devices.push(opened);
                    self.changed = true;
                }
            }
        }
    }

    pub(crate) fn poll_mask(&mut self) -> u16 {
        self.refresh_devices();
        let mut mask = 0u16;
        let mut i = 0;
        while i < self.devices.len() {
            match self.devices[i].poll() {
                Ok(m) => {
                    mask |= m;
                    i += 1;
                }
                Err(()) => {
                    self.devices.remove(i);
                    self.changed = true;
                }
            }
        }
        mask
    }
}

#[cfg(not(target_os = "linux"))]
pub(crate) struct LinuxGamepad;

#[cfg(not(target_os = "linux"))]
impl LinuxGamepad {
    pub(crate) fn new(_selected: String) -> Self { Self }
    pub(crate) fn set_selected(&mut self, _selected: String) {}
    pub(crate) fn active_summary(&self) -> String { "Keyboard only".into() }
    pub(crate) fn take_changed_summary(&mut self) -> Option<String> { None }
    pub(crate) fn poll_mask(&mut self) -> u16 { 0 }
}

// ------------------------------------------------------------------ Libretro FFI & Dynamic Loader

#[repr(C)]
struct RetroSystemInfo {
    library_name: *const c_char,
    library_version: *const c_char,
    valid_extensions: *const c_char,
    need_fullpath: bool,
    block_extract: bool,
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
struct RetroGameGeometry {
    base_width: c_uint,
    base_height: c_uint,
    max_width: c_uint,
    max_height: c_uint,
    aspect_ratio: f32,
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
struct RetroSystemTiming {
    fps: f64,
    sample_rate: f64,
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
struct RetroSystemAvInfo {
    geometry: RetroGameGeometry,
    timing: RetroSystemTiming,
}

#[repr(C)]
struct RetroGameInfo {
    path: *const c_char,
    data: *const c_void,
    size: usize,
    meta: *const c_char,
}

#[repr(C)]
struct RetroLogCallback {
    log: Option<unsafe extern "C" fn(c_uint, *const c_char)>,
}

type FnInit = unsafe extern "C" fn();
type FnDeinit = unsafe extern "C" fn();
type FnReset = unsafe extern "C" fn();
type FnRun = unsafe extern "C" fn();
type FnUnload = unsafe extern "C" fn();
type FnLoadGame = unsafe extern "C" fn(*const RetroGameInfo) -> bool;
type FnGetSysInfo = unsafe extern "C" fn(*mut RetroSystemInfo);
type FnGetAvInfo = unsafe extern "C" fn(*mut RetroSystemAvInfo);
type FnSetCtrlPort = unsafe extern "C" fn(c_uint, c_uint);
type FnGetMemData = unsafe extern "C" fn(c_uint) -> *mut c_void;
type FnGetMemSize = unsafe extern "C" fn(c_uint) -> usize;
type FnSerSize = unsafe extern "C" fn() -> usize;
type FnSer = unsafe extern "C" fn(*mut c_void, usize) -> bool;
type FnUnser = unsafe extern "C" fn(*const c_void, usize) -> bool;

type CbEnv = unsafe extern "C" fn(c_uint, *mut c_void) -> bool;
type CbVideo = unsafe extern "C" fn(*const c_void, c_uint, c_uint, usize);
type CbAudioSample = unsafe extern "C" fn(i16, i16);
type CbAudioBatch = unsafe extern "C" fn(*const i16, usize) -> usize;
type CbInputPoll = unsafe extern "C" fn();
type CbInputState = unsafe extern "C" fn(c_uint, c_uint, c_uint, c_uint) -> i16;

type FnSetEnv = unsafe extern "C" fn(CbEnv);
type FnSetVideo = unsafe extern "C" fn(CbVideo);
type FnSetAudioSample = unsafe extern "C" fn(CbAudioSample);
type FnSetAudioBatch = unsafe extern "C" fn(CbAudioBatch);
type FnSetInputPoll = unsafe extern "C" fn(CbInputPoll);
type FnSetInputState = unsafe extern "C" fn(CbInputState);

#[cfg(unix)]
mod dynlib {
    use std::ffi::{CString, c_char, c_int, c_void};
    use std::path::Path;

    const RTLD_NOW: c_int = 2;
    const RTLD_LOCAL: c_int = 0;

    unsafe extern "C" {
        fn dlopen(filename: *const c_char, flags: c_int) -> *mut c_void;
        fn dlsym(handle: *mut c_void, symbol: *const c_char) -> *mut c_void;
        fn dlclose(handle: *mut c_void) -> c_int;
        fn dlerror() -> *const c_char;
    }

    pub struct DynLib(*mut c_void);

    impl DynLib {
        pub fn open(path: &Path) -> Result<Self, String> {
            let c = CString::new(path.to_string_lossy().as_bytes()).map_err(|_| "invalid path".to_string())?;
            let h = unsafe { dlopen(c.as_ptr(), RTLD_NOW | RTLD_LOCAL) };
            if h.is_null() {
                let err = unsafe {
                    let p = dlerror();
                    if p.is_null() { "dlopen failed".into() } else { std::ffi::CStr::from_ptr(p).to_string_lossy().into_owned() }
                };
                return Err(err);
            }
            Ok(Self(h))
        }

        pub unsafe fn sym<T: Copy>(&self, name: &str) -> Result<T, String> {
            let c = CString::new(name).map_err(|_| "invalid sym".to_string())?;
            let p = unsafe { dlsym(self.0, c.as_ptr()) };
            if p.is_null() {
                return Err(format!("missing symbol {name}"));
            }
            Ok(unsafe { std::mem::transmute_copy(&p) })
        }
    }

    impl Drop for DynLib {
        fn drop(&mut self) {
            if !self.0.is_null() {
                unsafe { dlclose(self.0) };
            }
        }
    }
}

#[cfg(windows)]
mod dynlib {
    use std::ffi::{CString, c_char, c_void};
    use std::path::Path;

    unsafe extern "system" {
        fn LoadLibraryA(name: *const c_char) -> *mut c_void;
        fn GetProcAddress(h: *mut c_void, name: *const c_char) -> *mut c_void;
        fn FreeLibrary(h: *mut c_void) -> i32;
    }

    pub struct DynLib(*mut c_void);

    impl DynLib {
        pub fn open(path: &Path) -> Result<Self, String> {
            let c = CString::new(path.to_string_lossy().as_bytes()).map_err(|_| "invalid path".to_string())?;
            let h = unsafe { LoadLibraryA(c.as_ptr()) };
            if h.is_null() {
                return Err(format!("LoadLibrary failed for {}", path.display()));
            }
            Ok(Self(h))
        }

        pub unsafe fn sym<T: Copy>(&self, name: &str) -> Result<T, String> {
            let c = CString::new(name).map_err(|_| "invalid sym".to_string())?;
            let p = unsafe { GetProcAddress(self.0, c.as_ptr()) };
            if p.is_null() {
                return Err(format!("missing symbol {name}"));
            }
            Ok(unsafe { std::mem::transmute_copy(&p) })
        }
    }

    impl Drop for DynLib {
        fn drop(&mut self) {
            if !self.0.is_null() {
                unsafe { FreeLibrary(self.0) };
            }
        }
    }
}

// ------------------------------------------------------------------ Thread-Local Core Callback Context

struct CoreRuntime {
    pixel_fmt: u32, // 0 = 0RGB1555, 1 = XRGB8888, 2 = RGB565
    system_dir: CString,
    save_dir: CString,
    src_rate: f64,
    resample_phase: f64,
    prev_l: f32,
    prev_r: f32,
    audio_out: Vec<f32>,
    ring: Option<Arc<AudioRing>>,
    audio_session: u64,
    shared: Arc<Mutex<RetroShared>>,
    input_mask: u16,
    key_mask: Arc<AtomicU16>,
    gamepad: LinuxGamepad,
}

thread_local! {
    static TLS_CTX: RefCell<*mut CoreRuntime> = const { RefCell::new(std::ptr::null_mut()) };
}

unsafe extern "C" fn cb_log(_level: c_uint, _fmt: *const c_char) {}

unsafe extern "C" fn cb_environment(cmd: c_uint, data: *mut c_void) -> bool {
    let ctx_ptr = TLS_CTX.with(|c| *c.borrow());
    if ctx_ptr.is_null() {
        return false;
    }
    let ctx = unsafe { &mut *ctx_ptr };
    match cmd {
        // RETRO_ENVIRONMENT_GET_CAN_DUPE
        3 => {
            if !data.is_null() {
                unsafe { *(data as *mut bool) = true };
            }
            true
        }
        // RETRO_ENVIRONMENT_GET_SYSTEM_DIRECTORY
        9 => {
            if !data.is_null() {
                unsafe { *(data as *mut *const c_char) = ctx.system_dir.as_ptr() };
            }
            true
        }
        // RETRO_ENVIRONMENT_SET_PIXEL_FORMAT
        10 => {
            if data.is_null() {
                return false;
            }
            let fmt = unsafe { *(data as *const u32) };
            if fmt <= 2 {
                ctx.pixel_fmt = fmt;
                true
            } else {
                false
            }
        }
        // RETRO_ENVIRONMENT_GET_VARIABLE_UPDATE
        17 => {
            if !data.is_null() {
                unsafe { *(data as *mut bool) = false };
            }
            true
        }
        // RETRO_ENVIRONMENT_SET_SUPPORT_NO_GAME
        18 => true,
        // RETRO_ENVIRONMENT_GET_LOG_INTERFACE
        27 => {
            if !data.is_null() {
                unsafe { (*(data as *mut RetroLogCallback)).log = Some(cb_log) };
            }
            true
        }
        // RETRO_ENVIRONMENT_GET_SAVE_DIRECTORY
        31 => {
            if !data.is_null() {
                unsafe { *(data as *mut *const c_char) = ctx.save_dir.as_ptr() };
            }
            true
        }
        // RETRO_ENVIRONMENT_SET_SYSTEM_AV_INFO
        32 => {
            if !data.is_null() {
                let info = unsafe { *(data as *const RetroSystemAvInfo) };
                if info.timing.sample_rate > 8000.0 {
                    ctx.src_rate = info.timing.sample_rate;
                }
            }
            true
        }
        // RETRO_ENVIRONMENT_SET_GEOMETRY
        37 => true,
        // RETRO_ENVIRONMENT_GET_INPUT_BITMASKS
        51 => true,
        // Everything else (including HW_RENDER=14 -> false so cores use software rendering)
        _ => false,
    }
}

unsafe extern "C" fn cb_video_refresh(data: *const c_void, width: c_uint, height: c_uint, pitch: usize) {
    if data.is_null() || width == 0 || height == 0 {
        return;
    }
    let ctx_ptr = TLS_CTX.with(|c| *c.borrow());
    if ctx_ptr.is_null() {
        return;
    }
    let ctx = unsafe { &mut *ctx_ptr };
    let w = width as usize;
    let h = height as usize;
    let mut rgba = vec![0u8; w * h * 4];
    let base = data as *const u8;

    match ctx.pixel_fmt {
        // XRGB8888 (LE: B, G, R, X)
        1 => {
            for y in 0..h {
                let row = unsafe { std::slice::from_raw_parts(base.add(y * pitch), w * 4) };
                let dst = &mut rgba[y * w * 4..(y + 1) * w * 4];
                for x in 0..w {
                    let si = x * 4;
                    dst[si] = row[si + 2];
                    dst[si + 1] = row[si + 1];
                    dst[si + 2] = row[si];
                    dst[si + 3] = 255;
                }
            }
        }
        // RGB565
        2 => {
            for y in 0..h {
                let row = unsafe { std::slice::from_raw_parts(base.add(y * pitch), w * 2) };
                let dst = &mut rgba[y * w * 4..(y + 1) * w * 4];
                for x in 0..w {
                    let px = u16::from_le_bytes([row[x * 2], row[x * 2 + 1]]);
                    let r5 = ((px >> 11) & 0x1F) as u8;
                    let g6 = ((px >> 5) & 0x3F) as u8;
                    let b5 = (px & 0x1F) as u8;
                    let di = x * 4;
                    dst[di] = (r5 << 3) | (r5 >> 2);
                    dst[di + 1] = (g6 << 2) | (g6 >> 4);
                    dst[di + 2] = (b5 << 3) | (b5 >> 2);
                    dst[di + 3] = 255;
                }
            }
        }
        // 0RGB1555
        _ => {
            for y in 0..h {
                let row = unsafe { std::slice::from_raw_parts(base.add(y * pitch), w * 2) };
                let dst = &mut rgba[y * w * 4..(y + 1) * w * 4];
                for x in 0..w {
                    let px = u16::from_le_bytes([row[x * 2], row[x * 2 + 1]]);
                    let r5 = ((px >> 10) & 0x1F) as u8;
                    let g5 = ((px >> 5) & 0x1F) as u8;
                    let b5 = (px & 0x1F) as u8;
                    let di = x * 4;
                    dst[di] = (r5 << 3) | (r5 >> 2);
                    dst[di + 1] = (g5 << 3) | (g5 >> 2);
                    dst[di + 2] = (b5 << 3) | (b5 >> 2);
                    dst[di + 3] = 255;
                }
            }
        }
    }

    let mut sh = ctx.shared.lock().unwrap();
    sh.frame_no += 1;
    sh.frame = Some(Arc::new((w, h, rgba)));
}

fn push_resampled_audio(ctx: &mut CoreRuntime, frames: &[(i16, i16)]) {
    let Some(ref ring) = ctx.ring else { return };
    if frames.is_empty() {
        return;
    }
    let ratio = ctx.src_rate / AUDIO_RATE as f64;
    ctx.audio_out.clear();
    for &(li, ri) in frames {
        let l = li as f32 / 32768.0;
        let r = ri as f32 / 32768.0;
        while ctx.resample_phase < 1.0 {
            let t = ctx.resample_phase as f32;
            ctx.audio_out.push(ctx.prev_l + (l - ctx.prev_l) * t);
            ctx.audio_out.push(ctx.prev_r + (r - ctx.prev_r) * t);
            ctx.resample_phase += ratio;
        }
        ctx.resample_phase -= 1.0;
        ctx.prev_l = l;
        ctx.prev_r = r;
    }
    if !ctx.audio_out.is_empty() {
        ring.push_f32_session_realtime(ctx.audio_session, &ctx.audio_out);
    }
}

unsafe extern "C" fn cb_audio_sample(left: i16, right: i16) {
    let ctx_ptr = TLS_CTX.with(|c| *c.borrow());
    if ctx_ptr.is_null() {
        return;
    }
    let ctx = unsafe { &mut *ctx_ptr };
    push_resampled_audio(ctx, &[(left, right)]);
}

unsafe extern "C" fn cb_audio_batch(data: *const i16, frames: usize) -> usize {
    if data.is_null() || frames == 0 {
        return 0;
    }
    let ctx_ptr = TLS_CTX.with(|c| *c.borrow());
    if ctx_ptr.is_null() {
        return frames;
    }
    let ctx = unsafe { &mut *ctx_ptr };
    let slice = unsafe { std::slice::from_raw_parts(data, frames * 2) };
    let mut pairs = Vec::with_capacity(frames);
    for chunk in slice.chunks_exact(2) {
        pairs.push((chunk[0], chunk[1]));
    }
    push_resampled_audio(ctx, &pairs);
    frames
}

unsafe extern "C" fn cb_input_poll() {
    let ctx_ptr = TLS_CTX.with(|c| *c.borrow());
    if ctx_ptr.is_null() {
        return;
    }
    let ctx = unsafe { &mut *ctx_ptr };
    let kb = ctx.key_mask.load(Ordering::Relaxed);
    let gp = ctx.gamepad.poll_mask();
    ctx.input_mask = kb | gp;
}

unsafe extern "C" fn cb_input_state(port: c_uint, device: c_uint, _index: c_uint, id: c_uint) -> i16 {
    if port != 0 || (device & 0xFF) != 1 {
        return 0;
    }
    let ctx_ptr = TLS_CTX.with(|c| *c.borrow());
    if ctx_ptr.is_null() {
        return 0;
    }
    let ctx = unsafe { &*ctx_ptr };
    if id == 256 {
        // RETRO_DEVICE_ID_JOYPAD_MASK
        ctx.input_mask as i16
    } else if id < 16 {
        if (ctx.input_mask & (1u16 << id)) != 0 { 1 } else { 0 }
    } else {
        0
    }
}

// ------------------------------------------------------------------ RetroPlayer Handle

pub struct RetroShared {
    pub frame: Option<Arc<(usize, usize, Vec<u8>)>>,
    pub frame_no: u64,
    pub core_name: String,
    pub game_name: String,
    pub status: String,
    pub active_gamepad: String,
    pub paused: bool,
    pub running: bool,
}

enum RetroCmd {
    TogglePause,
    Reset,
    SaveState,
    LoadState,
    SetGamepad(String),
    Stop,
}

pub struct RetroPlayer {
    pub shared: Arc<Mutex<RetroShared>>,
    pub key_mask: Arc<AtomicU16>,
    tx: Sender<RetroCmd>,
    stop: Arc<AtomicBool>,
    join: Option<std::thread::JoinHandle<()>>,
}

impl Drop for RetroPlayer {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        let _ = self.tx.send(RetroCmd::Stop);
        if let Some(h) = self.join.take() {
            let _ = h.join();
        }
    }
}

impl RetroPlayer {
    pub fn start_standby(ch_num: u16, msg: &str) -> Self {
        let (tx, _rx) = channel();
        let shared = Arc::new(Mutex::new(RetroShared {
            frame: Some(render_standby_frame(ch_num, msg)),
            frame_no: 1,
            core_name: String::new(),
            game_name: "INSERT CARTRIDGE".into(),
            status: if msg.is_empty() {
                "Waiting for ROM cartridge…".into()
            } else {
                msg.to_string()
            },
            active_gamepad: String::new(),
            paused: false,
            running: false,
        }));
        Self {
            shared,
            key_mask: Arc::new(AtomicU16::new(0)),
            tx,
            stop: Arc::new(AtomicBool::new(true)),
            join: None,
        }
    }

    pub fn start(
        core_path: PathBuf,
        rom_path: PathBuf,
        ring: Option<Arc<AudioRing>>,
        gamepad_device: String,
    ) -> Self {
        let game_name = rom_path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("Game")
            .to_string();
        let core_name = core_path
            .file_stem()
            .and_then(|s| s.to_str())
            .map(|s| s.strip_suffix("_libretro").unwrap_or(s).to_string())
            .unwrap_or_else(|| "libretro".into());

        let shared = Arc::new(Mutex::new(RetroShared {
            frame: None,
            frame_no: 0,
            core_name: core_name.clone(),
            game_name: game_name.clone(),
            status: format!("Booting {game_name} ({core_name})…"),
            active_gamepad: "Detecting…".into(),
            paused: false,
            running: true,
        }));
        let key_mask = Arc::new(AtomicU16::new(0));
        let stop = Arc::new(AtomicBool::new(false));
        let (tx, rx) = channel();

        let sh_bg = shared.clone();
        let km_bg = key_mask.clone();
        let stop_bg = stop.clone();

        let join = std::thread::spawn(move || {
            if let Err(err) = run_core_loop(&core_path, &rom_path, ring, gamepad_device, &sh_bg, km_bg, &stop_bg, rx) {
                let mut g = sh_bg.lock().unwrap();
                g.status = err.clone();
                g.running = false;
                g.frame_no += 1;
                g.frame = Some(render_standby_frame(3, &err));
            }
        });

        Self {
            shared,
            key_mask,
            tx,
            stop,
            join: Some(join),
        }
    }

    pub fn toggle_pause(&self) {
        let _ = self.tx.send(RetroCmd::TogglePause);
    }

    pub fn reset(&self) {
        let _ = self.tx.send(RetroCmd::Reset);
    }

    pub fn save_state(&self) {
        let _ = self.tx.send(RetroCmd::SaveState);
    }

    pub fn load_state(&self) {
        let _ = self.tx.send(RetroCmd::LoadState);
    }

    pub fn set_gamepad(&self, device: String) {
        let _ = self.tx.send(RetroCmd::SetGamepad(device));
    }
}

fn srm_path_for(rom_path: &Path) -> PathBuf {
    let dir = retro_saves_dir();
    let _ = std::fs::create_dir_all(&dir);
    let stem = rom_path.file_stem().and_then(|s| s.to_str()).unwrap_or("game");
    dir.join(format!("{stem}.srm"))
}

fn state_path_for(rom_path: &Path) -> PathBuf {
    let dir = retro_saves_dir();
    let _ = std::fs::create_dir_all(&dir);
    let stem = rom_path.file_stem().and_then(|s| s.to_str()).unwrap_or("game");
    dir.join(format!("{stem}.state"))
}

fn run_core_loop(
    core_path: &Path,
    rom_path: &Path,
    ring: Option<Arc<AudioRing>>,
    gamepad_device: String,
    shared: &Arc<Mutex<RetroShared>>,
    key_mask: Arc<AtomicU16>,
    stop: &Arc<AtomicBool>,
    rx: Receiver<RetroCmd>,
) -> Result<(), String> {
    let lib = dynlib::DynLib::open(core_path).map_err(|e| format!("Failed to load core: {e}"))?;
    let f_init: FnInit = unsafe { lib.sym("retro_init")? };
    let f_deinit: FnDeinit = unsafe { lib.sym("retro_deinit")? };
    let f_reset: FnReset = unsafe { lib.sym("retro_reset")? };
    let f_run: FnRun = unsafe { lib.sym("retro_run")? };
    let f_load_game: FnLoadGame = unsafe { lib.sym("retro_load_game")? };
    let f_unload_game: FnUnload = unsafe { lib.sym("retro_unload_game")? };
    let f_sys_info: FnGetSysInfo = unsafe { lib.sym("retro_get_system_info")? };
    let f_av_info: FnGetAvInfo = unsafe { lib.sym("retro_get_system_av_info")? };
    let f_set_port: Option<FnSetCtrlPort> = unsafe { lib.sym("retro_set_controller_port_device").ok() };
    let f_mem_data: Option<FnGetMemData> = unsafe { lib.sym("retro_get_memory_data").ok() };
    let f_mem_size: Option<FnGetMemSize> = unsafe { lib.sym("retro_get_memory_size").ok() };
    let f_ser_size: Option<FnSerSize> = unsafe { lib.sym("retro_serialize_size").ok() };
    let f_ser: Option<FnSer> = unsafe { lib.sym("retro_serialize").ok() };
    let f_unser: Option<FnUnser> = unsafe { lib.sym("retro_unserialize").ok() };

    let f_set_env: FnSetEnv = unsafe { lib.sym("retro_set_environment")? };
    let f_set_vid: FnSetVideo = unsafe { lib.sym("retro_set_video_refresh")? };
    let f_set_as: FnSetAudioSample = unsafe { lib.sym("retro_set_audio_sample")? };
    let f_set_ab: FnSetAudioBatch = unsafe { lib.sym("retro_set_audio_sample_batch")? };
    let f_set_ip: FnSetInputPoll = unsafe { lib.sym("retro_set_input_poll")? };
    let f_set_is: FnSetInputState = unsafe { lib.sym("retro_set_input_state")? };

    let sys_dir_p = retro_system_dir();
    let save_dir_p = retro_saves_dir();
    let _ = std::fs::create_dir_all(&sys_dir_p);
    let _ = std::fs::create_dir_all(&save_dir_p);

    let audio_session = ring.as_ref().map(|r| r.begin_session()).unwrap_or(0);
    let mut ctx = Box::new(CoreRuntime {
        pixel_fmt: 0,
        system_dir: CString::new(sys_dir_p.to_string_lossy().as_bytes()).unwrap(),
        save_dir: CString::new(save_dir_p.to_string_lossy().as_bytes()).unwrap(),
        src_rate: 48000.0,
        resample_phase: 0.0,
        prev_l: 0.0,
        prev_r: 0.0,
        audio_out: Vec::with_capacity(4096),
        ring: ring.clone(),
        audio_session,
        shared: shared.clone(),
        input_mask: 0,
        key_mask,
        gamepad: LinuxGamepad::new(gamepad_device),
    });
    if let Some(summary) = ctx.gamepad.take_changed_summary() {
        shared.lock().unwrap().active_gamepad = summary;
    }

    let ctx_raw: *mut CoreRuntime = &mut *ctx;
    TLS_CTX.with(|c| *c.borrow_mut() = ctx_raw);

    struct TlsGuard;
    impl Drop for TlsGuard {
        fn drop(&mut self) {
            TLS_CTX.with(|c| *c.borrow_mut() = std::ptr::null_mut());
        }
    }
    let _tls_guard = TlsGuard;

    unsafe {
        f_set_env(cb_environment);
        f_set_vid(cb_video_refresh);
        f_set_as(cb_audio_sample);
        f_set_ab(cb_audio_batch);
        f_set_ip(cb_input_poll);
        f_set_is(cb_input_state);
        f_init();
    }

    let mut sys_info = RetroSystemInfo {
        library_name: std::ptr::null(),
        library_version: std::ptr::null(),
        valid_extensions: std::ptr::null(),
        need_fullpath: false,
        block_extract: false,
    };
    unsafe { f_sys_info(&mut sys_info) };
    let core_disp = if sys_info.library_name.is_null() {
        String::new()
    } else {
        unsafe { CStr::from_ptr(sys_info.library_name).to_string_lossy().into_owned() }
    };

    let rom_cpath = CString::new(rom_path.to_string_lossy().as_bytes()).map_err(|_| "Invalid ROM path.".to_string())?;
    let rom_bytes = if sys_info.need_fullpath {
        Vec::new()
    } else {
        std::fs::read(rom_path).map_err(|e| format!("Couldn't read ROM: {e}"))?
    };

    let game_info = RetroGameInfo {
        path: rom_cpath.as_ptr(),
        data: if rom_bytes.is_empty() { std::ptr::null() } else { rom_bytes.as_ptr() as *const c_void },
        size: rom_bytes.len(),
        meta: std::ptr::null(),
    };

    let loaded = unsafe { f_load_game(&game_info) };
    if !loaded {
        unsafe { f_deinit() };
        return Err(format!(
            "Core {} rejected this ROM (check BIOS or use Window Capture for 3D HW cores).",
            if core_disp.is_empty() { "libretro" } else { &core_disp }
        ));
    }

    if let Some(set_port) = f_set_port {
        unsafe { set_port(0, 1) }; // RETRO_DEVICE_JOYPAD
    }

    let mut av_info = RetroSystemAvInfo::default();
    unsafe { f_av_info(&mut av_info) };
    let fps = if av_info.timing.fps > 20.0 && av_info.timing.fps < 120.0 {
        av_info.timing.fps
    } else {
        59.94
    };
    if av_info.timing.sample_rate > 8000.0 {
        ctx.src_rate = av_info.timing.sample_rate;
    }

    // Load SRAM battery save (.srm) if present
    let srm_path = srm_path_for(rom_path);
    if let (Some(get_data), Some(get_size)) = (f_mem_data, f_mem_size) {
        let sz = unsafe { get_size(0) };
        let ptr = unsafe { get_data(0) };
        if sz > 0 && !ptr.is_null() {
            if let Ok(saved) = std::fs::read(&srm_path) {
                let copy_len = saved.len().min(sz);
                unsafe { std::ptr::copy_nonoverlapping(saved.as_ptr(), ptr as *mut u8, copy_len) };
            }
        }
    }

    {
        let mut g = shared.lock().unwrap();
        if !core_disp.is_empty() {
            g.core_name = core_disp.clone();
        }
        g.status = format!("Playing via {} ({:.2} Hz)", g.core_name, fps);
    }

    let frame_dur = Duration::from_secs_f64(1.0 / fps);
    let mut next_tick = Instant::now();
    let mut paused = false;

    while !stop.load(Ordering::Relaxed) {
        while let Ok(cmd) = rx.try_recv() {
            match cmd {
                RetroCmd::Stop => {
                    stop.store(true, Ordering::SeqCst);
                }
                RetroCmd::TogglePause => {
                    paused = !paused;
                    shared.lock().unwrap().paused = paused;
                    if paused {
                        if let Some(r) = &ring {
                            r.clear_session(audio_session);
                        }
                    } else {
                        next_tick = Instant::now();
                    }
                }
                RetroCmd::Reset => {
                    unsafe { f_reset() };
                    next_tick = Instant::now();
                }
                RetroCmd::SaveState => {
                    if let (Some(ssz), Some(ser)) = (f_ser_size, f_ser) {
                        let sz = unsafe { ssz() };
                        if sz > 0 {
                            let mut buf = vec![0u8; sz];
                            if unsafe { ser(buf.as_mut_ptr() as *mut c_void, sz) } {
                                let _ = std::fs::write(state_path_for(rom_path), &buf);
                                shared.lock().unwrap().status = "Saved state (F2).".into();
                            }
                        }
                    }
                }
                RetroCmd::LoadState => {
                    if let Some(unser) = f_unser {
                        if let Ok(buf) = std::fs::read(state_path_for(rom_path)) {
                            if unsafe { unser(buf.as_ptr() as *const c_void, buf.len()) } {
                                shared.lock().unwrap().status = "Loaded state (F4).".into();
                                next_tick = Instant::now();
                            }
                        }
                    }
                }
                RetroCmd::SetGamepad(dev) => {
                    ctx.gamepad.set_selected(dev);
                    shared.lock().unwrap().active_gamepad = ctx.gamepad.active_summary();
                }
            }
        }
        if stop.load(Ordering::Relaxed) {
            break;
        }
        if paused {
            std::thread::sleep(Duration::from_millis(16));
            continue;
        }

        unsafe { f_run() };
        if let Some(summary) = ctx.gamepad.take_changed_summary() {
            shared.lock().unwrap().active_gamepad = summary;
        }

        next_tick += frame_dur;
        let now = Instant::now();
        if next_tick > now {
            std::thread::sleep(next_tick - now);
        } else if now.duration_since(next_tick) > Duration::from_millis(100) {
            next_tick = now;
        }
    }

    // Save SRAM battery save (.srm) cleanly before tearing down
    if let (Some(get_data), Some(get_size)) = (f_mem_data, f_mem_size) {
        let sz = unsafe { get_size(0) };
        let ptr = unsafe { get_data(0) };
        if sz > 0 && !ptr.is_null() {
            let slice = unsafe { std::slice::from_raw_parts(ptr as *const u8, sz) };
            let _ = std::fs::write(&srm_path, slice);
        }
    }

    if let Some(r) = &ring {
        r.clear_session(audio_session);
    }
    unsafe {
        f_unload_game();
        f_deinit();
    }
    Ok(())
}

// ------------------------------------------------------------------ Manager

pub struct RetroManager {
    pub cfg: RetroConfig,
    pub cores: Vec<CoreInfo>,
    pub gamepads: Vec<GamepadInfo>,
    pub channel: Option<Channel>,
    pub status: String,
}

impl RetroManager {
    pub fn new_offline() -> Self {
        Self {
            cfg: RetroConfig::default(),
            cores: Vec::new(),
            gamepads: Vec::new(),
            channel: None,
            status: String::new(),
        }
    }

    pub fn new() -> Self {
        let cfg = load_config();
        let mut mgr = Self {
            cfg,
            ..Self::new_offline()
        };
        mgr.rescan();
        mgr
    }

    pub fn refresh_gamepads(&mut self) {
        self.gamepads = discover_gamepads();
    }

    pub fn set_gamepad_device(&mut self, device: String) {
        if self.cfg.gamepad_device != device {
            self.cfg.gamepad_device = device;
            let _ = save_config(&self.cfg);
        }
    }

    pub fn rescan(&mut self) {
        self.cores = discover_cores(&self.cfg.custom_core_dir);
        self.gamepads = discover_gamepads();
        self.channel = build_console_channel(&self.cfg, &self.cores);
        let n_roms = self
            .channel
            .as_ref()
            .map(|c| {
                c.items()
                    .iter()
                    .filter(|i| i.id != "retro:standby")
                    .count()
            })
            .unwrap_or(0);
        self.status = if self.cores.is_empty() {
            "No libretro cores found in /usr/lib/libretro or ~/.config/retroarch/cores.".into()
        } else {
            format!("{} core(s) found · {} ROM(s)", self.cores.len(), n_roms)
        };
    }

    pub fn add_rom_dir(&mut self, dir: String) {
        let trimmed = dir.trim().to_string();
        if trimmed.is_empty() || self.cfg.rom_dirs.iter().any(|d| d == &trimmed) {
            return;
        }
        self.cfg.rom_dirs.push(trimmed);
        let _ = save_config(&self.cfg);
        self.rescan();
    }

    pub fn remove_rom_dir(&mut self, idx: usize) {
        if idx < self.cfg.rom_dirs.len() {
            self.cfg.rom_dirs.remove(idx);
            let _ = save_config(&self.cfg);
            self.rescan();
        }
    }

    pub fn set_last_rom(&mut self, rom_path: &str) {
        if self.cfg.last_rom != rom_path {
            self.cfg.last_rom = rom_path.to_string();
            let _ = save_config(&self.cfg);
            self.rescan();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn retro_url_and_core_matching_work() {
        assert!(is_retro_url("retro:///roms/mario.sfc?core=bsnes"));
        assert!(!is_retro_url("https://example.com/live.m3u8"));
        assert_eq!(
            parse_retro_url("retro:///roms/mario.sfc?core=bsnes"),
            ("/roms/mario.sfc".into(), Some("bsnes".into()))
        );
        assert_eq!(
            parse_retro_url("retro:///roms/sonic.md"),
            ("/roms/sonic.md".into(), None)
        );

        let cores = vec![
            CoreInfo { name: "bsnes".into(), path: PathBuf::from("/usr/lib/libretro/bsnes_libretro.so") },
            CoreInfo { name: "mesen".into(), path: PathBuf::from("/usr/lib/libretro/mesen_libretro.so") },
            CoreInfo { name: "genesis_plus_gx".into(), path: PathBuf::from("/usr/lib/libretro/genesis_plus_gx_libretro.so") },
        ];
        let empty = HashMap::new();
        assert_eq!(select_core_for_rom(Path::new("smw.sfc"), &cores, None, &empty).unwrap().name, "bsnes");
        assert_eq!(select_core_for_rom(Path::new("smb3.nes"), &cores, None, &empty).unwrap().name, "mesen");
        assert_eq!(select_core_for_rom(Path::new("sonic.md"), &cores, None, &empty).unwrap().name, "genesis_plus_gx");
    }

    #[test]
    fn standby_frame_renders_at_640x480() {
        let f = render_standby_frame(3, "");
        assert_eq!(f.0, 640);
        assert_eq!(f.1, 480);
        assert_eq!(f.2.len(), 640 * 480 * 4);
    }

    #[test]
    fn gamepad_classification_and_dynamic_mapping_match_real_devices() {
        // Real sysfs capabilities captured from Linux:
        // 1. ASRock LED Controller (bogus js0 HID device -> must NOT classify as gamepad)
        let led_keys = parse_sysfs_bitset(
            "c000 0 0 40000001000000 1200000000 c00000000000000 100000800000000 40000010cc00 10168000000000 0",
        );
        assert!(!classify_gamepad("ASRock LED Controller", &led_keys));

        // 2. Razer Wolverine V3 Pro (Xbox layout: D-pad on abs 6,7; L/R on 4,5; Select/Start on 6,7)
        let razer_keys = parse_sysfs_bitset("7cdb000000000000 0 0 0 0");
        let razer_abs = parse_sysfs_bitset("3003f");
        assert!(classify_gamepad("Razer Wolverine V3 Pro (2.4 GHz Dongle)", &razer_keys));
        let razer_btns = build_joydev_button_map(&razer_keys, is_nintendo_layout("Razer Wolverine V3 Pro"));
        assert_eq!(razer_btns[0], BTN_B);
        assert_eq!(razer_btns[1], BTN_A);
        assert_eq!(razer_btns[2], BTN_Y);
        assert_eq!(razer_btns[3], BTN_X);
        assert_eq!(razer_btns[4], BTN_L);
        assert_eq!(razer_btns[5], BTN_R);
        assert_eq!(razer_btns[6], BTN_SELECT);
        assert_eq!(razer_btns[7], BTN_START);
        let razer_axes = build_joydev_axis_map(&razer_abs);
        assert_eq!(razer_axes[6], 16); // ABS_HAT0X
        assert_eq!(razer_axes[7], 17); // ABS_HAT0Y

        // 3. Nintendo Switch Pro Controller (hid-nintendo: Capture at btn 4, L/R at 5/6, Select/Start at 9/10, D-pad at abs 4/5)
        let pro_keys = parse_sysfs_bitset("7ffb000000000000 0 0 0 0");
        let pro_abs = parse_sysfs_bitset("3001b");
        assert!(classify_gamepad("Nintendo Co., Ltd. Pro Controller", &pro_keys));
        let pro_btns = build_joydev_button_map(&pro_keys, is_nintendo_layout("Nintendo Co., Ltd. Pro Controller"));
        assert_eq!(pro_btns[0], BTN_B);      // South (B)
        assert_eq!(pro_btns[1], BTN_A);      // East (A)
        assert_eq!(pro_btns[2], BTN_X);      // North (X)
        assert_eq!(pro_btns[3], BTN_Y);      // West (Y)
        assert_eq!(pro_btns[4], 0);          // Capture (ignored)
        assert_eq!(pro_btns[5], BTN_L);      // L
        assert_eq!(pro_btns[6], BTN_R);      // R
        assert_eq!(pro_btns[9], BTN_SELECT); // Minus
        assert_eq!(pro_btns[10], BTN_START); // Plus
        let pro_axes = build_joydev_axis_map(&pro_abs);
        assert_eq!(pro_axes[4], 16); // ABS_HAT0X
        assert_eq!(pro_axes[5], 17); // ABS_HAT0Y
    }
}
