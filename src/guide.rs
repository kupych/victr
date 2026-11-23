//! Classic 1990s Prevue Channel split-screen Cable Guide rendered at 640x480.
//!
//! Fed directly into ViCTR's NTSC video input so the navy/blue/gold grid,
//! yellow cursor highlight, live preview window, and show info pass through
//! real composite chroma bleed, dot crawl, scanlines, shadow mask, and halation.

use crate::jellyfin::{ArtworkImage, Channel, ChannelKind};
use crate::osd::Canvas;
use chrono::{Local, TimeZone, Utc};
use std::sync::Arc;

pub const GUIDE_W: usize = 640;
pub const GUIDE_H: usize = 480;
// Classic 1990s SMPTE broadcast title-safe margins so CRT overscan & rounded
// tube corners never clip channel numbers, clock, or bottom hints.
const SAFE_X: i32 = 32;
const SAFE_Y: i32 = 24;
const SAFE_W: i32 = GUIDE_W as i32 - SAFE_X * 2; // 576px
const VISIBLE_ROWS: usize = 6;
const GRID_TOP: i32 = 236;
const ROW_H: i32 = 32;
const CH_COL_W: i32 = 104;
const COL_X0: i32 = SAFE_X + CH_COL_W + 4; // 140
const COL_W: i32 = (SAFE_W - CH_COL_W - 4) / 3; // 156px x 3 = 468px

pub struct GuideState {
    pub active: bool,
    pub selected_idx: usize,
    pub scroll_row: usize,
    /// Horizontal time-slot scroll offset in 30-minute increments (0 = current half-hour).
    pub time_offset_slots: i32,
    canvas: Canvas,
    frame_counter: u64,
}

impl Default for GuideState {
    fn default() -> Self {
        Self::new()
    }
}

impl GuideState {
    pub fn new() -> Self {
        Self {
            active: false,
            selected_idx: 0,
            scroll_row: 0,
            time_offset_slots: 0,
            canvas: Canvas::new(GUIDE_W, GUIDE_H),
            frame_counter: 0,
        }
    }

    pub fn clamp_selection(&mut self, len: usize) {
        if len == 0 {
            self.selected_idx = 0;
            self.scroll_row = 0;
            return;
        }
        if self.selected_idx >= len {
            self.selected_idx = len - 1;
        }
        if self.selected_idx < self.scroll_row {
            self.scroll_row = self.selected_idx;
        } else if self.selected_idx >= self.scroll_row + VISIBLE_ROWS {
            self.scroll_row = self.selected_idx + 1 - VISIBLE_ROWS;
        }
        let max_scroll = len.saturating_sub(VISIBLE_ROWS);
        if self.scroll_row > max_scroll {
            self.scroll_row = max_scroll;
        }
    }

    pub fn move_selection(&mut self, delta: isize, len: usize) {
        if len == 0 {
            return;
        }
        let next = (self.selected_idx as isize + delta).clamp(0, len as isize - 1) as usize;
        self.selected_idx = next;
        self.clamp_selection(len);
    }

    pub fn scroll_rows(&mut self, delta: isize, len: usize) {
        if len <= VISIBLE_ROWS {
            self.scroll_row = 0;
            return;
        }
        let max_scroll = len - VISIBLE_ROWS;
        self.scroll_row = (self.scroll_row as isize + delta).clamp(0, max_scroll as isize) as usize;
        if self.selected_idx < self.scroll_row {
            self.selected_idx = self.scroll_row;
        } else if self.selected_idx >= self.scroll_row + VISIBLE_ROWS {
            self.selected_idx = self.scroll_row + VISIBLE_ROWS - 1;
        }
    }

    pub fn shift_time(&mut self, delta_slots: i32) {
        self.time_offset_slots = (self.time_offset_slots + delta_slots).clamp(0, 12);
    }

