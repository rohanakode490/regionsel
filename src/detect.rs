//! Detectors: each looks for ONE kind of evidence and emits scored `Candidate`s.
//! They never talk to each other; `build_tree` reconciles them.
//!
//!   BackgroundRunDetector  rectangles of uniform background color  -> Block
//!   BorderDetector         closed rectangles of box-drawing glyphs -> Panel

use std::collections::HashMap;

use crate::{Candidate, Cell, Color, Detector, Grid, Rect, RegionKind, Signal};

// ───────────────────────── shared helpers ─────────────────────────

fn is_blank(cell: &Cell) -> bool {
    cell.ch.is_whitespace()
}

/// Trim `r` vertically to the rows that contain text, keeping its FULL width. None if blank.
/// Columns are deliberately not trimmed: the unwrap heuristic judges "is this line full?"
/// against the region width, so shrinking to the longest line would make it look full and
/// glue unrelated lines together. Left padding is removed later by dedent, right padding
/// by trailing-whitespace trimming.
fn ink_rows(grid: &Grid, r: Rect) -> Option<Rect> {
    let has_ink = |row: u16| {
        (r.col0..r.col1).any(|c| {
            let cell = grid.get(row, c);
            cell.width > 0 && !is_blank(cell)
        })
    };
    let first = (r.row0..r.row1).find(|&row| has_ink(row))?;
    let last = (r.row0..r.row1).rev().find(|&row| has_ink(row))?;
    Some(Rect { row0: first, row1: last + 1, ..r })
}

// ───────────────────────── BackgroundRunDetector ─────────────────────────

/// Finds rectangles of uniform, non-canvas background color.
///
/// Many TUIs mark message blocks, diff hunks, code blocks and panels with a background,
/// which is a more reliable signal than borders.
///
/// Algorithm:
///  1. Canvas = the most common background on screen (Default, or a theme color that a
///     full-screen app paints everywhere). It never forms regions.
///  2. Per row, split into horizontal runs of identical background. Reverse-video cells
///     count as their foreground color (htop headers, `less` status bars, selections).
///  3. Merge runs in consecutive rows when the color and left edge match. The right edge may
///     differ ("ragged" highlights that only cover the text), the group keeps the widest.
///  4. Drop groups narrower than `min_width` or with no text; trim blank rows for `content`.
///
/// Known limits: a blank line without background splits a block in two; a gutter with its
/// own shade becomes a separate sibling block instead of `Region::gutter`.
pub struct BackgroundRunDetector {
    pub min_width: u16,
}

impl Default for BackgroundRunDetector {
    fn default() -> Self {
        BackgroundRunDetector { min_width: 4 }
    }
}

fn effective_bg(cell: &Cell) -> Color {
    if cell.attrs.reverse {
        if cell.fg == Color::Default { Color::Indexed(15) } else { cell.fg }
    } else {
        cell.bg
    }
}

fn dominant_bg(grid: &Grid) -> Color {
    let mut counts: HashMap<Color, usize> = HashMap::new();
    for r in 0..grid.rows {
        for c in 0..grid.cols {
            let cell = grid.get(r, c);
            if cell.width > 0 {
                *counts.entry(effective_bg(cell)).or_insert(0) += 1;
            }
        }
    }
    // Deterministic on ties (HashMap order is random): prefer Default, then the lowest key.
    let rank = |c: Color| match c {
        Color::Default => (0u8, 0u32),
        Color::Indexed(i) => (1, i as u32),
        Color::Rgb(r, g, b) => (2, (r as u32) << 16 | (g as u32) << 8 | b as u32),
    };
    counts
        .into_iter()
        .max_by(|&(a, na), &(b, nb)| na.cmp(&nb).then(rank(b).cmp(&rank(a))))
        .map(|(c, _)| c)
        .unwrap_or(Color::Default)
}

struct Group {
    color: Color,
    r0: u16,
    r1: u16, // inclusive
    c0: u16,
    c1: u16, // exclusive
}

