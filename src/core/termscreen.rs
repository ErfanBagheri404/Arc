//! The terminal screen model: a scrollback of parsed lines atop a live viewport.
//!
//! Pure and testable: bytes in, styled cells out. The platform layer owns the
//! ConPTY pair (first spawn per `docs/05` — nothing here touches Win32); this
//! module owns *what the user sees* — a fixed `ROWS × COLS` grid of cells, each
//! a glyph plus 256-color SGR attributes, with scrollback and a dirty flag so
//! `platform` only repaints when output actually arrived.

/// Visible grid dimensions.
pub const COLS: usize = 80;
pub const ROWS: usize = 24;
/// Kept scrollback lines. Matches the plan's 2000-line budget.
pub const SCROLLBACK: usize = 2000;

/// One cell: a glyph plus the SGR subset we honor (foreground/background
/// color index into the 256-color table, bold flag).
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Cell {
    pub ch: char,
    pub fg: u8,
    pub bg: u8,
    pub bold: bool,
}

impl Cell {
    fn blank() -> Self {
        Self {
            ch: ' ',
            fg: 7,
            bg: 0,
            bold: false,
        }
    }
}

/// The screen. Feeding and rendering split across two impl blocks on purpose:
/// `feed` owns the ANSI state machine (via `vte`), `render` owns the view.
///
/// `Clone` because the platform layer snapshots it for the frame draw while the
/// reader thread keeps feeding the original.
#[derive(Clone)]
pub struct Screen {
    /// Live viewport, row 0 at top.
    grid: Vec<Vec<Cell>>,
    /// Older lines, oldest first. Capped at [`SCROLLBACK`].
    scrollback: Vec<Vec<Cell>>,
    /// Cursor position in the viewport.
    cur: (usize, usize),
    /// Pending SGR attributes for the next printed glyph.
    attr: Cell,
    /// Set by `feed` when anything changed; cleared by `take_dirty`.
    dirty: bool,
    /// Saved cursor (DECSC).
    saved: Option<(usize, usize)>,
}

impl Default for Screen {
    fn default() -> Self {
        Self::new()
    }
}

impl Screen {
    pub fn new() -> Self {
        Self {
            grid: vec![vec![Cell::blank(); COLS]; ROWS],
            scrollback: Vec::new(),
            cur: (0, 0),
            attr: Cell::blank(),
            dirty: true,
            saved: None,
        }
    }

    /// Feed raw PTY bytes. Runs `vte` over them and applies the resulting
    /// operations to the grid.
    pub fn feed(&mut self, bytes: &[u8]) {
        let mut p = vte::Parser::new();
        p.advance(self, bytes);
        self.dirty = true;
    }

    /// True when output arrived since the last [`Self::take_dirty`].
    pub fn dirty(&self) -> bool {
        self.dirty
    }

    /// Consume the dirty flag (`platform` calls this after repainting).
    pub fn take_dirty(&mut self) -> bool {
        std::mem::replace(&mut self.dirty, false)
    }

    /// Viewport row `r` as plain text (trailing blanks trimmed).
    pub fn line_text(&self, r: usize) -> String {
        self.grid
            .get(r)
            .map(|row| {
                row.iter()
                    .map(|c| c.ch)
                    .collect::<String>()
                    .trim_end()
                    .to_string()
            })
            .unwrap_or_default()
    }

    /// Viewport row `r` with per-cell attributes, for the renderer.
    pub fn line_cells(&self, r: usize) -> &[Cell] {
        self.grid.get(r).map(Vec::as_slice).unwrap_or(&[])
    }

    /// Whole viewport as text, for tests and copy-to-clipboard.
    pub fn text(&self) -> String {
        (0..ROWS)
            .map(|r| self.line_text(r))
            .collect::<Vec<_>>()
            .join("\n")
            .trim_end()
            .to_string()
    }

    /// Scrollback depth (capped at [`SCROLLBACK`]).
    pub fn scrollback_len(&self) -> usize {
        self.scrollback.len()
    }

