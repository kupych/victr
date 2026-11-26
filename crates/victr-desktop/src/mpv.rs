//! Routes video from mpv into the simulated TV.
//!
//! Preferred path: libmpv loaded at run time (no build-time dependency) using its
//! software render API, so mpv keeps doing the decoding, audio, A/V sync, URLs
//! (via yt-dlp), etc., and hands us each frame as pixels.
//! Fallback path: the `mpv` command in encode mode writing raw frames to a pipe,
//! plus a second `mpv --no-video` for the sound.

use std::ffi::{CStr, CString, c_char, c_int, c_void};
use std::io::Read;
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crate::audio::AudioRing;

pub const FW: usize = 640;
pub const FH: usize = 480;

#[derive(Default)]
pub struct Shared {
    pub frame: Option<Arc<(usize, usize, Vec<u8>)>>,
    pub frame_no: u64,
    pub status: String,
    pub pos: f64,
    pub duration: f64,
    pub paused: bool,
    pub seeking: bool,
    pub ended: bool,
    pub running: bool,
    pub backend: &'static str,
}

enum Cmd {
    TogglePause,
    #[allow(dead_code)]
    SetPaused(bool),
    Seek(f64),
    SeekFast(f64),
    SeekAbs(f64),
    Speed(f64),
    SetShuttle(f64),
    Volume(f64),
    AudioDelay(f64),
    Crop(bool),
    Key(String),
    Stop,
}

pub struct Player {
    tx: Sender<Cmd>,
    pub shared: Arc<Mutex<Shared>>,
    stop: Arc<AtomicBool>,
}

struct Audio {
    ring: Option<Arc<AudioRing>>,
    session: u64,
    stop: Arc<AtomicBool>,
    paused: Arc<AtomicBool>,
    pump_active: Arc<AtomicBool>,
    drain_req: Arc<std::sync::atomic::AtomicU64>,
    drain_ack: Arc<std::sync::atomic::AtomicU64>,
    bytes_in: Arc<std::sync::atomic::AtomicU64>,
}

impl Audio {
    fn new(ring: Option<Arc<AudioRing>>, session: u64, stop: Arc<AtomicBool>) -> Self {
        Audio {
            ring,
            session,
            stop,
            paused: Arc::new(AtomicBool::new(false)),
            pump_active: Arc::new(AtomicBool::new(false)),
            drain_req: Arc::new(std::sync::atomic::AtomicU64::new(0)),
            drain_ack: Arc::new(std::sync::atomic::AtomicU64::new(0)),
            bytes_in: Arc::new(std::sync::atomic::AtomicU64::new(0)),
        }
    }
    /// Copies raw s16le stereo PCM from a generic reader (CLI fallback) into the ring.
    fn pump(&self, mut r: impl Read + Send + 'static) {
        if let Some(ring) = self.ring.clone() {
            let stop = self.stop.clone();
            let bytes_in = self.bytes_in.clone();
            let session = self.session;
            std::thread::spawn(move || {
                // 1024 B = 256 stereo frames (~5.3 ms) so samples reach the DSP ring promptly
                let mut buf = vec![0u8; 1024];
                loop {
                    match r.read(&mut buf) {
                        Ok(0) | Err(_) => break,
                        Ok(n) => {
                            if stop.load(Ordering::Relaxed) {
                                break;
                            }
                            bytes_in.fetch_add(n as u64, Ordering::Relaxed);
                            let n = n & !3;
                            ring.push_s16_session(session, &buf[..n], &stop);
                            if stop.load(Ordering::Relaxed) {
                                break;
                            }
                        }
                    }
                }
            });
        }
    }
    /// Copies raw s16le stereo PCM from libmpv's `ao=pcm` pipe into the ring, supporting
    /// sample-accurate pause gating (leaving unread burst bytes in the kernel pipe) and
    /// synchronous non-blocking pipe draining before seeks/shuttle so stale audio never leaks.
    #[cfg(unix)]
    fn pump_pipe_fd(&self, fd: c_int) {
        let Some(ring) = self.ring.clone() else {
            close_pipe(Some((fd, -1)));
            return;
        };
        let stop = self.stop.clone();
        let paused = self.paused.clone();
        let pump_active = self.pump_active.clone();
        let drain_req = self.drain_req.clone();
        let drain_ack = self.drain_ack.clone();
        let bytes_in = self.bytes_in.clone();
        let session = self.session;
        pump_active.store(true, Ordering::SeqCst);
        std::thread::spawn(move || {
            let mut r = fd_reader(fd);
            let mut buf = vec![0u8; 1024];
            let mut scratch = [0u8; 4096];
            loop {
                if stop.load(Ordering::Relaxed) {
                    break;
                }
                let req = drain_req.load(Ordering::SeqCst);
                if req != drain_ack.load(Ordering::Relaxed) {
                    let mut avail = pipe_readable_bytes(fd);
                    while avail > 0 {
                        let want = avail.min(scratch.len());
                        match r.read(&mut scratch[..want]) {
                            Ok(0) | Err(_) => break,
                            Ok(n) => avail = avail.saturating_sub(n),
                        }
                    }
                    drain_ack.store(req, Ordering::SeqCst);
                    continue;
                }
                if paused.load(Ordering::Relaxed) {
                    std::thread::sleep(Duration::from_millis(5));
                    continue;
                }
                let mut pfd = PollFd { fd, events: POLLIN, revents: 0 };
                let pr = unsafe { poll(&mut pfd, 1, 5) };
                if pr <= 0 {
                    continue;
                }
                if (pfd.revents & POLLIN) == 0 && (pfd.revents & (POLLHUP | POLLERR)) != 0 {
                    break;
                }
                match r.read(&mut buf) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => {
                        if stop.load(Ordering::Relaxed) {
                            break;
                        }
                        bytes_in.fetch_add(n as u64, Ordering::Relaxed);
                        let n = n & !3;
                        ring.push_s16_session(session, &buf[..n], &stop);
                        if stop.load(Ordering::Relaxed) {
                            break;
                        }
                    }
                }
            }
            pump_active.store(false, Ordering::SeqCst);
        });
    }
    #[cfg(not(unix))]
    fn pump_pipe_fd(&self, fd: c_int) {
        self.pump(fd_reader(fd));
    }
    fn set_paused(&self, on: bool) {
        self.paused.store(on, Ordering::SeqCst);
        if on {
            self.clear();
        }
    }
    /// Clears the ring and synchronously discards any stale bytes currently queued
    /// inside the kernel PCM pipe (must be called BEFORE issuing a seek/restart).
    fn drain_pipe(&self) {
        self.clear();
        if !self.pump_active.load(Ordering::Relaxed) {
            return;
        }
        let seq = self.drain_req.fetch_add(1, Ordering::SeqCst) + 1;
        for _ in 0..20 {
            if self.drain_ack.load(Ordering::SeqCst) >= seq || !self.pump_active.load(Ordering::Relaxed) {
                break;
            }
            self.clear();
            std::thread::sleep(Duration::from_millis(1));
        }
        self.clear();
    }
    fn clear(&self) {
        if let Some(r) = &self.ring {
            r.clear_session(self.session);
        }
    }
}

