//! Linking the person's own mpv to Phosphor Lab.
//!
//! * `install_mpv_script` writes a small Lua script into mpv's scripts folder.
//!   Pressing Ctrl+T in mpv then pauses mpv and sends the current file/URL and
//!   position to Phosphor Lab (`crt_sim --mpv <path> --start <seconds>`).
//! * When Phosphor Lab is already open, that second launch hands the request to
//!   the running window over a local socket and exits, so you don't get two TVs.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

pub type Inbox = Arc<Mutex<Vec<(String, f64)>>>;

#[cfg(unix)]
fn socket_path() -> PathBuf {
    let base = std::env::var_os("XDG_RUNTIME_DIR").map(PathBuf::from).unwrap_or_else(std::env::temp_dir);
    let user = std::env::var("USER").unwrap_or_else(|_| "user".into());
    base.join(format!("phosphor-lab-{user}.sock"))
}

/// Hands `src` to an already-running Phosphor Lab. Returns true if one took it.
#[cfg(unix)]
pub fn forward_to_running(src: &str, start: f64) -> bool {
    forward_at(&socket_path(), src, start)
}

#[cfg(unix)]
fn forward_at(path: &Path, src: &str, start: f64) -> bool {
    use std::io::{Read, Write};
    use std::os::unix::net::UnixStream;
    let Ok(mut s) = UnixStream::connect(path) else { return false };
    let _ = s.set_read_timeout(Some(std::time::Duration::from_secs(2)));
    if s.write_all(format!("OPEN\t{start}\t{src}\n").as_bytes()).is_err() {
        return false;
    }
    let mut ok = [0u8; 2];
    s.read_exact(&mut ok).is_ok() && &ok == b"OK"
}
#[cfg(not(unix))]
pub fn forward_to_running(_src: &str, _start: f64) -> bool {
    false
}

/// Listens for requests from later launches (e.g. from the mpv shortcut).
#[cfg(unix)]
pub fn listen(ctx: eframe::egui::Context) -> Inbox {
    listen_at(socket_path(), ctx)
}

#[cfg(unix)]
fn listen_at(path: PathBuf, ctx: eframe::egui::Context) -> Inbox {
    use std::io::{BufRead, BufReader, Write};
    use std::os::unix::net::{UnixListener, UnixStream};
    let inbox: Inbox = Arc::new(Mutex::new(Vec::new()));
    // a socket file left by a crashed instance: nobody answers, so replace it
    if path.exists() && UnixStream::connect(&path).is_err() {
        let _ = std::fs::remove_file(&path);
    }
    let Ok(listener) = UnixListener::bind(&path) else { return inbox };
    let ib = inbox.clone();
    std::thread::spawn(move || {
        for stream in listener.incoming().flatten() {
            let mut line = String::new();
            let mut reader = BufReader::new(&stream);
            if reader.read_line(&mut line).is_err() {
                continue;
            }
            let mut parts = line.trim_end_matches('\n').splitn(3, '\t');
            if parts.next() == Some("OPEN") {
                let start = parts.next().and_then(|s| s.parse().ok()).unwrap_or(0.0);
                if let Some(src) = parts.next().filter(|s| !s.is_empty()) {
                    ib.lock().unwrap().push((src.to_string(), start));
                    let mut w = &stream;
                    let _ = w.write_all(b"OK");
                    ctx.request_repaint();
                }
            }
        }
    });
    inbox
}
#[cfg(not(unix))]
pub fn listen(_ctx: eframe::egui::Context) -> Inbox {
    Arc::new(Mutex::new(Vec::new()))
}

#[cfg(unix)]
pub fn cleanup() {
    let _ = std::fs::remove_file(socket_path());
}
#[cfg(not(unix))]
pub fn cleanup() {}

