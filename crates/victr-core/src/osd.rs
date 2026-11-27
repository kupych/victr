//! Classic 1990s CRT TV On-Screen Display (OSD) and crisp 8x8 bitmap pixel font.
//!
//! Rendered into a 640x480 RGBA buffer and injected post-NTSC-decoder right
//! before the electron guns (modeling a real TV Jungle IC's RGB fast-blanking
//! input), so the phosphor-green/white text stays crisp over snowy RF signals
//! while still passing through scanlines, shadow mask triads, and glass bloom.

use std::sync::Arc;
use web_time::Instant;

pub const OSD_W: usize = 640;
pub const OSD_H: usize = 480;

// ------------------------------------------------------------------ 8x8 font

/// Compact 8x8 bitmap font for ASCII 32..=95 (uppercase + digits + punctuation).
/// Lowercase letters automatically fold to uppercase for classic 90s TV OSD/Prevue authenticity
/// unless `draw_text_mixed` is used with a small-caps feel.
pub fn glyph_8x8(ch: char) -> [u8; 8] {
    let c = ch.to_ascii_uppercase();
    match c {
        ' ' => [0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00],
        '!' => [0x18, 0x18, 0x18, 0x18, 0x18, 0x00, 0x18, 0x00],
        '"' => [0x66, 0x66, 0x24, 0x00, 0x00, 0x00, 0x00, 0x00],
        '#' => [0x24, 0x7E, 0x24, 0x24, 0x7E, 0x24, 0x00, 0x00],
        '$' => [0x18, 0x3E, 0x60, 0x3C, 0x06, 0x7C, 0x18, 0x00],
        '%' => [0x62, 0x64, 0x08, 0x10, 0x26, 0x46, 0x00, 0x00],
        '&' => [0x38, 0x6C, 0x38, 0x76, 0xDC, 0xCC, 0x76, 0x00],
        '\'' => [0x18, 0x18, 0x30, 0x00, 0x00, 0x00, 0x00, 0x00],
        '(' => [0x0C, 0x18, 0x30, 0x30, 0x30, 0x18, 0x0C, 0x00],
        ')' => [0x30, 0x18, 0x0C, 0x0C, 0x0C, 0x18, 0x30, 0x00],
        '*' => [0x00, 0x66, 0x3C, 0xFF, 0x3C, 0x66, 0x00, 0x00],
        '+' => [0x00, 0x18, 0x18, 0x7E, 0x18, 0x18, 0x00, 0x00],
        ',' => [0x00, 0x00, 0x00, 0x00, 0x00, 0x18, 0x18, 0x30],
        '-' => [0x00, 0x00, 0x00, 0x7E, 0x00, 0x00, 0x00, 0x00],
        '.' => [0x00, 0x00, 0x00, 0x00, 0x00, 0x18, 0x18, 0x00],
        '/' => [0x02, 0x06, 0x0C, 0x18, 0x30, 0x60, 0x40, 0x00],
        '0' => [0x3C, 0x66, 0x6E, 0x76, 0x66, 0x66, 0x3C, 0x00],
        '1' => [0x18, 0x38, 0x18, 0x18, 0x18, 0x18, 0x7E, 0x00],
        '2' => [0x3C, 0x66, 0x06, 0x1C, 0x30, 0x60, 0x7E, 0x00],
        '3' => [0x3C, 0x66, 0x06, 0x1C, 0x06, 0x66, 0x3C, 0x00],
        '4' => [0x0C, 0x1C, 0x3C, 0x6C, 0x7E, 0x0C, 0x0C, 0x00],
        '5' => [0x7E, 0x60, 0x7C, 0x06, 0x06, 0x66, 0x3C, 0x00],
        '6' => [0x3C, 0x60, 0x7C, 0x66, 0x66, 0x66, 0x3C, 0x00],
        '7' => [0x7E, 0x66, 0x0C, 0x18, 0x18, 0x18, 0x18, 0x00],
        '8' => [0x3C, 0x66, 0x66, 0x3C, 0x66, 0x66, 0x3C, 0x00],
        '9' => [0x3C, 0x66, 0x66, 0x3E, 0x06, 0x0C, 0x38, 0x00],
        ':' => [0x00, 0x18, 0x18, 0x00, 0x18, 0x18, 0x00, 0x00],
        ';' => [0x00, 0x18, 0x18, 0x00, 0x18, 0x18, 0x30, 0x00],
        '<' => [0x0C, 0x18, 0x30, 0x60, 0x30, 0x18, 0x0C, 0x00],
        '=' => [0x00, 0x00, 0x7E, 0x00, 0x7E, 0x00, 0x00, 0x00],
        '>' => [0x30, 0x18, 0x0C, 0x06, 0x0C, 0x18, 0x30, 0x00],
        '?' => [0x3C, 0x66, 0x06, 0x1C, 0x18, 0x00, 0x18, 0x00],
        '@' => [0x3C, 0x66, 0x6E, 0x6E, 0x60, 0x62, 0x3C, 0x00],
        'A' => [0x18, 0x3C, 0x66, 0x66, 0x7E, 0x66, 0x66, 0x00],
        'B' => [0x7C, 0x66, 0x66, 0x7C, 0x66, 0x66, 0x7C, 0x00],
        'C' => [0x3C, 0x66, 0x60, 0x60, 0x60, 0x66, 0x3C, 0x00],
        'D' => [0x78, 0x6C, 0x66, 0x66, 0x66, 0x6C, 0x78, 0x00],
        'E' => [0x7E, 0x60, 0x60, 0x78, 0x60, 0x60, 0x7E, 0x00],
        'F' => [0x7E, 0x60, 0x60, 0x78, 0x60, 0x60, 0x60, 0x00],
        'G' => [0x3C, 0x66, 0x60, 0x6E, 0x66, 0x66, 0x3E, 0x00],
        'H' => [0x66, 0x66, 0x66, 0x7E, 0x66, 0x66, 0x66, 0x00],
        'I' => [0x3C, 0x18, 0x18, 0x18, 0x18, 0x18, 0x3C, 0x00],
        'J' => [0x1E, 0x0C, 0x0C, 0x0C, 0x6C, 0x6C, 0x38, 0x00],
        'K' => [0x66, 0x6C, 0x78, 0x70, 0x78, 0x6C, 0x66, 0x00],
        'L' => [0x60, 0x60, 0x60, 0x60, 0x60, 0x60, 0x7E, 0x00],
        'M' => [0x63, 0x77, 0x7F, 0x6B, 0x63, 0x63, 0x63, 0x00],
        'N' => [0x66, 0x76, 0x7E, 0x7E, 0x6E, 0x66, 0x66, 0x00],
        'O' => [0x3C, 0x66, 0x66, 0x66, 0x66, 0x66, 0x3C, 0x00],
        'P' => [0x7C, 0x66, 0x66, 0x7C, 0x60, 0x60, 0x60, 0x00],
        'Q' => [0x3C, 0x66, 0x66, 0x66, 0x6A, 0x6C, 0x36, 0x00],
        'R' => [0x7C, 0x66, 0x66, 0x7C, 0x6C, 0x66, 0x66, 0x00],
        'S' => [0x3C, 0x66, 0x60, 0x3C, 0x06, 0x66, 0x3C, 0x00],
        'T' => [0x7E, 0x18, 0x18, 0x18, 0x18, 0x18, 0x18, 0x00],
        'U' => [0x66, 0x66, 0x66, 0x66, 0x66, 0x66, 0x3C, 0x00],
        'V' => [0x66, 0x66, 0x66, 0x66, 0x66, 0x3C, 0x18, 0x00],
        'W' => [0x63, 0x63, 0x63, 0x6B, 0x7F, 0x77, 0x63, 0x00],
        'X' => [0x66, 0x66, 0x3C, 0x18, 0x3C, 0x66, 0x66, 0x00],
        'Y' => [0x66, 0x66, 0x66, 0x3C, 0x18, 0x18, 0x18, 0x00],
        'Z' => [0x7E, 0x06, 0x0C, 0x18, 0x30, 0x60, 0x7E, 0x00],
        '[' => [0x3C, 0x30, 0x30, 0x30, 0x30, 0x30, 0x3C, 0x00],
        '\\' => [0x40, 0x60, 0x30, 0x18, 0x0C, 0x06, 0x02, 0x00],
        ']' => [0x3C, 0x0C, 0x0C, 0x0C, 0x0C, 0x0C, 0x3C, 0x00],
        '^' => [0x18, 0x3C, 0x66, 0x00, 0x00, 0x00, 0x00, 0x00],
        '_' => [0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x7E, 0x00],
        '|' => [0x18, 0x18, 0x18, 0x18, 0x18, 0x18, 0x18, 0x00],
        '·' | '•' => [0x00, 0x00, 0x00, 0x18, 0x18, 0x00, 0x00, 0x00],
        '►' => [0x40, 0x70, 0x7C, 0x7F, 0x7C, 0x70, 0x40, 0x00],
        '■' => [0x00, 0x7E, 0x7E, 0x7E, 0x7E, 0x7E, 0x00, 0x00],
        _ => [0x3C, 0x66, 0x06, 0x1C, 0x18, 0x00, 0x18, 0x00],
    }
}

