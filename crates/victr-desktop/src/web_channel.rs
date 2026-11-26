//! Live Web Page Channel receiver for ViCTR (e.g. WeatherStar 4000+ at `4000.weatherstar.dev`).
//!
//! Launches a headless Chromium/Chrome/Brave/Edge process at 640×480 and streams
//! live composited frames via Chrome DevTools Protocol (`Page.startScreencast`)
//! over a local loopback RFC 6455 WebSocket — zero external servers or extra crates.
//! Frames publish directly into ViCTR's NTSC/CRT physics pipeline so web emulators
//! get real composite chroma bleed, scanlines, shadow mask, halation, and TV OSD.

use crate::provider::MediaItem;
use crate::retro::{self, LinuxGamepad};
use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpStream;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

pub const WEB_W: u32 = 640;
pub const WEB_H: u32 = 480;

pub const WEATHERSTAR_PRESET_URL: &str =
    "web://https://4000.weatherstar.dev/?kiosk=true&wide=false&scanLines=false&mediaPlaying=true";

pub const PUZZLESCRIPT_GALLERY_DAT_URL: &str = "https://www.puzzlescript.net/games_dat.js";

pub const PICO8_BBS_FEATURED_URL: &str =
    "https://www.lexaloffle.com/bbs/lister.php?use_hurl=1&cat=7&sub=2&mode=carts&orderby=featured";

pub fn is_web_channel_url(src: &str) -> bool {
    let s = src.trim();
    s.len() >= 6 && s[..6].eq_ignore_ascii_case("web://")
}

/// Returns `true` when `raw` refers to the Lexaloffle PICO-8 BBS cart listing
/// (as opposed to an individual cart `?pid=...` or `widget.php` URL).
pub fn is_pico8_gallery_url(raw: &str) -> bool {
    let norm = normalize_web_target_url(raw).to_ascii_lowercase();
    if norm.contains("pid=") || norm.contains("widget.php") {
        return false;
    }
    norm.contains("lexaloffle.com/bbs")
        && (norm.contains("cat=7") || norm.contains("lister.php") || norm.ends_with("/bbs") || norm.ends_with("/bbs/"))
}

/// Resolves a PICO-8 BBS gallery URL & optional mode (`"featured"`, `"new"`, `"lucky"`) to its `lister.php` URL.
pub fn pico8_lister_url_for(raw: &str, mode: &str) -> String {
    let trimmed = raw.trim();
    if !trimmed.is_empty() && trimmed.to_ascii_lowercase().contains("lister.php") {
        return normalize_web_target_url(trimmed);
    }
    match mode.trim().to_ascii_lowercase().as_str() {
        "new" | "latest" | "recent" => {
            "https://www.lexaloffle.com/bbs/lister.php?use_hurl=1&cat=7&sub=2&mode=carts&orderby=ts".to_string()
        }
        "lucky" | "random" => {
            "https://www.lexaloffle.com/bbs/lister.php?use_hurl=1&cat=7&sub=2&mode=carts&orderby=lucky".to_string()
        }
        _ => PICO8_BBS_FEATURED_URL.to_string(),
    }
}

/// Returns `true` when `raw` refers to the PuzzleScript Gallery index or `games_dat.js`
/// (as opposed to a specific game's `play.html?p=...` URL).
pub fn is_puzzlescript_gallery_url(raw: &str) -> bool {
    let norm = normalize_web_target_url(raw).to_ascii_lowercase();
    let trimmed = norm.trim_end_matches('/');
    if trimmed.contains("play.html") || trimmed.contains("editor.html") {
        return false;
    }
    trimmed.ends_with("games_dat.js")
        || trimmed == "https://www.puzzlescript.net"
        || trimmed == "http://www.puzzlescript.net"
        || trimmed == "https://puzzlescript.net"
        || trimmed == "http://puzzlescript.net"
        || trimmed.ends_with("puzzlescript.net/gallery")
        || trimmed.ends_with("puzzlescript.net/gallery/index.html")
}

/// Resolves a PuzzleScript gallery URL or empty string to its `games_dat.js` endpoint.
pub fn puzzlescript_dat_url_for(raw: &str) -> String {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return PUZZLESCRIPT_GALLERY_DAT_URL.to_string();
    }
    let norm = normalize_web_target_url(trimmed);
    if norm.to_ascii_lowercase().ends_with("games_dat.js") {
        norm
    } else if let Some(idx) = norm.to_ascii_lowercase().find("/gallery") {
        format!("{}/games_dat.js", norm[..idx].trim_end_matches('/'))
    } else {
        format!("{}/games_dat.js", norm.trim_end_matches('/'))
    }
}

/// Converts `web://https://4000.weatherstar.dev` or `web://4000.weatherstar.dev` into an HTTP(S) URL.
pub fn normalize_web_target_url(src: &str) -> String {
    let s = src.trim();
    let rest = if is_web_channel_url(s) { &s[6..] } else { s }.trim();
    if rest.starts_with("http://") || rest.starts_with("https://") || rest.starts_with("data:") || rest.starts_with("file://") {
        rest.to_string()
    } else {
        format!("https://{rest}")
    }
}

/// Ensures a user-entered web channel URL carries the `web://` marker scheme.
pub fn ensure_web_prefix(raw: &str) -> String {
    let s = raw.trim();
    if s.is_empty() {
        return String::new();
    }
    if is_web_channel_url(s) {
        s.to_string()
    } else if s.starts_with("http://") || s.starts_with("https://") {
        format!("web://{s}")
    } else {
        format!("web://https://{s}")
    }
}

pub enum WebInput {
    Key {
        down: bool,
        key: String,
        code: String,
        vk: i32,
        text: Option<String>,
    },
    MouseClick {
        x: f32,
        y: f32,
        button: &'static str,
    },
    Scroll {
        x: f32,
        y: f32,
        dx: f32,
        dy: f32,
    },
    SetGamepad(String),
}

pub struct WebShared {
    pub frame: Option<Arc<(usize, usize, Vec<u8>)>>,
    pub frame_no: u64,
    pub status: String,
    pub active_gamepad: String,
    pub running: bool,
}

