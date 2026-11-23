//! Viewport: which part of a (possibly larger) grid is on screen. Pure arithmetic, no I/O.
//!
//! The grid can be bigger than the popup: scrollback makes it taller, and a popup that tmux
//! had to shrink makes the screen smaller than the pane. The frontend draws
//! `grid[top + y][left + x]` for each screen cell and asks `to_grid` to map a mouse click back.

use crate::Pos;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Viewport {
    pub top: u16,
    pub left: u16,
    pub width: u16,
    pub height: u16,
    grid_cols: u16,
    grid_rows: u16,
}

impl Viewport {
    /// Starts bottom-aligned (the live screen is at the bottom of the scrollback) and
    /// left-aligned.
    pub fn new(grid_cols: u16, grid_rows: u16, width: u16, height: u16) -> Self {
        let mut v = Viewport { top: 0, left: 0, width, height, grid_cols, grid_rows };
        v.top = grid_rows.saturating_sub(height);
        v
    }

    pub fn resize(&mut self, width: u16, height: u16) {
        self.width = width;
        self.height = height;
        self.clamp();
    }

    fn clamp(&mut self) {
        self.top = self.top.min(self.grid_rows.saturating_sub(self.height));
        self.left = self.left.min(self.grid_cols.saturating_sub(self.width));
    }

    /// Scroll the minimum amount that makes `p` visible.
    pub fn follow(&mut self, p: Pos) {
        if self.height > 0 {
            if p.row < self.top {
                self.top = p.row;
            } else if p.row >= self.top + self.height {
                self.top = p.row + 1 - self.height;
            }
        }
        if self.width > 0 {
            if p.col < self.left {
                self.left = p.col;
            } else if p.col >= self.left + self.width {
                self.left = p.col + 1 - self.width;
            }
        }
        self.clamp();
    }

    /// Mouse wheel: scroll by whole rows, never past either end.
    pub fn scroll_rows(&mut self, delta: i32) {
        let top = (self.top as i32 + delta).max(0).min(u16::MAX as i32) as u16;
        self.top = top;
        self.clamp();
    }

    /// Screen cell -> grid position; None if the cell is outside the grid.
    pub fn to_grid(&self, x: u16, y: u16) -> Option<Pos> {
        let (row, col) = (self.top.checked_add(y)?, self.left.checked_add(x)?);
        (row < self.grid_rows && col < self.grid_cols).then_some(Pos { row, col })
    }

    /// Number of grid rows / cols that are actually drawn.
    pub fn visible_rows(&self) -> u16 {
        self.height.min(self.grid_rows - self.top)
    }
    pub fn visible_cols(&self) -> u16 {
        self.width.min(self.grid_cols - self.left)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(row: u16, col: u16) -> Pos {
        Pos { row, col }
    }

    #[test]
    fn starts_bottom_aligned_so_the_live_screen_is_what_you_first_see() {
        let v = Viewport::new(80, 2040, 80, 40);
        assert_eq!((v.top, v.left), (2000, 0));
        let small_grid = Viewport::new(80, 10, 80, 40);
        assert_eq!(small_grid.top, 0);
    }

    #[test]
    fn follow_scrolls_the_minimum_amount() {
        let mut v = Viewport::new(200, 100, 80, 20); // top = 80
        v.follow(p(85, 10));
        assert_eq!(v.top, 80); // already visible: no scroll
        v.follow(p(79, 10));
        assert_eq!(v.top, 79); // one row above: scroll one row
        v.follow(p(0, 150));
        assert_eq!((v.top, v.left), (0, 71)); // col 150 just fits at the right edge
        v.follow(p(99, 0));
        assert_eq!((v.top, v.left), (80, 0));
    }

    #[test]
    fn wheel_scrolling_stops_at_both_ends() {
        let mut v = Viewport::new(80, 100, 80, 20);
        v.scroll_rows(-1000);
        assert_eq!(v.top, 0);
        v.scroll_rows(1000);
        assert_eq!(v.top, 80);
        v.scroll_rows(-3);
        assert_eq!(v.top, 77);
    }

    #[test]
    fn maps_clicks_back_to_the_grid_and_rejects_cells_outside_it() {
        let mut v = Viewport::new(100, 50, 40, 10);
        v.top = 30;
        v.left = 20;
        assert_eq!(v.to_grid(0, 0), Some(p(30, 20)));
        assert_eq!(v.to_grid(5, 3), Some(p(33, 25)));
        let short = Viewport::new(10, 4, 40, 10); // screen larger than the grid
        assert_eq!(short.to_grid(3, 3), Some(p(3, 3)));
        assert_eq!(short.to_grid(3, 4), None);
        assert_eq!(short.to_grid(10, 0), None);
        assert_eq!((short.visible_rows(), short.visible_cols()), (4, 10));
    }

    #[test]
    fn resize_keeps_the_view_inside_the_grid() {
        let mut v = Viewport::new(80, 100, 80, 20); // top 80
        v.resize(80, 40);
        assert_eq!(v.top, 60);
        v.resize(120, 200); // bigger than the grid
        assert_eq!((v.top, v.left), (0, 0));
    }
}
