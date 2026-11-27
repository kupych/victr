//! Classic 1980s/90s Ceefax / World System Teletext (WST) data-channel renderer (640x480).
//!
//! Used for ViCTR's live stream tuning/buffering splash, off-air / stream-inactive
//! diagnostics, and future Teletext data pages. Rendered at 640x480 inside SMPTE
//! title-safe margins using the authentic 8-color RGBYCMW Teletext palette so it
//! passes through real NTSC chroma bleed, scanlines, shadow mask, and phosphor bloom.

use crate::osd::Canvas;
use chrono::Local;
use std::sync::Arc;

pub const TT_W: usize = 640;
pub const TT_H: usize = 480;

const SAFE_X: i32 = 32;
const SAFE_Y: i32 = 24;
const SAFE_W: i32 = TT_W as i32 - SAFE_X * 2; // 576px

// Authentic 3-bit RGB World System Teletext (Ceefax / ORACLE) 8-color palette
const BLACK: [u8; 4] = [6, 6, 12, 255];
const RED: [u8; 4] = [232, 34, 34, 255];
const GREEN: [u8; 4] = [38, 228, 68, 255];
const YELLOW: [u8; 4] = [248, 232, 44, 255];
const BLUE: [u8; 4] = [24, 58, 208, 255];
const MAGENTA: [u8; 4] = [218, 46, 212, 255];
const CYAN: [u8; 4] = [44, 224, 238, 255];
const WHITE: [u8; 4] = [242, 244, 248, 255];
const DIM_BLUE: [u8; 4] = [14, 28, 92, 255];
const DIM_RED: [u8; 4] = [96, 14, 18, 255];

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum TeletextMode {
    /// Actively connecting / resolving / buffering a channel or live stream.
    Loading,
    /// Stream failed, went offline, or timed out with no incoming video frames.
    Inactive,
}

#[derive(Clone, Debug)]
pub struct TeletextInfo<'a> {
    pub mode: TeletextMode,
    pub channel_number: Option<u16>,
    pub callsign: &'a str,
    pub title: &'a str,
    pub subtitle: &'a str,
    pub source_kind: &'a str,
    pub source_host: &'a str,
    pub status_line: &'a str,
    pub elapsed_secs: f32,
}

pub struct TeletextRenderer {
    canvas: Canvas,
    frame_counter: u64,
}

impl Default for TeletextRenderer {
    fn default() -> Self {
        Self::new()
    }
}

impl TeletextRenderer {
    pub fn new() -> Self {
        Self {
            canvas: Canvas::new(TT_W, TT_H),
            frame_counter: 0,
        }
    }