// ------------------------------------------------------------------ 2D raster helpers

pub struct Canvas {
    pub w: usize,
    pub h: usize,
    pub rgba: Vec<u8>,
}

impl Canvas {
    pub fn new(w: usize, h: usize) -> Self {
        Self { w, h, rgba: vec![0u8; w * h * 4] }
    }

    pub fn clear(&mut self, c: [u8; 4]) {
        for px in self.rgba.chunks_exact_mut(4) {
            px.copy_from_slice(&c);
        }
    }

    #[inline]
    pub fn put(&mut self, x: i32, y: i32, c: [u8; 4]) {
        if x < 0 || y < 0 || x >= self.w as i32 || y >= self.h as i32 {
            return;
        }
        let idx = (y as usize * self.w + x as usize) * 4;
        if c[3] == 255 {
            self.rgba[idx..idx + 4].copy_from_slice(&c);
        } else if c[3] > 0 {
            let a = c[3] as u32;
            let ia = 255 - a;
            let dst = &mut self.rgba[idx..idx + 4];
            // Straight-alpha over-blend
            dst[0] = ((c[0] as u32 * a + dst[0] as u32 * ia) / 255) as u8;
            dst[1] = ((c[1] as u32 * a + dst[1] as u32 * ia) / 255) as u8;
            dst[2] = ((c[2] as u32 * a + dst[2] as u32 * ia) / 255) as u8;
            dst[3] = (a + (dst[3] as u32 * ia) / 255).min(255) as u8;
        }
    }