impl Player {
    /// Starts playing `source` (a file path or a URL mpv understands).
    /// With `audio` set, mpv's sound is decoded to raw samples and routed through
    /// the simulated TV sound path instead of being played directly.
    pub fn start(source: &str, opts: StartOpts, audio: Option<Arc<AudioRing>>) -> Player {
        let StartOpts { volume, crop, .. } = opts;
        let session = audio.as_ref().map(|r| r.begin_session()).unwrap_or(0);
        let (tx, rx) = channel();
        let shared = Arc::new(Mutex::new(Shared {
            status: "Starting mpv…".into(),
            running: true,
            ..Default::default()
        }));
        let sh = shared.clone();
        let src = source.to_string();
        let stop = Arc::new(AtomicBool::new(false));
        let stop2 = stop.clone();
        std::thread::spawn(move || {
            let audio = Audio::new(audio, session, stop2);
            let lib_err = match Lib::load() {
                Ok(lib) => match run_libmpv(&lib, &src, volume, crop, &opts, &rx, &sh, &audio) {
                    Ok(()) => {
                        finish(&sh);
                        return;
                    }
                    Err(e) => e,
                },
                Err(e) => e,
            };
            if let Err(e) = run_cli(&src, volume, crop, opts.start, opts.user_config, &rx, &sh, &lib_err, &audio) {
                let mut s = sh.lock().unwrap();
                s.status = e;
                s.running = false;
                return;
            }
            finish(&sh);
        });
        Player { tx, shared, stop }
    }
    pub fn toggle_pause(&self) {
        let _ = self.tx.send(Cmd::TogglePause);
    }
    #[allow(dead_code)]
    pub fn set_paused(&self, paused: bool) {
        let _ = self.tx.send(Cmd::SetPaused(paused));
    }
    pub fn seek(&self, secs: f64) {
        let _ = self.tx.send(Cmd::Seek(secs));
    }
    /// Fast keyframe-aligned relative seek used during stepped VHS FF/RW picture search.
    pub fn seek_fast(&self, secs: f64) {
        let _ = self.tx.send(Cmd::SeekFast(secs));
    }
    pub fn seek_abs(&self, secs: f64) {
        let _ = self.tx.send(Cmd::SeekAbs(secs));
    }
    #[allow(dead_code)]
    pub fn set_speed(&self, speed: f64) {
        let _ = self.tx.send(Cmd::Speed(speed));
    }
    /// Enters or exits VHS FF/RW shuttle cleanly (`0.0` = Play, `4.0`/`8.0` = smooth FF,
    /// `< 0` or `> 8.5` = paused keyframe-stepped shuttle) without letting `ao=pcm` drift A/V sync.
    pub fn set_shuttle(&self, speed: f64) {
        let _ = self.tx.send(Cmd::SetShuttle(speed));
    }
    pub fn volume(&self, v: f64) {
        let _ = self.tx.send(Cmd::Volume(v));
    }
    /// Adjusts A/V sync offset (in seconds, added on top of automatic DSP pipeline compensation).
    pub fn audio_delay(&self, offset_secs: f64) {
        let _ = self.tx.send(Cmd::AudioDelay(offset_secs));
    }
    pub fn crop(&self, c: bool) {
        let _ = self.tx.send(Cmd::Crop(c));
    }
    /// Sends a key to mpv (mpv key name, e.g. "SPACE", "LEFT", "Ctrl+s", "m"),
    /// so the person's own mpv key bindings work while watching the TV.
    pub fn key(&self, name: &str) {
        let _ = self.tx.send(Cmd::Key(name.to_string()));
    }
}

/// Baseline DSP pipeline latency compensation (in seconds) when routing audio through `AudioRing` + sink:
/// ~55 ms steady-state pcm_pipe + ~45 ms AudioRing + ~5 ms block + ~21 ms out pipe + ~35 ms sink
/// minus ~21 ms video presentation latency ≈ 140 ms net audio lag -> advance audio by -0.14 s.
const DSP_AUDIO_DELAY_BASE_SECS: f64 = -0.14;

#[derive(Clone, Copy)]
pub struct StartOpts {
    pub volume: f64,
    pub crop: bool,
    /// Seconds into the video to start at.
    pub start: f64,
    /// Load the person's own mpv.conf, input.conf and scripts.
    pub user_config: bool,
    /// Extra user A/V sync nudge in seconds (negative = earlier audio, positive = later audio).
    pub audio_delay: f64,
}

impl Drop for Player {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        let _ = self.tx.send(Cmd::Stop);
    }
}

fn finish(sh: &Arc<Mutex<Shared>>) {
    let mut s = sh.lock().unwrap();
    s.running = false;
    if s.status.is_empty() || s.status.starts_with("Playing") {
        s.status = "Stopped.".into();
    }
}

fn publish(sh: &Arc<Mutex<Shared>>, data: Vec<u8>) {
    let mut s = sh.lock().unwrap();
    s.frame = Some(Arc::new((FW, FH, data)));
    s.frame_no += 1;
}

// ------------------------------------------------------------------ dynamic loading

#[cfg(unix)]
mod dl {
    use super::*;
    unsafe extern "C" {
        fn dlopen(filename: *const c_char, flag: c_int) -> *mut c_void;
        fn dlsym(handle: *mut c_void, symbol: *const c_char) -> *mut c_void;
    }
    pub fn open(names: &[&str]) -> Option<*mut c_void> {
        for n in names {
            let c = CString::new(*n).ok()?;
            let h = unsafe { dlopen(c.as_ptr(), 2 /* RTLD_NOW */) };
            if !h.is_null() {
                return Some(h);
            }
        }
        None
    }
    pub fn sym(h: *mut c_void, name: &str) -> *mut c_void {
        let c = CString::new(name).unwrap();
        unsafe { dlsym(h, c.as_ptr()) }
    }
    pub const NAMES: &[&str] = &[
        "libmpv.so.2",
        "libmpv.so.1",
        "libmpv.so",
        "libmpv.2.dylib",
        "libmpv.dylib",
        "/opt/homebrew/lib/libmpv.dylib",
        "/usr/local/lib/libmpv.dylib",
    ];
}

