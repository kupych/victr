//! External Window / Monitor / USB Capture Card live input (`AV 1`) for ViCTR.
//!
//! Lets you mirror ANY running emulator or app (Dolphin, PCSX2, RetroArch, RPCS3,
//! Steam games) or real hardware console via a USB V4L2 capture card directly into
//! ViCTR's full NTSC/RF/CRT physics pipeline using `capture://` URLs:
//! - `capture://window/<class_or_title>` — Hyprland/Sway window capture via `wf-recorder` (60 fps raw RGBA)
//! - `capture://output/<monitor_name>` — Full Wayland output capture via `wf-recorder -o`
//! - `capture://region/<x>,<y>,<w>,<h>` — Explicit screen region capture via `wf-recorder` / `x11grab`
//! - `capture://v4l2/<device_path>` — USB HDMI/Composite capture card via `ffmpeg -f v4l2`

use crate::osd::Canvas;
use std::io::Read;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

pub const CAP_W: usize = 640;
pub const CAP_H: usize = 480;
const FRAME_BYTES: usize = CAP_W * CAP_H * 4;

pub fn is_capture_url(src: &str) -> bool {
    let s = src.trim();
    s.len() >= 10 && s[..10].eq_ignore_ascii_case("capture://")
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CaptureTarget {
    Window(String),
    Output(String),
    Region { x: i32, y: i32, w: u32, h: u32 },
    V4l2(String),
}

pub fn parse_capture_url(src: &str) -> Option<CaptureTarget> {
    let s = src.trim();
    let rest = if is_capture_url(s) { &s[10..] } else { return None };
    if let Some(q) = rest.strip_prefix("window/") {
        let q = q.trim();
        if !q.is_empty() {
            return Some(CaptureTarget::Window(q.to_string()));
        }
    } else if let Some(out) = rest.strip_prefix("output/") {
        let out = out.trim();
        if !out.is_empty() {
            return Some(CaptureTarget::Output(out.to_string()));
        }
    } else if let Some(reg) = rest.strip_prefix("region/") {
        let parts: Vec<&str> = reg.trim().split(',').collect();
        if parts.len() == 4 {
            if let (Ok(x), Ok(y), Ok(w), Ok(h)) = (
                parts[0].trim().parse::<i32>(),
                parts[1].trim().parse::<i32>(),
                parts[2].trim().parse::<u32>(),
                parts[3].trim().parse::<u32>(),
            ) {
                if w > 0 && h > 0 {
                    return Some(CaptureTarget::Region { x, y, w, h });
                }
            }
        }
    } else if let Some(dev) = rest.strip_prefix("v4l2/") {
        let d = dev.trim();
        if !d.is_empty() {
            let full = if d.starts_with('/') { d.to_string() } else { format!("/{d}") };
            return Some(CaptureTarget::V4l2(full));
        }
    }
    None
}

// ------------------------------------------------------------------ Window & Device Discovery

#[derive(Clone, Debug)]
pub struct WindowInfo {
    pub class: String,
    pub title: String,
    pub x: i32,
    pub y: i32,
    pub w: u32,
    pub h: u32,
}

impl WindowInfo {
    pub fn label(&self) -> String {
        if self.title.is_empty() {
            self.class.clone()
        } else if self.class.is_empty() {
            self.title.clone()
        } else {
            format!("{} — {}", self.class, self.title)
        }
    }

    pub fn match_key(&self) -> String {
        if !self.class.is_empty() {
            self.class.clone()
        } else {
            self.title.clone()
        }
    }
}

/// Lists open desktop windows on Hyprland (via `hyprctl clients -j`).
pub fn list_open_windows() -> Vec<WindowInfo> {
    let Ok(out) = Command::new("hyprctl").args(["clients", "-j"]).output() else {
        return Vec::new();
    };
    if !out.status.success() {
        return Vec::new();
    }
    let Ok(val) = serde_json::from_slice::<serde_json::Value>(&out.stdout) else {
        return Vec::new();
    };
    let Some(arr) = val.as_array() else { return Vec::new() };
    let mut windows = Vec::new();
    for item in arr {
        let class = item.get("class").and_then(|v| v.as_str()).unwrap_or("").to_string();
        let title = item.get("title").and_then(|v| v.as_str()).unwrap_or("").to_string();
        if class.eq_ignore_ascii_case("victr") {
            continue;
        }
        let at = item.get("at").and_then(|v| v.as_array());
        let sz = item.get("size").and_then(|v| v.as_array());
        let (Some(at), Some(sz)) = (at, sz) else { continue };
        let x = at.first().and_then(|v| v.as_i64()).unwrap_or(0) as i32;
        let y = at.get(1).and_then(|v| v.as_i64()).unwrap_or(0) as i32;
        let w = sz.first().and_then(|v| v.as_i64()).unwrap_or(0).max(0) as u32;
        let h = sz.get(1).and_then(|v| v.as_i64()).unwrap_or(0).max(0) as u32;
        if w >= 64 && h >= 64 {
            windows.push(WindowInfo { class, title, x, y, w, h });
        }
    }
    windows
}

/// Lists connected Wayland outputs on Hyprland (via `hyprctl monitors -j`).
pub fn list_monitors() -> Vec<String> {
    let Ok(out) = Command::new("hyprctl").args(["monitors", "-j"]).output() else {
        return Vec::new();
    };
    if !out.status.success() {
        return Vec::new();
    }
    let Ok(val) = serde_json::from_slice::<serde_json::Value>(&out.stdout) else {
        return Vec::new();
    };
    val.as_array()
        .map(|arr| {
            arr.iter()
                .filter_map(|m| m.get("name").and_then(|n| n.as_str()).map(|s| s.to_string()))
                .collect()
        })
        .unwrap_or_default()
}

/// Lists available Linux V4L2 video capture devices (`/dev/video*`).
pub fn list_v4l2_devices() -> Vec<String> {
    let Ok(entries) = std::fs::read_dir("/dev") else { return Vec::new() };
    let mut devs = Vec::new();
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().to_string();
        if name.starts_with("video") && name[5..].chars().all(|c| c.is_ascii_digit()) {
            devs.push(format!("/dev/{name}"));
        }
    }
    devs.sort();
    devs
}