    pub fn fill_rect(&mut self, x: i32, y: i32, w: i32, h: i32, c: [u8; 4]) {
        let x0 = x.max(0).min(self.w as i32);
        let y0 = y.max(0).min(self.h as i32);
        let x1 = (x + w).max(0).min(self.w as i32);
        let y1 = (y + h).max(0).min(self.h as i32);
        for py in y0..y1 {
            for px in x0..x1 {
                self.put(px, py, c);
            }
        }
    }

    pub fn fill_vgrad(&mut self, x: i32, y: i32, w: i32, h: i32, top: [u8; 4], bot: [u8; 4]) {
        if h <= 0 || w <= 0 {
            return;
        }
        let x0 = x.max(0).min(self.w as i32);
        let x1 = (x + w).max(0).min(self.w as i32);
        let y0 = y.max(0).min(self.h as i32);
        let y1 = (y + h).max(0).min(self.h as i32);
        for py in y0..y1 {
            let t = ((py - y) as f32 / (h - 1).max(1) as f32).clamp(0.0, 1.0);
            let col = [
                (top[0] as f32 + (bot[0] as f32 - top[0] as f32) * t) as u8,
                (top[1] as f32 + (bot[1] as f32 - top[1] as f32) * t) as u8,
                (top[2] as f32 + (bot[2] as f32 - top[2] as f32) * t) as u8,
                (top[3] as f32 + (bot[3] as f32 - top[3] as f32) * t) as u8,
            ];
            for px in x0..x1 {
                self.put(px, py, col);
            }
        }
    }

    pub fn stroke_rect(&mut self, x: i32, y: i32, w: i32, h: i32, thick: i32, c: [u8; 4]) {
        self.fill_rect(x, y, w, thick, c);
        self.fill_rect(x, y + h - thick, w, thick, c);
        self.fill_rect(x, y, thick, h, c);
        self.fill_rect(x + w - thick, y, thick, h, c);
    }

    pub fn bevel_rect(&mut self, x: i32, y: i32, w: i32, h: i32, fill: [u8; 4], hi: [u8; 4], lo: [u8; 4]) {
        self.fill_rect(x, y, w, h, fill);
        self.fill_rect(x, y, w, 2, hi);
        self.fill_rect(x, y, 2, h, hi);
        self.fill_rect(x, y + h - 2, w, 2, lo);
        self.fill_rect(x + w - 2, y, 2, h, lo);
    }