    /// Renders a 640x480 authentic Ceefax/Teletext status frame.
    pub fn render(&mut self, info: &TeletextInfo<'_>) -> (u64, Arc<(usize, usize, Vec<u8>)>) {
        self.frame_counter = self.frame_counter.wrapping_add(1);
        let tick = self.frame_counter;
        let now = Local::now();

        // Deep Teletext black background (full-bleed behind overscan)
        self.canvas.fill_rect(0, 0, TT_W as i32, TT_H as i32, BLACK);

        // ---- Row 0: Classic Teletext Header Bar (`P100  ViCTR-TEXT  Thu 26 Nov 22:16:04`)
        let hdr_y = SAFE_Y;
        let (page_str, page_col) = match info.mode {
            TeletextMode::Loading => {
                // Classic Ceefax rolling page-hunt counter while acquiring lock
                let roll = 100 + ((tick / 4) % 99);
                (format!("P{roll:03}"), GREEN)
            }
            TeletextMode::Inactive => ("P888".to_string(), RED),
        };
        self.canvas.draw_text(SAFE_X, hdr_y, &page_str, 1, 2, page_col, None);
        self.canvas.draw_text(SAFE_X + 46, hdr_y, "VICTR-TEXT", 1, 2, WHITE, None);
        if let Some(ch) = info.channel_number.filter(|&n| n > 0) {
            let cs = if info.callsign.is_empty() {
                format!("CH {:02}", ch)
            } else {
                format!("CH {:02} {}", ch, info.callsign)
            };
            self.canvas.draw_text_clipped(SAFE_X + 144, hdr_y, &cs, 160, 1, 2, CYAN, None);
        }
        let clock = now.format("%a %d %b %H:%M:%S").to_string().to_ascii_uppercase();
        let clock_w = clock.len() as i32 * 8;
        self.canvas
            .draw_text(SAFE_X + SAFE_W - clock_w, hdr_y, &clock, 1, 2, YELLOW, None);

        // ---- Rows 1-3: Double-Height Teletext Masthead Banner with Mosaic Blocks
        let ban_y = SAFE_Y + 24;
        let ban_h = 68;
        let ban_bg = match info.mode {
            TeletextMode::Loading => BLUE,
            TeletextMode::Inactive => DIM_RED,
        };
        self.canvas.fill_rect(SAFE_X, ban_y, SAFE_W, ban_h, ban_bg);
        // Classic 2x3 Teletext mosaic pixel blocks on the left of the masthead
        let mosaic_colors = match info.mode {
            TeletextMode::Loading => [YELLOW, CYAN, GREEN, CYAN, MAGENTA, YELLOW],
            TeletextMode::Inactive => [RED, YELLOW, RED, YELLOW, RED, YELLOW],
        };
        for r in 0..3 {
            for c in 0..2 {
                let col = mosaic_colors[r * 2 + c];
                self.canvas.fill_rect(
                    SAFE_X + 10 + c as i32 * 16,
                    ban_y + 10 + r as i32 * 16,
                    12,
                    12,
                    col,
                );
            }
        }
        let title_x = SAFE_X + 52;
        let title_w = SAFE_W - 64;
        let title_txt = if info.title.trim().is_empty() {
            "DATA CHANNEL"
        } else {
            info.title.trim()
        };
        // Double-height Teletext headline (scale 2x3 = tall iconic Teletext font!)
        self.canvas
            .draw_text_clipped(title_x, ban_y + 8, title_txt, title_w, 2, 3, YELLOW, None);
        if !info.subtitle.trim().is_empty() {
            self.canvas.draw_text_clipped(
                title_x,
                ban_y + 44,
                info.subtitle.trim(),
                title_w,
                1,
                2,
                WHITE,
                None,
            );
        }

        // ---- Mosaic Divider Strip
        let sep_y = ban_y + ban_h + 8;
        let seg_colors = match info.mode {
            TeletextMode::Loading => [CYAN, BLUE, YELLOW, BLUE],
            TeletextMode::Inactive => [RED, YELLOW, RED, MAGENTA],
        };
        for i in 0..36 {
            let col = seg_colors[i % seg_colors.len()];
            self.canvas.fill_rect(SAFE_X + i as i32 * 16, sep_y, 12, 6, col);
        }

        // ---- Main Status Panel (Loading vs Stream Inactive)
        let box_y = sep_y + 18;
        let box_h = 152;
        let (panel_bg, accent, head_txt) = match info.mode {
            TeletextMode::Loading => (DIM_BLUE, CYAN, "TUNING / BUFFERING STREAM"),
            TeletextMode::Inactive => (DIM_RED, RED, "! STREAM INACTIVE / OFF AIR !"),
        };
        self.canvas.fill_rect(SAFE_X, box_y, SAFE_W, box_h, panel_bg);
        self.canvas.fill_rect(SAFE_X, box_y, SAFE_W, 34, accent);

        let blink = (tick / 20).is_multiple_of(2);
        let head_fg = match info.mode {
            TeletextMode::Loading => BLACK,
            TeletextMode::Inactive => if blink { YELLOW } else { WHITE },
        };
        let head_w = (head_txt.len() as i32 * 16).min(SAFE_W - 16);
        let head_x = SAFE_X + ((SAFE_W - head_w) / 2).max(8);
        self.canvas
            .draw_text_clipped(head_x, box_y + 7, head_txt, SAFE_W - 16, 2, 2, head_fg, None);

        // Animated Teletext block scanner bar (Loading) OR bold Red/Yellow warning stripe (Inactive)
        let bar_y = box_y + 46;
        let cells = 28i32;
        let cell_w = 16i32;
        let bar_x = SAFE_X + (SAFE_W - cells * cell_w) / 2;
        match info.mode {
            TeletextMode::Loading => {
                // Smooth Knight-Rider / Ceefax carrier-acquisition sweep
                let span = (cells - 5).max(1) as u64;
                let phase = (tick / 3) % (span * 2);
                let pos = if phase < span { phase } else { span * 2 - phase } as i32;
                for c in 0..cells {
                    let dist = (c - (pos + 2)).abs();
                    let col = if dist == 0 {
                        YELLOW
                    } else if dist == 1 {
                        CYAN
                    } else if dist == 2 {
                        GREEN
                    } else {
                        [18, 42, 112, 255]
                    };
                    self.canvas.fill_rect(bar_x + c * cell_w, bar_y, 12, 12, col);
                }
            }
            TeletextMode::Inactive => {
                for c in 0..cells {
                    let col = if (c as u64 + tick / 24).is_multiple_of(2) {
                        RED
                    } else {
                        [72, 16, 20, 255]
                    };
                    self.canvas.fill_rect(bar_x + c * cell_w, bar_y, 12, 10, col);
                }
            }
        }

        // Live telemetry rows inside the status panel
        let info_y = bar_y + 24;
        let secs = info.elapsed_secs.max(0.0) as u32;
        let elapsed_str = format!("{:02}:{:02}", secs / 60, secs % 60);
        let status_label = match info.mode {
            TeletextMode::Loading => "STATUS :",
            TeletextMode::Inactive => "REASON :",
        };
        let status_val = if info.status_line.trim().is_empty() {
            match info.mode {
                TeletextMode::Loading => "ACQUIRING CARRIER SIGNAL...",
                TeletextMode::Inactive => "NO CARRIER OR STREAM OFFLINE",
            }
        } else {
            info.status_line.trim()
        };
        self.canvas.draw_text(SAFE_X + 14, info_y, status_label, 1, 2, YELLOW, None);
        self.canvas.draw_text_clipped(
            SAFE_X + 86,
            info_y,
            status_val,
            SAFE_W - 100,
            1,
            2,
            WHITE,
            None,
        );

        self.canvas.draw_text(SAFE_X + 14, info_y + 24, "SOURCE :", 1, 2, YELLOW, None);
        let src_line = if info.source_host.trim().is_empty() {
            info.source_kind.to_string()
        } else {
            format!("{}  ({})", info.source_kind, info.source_host.trim())
        };
        self.canvas.draw_text_clipped(
            SAFE_X + 86,
            info_y + 24,
            &src_line,
            SAFE_W - 100,
            1,
            2,
            CYAN,
            None,
        );

        let third_lbl = match info.mode {
            TeletextMode::Loading => "WAIT   :",
            TeletextMode::Inactive => "SIGNAL :",
        };
        let third_val = match info.mode {
            TeletextMode::Loading => format!("{elapsed_str}  (HOLDING FOR FIRST FRAME)"),
            TeletextMode::Inactive => format!("LOST AFTER {elapsed_str} · PRESS R TO RETRY"),
        };
        self.canvas.draw_text(SAFE_X + 14, info_y + 48, third_lbl, 1, 2, YELLOW, None);
        self.canvas.draw_text_clipped(
            SAFE_X + 86,
            info_y + 48,
            &third_val,
            SAFE_W - 100,
            1,
            2,
            if info.mode == TeletextMode::Loading { GREEN } else { YELLOW },
            None,
        );

        // ---- Teletext Page Directory (classic Ceefax dotted-leader index)
        let dir_y = box_y + box_h + 18;
        let rows = [
            ("100", "PREVUE CABLE GUIDE", "[G]", YELLOW),
            ("200", "EPISODES / GAME CATALOG", "[E]", CYAN),
            ("300", "RETRY / RE-TUNE CHANNEL", "[R]", GREEN),
            ("400", "CHANNEL UP / DOWN", "[ ] / [ [", WHITE),
        ];
        for (i, (pg, label, key, col)) in rows.iter().enumerate() {
            let y = dir_y + i as i32 * 22;
            self.canvas.draw_text(SAFE_X + 10, y, pg, 1, 2, YELLOW, None);
            self.canvas.draw_text(SAFE_X + 54, y, label, 1, 2, *col, None);
            // Dotted leader
            let dot_start = SAFE_X + 54 + label.len() as i32 * 8 + 8;
            let kw = key.len() as i32 * 8;
            let dot_end = SAFE_X + SAFE_W - kw - 18;
            let mut dx = dot_start;
            while dx < dot_end {
                self.canvas.draw_text(dx, y, ".", 1, 2, [96, 128, 176, 255], None);
                dx += 12;
            }
            self.canvas.draw_text(SAFE_X + SAFE_W - kw - 10, y, key, 1, 2, WHITE, None);
        }

        // ---- Bottom Iconic 4-Color Teletext Fastext Buttons (Red / Green / Yellow / Cyan)
        let fast_y = SAFE_Y + 404;
        let fast_h = 26;
        let fast_w = SAFE_W / 4;
        let buttons = [
            ("R:RETRY", RED, WHITE),
            ("G:GUIDE", GREEN, BLACK),
            ("E:LIST", YELLOW, BLACK),
            ("]:NEXT CH", CYAN, BLACK),
        ];
        for (i, (txt, bg, fg)) in buttons.iter().enumerate() {
            let bx = SAFE_X + i as i32 * fast_w;
            let bw = if i == 3 { SAFE_W - i as i32 * fast_w } else { fast_w - 4 };
            self.canvas.fill_rect(bx, fast_y, bw, fast_h, *bg);
            let tw = txt.len() as i32 * 8;
            self.canvas.draw_text(bx + (bw - tw) / 2, fast_y + 5, txt, 1, 2, *fg, None);
        }

        // Unique frame ID in separate high-bit namespace from Guide & video frames
        let id = 0x4000_0000_0000_0000 | self.frame_counter;
        (id, Arc::new((TT_W, TT_H, self.canvas.rgba.clone())))
    }
}