    fn scroll_up(&mut self) {
        let mut top: Vec<Cell> = std::mem::replace(&mut self.grid[0], vec![Cell::blank(); COLS]);
        // Reuse the bottom row's allocation for the new blank line.
        std::mem::swap(&mut top, &mut self.grid[ROWS - 1]);
        self.scrollback.push(top);
        if self.scrollback.len() > SCROLLBACK {
            let excess = self.scrollback.len() - SCROLLBACK;
            self.scrollback.drain(..excess);
        }
        self.grid.rotate_left(1);
    }

    fn newline(&mut self) {
        if self.cur.0 + 1 >= ROWS {
            self.scroll_up();
            self.cur.0 = ROWS - 1;
        } else {
            self.cur.0 += 1;
        }
        self.cur.1 = 0;
    }

    fn put_char(&mut self, c: char) {
        if self.cur.1 >= COLS {
            self.newline();
        }
        let (r, col) = self.cur;
        if r < ROWS && col < COLS {
            self.grid[r][col] = Cell {
                ch: c,
                fg: self.attr.fg,
                bg: self.attr.bg,
                bold: self.attr.bold,
            };
        }
        self.cur.1 += 1;
    }
}

impl vte::Perform for Screen {
    fn print(&mut self, c: char) {
        self.put_char(c);
    }

    fn execute(&mut self, byte: u8) {
        match byte {
            b'\n' | 0x0b | 0x0c => self.newline(),
            b'\r' => self.cur.1 = 0,
            0x08 => self.cur.1 = self.cur.1.saturating_sub(1),
            0x07 => {} // BEL: no audible bell on an overlay.
            0x09 => {
                // TAB to the next 8-column stop.
                let next = (self.cur.1 / 8 + 1) * 8;
                self.cur.1 = next.min(COLS.saturating_sub(1));
            }
            _ => {}
        }
    }

    fn csi_dispatch(
        &mut self,
        params: &vte::Params,
        _intermediates: &[u8],
        _ignore: bool,
        action: char,
    ) {
        let p: Vec<u16> = params.iter().map(|s| s.iter().fold(0u16, |a, n| a * 10 + n)).collect();
        let arg = |i: usize, dflt: u16| p.get(i).copied().unwrap_or(dflt).max(1) as usize;
        match action {
            'A' => self.cur.0 = self.cur.0.saturating_sub(arg(0, 1)),
            'B' => self.cur.0 = (self.cur.0 + arg(0, 1)).min(ROWS - 1),
            'C' => self.cur.1 = (self.cur.1 + arg(0, 1)).min(COLS - 1),
            'D' => self.cur.1 = self.cur.1.saturating_sub(arg(0, 1)),
            'E' => {
                self.cur.0 = (self.cur.0 + arg(0, 1)).min(ROWS - 1);
                self.cur.1 = 0;
            }
            'F' => {
                self.cur.0 = self.cur.0.saturating_sub(arg(0, 1));
                self.cur.1 = 0;
            }
            'G' => self.cur.1 = arg(0, 1).saturating_sub(1).min(COLS - 1),
            'H' | 'f' => {
                self.cur.0 = arg(0, 1).saturating_sub(1).min(ROWS - 1);
                self.cur.1 = arg(1, 1).saturating_sub(1).min(COLS - 1);
            }
            'J' => {
                // ED: 0 = below (default), 1 = above, 2/3 = whole screen.
                match p.first().copied().unwrap_or(0) {
                    0 => {
                        let (r, c) = self.cur;
                        for col in c..COLS {
                            self.grid[r][col] = Cell::blank();
                        }
                        for row in self.grid.iter_mut().skip(r + 1) {
                            row.fill_with(Cell::blank);
                        }
                    }
                    1 => {
                        let (r, c) = self.cur;
                        for row in self.grid.iter_mut().take(r + 1) {
                            row.fill_with(Cell::blank);
                        }
                        for col in 0..=c.min(COLS - 1) {
                            self.grid[r][col] = Cell::blank();
                        }
                    }
                    _ => {
                        for row in self.grid.iter_mut() {
                            row.fill_with(Cell::blank);
                        }
                        self.cur = (0, 0);
                    }
                }
            }
            'K' => {
                // EL: 0 = right (default), 1 = left, 2 = whole line.
                let (r, c) = self.cur;
                match p.first().copied().unwrap_or(0) {
                    1 => {
                        for col in 0..=c.min(COLS - 1) {
                            self.grid[r][col] = Cell::blank();
                        }
                    }
                    2 => self.grid[r].fill_with(Cell::blank),
                    _ => {
                        for col in c..COLS {
                            self.grid[r][col] = Cell::blank();
                        }
                    }
                }
            }
            'm' => apply_sgr(&mut self.attr, &p),
            's' => self.saved = Some(self.cur),
            'u' => {
                if let Some(pos) = self.saved {
                    self.cur = pos;
                }
            }
            _ => {}
        }
    }

