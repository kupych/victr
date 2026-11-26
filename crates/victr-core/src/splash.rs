//! Startup CRT splash screen:
//! 1. Untuned snowy RF static coming up as the cold picture tube warms in (~1.5 s)
//! 2. Crisp snap into a classic 1990s cobalt blue screen with white
//!    "ViCTR - Virtual CRT Television Receiver" text (~2.4 s, or until Jellyfin connects)
//! 3. Clean handoff into the user's configured startup destination (Cable Guide,
//!    Last Watched, or default test pattern).
//!
//! Structured as a self-contained state machine so a custom boot animation can be
//! plugged in later without touching the rest of the app.

use crate::osd::{Canvas, glyph_8x8};
use std::sync::Arc;

pub const SPLASH_W: usize = 640;
pub const SPLASH_H: usize = 480;

const STATIC_SECS: f32 = 1.55;
const BLUE_MIN_SECS: f32 = 2.40;
const BLUE_MAX_WAIT_SECS: f32 = 5.20;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum SplashPhase {
    Static,
    BlueScreen,
    Done,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum SplashEvent {
    None,
    SnappedToBlue,
    Finished,
}

pub struct SplashState {
    phase: SplashPhase,
    phase_t: f32,
    rng: u64,
    canvas: Canvas,
    frame_counter: u64,
}

impl SplashState {
    pub fn new(enabled: bool) -> Self {
        Self {
            phase: if enabled { SplashPhase::Static } else { SplashPhase::Done },
            phase_t: 0.0,
            rng: 0x51a5_4c0d_e91f_3a7b,
            canvas: Canvas::new(SPLASH_W, SPLASH_H),
            frame_counter: 0,
        }
    }

    pub fn is_active(&self) -> bool {
        self.phase != SplashPhase::Done
    }

    pub fn is_static(&self) -> bool {
        self.phase == SplashPhase::Static
    }

    #[cfg(test)]
    pub fn phase(&self) -> SplashPhase {
        self.phase
    }

    /// Immediately ends the splash screen (e.g. user pressed a key or clicked).
    pub fn skip(&mut self) -> bool {
        if self.phase != SplashPhase::Done {
            self.phase = SplashPhase::Done;
            true
        } else {
            false
        }
    }

    /// Advances the splash timeline. Pass `waiting_for_jellyfin = true` if the
    /// configured destination needs Jellyfin and it is still connecting.
    pub fn step(&mut self, dt: f32, waiting_for_jellyfin: bool) -> SplashEvent {
        let dt = dt.clamp(0.0, 0.1);
        match self.phase {
            SplashPhase::Static => {
                self.phase_t += dt;
                if self.phase_t >= STATIC_SECS {
                    self.phase = SplashPhase::BlueScreen;
                    self.phase_t = 0.0;
                    return SplashEvent::SnappedToBlue;
                }
                SplashEvent::None
            }
            SplashPhase::BlueScreen => {
                self.phase_t += dt;
                let ready = self.phase_t >= BLUE_MIN_SECS
                    && (!waiting_for_jellyfin || self.phase_t >= BLUE_MAX_WAIT_SECS);
                if ready {
                    self.phase = SplashPhase::Done;
                    return SplashEvent::Finished;
                }
                SplashEvent::None
            }
            SplashPhase::Done => SplashEvent::None,
        }
    }

    #[inline]
    fn next_u32(&mut self) -> u32 {
        self.rng ^= self.rng << 13;
        self.rng ^= self.rng >> 7;
        self.rng ^= self.rng << 17;
        (self.rng >> 32) as u32
    }

    /// Renders the current splash frame as a 640x480 RGBA image fed straight into
    /// the CRT's NTSC signal path.
    pub fn render(&mut self, status_hint: Option<&str>) -> (u64, Arc<(usize, usize, Vec<u8>)>) {
        match self.phase {
            SplashPhase::Static => {
                // Monochrome RF snow with slight horizontal correlation like analog VHF static
                let mut prev = 128i32;
                for y in 0..SPLASH_H {
                    // Subtle rolling horizontal noise streak per scanline
                    let line_bias = ((self.next_u32() & 31) as i32) - 16;
                    for x in 0..SPLASH_W {
                        let r = (self.next_u32() & 255) as i32;
                        let v = ((r * 3 + prev + line_bias) >> 2).clamp(12, 245) as u8;
                        prev = v as i32;
                        let idx = (y * SPLASH_W + x) * 4;
                        self.canvas.rgba[idx] = v;
                        self.canvas.rgba[idx + 1] = v;
                        self.canvas.rgba[idx + 2] = v;
                        self.canvas.rgba[idx + 3] = 255;
                    }
                }
            }
            SplashPhase::BlueScreen | SplashPhase::Done => {
                // Classic 1990s CRT/VCR "no signal" cobalt blue screen
                self.canvas.clear([18, 42, 168, 255]);

                let white = [246, 248, 255, 255];
                let soft_white = [204, 222, 255, 255];
                let shadow = Some([8, 18, 86, 255]);

                // Classic top-left input tag (title-safe inset)
                self.canvas.draw_text(44, 34, "VIDEO 1", 2, 2, white, shadow);

                // Tiny retro CRT icon centered above the title
                let ic_w = 56;
                let ic_h = 40;
                let ic_x = (SPLASH_W as i32 - ic_w) / 2;
                let ic_y = 142;
                self.canvas.stroke_rect(ic_x, ic_y, ic_w, ic_h, 3, white);
                self.canvas.fill_rect(ic_x + 7, ic_y + 7, ic_w - 14, ic_h - 14, [30, 68, 204, 255]);
                self.canvas.stroke_rect(ic_x + 7, ic_y + 7, ic_w - 14, ic_h - 14, 2, soft_white);
                // Small CRT feet
                self.canvas.fill_rect(ic_x + 10, ic_y + ic_h, 8, 4, white);
                self.canvas.fill_rect(ic_x + ic_w - 18, ic_y + ic_h, 8, 4, white);

                // Centered iconic ViCTR wordmark (with crisp lowercase 'i')
                draw_victr_wordmark(&mut self.canvas, 200, 4, 4, white, shadow);

                // Clean horizontal accent divider
                self.canvas.fill_rect(120, 248, SPLASH_W as i32 - 240, 2, soft_white);

                // Requested white subtitle line: "ViCTR - Virtual CRT Television Receiver"
                let line = "ViCTR - Virtual CRT Television Receiver";
                let char_w = 12; // 1.5x crisp pixel width -> 39 * 12 = 468px (well inside 576px title-safe)
                let line_w = line.chars().count() as i32 * char_w;
                let lx = (SPLASH_W as i32 - line_w) / 2;
                draw_text_mixed(&mut self.canvas, lx, 268, line, 1, 2, char_w, white, shadow);

                // Subtle bottom-right status hint if waiting for Jellyfin or showing destination
                if let Some(hint) = status_hint {
                    let hw = hint.chars().count() as i32 * 8;
                    self.canvas.draw_text(
                        SPLASH_W as i32 - 44 - hw,
                        SPLASH_H as i32 - 48,
                        hint,
                        1,
                        2,
                        soft_white,
                        shadow,
                    );
                }
            }
        }

        self.frame_counter += 1;
        // Distinct high-bit namespace so splash frame IDs never collide with guide/video/image IDs
        let id = 0x4000_0000_0000_0000 | self.frame_counter;
        (id, Arc::new((SPLASH_W, SPLASH_H, self.canvas.rgba.clone())))
    }
}

/// Crisp lowercase bitmap glyphs for authentic mixed-case 90s OSD / splash text.
fn glyph_mixed_8x8(ch: char) -> [u8; 8] {
    match ch {
        'a' => [0x00, 0x00, 0x3C, 0x06, 0x3E, 0x66, 0x3E, 0x00],
        'c' => [0x00, 0x00, 0x3C, 0x60, 0x60, 0x66, 0x3C, 0x00],
        'e' => [0x00, 0x00, 0x3C, 0x66, 0x7E, 0x60, 0x3C, 0x00],
        'i' => [0x18, 0x00, 0x38, 0x18, 0x18, 0x18, 0x3C, 0x00],
        'l' => [0x38, 0x18, 0x18, 0x18, 0x18, 0x18, 0x3C, 0x00],
        'n' => [0x00, 0x00, 0x7C, 0x66, 0x66, 0x66, 0x66, 0x00],
        'o' => [0x00, 0x00, 0x3C, 0x66, 0x66, 0x66, 0x3C, 0x00],
        'r' => [0x00, 0x00, 0x6C, 0x76, 0x60, 0x60, 0x60, 0x00],
        's' => [0x00, 0x00, 0x3E, 0x60, 0x3C, 0x06, 0x7C, 0x00],
        't' => [0x18, 0x18, 0x7E, 0x18, 0x18, 0x1A, 0x0C, 0x00],
        'u' => [0x00, 0x00, 0x66, 0x66, 0x66, 0x66, 0x3E, 0x00],
        'v' => [0x00, 0x00, 0x66, 0x66, 0x66, 0x3C, 0x18, 0x00],
        _ => glyph_8x8(ch),
    }
}

fn draw_victr_wordmark(canvas: &mut Canvas, y: i32, sx: i32, sy: i32, fg: [u8; 4], shadow: Option<[u8; 4]>) {
    let text = "ViCTR";
    let adv = 9 * sx;
    let total_w = text.chars().count() as i32 * adv - sx;
    let x = (canvas.w as i32 - total_w) / 2;
    draw_text_mixed(canvas, x, y, text, sx, sy, adv, fg, shadow);
}

fn draw_text_mixed(
    canvas: &mut Canvas,
    x: i32,
    y: i32,
    text: &str,
    sx: i32,
    sy: i32,
    adv: i32,
    fg: [u8; 4],
    shadow: Option<[u8; 4]>,
) {
    let mut cx = x;
    for ch in text.chars() {
        let bits = glyph_mixed_8x8(ch);
        if let Some(sh) = shadow {
            let ox = sx.max(1);
            let oy = sy.max(1);
            for (row, &mask) in bits.iter().enumerate() {
                if mask == 0 {
                    continue;
                }
                for col in 0..8 {
                    if (mask & (0x80 >> col)) != 0 {
                        canvas.fill_rect(cx + col * sx + ox, y + row as i32 * sy + oy, sx, sy, sh);
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
                    canvas.fill_rect(cx + col * sx, y + row as i32 * sy, sx, sy, fg);
                }
            }
        }
        cx += adv;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splash_transitions_static_to_blue_to_done() {
        let mut s = SplashState::new(true);
        assert_eq!(s.phase(), SplashPhase::Static);
        assert!(s.is_static());

        let (_, static_img) = s.render(None);
        assert_eq!(static_img.0, SPLASH_W);
        assert_eq!(static_img.1, SPLASH_H);

        // Advance past static duration
        let mut snapped = false;
        for _ in 0..20 {
            if s.step(0.1, false) == SplashEvent::SnappedToBlue {
                snapped = true;
            }
        }
        assert!(snapped);
        assert_eq!(s.phase(), SplashPhase::BlueScreen);
        assert!(!s.is_static());

        // Blue screen corner should be classic cobalt blue
        let (_, blue_img) = s.render(Some("LOADING GUIDE..."));
        assert_eq!(&blue_img.2[0..4], &[18, 42, 168, 255]);

        // Advance past blue hold duration -> Finished
        let mut finished = false;
        for _ in 0..30 {
            if s.step(0.1, false) == SplashEvent::Finished {
                finished = true;
            }
        }
        assert!(finished);
        assert!(!s.is_active());
    }
}