    pub fn draw_text(&mut self, x: i32, y: i32, text: &str, sx: i32, sy: i32, fg: [u8; 4], shadow: Option<[u8; 4]>) {
        let adv = 8 * sx;
        let mut cx = x;
        for ch in text.chars() {
            let bits = glyph_8x8(ch);
            if let Some(sh) = shadow {
                let ox = sx.max(1);
                let oy = sy.max(1);
                for (row, &mask) in bits.iter().enumerate() {
                    if mask == 0 {
                        continue;
                    }
                    for col in 0..8 {
                        if (mask & (0x80 >> col)) != 0 {
                            self.fill_rect(cx + col * sx + ox, y + row as i32 * sy + oy, sx, sy, sh);
                        }
                    }
                }
            }
            for (row, &mask) in bits.iter().enumerate() {
                if mask == 0 {
                    continue;
                }
                for col in 0..8 {
                    if (mask & (0x80 >> col)) != 0 {
                        self.fill_rect(cx + col * sx, y + row as i32 * sy, sx, sy, fg);
                    }
                }
            }
            cx += adv;
        }
    }

    /// Draws text with a crisp 1-cell dark outline (classic 90s TV OSD style).
    pub fn draw_text_outlined(&mut self, x: i32, y: i32, text: &str, sx: i32, sy: i32, fg: [u8; 4], outline: [u8; 4]) {
        let ox = sx.max(1);
        let oy = sy.max(1);
        for &(dx, dy) in &[(-ox, 0), (ox, 0), (0, -oy), (0, oy), (ox, oy)] {
            self.draw_text(x + dx, y + dy, text, sx, sy, outline, None);
        }
        self.draw_text(x, y, text, sx, sy, fg, None);
    }

    /// Draws text clipped to `max_w` pixels (truncating with `..` if needed).
    pub fn draw_text_clipped(
        &mut self,
        x: i32,
        y: i32,
        text: &str,
        max_w: i32,
        sx: i32,
        sy: i32,
        fg: [u8; 4],
        shadow: Option<[u8; 4]>,
    ) {
        let max_chars = (max_w / (8 * sx).max(1)).max(0) as usize;
        if max_chars == 0 {
            return;
        }
        let count = text.chars().count();
        if count <= max_chars {
            self.draw_text(x, y, text, sx, sy, fg, shadow);
        } else if max_chars <= 2 {
            let s: String = text.chars().take(max_chars).collect();
            self.draw_text(x, y, &s, sx, sy, fg, shadow);
        } else {
            let mut s: String = text.chars().take(max_chars - 2).collect();
            s.push_str("..");
            self.draw_text(x, y, &s, sx, sy, fg, shadow);
        }
    }

    /// Bilinear/nearest scaled blit of an RGBA8 image into a destination rect (preserving aspect or filling).
    pub fn blit_rgba(&mut self, src: &[u8], sw: usize, sh: usize, dx: i32, dy: i32, dw: i32, dh: i32) {
        if sw == 0 || sh == 0 || dw <= 0 || dh <= 0 || src.len() < sw * sh * 4 {
            return;
        }
        for py in 0..dh {
            let ty = dy + py;
            if ty < 0 || ty >= self.h as i32 {
                continue;
            }
            let sy = (py as usize * sh) / dh as usize;
            for px in 0..dw {
                let tx = dx + px;
                if tx < 0 || tx >= self.w as i32 {
                    continue;
                }
                let sx = (px as usize * sw) / dw as usize;
                let si = (sy * sw + sx) * 4;
                let di = (ty as usize * self.w + tx as usize) * 4;
                self.rgba[di] = src[si];
                self.rgba[di + 1] = src[si + 1];
                self.rgba[di + 2] = src[si + 2];
                self.rgba[di + 3] = 255;
            }
        }
    }
}

// ------------------------------------------------------------------ TV OSD State

const OSD_TIMEOUT_SECS: f32 = 3.6;

/// Classic 90s green/white CRT TV On-Screen Display controller.
pub struct TvOsd {
    channel_num: Option<String>,
    callsign: String,
    subtitle: String,
    audio_badge: String,
    channel_until: Option<Instant>,

    digit_buf: String,
    digit_at: Option<Instant>,

    volume: f32,
    muted: bool,
    volume_until: Option<Instant>,

