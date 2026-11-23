//! The popup UI: a render loop around the core `Session`.
//! Draws the frozen snapshot, highlights `session.highlight()`, maps keys to core `Key`s.

use std::io::{self, Write};

use crossterm::event::{
    self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers,
    MouseButton, MouseEventKind,
};
use crossterm::style::{
    Attribute, Color as CtColor, Print, ResetColor, SetAttribute, SetBackgroundColor,
    SetForegroundColor,
};
use crossterm::terminal::{self, EnterAlternateScreen, LeaveAlternateScreen};
use crossterm::{cursor, execute, queue};

use regionsel::view::Viewport;
use regionsel::{Action, Analysis, Attrs, Color, Key, Pos, Session};

/// Rows moved per mouse-wheel notch.
const WHEEL_ROWS: i32 = 3;

/// Returns Some(text) if the user pressed Enter, None if they cancelled.
/// `click_start`: the popup was opened by a mouse click, so start with that cell selected.
pub fn run(analysis: &Analysis, start: Pos, click_start: bool) -> io::Result<Option<String>> {
    let mut out = io::stdout();
    terminal::enable_raw_mode()?;
    execute!(out, EnterAlternateScreen, EnableMouseCapture, cursor::Hide)?;
    let result = event_loop(&mut out, analysis, start, click_start);
    // Always restore the terminal, even if the loop errored.
    let _ = execute!(out, cursor::Show, DisableMouseCapture, LeaveAlternateScreen);
    let _ = terminal::disable_raw_mode();
    result
}

fn event_loop(out: &mut impl Write, analysis: &Analysis, start: Pos, click_start: bool) -> io::Result<Option<String>> {
    let grid = analysis.grid;
    let (tw, th) = terminal::size()?;
    let mut view = Viewport::new(grid.cols, grid.rows, tw, th);
    let mut session = Session::new(analysis, start);
    if click_start {
        session.click(start);
    }
    view.follow(session.cursor());
    let mut keymap = Keymap::default();
    draw(out, analysis, &session, &view)?;
    loop {
        match event::read()? {
            Event::Key(k) if k.kind != KeyEventKind::Release => {
                if let Some((key, repeat)) = keymap.feed(k, u32::from(view.height)) {
                    for _ in 0..repeat {
                        match session.handle(key) {
                            Action::Copy(text) => return Ok(Some(text)),
                            Action::Cancel => return Ok(None),
                            Action::Redraw | Action::None => {}
                        }
                    }
                    view.follow(session.cursor());
                    draw(out, analysis, &session, &view)?;
                }
            }
            Event::Mouse(m) => match m.kind {
                MouseEventKind::Down(MouseButton::Left) => {
                    // Any left click works inside the popup; Alt is only needed to OPEN it.
                    if let Some(p) = view.to_grid(m.column, m.row) {
                        session.click(p);
                        view.follow(session.cursor());
                        draw(out, analysis, &session, &view)?;
                    }
                }
                // The wheel scrolls the view and leaves the cursor and selection alone.
                MouseEventKind::ScrollUp => {
                    view.scroll_rows(-WHEEL_ROWS);
                    draw(out, analysis, &session, &view)?;
                }
                MouseEventKind::ScrollDown => {
                    view.scroll_rows(WHEEL_ROWS);
                    draw(out, analysis, &session, &view)?;
                }
                _ => {}
            },
            Event::Resize(w, h) => {
                view.resize(w, h);
                view.follow(session.cursor());
                draw(out, analysis, &session, &view)?;
            }
            _ => {}
        }
    }
}

// ───────────── keys ─────────────

/// vim-style bindings with count prefixes (5j, 3w) and `gg`.
/// This is the ONLY place that knows about physical keys; make it configurable later.
#[derive(Default)]
struct Keymap {
    pending_g: bool,
    count: Option<u32>,
}

impl Keymap {
    /// `page`: rows on screen, for the page-wise scroll keys.
    fn feed(&mut self, ev: KeyEvent, page: u32) -> Option<(Key, u32)> {
        let ctrl = ev.modifiers.contains(KeyModifiers::CONTROL);
        let half = (page / 2).max(1);
        let page = page.saturating_sub(1).max(1); // keep one row of context, like vim
        match (ev.code, ctrl) {
            (KeyCode::Char('d'), true) => return self.scroll(Key::Down, half),
            (KeyCode::Char('u'), true) => return self.scroll(Key::Up, half),
            (KeyCode::Char('f'), true) | (KeyCode::PageDown, _) => return self.scroll(Key::Down, page),
            (KeyCode::Char('b'), true) | (KeyCode::PageUp, _) => return self.scroll(Key::Up, page),
            _ => {}
        }

        if self.pending_g {
            self.pending_g = false;
            if ev.code == KeyCode::Char('g') {
                return self.emit(Key::Top);
            }
        }

        let key = match (ev.code, ctrl) {
            (KeyCode::Char('c'), true) | (KeyCode::Esc, _) | (KeyCode::Char('q'), false) => Key::Cancel,
            (KeyCode::Char('v'), true) => Key::VisualRect,
            (KeyCode::Char(d), false) if d.is_ascii_digit() && (d != '0' || self.count.is_some()) => {
                let digit = d.to_digit(10).unwrap_or(0);
                self.count = Some(self.count.unwrap_or(0).saturating_mul(10).saturating_add(digit));
                return None;
            }
            (KeyCode::Char('g'), false) => {
                self.pending_g = true;
                return None;
            }
            (KeyCode::Char('h'), false) | (KeyCode::Left, _) => Key::Left,
            (KeyCode::Char('j'), false) | (KeyCode::Down, _) => Key::Down,
            (KeyCode::Char('k'), false) | (KeyCode::Up, _) => Key::Up,
            (KeyCode::Char('l'), false) | (KeyCode::Right, _) => Key::Right,
            (KeyCode::Char('w'), false) => Key::WordFwd,
            (KeyCode::Char('b'), false) => Key::WordBack,
            (KeyCode::Char('0'), false) => Key::LineStart,
            (KeyCode::Char('$'), false) => Key::LineEnd,
            (KeyCode::Char('G'), false) => Key::Bottom,
            (KeyCode::Char(' '), _) => Key::Expand,
            (KeyCode::Backspace, _) => Key::Shrink,
            (KeyCode::Char('v'), false) => Key::VisualChar,
            (KeyCode::Char('V'), false) => Key::VisualLine,
            (KeyCode::Enter, _) => Key::Copy,
            _ => {
                self.count = None;
                return None;
            }
        };
        self.emit(key)
    }