#[cfg(windows)]
mod dl {
    use super::*;
    unsafe extern "system" {
        fn LoadLibraryA(name: *const c_char) -> *mut c_void;
        fn GetProcAddress(h: *mut c_void, name: *const c_char) -> *mut c_void;
    }
    pub fn open(names: &[&str]) -> Option<*mut c_void> {
        for n in names {
            let c = CString::new(*n).ok()?;
            let h = unsafe { LoadLibraryA(c.as_ptr()) };
            if !h.is_null() {
                return Some(h);
            }
        }
        None
    }
    pub fn sym(h: *mut c_void, name: &str) -> *mut c_void {
        let c = CString::new(name).unwrap();
        unsafe { GetProcAddress(h, c.as_ptr()) }
    }
    pub const NAMES: &[&str] = &["libmpv-2.dll", "mpv-2.dll", "mpv-1.dll"];
}

#[repr(C)]
struct RenderParam {
    kind: c_int,
    data: *mut c_void,
}

#[repr(C)]
struct MpvEvent {
    event_id: c_int,
    error: c_int,
    reply_userdata: u64,
    data: *mut c_void,
}

type Handle = *mut c_void;
type RenderCtx = *mut c_void;

#[allow(clippy::type_complexity)]
struct Lib {
    create: unsafe extern "C" fn() -> Handle,
    initialize: unsafe extern "C" fn(Handle) -> c_int,
    set_option_string: unsafe extern "C" fn(Handle, *const c_char, *const c_char) -> c_int,
    set_property_string: unsafe extern "C" fn(Handle, *const c_char, *const c_char) -> c_int,
    get_property: unsafe extern "C" fn(Handle, *const c_char, c_int, *mut c_void) -> c_int,
    command: unsafe extern "C" fn(Handle, *const *const c_char) -> c_int,
    wait_event: unsafe extern "C" fn(Handle, f64) -> *mut MpvEvent,
    terminate_destroy: unsafe extern "C" fn(Handle),
    error_string: unsafe extern "C" fn(c_int) -> *const c_char,
    rc_create: unsafe extern "C" fn(*mut RenderCtx, Handle, *mut RenderParam) -> c_int,
    rc_update: unsafe extern "C" fn(RenderCtx) -> u64,
    rc_render: unsafe extern "C" fn(RenderCtx, *mut RenderParam) -> c_int,
    rc_free: unsafe extern "C" fn(RenderCtx),
}

unsafe impl Send for Lib {}

impl Lib {
    #[cfg(any(unix, windows))]
    fn load() -> Result<Lib, String> {
        let h = dl::open(dl::NAMES).ok_or_else(|| "libmpv not found".to_string())?;
        macro_rules! f {
            ($n:expr) => {{
                let p = dl::sym(h, $n);
                if p.is_null() {
                    return Err(format!("libmpv is missing {}", $n));
                }
                unsafe { std::mem::transmute::<*mut c_void, _>(p) }
            }};
        }
        Ok(Lib {
            create: f!("mpv_create"),
            initialize: f!("mpv_initialize"),
            set_option_string: f!("mpv_set_option_string"),
            set_property_string: f!("mpv_set_property_string"),
            get_property: f!("mpv_get_property"),
            command: f!("mpv_command"),
            wait_event: f!("mpv_wait_event"),
            terminate_destroy: f!("mpv_terminate_destroy"),
            error_string: f!("mpv_error_string"),
            rc_create: f!("mpv_render_context_create"),
            rc_update: f!("mpv_render_context_update"),
            rc_render: f!("mpv_render_context_render"),
            rc_free: f!("mpv_render_context_free"),
        })
    }
    #[cfg(not(any(unix, windows)))]
    fn load() -> Result<Lib, String> {
        Err("libmpv is not supported on this platform".into())
    }
}

/// 64-byte aligned pixel buffer for mpv's software renderer.
struct AlignedBuf {
    ptr: *mut u8,
    layout: std::alloc::Layout,
}
impl AlignedBuf {
    fn new(len: usize) -> Self {
        let layout = std::alloc::Layout::from_size_align(len, 64).unwrap();
        let ptr = unsafe { std::alloc::alloc_zeroed(layout) };
        assert!(!ptr.is_null());
        AlignedBuf { ptr, layout }
    }
    fn as_slice(&self) -> &[u8] {
        unsafe { std::slice::from_raw_parts(self.ptr, self.layout.size()) }
    }
}
impl Drop for AlignedBuf {
    fn drop(&mut self) {
        unsafe { std::alloc::dealloc(self.ptr, self.layout) }
    }
}

const RP_API_TYPE: c_int = 1;
const RP_SW_SIZE: c_int = 17;
const RP_SW_FORMAT: c_int = 18;
const RP_SW_STRIDE: c_int = 19;
const RP_SW_POINTER: c_int = 20;
const EV_SHUTDOWN: c_int = 1;
const EV_END_FILE: c_int = 7;
const EV_FILE_LOADED: c_int = 8;
const EV_SEEK: c_int = 20;
const EV_PLAYBACK_RESTART: c_int = 21;
const FMT_DOUBLE: c_int = 5;
const FMT_FLAG: c_int = 3;