pub struct WebPlayer {
    pub shared: Arc<Mutex<WebShared>>,
    tx: Sender<WebInput>,
    stop: Arc<AtomicBool>,
    child: Arc<Mutex<Option<Child>>>,
    profile_dir: Option<PathBuf>,
}

impl Drop for WebPlayer {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(mut child) = self.child.lock().unwrap().take() {
            let _ = child.kill();
            let _ = child.wait();
        }
        if let Some(dir) = self.profile_dir.take() {
            let _ = std::fs::remove_dir_all(dir);
        }
    }
}

impl WebPlayer {
    pub fn start(web_url: &str, muted: bool, gamepad_device: String) -> Self {
        let target_url = normalize_web_target_url(web_url);
        let shared = Arc::new(Mutex::new(WebShared {
            frame: None,
            frame_no: 0,
            status: "Starting web channel…".into(),
            active_gamepad: String::new(),
            running: true,
        }));
        let (tx, rx) = channel();
        let stop = Arc::new(AtomicBool::new(false));
        let child_slot: Arc<Mutex<Option<Child>>> = Arc::new(Mutex::new(None));

        let ts = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let profile_dir = std::env::temp_dir().join(format!("victr-web-{}-{ts}", std::process::id()));

        let sh_bg = shared.clone();
        let stop_bg = stop.clone();
        let child_bg = child_slot.clone();
        let prof_bg = profile_dir.clone();

        std::thread::spawn(move || {
            if let Err(err) = run_web_channel(
                &target_url,
                muted,
                gamepad_device,
                &prof_bg,
                &sh_bg,
                rx,
                &stop_bg,
                &child_bg,
            ) {
                let mut g = sh_bg.lock().unwrap();
                g.status = err;
                g.running = false;
            }
        });

        Self {
            shared,
            tx,
            stop,
            child: child_slot,
            profile_dir: Some(profile_dir),
        }
    }

    pub fn send_key(&self, down: bool, key: &str, code: &str, vk: i32, text: Option<&str>) {
        let _ = self.tx.send(WebInput::Key {
            down,
            key: key.to_string(),
            code: code.to_string(),
            vk,
            text: text.map(str::to_string),
        });
    }

    pub fn send_mouse_click(&self, x: f32, y: f32, button: &'static str) {
        let _ = self.tx.send(WebInput::MouseClick { x, y, button });
    }

    pub fn send_scroll(&self, x: f32, y: f32, dx: f32, dy: f32) {
        let _ = self.tx.send(WebInput::Scroll { x, y, dx, dy });
    }

    pub fn set_gamepad(&self, dev: String) {
        let _ = self.tx.send(WebInput::SetGamepad(dev));
    }
}

fn find_chromium_binary() -> Option<PathBuf> {
    if let Some(env_bin) = std::env::var_os("VICTR_BROWSER").filter(|v| !v.is_empty()) {
        let p = PathBuf::from(env_bin);
        if p.exists() {
            return Some(p);
        }
    }
    let candidates: &[&str] = &[
        "chromium",
        "chromium-browser",
        "google-chrome-stable",
        "google-chrome",
        "brave-browser",
        "brave",
        "microsoft-edge-stable",
        "microsoft-edge",
    ];
    for bin in candidates {
        if let Some(found) = which_in_path(bin) {
            return Some(found);
        }
    }
    #[cfg(target_os = "macos")]
    for p in [
        "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome",
        "/Applications/Chromium.app/Contents/MacOS/Chromium",
        "/Applications/Brave Browser.app/Contents/MacOS/Brave Browser",
    ] {
        if Path::new(p).is_file() {
            return Some(PathBuf::from(p));
        }
    }
    #[cfg(target_os = "windows")]
    for p in [
        r"C:\Program Files\Google\Chrome\Application\chrome.exe",
        r"C:\Program Files (x86)\Google\Chrome\Application\chrome.exe",
        r"C:\Program Files (x86)\Microsoft\Edge\Application\msedge.exe",
        r"C:\Program Files\BraveSoftware\Brave-Browser\Application\brave.exe",
    ] {
        if Path::new(p).is_file() {
            return Some(PathBuf::from(p));
        }
    }
    None
}

fn which_in_path(name: &str) -> Option<PathBuf> {
    let path_var = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&path_var) {
        let cand = dir.join(name);
        if cand.is_file() {
            return Some(cand);
        }
    }
    None
}

