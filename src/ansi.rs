//! `tmux capture-pane -p -e -N` output -> core Grid.
//! vt100 does the escape parsing and wide-char bookkeeping so we don't have to.

use crate::{Attrs, Cell, Color, Grid};

fn color(c: vt100::Color) -> Color {
    match c {
        vt100::Color::Default => Color::Default,
        vt100::Color::Idx(i) => Color::Indexed(i),
        vt100::Color::Rgb(r, g, b) => Color::Rgb(r, g, b),
    }
}

pub fn grid_from_capture(raw: &str, cols: u16, rows: u16) -> Grid {
    let mut parser = vt100::Parser::new(rows, cols, 0);
    // Rows are separated by bare \n; a terminal needs \r\n. Drop the final newline so the
    // last row doesn't scroll the screen.
    let body = raw.strip_suffix('\n').unwrap_or(raw);
    parser.process(body.replace('\n', "\r\n").as_bytes());
    let screen = parser.screen();

    let mut grid = Grid::new(cols, rows);
    for r in 0..rows {
        for c in 0..cols {
            let Some(cell) = screen.cell(r, c) else { continue };
            let width = if cell.is_wide_continuation() {
                0
            } else if cell.is_wide() {
                2
            } else {
                1
            };
            grid.set(
                r,
                c,
                Cell {
                    ch: cell.contents().chars().next().unwrap_or(' '),
                    width,
                    fg: color(cell.fgcolor()),
                    bg: color(cell.bgcolor()),
                    attrs: Attrs {
                        bold: cell.bold(),
                        dim: false,
                        italic: cell.italic(),
                        underline: cell.underline(),
                        reverse: cell.inverse(),
                    },
                },
            );
        }
    }
    grid
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_colors_and_rows() {
        let raw = "\x1b[44mab\x1b[0m cd\nxy  \n";
        let g = grid_from_capture(raw, 5, 2);
        assert_eq!(g.get(0, 0).ch, 'a');
        assert_eq!(g.get(0, 0).bg, Color::Indexed(4));
        assert_eq!(g.get(0, 2).bg, Color::Default);
        assert_eq!(g.get(0, 3).ch, 'c');
        assert_eq!(g.get(1, 0).ch, 'x');
    }

    #[test]
    fn full_width_rows_do_not_scroll() {
        let g = grid_from_capture("abc\ndef\n", 3, 2);
        assert_eq!(g.get(0, 0).ch, 'a');
        assert_eq!(g.get(1, 2).ch, 'f');
    }
}