fn run_libmpv(
    lib: &Lib,
    src: &str,
    volume: f64,
    crop: bool,
    o: &StartOpts,
    rx: &Receiver<Cmd>,
    sh: &Arc<Mutex<Shared>>,
    audio: &Audio,
) -> Result<(), String> {
    unsafe {
        let h = (lib.create)();
        if h.is_null() {
            return Err("mpv_create failed".into());
        }
        let opt = |k: &str, v: &str| {
            let k = CString::new(k).unwrap();
            let v = CString::new(v).unwrap();
            (lib.set_option_string)(h, k.as_ptr(), v.as_ptr())
        };
        // The person's own mpv: their mpv.conf, input.conf (key bindings) and scripts.
        opt("config", if o.user_config { "yes" } else { "no" });
        opt("input-default-bindings", "yes");
        opt("ytdl", "yes");
        opt("hwdec", "auto-copy-safe");
        // Keep generous forward & back cache in RAM + auto-reconnect on HTTP/HTTPS streams
        // so pausing, rewinding, and fast-forwarding over Jellyfin/Plex never stall on dropped sockets.
        opt("cache", "yes");
        opt("demuxer-max-bytes", "64MiB");
        opt("demuxer-max-back-bytes", "32MiB");
        if src.starts_with("http://") || src.starts_with("https://") {
            // Note: never set `reconnect_at_eof=1` globally — it causes ffmpeg's https protocol
            // to infinitely reconnect at normal EOF on finite HLS `.m3u8` / YouTube Live manifests.
            opt(
                "stream-lavf-o",
                "reconnect=1,reconnect_streamed=1,reconnect_on_network_error=1,reconnect_delay_max=4",
            );
            opt("demuxer-lavf-o", "rw_timeout=8000000");
        }
        if o.start > 0.0 {
            opt("start", &format!("{:.3}", o.start));
        }
        // Route the decoded sound into our TV sound path through a pipe.
        let pcm_pipe = if audio.ring.is_some() { make_pipe() } else { None };
        // Settings the TV needs no matter what the person's config says. They are
        // applied before start-up and again afterwards, because mpv.conf is read
        // during start-up and could override them (e.g. vo=gpu or a custom ao).
        let verbose_log = std::env::var_os("VICTR_MPV_LOG").is_some();
        let essentials = |set: &dyn Fn(&str, &str) -> c_int| {
            set("vo", "libmpv");
            set("terminal", if verbose_log { "yes" } else { "no" });
            if verbose_log {
                set("msg-level", "ao=v,ad=v,cplayer=v");
            }
            set("osc", "no");
            set("keep-open", "yes");
            set("panscan", if crop { "1.0" } else { "0.0" });
            if let Some((_, w)) = pcm_pipe {
                set("ao", "pcm");
                set("ao-pcm-file", &format!("/dev/fd/{w}"));
                set("ao-pcm-waveheader", "no");
                set("audio-format", "s16");
                set("audio-samplerate", "48000");
                set("audio-channels", "stereo");
                set("audio-buffer", "0.15");
                // Compensate for the shrunk DSP pipe + ring + sink latency (+ any user nudge)
                let delay = DSP_AUDIO_DELAY_BASE_SECS + o.audio_delay;
                set("audio-delay", &format!("{delay:.4}"));
                set("volume", "100");
            } else {
                set("audio-delay", &format!("{:.4}", o.audio_delay));
                set("volume", &format!("{volume:.0}"));
            }
        };
        essentials(&opt);
        let r = (lib.initialize)(h);
        if r < 0 {
            let e = CStr::from_ptr((lib.error_string)(r)).to_string_lossy().to_string();
            (lib.terminate_destroy)(h);
            close_pipe(pcm_pipe);
            return Err(format!("mpv_initialize failed: {e}"));
        }
        if o.user_config {
            let setp = |k: &str, v: &str| {
                let k = CString::new(k).unwrap();
                let v = CString::new(v).unwrap();
                (lib.set_property_string)(h, k.as_ptr(), v.as_ptr())
            };
            essentials(&setp);
        }
        if let Some((r, _)) = pcm_pipe {
            audio.pump_pipe_fd(r);
        }
        let api = CString::new("sw").unwrap();
        let mut params = [
            RenderParam { kind: RP_API_TYPE, data: api.as_ptr() as *mut c_void },
            RenderParam { kind: 0, data: std::ptr::null_mut() },
        ];
        let mut rc: RenderCtx = std::ptr::null_mut();
        let r = (lib.rc_create)(&mut rc, h, params.as_mut_ptr());
        if r < 0 || rc.is_null() {
            let e = CStr::from_ptr((lib.error_string)(r)).to_string_lossy().to_string();
            (lib.terminate_destroy)(h);
            close_pipe(pcm_pipe.map(|(_, w)| (-1, w)));
            return Err(format!("mpv software renderer unavailable: {e}"));
        }

        let cmd = |args: &[&str]| {
            let cs: Vec<CString> = args.iter().map(|a| CString::new(*a).unwrap()).collect();
            let mut ptrs: Vec<*const c_char> = cs.iter().map(|c| c.as_ptr()).collect();
            ptrs.push(std::ptr::null());
            (lib.command)(h, ptrs.as_ptr())
        };
        let prop = |k: &str, v: &str| {
            let k = CString::new(k).unwrap();
            let v = CString::new(v).unwrap();
            (lib.set_property_string)(h, k.as_ptr(), v.as_ptr())
        };
        let get_f64 = |k: &str| -> Option<f64> {
            let k = CString::new(k).unwrap();
            let mut v = 0f64;
            if (lib.get_property)(h, k.as_ptr(), FMT_DOUBLE, &mut v as *mut f64 as *mut c_void) >= 0 { Some(v) } else { None }
        };
        let get_flag = |k: &str| -> Option<bool> {
            let k = CString::new(k).unwrap();
            let mut v: c_int = 0;
            if (lib.get_property)(h, k.as_ptr(), FMT_FLAG, &mut v as *mut c_int as *mut c_void) >= 0 { Some(v != 0) } else { None }
        };

        cmd(&["loadfile", src]);
        {
            let mut s = sh.lock().unwrap();
            s.backend = if pcm_pipe.is_some() { "libmpv, sound through the TV" } else { "libmpv" };
            s.status = "Loading…".into();
        }

        let buf = AlignedBuf::new(FW * FH * 4);
        let fmt = CString::new("rgb0").unwrap();
        let mut size: [c_int; 2] = [FW as c_int, FH as c_int];
        let mut stride: usize = FW * 4;
        let mut last_poll = Instant::now();
        let mut last_log = Instant::now();
        let mut last_log_bytes = 0u64;
        let mut last_poll_bytes = 0u64;
        let mut last_poll_pos = -1.0f64;
        let mut silent_play_since: Option<Instant> = None;
        let mut healthy_audio_since: Option<Instant> = None;
        let mut stall_recoveries: u32 = 0;
        let mut last_resync = Instant::now() - Duration::from_secs(10);
        let mut shuttle_active = false;
        let mut was_paused = false;
        'outer: loop {
            // Drain pending commands and coalesce consecutive relative seeks so remote HTTP streams
            // never accumulate a backlog of overlapping range requests.
            let mut pending_seek = 0.0f64;
            let mut pending_seek_fast = false;
            let flush_seek = |pending_seek: &mut f64, pending_seek_fast: &mut bool, in_shuttle: bool| {
                if pending_seek.abs() > 0.001 {
                    let mode = if *pending_seek_fast { "relative+keyframes" } else { "relative" };
                    if !in_shuttle {
                        audio.drain_pipe();
                    }
                    cmd(&["seek", &format!("{:.3}", *pending_seek), mode]);
                    audio.clear();
                    sh.lock().unwrap().seeking = true;
                    *pending_seek = 0.0;
                    *pending_seek_fast = false;
                }
            };
            while let Ok(c) = rx.try_recv() {
                match c {
                    Cmd::Seek(s) => {
                        pending_seek += s;
                        continue;
                    }
                    Cmd::SeekFast(s) => {
                        pending_seek += s;
                        pending_seek_fast = true;
                        continue;
                    }
                    other => {
                        flush_seek(&mut pending_seek, &mut pending_seek_fast, shuttle_active);
                        match other {
                            Cmd::Stop => break 'outer,
                            Cmd::TogglePause => {
                                cmd(&["cycle", "pause"]);
                                if let Some(p) = get_flag("pause") {
                                    audio.set_paused(p && !shuttle_active);
                                    was_paused = p;
                                }
                            }
                            Cmd::SetPaused(p) => {
                                prop("pause", if p { "yes" } else { "no" });
                                audio.set_paused(p && !shuttle_active);
                                was_paused = p;
                            }
                            Cmd::SeekAbs(s) => {
                                audio.drain_pipe();
                                cmd(&["seek", &format!("{s:.3}"), "absolute"]);
                                audio.clear();
                                sh.lock().unwrap().seeking = true;
                                silent_play_since = None;
                                stall_recoveries = 0;
                            }
                            Cmd::Speed(sp) => {
                                let clamped = sp.clamp(0.25, 16.0);
                                prop("speed", &format!("{clamped:.2}"));
                                if clamped > 1.05 {
                                    audio.clear();
                                }
                            }
                            Cmd::SetShuttle(spd) => {
                                if spd.abs() > 0.1 {
                                    shuttle_active = true;
                                    // Detach audio decoding (`aid=no`) while shuttling, then flush any
                                    // remaining initial-burst bytes out of the kernel pipe.
                                    prop("aid", "no");
                                    audio.set_paused(false);
                                    audio.drain_pipe();
                                    if spd > 0.1 && spd <= 8.5 {
                                        prop("pause", "no");
                                        prop("speed", &format!("{:.2}", spd.clamp(1.0, 8.0)));
                                    } else {
                                        // Stepped RW & 16x/32x FF: pause decoder so only clean keyframes decode
                                        prop("speed", "1.0");
                                        prop("pause", "yes");
                                    }
                                } else {
                                    let was_sh = shuttle_active;
                                    shuttle_active = false;
                                    prop("speed", "1.0");
                                    prop("pause", "no");
                                    audio.set_paused(false);
                                    audio.drain_pipe();
                                    if was_sh {
                                        let cur_pos = get_f64("time-pos").unwrap_or_else(|| sh.lock().unwrap().pos).max(0.0);
                                        // Re-attach audio BEFORE seeking, and use an exact `absolute` seek
                                        // so playback resumes right where shuttle stopped instead of snapping
                                        // backwards 3-5 seconds to a previous keyframe.
                                        prop("aid", "auto");
                                        cmd(&["seek", &format!("{cur_pos:.3}"), "absolute"]);
                                        sh.lock().unwrap().seeking = true;
                                        silent_play_since = None;
                                        healthy_audio_since = None;
                                        stall_recoveries = 0;
                                        last_resync = Instant::now();
                                    } else {
                                        prop("aid", "auto");
                                    }
                                }
                            }
                            Cmd::Volume(v) => {
                                if pcm_pipe.is_none() {
                                    prop("volume", &format!("{v:.0}"));
                                }
                            }
                            Cmd::AudioDelay(offset) => {
                                let delay = if pcm_pipe.is_some() { DSP_AUDIO_DELAY_BASE_SECS + offset } else { offset };
                                prop("audio-delay", &format!("{delay:.4}"));
                            }
                            Cmd::Crop(c) => {
                                prop("panscan", if c { "1.0" } else { "0.0" });
                            }
                            Cmd::Key(k) => {
                                cmd(&["keypress", &k]);
                                if let Some(p) = get_flag("pause") {
                                    audio.set_paused(p && !shuttle_active);
                                    was_paused = p;
                                }
                            }
                            Cmd::Seek(_) | Cmd::SeekFast(_) => {}
                        }
                    }
                }
            }
            flush_seek(&mut pending_seek, &mut pending_seek_fast, shuttle_active);
            loop {
                let ev = (lib.wait_event)(h, 0.0);
                if ev.is_null() || (*ev).event_id == 0 {
                    break;
                }
                match (*ev).event_id {
                    EV_SHUTDOWN => break 'outer,
                    EV_FILE_LOADED => {
                        let mut s = sh.lock().unwrap();
                        s.status = "Playing".into();
                        s.ended = false;
                        s.seeking = false;
                    }
                    EV_SEEK => {
                        sh.lock().unwrap().seeking = true;
                        audio.clear();
                    }
                    EV_PLAYBACK_RESTART => {
                        sh.lock().unwrap().seeking = false;
                        if !shuttle_active {
                            audio.clear();
                        }
                    }
                    EV_END_FILE => {
                        let mut s = sh.lock().unwrap();
                        s.ended = true;
                        s.seeking = false;
                        if (*ev).error < 0 || s.frame_no == 0 {
                            s.status = "mpv couldn't play that (check the path or URL).".into();
                        } else {
                            s.status = "Finished.".into();
                        }
                    }
                    _ => {}
                }
            }
            if (lib.rc_update)(rc) & 1 != 0 {
                let mut p = [
                    RenderParam { kind: RP_SW_SIZE, data: size.as_mut_ptr() as *mut c_void },
                    RenderParam { kind: RP_SW_FORMAT, data: fmt.as_ptr() as *mut c_void },
                    RenderParam { kind: RP_SW_STRIDE, data: &mut stride as *mut usize as *mut c_void },
                    RenderParam { kind: RP_SW_POINTER, data: buf.ptr as *mut c_void },
                    RenderParam { kind: 0, data: std::ptr::null_mut() },
                ];
                if (lib.rc_render)(rc, p.as_mut_ptr()) >= 0 {
                    publish(sh, buf.as_slice().to_vec());
                }
            }
            if last_poll.elapsed() > Duration::from_millis(120) {
                last_poll = Instant::now();
                let pos = get_f64("time-pos");
                let dur = get_f64("duration");
                let paused = get_flag("pause");
                let seeking = get_flag("seeking");
                let avsync = get_f64("avsync").unwrap_or(0.0);
                let audio_pts = get_f64("audio-pts");
                let is_paused = paused.unwrap_or(was_paused);
                let is_seeking = seeking.unwrap_or(false);
                audio.set_paused(is_paused && !shuttle_active);
                let cur_bytes = audio.bytes_in.load(Ordering::Relaxed);
                let bytes_delta = cur_bytes.saturating_sub(last_poll_bytes);
                last_poll_bytes = cur_bytes;
                let cur_pos = pos.unwrap_or(last_poll_pos.max(0.0));
                let pos_advancing = pos.is_some() && last_poll_pos >= 0.0 && (cur_pos - last_poll_pos) > 0.02;
                if let Some(p) = pos {
                    last_poll_pos = p;
                }

                // Track prolonged silent playback while video is actively advancing:
                // catches mpv's `delaying audio start X vs Y` state where `avsync` reports 0.00
                // even though `audio-pts` is 30-60s ahead of `video-pts`.
                // Note: with the 512 KiB audio pipe, ao_pcm's initial 65536-sample burst drains
                // smoothly at 192 KB/s with zero silence (`bytes_delta > 0` on every tick), while
                // `audio-pts` temporarily leads `pos` by ~1.4s. Therefore we NEVER trigger recovery
                // while PCM bytes are actively flowing, and we only clear `stall_recoveries` after
                // 4 seconds of continuous healthy audio (preventing any recovery loop).
                if !is_paused && !shuttle_active && !is_seeking {
                    if bytes_delta > 0 {
                        silent_play_since = None;
                        let h0 = healthy_audio_since.get_or_insert_with(Instant::now);
                        if h0.elapsed() >= Duration::from_secs(4) {
                            stall_recoveries = 0;
                        }
                    } else if pos_advancing {
                        healthy_audio_since = None;
                        if silent_play_since.is_none() {
                            silent_play_since = Some(Instant::now());
                        }
                    }
                } else {
                    silent_play_since = None;
                    healthy_audio_since = None;
                }

                let silent_ms = silent_play_since.map_or(0, |t0| t0.elapsed().as_millis());
                let apts_runaway = bytes_delta == 0
                    && silent_ms >= 900
                    && pos.is_some()
                    && audio_pts.is_some_and(|a| (a - cur_pos).abs() > 3.5);
                let prolonged_silence = silent_ms >= 1800;
                let severe_desync = bytes_delta == 0 && silent_ms >= 900 && avsync.abs() > 3.5;

                // Only resync on unpause if audio-pts genuinely ran far away (> 3.5s); normal
                // early pauses during ao_pcm's 1.36s burst resume cleanly from the kernel pipe.
                if was_paused && !is_paused && !shuttle_active {
                    let far_drift = pos.is_some() && audio_pts.is_some_and(|a| (a - cur_pos).abs() > 3.5);
                    if far_drift && last_resync.elapsed() > Duration::from_secs(2) {
                        audio.drain_pipe();
                        prop("aid", "auto");
                        cmd(&["seek", &format!("{cur_pos:.3}"), "absolute"]);
                        last_resync = Instant::now();
                    }
                } else if (apts_runaway || prolonged_silence || severe_desync)
                    && stall_recoveries < 2
                    && last_resync.elapsed() > Duration::from_secs(4)
                {
                    // Self-healing recovery for `delaying audio start` or severe A/V desync:
                    // Attempt 1 re-attaches audio BEFORE an exact `absolute` seek to `cur_pos`.
                    // Attempt 2 reloads the stream cleanly at `cur_pos`. Capped at 2 attempts
                    // until 4s of continuous healthy audio resets the counter.
                    stall_recoveries = stall_recoveries.saturating_add(1);
                    silent_play_since = None;
                    healthy_audio_since = None;
                    last_resync = Instant::now();
                    if verbose_log {
                        eprintln!(
                            "[victr-mpv] audio stall detected (attempt={}, pos={:.2}, apts={:?}, avsync={:.2}), resyncing…",
                            stall_recoveries, cur_pos, audio_pts, avsync
                        );
                    }
                    if stall_recoveries == 1 {
                        prop("aid", "no");
                        audio.drain_pipe();
                        prop("aid", "auto");
                        cmd(&["seek", &format!("{cur_pos:.3}"), "absolute"]);
                    } else if stall_recoveries == 2 {
                        audio.drain_pipe();
                        let start_opt = format!("start={cur_pos:.2}");
                        cmd(&["loadfile", src, "replace", &start_opt]);
                    }
                }
                was_paused = is_paused;
                let mut s = sh.lock().unwrap();
                if let Some(p) = pos {
                    s.pos = p;
                }
                if let Some(d) = dur {
                    s.duration = d;
                }
                if let Some(p) = paused {
                    s.paused = p;
                }
                if let Some(sk) = seeking {
                    s.seeking = sk;
                }
                if verbose_log && last_log.elapsed() >= Duration::from_secs(1) {
                    let dt_s = last_log.elapsed().as_secs_f64().max(0.001);
                    let bps = (cur_bytes.saturating_sub(last_log_bytes)) as f64 / dt_s;
                    last_log_bytes = cur_bytes;
                    last_log = Instant::now();
                    eprintln!(
                        "[victr-mpv] pos={:.2} apts={:?} avsync={:.2} paused={} seeking={} shuttle={} pcm={:.0}B/s",
                        s.pos, audio_pts, avsync, is_paused, is_seeking, shuttle_active, bps
                    );
                }
            }
            std::thread::sleep(Duration::from_millis(3));
        }
        audio.stop.store(true, Ordering::SeqCst);
        audio.set_paused(false);
        audio.clear();
        (lib.rc_free)(rc);
        (lib.terminate_destroy)(h);
        close_pipe(pcm_pipe.map(|(_, w)| (-1, w)));
        audio.clear();
        Ok(())
    }
}