fn run_web_channel(
    target_url: &str,
    muted: bool,
    gamepad_device: String,
    profile_dir: &Path,
    shared: &Arc<Mutex<WebShared>>,
    rx: Receiver<WebInput>,
    stop: &Arc<AtomicBool>,
    child_slot: &Arc<Mutex<Option<Child>>>,
) -> Result<(), String> {
    let browser = find_chromium_binary().ok_or_else(|| {
        "Web channel needs Chromium, Chrome, Brave, or Edge installed (or set VICTR_BROWSER).".to_string()
    })?;
    let _ = std::fs::create_dir_all(profile_dir);

    let mut cmd = Command::new(&browser);
    cmd.args([
        "--headless=new",
        "--remote-debugging-port=0",
        "--remote-allow-origins=*",
        &format!("--window-size={WEB_W},{WEB_H}"),
        "--autoplay-policy=no-user-gesture-required",
        "--hide-scrollbars",
        "--no-first-run",
        "--no-default-browser-check",
        "--disable-background-timer-throttling",
        "--disable-backgrounding-occluded-windows",
        "--disable-renderer-backgrounding",
        "--disable-infobars",
        "--disable-session-crashed-bubble",
        &format!("--user-data-dir={}", profile_dir.display()),
    ]);
    if muted {
        cmd.arg("--mute-audio");
    }
    cmd.arg(target_url)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped());

    let mut child = cmd
        .spawn()
        .map_err(|e| format!("Failed to launch {}: {e}", browser.display()))?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| "Couldn't read browser DevTools port.".to_string())?;
    *child_slot.lock().unwrap() = Some(child);

    // Wait for `DevTools listening on ws://127.0.0.1:PORT/...` on stderr
    let port_box: Arc<Mutex<Option<u16>>> = Arc::new(Mutex::new(None));
    let port_bg = port_box.clone();
    std::thread::spawn(move || {
        let reader = BufReader::new(stderr);
        for line in reader.lines().map_while(Result::ok) {
            if let Some(pos) = line.find("ws://127.0.0.1:") {
                let rest = &line[pos + "ws://127.0.0.1:".len()..];
                let digits: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
                if let Ok(p) = digits.parse::<u16>() {
                    *port_bg.lock().unwrap() = Some(p);
                }
            }
        }
    });

    let deadline = Instant::now() + Duration::from_secs(10);
    let port = loop {
        if stop.load(Ordering::SeqCst) {
            return Ok(());
        }
        if let Some(p) = *port_box.lock().unwrap() {
            break p;
        }
        if Instant::now() >= deadline {
            return Err("Timed out waiting for headless browser DevTools.".into());
        }
        std::thread::sleep(Duration::from_millis(40));
    };

    // Query `/json/list` for the page target's WebSocket URL
    let ws_url = fetch_page_ws_url(port, stop)?;
    let (host_port, ws_path) = parse_ws_url(&ws_url).ok_or_else(|| format!("Invalid DevTools WS URL: {ws_url}"))?;

    let mut ws = WsConn::connect(&host_port, &ws_path)?;
    let mut gamepad = LinuxGamepad::new(gamepad_device);
    {
        let mut g = shared.lock().unwrap();
        g.status = "Live web channel".into();
        g.active_gamepad = gamepad.active_summary();
    }

    // Force exact 4:3 640×480 viewport and start continuous CDP screencast
    ws.send_text(
        r#"{"id":1,"method":"Emulation.setDeviceMetricsOverride","params":{"width":640,"height":480,"deviceScaleFactor":1,"mobile":false}}"#,
    )?;
    ws.send_text(r#"{"id":2,"method":"Page.enable","params":{}}"#)?;
    ws.send_text(
        r#"{"id":3,"method":"Page.startScreencast","params":{"format":"jpeg","quality":88,"maxWidth":640,"maxHeight":480,"everyNthFrame":1}}"#,
    )?;

    let mut next_id: u64 = 10;
    let mut prev_gp_mask: u16 = 0;
    let mut focused_canvas = false;

    let send_cdp_key = |ws: &mut WsConn, next_id: &mut u64, down: bool, key: &str, code: &str, vk: i32, text: Option<&str>| {
        let mut params = serde_json::json!({
            "type": if down { "keyDown" } else { "keyUp" },
            "key": key,
            "code": code,
            "windowsVirtualKeyCode": vk,
            "nativeVirtualKeyCode": vk,
        });
        if down {
            if let Some(t) = text.filter(|s| !s.is_empty()) {
                params["text"] = serde_json::Value::String(t.to_string());
                params["unmodifiedText"] = serde_json::Value::String(t.to_string());
            }
        }
        let msg = serde_json::json!({
            "id": *next_id,
            "method": "Input.dispatchKeyEvent",
            "params": params,
        });
        *next_id += 1;
        let _ = ws.send_text(&msg.to_string());
    };

    let is_pico8_page = target_url.to_ascii_lowercase().contains("lexaloffle.com/bbs/");
    let focus_page = |ws: &mut WsConn, next_id: &mut u64| {
        let expr = if is_pico8_page {
            // Auto-boot the Lexaloffle PICO-8 WASM player and center its 128×128 canvas cleanly
            // at crisp 480×480 pixelated scale on pure black inside our 640×480 CRT frame.
            r#"(() => {
                const d = document.getElementById('dormant_label_container') || document.querySelector('.dormant_player');
                const c = document.getElementById('p8_container');
                if (d) d.click();
                if (c) c.click();
                if (!document.getElementById('victr-p8-style')) {
                    const st = document.createElement('style');
                    st.id = 'victr-p8-style';
                    st.textContent = 'html,body{margin:0!important;padding:0!important;overflow:hidden!important;background:#000!important;width:640px!important;height:480px!important;} #p8_frame_0,#p8_frame,#p8_container{position:fixed!important;inset:0!important;width:640px!important;height:480px!important;max-width:none!important;max-height:none!important;margin:0!important;background:#000!important;display:flex!important;align-items:center!important;justify-content:center!important;z-index:99999!important;} #p8_menu_buttons,.p8_menu_buttons,.dormant_player{display:none!important;} #canvas{width:480px!important;height:480px!important;image-rendering:pixelated!important;}';
                    document.head.appendChild(st);
                }
                window.focus();
                (document.getElementById('canvas') || document.querySelector('canvas') || document.body)?.focus?.();
            })()"#
        } else {
            "window.focus(); (document.querySelector('canvas') || document.body)?.focus?.();"
        };
        let msg = serde_json::json!({
            "id": *next_id,
            "method": "Runtime.evaluate",
            "params": { "expression": expr }
        });
        *next_id += 1;
        let _ = ws.send_text(&msg.to_string());
    };

    let gp_bindings: &[(u16, &str, &str, i32, Option<&str>)] = &[
        (retro::BTN_UP, "ArrowUp", "ArrowUp", 38, None),
        (retro::BTN_DOWN, "ArrowDown", "ArrowDown", 40, None),
        (retro::BTN_LEFT, "ArrowLeft", "ArrowLeft", 37, None),
        (retro::BTN_RIGHT, "ArrowRight", "ArrowRight", 39, None),
        (retro::BTN_A, "x", "KeyX", 88, Some("x")),
        (retro::BTN_B, "z", "KeyZ", 90, Some("z")),
        (retro::BTN_X, " ", "Space", 32, Some(" ")),
        (retro::BTN_Y, "r", "KeyR", 82, Some("r")),
        (retro::BTN_START, "Enter", "Enter", 13, Some("\r")),
        (retro::BTN_SELECT, "Escape", "Escape", 27, None),
        (retro::BTN_L, "z", "KeyZ", 90, Some("z")),
        (retro::BTN_R, "r", "KeyR", 82, Some("r")),
    ];

    let drain_inputs = |ws: &mut WsConn,
                        next_id: &mut u64,
                        rx: &Receiver<WebInput>,
                        gamepad: &mut LinuxGamepad,
                        prev_gp_mask: &mut u16,
                        shared: &Arc<Mutex<WebShared>>| {
        while let Ok(inp) = rx.try_recv() {
            match inp {
                WebInput::Key { down, key, code, vk, text } => {
                    send_cdp_key(ws, next_id, down, &key, &code, vk, text.as_deref());
                }
                WebInput::MouseClick { x, y, button } => {
                    let cx = x.clamp(0.0, (WEB_W - 1) as f32);
                    let cy = y.clamp(0.0, (WEB_H - 1) as f32);
                    for (ty, clicks) in [("mouseMoved", 0), ("mousePressed", 1), ("mouseReleased", 1)] {
                        let msg = serde_json::json!({
                            "id": *next_id,
                            "method": "Input.dispatchMouseEvent",
                            "params": {
                                "type": ty,
                                "x": cx,
                                "y": cy,
                                "button": if ty == "mouseMoved" { "none" } else { button },
                                "clickCount": clicks,
                            }
                        });
                        *next_id += 1;
                        let _ = ws.send_text(&msg.to_string());
                    }
                }
                WebInput::Scroll { x, y, dx, dy } => {
                    let msg = serde_json::json!({
                        "id": *next_id,
                        "method": "Input.dispatchMouseEvent",
                        "params": {
                            "type": "mouseWheel",
                            "x": x.clamp(0.0, (WEB_W - 1) as f32),
                            "y": y.clamp(0.0, (WEB_H - 1) as f32),
                            "deltaX": dx,
                            "deltaY": dy,
                        }
                    });
                    *next_id += 1;
                    let _ = ws.send_text(&msg.to_string());
                }
                WebInput::SetGamepad(dev) => {
                    gamepad.set_selected(dev);
                }
            }
        }
        let gp_mask = gamepad.poll_mask();
        if let Some(summary) = gamepad.take_changed_summary() {
            shared.lock().unwrap().active_gamepad = summary;
        }
        let diff = gp_mask ^ *prev_gp_mask;
        if diff != 0 {
            for &(bit, key, code, vk, text) in gp_bindings {
                if (diff & bit) != 0 {
                    let down = (gp_mask & bit) != 0;
                    send_cdp_key(ws, next_id, down, key, code, vk, text);
                }
            }
            *prev_gp_mask = gp_mask;
        }
    };

    while !stop.load(Ordering::SeqCst) {
        drain_inputs(&mut ws, &mut next_id, &rx, &mut gamepad, &mut prev_gp_mask, shared);
        match ws.read_text_frame() {
            Ok(Some(msg)) => {
                let Ok(val) = serde_json::from_str::<serde_json::Value>(&msg) else { continue };
                match val.get("method").and_then(|m| m.as_str()) {
                    Some("Page.loadEventFired") => {
                        focus_page(&mut ws, &mut next_id);
                        focused_canvas = true;
                    }
                    Some("Page.screencastFrame") => {
                        if let Some(params) = val.get("params") {
                            let session_id = params.get("sessionId").and_then(|s| s.as_i64()).unwrap_or(0);
                            // Acknowledge immediately so Chromium keeps pushing smoothly
                            let ack = format!(
                                r#"{{"id":{next_id},"method":"Page.screencastFrameAck","params":{{"sessionId":{session_id}}}}}"#
                            );
                            next_id += 1;
                            let _ = ws.send_text(&ack);

                            if let Some(b64) = params.get("data").and_then(|d| d.as_str()) {
                                if let Some(jpeg) = decode_base64(b64) {
                                    if let Ok(img) = image::load_from_memory(&jpeg) {
                                        let rgba = img.to_rgba8();
                                        let (w, h) = rgba.dimensions();
                                        if w > 0 && h > 0 {
                                            let mut g = shared.lock().unwrap();
                                            g.frame_no += 1;
                                            g.frame = Some(Arc::new((w as usize, h as usize, rgba.into_raw())));
                                        }
                                    }
                                }
                            }
                            if !focused_canvas {
                                focus_page(&mut ws, &mut next_id);
                                focused_canvas = true;
                            }
                        }
                    }
                    _ => {}
                }
            }
            Ok(None) => {
                // Short read timeout — loop immediately to keep input latency < 10 ms
            }
            Err(_) => {
                if !stop.load(Ordering::SeqCst) {
                    return Err("Web channel stream closed.".into());
                }
                break;
            }
        }
    }
    Ok(())
}

