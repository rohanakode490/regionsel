//! Detectors: each looks for ONE kind of evidence and emits scored `Candidate`s.
//! They never talk to each other; `build_tree` reconciles them.
//!
//!   BackgroundRunDetector  rectangles of uniform background color  -> Block
//!   BorderDetector         closed rectangles of box-drawing glyphs -> Panel
//!   SeparatorDetector      reverse-video vertical dividers (vim splits) -> Pane
//!   IndentPrefixDetector   (refiner) blank-separated paragraphs, indented bodies -> Paragraph/Block

use std::collections::HashMap;

use crate::{Candidate, Cell, Color, Detector, Grid, Rect, Refiner, RegionKind, Signal};

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
            // lazygit-style scrollbar thumb drawn over a vertical border.
            0x2590 => U | D,
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

        // vspan[r][c]: how many steps an unbroken line runs DOWN from (r,c). Side edges of a
        // box must be unbroken, which anchors the rectangle.
        let mut vspan = vec![0u16; rows * cols];
        for c in 0..cols {
            for r in (0..rows - 1).rev() {
                if flags[r * cols + c] & D != 0 && flags[(r + 1) * cols + c] & U != 0 {
                    vspan[r * cols + c] = 1 + vspan[(r + 1) * cols + c];
                }
            }
        }
        // hard_top / hard_bot: prefix counts, per row, of cells that cannot be part of a top /
        // bottom horizontal edge. Titles are allowed on an edge (`╭─[1]─Status───╮`); these
        // are not:
        //  - a glyph with no horizontal direction (a bare `|` or `│`), and
        //  - a one-sided corner (`┐` `┌` ...) with a real line attached on the inside, because
        //    that is the border of a neighbouring box (`╮╭` between two panels), not a title.
        //    (`┬` `┴` `┼` are through-cells: those are legitimate table junctions.)
        let mut hard_top = vec![0u16; rows * (cols + 1)];
        let mut hard_bot = vec![0u16; rows * (cols + 1)];
        for r in 0..rows {
            for c in 0..cols {
                let f = flags[r * cols + c];
                let bare = f != 0 && f & (L | R) == 0;
                let one_sided = f & (L | R) != 0 && f & (L | R) != (L | R);
                let down_link = f & D != 0 && vspan[r * cols + c] > 0;
                let up_link = f & U != 0 && r > 0 && flags[(r - 1) * cols + c] & D != 0;
                let i = r * (cols + 1) + c;
                hard_top[i + 1] = hard_top[i] + (bare || (one_sided && down_link)) as u16;
                hard_bot[i + 1] = hard_bot[i] + (bare || (one_sided && up_link)) as u16;
            }
        }
        // blank[r][c]: the cell belongs to a run of non-glyph cells that is all whitespace.
        // Such a gap in an edge is damage, not a title (titles contain visible text).
        let mut blank = vec![0u16; rows * (cols + 1)];
        for r in 0..rows {
            let mut c = 0;
            while c < cols {
                let mut mark = false;
                let start = c;
                if flags[r * cols + c] == 0 {
                    while c < cols && flags[r * cols + c] == 0 {
                        c += 1;
                    }
                    mark = (start..c).all(|x| is_blank(grid.get(r as u16, x as u16)));
                } else {
                    c += 1;
                }
                for x in start..c {
                    blank[r * (cols + 1) + x + 1] = mark as u16;
                }
            }
            for x in 0..cols {
                blank[r * (cols + 1) + x + 1] += blank[r * (cols + 1) + x];
            }
        }
        // A horizontal edge from (r,c0) to (r,c1): corners point inward, the cells next to the
        // corners are plain line (so `┌title┐` without dashes is NOT accepted), and nothing
        // in between is a vertical glyph. Anything else in between is title text.
        let through = |r: usize, c: usize| flags[r * cols + c] & (L | R) == (L | R);
        let edge_ok = |r: usize, c0: usize, c1: usize, top: bool| {
            let hard = if top { &hard_top } else { &hard_bot };
            flags[r * cols + c0] & R != 0
                && flags[r * cols + c1] & L != 0
                && through(r, c0 + 1)
                && through(r, c1 - 1)
                && hard[r * (cols + 1) + c1] == hard[r * (cols + 1) + c0 + 1] // interior only
                && blank[r * (cols + 1) + c1 + 1] == blank[r * (cols + 1) + c0]
        };

        // Every rectangle whose side edges are unbroken lines and whose top and bottom edges
        // pass `edge_ok` (at least 3x3 so there is an interior).
        let mut rects: Vec<Rect> = Vec::new();
        'search: for r0 in 0..rows {
            for c0 in 0..cols {
                let vs = vspan[r0 * cols + c0] as usize;
                if vs < 2 || flags[r0 * cols + c0] & R == 0 {
                    continue;
                }
                for c1 in c0 + 2..cols {
                    // Passing a vertical glyph on the top row ends every longer edge too.
                    if hard_top[r0 * (cols + 1) + c1] != hard_top[r0 * (cols + 1) + c0 + 1] {
                        break;
                    }
                    let vs_right = vspan[r0 * cols + c1] as usize;
                    if vs_right < 2 || !edge_ok(r0, c0, c1, true) {
                        continue;
                    }
                    for r1 in r0 + 2..=r0 + vs.min(vs_right) {
                        if edge_ok(r1, c0, c1, false) {
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

// ───────────────────────── SeparatorDetector ─────────────────────────

/// Finds split panes divided by a vertical bar drawn in reverse video (vim / `:vsplit`).
///
/// Evidence, taken from a recorded vim screen: the divider is a `|` (not a box-drawing
/// glyph, so `BorderDetector` cannot see it) with the reverse attribute, one cell per row
/// for as long as the window is tall. Plain (non-reverse) bar columns are deliberately NOT
/// accepted: markdown tables and box side-edges look exactly like that.
///
/// Each separator yields the panes on both sides of it, limited to the rows of the divider
/// and bounded by the neighbouring separators that share rows. Horizontal splits and
/// status lines are not handled here (the status bar is picked up as a background block).
pub struct SeparatorDetector {
    pub min_rows: u16,
}

impl Default for SeparatorDetector {
    fn default() -> Self {
        SeparatorDetector { min_rows: 3 }
    }
}

fn is_vbar(ch: char) -> bool {
    matches!(ch, '|' | '│' | '┃' | '║' | '¦' | '┆' | '┊')
}

struct Sep {
    col: u16,
    r0: u16,
    r1: u16, // exclusive
}

impl Detector for SeparatorDetector {
    fn name(&self) -> &'static str {
        "separator"
    }

    fn detect(&self, grid: &Grid) -> Vec<Candidate> {
        let is_div = |r: u16, c: u16| {
            let cell = grid.get(r, c);
            cell.width > 0 && cell.attrs.reverse && is_vbar(cell.ch)
        };
        let mut seps: Vec<Sep> = Vec::new();
        for c in 0..grid.cols {
            let mut r = 0;
            while r < grid.rows {
                if !is_div(r, c) {
                    r += 1;
                    continue;
                }
                let start = r;
                while r < grid.rows && is_div(r, c) {
                    r += 1;
                }
                if r - start >= self.min_rows {
                    seps.push(Sep { col: c, r0: start, r1: r });
                }
            }
        }

        let overlaps = |a: &Sep, b: &Sep| a.r0 < b.r1 && b.r0 < a.r1;
        let mut rects: Vec<Rect> = Vec::new();
        for s in &seps {
            let left = seps
                .iter()
                .filter(|t| t.col < s.col && overlaps(s, t))
                .map(|t| t.col + 1)
                .max()
                .unwrap_or(0);
            let right =
                seps.iter().filter(|t| t.col > s.col && overlaps(s, t)).map(|t| t.col).min().unwrap_or(grid.cols);
            for (c0, c1) in [(left, s.col), (s.col + 1, right)] {
                let r = Rect { row0: s.r0, col0: c0, row1: s.r1, col1: c1 };
                if c1 > c0 && ink_rows(grid, r).is_some() && !rects.contains(&r) {
                    rects.push(r);
                }
            }
        }
        rects
            .into_iter()
            .map(|r| Candidate {
                kind: RegionKind::Pane,
                outer: r,
                content: ink_rows(grid, r).unwrap_or(r),
                gutter: None,
                confidence: 0.8,
                signal: Signal::Separator,
            })
            .collect()
    }
}

// ───────────────────────── IndentPrefixDetector ─────────────────────────

/// Second-phase detector: finds text structure inside an area, from layout alone.
///
/// Evidence, from a recorded Claude Code screen: a response is a body indented by 2 columns
/// under rows at column 0 (the `❯` prompt, the `✻ Worked for` line, the input-box rules), with
/// blank rows between paragraphs. The same shape is `git log` (indented commit message under
/// a `commit` line). Two signals, both free of app-specific glyphs:
///
///  1. Paragraph: a run of 2+ non-blank rows between blank rows (only when the area has more
///     than one such run; a single run is the area itself).
///  2. Block: a run of rows indented at least `min_indent_delta` columns deeper than the
///     area's left margin (its least-indented row), blank rows allowed inside, 2+ text rows.
///
/// Prefix markers (`⏺`, `>`) are NOT handled: the capture had no bullet on response bodies,
/// so there is no evidence to build on yet.
pub struct IndentPrefixDetector {
    pub min_indent_delta: u16,
}

impl Default for IndentPrefixDetector {
    fn default() -> Self {
        IndentPrefixDetector { min_indent_delta: 2 }
    }
}

impl Refiner for IndentPrefixDetector {
    fn name(&self) -> &'static str {
        "indent-prefix"
    }

    fn refine(&self, grid: &Grid, area: Rect) -> Vec<Candidate> {
        if area.row1 <= area.row0 || area.col1 <= area.col0 {
            return Vec::new();
        }
        // indent[i] = columns before the first ink on row area.row0 + i, None if blank.
        let indent: Vec<Option<u16>> = (area.row0..area.row1)
            .map(|r| {
                (area.col0..area.col1)
                    .find(|&c| {
                        let cell = grid.get(r, c);
                        cell.width > 0 && !is_blank(cell)
                    })
                    .map(|c| c - area.col0)
            })
            .collect();
        let rect = |i0: usize, i1: usize| Rect {
            row0: area.row0 + i0 as u16,
            col0: area.col0,
            row1: area.row0 + i1 as u16,
            col1: area.col1,
        };
        let mut out = Vec::new();

        // 1. Paragraphs.
        let mut runs: Vec<(usize, usize)> = Vec::new();
        let mut i = 0;
        while i < indent.len() {
            if indent[i].is_none() {
                i += 1;
                continue;
            }
            let start = i;
            while i < indent.len() && indent[i].is_some() {
                i += 1;
            }
            runs.push((start, i));
        }
        if runs.len() >= 2 {
            for &(a, b) in runs.iter().filter(|&&(a, b)| b - a >= 2) {
                out.push(Candidate {
                    kind: RegionKind::Paragraph,
                    outer: rect(a, b),
                    content: rect(a, b),
                    gutter: None,
                    confidence: 0.6,
                    signal: Signal::Indent,
                });
            }
        }

        // 2. Indented bodies.
        let Some(margin) = indent.iter().flatten().copied().min() else { return out };
        let is_body = |i: usize| indent[i].is_some_and(|n| n >= margin + self.min_indent_delta);
        let mut i = 0;
        while i < indent.len() {
            if !is_body(i) {
                i += 1;
                continue;
            }
            let (start, mut last, mut rows) = (i, i, 0);
            while i < indent.len() && (is_body(i) || indent[i].is_none()) {
                if is_body(i) {
                    last = i;
                    rows += 1;
                }
                i += 1;
            }
            if rows >= 2 {
                out.push(Candidate {
                    kind: RegionKind::Block,
                    outer: rect(start, last + 1),
                    content: rect(start, last + 1),
                    gutter: None,
                    confidence: 0.65,
                    signal: Signal::Indent,
                });
            }
        }
        out
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
    fn titled_top_and_bottom_borders_are_boxes() {
        let mut g = Grid::new(30, 6);
        draw_box(&mut g, 1, 2, 4, 25);
        text(&mut g, 1, 5, " pods(default)[3] "); // title replaces part of the top edge
        text(&mut g, 4, 18, "2 of 4"); // status on the bottom edge
        text(&mut g, 2, 4, "row");
        let cands = BorderDetector::default().detect(&g);
        assert_eq!(cands.len(), 1, "{cands:?}");
        assert_eq!(cands[0].outer, rect(1, 2, 5, 26));
        assert_eq!(cands[0].content, rect(2, 3, 3, 25));
    }

    #[test]
    fn two_titled_panels_side_by_side() {
        // lazygit-style: `╮╭` touching, each with a `[n]-Title` in its top edge.
        let mut g = Grid::new(30, 5);
        draw_box(&mut g, 0, 0, 4, 11);
        draw_box(&mut g, 0, 12, 4, 29);
        text(&mut g, 0, 2, "[1]-A");
        text(&mut g, 0, 14, "[0]-Unstaged");
        let mut rects: Vec<Rect> = BorderDetector::default().detect(&g).iter().map(|c| c.outer).collect();
        rects.sort_by_key(|r| r.col0);
        assert_eq!(rects, vec![rect(0, 0, 5, 12), rect(0, 12, 5, 30)]);
    }

    #[test]
    fn scrollbar_thumb_on_a_side_border_is_still_a_box() {
        let mut g = Grid::new(20, 6);
        draw_box(&mut g, 0, 0, 5, 19);
        put(&mut g, 2, 19, '▐');
        put(&mut g, 3, 19, '▐');
        assert_eq!(BorderDetector::default().detect(&g).len(), 1);
    }

    #[test]
    fn title_must_be_flanked_by_line_and_corners_need_real_sides() {
        // No dash next to the corner: not accepted (too easy to confuse with text).
        let mut g = Grid::new(20, 5);
        draw_box(&mut g, 0, 0, 4, 19);
        text(&mut g, 0, 1, "title");
        assert!(BorderDetector::default().detect(&g).is_empty());
        // Corners on a text row with no sides below: nothing.
        let mut g = Grid::new(30, 5);
        text(&mut g, 1, 0, "┌─ just some text ─┐");
        assert!(BorderDetector::default().detect(&g).is_empty());
        // A vertical glyph inside the top edge splits it: no box spanning over it.
        let mut g = Grid::new(30, 6);
        draw_box(&mut g, 1, 2, 4, 25);
        put(&mut g, 1, 10, '│');
        assert!(BorderDetector::default().detect(&g).is_empty());
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

    // ── indent / paragraph refiner ──

    fn grid_lines(lines: &[&str], cols: u16) -> Grid {
        let mut g = Grid::new(cols, lines.len() as u16);
        for (r, l) in lines.iter().enumerate() {
            text(&mut g, r as u16, 0, l);
        }
        g
    }

    #[test]
    fn paragraphs_are_blank_separated_runs_of_two_or_more_rows() {
        let g = grid_lines(&["one a", "one b", "", "single", "", "three a", "three b", "three c"], 20);
        let c = IndentPrefixDetector::default().refine(&g, g.bounds());
        let kinds: Vec<_> = c.iter().map(|c| (c.kind, c.outer)).collect();
        assert_eq!(kinds, vec![(RegionKind::Paragraph, rect(0, 0, 2, 20)), (RegionKind::Paragraph, rect(5, 0, 8, 20))]);
    }

    #[test]
    fn a_single_run_is_not_a_paragraph_split() {
        let g = grid_lines(&["a", "b", "c"], 10);
        assert!(IndentPrefixDetector::default().refine(&g, g.bounds()).is_empty());
    }

    #[test]
    fn indented_body_under_margin_rows_is_a_block() {
        let g = grid_lines(&["> prompt", "", "  first para", "  more", "", "  second para", "", "* footer"], 20);
        let c = IndentPrefixDetector::default().refine(&g, g.bounds());
        let blocks: Vec<Rect> = c.iter().filter(|c| c.kind == RegionKind::Block).map(|c| c.outer).collect();
        assert_eq!(blocks, vec![rect(2, 0, 6, 20)]); // trailing blank row excluded
    }

    #[test]
    fn uniform_indent_has_no_margin_to_contrast_against() {
        let g = grid_lines(&["  a", "  b", "", "  c", "  d"], 20);
        let c = IndentPrefixDetector::default().refine(&g, g.bounds());
        assert!(c.iter().all(|c| c.kind != RegionKind::Block));
    }

    #[test]
    fn engine_ladder_word_line_paragraph_block_pane_and_unwrap_in_block() {
        // width 30. "  3. aaa bbb ccc ddd eee fff" is 29 wide, so "ggg" was wrapped onto the next row.
        let g = grid_lines(
            &["> question", "", "  1. short item", "  2. a b", "  more words", "", "  3. aaa bbb ccc ddd eee fff", "  ggg end", "", "done"],
            30,
        );
        let a = Engine::with_defaults().analyze(&g);
        let ladder = a.ladder_at(Pos { row: 3, col: 5 });
        let kinds: Vec<_> = ladder.iter().map(|t| t.kind).collect();
        assert_eq!(
            kinds,
            vec![RegionKind::Word, RegionKind::Line, RegionKind::Paragraph, RegionKind::Block, RegionKind::Pane]
        );
        assert_eq!(a.extract(&ladder[2]), "1. short item\n2. a b\nmore words");
        let block = a.extract(&ladder[3]);
        assert!(block.contains("3. aaa bbb ccc ddd eee fff ggg end"), "{block}"); // dedented, then unwrapped
        assert!(!block.contains("> question") && !block.contains("done"));
    }

    // ── separator ──

    fn vsep(g: &mut Grid, col: u16, r0: u16, r1: u16, reverse: bool) {
        for r in r0..r1 {
            let mut cell = *g.get(r, col);
            cell.ch = '|';
            cell.attrs.reverse = reverse;
            g.set(r, col, cell);
        }
    }

    #[test]
    fn reverse_video_bar_splits_two_panes() {
        let mut g = Grid::new(30, 8);
        vsep(&mut g, 14, 0, 7, true);
        text(&mut g, 1, 1, "left text");
        text(&mut g, 1, 16, "right text");
        let mut rects: Vec<Rect> = SeparatorDetector::default().detect(&g).iter().map(|c| c.outer).collect();
        rects.sort_by_key(|r| r.col0);
        assert_eq!(rects, vec![rect(0, 0, 7, 14), rect(0, 15, 7, 30)]);
    }

    #[test]
    fn three_panes_dedupe_the_middle_one() {
        let mut g = Grid::new(40, 6);
        vsep(&mut g, 12, 0, 5, true);
        vsep(&mut g, 26, 0, 5, true);
        for (c, w) in [(1, "a"), (14, "b"), (28, "c")] {
            text(&mut g, 1, c, w);
        }
        let mut rects: Vec<Rect> = SeparatorDetector::default().detect(&g).iter().map(|c| c.outer).collect();
        rects.sort_by_key(|r| r.col0);
        assert_eq!(rects, vec![rect(0, 0, 5, 12), rect(0, 13, 5, 26), rect(0, 27, 5, 40)]);
    }

    #[test]
    fn plain_bars_and_short_runs_are_not_separators() {
        let mut g = Grid::new(30, 8);
        text(&mut g, 1, 1, "x");
        vsep(&mut g, 14, 0, 7, false); // markdown-table-like column: no reverse video
        assert!(SeparatorDetector::default().detect(&g).is_empty());
        let mut g = Grid::new(30, 8);
        text(&mut g, 1, 1, "x");
        vsep(&mut g, 14, 2, 4, true); // only 2 rows
        assert!(SeparatorDetector::default().detect(&g).is_empty());
    }

    #[test]
    fn line_and_ladder_stay_inside_their_split_pane() {
        let mut g = Grid::new(30, 8);
        vsep(&mut g, 14, 0, 7, true);
        text(&mut g, 2, 1, "left one");
        text(&mut g, 3, 1, "left two");
        text(&mut g, 2, 16, "right one");
        let a = Engine::with_defaults().analyze(&g);
        let ladder = a.ladder_at(Pos { row: 2, col: 3 });
        let kinds: Vec<_> = ladder.iter().map(|t| t.kind).collect();
        assert_eq!(kinds, vec![RegionKind::Word, RegionKind::Line, RegionKind::Pane, RegionKind::Pane]);
        assert_eq!(a.extract(&ladder[1]), "left one"); // not "left one|right one"
        assert_eq!(a.extract(&ladder[2]), "left one\nleft two"); // nothing from the right pane
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
