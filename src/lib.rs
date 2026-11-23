//! regionsel-core: interface sketch (single file for readability; split into modules later)
//!
//! Contract: Grid in -> RegionTree out -> clean text out.
//! No I/O, no clipboard, no keys, no tmux. Frontends own all of that.
//!
//! Pipeline:  Grid --Detectors--> Candidates --build_tree--> RegionTree
//!            RegionTree + Pos --ladder_at--> [Word, Line, Region...] --extract--> String
//!            Session: Key -> Action state machine (cursor, expand/shrink, visual modes, copy)

pub mod ansi;
pub mod detect;
pub mod view;

// ───────────────────────── 1. Input model ─────────────────────────

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Default)]
pub enum Color {
    #[default]
    Default,
    Indexed(u8),
    Rgb(u8, u8, u8),
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct Attrs {
    pub bold: bool,
    pub dim: bool,
    pub italic: bool,
    pub underline: bool,
    pub reverse: bool,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Cell {
    pub ch: char,
    /// 1 = normal, 2 = wide (CJK/emoji), 0 = continuation of a wide char (skipped on extract)
    pub width: u8,
    pub fg: Color,
    pub bg: Color,
    pub attrs: Attrs,
}

impl Default for Cell {
    fn default() -> Self {
        Cell { ch: ' ', width: 1, fg: Color::Default, bg: Color::Default, attrs: Attrs::default() }
    }
}

pub struct Grid {
    pub cols: u16,
    pub rows: u16,
    cells: Vec<Cell>, // row-major
}

impl Grid {
    pub fn new(cols: u16, rows: u16) -> Self {
        Grid { cols, rows, cells: vec![Cell::default(); cols as usize * rows as usize] }
    }
    pub fn get(&self, row: u16, col: u16) -> &Cell {
        &self.cells[row as usize * self.cols as usize + col as usize]
    }
    pub fn set(&mut self, row: u16, col: u16, cell: Cell) {
        let i = row as usize * self.cols as usize + col as usize;
        self.cells[i] = cell;
    }
    pub fn bounds(&self) -> Rect {
        Rect { row0: 0, col0: 0, row1: self.rows, col1: self.cols }
    }
    // Building a Grid from `tmux capture-pane -p -e` output (ANSI SGR) or from a
    // `vt100` screen lives in the frontend crate, not here:
    //   pub fn from_ansi(bytes: &[u8], cols: u16, rows: u16) -> Grid
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Pos {
    pub row: u16,
    pub col: u16,
}

/// Half-open rectangle: rows row0..row1, cols col0..col1.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Rect {
    pub row0: u16,
    pub col0: u16,
    pub row1: u16,
    pub col1: u16,
}

impl Rect {
    pub fn width(&self) -> u16 { self.col1 - self.col0 }
    pub fn area(&self) -> u32 { (self.row1 - self.row0) as u32 * self.width() as u32 }
    pub fn contains(&self, row: u16, col: u16) -> bool {
        row >= self.row0 && row < self.row1 && col >= self.col0 && col < self.col1
    }
    pub fn contains_rect(&self, o: &Rect) -> bool {
        o.row0 >= self.row0 && o.row1 <= self.row1 && o.col0 >= self.col0 && o.col1 <= self.col1
    }
    pub fn intersects(&self, o: &Rect) -> bool {
        self.row0 < o.row1 && o.row0 < self.row1 && self.col0 < o.col1 && o.col0 < self.col1
    }
    pub fn partially_overlaps(&self, o: &Rect) -> bool {
        self.intersects(o) && !self.contains_rect(o) && !o.contains_rect(self)
    }
    /// A real conflict: partial overlap AND the overlap is at least 2x2. Panels sharing a
    /// single border line (tmux splits, table cells) are NOT in conflict.
    pub fn crosses(&self, o: &Rect) -> bool {
        if !self.partially_overlaps(o) {
            return false;
        }
        let h = self.row1.min(o.row1) - self.row0.max(o.row0);
        let w = self.col1.min(o.col1) - self.col0.max(o.col0);
        h >= 2 && w >= 2
    }
}

// ───────────────────────── 2. Region model ─────────────────────────

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum RegionKind {
    Word,      // synthesized on demand, never stored in the tree
    Line,      // synthesized on demand
    Paragraph, // run of non-blank lines inside a region
    Code,      // monospace/indented/fenced block, never unwrapped
    Block,     // background-colored or prefix-led block (e.g. one chat message)
    Panel,     // bordered rectangle
    Pane,      // whole grid, or a split pane
}

/// Why we believe a region exists. Kept for debugging and for tuning confidence.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Signal {
    BoxBorder,
    BackgroundRun,
    Separator,
    Indent,
    Prefix,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct RegionId(pub usize);

#[derive(Clone, Debug)]
pub struct Region {
    pub id: RegionId,
    pub kind: RegionKind,
    /// Full extent including borders/padding.
    pub outer: Rect,
    /// What you actually want to copy: outer minus borders, padding and gutter.
    pub content: Rect,
    /// Line numbers, diff +/- column, bullet column. Excluded from `content` by default.
    pub gutter: Option<Rect>,
    pub parent: Option<RegionId>,
    pub children: Vec<RegionId>,
    pub confidence: f32,
    pub signals: Vec<Signal>,
}

pub struct RegionTree {
    regions: Vec<Region>, // index 0 is the root Pane
}

impl RegionTree {
    pub fn get(&self, id: RegionId) -> &Region { &self.regions[id.0] }
    pub fn regions(&self) -> &[Region] { &self.regions }

    /// Innermost-first chain of regions containing `p` (the "expansion ladder" backbone).
    pub fn chain_at(&self, p: Pos) -> Vec<RegionId> {
        let mut chain = Vec::new();
        let mut cur = RegionId(0);
        if !self.get(cur).outer.contains(p.row, p.col) {
            return chain;
        }
        loop {
            chain.push(cur);
            let next = self.get(cur).children.iter().copied()
                .find(|&c| self.get(c).outer.contains(p.row, p.col));
            match next {
                Some(c) => cur = c,
                None => break,
            }
        }
        chain.reverse();
        chain
    }

    fn deepest_containing(&self, r: &Rect) -> RegionId {
        let mut cur = RegionId(0);
        while let Some(c) = self.get(cur).children.iter().copied()
            .find(|&c| self.get(c).outer.contains_rect(r))
        {
            cur = c;
        }
        cur
    }
}

// ───────────────────────── 3. Detection ─────────────────────────

/// One weak opinion about "there is a region here".
#[derive(Clone, Debug)]
pub struct Candidate {
    pub kind: RegionKind,
    pub outer: Rect,
    pub content: Rect,
    pub gutter: Option<Rect>,
    pub confidence: f32, // 0.0..=1.0
    pub signal: Signal,
}

/// Each detector looks for ONE kind of evidence. Keep them independent and individually testable.
pub trait Detector: Send + Sync {
    fn name(&self) -> &'static str;
    fn detect(&self, grid: &Grid) -> Vec<Candidate>;
}

/// Second-phase detector: looks for structure INSIDE an area that phase one already found
/// (a panel's interior, or the whole screen). Splitting text into paragraphs needs to know
/// where the panels are, otherwise a paragraph would cut through one.
pub trait Refiner: Send + Sync {
    fn name(&self) -> &'static str;
    fn refine(&self, grid: &Grid, area: Rect) -> Vec<Candidate>;
}

// The detectors live in `detect.rs`.
pub use detect::{BackgroundRunDetector, BorderDetector, IndentPrefixDetector, SeparatorDetector};



/// Merge candidates into a containment tree. Larger first; partial overlaps lose to
/// whatever was inserted first (higher confidence among equal areas).
pub fn build_tree(root: Rect, mut cands: Vec<Candidate>, min_conf: f32) -> RegionTree {
    let mut tree = RegionTree {
        regions: vec![Region {
            id: RegionId(0),
            kind: RegionKind::Pane,
            outer: root,
            content: root,
            gutter: None,
            parent: None,
            children: vec![],
            confidence: 1.0,
            signals: vec![],
        }],
    };
    cands.retain(|c| c.confidence >= min_conf);
    cands.sort_by(|a, b| {
        b.outer.area().cmp(&a.outer.area()).then(b.confidence.total_cmp(&a.confidence))
    });
    for c in cands {
        if tree.regions.iter().any(|r| r.outer.crosses(&c.outer)) {
            continue;
        }
        let parent = tree.deepest_containing(&c.outer);
        let p = tree.get(parent);
        if p.outer == c.outer && p.content == c.content {
            continue; // exact duplicate of what is already there
        }
        let id = RegionId(tree.regions.len());
        tree.regions.push(Region {
            id,
            kind: c.kind,
            outer: c.outer,
            content: c.content,
            gutter: c.gutter,
            parent: Some(parent),
            children: vec![],
            confidence: c.confidence,
            signals: vec![c.signal],
        });
        tree.regions[parent.0].children.push(id);
    }
    tree
}

// ───────────────────────── 4. Extraction ─────────────────────────

#[derive(Clone, Debug)]
pub struct ExtractOptions {
    pub trim_trailing_ws: bool,
    pub dedent: bool,
    pub unwrap: bool, // rejoin soft-wrapped lines (prose only)
}

impl ExtractOptions {
    pub fn for_kind(kind: RegionKind) -> Self {
        match kind {
            // A single line has no relative indentation to preserve: strip its padding.
            RegionKind::Word | RegionKind::Line => {
                ExtractOptions { trim_trailing_ws: true, dedent: true, unwrap: false }
            }
            // Code keeps its indentation (relative structure matters).
            RegionKind::Code => ExtractOptions { trim_trailing_ws: true, dedent: false, unwrap: false },
            _ => ExtractOptions { trim_trailing_ws: true, dedent: true, unwrap: true },
        }
    }
}

pub fn extract(grid: &Grid, rect: Rect, opts: &ExtractOptions) -> String {
    let mut lines: Vec<String> = (rect.row0..rect.row1)
        .map(|r| {
            let mut s = String::new();
            for c in rect.col0..rect.col1 {
                let cell = grid.get(r, c);
                if cell.width > 0 {
                    s.push(cell.ch);
                }
            }
            if opts.trim_trailing_ws { s.trim_end().to_string() } else { s }
        })
        .collect();
    // The wrapper that produced the soft wraps had the width MINUS the indent we strip.
    let removed = if opts.dedent { dedent(&mut lines) } else { 0 };
    if opts.unwrap {
        lines = unwrap_soft_wraps(lines, rect.width().saturating_sub(removed));
    }
    lines.join("\n")
}

/// Remove the common leading whitespace and return how many columns were removed. Counted in
/// chars, never bytes, so a multibyte character can't be split.
fn dedent(lines: &mut [String]) -> u16 {
    let lead = |l: &str| l.chars().take_while(|c| c.is_whitespace()).count();
    let indent = lines.iter().filter(|l| !l.trim().is_empty()).map(|l| lead(l)).min().unwrap_or(0);
    if indent > 0 {
        for l in lines.iter_mut().filter(|l| !l.trim().is_empty()) {
            *l = l.chars().skip(indent).collect();
        }
    }
    indent as u16
}

/// Heuristic: `prev` was soft-wrapped onto `next` if a word-wrapper would have had to break
/// there, i.e. the first word of `next` would not have fit on `prev`. That is a much sharper
/// signal than "the line is nearly full", which also fires on every row of a table.
/// The terminal's own wrap flag is useless here: TUIs repaint with absolute cursor moves.
///
/// Guards against false joins: neither line may contain a column-alignment gap (2+ spaces),
/// `prev` must be at least half the width and hold more than one word (a column of long paths
/// is not prose), and `next` must not start a new block (blank, indented, list item).
fn unwrap_soft_wraps(lines: Vec<String>, width: u16) -> Vec<String> {
    let width = width as usize;
    let mut out: Vec<String> = Vec::new();
    // The previous PHYSICAL line (before any joining): that is what the wrapper saw.
    let mut prev_phys: Option<String> = None;
    for line in lines {
        let join = match prev_phys.as_ref().filter(|_| !out.is_empty()) {
            Some(prev) => {
                let starts_block = line.is_empty()
                    || line.starts_with(char::is_whitespace)
                    || looks_like_list_item(&line);
                let first_word = line.split_whitespace().next().map_or(0, |w| w.chars().count());
                let prev_len = prev.chars().count();
                !starts_block
                    && prev.contains(' ')
                    && prev_len * 2 >= width
                    && prev_len + 1 + first_word > width
                    && !has_alignment_gap(prev)
                    && !has_alignment_gap(&line)
            }
            None => false,
        };
        if join {
            let last = out.last_mut().unwrap();
            last.push(' ');
            last.push_str(&line);
        } else {
            out.push(line.clone());
        }
        prev_phys = Some(line);
    }
    out
}

/// Two or more consecutive spaces inside a line: column alignment, not prose.
fn has_alignment_gap(s: &str) -> bool {
    s.trim().contains("  ")
}

fn looks_like_list_item(s: &str) -> bool {
    s.starts_with("- ") || s.starts_with("* ") || s.starts_with("• ")
        || s.split_once(". ").map_or(false, |(n, _)| !n.is_empty() && n.chars().all(|c| c.is_ascii_digit()))
}

// ───────────────────────── 5. Facade ─────────────────────────

#[derive(Clone, Debug)]
pub struct Target {
    pub kind: RegionKind,
    pub rect: Rect,
    pub region: Option<RegionId>, // None for Word/Line
}

pub struct Engine {
    detectors: Vec<Box<dyn Detector>>,
    refiners: Vec<Box<dyn Refiner>>,
    pub min_confidence: f32,
}

impl Engine {
    pub fn with_defaults() -> Self {
        Engine {
            detectors: vec![
                Box::new(BorderDetector::default()),
                Box::new(BackgroundRunDetector::default()),
                Box::new(SeparatorDetector::default()),
            ],
            refiners: vec![Box::new(IndentPrefixDetector::default())],
            min_confidence: 0.5,
        }
    }

    pub fn analyze<'g>(&self, grid: &'g Grid) -> Analysis<'g> {
        let mut cands: Vec<Candidate> = self.detectors.iter().flat_map(|d| d.detect(grid)).collect();
        let first = build_tree(grid.bounds(), cands.clone(), self.min_confidence);
        // Phase two: refine inside every region. A refined candidate must not partially
        // overlap (cross) any region from phase one: containing or avoiding one is fine,
        // cutting through one is not, so a paragraph can never destroy a panel.
        let mut extra: Vec<Candidate> = Vec::new();
        for region in first.regions() {
            for refiner in &self.refiners {
                extra.extend(
                    refiner
                        .refine(grid, region.content)
                        .into_iter()
                        .filter(|c| !first.regions().iter().any(|r| r.outer.crosses(&c.outer))),
                );
            }
        }
        cands.extend(extra);
        Analysis { grid, tree: build_tree(grid.bounds(), cands, self.min_confidence) }
    }
}