// ------------------------------------------------------------------ pipes

#[cfg(unix)]
const POLLIN: i16 = 0x0001;
#[cfg(unix)]
const POLLERR: i16 = 0x0008;
#[cfg(unix)]
const POLLHUP: i16 = 0x0010;

#[cfg(unix)]
#[repr(C)]
struct PollFd {
    fd: c_int,
    events: i16,
    revents: i16,
}

#[cfg(unix)]
unsafe extern "C" {
    fn pipe(fds: *mut c_int) -> c_int;
    fn close(fd: c_int) -> c_int;
    fn poll(fds: *mut PollFd, nfds: usize, timeout: c_int) -> c_int;
    #[cfg(target_os = "linux")]
    fn fcntl(fd: c_int, cmd: c_int, ...) -> c_int;
    #[cfg(target_os = "linux")]
    fn ioctl(fd: c_int, request: std::ffi::c_ulong, ...) -> c_int;
}

#[cfg(target_os = "linux")]
fn pipe_readable_bytes(fd: c_int) -> usize {
    // FIONREAD = 0x541B on Linux: exact byte count currently buffered in the pipe.
    let mut avail: c_int = 0;
    if unsafe { ioctl(fd, 0x541B, &mut avail as *mut c_int) } == 0 && avail > 0 {
        avail as usize
    } else {
        0
    }
}
#[cfg(all(unix, not(target_os = "linux")))]
fn pipe_readable_bytes(_fd: c_int) -> usize {
    0
}