    /// Maps a normalized `[0..1, 0..1]` CRT picture coordinate to a visible channel index.
    pub fn hit_test_row(&self, u: f32, v: f32, total_channels: usize) -> Option<usize> {
        let px = (u * GUIDE_W as f32) as i32;
        let py = (v * GUIDE_H as f32) as i32;
        if px < SAFE_X || px > GUIDE_W as i32 - SAFE_X || py < GRID_TOP || py >= GRID_TOP + VISIBLE_ROWS as i32 * ROW_H {
            return None;
        }
        let rel = ((py - GRID_TOP) / ROW_H) as usize;
        let idx = self.scroll_row + rel;
        if idx < total_channels { Some(idx) } else { None }
    }

    /// Renders one complete 640x480 classic 1990s Prevue Channel frame.
    pub fn render(
        &mut self,
        channels: &[&Channel],
        live_video: Option<&Arc<(usize, usize, Vec<u8>)>>,
        now_playing_label: Option<&str>,
        highlighted_art: Option<&ArtworkImage>,
    ) -> (u64, Arc<(usize, usize, Vec<u8>)>) {
        self.clamp_selection(channels.len());
        let now_unix = Utc::now().timestamp();
        let local_now = Local::now();

        // Full-bleed navy backdrop extending behind overscan so zero black borders ever show
        self.canvas
            .fill_vgrad(0, 0, GUIDE_W as i32, GUIDE_H as i32, [10, 28, 92, 255], [6, 14, 46, 255]);

        // ---- 1. Top Half: Classic Prevue Royal-Blue Split Backdrop
        self.canvas
            .fill_vgrad(0, 0, GUIDE_W as i32, 204, [14, 38, 118, 255], [7, 18, 62, 255]);
        // Subtle 90s studio horizontal striping on the top backdrop
        for y in (4..202).step_by(6) {
            self.canvas.fill_rect(0, y, GUIDE_W as i32, 1, [255, 255, 255, 10]);
        }

        // ---- 2. Top-Left: Live Preview Window (244x166 framed inside title-safe)
        let pv_x = SAFE_X + 4;
        let pv_y = SAFE_Y + 4;
        let pv_w = 244;
        let pv_h = 166;
        self.canvas
            .bevel_rect(pv_x - 4, pv_y - 4, pv_w + 8, pv_h + 8, [22, 30, 56, 255], [244, 198, 68, 255], [108, 76, 16, 255]);
        if let Some(vid) = live_video {
            let (vw, vh, ref data) = **vid;
            self.canvas.blit_rgba(data, vw, vh, pv_x, pv_y, pv_w, pv_h);
        } else {
            self.canvas
                .fill_vgrad(pv_x, pv_y, pv_w, pv_h, [18, 46, 134, 255], [10, 22, 74, 255]);
            self.canvas.draw_text(
                pv_x + 32,
                pv_y + 44,
                "ViCTR CABLE",
                2,
                2,
                [248, 206, 72, 255],
                Some([8, 12, 30, 255]),
            );
            self.canvas.draw_text(
                pv_x + 20,
                pv_y + 76,
                "PREVUE CHANNEL",
                2,
                2,
                [235, 242, 255, 255],
                Some([8, 12, 30, 255]),
            );
            self.canvas.draw_text(
                pv_x + 28,
                pv_y + 118,
                "SELECT A SHOW BELOW",
                1,
                2,
                [148, 204, 255, 255],
                None,
            );
        }
        if let Some(np) = now_playing_label {
            self.canvas.fill_rect(pv_x, pv_y + pv_h - 20, pv_w, 20, [6, 12, 32, 205]);
            self.canvas.draw_text_clipped(
                pv_x + 6,
                pv_y + pv_h - 16,
                np,
                pv_w - 12,
                1,
                2,
                [90, 255, 118, 255],
                None,
            );
        }

        // ---- 3. Top-Right: Prevue Brand + Live Clock + Highlighted Show Info & Artwork
        let rx = pv_x + pv_w + 14;
        let rw = (SAFE_X + SAFE_W) - rx;
        self.canvas
            .bevel_rect(rx, SAFE_Y, rw, 26, [18, 42, 118, 255], [96, 146, 232, 255], [8, 18, 56, 255]);
        self.canvas.draw_text(rx + 8, SAFE_Y + 5, "PREVUE", 2, 2, [248, 202, 58, 255], Some([6, 12, 34, 255]));
        let clock_str = local_now.format("%I:%M:%S %p").to_string();
        let clock_w = clock_str.len() as i32 * 8;
        self.canvas
            .draw_text(rx + rw - clock_w - 8, SAFE_Y + 5, &clock_str, 1, 2, [242, 248, 255, 255], Some([6, 12, 34, 255]));

        let card_y = SAFE_Y + 30;
        let card_h = 144;
        self.canvas
            .bevel_rect(rx, card_y, rw, card_h, [10, 24, 72, 255], [76, 118, 204, 255], [5, 10, 32, 255]);

        if let Some(sel) = channels.get(self.selected_idx) {
            let (title, meta, overview, _) = sel.preview_info(now_unix);
            let badge = format!("CH {:02} {}", sel.number, sel.callsign);
            self.canvas
                .draw_text_clipped(rx + 8, card_y + 7, &badge, rw - 16, 2, 2, [248, 206, 68, 255], Some([4, 8, 24, 255]));

            let text_w = if let Some(art) = highlighted_art {
                let (aw, ah, ref data) = **art;
                let tw = 88;
                let th = 54;
                let tx = rx + rw - tw - 8;
                let ty = card_y + 28;
                self.canvas.stroke_rect(tx - 2, ty - 2, tw + 4, th + 4, 2, [216, 174, 58, 255]);
                self.canvas.blit_rgba(data, aw, ah, tx, ty, tw, th);
                rw - tw - 22
            } else {
                rw - 16
            };

            self.canvas.draw_text_clipped(
                rx + 8,
                card_y + 29,
                &title,
                text_w,
                1,
                2,
                [244, 248, 255, 255],
                Some([4, 8, 24, 255]),
            );
            self.canvas.draw_text_clipped(
                rx + 8,
                card_y + 49,
                &meta,
                text_w,
                1,
                2,
                [112, 236, 172, 255],
                None,
            );

            let lines = wrap_text(&overview, ((rw - 16) / 8).max(12) as usize, 4);
            for (li, line) in lines.iter().enumerate() {
                self.canvas.draw_text(
                    rx + 8,
                    card_y + 72 + li as i32 * 17,
                    line,
                    1,
                    2,
                    [196, 218, 248, 255],
                    None,
                );
            }
        }

        // ---- 4. Classic Gold/Navy 30-Minute Time Header Bar (y = 204..236)
        self.canvas.fill_rect(0, 204, GUIDE_W as i32, 3, [242, 194, 52, 255]);
        self.canvas
            .fill_vgrad(0, 207, GUIDE_W as i32, 29, [26, 54, 138, 255], [14, 30, 84, 255]);

        let date_str = local_now.format("%a %b %d").to_string().to_ascii_uppercase();
        self.canvas.bevel_rect(SAFE_X, 209, CH_COL_W, 25, [186, 134, 22, 255], [250, 216, 98, 255], [108, 74, 8, 255]);
        self.canvas
            .draw_text_clipped(SAFE_X + 8, 214, &date_str, CH_COL_W - 14, 1, 2, [10, 18, 44, 255], None);

        let base_slot = (now_unix / 1800) * 1800 + self.time_offset_slots as i64 * 1800;
        let win_start = base_slot;
        let win_end = base_slot + 3 * 1800;

        for c in 0..3 {
            let slot_ts = base_slot + c as i64 * 1800;
            let label = Local
                .timestamp_opt(slot_ts, 0)
                .single()
                .map(|dt| dt.format("%I:%M %p").to_string())
                .unwrap_or_else(|| "+30M".into());
            let cx = COL_X0 + c as i32 * COL_W;
            self.canvas
                .bevel_rect(cx, 209, COL_W - 2, 25, [22, 52, 142, 255], [104, 152, 242, 255], [8, 20, 62, 255]);
            self.canvas.draw_text(cx + 10, 214, &label, 1, 2, [248, 212, 74, 255], Some([6, 12, 36, 255]));
        }

        // ---- 5. Scrolling Channel Grid (6 rows x 32px = 192px, y = 236..428)
        self.canvas.fill_rect(0, GRID_TOP, GUIDE_W as i32, VISIBLE_ROWS as i32 * ROW_H, [6, 14, 46, 255]);

        for row in 0..VISIBLE_ROWS {
            let idx = self.scroll_row + row;
            let ry = GRID_TOP + row as i32 * ROW_H;
            let Some(ch) = channels.get(idx) else {
                self.canvas.fill_rect(SAFE_X, ry + 1, SAFE_W, ROW_H - 2, [8, 18, 56, 255]);
                continue;
            };
            let is_sel = idx == self.selected_idx;
            let is_movie = matches!(ch.kind, ChannelKind::MovieGenre { .. });

            let (c_fill, c_hi, c_lo, c_fg) = if is_sel {
                ([244, 198, 52, 255], [255, 236, 148, 255], [154, 112, 16, 255], [10, 18, 44, 255])
            } else if is_movie {
                ([34, 72, 128, 255], [96, 154, 220, 255], [14, 34, 68, 255], [248, 214, 96, 255])
            } else {
                ([24, 52, 134, 255], [86, 132, 224, 255], [10, 22, 66, 255], [244, 208, 78, 255])
            };
            self.canvas.bevel_rect(SAFE_X, ry + 1, CH_COL_W, ROW_H - 2, c_fill, c_hi, c_lo);
            let num_s = format!("{:02}", ch.number);
            self.canvas.draw_text(SAFE_X + 6, ry + 8, &num_s, 2, 2, c_fg, None);
            let cs_fg = if is_sel { [10, 18, 44, 255] } else { [238, 244, 255, 255] };
            self.canvas.draw_text_clipped(SAFE_X + 44, ry + 8, &ch.callsign, CH_COL_W - 48, 1, 2, cs_fg, None);

            let entries = ch.grid_entries(win_start, win_end, now_unix);
            let total_span = (win_end - win_start) as f32;
            let grid_w = (3 * COL_W - 2) as f32;
            for entry in entries {
                let s = entry.start_unix.max(win_start);
                let e = entry.end_unix.min(win_end);
                if e <= s {
                    continue;
                }
                let x0 = COL_X0 + (((s - win_start) as f32 / total_span) * grid_w).round() as i32;
                let x1 = COL_X0 + (((e - win_start) as f32 / total_span) * grid_w).round() as i32;
                let cw = (x1 - x0 - 2).max(12);

                let (fill, hi, lo, fg) = if is_sel {
                    ([246, 202, 58, 255], [255, 240, 162, 255], [156, 116, 18, 255], [10, 18, 44, 255])
                } else if is_movie {
                    ([22, 60, 108, 255], [76, 134, 198, 255], [10, 28, 56, 255], [234, 246, 255, 255])
                } else {
                    ([18, 44, 122, 255], [72, 114, 210, 255], [8, 18, 56, 255], [234, 242, 255, 255])
                };
                self.canvas.bevel_rect(x0, ry + 1, cw, ROW_H - 2, fill, hi, lo);

                let mut label = String::new();
                if entry.start_unix < win_start {
                    label.push_str("< ");
                } else if entry.is_live_in_progress && is_movie {
                    label.push_str("► ");
                }
                label.push_str(&entry.title);
                self.canvas.draw_text_clipped(
                    x0 + 6,
                    ry + 8,
                    &label,
                    cw - 12,
                    1,
                    2,
                    fg,
                    if is_sel { None } else { Some([6, 12, 34, 255]) },
                );
            }
        }

        // ---- 6. Bottom Status Ticker (y = 428..456, inside title-safe)
        let foot_y = GRID_TOP + VISIBLE_ROWS as i32 * ROW_H;
        self.canvas.fill_rect(0, foot_y, GUIDE_W as i32, 2, [242, 194, 52, 255]);
        self.canvas.fill_rect(0, foot_y + 2, GUIDE_W as i32, GUIDE_H as i32 - foot_y - 2, [8, 16, 48, 255]);
        let page_info = if channels.is_empty() {
            "NO CHANNELS".to_string()
        } else {
            format!("{}/{}", self.selected_idx + 1, channels.len())
        };
        self.canvas.draw_text(
            SAFE_X + 4,
            foot_y + 7,
            "ARROWS:BROWSE  ENTER:TUNE  G:EXIT  0-9:DIAL",
            1,
            2,
            [244, 206, 78, 255],
            None,
        );
        let pw = page_info.len() as i32 * 8;
        self.canvas
            .draw_text(SAFE_X + SAFE_W - pw - 4, foot_y + 7, &page_info, 1, 2, [152, 212, 255, 255], None);

        self.frame_counter += 1;
        // High bit set so guide frame IDs never collide with user/mpv image IDs
        let id = 0x8000_0000_0000_0000 | self.frame_counter;
        (id, Arc::new((GUIDE_W, GUIDE_H, self.canvas.rgba.clone())))
    }
}