pub fn render_waiting_frame(title: &str, detail: &str) -> Arc<(usize, usize, Vec<u8>)> {
    let mut c = Canvas::new(CAP_W, CAP_H);
    c.fill_vgrad(0, 0, CAP_W as i32, CAP_H as i32, [12, 36, 138, 255], [8, 22, 98, 255]);
    c.draw_text(44, 40, "AV 1  EXTERNAL INPUT", 2, 2, [96, 255, 134, 255], Some([6, 14, 48, 255]));
    c.draw_text_clipped(60, 194, title, 520, 2, 2, [248, 212, 74, 255], Some([6, 14, 48, 255]));
    if !detail.is_empty() {
        c.draw_text_clipped(60, 250, detail, 520, 1, 2, [224, 238, 255, 255], None);
    }
    Arc::new((CAP_W, CAP_H, c.rgba))
}

// ------------------------------------------------------------------ Live Capture Player

pub struct CaptureShared {
    pub frame: Option<Arc<(usize, usize, Vec<u8>)>>,
    pub frame_no: u64,
    pub status: String,
    pub running: bool,
}

pub struct CapturePlayer {
    pub shared: Arc<Mutex<CaptureShared>>,
    stop: Arc<AtomicBool>,
    child: Arc<Mutex<Option<Child>>>,
}

impl Drop for CapturePlayer {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(mut child) = self.child.lock().unwrap().take() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

impl CapturePlayer {
    pub fn start(capture_url: &str) -> Self {
        let target = parse_capture_url(capture_url);
        let shared = Arc::new(Mutex::new(CaptureShared {
            frame: Some(render_waiting_frame("LOCKING VIDEO INPUT...", capture_url)),
            frame_no: 1,
            status: "Connecting to capture source…".into(),
            running: true,
        }));
        let stop = Arc::new(AtomicBool::new(false));
        let child_slot: Arc<Mutex<Option<Child>>> = Arc::new(Mutex::new(None));

        let sh_bg = shared.clone();
        let stop_bg = stop.clone();
        let child_bg = child_slot.clone();

        std::thread::spawn(move || {
            let Some(target) = target else {
                let mut g = sh_bg.lock().unwrap();
                g.status = "Invalid capture:// URL.".into();
                g.running = false;
                return;
            };
            run_capture_loop(target, &sh_bg, &stop_bg, &child_bg);
        });

        Self {
            shared,
            stop,
            child: child_slot,
        }
    }
}

fn spawn_capture_process(target: &CaptureTarget) -> Result<(Child, String), String> {
    match target {
        CaptureTarget::Window(query) => {
            let low = query.to_ascii_lowercase();
            let wins = list_open_windows();
            let win = wins
                .iter()
                .find(|w| {
                    w.class.to_ascii_lowercase().contains(&low)
                        || w.title.to_ascii_lowercase().contains(&low)
                })
                .ok_or_else(|| format!("WAITING FOR WINDOW: {query}"))?;
            let geom = format!("{},{} {}x{}", win.x, win.y, win.w, win.h);
            let child = spawn_wf_recorder(Some(&geom), None)?;
            Ok((child, format!("Capturing {} ({}x{})", win.label(), win.w, win.h)))
        }
        CaptureTarget::Region { x, y, w, h } => {
            let geom = format!("{x},{y} {w}x{h}");
            let child = spawn_wf_recorder(Some(&geom), None)?;
            Ok((child, format!("Capturing region {geom}")))
        }
        CaptureTarget::Output(mon) => {
            let child = spawn_wf_recorder(None, Some(mon))?;
            Ok((child, format!("Capturing display {mon}")))
        }
        CaptureTarget::V4l2(dev) => {
            let child = Command::new("ffmpeg")
                .args([
                    "-hide_banner",
                    "-loglevel",
                    "error",
                    "-f",
                    "v4l2",
                    "-i",
                    dev,
                    "-vf",
                    "scale=640:480,format=rgba",
                    "-f",
                    "rawvideo",
                    "-pix_fmt",
                    "rgba",
                    "pipe:1",
                ])
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::null())
                .spawn()
                .map_err(|e| format!("ffmpeg v4l2 failed: {e}"))?;
            Ok((child, format!("Live V4L2 capture ({dev})")))
        }
    }
}