impl Detector for BackgroundRunDetector {
    fn name(&self) -> &'static str {
        "bg-run"
    }

    fn detect(&self, grid: &Grid) -> Vec<Candidate> {
        let canvas = dominant_bg(grid);
        let is_bg = |c: Color| c != Color::Default && c != canvas;

        let mut groups: Vec<Group> = Vec::new();
        for r in 0..grid.rows {
            let mut c = 0;
            while c < grid.cols {
                let color = effective_bg(grid.get(r, c));
                if !is_bg(color) {
                    c += 1;
                    continue;
                }
                let start = c;
                while c < grid.cols && effective_bg(grid.get(r, c)) == color {
                    c += 1;
                }
                match groups.iter_mut().find(|g| g.color == color && g.c0 == start && g.r1 + 1 == r) {
                    Some(g) => {
                        g.r1 = r;
                        g.c1 = g.c1.max(c);
                    }
                    None => groups.push(Group { color, r0: r, r1: r, c0: start, c1: c }),
                }
            }
        }

        groups
            .into_iter()
            .filter_map(|g| {
                let outer = Rect { row0: g.r0, col0: g.c0, row1: g.r1 + 1, col1: g.c1 };
                if outer.width() < self.min_width {
                    return None;
                }
                let content = ink_rows(grid, outer)?; // nothing to copy -> not a region
                let rows = g.r1 - g.r0 + 1;
                let w = outer.width();

                // Bigger and taller = more likely a real block than a badge or a highlight.
                let mut conf: f32 = 0.55;
                if rows >= 2 {
                    conf += 0.20;
                }
                if rows >= 4 {
                    conf += 0.05;
                }
                if w * 2 >= grid.cols {
                    conf += 0.15;
                }
                if rows == 1 && w * 2 < grid.cols {
                    conf -= 0.20;
                }
                Some(Candidate {
                    kind: RegionKind::Block,
                    outer,
                    content,
                    gutter: None,
                    confidence: conf.clamp(0.0, 0.95),
                    signal: Signal::BackgroundRun,
                })
            })
            .collect()
    }
}

// ───────────────────────── BorderDetector ─────────────────────────

/// Finds closed rectangles drawn with box-drawing glyphs.
///
/// Every cell gets direction flags (UP/DOWN/LEFT/RIGHT) from its glyph: `┌` = DOWN|RIGHT,
/// `┼` = all four, `─` = LEFT|RIGHT. Two neighbours are *linked* when both point at each
/// other. From the links we precompute, per cell, how far an unbroken line runs right
/// (`hspan`) and down (`vspan`). A rectangle exists for corners (r0,c0)..(r1,c1) iff all
/// four edges are unbroken lines, which is four table lookups.
///
/// Tables and tmux-style splits produce many overlapping candidate rectangles (every
/// union of adjacent cells). We keep a rectangle only if no other candidate *crosses* it
/// (partial overlap of at least 2x2), which leaves single cells plus the outer frame and
/// drops the ambiguous in-between unions.
///
/// `ascii` also accepts `+---+` / `|` frames at lower confidence.
pub struct BorderDetector {
    pub ascii: bool,
    /// Safety valve: dense grids of lines can have a huge number of candidate rectangles.
    pub max_rects: usize,
}

impl Default for BorderDetector {
    fn default() -> Self {
        BorderDetector { ascii: true, max_rects: 2000 }
    }
}

const U: u8 = 1;
const D: u8 = 2;
const L: u8 = 4;
const R: u8 = 8;

