//! Lightweight environment & dependency checks so newcomers see immediately
//! what is ready (video, YouTube/streams, web channels, retro cores, audio)
//! instead of hitting silent failures.

use std::path::Path;
use std::process::Command;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum CheckLevel {
    Ready,
    Optional,
    Missing,
}

#[derive(Clone, Debug)]
pub struct CheckItem {
    pub label: &'static str,
    pub level: CheckLevel,
    pub detail: String,
    pub hint: &'static str,
}

#[derive(Clone, Debug, Default)]
pub struct SystemCheck {
    pub items: Vec<CheckItem>,
}

fn has_libmpv_library() -> Option<&'static str> {
    if crate::mpv::has_libmpv() {
        Some("libmpv")
    } else {
        None
    }
}

fn find_first_in_path(names: &[&str]) -> Option<String> {
    for n in names {
        if crate::web_channel::which_in_path(n).is_some() {
            return Some((*n).to_string());
        }
    }
    None
}

impl SystemCheck {
    pub fn run(retro_core_count: usize) -> Self {
        let mut items = Vec::with_capacity(6);

        // 1. Video playback (libmpv or mpv CLI)
        let libmpv = has_libmpv_library();
        let mpv_cli = find_first_in_path(&["mpv", "mpv.exe"]);
        if let Some(lib) = libmpv {
            items.push(CheckItem {
                label: "Video Playback",
                level: CheckLevel::Ready,
                detail: format!("Ready ({lib})"),
                hint: "",
            });
        } else if let Some(bin) = mpv_cli {
            items.push(CheckItem {
                label: "Video Playback",
                level: CheckLevel::Ready,
                detail: format!("Ready ({bin} CLI)"),
                hint: "Tip: installing libmpv gives smoother seeking and zero-lag playback.",
            });
        } else {
            items.push(CheckItem {
                label: "Video Playback",
                level: CheckLevel::Missing,
                detail: "mpv / libmpv not found".into(),
                hint: "Install mpv (or libmpv) to play local video files, Jellyfin, Plex, and live streams.",
            });
        }

        // 2. Online / YouTube streams (yt-dlp)
        if let Some(bin) = find_first_in_path(&["yt-dlp", "yt-dlp.exe", "youtube-dl"]) {
            items.push(CheckItem {
                label: "YouTube & Web Streams",
                level: CheckLevel::Ready,
                detail: format!("Ready ({bin})"),
                hint: "",
            });
        } else {
            items.push(CheckItem {
                label: "YouTube & Web Streams",
                level: CheckLevel::Optional,
                detail: "yt-dlp not found".into(),
                hint: "Install yt-dlp to tune YouTube Live channels and web video URLs (direct .m3u8 streams still work).",
            });
        }

        // 3. Interactive Web Channels (Chromium / Chrome)
        if let Some(browser) = crate::web_channel::find_chromium_binary() {
            let name = browser
                .file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_else(|| "Chromium".into());
            items.push(CheckItem {
                label: "Web & Arcade Channels",
                level: CheckLevel::Ready,
                detail: format!("Ready ({name})"),
                hint: "",
            });
        } else {
            items.push(CheckItem {
                label: "Web & Arcade Channels",
                level: CheckLevel::Optional,
                detail: "Chromium / Chrome not found".into(),
                hint: "Install Chromium or Chrome for WeatherStar 4000+, PICO-8 BBS, and PuzzleScript channels.",
            });
        }

        // 4. Retro Game Console (libretro cores)
        if retro_core_count > 0 {
            items.push(CheckItem {
                label: "Retro Console (CH 03)",
                level: CheckLevel::Ready,
                detail: format!(
                    "{retro_core_count} core{} installed",
                    if retro_core_count == 1 { "" } else { "s" }
                ),
                hint: "",
            });
        } else {
            items.push(CheckItem {
                label: "Retro Console (CH 03)",
                level: CheckLevel::Optional,
                detail: "No libretro cores found".into(),
                hint: "Install RetroArch libretro cores (e.g. fceumm, snes9x, mgba, genesis_plus_gx) to play ROMs directly on CH 03.",
            });
        }

        // 5. Audio DSP output
        #[cfg(target_os = "linux")]
        let audio_bins: &[&str] = &["pw-cat", "pacat", "aplay", "mpv", "ffplay"];
        #[cfg(not(target_os = "linux"))]
        let audio_bins: &[&str] = &["mpv", "ffplay", "mpv.exe", "ffplay.exe"];
        if let Some(bin) = find_first_in_path(audio_bins) {
            items.push(CheckItem {
                label: "Analog TV Sound DSP",
                level: CheckLevel::Ready,
                detail: format!("Ready ({bin})"),
                hint: "",
            });
        } else {
            items.push(CheckItem {
                label: "Analog TV Sound DSP",
                level: CheckLevel::Optional,
                detail: "No audio sink CLI found".into(),
                hint: "Install PipeWire (pw-cat), PulseAudio (pacat), or ALSA (aplay) for simulated TV speaker/RF audio.",
            });
        }

        // 6. Window / Capture mirror (Linux Wayland)
        #[cfg(target_os = "linux")]
        {
            if find_first_in_path(&["wf-recorder"]).is_some() {
                items.push(CheckItem {
                    label: "Emulator / Screen Capture",
                    level: CheckLevel::Ready,
                    detail: "Ready (wf-recorder)".into(),
                    hint: "",
                });
            } else {
                items.push(CheckItem {
                    label: "Emulator / Screen Capture",
                    level: CheckLevel::Optional,
                    detail: "wf-recorder not found".into(),
                    hint: "Optional: install wf-recorder to mirror Dolphin, PCSX2, or monitors onto AV 1.",
                });
            }
        }

        Self { items }
    }

    pub fn summary_counts(&self) -> (usize, usize, usize) {
        let mut ready = 0;
        let mut opt = 0;
        let mut missing = 0;
        for it in &self.items {
            match it.level {
                CheckLevel::Ready => ready += 1,
                CheckLevel::Optional => opt += 1,
                CheckLevel::Missing => missing += 1,
            }
        }
        (ready, opt, missing)
    }
}

/// Opens a folder (or file) in the user's desktop file manager without blocking the UI.
pub fn open_path_in_file_manager(path: &Path) -> Result<(), String> {
    if !path.exists() {
        std::fs::create_dir_all(path).map_err(|e| format!("Couldn't create {}: {e}", path.display()))?;
    }
    #[cfg(target_os = "linux")]
    let mut cmd = {
        let mut c = Command::new("xdg-open");
        c.arg(path);
        c
    };
    #[cfg(target_os = "macos")]
    let mut cmd = {
        let mut c = Command::new("open");
        c.arg(path);
        c
    };
    #[cfg(target_os = "windows")]
    let mut cmd = {
        let mut c = Command::new("explorer");
        c.arg(path);
        c
    };
    cmd.spawn()
        .map(|_| ())
        .map_err(|e| format!("Couldn't open folder ({}): {e}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn system_check_populates_items_without_panicking() {
        let chk = SystemCheck::run(2);
        assert!(chk.items.len() >= 5);
        assert!(chk.items.iter().any(|i| i.label == "Retro Console (CH 03)" && i.level == CheckLevel::Ready));
        let (r, o, m) = chk.summary_counts();
        assert_eq!(r + o + m, chk.items.len());
    }
}