    fn esc_dispatch(&mut self, _intermediates: &[u8], _ignore: bool, byte: u8) {
        match byte {
            b'7' => self.saved = Some(self.cur),
            b'8' => {
                if let Some(pos) = self.saved {
                    self.cur = pos;
                }
            }
            b'M' => {
                // RI: reverse index.
                if self.cur.0 == 0 {
                    self.grid.rotate_right(1);
                    self.grid[0].fill_with(Cell::blank);
                } else {
                    self.cur.0 -= 1;
                }
            }
            _ => {}
        }
    }
}

/// Apply SGR parameters to the pending attributes. Honors the 256-color
/// subset (30-37/40-47, 90-97/100-107, 38;5;n/48;5;n, 38;2;r;g;b
/// approximated to the nearest 256 slot); everything else is ignored.
fn apply_sgr(attr: &mut Cell, p: &[u16]) {
    if p.is_empty() {
        *attr = Cell::blank();
        return;
    }
    let mut i = 0;
    while i < p.len() {
        match p[i] {
            0 => *attr = Cell::blank(),
            1 => attr.bold = true,
            22 => attr.bold = false,
            30..=37 => attr.fg = (p[i] - 30) as u8,
            90..=97 => attr.fg = (p[i] - 90 + 8) as u8,
            40..=47 => attr.bg = (p[i] - 40) as u8,
            100..=107 => attr.bg = (p[i] - 100 + 8) as u8,
            39 => attr.fg = 7,
            49 => attr.bg = 0,
            38 | 48 => {
                // Extended color: `38;5;n` / `48;5;n` or `38;2;r;g;b`.
                let is_fg = p[i] == 38;
                if p.get(i + 1) == Some(&5) {
                    if let Some(&n) = p.get(i + 2) {
                        let slot = n.min(255) as u8;
                        if is_fg {
                            attr.fg = slot;
                        } else {
                            attr.bg = slot;
                        }
                        i += 2;
                    }
                } else if p.get(i + 1) == Some(&2) {
                    if let (Some(&r), Some(&g), Some(&b)) =
                        (p.get(i + 2), p.get(i + 3), p.get(i + 4))
                    {
                        let slot = rgb_to_256(r.min(255) as u8, g.min(255) as u8, b.min(255) as u8);
                        if is_fg {
                            attr.fg = slot;
                        } else {
                            attr.bg = slot;
                        }
                        i += 4;
                    }
                }
            }
            _ => {}
        }
        i += 1;
    }
}

/// Nearest 256-color slot for a truecolor triple: the 6×6×6 cube.
fn rgb_to_256(r: u8, g: u8, b: u8) -> u8 {
    16 + (r as u16 * 5 / 255) as u8 * 36 + (g as u16 * 5 / 255) as u8 * 6 + (b as u16 * 5 / 255) as u8
}