fn spawn_wf_recorder(geom: Option<&str>, output: Option<&str>) -> Result<Child, String> {
    let mut cmd = Command::new("wf-recorder");
    cmd.arg("-y");
    if let Some(g) = geom {
        cmd.args(["-g", g]);
    }
    if let Some(o) = output {
        cmd.args(["-o", o]);
    }
    cmd.args([
        "-r",
        "60",
        "-c",
        "rawvideo",
        "-m",
        "rawvideo",
        "-x",
        "rgba",
        "-F",
        "scale=640:480,format=rgba",
        "-f",
        "/dev/stdout",
    ])
    .stdin(Stdio::null())
    .stdout(Stdio::piped())
    .stderr(Stdio::null());
    cmd.spawn()
        .map_err(|e| format!("wf-recorder failed to start (install wf-recorder for Wayland capture): {e}"))
}

fn run_capture_loop(
    target: CaptureTarget,
    shared: &Arc<Mutex<CaptureShared>>,
    stop: &Arc<AtomicBool>,
    child_slot: &Arc<Mutex<Option<Child>>>,
) {
    let mut buf = vec![0u8; FRAME_BYTES];
    while !stop.load(Ordering::Relaxed) {
        let (mut child, live_status) = match spawn_capture_process(&target) {
            Ok(pair) => pair,
            Err(msg) => {
                {
                    let mut g = shared.lock().unwrap();
                    g.status = msg.clone();
                    g.frame_no += 1;
                    g.frame = Some(render_waiting_frame("NO SIGNAL — WAITING FOR SOURCE", &msg));
                }
                for _ in 0..16 {
                    if stop.load(Ordering::Relaxed) {
                        return;
                    }
                    std::thread::sleep(Duration::from_millis(50));
                }
                continue;
            }
        };

        let Some(mut stdout) = child.stdout.take() else {
            let _ = child.kill();
            let _ = child.wait();
            return;
        };
        *child_slot.lock().unwrap() = Some(child);
        shared.lock().unwrap().status = live_status;

        while !stop.load(Ordering::Relaxed) {
            if stdout.read_exact(&mut buf).is_err() {
                break;
            }
            let frame = Arc::new((CAP_W, CAP_H, buf.clone()));
            let mut g = shared.lock().unwrap();
            g.frame_no += 1;
            g.frame = Some(frame);
        }

        if let Some(mut c) = child_slot.lock().unwrap().take() {
            let _ = c.kill();
            let _ = c.wait();
        }
        if stop.load(Ordering::Relaxed) {
            break;
        }
        std::thread::sleep(Duration::from_millis(400));
    }
    shared.lock().unwrap().running = false;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_all_capture_url_variants() {
        assert!(is_capture_url("capture://window/dolphin-emu"));
        assert_eq!(
            parse_capture_url("capture://window/dolphin-emu"),
            Some(CaptureTarget::Window("dolphin-emu".into()))
        );
        assert_eq!(
            parse_capture_url("capture://output/DP-1"),
            Some(CaptureTarget::Output("DP-1".into()))
        );
        assert_eq!(
            parse_capture_url("capture://region/100,50,1280,960"),
            Some(CaptureTarget::Region { x: 100, y: 50, w: 1280, h: 960 })
        );
        assert_eq!(
            parse_capture_url("capture://v4l2/dev/video0"),
            Some(CaptureTarget::V4l2("/dev/video0".into()))
        );
    }
}
