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
    pub ended: bool,
    pub running: bool,
    pub backend: &'static str,
}

enum Cmd {
    TogglePause,
    Seek(f64),
    SeekAbs(f64),
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
}

impl Audio {
    /// Copies raw s16le stereo PCM from `r` into the ring until EOF or stop.
    fn pump(&self, mut r: impl Read + Send + 'static) {
        if let Some(ring) = self.ring.clone() {
            let stop = self.stop.clone();
            let session = self.session;
            std::thread::spawn(move || {
                // 1024 B = 256 stereo frames (~5.3 ms) so samples reach the DSP ring promptly
                let mut buf = vec![0u8; 1024];
                loop {
                    match r.read(&mut buf) {
                        Ok(0) | Err(_) => break,
                        Ok(n) => {
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
            let audio = Audio { ring: audio, session, stop: stop2 };
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
    pub fn seek(&self, secs: f64) {
        let _ = self.tx.send(Cmd::Seek(secs));
    }
    pub fn seek_abs(&self, secs: f64) {
        let _ = self.tx.send(Cmd::SeekAbs(secs));
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
/// ~21 ms ao_pcm stdio + ~21 ms 4 KB pcm_pipe + ~45 ms AudioRing + ~5 ms block + ~21 ms out pipe + ~35 ms sink
/// minus ~28 ms video presentation latency ≈ 120 ms net audio lag -> advance audio by -0.12 s.
const DSP_AUDIO_DELAY_BASE_SECS: f64 = -0.12;

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
        if o.start > 0.0 {
            opt("start", &format!("{:.3}", o.start));
        }
        // Route the decoded sound into our TV sound path through a pipe.
        let pcm_pipe = if audio.ring.is_some() { make_pipe() } else { None };
        // Settings the TV needs no matter what the person's config says. They are
        // applied before start-up and again afterwards, because mpv.conf is read
        // during start-up and could override them (e.g. vo=gpu or a custom ao).
        let essentials = |set: &dyn Fn(&str, &str) -> c_int| {
            set("vo", "libmpv");
            set("terminal", "no");
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
            audio.pump(fd_reader(r));
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
        'outer: loop {
            while let Ok(c) = rx.try_recv() {
                match c {
                    Cmd::Stop => break 'outer,
                    Cmd::TogglePause => {
                        cmd(&["cycle", "pause"]);
                    }
                    Cmd::Seek(s) => {
                        cmd(&["seek", &format!("{s}"), "relative"]);
                        audio.clear();
                    }
                    Cmd::SeekAbs(s) => {
                        cmd(&["seek", &format!("{s}"), "absolute"]);
                        audio.clear();
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
                    }
                }
            }
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
                    }
                    EV_END_FILE => {
                        let mut s = sh.lock().unwrap();
                        s.ended = true;
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
            if last_poll.elapsed() > Duration::from_millis(200) {
                last_poll = Instant::now();
                let pos = get_f64("time-pos");
                let dur = get_f64("duration");
                let paused = get_flag("pause");
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
            }
            std::thread::sleep(Duration::from_millis(3));
        }
        (lib.rc_free)(rc);
        (lib.terminate_destroy)(h);
        close_pipe(pcm_pipe.map(|(_, w)| (-1, w)));
        audio.clear();
        Ok(())
    }
}

// ------------------------------------------------------------------ pipes

#[cfg(unix)]
unsafe extern "C" {
    fn pipe(fds: *mut c_int) -> c_int;
    fn close(fd: c_int) -> c_int;
    #[cfg(target_os = "linux")]
    fn fcntl(fd: c_int, cmd: c_int, ...) -> c_int;
}

#[cfg(target_os = "linux")]
fn shrink_pipe_fd(fd: c_int) {
    // F_SETPIPE_SZ = 1031: shrink Linux's 64 KB default pipe (~341 ms at 48 kHz stereo s16)
    // down to 1 page (4096 B = ~21 ms) so audio stays tightly lip-synced to video.
    unsafe { fcntl(fd, 1031, 4096 as c_int) };
}
#[cfg(not(target_os = "linux"))]
fn shrink_pipe_fd(_fd: c_int) {}

#[cfg(unix)]
fn make_pipe() -> Option<(c_int, c_int)> {
    let mut fds = [0 as c_int; 2];
    if unsafe { pipe(fds.as_mut_ptr()) } == 0 {
        shrink_pipe_fd(fds[1]);
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
            shrink_pipe_fd(out.as_raw_fd());
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
                Ok(Cmd::Seek(d)) => {
                    start = (start + d).max(0.0);
                    sh.lock().unwrap().pos = start;
                }
                Ok(Cmd::SeekAbs(t)) => {
                    start = t.max(0.0);
                    sh.lock().unwrap().pos = start;
                }
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
                    Cmd::Seek(d) => {
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
        let audio = Audio { ring: Some(ring.clone()), session, stop: Arc::new(AtomicBool::new(false)) };
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