/// Extracts a concise hostname or filename label from a stream URL / file path for Teletext telemetry.
pub fn extract_source_host(src: &str) -> String {
    let s = src.trim();
    let stripped = s
        .strip_prefix("web://")
        .or_else(|| s.strip_prefix("retro://"))
        .or_else(|| s.strip_prefix("capture://"))
        .unwrap_or(s);
    if let Some(after_scheme) = stripped.split_once("://").map(|(_, rest)| rest) {
        let host = after_scheme
            .split(['/', '?', '#'])
            .next()
            .unwrap_or("")
            .trim_start_matches("www.");
        if !host.is_empty() {
            return host.to_string();
        }
    }
    std::path::Path::new(stripped)
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or(stripped)
        .chars()
        .take(32)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn teletext_renders_loading_and_inactive_frames() {
        let mut tt = TeletextRenderer::new();
        let (id1, f1) = tt.render(&TeletextInfo {
            mode: TeletextMode::Loading,
            channel_number: Some(20),
            callsign: "NASA",
            title: "NASA Live",
            subtitle: "ISS Earth View",
            source_kind: "LIVE STREAM",
            source_host: "youtube.com",
            status_line: "Connecting...",
            elapsed_secs: 2.5,
        });
        assert!(id1 > 0);
        assert_eq!(f1.0, TT_W);
        assert_eq!(f1.1, TT_H);
        assert_eq!(f1.2.len(), TT_W * TT_H * 4);

        let (id2, f2) = tt.render(&TeletextInfo {
            mode: TeletextMode::Inactive,
            channel_number: Some(20),
            callsign: "NASA",
            title: "NASA Live",
            subtitle: "",
            source_kind: "LIVE STREAM",
            source_host: "youtube.com",
            status_line: "Stream offline or unreachable",
            elapsed_secs: 18.0,
        });
        assert!(id2 > id1);
        assert_ne!(f1.2, f2.2);
        assert_eq!(extract_source_host("web://https://www.puzzlescript.net/play.html?p=123"), "puzzlescript.net");
        assert_eq!(extract_source_host("https://www.youtube.com/@NASA/live"), "youtube.com");
    }
}
