//! Authentic 1980s World System Teletext (WST / BBC Ceefax / SAA5050) 40x24 cell renderer.
//!
//! Strictly locked to a rigid 40-column x 24-row character/mosaic grid inside SMPTE
//! title-safe margins using ONLY the raw 8-color 3-bit RGB Teletext palette (no mid-tones,
//! no gradients, no modern UI panels) with authentic 2x3 separated sixel mosaic blocks and
//! double-height Teletext headlines.

use crate::osd::Canvas;
use chrono::Local;
use std::sync::Arc;

pub const TT_W: usize = 640;
pub const TT_H: usize = 480;

// Rigid 40x24 Teletext character cell grid (40*14 = 560px wide, 24*18 = 432px tall)
const COLS: i32 = 40;
const CELL_W: i32 = 14;
const CELL_H: i32 = 18;
const ORIGIN_X: i32 = (TT_W as i32 - COLS * CELL_W) / 2; // 40px
const ORIGIN_Y: i32 = (TT_H as i32 - 24 * CELL_H) / 2;   // 24px

// Strict 8-color 1-bit-per-channel World System Teletext palette (pure RGBYCMW + Black)
const BLACK: [u8; 4] = [0, 0, 0, 255];
const RED: [u8; 4] = [235, 24, 24, 255];
const GREEN: [u8; 4] = [28, 232, 40, 255];
const YELLOW: [u8; 4] = [248, 236, 28, 255];
const BLUE: [u8; 4] = [24, 44, 224, 255];
#[allow(dead_code)]
const MAGENTA: [u8; 4] = [230, 32, 224, 255];
const CYAN: [u8; 4] = [28, 232, 238, 255];
const WHITE: [u8; 4] = [244, 244, 244, 255];

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum TeletextMode {
    /// Actively connecting / resolving / buffering a web stream or web channel.
    Loading,
    /// Web stream failed, went offline, or timed out with no incoming video frames.
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

    /// Fills a cell span `[col..col+len)` on `row` with a solid 8-color Teletext background.
    fn fill_cells(&mut self, col: i32, row: i32, len: i32, bg: [u8; 4]) {
        self.canvas.fill_rect(
            ORIGIN_X + col * CELL_W,
            ORIGIN_Y + row * CELL_H,
            len * CELL_W,
            CELL_H,
            bg,
        );
    }

    /// Draws strict 40-column fixed-width Teletext text snapped to cell boundaries (`14x18` cells).
    fn put_text(&mut self, col: i32, row: i32, text: &str, fg: [u8; 4], bg: Option<[u8; 4]>) {
        let max_chars = (COLS - col).max(0) as usize;
        let clipped: String = text.chars().take(max_chars).collect();
        if let Some(b) = bg {
            self.fill_cells(col, row, clipped.chars().count() as i32, b);
        }
        for (i, ch) in clipped.chars().enumerate() {
            let x = ORIGIN_X + (col + i as i32) * CELL_W + 2;
            let y = ORIGIN_Y + row * CELL_H + 1;
            let mut s = [0u8; 4];
            let s = ch.encode_utf8(&mut s);
            self.canvas.draw_text(x, y, s, 1, 2, fg, None);
        }
    }

    /// Draws authentic WST **double-height** Teletext text spanning `row` and `row + 1` (14x36 per cell!).
    fn put_double_height(&mut self, col: i32, row: i32, text: &str, fg: [u8; 4], bg: Option<[u8; 4]>) {
        let max_chars = (COLS - col).max(0) as usize;
        let clipped: String = text.chars().take(max_chars).collect();
        if let Some(b) = bg {
            self.fill_cells(col, row, clipped.chars().count() as i32, b);
            self.fill_cells(col, row + 1, clipped.chars().count() as i32, b);
        }
        for (i, ch) in clipped.chars().enumerate() {
            let x = ORIGIN_X + (col + i as i32) * CELL_W + 2;
            let y = ORIGIN_Y + row * CELL_H + 2;
            let mut s = [0u8; 4];
            let s = ch.encode_utf8(&mut s);
            // Tall 1x4 double-height SAA5050 proportions inside 14x36 cell
            self.canvas.draw_text(x, y, s, 1, 4, fg, None);
        }
    }

    /// Draws a single WST 2x3 separated sixel mosaic character (`bits`: bit0=TL, bit1=TR, bit2=ML, bit3=MR, bit4=BL, bit5=BR).
    fn put_sixel(&mut self, col: i32, row: i32, bits: u8, fg: [u8; 4], separated: bool) {
        let cx = ORIGIN_X + col * CELL_W;
        let cy = ORIGIN_Y + row * CELL_H;
        let gap = if separated { 2 } else { 0 };
        let sub_w = CELL_W / 2;
        let sub_h = CELL_H / 3;
        for sy in 0..3 {
            for sx in 0..2 {
                let bit = 1u8 << (sy * 2 + sx);
                if (bits & bit) != 0 {
                    self.canvas.fill_rect(
                        cx + sx * sub_w + gap / 2,
                        cy + sy * sub_h + gap / 2,
                        (sub_w - gap).max(1),
                        (sub_h - gap).max(1),
                        fg,
                    );
                }
            }
        }
    }

    /// Renders a 640x480 authentic 1980s 40x24 Ceefax/Teletext status frame.
    pub fn render(&mut self, info: &TeletextInfo<'_>) -> (u64, Arc<(usize, usize, Vec<u8>)>) {
        self.frame_counter = self.frame_counter.wrapping_add(1);
        let tick = self.frame_counter;
        let now = Local::now();

        // Pure Teletext black field (full-bleed behind overscan)
        self.canvas.fill_rect(0, 0, TT_W as i32, TT_H as i32, BLACK);

        // ---- Row 0: Classic BBC Ceefax / WST Page Header (`P104  VICTR 104  Thu 26 Nov 22:57:43`)
        let (page_str, page_col) = match info.mode {
            TeletextMode::Loading => {
                // Classic rolling Teletext page-search counter (`P100`..`P199`)
                let roll = 100 + ((tick / 5) % 99);
                (format!("P{roll:03}"), GREEN)
            }
            TeletextMode::Inactive => ("P888".to_string(), RED),
        };
        self.put_text(0, 0, &page_str, page_col, None);
        let ch_tag = match info.channel_number.filter(|&n| n > 0) {
            Some(n) if !info.callsign.is_empty() => format!("VICTR {:02} {}", n, info.callsign),
            Some(n) => format!("VICTR CH {:02}", n),
            None => "VICTR DATA".to_string(),
        };
        self.put_text(6, 0, &ch_tag, WHITE, None);
        let clock = now.format("%a %d %b %H:%M:%S").to_string().to_ascii_uppercase();
        let clock_col = (COLS - clock.len() as i32).max(20);
        self.put_text(clock_col, 0, &clock, YELLOW, None);

        // ---- Rows 2-4: Classic Full-Width Teletext Block Masthead (NO rounded cards, pure edge-to-edge strip)
        let ban_bg = match info.mode {
            TeletextMode::Loading => BLUE,
            TeletextMode::Inactive => RED,
        };
        for r in 2..=4 {
            self.fill_cells(0, r, COLS, ban_bg);
        }
        // Chunky 2x3 separated sixel Teletext graphics emblem on columns 1..6 (like Ceefax / ORACLE header art)
        let emblem_col = match info.mode {
            TeletextMode::Loading => YELLOW,
            TeletextMode::Inactive => WHITE,
        };
        let emblem: &[(i32, i32, u8)] = &[
            (1, 2, 0b111111), (2, 2, 0b110011), (3, 2, 0b111111),
            (1, 3, 0b111111), (2, 3, 0b001100), (3, 3, 0b111111),
            (1, 4, 0b001111), (2, 4, 0b111111), (3, 4, 0b001111),
        ];
        for &(ec, er, bits) in emblem {
            self.put_sixel(ec, er, bits, emblem_col, true);
        }
        let title_txt = if info.title.trim().is_empty() {
            "WEB DATA CHANNEL"
        } else {
            info.title.trim()
        };
        // Double-height Teletext headline across rows 2-3 + subtitle on row 4
        self.put_double_height(5, 2, title_txt, YELLOW, None);
        let sub_txt = if !info.subtitle.trim().is_empty() {
            info.subtitle.trim()
        } else if !info.source_host.trim().is_empty() {
            info.source_host.trim()
        } else {
            info.source_kind
        };
        self.put_text(5, 4, sub_txt, if info.mode == TeletextMode::Loading { CYAN } else { WHITE }, None);

        // ---- Row 5: Full-Width Separated Sixel Mosaic Rule
        let rule_col = match info.mode {
            TeletextMode::Loading => YELLOW,
            TeletextMode::Inactive => YELLOW,
        };
        for c in 0..COLS {
            self.put_sixel(c, 5, 0b001100, rule_col, true);
        }

        // ---- Rows 7-8: Double-Height Status Banner (Raw Teletext — blinking flash attribute!)
        let flash_on = !(tick / 22).is_multiple_of(2);
        match info.mode {
            TeletextMode::Loading => {
                let banner = if flash_on {
                    "*  TUNING / BUFFERING WEB STREAM  *"
                } else {
                    "   TUNING / BUFFERING WEB STREAM   "
                };
                self.put_double_height(2, 7, banner, CYAN, None);
            }
            TeletextMode::Inactive => {
                self.fill_cells(0, 7, COLS, RED);
                self.fill_cells(0, 8, COLS, RED);
                let banner = "   STREAM INACTIVE / NO SIGNAL    ";
                self.put_double_height(3, 7, banner, if flash_on { YELLOW } else { WHITE }, None);
            }
        }

        // ---- Row 10: Coarse 40-Column Teletext Separated-Sixel Carrier Bar
        match info.mode {
            TeletextMode::Loading => {
                // Coarse stepping sixel scan (pure 8-color Teletext blocks, zero smooth easing)
                let step = ((tick / 6) % 34) as i32;
                for c in 2..38 {
                    let pos = c - 2;
                    let (bits, col) = if pos >= step && pos < step + 4 {
                        (0b111111, YELLOW)
                    } else if pos == step - 1 || pos == step + 4 {
                        (0b111111, CYAN)
                    } else {
                        (0b001100, BLUE)
                    };
                    self.put_sixel(c, 10, bits, col, true);
                }
            }
            TeletextMode::Inactive => {
                for c in 2..38 {
                    let col = if (c + (tick / 24) as i32) % 2 == 0 { RED } else { YELLOW };
                    self.put_sixel(c, 10, 0b001111, col, true);
                }
            }
        }

        // ---- Rows 12-15: Pure 40-Column Fixed-Width Teletext Data Rows on Black (No modern card box)
        let secs = info.elapsed_secs.max(0.0) as u32;
        let elapsed_str = format!("{:02}:{:02}", secs / 60, secs % 60);
        let status_val = if info.status_line.trim().is_empty() {
            match info.mode {
                TeletextMode::Loading => "ACQUIRING CARRIER...",
                TeletextMode::Inactive => "OFF AIR / NO CARRIER",
            }
        } else {
            info.status_line.trim()
        };
        let host_val = if info.source_host.trim().is_empty() {
            info.source_kind
        } else {
            info.source_host.trim()
        };

        self.put_text(1, 12, "STATUS", YELLOW, None);
        self.put_text(10, 12, status_val, WHITE, None);

        self.put_text(1, 13, "CARRIER", YELLOW, None);
        self.put_text(10, 13, host_val, CYAN, None);

        self.put_text(1, 14, "SERVICE", YELLOW, None);
        self.put_text(10, 14, info.source_kind, GREEN, None);

        self.put_text(1, 15, if info.mode == TeletextMode::Loading { "ELAPSED" } else { "OFFLINE" }, YELLOW, None);
        let time_note = match info.mode {
            TeletextMode::Loading => format!("{elapsed_str}  WAITING FOR VIDEO FRAME"),
            TeletextMode::Inactive => format!("{elapsed_str}  PRESS R TO RE-TUNE"),
        };
        self.put_text(10, 15, &time_note, if info.mode == TeletextMode::Loading { WHITE } else { YELLOW }, None);

        // ---- Row 17: Full-Width Blue Mosaic Separator Strip
        for c in 0..COLS {
            self.put_sixel(c, 17, 0b001100, CYAN, false);
        }

        // ---- Rows 18-20: Authentic 2-Column Ceefax Page Index
        self.put_text(1, 18, "100", YELLOW, None);
        self.put_text(5, 18, "GUIDE ...... G", WHITE, None);
        self.put_text(21, 18, "300", YELLOW, None);
        self.put_text(25, 18, "RETRY ...... R", GREEN, None);

        self.put_text(1, 19, "200", YELLOW, None);
        self.put_text(5, 19, "CATALOG .... E", CYAN, None);
        self.put_text(21, 19, "400", YELLOW, None);
        self.put_text(25, 19, "CH +/- ... ]/[", WHITE, None);

        // ---- Row 21: Solid Blue Service Notice Strip (classic BBC Ceefax sub-footer)
        self.fill_cells(0, 21, COLS, BLUE);
        self.put_text(1, 21, "VICTR TELETEXT SERVICE   40-COL WST", YELLOW, None);

        // ---- Row 23: Authentic 4-Color Teletext Fastext Line (colored text directly on black, no pill boxes!)
        self.put_text(1, 23, "RETRY(R)", RED, None);
        self.put_text(11, 23, "GUIDE(G)", GREEN, None);
        self.put_text(21, 23, "LIST(E)", YELLOW, None);
        self.put_text(31, 23, "NEXT(])", CYAN, None);

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
