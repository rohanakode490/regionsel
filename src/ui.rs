//! The popup UI: a render loop around the core `Session`.
//! Draws the frozen snapshot, highlights `session.highlight()`, maps keys to core `Key`s.

use std::io::{self, Write};

use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use crossterm::style::{
    Attribute, Color as CtColor, Print, ResetColor, SetAttribute, SetBackgroundColor,
    SetForegroundColor,
};
use crossterm::terminal::{self, EnterAlternateScreen, LeaveAlternateScreen};
use crossterm::{cursor, execute, queue};

use regionsel::{Action, Analysis, Attrs, Color, Key, Pos, Session};

/// Returns Some(text) if the user pressed Enter, None if they cancelled.
pub fn run(analysis: &Analysis, start: Pos) -> io::Result<Option<String>> {
    let mut out = io::stdout();
    terminal::enable_raw_mode()?;
    execute!(out, EnterAlternateScreen, cursor::Hide)?;
    let result = event_loop(&mut out, analysis, start);
    // Always restore the terminal, even if the loop errored.
    let _ = execute!(out, cursor::Show, LeaveAlternateScreen);
    let _ = terminal::disable_raw_mode();
    result
}

fn event_loop(out: &mut impl Write, analysis: &Analysis, start: Pos) -> io::Result<Option<String>> {
    let mut session = Session::new(analysis, start);
    let mut keymap = Keymap::default();
    draw(out, analysis, &session)?;
    loop {
        match event::read()? {
            Event::Key(k) if k.kind != KeyEventKind::Release => {
                if let Some((key, repeat)) = keymap.feed(k) {
                    for _ in 0..repeat {
                        match session.handle(key) {
                            Action::Copy(text) => return Ok(Some(text)),
                            Action::Cancel => return Ok(None),
                            Action::Redraw | Action::None => {}
                        }
                    }
                    draw(out, analysis, &session)?;
                }
            }
            Event::Resize(..) => draw(out, analysis, &session)?,
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
    fn feed(&mut self, ev: KeyEvent) -> Option<(Key, u32)> {
        let ctrl = ev.modifiers.contains(KeyModifiers::CONTROL);

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

fn draw(out: &mut impl Write, analysis: &Analysis, session: &Session) -> io::Result<()> {
    let grid = analysis.grid;
    let (term_w, term_h) = terminal::size()?;
    let (rows, cols) = (grid.rows.min(term_h), grid.cols.min(term_w)); // clip; no scrolling yet
    let hl = session.highlight();
    let cur = session.cursor();

    for r in 0..rows {
        queue!(out, cursor::MoveTo(0, r))?;
        let mut last: Option<(Color, Color, Attrs, bool)> = None;
        for c in 0..cols {
            let cell = grid.get(r, c);
            if cell.width == 0 {
                continue; // continuation half of a wide char
            }
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
            queue!(out, Print(cell.ch))?;
        }
        queue!(out, SetAttribute(Attribute::Reset), ResetColor)?;
    }

    // Status tag, bottom-right. (Covers a few cells of the last row; a real status line
    // would need the popup to be one row taller than the pane.)
    let tag = format!(" {} ", session.status());
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