    action_text: String,
    action_until: Option<Instant>,

    progress_label: String,
    progress_pos: f64,
    progress_dur: f64,
    progress_until: Option<Instant>,

    canvas: Canvas,
    version: u64,
    cached: Option<(u64, Arc<Vec<u8>>)>,
    dirty: bool,
    was_visible: bool,
}

impl Default for TvOsd {
    fn default() -> Self {
        Self::new()
    }
}

impl TvOsd {
    pub fn new() -> Self {
        Self {
            channel_num: None,
            callsign: String::new(),
            subtitle: String::new(),
            audio_badge: String::new(),
            channel_until: None,
            digit_buf: String::new(),
            digit_at: None,
            volume: 0.5,
            muted: false,
            volume_until: None,
            action_text: String::new(),
            action_until: None,
            progress_label: String::new(),
            progress_pos: 0.0,
            progress_dur: 0.0,
            progress_until: None,
            canvas: Canvas::new(OSD_W, OSD_H),
            version: 0,
            cached: None,
            dirty: false,
            was_visible: false,
        }
    }

    pub fn show_channel(&mut self, number: u16, callsign: &str, subtitle: &str, mono: bool) {
        self.channel_num = Some(format!("CH {:02}", number));
        self.callsign = callsign.to_string();
        self.subtitle = subtitle.to_string();
        self.audio_badge = if mono { "MONO".into() } else { "STEREO".into() };
        self.channel_until = Some(Instant::now() + std::time::Duration::from_secs_f32(OSD_TIMEOUT_SECS));
        self.digit_buf.clear();
        self.digit_at = None;
        self.dirty = true;
    }

    pub fn show_volume(&mut self, volume: f32, muted: bool) {
        self.volume = volume.clamp(0.0, 1.0);
        self.muted = muted;
        self.volume_until = Some(Instant::now() + std::time::Duration::from_secs_f32(2.6));
        self.dirty = true;
    }

    pub fn show_action(&mut self, text: &str) {
        self.action_text = text.to_string();
        self.action_until = Some(Instant::now() + std::time::Duration::from_secs_f32(2.4));
        self.dirty = true;
    }

    /// Shows the classic 90s green VCR/tape transport progress bar & timecode at bottom-center.
    pub fn show_progress(&mut self, label: &str, pos: f64, dur: f64) {
        let pos_c = pos.max(0.0);
        let dur_c = dur.max(0.0);
        let changed = self.progress_label != label
            || (self.progress_pos - pos_c).abs() >= 0.25
            || (self.progress_dur - dur_c).abs() >= 0.5
            || self.progress_until.is_none();
        self.progress_label = label.to_string();
        self.progress_pos = pos_c;
        self.progress_dur = dur_c;
        self.progress_until = Some(Instant::now() + std::time::Duration::from_secs_f32(2.6));
        if changed {
            self.dirty = true;
        }
    }

    /// Appends a remote digit (`0`..=`9`). Returns `Some(ch)` immediately once 3 digits are entered.
    pub fn push_digit(&mut self, digit: char) -> Option<u16> {
        if !digit.is_ascii_digit() {
            return None;
        }
        if self.digit_buf.len() >= 3 {
            self.digit_buf.clear();
        }
        self.digit_buf.push(digit);
        self.digit_at = Some(Instant::now());
        self.dirty = true;
        if self.digit_buf.len() >= 3 {
            return self.commit_digits();
        }
        None
    }

    /// Commits pending digits if ready (either on Enter or after 1.1s pause).
    pub fn poll_digit_commit(&mut self) -> Option<u16> {
        let t = self.digit_at?;
        if t.elapsed().as_secs_f32() >= 1.1 {
            self.commit_digits()
        } else {
            None
        }
    }

    pub fn commit_digits(&mut self) -> Option<u16> {
        if self.digit_buf.is_empty() {
            return None;
        }
        let parsed = self.digit_buf.parse::<u16>().ok();
        self.digit_buf.clear();
        self.digit_at = None;
        self.dirty = true;
        parsed
    }

    pub fn has_pending_digits(&self) -> bool {
        !self.digit_buf.is_empty()
    }