/// mpv's config folders that exist (or the standard one, to be created).
pub fn mpv_config_dirs() -> Vec<PathBuf> {
    let mut v = Vec::new();
    if let Some(h) = std::env::var_os("MPV_HOME") {
        v.push(PathBuf::from(h));
    }
    #[cfg(windows)]
    if let Some(a) = std::env::var_os("APPDATA") {
        v.push(PathBuf::from(a).join("mpv"));
    }
    #[cfg(not(windows))]
    {
        let home = std::env::var_os("HOME").map(PathBuf::from).unwrap_or_default();
        let xdg = std::env::var_os("XDG_CONFIG_HOME").map(PathBuf::from).unwrap_or_else(|| home.join(".config"));
        v.push(xdg.join("mpv"));
        // mpv installed from Flathub keeps its config in its own folder
        let flat = home.join(".var/app/io.mpv.Mpv/config/mpv");
        if flat.parent().map(|p| p.exists()).unwrap_or(false) {
            v.push(flat);
        }
    }
    v.dedup();
    v
}

pub fn script_text(exe: &Path) -> String {
    let exe = exe.to_string_lossy().replace("]]", "] ]");
    format!(
        r#"-- Phosphor Lab: watch what mpv is playing on the simulated CRT.
-- Press Ctrl+T in mpv (rebind with: <key> script-binding send-to-phosphor-lab).
local utils = require 'mp.utils'
local APP = [[{exe}]]

local function send()
    local path = mp.get_property("path")
    if not path then
        mp.osd_message("Phosphor Lab: nothing is playing")
        return
    end
    if not path:find("://") and path:sub(1, 1) ~= "/" and not path:match("^%a:[\\/]") then
        path = utils.join_path(mp.get_property("working-directory"), path)
    end
    local pos = mp.get_property_number("time-pos") or 0
    mp.set_property_bool("pause", true)
    mp.commandv("run", APP, "--mpv", path, "--start", string.format("%.2f", pos))
    mp.osd_message("Sent to Phosphor Lab")
end

mp.add_key_binding("ctrl+t", "send-to-phosphor-lab", send)
"#
    )
}

/// Writes the script into every mpv config folder found. Returns the files written.
pub fn install_mpv_script() -> Result<Vec<PathBuf>, String> {
    let exe = std::env::current_exe().map_err(|e| format!("can't find this program's location: {e}"))?;
    let text = script_text(&exe);
    let mut written = Vec::new();
    let mut last_err = String::new();
    for dir in mpv_config_dirs() {
        let scripts = dir.join("scripts");
        match std::fs::create_dir_all(&scripts).and_then(|_| std::fs::write(scripts.join("phosphor-lab.lua"), &text)) {
            Ok(()) => written.push(scripts.join("phosphor-lab.lua")),
            Err(e) => last_err = format!("{}: {e}", scripts.display()),
        }
    }
    if written.is_empty() { Err(last_err) } else { Ok(written) }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    #[test]
    fn second_launch_hands_video_to_running_window() {
        // private socket in the temp folder, so a real running copy is never touched
        let sock = std::env::temp_dir().join(format!("phosphor-link-test-{}.sock", std::process::id()));
        let inbox = listen_at(sock.clone(), eframe::egui::Context::default());
        std::thread::sleep(std::time::Duration::from_millis(100));
        assert!(forward_at(&sock, "/videos/my clip.mkv", 83.5));
        assert!(forward_at(&sock, "https://example.com/watch?v=1", 0.0));
        std::thread::sleep(std::time::Duration::from_millis(100));
        let got = inbox.lock().unwrap().clone();
        assert_eq!(got, vec![("/videos/my clip.mkv".to_string(), 83.5), ("https://example.com/watch?v=1".to_string(), 0.0)]);
        let _ = std::fs::remove_file(&sock);
        assert!(!forward_at(&sock, "x", 0.0), "nobody should answer once the socket is gone");
    }

    #[test]
    fn script_mentions_app_and_binding() {
        let t = script_text(Path::new("/home/me/Phosphor Lab/crt_sim"));
        assert!(t.contains("local APP = [[/home/me/Phosphor Lab/crt_sim]]"));
        assert!(t.contains("mp.add_key_binding(\"ctrl+t\""));
        assert!(t.contains("\"--start\""));
    }
}