/// Direction flags for a glyph (light, heavy, double, rounded, dashed and mixed variants
/// all behave the same for our purposes).
fn dirs(ch: char, ascii: bool) -> u8 {
    match ch {
        '+' if ascii => U | D | L | R,
        '-' if ascii => L | R,
        '|' if ascii => U | D,
        _ => match ch as u32 {
            0x2500 | 0x2501 | 0x2504 | 0x2505 | 0x2508 | 0x2509 | 0x254C | 0x254D | 0x2550 | 0x257C
            | 0x257E => L | R,
            0x2502 | 0x2503 | 0x2506 | 0x2507 | 0x250A | 0x250B | 0x254E | 0x254F | 0x2551 | 0x257D
            | 0x257F => U | D,
            0x250C..=0x250F | 0x2552..=0x2554 | 0x256D => D | R,
            0x2510..=0x2513 | 0x2555..=0x2557 | 0x256E => D | L,
            0x2514..=0x2517 | 0x2558..=0x255A | 0x2570 => U | R,
            0x2518..=0x251B | 0x255B..=0x255D | 0x256F => U | L,
            0x251C..=0x2523 | 0x255E..=0x2560 => U | D | R,
            0x2524..=0x252B | 0x2561..=0x2563 => U | D | L,
            0x252C..=0x2533 | 0x2564..=0x2566 => D | L | R,
            0x2534..=0x253B | 0x2567..=0x2569 => U | L | R,
            0x253C..=0x254B | 0x256A..=0x256C => U | D | L | R,
            0x2574 | 0x2578 => L,
            0x2575 | 0x2579 => U,
            0x2576 | 0x257A => R,
            0x2577 | 0x257B => D,
            _ => 0,
        },
    }
}

impl Detector for BorderDetector {
    fn name(&self) -> &'static str {
        "border"
    }

    fn detect(&self, grid: &Grid) -> Vec<Candidate> {
        let (rows, cols) = (grid.rows as usize, grid.cols as usize);
        if rows < 3 || cols < 3 {
            return Vec::new();
        }

        let mut flags = vec![0u8; rows * cols];
        for r in 0..rows {
            for c in 0..cols {
                let cell = grid.get(r as u16, c as u16);
                if cell.width > 0 {
                    flags[r * cols + c] = dirs(cell.ch, self.ascii);
                }
            }
        }

        // hspan[r][c]: how many steps an unbroken line runs RIGHT from (r,c). vspan: DOWN.
        let mut hspan = vec![0u16; rows * cols];
        let mut vspan = vec![0u16; rows * cols];
        for r in 0..rows {
            for c in (0..cols - 1).rev() {
                if flags[r * cols + c] & R != 0 && flags[r * cols + c + 1] & L != 0 {
                    hspan[r * cols + c] = 1 + hspan[r * cols + c + 1];
                }
            }
        }
        for c in 0..cols {
            for r in (0..rows - 1).rev() {
                if flags[r * cols + c] & D != 0 && flags[(r + 1) * cols + c] & U != 0 {
                    vspan[r * cols + c] = 1 + vspan[(r + 1) * cols + c];
                }
            }
        }

        // Every rectangle whose four edges are unbroken lines (at least 3x3 so there is an
        // interior). Top edge: hspan at the corner. Left/right: vspan at the top corners.
        // Bottom edge: hspan at the bottom-left corner.
        let mut rects: Vec<Rect> = Vec::new();
        'search: for r0 in 0..rows {
            for c0 in 0..cols {
                let hs = hspan[r0 * cols + c0] as usize;
                let vs = vspan[r0 * cols + c0] as usize;
                if hs < 2 || vs < 2 {
                    continue;
                }
                for c1 in c0 + 2..=c0 + hs {
                    let vs_right = vspan[r0 * cols + c1] as usize;
                    if vs_right < 2 {
                        continue;
                    }
                    for r1 in r0 + 2..=r0 + vs.min(vs_right) {
                        if hspan[r1 * cols + c0] as usize >= c1 - c0 {
                            rects.push(Rect {
                                row0: r0 as u16,
                                col0: c0 as u16,
                                row1: (r1 + 1) as u16,
                                col1: (c1 + 1) as u16,
                            });
                            if rects.len() >= self.max_rects {
                                break 'search;
                            }
                        }
                    }
                }
            }
        }

        // Drop rectangles crossed by another candidate (ambiguous unions of table cells).
        let keep: Vec<bool> = (0..rects.len())
            .map(|i| !rects.iter().enumerate().any(|(j, o)| j != i && rects[i].crosses(o)))
            .collect();

        rects
            .iter()
            .zip(keep)
            .filter(|&(_, k)| k)
            .map(|(r, _)| {
                let interior =
                    Rect { row0: r.row0 + 1, col0: r.col0 + 1, row1: r.row1 - 1, col1: r.col1 - 1 };
                let content = ink_rows(grid, interior).unwrap_or(interior);
                let ascii_frame = grid.get(r.row0, r.col0).ch == '+';
                Candidate {
                    kind: RegionKind::Panel,
                    outer: *r,
                    content,
                    gutter: None,
                    confidence: if ascii_frame { 0.7 } else { 0.9 },
                    signal: Signal::BoxBorder,
                }
            })
            .collect()
    }
}