pub struct Analysis<'g> {
    pub grid: &'g Grid,
    pub tree: RegionTree,
}

impl Analysis<'_> {
    /// The expansion ladder at a point: word -> line -> innermost region -> ... -> pane.
    /// Consecutive duplicates (same rect) are collapsed so every step grows the selection.
    pub fn ladder_at(&self, p: Pos) -> Vec<Target> {
        let chain = self.tree.chain_at(p);
        let Some(&innermost) = chain.first() else { return vec![] };
        let mut out: Vec<Target> = Vec::new();
        let push = |t: Target, out: &mut Vec<Target>| {
            if out.last().map_or(true, |l| l.rect != t.rect) {
                out.push(t);
            }
        };

        let bounds = self.tree.get(innermost).content;
        if bounds.contains(p.row, p.col) {
            if let Some(w) = word_rect(self.grid, p, bounds) {
                push(Target { kind: RegionKind::Word, rect: w, region: None }, &mut out);
            }
            let line = Rect { row0: p.row, row1: p.row + 1, col0: bounds.col0, col1: bounds.col1 };
            push(Target { kind: RegionKind::Line, rect: line, region: None }, &mut out);
        }
        for id in chain {
            let r = self.tree.get(id);
            push(Target { kind: r.kind, rect: r.content, region: Some(id) }, &mut out);
        }
        out
    }

    pub fn extract(&self, t: &Target) -> String {
        extract(self.grid, t.rect, &ExtractOptions::for_kind(t.kind))
    }
}