    /// Half/full page keys move the cursor `n` rows; the viewport follows it.
    fn scroll(&mut self, key: Key, n: u32) -> Option<(Key, u32)> {
        self.pending_g = false;
        self.count = None;
        Some((key, n))
    }

    fn emit(&mut self, key: Key) -> Option<(Key, u32)> {
        let n = self.count.take().unwrap_or(1).clamp(1, 9999);
        let repeatable = matches!(
            key,
            Key::Left | Key::Right | Key::Up | Key::Down | Key::WordFwd | Key::WordBack | Key::Expand | Key::Shrink
        );
        Some((key, if repeatable { n } else { 1 }))
    }
}

// ───────────── drawing ─────────────

fn to_ct(c: Color) -> CtColor {
    match c {
        Color::Default => CtColor::Reset,
        Color::Indexed(i) => CtColor::AnsiValue(i),
        Color::Rgb(r, g, b) => CtColor::Rgb { r, g, b },
    }
}

fn draw(out: &mut impl Write, analysis: &Analysis, session: &Session, view: &Viewport) -> io::Result<()> {
    let grid = analysis.grid;
    let (term_w, term_h) = (view.width, view.height);
    let hl = session.highlight();
    let cur = session.cursor();

    for y in 0..view.visible_rows() {
        let r = view.top + y;
        queue!(out, cursor::MoveTo(0, y))?;
        let mut last: Option<(Color, Color, Attrs, bool)> = None;
        for x in 0..view.visible_cols() {
            let c = view.left + x;
            let cell = grid.get(r, c);
            // The left half of a wide char can be scrolled off: show a blank, not a gap.
            let ch = if cell.width == 0 {
                if x == 0 { ' ' } else { continue } // continuation half of a wide char
            } else {
                cell.ch
            };
            let selected = hl.iter().any(|h| h.contains(r, c));
            let is_cursor = r == cur.row && c == cur.col;
            // Selection = reverse video. The cursor flips whatever is under it, so it stays
            // visible both on plain text and inside a selection.
            let inverted = (selected != is_cursor) != cell.attrs.reverse;

            let style = (cell.fg, cell.bg, cell.attrs, inverted);
            if last != Some(style) {
                queue!(
                    out,
                    SetAttribute(Attribute::Reset),
                    SetForegroundColor(to_ct(cell.fg)),
                    SetBackgroundColor(to_ct(cell.bg))
                )?;
                if cell.attrs.bold {
                    queue!(out, SetAttribute(Attribute::Bold))?;
                }
                if cell.attrs.italic {
                    queue!(out, SetAttribute(Attribute::Italic))?;
                }
                if cell.attrs.underline {
                    queue!(out, SetAttribute(Attribute::Underlined))?;
                }
                if inverted {
                    queue!(out, SetAttribute(Attribute::Reverse))?;
                }
                last = Some(style);
            }
            queue!(out, Print(ch))?;
        }
        queue!(out, SetAttribute(Attribute::Reset), ResetColor)?;
    }
    // Rows below the grid (a screen taller than the grid): clear leftovers.
    for y in view.visible_rows()..term_h {
        queue!(out, cursor::MoveTo(0, y), terminal::Clear(terminal::ClearType::CurrentLine))?;
    }

    // Status tag, bottom-right. (Covers a few cells of the last row; a real status line
    // would need the popup to be one row taller than the pane.) When the grid is bigger
    // than the screen it also shows where the cursor is.
    let mut tag = format!(" {} ", session.status());
    if grid.rows > term_h {
        tag = format!(" {} {}/{} ", session.status(), cur.row + 1, grid.rows);
    }
    let len = tag.chars().count() as u16;
    if term_h > 0 && term_w >= len {
        queue!(
            out,
            cursor::MoveTo(term_w - len, term_h - 1),
            SetAttribute(Attribute::Reverse),
            SetAttribute(Attribute::Bold),
            Print(tag),
            SetAttribute(Attribute::Reset)
        )?;
    }
    out.flush()
}