// ───────────────────────── tests ─────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Engine, Pos};

    fn put(g: &mut Grid, r: u16, c: u16, ch: char) {
        let mut cell = *g.get(r, c); // keep any background already painted
        cell.ch = ch;
        g.set(r, c, cell);
    }
    fn text(g: &mut Grid, r: u16, c: u16, s: &str) {
        for (i, ch) in s.chars().enumerate() {
            put(g, r, c + i as u16, ch);
        }
    }
    /// Draw a light box; corners are inclusive.
    fn draw_box(g: &mut Grid, r0: u16, c0: u16, r1: u16, c1: u16) {
        for c in c0 + 1..c1 {
            put(g, r0, c, '─');
            put(g, r1, c, '─');
        }
        for r in r0 + 1..r1 {
            put(g, r, c0, '│');
            put(g, r, c1, '│');
        }
        put(g, r0, c0, '┌');
        put(g, r0, c1, '┐');
        put(g, r1, c0, '└');
        put(g, r1, c1, '┘');
    }
    fn fill_bg(g: &mut Grid, rect: Rect, color: Color) {
        for r in rect.row0..rect.row1 {
            for c in rect.col0..rect.col1 {
                let mut cell = *g.get(r, c);
                cell.bg = color;
                g.set(r, c, cell);
            }
        }
    }
    fn rect(r0: u16, c0: u16, r1: u16, c1: u16) -> Rect {
        Rect { row0: r0, col0: c0, row1: r1, col1: c1 }
    }

    // ── borders ──

    #[test]
    fn single_box_with_text() {
        let mut g = Grid::new(20, 6);
        draw_box(&mut g, 1, 2, 4, 15);
        text(&mut g, 2, 4, "hello");
        let cands = BorderDetector::default().detect(&g);
        assert_eq!(cands.len(), 1);
        assert_eq!(cands[0].outer, rect(1, 2, 5, 16));
        assert_eq!(cands[0].content, rect(2, 3, 3, 15)); // ink rows, full interior width
        assert_eq!(cands[0].kind, RegionKind::Panel);
    }

    #[test]
    fn nested_boxes() {
        let mut g = Grid::new(30, 10);
        draw_box(&mut g, 0, 0, 9, 29);
        draw_box(&mut g, 2, 3, 6, 20);
        assert_eq!(BorderDetector::default().detect(&g).len(), 2);
    }

    #[test]
    fn shared_border_panels_are_both_kept_plus_the_frame() {
        // tmux-style: two panes sharing the middle column, with junctions ┬ and ┴.
        let mut g = Grid::new(21, 6);
        draw_box(&mut g, 0, 0, 5, 20);
        put(&mut g, 0, 10, '┬');
        put(&mut g, 5, 10, '┴');
        for r in 1..5 {
            put(&mut g, r, 10, '│');
        }
        let rects: Vec<Rect> = BorderDetector::default().detect(&g).iter().map(|c| c.outer).collect();
        assert_eq!(rects.len(), 3, "{rects:?}");
        assert!(rects.contains(&rect(0, 0, 6, 11))); // left
        assert!(rects.contains(&rect(0, 10, 6, 21))); // right
        assert!(rects.contains(&rect(0, 0, 6, 21))); // frame
    }

    #[test]
    fn two_by_two_table_keeps_cells_and_frame_only() {
        let mut g = Grid::new(21, 9);
        draw_box(&mut g, 0, 0, 8, 20);
        for c in 1..20 {
            put(&mut g, 4, c, '─');
        }
        for r in 1..8 {
            put(&mut g, r, 10, '│');
        }
        put(&mut g, 0, 10, '┬');
        put(&mut g, 8, 10, '┴');
        put(&mut g, 4, 0, '├');
        put(&mut g, 4, 20, '┤');
        put(&mut g, 4, 10, '┼');
        let cands = BorderDetector::default().detect(&g);
        // 4 cells + frame. Dominoes (two cells) and the 2x1 strips are dropped.
        assert_eq!(cands.len(), 5, "{:?}", cands.iter().map(|c| c.outer).collect::<Vec<_>>());
    }

    #[test]
    fn ascii_frame_has_lower_confidence() {
        let mut g = Grid::new(12, 5);
        text(&mut g, 0, 0, "+----------+");
        text(&mut g, 1, 0, "| hi       |");
        text(&mut g, 2, 0, "| there    |");
        text(&mut g, 3, 0, "+----------+");
        let cands = BorderDetector::default().detect(&g);
        assert_eq!(cands.len(), 1);
        assert!(cands[0].confidence < 0.9);
        assert!(BorderDetector { ascii: false, max_rects: 10 }.detect(&g).is_empty());
    }

    #[test]
    fn markdown_table_and_diff_lines_are_not_frames() {
        let mut g = Grid::new(20, 6);
        text(&mut g, 0, 0, "| a | b |");
        text(&mut g, 1, 0, "|---|---|");
        text(&mut g, 2, 0, "| 1 | 2 |");
        text(&mut g, 3, 0, "+added line");
        text(&mut g, 4, 0, "-removed line");
        assert!(BorderDetector::default().detect(&g).is_empty());
    }

    #[test]
    fn broken_edge_is_not_a_box() {
        let mut g = Grid::new(20, 6);
        draw_box(&mut g, 1, 2, 4, 15);
        put(&mut g, 1, 8, ' '); // gap in the top edge
        assert!(BorderDetector::default().detect(&g).is_empty());
    }

    // ── background ──

    #[test]
    fn bg_block_is_found_and_padding_is_trimmed() {
        let mut g = Grid::new(30, 8);
        let blue = Color::Indexed(4);
        fill_bg(&mut g, rect(2, 0, 5, 30), blue);
        text(&mut g, 3, 2, "hello world");
        text(&mut g, 4, 2, "second");
        let cands = BackgroundRunDetector::default().detect(&g);
        assert_eq!(cands.len(), 1);
        assert_eq!(cands[0].outer, rect(2, 0, 5, 30));
        assert_eq!(cands[0].content, rect(3, 0, 5, 30)); // blank first row trimmed
    }

    #[test]
    fn explicit_canvas_color_is_ignored() {
        let mut g = Grid::new(30, 8);
        let all = g.bounds();
        fill_bg(&mut g, all, Color::Indexed(234)); // app paints the whole screen
        fill_bg(&mut g, rect(2, 4, 5, 20), Color::Indexed(236)); // a panel on top
        text(&mut g, 3, 6, "panel text");
        let cands = BackgroundRunDetector::default().detect(&g);
        assert_eq!(cands.len(), 1);
        assert_eq!(cands[0].outer, rect(2, 4, 5, 20));
    }

    #[test]
    fn ragged_right_rows_merge_but_badges_and_blank_bars_do_not() {
        let mut g = Grid::new(40, 8);
        let green = Color::Indexed(2);
        fill_bg(&mut g, rect(1, 0, 2, 20), green);
        fill_bg(&mut g, rect(2, 0, 3, 11), green);
        text(&mut g, 1, 1, "first line is long");
        text(&mut g, 2, 1, "short one");
        fill_bg(&mut g, rect(6, 0, 7, 2), green); // 2-wide badge: below min_width
        fill_bg(&mut g, rect(5, 10, 6, 30), Color::Indexed(1)); // colored bar, no text
        let cands = BackgroundRunDetector::default().detect(&g);
        assert_eq!(cands.len(), 1, "{cands:?}");
        assert_eq!(cands[0].outer, rect(1, 0, 3, 20));
    }

    #[test]
    fn different_colors_do_not_merge() {
        let mut g = Grid::new(30, 6);
        fill_bg(&mut g, rect(1, 0, 3, 30), Color::Indexed(1)); // red hunk
        fill_bg(&mut g, rect(3, 0, 5, 30), Color::Indexed(2)); // green hunk
        text(&mut g, 1, 1, "removed");
        text(&mut g, 3, 1, "added");
        assert_eq!(BackgroundRunDetector::default().detect(&g).len(), 2);
    }

    #[test]
    fn canvas_tie_is_deterministic_and_prefers_default() {
        // 60 default / 60 red / 60 green cells: must never pick a colored hunk as canvas.
        let mut g = Grid::new(30, 6);
        fill_bg(&mut g, rect(1, 0, 3, 30), Color::Indexed(1));
        fill_bg(&mut g, rect(3, 0, 5, 30), Color::Indexed(2));
        for _ in 0..50 {
            assert_eq!(dominant_bg(&g), Color::Default);
        }
    }

    #[test]
    fn reverse_video_counts_as_a_background() {
        let mut g = Grid::new(30, 4);
        for c in 0..30 {
            g.set(3, c, Cell { ch: 'x', attrs: crate::Attrs { reverse: true, ..Default::default() }, ..Cell::default() });
        }
        let cands = BackgroundRunDetector::default().detect(&g);
        assert_eq!(cands.len(), 1);
        assert_eq!(cands[0].outer, rect(3, 0, 4, 30));
    }

    // ── through the engine ──

    #[test]
    fn engine_ladder_goes_word_line_panel_pane() {
        let mut g = Grid::new(30, 8);
        draw_box(&mut g, 1, 1, 6, 28);
        text(&mut g, 2, 3, "alpha beta");
        text(&mut g, 3, 3, "gamma");
        let a = Engine::with_defaults().analyze(&g);
        let ladder = a.ladder_at(Pos { row: 2, col: 4 });
        let kinds: Vec<_> = ladder.iter().map(|t| t.kind).collect();
        assert_eq!(kinds, vec![RegionKind::Word, RegionKind::Line, RegionKind::Panel, RegionKind::Pane]);
        assert_eq!(a.extract(&ladder[0]), "alpha");
        assert_eq!(a.extract(&ladder[2]), "alpha beta\ngamma"); // no borders, no padding
    }

    #[test]
    fn fully_framed_screen_still_yields_the_interior() {
        // A frame whose outer rect equals the whole grid must not be swallowed by the root.
        let mut g = Grid::new(20, 6);
        draw_box(&mut g, 0, 0, 5, 19);
        text(&mut g, 2, 3, "inside");
        text(&mut g, 3, 3, "more");
        let a = Engine::with_defaults().analyze(&g);
        let ladder = a.ladder_at(Pos { row: 2, col: 4 });
        let panel = ladder.iter().find(|t| t.kind == RegionKind::Panel).expect("panel step");
        assert_eq!(a.extract(panel), "inside\nmore");
        assert_eq!(ladder.last().unwrap().kind, RegionKind::Pane);
    }
}