fn fetch_page_ws_url(port: u16, stop: &Arc<AtomicBool>) -> Result<String, String> {
    let agent = ureq::AgentBuilder::new()
        .timeout_connect(Duration::from_secs(2))
        .timeout(Duration::from_secs(3))
        .build();
    let url = format!("http://127.0.0.1:{port}/json/list");
    let deadline = Instant::now() + Duration::from_secs(8);
    while Instant::now() < deadline {
        if stop.load(Ordering::SeqCst) {
            return Err("Stopped.".into());
        }
        if let Ok(resp) = agent.get(&url).call() {
            if let Ok(list) = resp.into_json::<Vec<serde_json::Value>>() {
                if let Some(ws) = list
                    .iter()
                    .find(|t| t.get("type").and_then(|v| v.as_str()) == Some("page"))
                    .and_then(|t| t.get("webSocketDebuggerUrl").and_then(|v| v.as_str()))
                {
                    return Ok(ws.to_string());
                }
            }
        }
        std::thread::sleep(Duration::from_millis(60));
    }
    Err("Couldn't find browser page target.".into())
}

fn parse_ws_url(url: &str) -> Option<(String, String)> {
    let rest = url.strip_prefix("ws://")?;
    let (host_port, path) = match rest.find('/') {
        Some(idx) => (&rest[..idx], &rest[idx..]),
        None => (rest, "/"),
    };
    Some((host_port.to_string(), path.to_string()))
}

// ------------------------------------------------------------------ Minimal RFC 6455 WebSocket Client

struct WsConn {
    stream: TcpStream,
    buf: Vec<u8>,
}