fn word_rect(grid: &Grid, p: Pos, bounds: Rect) -> Option<Rect> {
    let is_word = |c: u16| !grid.get(p.row, c).ch.is_whitespace();
    if !bounds.contains(p.row, p.col) || !is_word(p.col) {
        return None;
    }
    let mut l = p.col;
    while l > bounds.col0 && is_word(l - 1) {
        l -= 1;
    }
    let mut r = p.col + 1;
    while r < bounds.col1 && is_word(r) {
        r += 1;
    }
    Some(Rect { row0: p.row, row1: p.row + 1, col0: l, col1: r })
}

// ───────────────────────── 6. Selection state machine ─────────────────────────
//
// Pure logic, like the rest: abstract keys in, Action out. The frontend maps its own
// bindings (h/j/k/l, Space, Enter, ...) to `Key` and draws `highlight()` rects in
// reverse video. Nothing here knows about tmux, termios or the clipboard.
//
// Modes:
//   Semantic (default)  Space expands along the ladder, Backspace shrinks.
//   Char / Line / Rect  Manual vim-style visual modes; moving the cursor extends from
//                       the anchor. These are literal (no unwrap/dedent), the escape
//                       hatch for when the heuristics guess wrong.

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Key {
    Left, Down, Up, Right, // h j k l
    WordFwd, WordBack,     // w b
    LineStart, LineEnd,    // 0 $
    Top, Bottom,           // gg G
    Expand, Shrink,        // Space, Backspace
    VisualChar,            // v   (toggle)
    VisualLine,            // V   (toggle)
    VisualRect,            // Ctrl-v (toggle)
    Copy,                  // Enter
    Cancel,                // Esc / q
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Mode {
    Semantic,
    Char,
    Line,
    Rect,
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Action {
    /// Nothing changed; no need to redraw.
    None,
    /// Cursor or highlight changed.
    Redraw,
    /// Put this text on the clipboard, then exit.
    Copy(String),
    /// Exit without copying.
    Cancel,
}

pub struct Session<'a, 'g> {
    analysis: &'a Analysis<'g>,
    cursor: Pos,
    mode: Mode,
    anchor: Option<Pos>,
    ladder: Vec<Target>,
    level: Option<usize>, // None = no semantic selection yet
}

impl<'a, 'g> Session<'a, 'g> {
    pub fn new(analysis: &'a Analysis<'g>, start: Pos) -> Self {
        let mut s = Session {
            analysis,
            cursor: start,
            mode: Mode::Semantic,
            anchor: None,
            ladder: vec![],
            level: None,
        };
        s.set_cursor(start);
        s
    }

    pub fn cursor(&self) -> Pos { self.cursor }
    pub fn mode(&self) -> Mode { self.mode }

    pub fn handle(&mut self, key: Key) -> Action {
        let g = self.analysis.grid;
        let last_row = g.rows.saturating_sub(1);
        let last_col = g.cols.saturating_sub(1);
        let c = self.cursor;
        match key {
            Key::Left => self.move_to(Pos { row: c.row, col: c.col.saturating_sub(1) }),
            Key::Right => self.move_to(Pos { row: c.row, col: (c.col + 1).min(last_col) }),
            Key::Up => self.move_to(Pos { row: c.row.saturating_sub(1), col: c.col }),
            Key::Down => self.move_to(Pos { row: (c.row + 1).min(last_row), col: c.col }),
            Key::Top => self.move_to(Pos { row: 0, col: c.col }),
            Key::Bottom => self.move_to(Pos { row: last_row, col: c.col }),
            Key::WordFwd => self.move_to(word_fwd(g, c)),
            Key::WordBack => self.move_to(word_back(g, c)),
            Key::LineStart => {
                let b = self.bounds_at(c);
                self.move_to(Pos { row: c.row, col: b.col0 })
            }
            Key::LineEnd => {
                let b = self.bounds_at(c);
                let col = (b.col0..b.col1)
                    .rev()
                    .find(|&x| !g.get(c.row, x).ch.is_whitespace())
                    .unwrap_or(b.col1.saturating_sub(1));
                self.move_to(Pos { row: c.row, col })
            }
            Key::Expand => {
                if self.mode != Mode::Semantic || self.ladder.is_empty() {
                    return Action::None;
                }
                let next = self.level.map_or(0, |i| (i + 1).min(self.ladder.len() - 1));
                self.level = Some(next);
                Action::Redraw
            }
            Key::Shrink => {
                if self.mode != Mode::Semantic {
                    return Action::None;
                }
                self.level = match self.level {
                    None | Some(0) => None,
                    Some(i) => Some(i - 1),
                };
                Action::Redraw
            }
            Key::VisualChar => self.toggle_mode(Mode::Char),
            Key::VisualLine => self.toggle_mode(Mode::Line),
            Key::VisualRect => self.toggle_mode(Mode::Rect),
            Key::Copy => Action::Copy(self.selected_text()),
            Key::Cancel => Action::Cancel,
        }
    }

    /// A mouse click. In Semantic mode it GROWS the selection one level when it lands inside
    /// the current highlight (and it can still grow), and otherwise selects the smallest thing under the pointer
    /// (so one click picks a word, a second click on it picks the line, and so on). In the
    /// manual visual modes it just moves the cursor, extending the selection.
    /// Clicks outside the grid are ignored.
    pub fn click(&mut self, p: Pos) -> Action {
        let g = self.analysis.grid;
        if p.row >= g.rows || p.col >= g.cols {
            return Action::None;
        }
        if self.mode != Mode::Semantic {
            return self.move_to(p);
        }
        if let Some(i) = self.level {
            // Grow while there is something bigger; at the top of the ladder a click starts over.
            if i + 1 < self.ladder.len() && self.ladder[i].rect.contains(p.row, p.col) {
                return self.handle(Key::Expand);
            }
        }
        self.set_cursor(p);
        self.level = if self.ladder.is_empty() { None } else { Some(0) };
        Action::Redraw
    }

    /// Rectangles to draw as selected (reverse video). Empty = just show the cursor.
    pub fn highlight(&self) -> Vec<Rect> {
        let c = self.cursor;
        let a = self.anchor.unwrap_or(c);
        match self.mode {
            Mode::Semantic => self.level.map(|i| vec![self.ladder[i].rect]).unwrap_or_default(),
            Mode::Rect => vec![Rect {
                row0: a.row.min(c.row),
                row1: a.row.max(c.row) + 1,
                col0: a.col.min(c.col),
                col1: a.col.max(c.col) + 1,
            }],
            Mode::Line => {
                let b = self.bounds_at(c);
                vec![Rect {
                    row0: a.row.min(c.row),
                    row1: a.row.max(c.row) + 1,
                    col0: b.col0,
                    col1: b.col1,
                }]
            }
            Mode::Char => {
                let (s, e) = if (a.row, a.col) <= (c.row, c.col) { (a, c) } else { (c, a) };
                let cols = self.analysis.grid.cols;
                (s.row..=e.row)
                    .map(|r| Rect {
                        row0: r,
                        row1: r + 1,
                        col0: if r == s.row { s.col } else { 0 },
                        col1: if r == e.row { e.col + 1 } else { cols },
                    })
                    .collect()
            }
        }
    }

    /// Text for Enter. With no selection yet in Semantic mode, copies the cursor's line,
    /// so `open, move to the line, Enter` behaves like a quick yank.
    pub fn selected_text(&self) -> String {
        match (self.mode, self.level) {
            (Mode::Semantic, Some(i)) => self.analysis.extract(&self.ladder[i]),
            (Mode::Semantic, None) => self
                .ladder
                .iter()
                .find(|t| t.kind == RegionKind::Line)
                .map(|t| self.analysis.extract(t))
                .unwrap_or_default(),
            _ => {
                let raw = ExtractOptions { trim_trailing_ws: true, dedent: false, unwrap: false };
                self.highlight()
                    .iter()
                    .map(|r| extract(self.analysis.grid, *r, &raw))
                    .collect::<Vec<_>>()
                    .join("\n")
            }
        }
    }

    /// Short text for a status line, e.g. "SELECT Block (3/5)".
    pub fn status(&self) -> String {
        match (self.mode, self.level) {
            (Mode::Semantic, Some(i)) => {
                format!("SELECT {:?} ({}/{})", self.ladder[i].kind, i + 1, self.ladder.len())
            }
            (Mode::Semantic, None) => "NORMAL".to_string(),
            (Mode::Char, _) => "VISUAL".to_string(),
            (Mode::Line, _) => "V-LINE".to_string(),
            (Mode::Rect, _) => "V-BLOCK".to_string(),
        }
    }

    // ── internals ──

    fn move_to(&mut self, p: Pos) -> Action {
        self.set_cursor(p);
        Action::Redraw
    }

    /// Moving resets the semantic ladder but keeps a manual anchor.
    fn set_cursor(&mut self, p: Pos) {
        self.cursor = p;
        self.ladder = self.analysis.ladder_at(p);
        self.level = None;
    }

    fn toggle_mode(&mut self, m: Mode) -> Action {
        if self.mode == m {
            self.mode = Mode::Semantic;
            self.anchor = None;
        } else {
            if self.mode == Mode::Semantic {
                self.anchor = Some(self.cursor);
            }
            self.mode = m;
        }
        self.level = None;
        Action::Redraw
    }

    /// Column bounds for line-oriented motions: the innermost region's content,
    /// so `0` lands on the text and not on a border.
    fn bounds_at(&self, p: Pos) -> Rect {
        let tree = &self.analysis.tree;
        tree.chain_at(p)
            .first()
            .map(|&id| tree.get(id).content)
            .filter(|r| r.contains(p.row, p.col))
            .unwrap_or(self.analysis.grid.bounds())
    }
}

fn word_fwd(grid: &Grid, p: Pos) -> Pos {
    let cols = grid.cols as usize;
    let total = cols * grid.rows as usize;
    let ws = |i: usize| grid.get((i / cols) as u16, (i % cols) as u16).ch.is_whitespace();
    let mut i = p.row as usize * cols + p.col as usize;
    while i + 1 < total && !ws(i) {
        i += 1;
        if i % cols == 0 {
            break; // a word never spans rows
        }
    }
    while i + 1 < total && ws(i) {
        i += 1;
    }
    Pos { row: (i / cols) as u16, col: (i % cols) as u16 }
}

fn word_back(grid: &Grid, p: Pos) -> Pos {
    let cols = grid.cols as usize;
    let ws = |i: usize| grid.get((i / cols) as u16, (i % cols) as u16).ch.is_whitespace();
    let mut i = p.row as usize * cols + p.col as usize;
    if i == 0 {
        return p;
    }
    i -= 1;
    while i > 0 && ws(i) {
        i -= 1;
    }
    while i > 0 && i % cols != 0 && !ws(i - 1) {
        i -= 1;
    }
    Pos { row: (i / cols) as u16, col: (i % cols) as u16 }
}

#[cfg(test)]
mod selection_tests {
    use super::*;

    fn grid_from(lines: &[&str]) -> Grid {
        let cols = lines.iter().map(|l| l.chars().count()).max().unwrap_or(0) as u16;
        let mut g = Grid::new(cols, lines.len() as u16);
        for (r, line) in lines.iter().enumerate() {
            for (c, ch) in line.chars().enumerate() {
                g.set(r as u16, c as u16, Cell { ch, ..Cell::default() });
            }
        }
        g
    }

    #[test]
    fn space_expands_and_backspace_shrinks() {
        let g = grid_from(&["hello world", "second line"]);
        let a = Engine::with_defaults().analyze(&g);
        let mut s = Session::new(&a, Pos { row: 0, col: 1 });

        s.handle(Key::Expand); // word
        assert_eq!(s.selected_text(), "hello");
        s.handle(Key::Expand); // line
        assert_eq!(s.selected_text(), "hello world");
        s.handle(Key::Expand); // pane
        assert_eq!(s.highlight(), vec![g.bounds()]);
        s.handle(Key::Expand); // already at top of the ladder
        assert_eq!(s.highlight(), vec![g.bounds()]);
        s.handle(Key::Shrink); // back to line
        assert_eq!(s.selected_text(), "hello world");
    }

    #[test]
    fn enter_with_no_selection_copies_current_line() {
        let g = grid_from(&["hello world", "second line"]);
        let a = Engine::with_defaults().analyze(&g);
        let mut s = Session::new(&a, Pos { row: 1, col: 3 });
        assert_eq!(s.handle(Key::Copy), Action::Copy("second line".to_string()));
    }

    #[test]
    fn visual_char_spans_rows() {
        let g = grid_from(&["hello world", "second line"]);
        let a = Engine::with_defaults().analyze(&g);
        let mut s = Session::new(&a, Pos { row: 0, col: 6 });
        s.handle(Key::VisualChar);
        s.handle(Key::Down);
        assert_eq!(s.selected_text(), "world\nsecond");
    }

    #[test]
    fn click_selects_then_grows_inside_the_selection_then_restarts_elsewhere() {
        let g = grid_from(&["hello world", "second line"]);
        let a = Engine::with_defaults().analyze(&g);
        let mut s = Session::new(&a, Pos { row: 1, col: 0 });
        s.click(Pos { row: 0, col: 1 });
        assert_eq!(s.selected_text(), "hello"); // first click: the word
        s.click(Pos { row: 0, col: 3 }); // another cell of the same word: grow
        assert_eq!(s.selected_text(), "hello world");
        s.click(Pos { row: 0, col: 9 }); // inside the line selection: grow again
        assert_eq!(s.highlight(), vec![g.bounds()]);
        s.click(Pos { row: 1, col: 8 }); // outside the old selection: start over
        assert_eq!(s.selected_text(), "line");
        assert_eq!(s.cursor(), Pos { row: 1, col: 8 });
    }

    #[test]
    fn click_outside_the_grid_is_ignored_and_visual_mode_just_moves() {
        let g = grid_from(&["hello world", "second line"]);
        let a = Engine::with_defaults().analyze(&g);
        let mut s = Session::new(&a, Pos { row: 0, col: 0 });
        assert_eq!(s.click(Pos { row: 5, col: 0 }), Action::None);
        assert_eq!(s.status(), "NORMAL");
        s.handle(Key::VisualChar);
        s.click(Pos { row: 1, col: 5 });
        assert_eq!(s.selected_text(), "hello world\nsecond");
    }

    #[test]
    fn moving_resets_semantic_level_but_not_anchor() {
        let g = grid_from(&["hello world", "second line"]);
        let a = Engine::with_defaults().analyze(&g);
        let mut s = Session::new(&a, Pos { row: 0, col: 0 });
        s.handle(Key::Expand);
        s.handle(Key::Down);
        assert_eq!(s.status(), "NORMAL");
    }
}

// ───────────────────────── 7. Tests ─────────────────────────
//
// Real moat: tests/fixtures/<app>/<case>.ansi  (captured with `tmux capture-pane -p -e`)
//            tests/fixtures/<app>/<case>.expected.json  ([{pos, ladder: [{kind, text}]}])
// Snapshot every popular TUI (claude-code, lazygit, k9s, btop, vim, htop, ...) in CI.

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unwraps_prose_but_keeps_list_items() {
        let lines = vec![
            "this line is long enough to be".to_string(), // 30 chars, width 32 -> "full"
            "wrapped onto the next one".to_string(),
            "- a list item".to_string(),
        ];
        let out = unwrap_soft_wraps(lines, 32);
        assert_eq!(out, vec!["this line is long enough to be wrapped onto the next one", "- a list item"]);
    }

    fn grid_of(lines: &[&str]) -> Grid {
        let cols = lines.iter().map(|l| l.chars().count()).max().unwrap_or(0) as u16;
        let mut g = Grid::new(cols, lines.len() as u16);
        for (r, line) in lines.iter().enumerate() {
            for (c, ch) in line.chars().enumerate() {
                g.set(r as u16, c as u16, Cell { ch, ..Cell::default() });
            }
        }
        g
    }

    #[test]
    fn line_and_word_drop_leading_padding() {
        let g = grid_of(&["   src/lib.rs      ", "     main.rs      "]);
        let a = Engine::with_defaults().analyze(&g);
        let ladder = a.ladder_at(Pos { row: 0, col: 5 });
        let word = ladder.iter().find(|t| t.kind == RegionKind::Word).unwrap();
        let line = ladder.iter().find(|t| t.kind == RegionKind::Line).unwrap();
        assert_eq!(a.extract(word), "src/lib.rs");
        assert_eq!(a.extract(line), "src/lib.rs");
    }

    #[test]
    fn line_extract_is_not_affected_by_neighbour_lines() {
        // Line sees only its own row: dedent must not depend on other rows.
        let g = grid_of(&["        deep", "x shallow"]);
        let a = Engine::with_defaults().analyze(&g);
        let ladder = a.ladder_at(Pos { row: 0, col: 9 });
        let line = ladder.iter().find(|t| t.kind == RegionKind::Line).unwrap();
        assert_eq!(a.extract(line), "deep");
    }

    #[test]
    fn code_keeps_its_indentation() {
        let g = grid_of(&["  fn f() {", "      1", "  }"]);
        let t = Target { kind: RegionKind::Code, rect: g.bounds(), region: None };
        let a = Engine::with_defaults().analyze(&g);
        assert_eq!(a.extract(&t), "  fn f() {\n      1\n  }");
    }

    #[test]
    fn dedent_is_char_based_and_keeps_relative_indent() {
        let mut lines = vec!["  é one".to_string(), "    two".to_string(), "".to_string()];
        dedent(&mut lines);
        assert_eq!(lines, vec!["é one", "  two", ""]);
    }

    fn v(lines: &[&str]) -> Vec<String> {
        lines.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn unwrap_joins_only_where_the_next_word_would_not_have_fit() {
        // width 20: "aaaa bbbb cccc dddd" (19) + "eeee" would be 24 > 20 -> wrapped.
        let out = unwrap_soft_wraps(v(&["aaaa bbbb cccc dddd", "eeee ffff", "next para"]), 20);
        assert_eq!(out, v(&["aaaa bbbb cccc dddd eeee ffff", "next para"]));
        // ...but a long-ish line whose next word WOULD have fit is a real line break.
        let out = unwrap_soft_wraps(v(&["aaaa bbbb cccc", "dd"]), 20);
        assert_eq!(out, v(&["aaaa bbbb cccc", "dd"]));
    }

    #[test]
    fn unwrap_leaves_tables_and_path_lists_alone() {
        // Aligned columns (2+ spaces), every row nearly full width.
        let rows = v(&["web-1   1/1   Running   10.0.0.7", "web-2   1/1   Running   10.0.0.8"]);
        assert_eq!(unwrap_soft_wraps(rows.clone(), 34), rows);
        // A column of long single tokens.
        let paths = v(&["src/very/long/path/to/file.rs", "src/very/long/path/to/other.rs"]);
        assert_eq!(unwrap_soft_wraps(paths.clone(), 30), paths);
    }

    #[test]
    fn unwrap_keeps_blank_lines_and_indented_lines() {
        let out = unwrap_soft_wraps(v(&["aaaa bbbb cccc dddd", "", "eeee ffff gggg hhhh", "  indented"]), 20);
        assert_eq!(out, v(&["aaaa bbbb cccc dddd", "", "eeee ffff gggg hhhh", "  indented"]));
    }

    #[test]
    fn ladder_without_detectors_is_word_line_pane() {
        let mut g = Grid::new(10, 2);
        for (i, ch) in "hello you".chars().enumerate() {
            g.set(0, i as u16, Cell { ch, ..Cell::default() });
        }
        let a = Engine::with_defaults().analyze(&g);
        let kinds: Vec<_> = a.ladder_at(Pos { row: 0, col: 1 }).iter().map(|t| t.kind).collect();
        assert_eq!(kinds, vec![RegionKind::Word, RegionKind::Line, RegionKind::Pane]);
    }
}
