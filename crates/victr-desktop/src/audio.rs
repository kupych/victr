//! Desktop sound sink for ViCTR: feeds `victr_core::audio_dsp::Processor` output
//! to the first local audio player found (`pacat` / `pw-cat` / `aplay` / `mpv` / `ffplay`)
//! as raw 48 kHz stereo 16-bit PCM over a pipe.

pub use victr_core::audio_dsp::*;

use std::io::Write;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

// ------------------------------------------------------------------ output

pub struct AudioEngine {
    pub params: Arc<Mutex<AudioParams>>,
    pub ring: Arc<AudioRing>,
    pub status: Arc<Mutex<String>>,
    stop: Arc<AtomicBool>,
}

impl AudioEngine {
    pub fn start() -> AudioEngine {
        let params = Arc::new(Mutex::new(AudioParams::default()));
        let ring = AudioRing::new();
        let status = Arc::new(Mutex::new("Looking for a sound output…".to_string()));
        let stop = Arc::new(AtomicBool::new(false));
        let (p2, r2, s2, st2) = (params.clone(), ring.clone(), status.clone(), stop.clone());
        std::thread::spawn(move || run_output(p2, r2, s2, st2));
        AudioEngine { params, ring, status, stop }
    }
}

impl Drop for AudioEngine {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
    }
}

fn sinks() -> Vec<(&'static str, Vec<&'static str>)> {
    vec![
        (
            "pacat",
            vec![
                "--playback",
                "--raw",
                "--format=s16le",
                "--rate=48000",
                "--channels=2",
                "--latency-msec=35",
                "--client-name=ViCTR",
            ],
        ),
        ("pw-cat", vec!["--playback", "--format", "s16", "--rate", "48000", "--channels", "2", "--latency", "35ms", "-"]),
        ("aplay", vec!["-q", "-f", "S16_LE", "-r", "48000", "-c", "2", "-t", "raw", "--buffer-time=40000"]),
        (
            "mpv",
            vec![
                "--no-config",
                "--really-quiet",
                "--no-terminal",
                "--no-video",
                "--demuxer=rawaudio",
                "--demuxer-rawaudio-format=s16le",
                "--demuxer-rawaudio-rate=48000",
                "--demuxer-rawaudio-channels=2",
                "--cache=no",
                "--audio-buffer=0.025",
                "--stream-buffer-size=4k",
                "--demuxer-readahead-secs=0",
                "-",
            ],
        ),
        (
            "ffplay",
            vec![
                "-nodisp",
                "-autoexit",
                "-loglevel",
                "quiet",
                "-f",
                "s16le",
                "-ar",
                "48000",
                "-ch_layout",
                "stereo",
                "-i",
                "pipe:0",
            ],
        ),
    ]
}

#[cfg(target_os = "linux")]
fn shrink_pipe(child: &Child) {
    use std::os::unix::io::AsRawFd;
    unsafe extern "C" {
        fn fcntl(fd: i32, cmd: i32, ...) -> i32;
    }
    if let Some(stdin) = child.stdin.as_ref() {
        // F_SETPIPE_SZ: 4096 B (minimum page size = ~21 ms at 48 kHz stereo s16) keeps lip-sync tight
        unsafe { fcntl(stdin.as_raw_fd(), 1031, 4096i32) };
    }
}
#[cfg(not(target_os = "linux"))]
fn shrink_pipe(_child: &Child) {}

fn run_output(params: Arc<Mutex<AudioParams>>, ring: Arc<AudioRing>, status: Arc<Mutex<String>>, stop: Arc<AtomicBool>) {
    let mut proc = Processor::new();
    let mut buf = vec![0i16; BLOCK * 2];
    let mut bytes = vec![0u8; BLOCK * 4];
    for (cmd, args) in sinks() {
        if stop.load(Ordering::SeqCst) {
            return;
        }
        let child = Command::new(cmd).args(&args).stdin(Stdio::piped()).stdout(Stdio::null()).stderr(Stdio::null()).spawn();
        let mut child: Child = match child {
            Ok(c) => c,
            Err(_) => continue,
        };
        shrink_pipe(&child);
        let mut stdin = child.stdin.take().unwrap();
        *status.lock().unwrap() = format!("Sound out through {cmd}");
        let mut written = 0usize;
        let ok = loop {
            if stop.load(Ordering::SeqCst) {
                let _ = child.kill();
                return;
            }
            let p = params.lock().unwrap().clone();
            proc.process(&p, &ring, &mut buf);
            for (k, s) in buf.iter().enumerate() {
                let b = s.to_le_bytes();
                bytes[2 * k] = b[0];
                bytes[2 * k + 1] = b[1];
            }
            if stdin.write_all(&bytes).is_err() {
                break written > (RATE as usize) * 4; // ran for a while: it was a real sink
            }
            written += bytes.len();
        };
        let _ = child.kill();
        let _ = child.wait();
        if ok {
            *status.lock().unwrap() = format!("Sound output ({cmd}) stopped.");
            return;
        }
    }
    *status.lock().unwrap() = "No sound output found (needs pacat, pw-cat, aplay or mpv).".to_string();
}


#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;
    #[test]
    fn engine_writes_to_sink() {
        let fake = "/tmp/crtfake";
        if !std::path::Path::new(&format!("{fake}/pacat")).exists() {
            return;
        }
        let _ = std::fs::remove_file(format!("{fake}/sound.raw"));
        let path = std::env::var("PATH").unwrap_or_default();
        unsafe { std::env::set_var("PATH", format!("{fake}:{path}")) };
        let eng = AudioEngine::start();
        {
            let mut p = eng.params.lock().unwrap();
            p.rf = true;
            p.snr_db = -25.0;
        }
        std::thread::sleep(Duration::from_millis(1500));
        let st = eng.status.lock().unwrap().clone();
        drop(eng);
        std::thread::sleep(Duration::from_millis(200));
        let len = std::fs::metadata(format!("{fake}/sound.raw")).map(|m| m.len()).unwrap_or(0);
        println!("status: {st}, bytes: {len}");
        assert!(st.contains("pacat"));
        assert!(len > 10000);
    }
}