impl WsConn {
    fn connect(host_port: &str, path: &str) -> Result<Self, String> {
        let mut stream =
            TcpStream::connect(host_port).map_err(|e| format!("CDP socket connect failed: {e}"))?;
        let _ = stream.set_read_timeout(Some(Duration::from_millis(8)));
        let _ = stream.set_write_timeout(Some(Duration::from_secs(3)));

        let req = format!(
            "GET {path} HTTP/1.1\r\n\
             Host: {host_port}\r\n\
             Upgrade: websocket\r\n\
             Connection: Upgrade\r\n\
             Sec-WebSocket-Key: dmljdHItY3J0LXdlYi1jaA==\r\n\
             Sec-WebSocket-Version: 13\r\n\r\n"
        );
        stream
            .write_all(req.as_bytes())
            .map_err(|e| format!("CDP handshake write failed: {e}"))?;

        let mut hdr = Vec::with_capacity(512);
        let mut tmp = [0u8; 512];
        let header_end = loop {
            let n = stream
                .read(&mut tmp)
                .map_err(|e| format!("CDP handshake read failed: {e}"))?;
            if n == 0 {
                return Err("CDP closed during handshake.".into());
            }
            hdr.extend_from_slice(&tmp[..n]);
            if let Some(pos) = hdr.windows(4).position(|w| w == b"\r\n\r\n") {
                break pos + 4;
            }
            if hdr.len() > 16384 {
                return Err("CDP handshake header too large.".into());
            }
        };
        let status_line = String::from_utf8_lossy(&hdr[..header_end]);
        if !status_line.contains("101") {
            return Err(format!("CDP WebSocket upgrade rejected: {}", status_line.lines().next().unwrap_or("")));
        }
        let leftover = hdr[header_end..].to_vec();
        Ok(Self { stream, buf: leftover })
    }

    fn send_text(&mut self, text: &str) -> Result<(), String> {
        let frame = encode_client_frame(0x1, text.as_bytes());
        self.stream
            .write_all(&frame)
            .map_err(|e| format!("CDP write failed: {e}"))
    }

    fn send_pong(&mut self, payload: &[u8]) {
        let frame = encode_client_frame(0xA, payload);
        let _ = self.stream.write_all(&frame);
    }

    fn read_exact_buffered(&mut self, n: usize) -> Result<Option<Vec<u8>>, String> {
        while self.buf.len() < n {
            let mut tmp = [0u8; 16384];
            match self.stream.read(&mut tmp) {
                Ok(0) => return Err("EOF".into()),
                Ok(k) => self.buf.extend_from_slice(&tmp[..k]),
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock || e.kind() == std::io::ErrorKind::TimedOut => {
                    if self.buf.is_empty() {
                        return Ok(None);
                    }
                }
                Err(e) => return Err(e.to_string()),
            }
        }
        let out: Vec<u8> = self.buf.drain(..n).collect();
        Ok(Some(out))
    }

    fn read_text_frame(&mut self) -> Result<Option<String>, String> {
        let mut assembled: Vec<u8> = Vec::new();
        loop {
            let Some(hdr) = self.read_exact_buffered(2)? else {
                if assembled.is_empty() {
                    return Ok(None);
                }
                continue;
            };
            let fin = (hdr[0] & 0x80) != 0;
            let opcode = hdr[0] & 0x0F;
            let masked = (hdr[1] & 0x80) != 0;
            let mut len = (hdr[1] & 0x7F) as u64;
            if len == 126 {
                let ext = self.read_exact_buffered(2)?.ok_or("truncated len16")?;
                len = u16::from_be_bytes([ext[0], ext[1]]) as u64;
            } else if len == 127 {
                let ext = self.read_exact_buffered(8)?.ok_or("truncated len64")?;
                len = u64::from_be_bytes([ext[0], ext[1], ext[2], ext[3], ext[4], ext[5], ext[6], ext[7]]);
            }
            if len > 16 * 1024 * 1024 {
                return Err("WS frame too large".into());
            }
            let mask_key = if masked {
                let m = self.read_exact_buffered(4)?.ok_or("truncated mask")?;
                Some([m[0], m[1], m[2], m[3]])
            } else {
                None
            };
            let mut payload = if len > 0 {
                self.read_exact_buffered(len as usize)?.ok_or("truncated payload")?
            } else {
                Vec::new()
            };
            if let Some(k) = mask_key {
                for (i, b) in payload.iter_mut().enumerate() {
                    *b ^= k[i & 3];
                }
            }
            match opcode {
                0x8 => return Err("close frame".into()),
                0x9 => {
                    self.send_pong(&payload);
                    continue;
                }
                0xA => continue,
                0x1 | 0x0 => {
                    assembled.extend_from_slice(&payload);
                    if fin {
                        return Ok(Some(String::from_utf8_lossy(&assembled).to_string()));
                    }
                }
                _ => {
                    if fin && assembled.is_empty() {
                        return Ok(None);
                    }
                }
            }
        }
    }
}

fn encode_client_frame(opcode: u8, payload: &[u8]) -> Vec<u8> {
    let mask = [0x37u8, 0xFA, 0x21, 0x9C];
    let len = payload.len();
    let mut out = Vec::with_capacity(len + 14);
    out.push(0x80 | (opcode & 0x0F));
    if len < 126 {
        out.push(0x80 | len as u8);
    } else if len <= u16::MAX as usize {
        out.push(0x80 | 126);
        out.extend_from_slice(&(len as u16).to_be_bytes());
    } else {
        out.push(0x80 | 127);
        out.extend_from_slice(&(len as u64).to_be_bytes());
    }
    out.extend_from_slice(&mask);
    for (i, &b) in payload.iter().enumerate() {
        out.push(b ^ mask[i & 3]);
    }
    out
}