    /// Updates timers and returns `Some((version, rgba_640x480))` when OSD is active.
    pub fn render(&mut self) -> Option<(u64, Arc<Vec<u8>>)> {
        let now = Instant::now();
        let show_digits = !self.digit_buf.is_empty();
        let show_ch = self.channel_until.is_some_and(|u| now < u);
        let show_vol = self.volume_until.is_some_and(|u| now < u);
        let show_act = self.action_until.is_some_and(|u| now < u);
        let show_prog = self.progress_until.is_some_and(|u| now < u);
        let show_mute_persist = self.muted;
        let visible = show_digits || show_ch || show_vol || show_act || show_prog || show_mute_persist;

        if !visible {
            if self.was_visible {
                self.was_visible = false;
                self.cached = None;
            }
            return None;
        }

        if !self.was_visible {
            self.dirty = true;
            self.was_visible = true;
        }

        // Re-render whenever any element transitions off
        if !show_ch && self.channel_until.is_some() {
            self.channel_until = None;
            self.dirty = true;
        }
        if !show_vol && self.volume_until.is_some() {
            self.volume_until = None;
            self.dirty = true;
        }
        if !show_act && self.action_until.is_some() {
            self.action_until = None;
            self.dirty = true;
        }
        if !show_prog && self.progress_until.is_some() {
            self.progress_until = None;
            self.dirty = true;
        }

        if !self.dirty {
            return self.cached.clone();
        }
        self.dirty = false;

        self.canvas.clear([0, 0, 0, 0]);

        let green = [54, 255, 86, 255];
        let white = [240, 246, 232, 255];
        let dim_green = [28, 128, 44, 230];
        let outline = [6, 18, 8, 225];

        // ---- Top-right: Digit entry OR Channel number + callsign + STEREO/MONO
        if show_digits {
            let padded = match self.digit_buf.len() {
                1 => format!("CH {}--", self.digit_buf),
                2 => format!("CH {}-", self.digit_buf),
                _ => format!("CH {}", self.digit_buf),
            };
            let w = padded.len() as i32 * 24;
            self.canvas.draw_text_outlined(OSD_W as i32 - 44 - w, 34, &padded, 3, 3, green, outline);
        } else if show_ch {
            if let Some(ch) = &self.channel_num {
                let w = ch.len() as i32 * 24;
                self.canvas.draw_text_outlined(OSD_W as i32 - 44 - w, 34, ch, 3, 3, green, outline);
            }
            if !self.callsign.is_empty() {
                let label: String = self.callsign.chars().take(18).collect();
                let w = label.len() as i32 * 16;
                self.canvas.draw_text_outlined(OSD_W as i32 - 44 - w, 66, &label, 2, 2, white, outline);
            }
            if !self.audio_badge.is_empty() {
                let w = self.audio_badge.len() as i32 * 8;
                self.canvas.draw_text_outlined(OSD_W as i32 - 44 - w, 90, &self.audio_badge, 1, 2, green, outline);
            }
        }

        // ---- Top-left: MUTE badge or brief action text (e.g. RESTART 0:00)
        if show_act && !self.action_text.is_empty() {
            self.canvas.draw_text_outlined(44, 38, &self.action_text, 2, 2, green, outline);
        } else if self.muted {
            self.canvas.draw_text_outlined(44, 38, "MUTE", 2, 3, green, outline);
        }

        // ---- Bottom-left / bottom-center: 1-line show/episode banner
        if show_ch && !self.subtitle.is_empty() && !show_vol && !show_prog {
            let max_chars = 34usize;
            let txt: String = if self.subtitle.chars().count() > max_chars {
                let mut s: String = self.subtitle.chars().take(max_chars - 2).collect();
                s.push_str("..");
                s
            } else {
                self.subtitle.clone()
            };
            self.canvas.fill_rect(36, 404, OSD_W as i32 - 72, 30, [4, 14, 6, 170]);
            self.canvas.draw_text_outlined(48, 412, &txt, 2, 2, white, outline);
        }

        // ---- Bottom-center: Classic 90s green VCR / tape progress bar & timecode
        if show_prog && !show_vol {
            let bx = 56;
            let by = 382;
            let bw = OSD_W as i32 - 112; // 528 px
            self.canvas.fill_rect(bx, by, bw, 58, [4, 14, 6, 192]);
            self.canvas.stroke_rect(bx, by, bw, 58, 1, [34, 128, 52, 165]);

            // Top line: transport label on left, HH:MM:SS / HH:MM:SS timecode on right
            let lbl: String = self.progress_label.chars().take(16).collect();
            self.canvas.draw_text_outlined(bx + 14, by + 8, &lbl, 2, 2, green, outline);

            let fmt_tc = |secs: f64| -> String {
                let s = secs.max(0.0).round() as u64;
                let h = s / 3600;
                let m = (s % 3600) / 60;
                let sec = s % 60;
                if h > 0 {
                    format!("{h}:{m:02}:{sec:02}")
                } else {
                    format!("{m:02}:{sec:02}")
                }
            };
            let tc = if self.progress_dur > 0.5 {
                format!("{}/{}", fmt_tc(self.progress_pos), fmt_tc(self.progress_dur))
            } else {
                fmt_tc(self.progress_pos)
            };
            let tc_w = tc.len() as i32 * 16;
            self.canvas.draw_text_outlined(bx + bw - 14 - tc_w, by + 8, &tc, 2, 2, white, outline);

            // Bottom line: 32 crisp segmented green tape-position blocks with bright head cursor
            let segs = 32;
            let frac = if self.progress_dur > 0.5 {
                (self.progress_pos / self.progress_dur).clamp(0.0, 1.0)
            } else {
                0.0
            };
            let filled = (frac * segs as f64).round() as i32;
            let bar_x0 = bx + 14;
            let bar_y = by + 32;
            let step = (bw - 28) / segs; // 500 / 32 = 15 px
            for s in 0..segs {
                let sx = bar_x0 + s * step;
                let is_head = s == filled.clamp(0, segs - 1) && self.progress_dur > 0.5;
                let col = if is_head {
                    white
                } else if s < filled {
                    green
                } else {
                    dim_green
                };
                self.canvas.fill_rect(sx - 1, bar_y - 1, step - 3, 18, outline);
                self.canvas.fill_rect(sx, bar_y, step - 5, 16, col);
            }
        }

        // ---- Bottom-center: Classic 90s green segmented VOLUME bar
        if show_vol {
            let bx = 110;
            let by = 402;
            self.canvas.fill_rect(bx - 12, by - 10, 444, 44, [4, 14, 6, 185]);
            let label = if self.muted { "MUTE" } else { "VOL " };
            self.canvas.draw_text_outlined(bx, by + 4, label, 2, 2, green, outline);
            let segs = 20;
            let filled = if self.muted { 0 } else { (self.volume * segs as f32).round() as i32 };
            for s in 0..segs {
                let sx = bx + 76 + s * 15;
                let col = if s < filled { green } else { dim_green };
                self.canvas.fill_rect(sx - 1, by + 1, 11, 22, outline);
                self.canvas.fill_rect(sx, by + 2, 9, 20, col);
            }
            let pct = format!("{:>3}", if self.muted { 0 } else { (self.volume * 100.0).round() as i32 });
            self.canvas.draw_text_outlined(bx + 76 + segs * 15 + 8, by + 4, &pct, 2, 2, white, outline);
        }

        self.version += 1;
        let arc = Arc::new(self.canvas.rgba.clone());
        self.cached = Some((self.version, arc.clone()));
        Some((self.version, arc))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn osd_renders_channel_and_digits() {
        let mut osd = TvOsd::new();
        assert!(osd.render().is_none());
        osd.show_channel(4, "SEINFELD", "S04E11 - THE CONTEST", false);
        let (v1, buf) = osd.render().expect("channel OSD should render");
        assert_eq!(buf.len(), OSD_W * OSD_H * 4);
        assert!(buf.chunks_exact(4).any(|p| p[1] > 200 && p[3] == 255), "green OSD pixels expected");
        // Calling again without changes reuses version
        let (v2, _) = osd.render().unwrap();
        assert_eq!(v1, v2);
        // Typing 3 digits commits immediately
        assert_eq!(osd.push_digit('0'), None);
        assert_eq!(osd.push_digit('5'), None);
        assert_eq!(osd.push_digit('2'), Some(52));

        // Progress bar renders bottom-center timecode & segmented green blocks
        osd.show_progress("FF >>> 16X", 125.0, 600.0);
        let (_, pbuf) = osd.render().expect("progress bar OSD should render");
        // Check white head cursor & green pixels inside the bottom bar region (y=382..440)
        let bar_slice = &pbuf[(382 * OSD_W * 4)..(440 * OSD_W * 4)];
        assert!(bar_slice.chunks_exact(4).any(|p| p[1] > 200 && p[3] == 255));
    }
}