#[cfg(target_os = "linux")]
fn size_audio_pipe_fd(fd: c_int) {
    // F_SETPIPE_SZ = 1031: mpv's `ao=pcm` hardcodes a 65,536-sample (262,144 B = 1.365 s)
    // initial device buffer burst on every start, seek, and track attach. A 4 KiB pipe blocked
    // mpv's core inside `fwrite()` for 1.34 s and left a ~1.3 s silence gap while A-V caught up.
    // Sizing the pipe to 512 KiB absorbs the initial burst with zero blocking and naturally
    // drains down to ~12 KiB (~60 ms) at steady state.
    unsafe { fcntl(fd, 1031, 524_288 as c_int) };
}
#[cfg(not(target_os = "linux"))]
fn size_audio_pipe_fd(_fd: c_int) {}

#[cfg(unix)]
fn make_pipe() -> Option<(c_int, c_int)> {
    let mut fds = [0 as c_int; 2];
    if unsafe { pipe(fds.as_mut_ptr()) } == 0 {
        size_audio_pipe_fd(fds[1]);
        Some((fds[0], fds[1]))
    } else {
        None
    }
}
#[cfg(not(unix))]
fn make_pipe() -> Option<(c_int, c_int)> {
    None
}

#[cfg(unix)]
fn close_pipe(p: Option<(c_int, c_int)>) {
    if let Some((r, w)) = p {
        unsafe {
            if r >= 0 {
                close(r);
            }
            if w >= 0 {
                close(w);
            }
        }
    }
}
#[cfg(not(unix))]
fn close_pipe(_p: Option<(c_int, c_int)>) {}