pub fn decode_base64(input: &str) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(input.len() * 3 / 4);
    let mut buf: u32 = 0;
    let mut bits: u32 = 0;
    for b in input.bytes() {
        let val = match b {
            b'A'..=b'Z' => (b - b'A') as u32,
            b'a'..=b'z' => (b - b'a' + 26) as u32,
            b'0'..=b'9' => (b - b'0' + 52) as u32,
            b'+' => 62,
            b'/' => 63,
            b'=' | b'\r' | b'\n' | b' ' | b'\t' => continue,
            _ => return None,
        };
        buf = (buf << 6) | val;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push(((buf >> bits) & 0xFF) as u8);
        }
    }
    Some(out)
}

// ------------------------------------------------------------------ PuzzleScript Gallery Loader

#[derive(serde::Deserialize)]
struct PsGalleryEntry {
    #[serde(default)]
    url: String,
    #[serde(default)]
    source: String,
    #[serde(default)]
    thumb: String,
    #[serde(default)]
    title: String,
    #[serde(default)]
    author: String,
    #[serde(rename = "NSFW", default)]
    nsfw: bool,
}

/// Strips illegal trailing commas before `]` or `}` outside JSON strings (present in `games_dat.js`).
fn strip_json_trailing_commas(input: &str) -> String {
    let bytes = input.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    let mut in_str = false;
    let mut esc = false;
    while i < bytes.len() {
        let b = bytes[i];
        if in_str {
            out.push(b);
            if esc {
                esc = false;
            } else if b == b'\\' {
                esc = true;
            } else if b == b'"' {
                in_str = false;
            }
            i += 1;
            continue;
        }
        if b == b'"' {
            in_str = true;
            out.push(b);
            i += 1;
            continue;
        }
        if b == b',' {
            let mut j = i + 1;
            while j < bytes.len() && bytes[j].is_ascii_whitespace() {
                j += 1;
            }
            if j < bytes.len() && (bytes[j] == b']' || bytes[j] == b'}') {
                i += 1;
                continue;
            }
        }
        out.push(b);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn extract_puzzlescript_gist_id(url: &str) -> Option<String> {
    for needle in ["?p=", "&p=", "?hack=", "&hack="] {
        if let Some(pos) = url.find(needle) {
            let rest = &url[pos + needle.len()..];
            let id: String = rest
                .chars()
                .take_while(|c| c.is_ascii_alphanumeric() || *c == '_' || *c == '-')
                .collect();
            if !id.is_empty() {
                return Some(id);
            }
        }
    }
    None
}

/// Parses PuzzleScript's `games_dat.js` into browsable, playable `MediaItem` entries
/// (newest first, matching the official gallery). Entries whose `url` points off-site
/// (e.g. itch.io) are redirected to the clean `play.html?p=<id>` player whenever a
/// PuzzleScript gist `source` is present.
pub fn parse_puzzlescript_games_dat(js: &str, dat_url: &str, include_nsfw: bool) -> Result<Vec<MediaItem>, String> {
    let start = js
        .find('[')
        .ok_or_else(|| "PuzzleScript games_dat.js missing '[' array start".to_string())?;
    let end = js
        .rfind(']')
        .ok_or_else(|| "PuzzleScript games_dat.js missing ']' array end".to_string())?;
    if end <= start {
        return Err("PuzzleScript games_dat.js array bounds invalid".into());
    }
    let cleaned = strip_json_trailing_commas(&js[start..=end]);
    let mut entries: Vec<PsGalleryEntry> =
        serde_json::from_str(&cleaned).map_err(|e| format!("Invalid PuzzleScript gallery JSON: {e}"))?;
    entries.reverse();

    let base = dat_url
        .rsplit_once('/')
        .map(|(b, _)| b.trim_end_matches('/'))
        .unwrap_or("https://www.puzzlescript.net");

    let mut out = Vec::with_capacity(entries.len());
    for (idx, e) in entries.into_iter().enumerate() {
        if e.nsfw && !include_nsfw {
            continue;
        }
        let title = e.title.trim();
        if title.is_empty() {
            continue;
        }
        let raw_url = e.url.trim();
        let play_url = if raw_url.contains("play.html") {
            raw_url.to_string()
        } else if let Some(id) = extract_puzzlescript_gist_id(e.source.trim()) {
            format!("{base}/play.html?p={id}")
        } else if !raw_url.is_empty() {
            raw_url.to_string()
        } else {
            continue;
        };

        let gist_id = extract_puzzlescript_gist_id(&play_url).unwrap_or_else(|| format!("{idx}"));
        let artwork_id = if e.thumb.trim().is_empty() {
            String::new()
        } else if e.thumb.starts_with("http://") || e.thumb.starts_with("https://") {
            e.thumb.trim().to_string()
        } else {
            format!("{base}/Gallery/gifs/{}", e.thumb.trim().trim_start_matches('/'))
        };
        let author = e.author.trim();
        let overview = if author.is_empty() {
            "PuzzleScript Gallery · Arrows/WASD move · X/Space action · Z undo · R restart".to_string()
        } else {
            format!("by {author} · Arrows/WASD move · X/Space action · Z undo · R restart")
        };
        out.push(MediaItem {
            id: format!("ps:{gist_id}"),
            title: title.to_string(),
            season: None,
            episode: None,
            overview,
            runtime_secs: 1800.0,
            played: false,
            resume_secs: 0.0,
            artwork_id,
            stream_url: ensure_web_prefix(&play_url),
        });
    }
    Ok(out)
}

pub fn puzzlescript_cache_path() -> Option<PathBuf> {
    crate::settings::presets_dir().and_then(|d| d.parent().map(|p| p.join("puzzlescript_gallery.cache.js")))
}

pub fn load_cached_puzzlescript_gallery(dat_url: &str, cache_path: &Path) -> Option<Vec<MediaItem>> {
    let text = std::fs::read_to_string(cache_path).ok()?;
    let items = parse_puzzlescript_games_dat(&text, dat_url, false).ok()?;
    if items.is_empty() { None } else { Some(items) }
}

pub fn fetch_puzzlescript_gallery(dat_url: &str, cache_path: Option<&Path>) -> Result<Vec<MediaItem>, String> {
    let agent = ureq::AgentBuilder::new()
        .timeout_connect(Duration::from_secs(6))
        .timeout(Duration::from_secs(12))
        .build();
    let resp = agent
        .get(dat_url)
        .call()
        .map_err(|e| format!("Couldn't fetch PuzzleScript gallery ({dat_url}): {e}"))?;
    let body = resp
        .into_string()
        .map_err(|e| format!("Couldn't read PuzzleScript gallery response: {e}"))?;
    let items = parse_puzzlescript_games_dat(&body, dat_url, false)?;
    if items.is_empty() {
        return Err("PuzzleScript gallery returned no playable games.".into());
    }
    if let Some(cp) = cache_path {
        if let Some(parent) = cp.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let _ = std::fs::write(cp, &body);
    }
    Ok(items)
}

// ------------------------------------------------------------------ PICO-8 BBS Carts Loader

#[derive(Debug, Clone)]
enum JsVal {
    Str(String),
    Arr(Vec<JsVal>),
    Other,
}

fn parse_js_string(bytes: &[u8], i: &mut usize, quote: u8) -> String {
    *i += 1; // skip opening quote
    let mut out = Vec::new();
    while *i < bytes.len() {
        let b = bytes[*i];
        *i += 1;
        if b == b'\\' && *i < bytes.len() {
            let next = bytes[*i];
            *i += 1;
            match next {
                b'n' => out.push(b'\n'),
                b'r' => out.push(b'\r'),
                b't' => out.push(b'\t'),
                other => out.push(other),
            }
            continue;
        }
        if b == quote {
            break;
        }
        out.push(b);
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn parse_js_array(bytes: &[u8], i: &mut usize) -> Vec<JsVal> {
    let mut items = Vec::new();
    if *i >= bytes.len() || bytes[*i] != b'[' {
        return items;
    }
    *i += 1; // skip '['
    loop {
        while *i < bytes.len() && bytes[*i].is_ascii_whitespace() {
            *i += 1;
        }
        if *i >= bytes.len() || bytes[*i] == b']' {
            if *i < bytes.len() {
                *i += 1;
            }
            break;
        }
        if bytes[*i] == b',' {
            *i += 1;
            continue;
        }
        let val = match bytes[*i] {
            q @ (b'\'' | b'"' | b'`') => JsVal::Str(parse_js_string(bytes, i, q)),
            b'[' => JsVal::Arr(parse_js_array(bytes, i)),
            _ => {
                while *i < bytes.len() && bytes[*i] != b',' && bytes[*i] != b']' {
                    *i += 1;
                }
                JsVal::Other
            }
        };
        items.push(val);
    }
    items
}

/// Parses Lexaloffle's `lister.php` `pdat=[ ... ]` JavaScript cart table into playable `MediaItem`s.
pub fn parse_pico8_bbs_lister(html: &str) -> Result<Vec<MediaItem>, String> {
    let Some(pos) = html.find("pdat=[") else {
        return Err("Lexaloffle BBS response missing pdat=[ array".into());
    };
    let bytes = html.as_bytes();
    let mut i = pos + "pdat=".len();
    let rows = parse_js_array(bytes, &mut i);
    let mut out = Vec::new();
    for row in rows {
        let JsVal::Arr(cols) = row else { continue };
        if cols.len() < 10 {
            continue;
        }
        let pid = match cols.first() {
            Some(JsVal::Str(s)) => s.trim().to_string(),
            _ => continue,
        };
        if pid.is_empty() {
            continue;
        }
        let title = match cols.get(2) {
            Some(JsVal::Str(s)) if !s.trim().is_empty() => s.trim().to_string(),
            _ => continue,
        };
        let thumb = match cols.get(3) {
            Some(JsVal::Str(s)) => s.trim().to_string(),
            _ => String::new(),
        };
        let author = match cols.get(8) {
            Some(JsVal::Str(s)) => s.trim().to_string(),
            _ => String::new(),
        };
        let tags: Vec<String> = match cols.get(18) {
            Some(JsVal::Arr(arr)) => arr
                .iter()
                .filter_map(|v| match v {
                    JsVal::Str(s) if !s.trim().is_empty() => Some(s.trim().to_string()),
                    _ => None,
                })
                .take(4)
                .collect(),
            _ => Vec::new(),
        };
        let artwork_id = if thumb.is_empty() {
            String::new()
        } else if thumb.starts_with("http://") || thumb.starts_with("https://") {
            thumb
        } else {
            format!("https://www.lexaloffle.com/{}", thumb.trim_start_matches('/'))
        };
        let tag_str = if tags.is_empty() {
            String::new()
        } else {
            format!(" [{}]", tags.join(", "))
        };
        let author_str = if author.is_empty() {
            "PICO-8 BBS".to_string()
        } else {
            format!("by {author}")
        };
        let overview = format!("{author_str}{tag_str} · Arrows move · Z/X action · Enter pause");
        let play_url = format!("https://www.lexaloffle.com/bbs/?pid={pid}#p");
        out.push(MediaItem {
            id: format!("pico8:{pid}"),
            title,
            season: None,
            episode: None,
            overview,
            runtime_secs: 1800.0,
            played: false,
            resume_secs: 0.0,
            artwork_id,
            stream_url: ensure_web_prefix(&play_url),
        });
    }
    Ok(out)
}

pub fn pico8_cache_path() -> Option<PathBuf> {
    crate::settings::presets_dir().and_then(|d| d.parent().map(|p| p.join("pico8_bbs.cache.json")))
}

pub fn load_cached_pico8_gallery(cache_path: &Path) -> Option<Vec<MediaItem>> {
    let text = std::fs::read_to_string(cache_path).ok()?;
    let items: Vec<MediaItem> = serde_json::from_str(&text).ok()?;
    if items.is_empty() { None } else { Some(items) }
}

pub fn fetch_pico8_gallery(lister_url: &str, cache_path: Option<&Path>) -> Result<Vec<MediaItem>, String> {
    let agent = ureq::AgentBuilder::new()
        .timeout_connect(Duration::from_secs(6))
        .timeout(Duration::from_secs(12))
        .build();
    let mut items = Vec::new();
    let mut seen = std::collections::HashSet::new();
    // Fetch pages 1 & 2 (~64 featured/recent carts) so the arcade feels rich immediately.
    for page in 1..=2 {
        let url = if page == 1 || lister_url.contains("page=") {
            lister_url.to_string()
        } else if lister_url.contains('?') {
            format!("{lister_url}&page={page}")
        } else {
            format!("{lister_url}?page={page}")
        };
        let resp = match agent.get(&url).call() {
            Ok(r) => r,
            Err(e) if page == 1 => return Err(format!("Couldn't fetch PICO-8 BBS ({url}): {e}")),
            Err(_) => break,
        };
        let Ok(body) = resp.into_string() else { break };
        if let Ok(page_items) = parse_pico8_bbs_lister(&body) {
            for m in page_items {
                if seen.insert(m.id.clone()) {
                    items.push(m);
                }
            }
        }
        if lister_url.contains("page=") {
            break;
        }
    }
    if items.is_empty() {
        return Err("PICO-8 BBS returned no playable carts.".into());
    }
    if let Some(cp) = cache_path {
        if let Some(parent) = cp.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        if let Ok(json) = serde_json::to_string(&items) {
            let _ = std::fs::write(cp, json);
        }
    }
    Ok(items)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn web_url_helpers_and_base64_decode_correctly() {
        assert!(is_web_channel_url("web://https://4000.weatherstar.dev"));
        assert!(!is_web_channel_url("https://example.com/live.m3u8"));
        assert_eq!(
            normalize_web_target_url("web://4000.weatherstar.dev/?kiosk=true"),
            "https://4000.weatherstar.dev/?kiosk=true"
        );
        assert_eq!(
            ensure_web_prefix("4000.weatherstar.dev"),
            "web://https://4000.weatherstar.dev"
        );
        assert!(is_puzzlescript_gallery_url("https://www.puzzlescript.net/Gallery/"));
        assert!(is_puzzlescript_gallery_url("https://www.puzzlescript.net"));
        assert!(!is_puzzlescript_gallery_url("https://www.puzzlescript.net/play.html?p=6841165"));
        assert_eq!(
            puzzlescript_dat_url_for("https://www.puzzlescript.net/Gallery/index.html"),
            "https://www.puzzlescript.net/games_dat.js"
        );
        assert_eq!(decode_base64("VmlDVFIgQ1JU").unwrap(), b"ViCTR CRT");
        assert_eq!(decode_base64("SGVsbG8=").unwrap(), b"Hello");
    }

    #[test]
    fn puzzlescript_games_dat_parses_trailing_commas_and_prefers_play_url() {
        let sample = r#"
//do not change go const
let games_gallery = [
  {
    "url": "https://www.puzzlescript.net/play.html?p=6841165",
    "thumb": "6841165.gif",
    "title": "2D Whale World",
    "author": "increpare"
  },
  {
    "url": "https://example.itch.io/midwinter/",
    "source": "https://www.puzzlescript.net/editor.html?hack=3e73c82343df",
    "thumb": "3e73c82343df.gif",
    "title": "Midwinter Springtime",
    "author": "pap4qlxxlevb",
  },
  {
    "url": "https://www.puzzlescript.net/play.html?p=999",
    "thumb": "999.gif",
    "title": "Skipped NSFW",
    "author": "anon",
    "NSFW": true
  },
];
"#;
        let items = parse_puzzlescript_games_dat(sample, PUZZLESCRIPT_GALLERY_DAT_URL, false).unwrap();
        assert_eq!(items.len(), 2);
        // Reversed so newest appears first
        assert_eq!(items[0].title, "Midwinter Springtime");
        assert_eq!(items[0].stream_url, "web://https://www.puzzlescript.net/play.html?p=3e73c82343df");
        assert_eq!(items[0].artwork_id, "https://www.puzzlescript.net/Gallery/gifs/3e73c82343df.gif");
        assert!(items[0].overview.contains("by pap4qlxxlevb"));
        assert_eq!(items[1].title, "2D Whale World");
        assert_eq!(items[1].stream_url, "web://https://www.puzzlescript.net/play.html?p=6841165");
    }

    #[test]
    fn pico8_bbs_lister_parses_pdat_rows_and_tags() {
        let sample = r#"
<script>
pdat=[
  ['190262', 157055, `Forlorn Shores`,"/bbs/thumbs/pico8_forlornshores_jusiv_v01-9.png",256,170.6,"2026-05-25",24585,"Jusiv","2026-11-23",0,"",45,44,0,7,2,'0',["puzzle","adventure",],2,272,7,`Forlorn Shores v1.9`,``],
  ['165759', 148484, `Mole, "Mole"`,"/bbs/thumbs/pico8_molemole-1.png",256,170.6,"2025-04-18",13953,"Adam Atomic","2026-11-01",0,"",109,25,0,7,2,'0',[],2,276,7,`Mole Mole`,``],
];
</script>
"#;
        let items = parse_pico8_bbs_lister(sample).unwrap();
        assert_eq!(items.len(), 2);
        assert_eq!(items[0].id, "pico8:190262");
        assert_eq!(items[0].title, "Forlorn Shores");
        assert_eq!(items[0].stream_url, "web://https://www.lexaloffle.com/bbs/?pid=190262#p");
        assert_eq!(items[0].artwork_id, "https://www.lexaloffle.com/bbs/thumbs/pico8_forlornshores_jusiv_v01-9.png");
        assert!(items[0].overview.contains("by Jusiv [puzzle, adventure]"));
        assert_eq!(items[1].title, "Mole, \"Mole\"");
        assert!(is_pico8_gallery_url("https://www.lexaloffle.com/bbs/?cat=7"));
        assert!(!is_pico8_gallery_url("https://www.lexaloffle.com/bbs/?pid=190262#p"));
    }

    #[test]
    fn websocket_client_frame_encodes_masked_payload() {
        let frame = encode_client_frame(0x1, b"hi");
        assert_eq!(frame[0], 0x81);
        assert_eq!(frame[1], 0x82);
        let mask = &frame[2..6];
        let unmasked: Vec<u8> = frame[6..].iter().enumerate().map(|(i, b)| b ^ mask[i & 3]).collect();
        assert_eq!(unmasked, b"hi");
    }
}