fn wrap_text(text: &str, max_cols: usize, max_lines: usize) -> Vec<String> {
    let mut lines = Vec::new();
    let mut cur = String::new();
    for word in text.split_whitespace() {
        if cur.is_empty() {
            cur.push_str(word);
        } else if cur.len() + 1 + word.len() <= max_cols {
            cur.push(' ');
            cur.push_str(word);
        } else {
            lines.push(std::mem::take(&mut cur));
            if lines.len() >= max_lines {
                break;
            }
            cur.push_str(word);
        }
    }
    if !cur.is_empty() && lines.len() < max_lines {
        lines.push(cur);
    }
    if let Some(last) = lines.last_mut() {
        if last.len() > max_cols {
            last.truncate(max_cols.saturating_sub(2));
            last.push_str("..");
        }
    }
    lines
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::jellyfin::MediaItem;

    #[test]
    fn guide_renders_and_hit_tests_rows() {
        let ch1 = Channel {
            number: 1,
            callsign: "GUIDE".into(),
            name: "Program Guide".into(),
            artwork_id: String::new(),
            kind: ChannelKind::Guide,
        };
        let ch2 = Channel {
            number: 2,
            callsign: "SEINF".into(),
            name: "Seinfeld".into(),
            artwork_id: String::new(),
            kind: ChannelKind::Series {
                series_id: "s1".into(),
                overview: "A show about nothing in New York City.".into(),
                is_continue: true,
                is_favorite: false,
                episodes: vec![MediaItem {
                    id: "e1".into(),
                    title: "The Contest".into(),
                    season: Some(4),
                    episode: Some(11),
                    overview: "Jerry, George, Elaine, and Kramer make a wager.".into(),
                    runtime_secs: 1380.0,
                    played: false,
                    resume_secs: 0.0,
                    artwork_id: "s1".into(),
                }],
                next_up_episode_id: Some("e1".into()),
                loaded_episodes: true,
            },
        };
        let list = vec![&ch1, &ch2];
        let mut g = GuideState::new();
        g.selected_idx = 1;
        let (id, frame) = g.render(&list, None, Some("NOW: CH 02 SEINF"), None);
        assert!(id > 0);
        assert_eq!(frame.0, GUIDE_W);
        assert_eq!(frame.1, GUIDE_H);
        assert_eq!(frame.2.len(), GUIDE_W * GUIDE_H * 4);
        assert_eq!(g.hit_test_row(0.5, (GRID_TOP as f32 + 10.0) / GUIDE_H as f32, 2), Some(0));
        assert_eq!(g.hit_test_row(0.5, (GRID_TOP as f32 + ROW_H as f32 + 10.0) / GUIDE_H as f32, 2), Some(1));
    }
}