#[cfg(unix)]
fn fd_reader(fd: c_int) -> std::fs::File {
    use std::os::unix::io::FromRawFd;
    unsafe { std::fs::File::from_raw_fd(fd) }
}
#[cfg(not(unix))]
fn fd_reader(_fd: c_int) -> std::io::Empty {
    std::io::empty()
}


// ------------------------------------------------------------------ fallback: mpv command

/// Where the `mpv` program lives.
#[derive(Clone, Copy, PartialEq)]
enum Route {
    /// Next to us (normal desktop use).
    Local,
    /// On the host system while running inside a Flatpak sandbox: started with
    /// `flatpak-spawn --host`. Pictures and sound still come back through pipes.
    Host,
}

fn quiet_ok(mut c: Command) -> bool {
    c.stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

fn find_route() -> Option<Route> {
    let mut local = Command::new("mpv");
    local.arg("--version");
    if quiet_ok(local) {
        return Some(Route::Local);
    }
    if std::path::Path::new("/.flatpak-info").exists() {
        let mut host = Command::new("flatpak-spawn");
        host.args(["--host", "mpv", "--version"]);
        if quiet_ok(host) {
            return Some(Route::Host);
        }
    }
    None
}

fn mpv_command(route: Route) -> Command {
    match route {
        Route::Local => Command::new("mpv"),
        Route::Host => {
            let mut c = Command::new("flatpak-spawn");
            // --watch-bus: the host mpv exits if we do
            c.args(["--host", "--watch-bus", "mpv"]);
            c
        }
    }
}

/// Stops an mpv process. SIGTERM first: flatpak-spawn forwards it to the real
/// mpv on the host (a hard kill would only stop the go-between).
fn kill_child(c: &mut Option<Child>) {
    if let Some(ch) = c.as_mut() {
        #[cfg(unix)]
        {
            unsafe extern "C" {
                fn kill(pid: c_int, sig: c_int) -> c_int;
            }
            unsafe { kill(ch.id() as c_int, 15) };
            let t = Instant::now();
            while t.elapsed() < Duration::from_millis(500) {
                if let Ok(Some(_)) = ch.try_wait() {
                    break;
                }
                std::thread::sleep(Duration::from_millis(10));
            }
        }
        let _ = ch.kill();
        let _ = ch.wait();
    }
    *c = None;
}

const FPS: f64 = 60000.0 / 1001.0;

fn spawn_video(route: Route, src: &str, start: f64, crop: bool, user_config: bool) -> std::io::Result<Child> {
    let fit = if crop {
        format!("scale={FW}:{FH}:force_original_aspect_ratio=increase,crop={FW}:{FH}")
    } else {
        format!("scale={FW}:{FH}:force_original_aspect_ratio=decrease,pad={FW}:{FH}:-1:-1")
    };
    let mut c = mpv_command(route);
    c.arg(src);
    if !user_config {
        c.arg("--no-config");
    }
    c.arg("--really-quiet")
        .arg("--no-terminal")
        .arg("--no-audio")
        .arg(format!("--start={start}"))
        // one picture per TV field (59.94 per second)
        .arg(format!("--vf=lavfi=[{fit},fps=60000/1001,format=rgba]"))
        .arg("--of=rawvideo")
        .arg("--ovc=rawvideo")
        .arg("--o=-")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
}

fn spawn_audio(route: Route, src: &str, start: f64, volume: f64, user_config: bool, audio: &Audio) -> Option<Child> {
    let mut c = mpv_command(route);
    c.arg(src);
    if !user_config {
        c.arg("--no-config");
    }
    c.arg("--really-quiet")
        .arg("--no-terminal")
        .arg("--no-video")
        .arg(format!("--start={start}"))
        .stdin(Stdio::null())
        .stderr(Stdio::null());
    if cfg!(unix) && audio.ring.is_some() {
        c.args([
            "--ao=pcm",
            "--ao-pcm-file=/dev/stdout",
            "--ao-pcm-waveheader=no",
            "--audio-format=s16",
            "--audio-samplerate=48000",
            "--audio-channels=stereo",
            "--audio-buffer=0.15",
        ])
        .stdout(Stdio::piped());
    } else {
        c.arg(format!("--volume={volume:.0}")).stdout(Stdio::null());
    }
    let mut child = c.spawn().ok()?;
    if let Some(out) = child.stdout.take() {
        #[cfg(target_os = "linux")]
        {
            use std::os::unix::io::AsRawFd;
            size_audio_pipe_fd(out.as_raw_fd());
        }
        audio.pump(out);
    }
    Some(child)
}

#[allow(clippy::too_many_arguments)]
fn run_cli(
    src: &str,
    mut volume: f64,
    mut crop: bool,
    first_start: f64,
    user_config: bool,
    rx: &Receiver<Cmd>,
    sh: &Arc<Mutex<Shared>>,
    lib_err: &str,
    audio: &Audio,
) -> Result<(), String> {
    let route = find_route().ok_or_else(|| {
        format!("Couldn't find mpv ({lib_err}, and no `mpv` program, here or on your system). Install mpv and try again.")
    })?;
    let frame_len = FW * FH * 4;
    let mut start = first_start.max(0.0);
    let mut paused = false;
    'restart: loop {
        // paused: nothing runs; wait for play/seek/stop
        while paused {
            sh.lock().unwrap().paused = true;
            match rx.recv() {
                Ok(Cmd::TogglePause) | Ok(Cmd::Key(_)) => paused = false,
                Ok(Cmd::SetPaused(p)) => paused = p,
                Ok(Cmd::Seek(d)) | Ok(Cmd::SeekFast(d)) => {
                    start = (start + d).max(0.0);
                    sh.lock().unwrap().pos = start;
                }
                Ok(Cmd::SeekAbs(t)) => {
                    start = t.max(0.0);
                    sh.lock().unwrap().pos = start;
                }
                Ok(Cmd::Speed(_)) | Ok(Cmd::SetShuttle(_)) => {}
                Ok(Cmd::Volume(v)) => volume = v,
                Ok(Cmd::AudioDelay(_)) => {}
                Ok(Cmd::Crop(c)) => crop = c,
                Ok(Cmd::Stop) | Err(_) => return Ok(()),
            }
        }
        let mut video = Some(spawn_video(route, src, start, crop, user_config).map_err(|e| format!("Couldn't start mpv: {e}"))?);
        audio.clear();
        let mut audio_p = spawn_audio(route, src, start, volume, user_config, audio);
        let mut out = video.as_mut().unwrap().stdout.take().unwrap();
        {
            let mut s = sh.lock().unwrap();
            s.backend = match (route, audio.ring.is_some()) {
                (Route::Local, true) => "mpv program, sound through the TV",
                (Route::Local, false) => "mpv program",
                (Route::Host, true) => "your system's mpv (from the sandbox), sound through the TV",
                (Route::Host, false) => "your system's mpv (from the sandbox)",
            };
            s.status = "Playing".into();
            s.ended = false;
            s.paused = false;
            s.pos = start;
        }
        // read frames on a helper thread so pausing/stopping stays responsive
        let (ftx, frx) = channel::<Vec<u8>>();
        std::thread::spawn(move || {
            loop {
                let mut buf = vec![0u8; frame_len];
                if out.read_exact(&mut buf).is_err() {
                    break;
                }
                if ftx.send(buf).is_err() {
                    break;
                }
            }
        });
        let t0 = Instant::now();
        let mut shown = 0u64;
        let mut got_any = false;
        loop {
            while let Ok(c) = rx.try_recv() {
                // the mpv program can't take key presses here: map mpv's usual default keys
                let c = match c {
                    Cmd::Key(k) => match k.as_str() {
                        "SPACE" | "p" => Cmd::TogglePause,
                        "LEFT" => Cmd::Seek(-5.0),
                        "RIGHT" => Cmd::Seek(5.0),
                        "UP" => Cmd::Seek(60.0),
                        "DOWN" => Cmd::Seek(-60.0),
                        "q" => Cmd::Stop,
                        _ => continue,
                    },
                    other => other,
                };
                let pos = sh.lock().unwrap().pos;
                match c {
                    Cmd::Key(_) => {}
                    Cmd::Stop => {
                        kill_child(&mut video);
                        kill_child(&mut audio_p);
                        return Ok(());
                    }
                    Cmd::TogglePause => {
                        // pause by stopping both processes and remembering the position
                        start = pos;
                        paused = true;
                        kill_child(&mut video);
                        kill_child(&mut audio_p);
                        audio.clear();
                        continue 'restart;
                    }
                    Cmd::SetPaused(p) => {
                        if p {
                            start = pos;
                            paused = true;
                            kill_child(&mut video);
                            kill_child(&mut audio_p);
                            audio.clear();
                            continue 'restart;
                        }
                    }
                    Cmd::Speed(_) | Cmd::SetShuttle(_) => {}
                    Cmd::Seek(d) | Cmd::SeekFast(d) => {
                        start = (pos + d).max(0.0);
                        kill_child(&mut video);
                        kill_child(&mut audio_p);
                        continue 'restart;
                    }
                    Cmd::SeekAbs(t) => {
                        start = t.max(0.0);
                        kill_child(&mut video);
                        kill_child(&mut audio_p);
                        continue 'restart;
                    }
                    Cmd::Volume(v) => volume = v,
                    Cmd::AudioDelay(_) => {}
                    Cmd::Crop(c) => {
                        crop = c;
                        start = pos;
                        kill_child(&mut video);
                        kill_child(&mut audio_p);
                        continue 'restart;
                    }
                }
            }
            // pace frames at 59.94 per second against the wall clock (the sound process runs on its own)
            let due = (t0.elapsed().as_secs_f64() * FPS) as u64;
            if shown <= due {
                match frx.recv_timeout(Duration::from_millis(500)) {
                    Ok(f) => {
                        got_any = true;
                        shown += 1;
                        publish(sh, f);
                        sh.lock().unwrap().pos = start + shown as f64 / FPS;
                    }
                    Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
                    Err(_) => {
                        kill_child(&mut video);
                        kill_child(&mut audio_p);
                        let mut s = sh.lock().unwrap();
                        s.ended = true;
                        s.status = if got_any {
                            "Finished.".into()
                        } else if route == Route::Host {
                            "mpv couldn't play that. Files must be somewhere your system can see (e.g. your home folder).".into()
                        } else {
                            "mpv couldn't play that (check the path or URL).".into()
                        };
                        return Ok(());
                    }
                }
            } else {
                std::thread::sleep(Duration::from_millis(2));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn system_mpv_sound_reaches_the_tv() {
        let Some(route) = find_route() else { return };
        let ring = AudioRing::new();
        let session = ring.begin_session();
        let audio = Audio::new(Some(ring.clone()), session, Arc::new(AtomicBool::new(false)));
        let mut child = spawn_audio(route, "av://lavfi:sine=frequency=440:duration=2", 0.0, 100.0, false, &audio);
        assert!(child.is_some(), "mpv should start");
        let t = Instant::now();
        let mut got = 0;
        while t.elapsed() < Duration::from_secs(8) {
            got = ring.len();
            if got > 2000 {
                break;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        audio.stop.store(true, Ordering::SeqCst);
        kill_child(&mut child);
        println!("route {:?}, samples in ring: {got}", route == Route::Host);
        assert!(got > 2000);
    }
}