/// The 256-color table as sRGB floats. Standard VGA base + 6×6×6 cube +
/// grayscale ramp.
pub fn palette(i: u8) -> (f32, f32, f32) {
    const BASE: [(f32, f32, f32); 16] = [
        (0.0, 0.0, 0.0),
        (0.67, 0.0, 0.0),
        (0.0, 0.67, 0.0),
        (0.67, 0.67, 0.0),
        (0.0, 0.0, 0.67),
        (0.67, 0.0, 0.67),
        (0.0, 0.67, 0.67),
        (0.75, 0.75, 0.75),
        (0.5, 0.5, 0.5),
        (1.0, 0.0, 0.0),
        (0.0, 1.0, 0.0),
        (1.0, 1.0, 0.0),
        (0.0, 0.0, 1.0),
        (1.0, 0.0, 1.0),
        (0.0, 1.0, 1.0),
        (1.0, 1.0, 1.0),
    ];
    match i {
        0..=15 => BASE[i as usize],
        16..=231 => {
            let n = i - 16;
            let r = n / 36;
            let g = (n % 36) / 6;
            let b = n % 6;
            let v = |c: u8| {
                if c == 0 {
                    0.0
                } else {
                    0.35 + 0.13 * (c as f32 - 1.0)
                }
            };
            (v(r), v(g), v(b))
        }
        _ => {
            let g = 0.03 + 0.033 * (i - 232) as f32;
            (g, g, g)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_text_lands_on_the_first_row() {
        let mut s = Screen::new();
        s.feed(b"hello");
        assert_eq!(s.line_text(0), "hello");
        assert!(s.dirty());
        assert!(s.take_dirty());
        assert!(!s.dirty());
    }

    #[test]
    fn crlf_moves_to_the_next_row() {
        let mut s = Screen::new();
        s.feed(b"a\r\nb");
        assert_eq!(s.line_text(0), "a");
        assert_eq!(s.line_text(1), "b");
    }

    #[test]
    fn sgr_colors_stick_to_the_cells_they_wrap() {
        let mut s = Screen::new();
        s.feed(b"\x1b[31mok\x1b[0m.");
        let cells = s.line_cells(0);
        assert_eq!((cells[0].fg, cells[1].fg), (1, 1));
        assert_eq!(cells[2].fg, 7);
        assert_eq!(s.line_text(0), "ok.");
    }

    #[test]
    fn cursor_addressing_moves_the_pen() {
        let mut s = Screen::new();
        s.feed(b"\x1b[2;3HX");
        assert_eq!(s.line_text(1).chars().nth(2), Some('X'));
    }

    #[test]
    fn erase_line_clears_right_of_cursor() {
        let mut s = Screen::new();
        s.feed(b"abcdef\x1b[1;3H\x1b[K");
        assert_eq!(s.line_text(0), "ab");
    }

    #[test]
    fn erase_screen_clears_all_and_homes() {
        let mut s = Screen::new();
        s.feed(b"hi\x1b[2J");
        assert_eq!(s.text(), "");
    }

    #[test]
    fn scrolling_pushes_lines_into_scrollback() {
        let mut s = Screen::new();
        for i in 0..(ROWS + 5) {
            s.feed(format!("line{i}\r\n").as_bytes());
        }
        assert_eq!(s.scrollback_len(), 6);
        assert_eq!(s.line_text(0), "line6");
    }

    #[test]
    fn scrollback_is_capped() {
        let mut s = Screen::new();
        for i in 0..(SCROLLBACK + 100) {
            s.feed(format!("x{i}\r\n").as_bytes());
        }
        assert_eq!(s.scrollback_len(), SCROLLBACK);
    }

    #[test]
    fn backspace_erases_by_overwrite() {
        let mut s = Screen::new();
        s.feed(b"ab\x08c");
        assert_eq!(s.line_text(0), "ac");
    }

    #[test]
    fn xterm_256_color_fg() {
        let mut s = Screen::new();
        s.feed(b"\x1b[38;5;196mR");
        assert_eq!(s.line_cells(0)[0].fg, 196);
    }

    #[test]
    fn truecolor_maps_into_the_cube() {
        let mut s = Screen::new();
        s.feed(b"\x1b[38;2;255;0;0mR");
        assert_eq!(s.line_cells(0)[0].fg, 196);
    }

    #[test]
    fn cursor_save_restore_round_trip() {
        let mut s = Screen::new();
        s.feed(b"ab\x1b[sXY\x1b[uZ");
        // `s` saves after "ab"; XY print; `u` restores; Z overwrites X.
        assert_eq!(s.line_text(0), "abZY");
    }

    #[test]
    fn palette_covers_the_full_range() {
        assert_eq!(palette(0), (0.0, 0.0, 0.0));
        assert_eq!(palette(15), (1.0, 1.0, 1.0));
        // 196 is the cube's pure red: level 5 of red, 0 green, 0 blue.
        assert_eq!(palette(196), (0.87, 0.0, 0.0));
        let (g, _, _) = palette(240);
        assert!(g > 0.2 && g < 0.35);
    }
}
