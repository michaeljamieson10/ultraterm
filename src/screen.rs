use bitflags::bitflags;
use unicode_width::UnicodeWidthChar;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Rgb {
    pub r: u8,
    pub g: u8,
    pub b: u8,
}

impl Rgb {
    pub const fn new(r: u8, g: u8, b: u8) -> Self {
        Self { r, g, b }
    }
}

pub const DEFAULT_FG: Rgb = Rgb::new(230, 230, 230);
pub const DEFAULT_BG: Rgb = Rgb::new(12, 12, 12);
pub const CURSOR_COLOR: Rgb = Rgb::new(240, 240, 240);

bitflags! {
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub struct CellFlags: u8 {
        const BOLD = 0b0000_0001;
        const ITALIC = 0b0000_0010;
        const UNDERLINE = 0b0000_0100;
        const INVERSE = 0b0000_1000;
        const WIDE = 0b0001_0000;
        const WIDE_CONT = 0b0010_0000;
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Cell {
    pub ch: char,
    pub fg: Rgb,
    pub bg: Rgb,
    pub flags: CellFlags,
}

impl Cell {
    pub const fn blank() -> Self {
        Self {
            ch: ' ',
            fg: DEFAULT_FG,
            bg: DEFAULT_BG,
            flags: CellFlags::empty(),
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub struct AttrState {
    pub fg: Rgb,
    pub bg: Rgb,
    pub bold: bool,
    pub italic: bool,
    pub underline: bool,
    pub inverse: bool,
}

impl Default for AttrState {
    fn default() -> Self {
        Self {
            fg: DEFAULT_FG,
            bg: DEFAULT_BG,
            bold: false,
            italic: false,
            underline: false,
            inverse: false,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CursorShape {
    Block,
    Beam,
    Underline,
}

#[derive(Clone, Copy, Debug)]
pub struct Cursor {
    pub row: usize,
    pub col: usize,
    pub visible: bool,
    pub shape: CursorShape,
}

impl Default for Cursor {
    fn default() -> Self {
        Self {
            row: 0,
            col: 0,
            visible: true,
            shape: CursorShape::Block,
        }
    }
}

#[derive(Clone)]
struct Buffer {
    cols: usize,
    rows: usize,
    cells: Vec<Cell>,
    row_map: Vec<usize>,
    tabstops: Vec<bool>,
    cursor: Cursor,
    saved_cursor: Cursor,
    attrs: AttrState,
    saved_attrs: AttrState,
    scroll_top: usize,
    scroll_bottom: usize,
}

impl Buffer {
    fn new(cols: usize, rows: usize) -> Self {
        let mut tabstops = vec![false; cols.max(1)];
        for col in (0..cols).step_by(8) {
            tabstops[col] = true;
        }
        Self {
            cols,
            rows,
            cells: vec![Cell::blank(); cols * rows],
            row_map: (0..rows).collect(),
            tabstops,
            cursor: Cursor::default(),
            saved_cursor: Cursor::default(),
            attrs: AttrState::default(),
            saved_attrs: AttrState::default(),
            scroll_top: 0,
            scroll_bottom: rows.saturating_sub(1),
        }
    }

    fn reset_tabstops(&mut self) {
        self.tabstops.resize(self.cols.max(1), false);
        self.tabstops.fill(false);
        for col in (0..self.cols).step_by(8) {
            self.tabstops[col] = true;
        }
    }

    fn resize(&mut self, cols: usize, rows: usize) {
        let mut next = Buffer::new(cols, rows);
        let copy_rows = rows.min(self.rows);
        let copy_cols = cols.min(self.cols);

        for row in 0..copy_rows {
            let src = self.line(row);
            let dst = next.line_mut(row);
            dst[..copy_cols].copy_from_slice(&src[..copy_cols]);
        }

        next.cursor.row = self.cursor.row.min(rows.saturating_sub(1));
        next.cursor.col = self.cursor.col.min(cols.saturating_sub(1));
        next.saved_cursor = self.saved_cursor;
        next.saved_cursor.row = next.saved_cursor.row.min(rows.saturating_sub(1));
        next.saved_cursor.col = next.saved_cursor.col.min(cols.saturating_sub(1));
        next.attrs = self.attrs;
        next.saved_attrs = self.saved_attrs;
        *self = next;
    }

    fn clear_all(&mut self) {
        self.cells.fill(Cell::blank());
    }

    fn line(&self, row: usize) -> &[Cell] {
        let phys = self.row_map[row];
        let start = phys * self.cols;
        &self.cells[start..start + self.cols]
    }

    fn line_mut(&mut self, row: usize) -> &mut [Cell] {
        let phys = self.row_map[row];
        let start = phys * self.cols;
        &mut self.cells[start..start + self.cols]
    }

    fn physical_line(&self, physical_row: usize) -> &[Cell] {
        let start = physical_row * self.cols;
        &self.cells[start..start + self.cols]
    }

    fn clear_physical_row(&mut self, physical_row: usize) {
        let start = physical_row * self.cols;
        self.cells[start..start + self.cols].fill(Cell::blank());
    }

    fn rotate_up(&mut self, top: usize, bottom: usize) -> usize {
        let removed = self.row_map[top];
        for row in top..bottom {
            self.row_map[row] = self.row_map[row + 1];
        }
        self.row_map[bottom] = removed;
        removed
    }

    fn rotate_down(&mut self, top: usize, bottom: usize) -> usize {
        let inserted = self.row_map[bottom];
        for row in (top + 1..=bottom).rev() {
            self.row_map[row] = self.row_map[row - 1];
        }
        self.row_map[top] = inserted;
        inserted
    }

    fn cell_index(&self, row: usize, col: usize) -> usize {
        self.row_map[row] * self.cols + col
    }

    fn set_cell(&mut self, row: usize, col: usize, cell: Cell) {
        let idx = self.cell_index(row, col);
        self.cells[idx] = cell;
    }

    fn clamp_cursor(&mut self) {
        self.cursor.row = self.cursor.row.min(self.rows.saturating_sub(1));
        self.cursor.col = self.cursor.col.min(self.cols.saturating_sub(1));
    }
}

pub struct ScrollbackRing {
    lines: Vec<Vec<Cell>>,
    head: usize,
    len: usize,
    cap: usize,
}

impl ScrollbackRing {
    pub fn new(cap: usize) -> Self {
        Self {
            lines: vec![Vec::new(); cap.max(1)],
            head: 0,
            len: 0,
            cap: cap.max(1),
        }
    }

    pub fn push(&mut self, line: Vec<Cell>) {
        if self.cap == 0 {
            return;
        }

        if self.len < self.cap {
            let idx = (self.head + self.len) % self.cap;
            self.lines[idx] = line;
            self.len += 1;
            return;
        }

        self.lines[self.head] = line;
        self.head = (self.head + 1) % self.cap;
    }
}

pub struct Screen {
    cols: usize,
    rows: usize,
    primary: Buffer,
    alternate: Buffer,
    use_alternate: bool,
    pub scrollback: ScrollbackRing,
    dirty_rows: Vec<bool>,
}

impl Screen {
    pub fn new(cols: usize, rows: usize, scrollback_lines: usize) -> Self {
        let rows = rows.max(1);
        let cols = cols.max(1);
        Self {
            cols,
            rows,
            primary: Buffer::new(cols, rows),
            alternate: Buffer::new(cols, rows),
            use_alternate: false,
            scrollback: ScrollbackRing::new(scrollback_lines),
            dirty_rows: vec![true; rows],
        }
    }

    fn active(&self) -> &Buffer {
        if self.use_alternate {
            &self.alternate
        } else {
            &self.primary
        }
    }

    fn active_mut(&mut self) -> &mut Buffer {
        if self.use_alternate {
            &mut self.alternate
        } else {
            &mut self.primary
        }
    }

    pub fn cols(&self) -> usize {
        self.cols
    }

    pub fn rows(&self) -> usize {
        self.rows
    }

    pub fn cursor(&self) -> Cursor {
        self.active().cursor
    }

    pub fn set_cursor_visible(&mut self, visible: bool) {
        self.active_mut().cursor.visible = visible;
        self.mark_cursor_row_dirty();
    }

    pub fn set_cursor_shape(&mut self, shape: CursorShape) {
        self.active_mut().cursor.shape = shape;
        self.mark_cursor_row_dirty();
    }

    fn mark_cursor_row_dirty(&mut self) {
        let row = self.active().cursor.row.min(self.rows.saturating_sub(1));
        self.mark_dirty(row);
    }

    pub fn resize(&mut self, cols: usize, rows: usize) {
        let cols = cols.max(1);
        let rows = rows.max(1);
        if cols == self.cols && rows == self.rows {
            return;
        }
        self.cols = cols;
        self.rows = rows;
        self.primary.resize(cols, rows);
        self.alternate.resize(cols, rows);
        self.primary.reset_tabstops();
        self.alternate.reset_tabstops();
        self.primary.scroll_top = 0;
        self.primary.scroll_bottom = rows.saturating_sub(1);
        self.alternate.scroll_top = 0;
        self.alternate.scroll_bottom = rows.saturating_sub(1);
        self.dirty_rows.resize(rows, true);
        self.dirty_rows.fill(true);
    }

    pub fn mark_dirty(&mut self, row: usize) {
        if let Some(slot) = self.dirty_rows.get_mut(row) {
            *slot = true;
        }
    }

    pub fn mark_all_dirty(&mut self) {
        self.dirty_rows.fill(true);
    }

    pub fn take_dirty_rows(&mut self) -> Vec<usize> {
        let mut rows = Vec::with_capacity(self.rows);
        for (idx, dirty) in self.dirty_rows.iter_mut().enumerate() {
            if *dirty {
                rows.push(idx);
                *dirty = false;
            }
        }
        rows
    }

    pub fn line(&self, row: usize) -> &[Cell] {
        self.active().line(row)
    }

    pub fn scroll_region(&self) -> (usize, usize) {
        let active = self.active();
        (active.scroll_top, active.scroll_bottom)
    }

    pub fn set_alt_screen(&mut self, enable: bool) {
        if self.use_alternate == enable {
            return;
        }
        self.use_alternate = enable;
        if enable {
            self.alternate.clear_all();
            self.alternate.cursor = Cursor::default();
            self.alternate.attrs = AttrState::default();
            self.alternate.scroll_top = 0;
            self.alternate.scroll_bottom = self.rows.saturating_sub(1);
        }
        self.mark_all_dirty();
    }

    pub fn save_cursor(&mut self) {
        let active = self.active_mut();
        active.saved_cursor = active.cursor;
        active.saved_attrs = active.attrs;
    }

    pub fn restore_cursor(&mut self) {
        let active = self.active_mut();
        active.cursor = active.saved_cursor;
        active.attrs = active.saved_attrs;
        active.clamp_cursor();
        self.mark_cursor_row_dirty();
    }

    pub fn set_cursor(&mut self, row: usize, col: usize) {
        let max_row = self.rows.saturating_sub(1);
        let max_col = self.cols.saturating_sub(1);
        let active = self.active_mut();
        active.cursor.row = row.min(max_row);
        active.cursor.col = col.min(max_col);
        self.mark_cursor_row_dirty();
    }

    pub fn cursor_up(&mut self, n: usize) {
        let active = self.active_mut();
        active.cursor.row = active.cursor.row.saturating_sub(n);
        self.mark_cursor_row_dirty();
    }

    pub fn cursor_down(&mut self, n: usize) {
        let max_row = self.rows.saturating_sub(1);
        let active = self.active_mut();
        active.cursor.row = (active.cursor.row + n).min(max_row);
        self.mark_cursor_row_dirty();
    }

    pub fn cursor_forward(&mut self, n: usize) {
        let max_col = self.cols.saturating_sub(1);
        let active = self.active_mut();
        active.cursor.col = (active.cursor.col + n).min(max_col);
        self.mark_cursor_row_dirty();
    }

    pub fn cursor_back(&mut self, n: usize) {
        let active = self.active_mut();
        active.cursor.col = active.cursor.col.saturating_sub(n);
        self.mark_cursor_row_dirty();
    }

    pub fn carriage_return(&mut self) {
        self.active_mut().cursor.col = 0;
        self.mark_cursor_row_dirty();
    }

    pub fn backspace(&mut self) {
        let active = self.active_mut();
        active.cursor.col = active.cursor.col.saturating_sub(1);
        self.mark_cursor_row_dirty();
    }

    pub fn line_feed(&mut self) {
        let (row, scroll_top, scroll_bottom) = {
            let active = self.active();
            (active.cursor.row, active.scroll_top, active.scroll_bottom)
        };
        if row == scroll_bottom {
            self.scroll_up(1, scroll_top, scroll_bottom);
        } else {
            self.active_mut().cursor.row = (row + 1).min(self.rows.saturating_sub(1));
            self.mark_cursor_row_dirty();
        }
    }

    pub fn reverse_index(&mut self) {
        let (row, scroll_top, scroll_bottom) = {
            let active = self.active();
            (active.cursor.row, active.scroll_top, active.scroll_bottom)
        };
        if row == scroll_top {
            self.scroll_down(1, scroll_top, scroll_bottom);
        } else {
            self.active_mut().cursor.row = row.saturating_sub(1);
            self.mark_cursor_row_dirty();
        }
    }

    pub fn tab(&mut self) {
        let next_col = {
            let active = self.active();
            let mut col = active.cursor.col + 1;
            while col < self.cols && !active.tabstops[col] {
                col += 1;
            }
            col.min(self.cols.saturating_sub(1))
        };
        self.active_mut().cursor.col = next_col;
        self.mark_cursor_row_dirty();
    }

    pub fn set_scroll_region(&mut self, top_1: usize, bottom_1: usize) {
        let top = top_1.saturating_sub(1).min(self.rows.saturating_sub(1));
        let bottom = bottom_1.saturating_sub(1).min(self.rows.saturating_sub(1));
        if top < bottom {
            let active = self.active_mut();
            active.scroll_top = top;
            active.scroll_bottom = bottom;
            active.cursor.row = top;
            active.cursor.col = 0;
            self.mark_all_dirty();
        }
    }

    pub fn reset_scroll_region(&mut self) {
        let bottom = self.rows.saturating_sub(1);
        let active = self.active_mut();
        active.scroll_top = 0;
        active.scroll_bottom = bottom;
    }

    pub fn scroll_up(&mut self, n: usize, top: usize, bottom: usize) {
        let count = n.max(1).min(bottom.saturating_sub(top) + 1);
        let push_to_scrollback =
            !self.use_alternate && top == 0 && bottom == self.rows.saturating_sub(1);

        for _ in 0..count {
            let (removed_phys, removed_line) = {
                let active = self.active_mut();
                let phys = active.rotate_up(top, bottom);
                let line = if push_to_scrollback {
                    Some(active.physical_line(phys).to_vec())
                } else {
                    None
                };
                active.clear_physical_row(phys);
                (phys, line)
            };

            let _ = removed_phys;
            if let Some(line) = removed_line {
                self.scrollback.push(line);
            }
        }

        for row in top..=bottom {
            self.mark_dirty(row);
        }
    }

    pub fn scroll_down(&mut self, n: usize, top: usize, bottom: usize) {
        let count = n.max(1).min(bottom.saturating_sub(top) + 1);
        for _ in 0..count {
            let inserted = self.active_mut().rotate_down(top, bottom);
            self.active_mut().clear_physical_row(inserted);
        }
        for row in top..=bottom {
            self.mark_dirty(row);
        }
    }

    pub fn erase_in_display(&mut self, mode: usize) {
        let (cursor_row, cursor_col) = {
            let cursor = self.active().cursor;
            (cursor.row, cursor.col)
        };

        match mode {
            0 => {
                self.erase_in_line(0);
                for row in cursor_row + 1..self.rows {
                    self.active_mut().line_mut(row).fill(Cell::blank());
                    self.mark_dirty(row);
                }
            }
            1 => {
                self.erase_in_line(1);
                for row in 0..cursor_row {
                    self.active_mut().line_mut(row).fill(Cell::blank());
                    self.mark_dirty(row);
                }
            }
            2 | 3 => {
                for row in 0..self.rows {
                    self.active_mut().line_mut(row).fill(Cell::blank());
                    self.mark_dirty(row);
                }
            }
            _ => {}
        }

        let _ = cursor_col;
    }

    pub fn erase_in_line(&mut self, mode: usize) {
        let (row, col) = {
            let cursor = self.active().cursor;
            (cursor.row, cursor.col)
        };
        let line = self.active_mut().line_mut(row);
        match mode {
            0 => line[col..].fill(Cell::blank()),
            1 => line[..=col].fill(Cell::blank()),
            2 => line.fill(Cell::blank()),
            _ => {}
        }
        self.mark_dirty(row);
    }

    pub fn insert_lines(&mut self, n: usize) {
        let (top, bottom, row) = {
            let a = self.active();
            (a.scroll_top, a.scroll_bottom, a.cursor.row)
        };

        if row < top || row > bottom {
            return;
        }

        let count = n.max(1).min(bottom - row + 1);
        for _ in 0..count {
            let inserted = self.active_mut().rotate_down(row, bottom);
            self.active_mut().clear_physical_row(inserted);
        }
        for r in row..=bottom {
            self.mark_dirty(r);
        }
    }

    pub fn delete_lines(&mut self, n: usize) {
        let (top, bottom, row) = {
            let a = self.active();
            (a.scroll_top, a.scroll_bottom, a.cursor.row)
        };

        if row < top || row > bottom {
            return;
        }

        let count = n.max(1).min(bottom - row + 1);
        for _ in 0..count {
            let removed = self.active_mut().rotate_up(row, bottom);
            self.active_mut().clear_physical_row(removed);
        }
        for r in row..=bottom {
            self.mark_dirty(r);
        }
    }

    pub fn insert_blank_chars(&mut self, n: usize) {
        let (row, col) = {
            let a = self.active();
            (a.cursor.row, a.cursor.col)
        };
        let cols = self.cols;
        let count = n.max(1).min(cols - col);
        let line = self.active_mut().line_mut(row);
        for idx in (col..cols - count).rev() {
            line[idx + count] = line[idx];
        }
        line[col..col + count].fill(Cell::blank());
        self.mark_dirty(row);
    }

    pub fn delete_chars(&mut self, n: usize) {
        let (row, col) = {
            let a = self.active();
            (a.cursor.row, a.cursor.col)
        };
        let cols = self.cols;
        let count = n.max(1).min(cols - col);
        let line = self.active_mut().line_mut(row);
        for idx in col..cols - count {
            line[idx] = line[idx + count];
        }
        line[cols - count..].fill(Cell::blank());
        self.mark_dirty(row);
    }

    pub fn erase_chars(&mut self, n: usize) {
        let (row, col) = {
            let a = self.active();
            (a.cursor.row, a.cursor.col)
        };
        let count = n.max(1).min(self.cols - col);
        self.active_mut().line_mut(row)[col..col + count].fill(Cell::blank());
        self.mark_dirty(row);
    }

    fn current_cell_template(&self) -> Cell {
        let attrs = self.active().attrs;
        let mut fg = attrs.fg;
        let mut bg = attrs.bg;
        if attrs.inverse {
            std::mem::swap(&mut fg, &mut bg);
        }
        let mut flags = CellFlags::empty();
        if attrs.bold {
            flags |= CellFlags::BOLD;
        }
        if attrs.italic {
            flags |= CellFlags::ITALIC;
        }
        if attrs.underline {
            flags |= CellFlags::UNDERLINE;
        }
        if attrs.inverse {
            flags |= CellFlags::INVERSE;
        }

        Cell {
            ch: ' ',
            fg,
            bg,
            flags,
        }
    }

    pub fn put_char(&mut self, ch: char) {
        let width = UnicodeWidthChar::width(ch).unwrap_or(1).max(1);
        let (row, col, cols, scroll_top, scroll_bottom) = {
            let active = self.active();
            (
                active.cursor.row,
                active.cursor.col,
                active.cols,
                active.scroll_top,
                active.scroll_bottom,
            )
        };

        if col >= cols {
            self.active_mut().cursor.col = 0;
            if row == scroll_bottom {
                self.scroll_up(1, scroll_top, scroll_bottom);
            } else {
                self.active_mut().cursor.row = (row + 1).min(self.rows.saturating_sub(1));
            }
        }

        let (row, col) = {
            let active = self.active();
            (active.cursor.row, active.cursor.col)
        };

        if width == 2 && col + 1 >= self.cols {
            self.active_mut().cursor.col = 0;
            if row == scroll_bottom {
                self.scroll_up(1, scroll_top, scroll_bottom);
            } else {
                self.active_mut().cursor.row = (row + 1).min(self.rows.saturating_sub(1));
            }
        }

        let mut cell = self.current_cell_template();
        cell.ch = ch;

        let row = self.active().cursor.row;
        let col = self.active().cursor.col;
        self.active_mut().set_cell(row, col, cell);

        if width == 2 && col + 1 < self.cols {
            let mut cont = cell;
            cont.ch = ' ';
            cont.flags |= CellFlags::WIDE_CONT;
            self.active_mut().set_cell(row, col + 1, cont);
            self.active_mut().cursor.col = (col + 2).min(self.cols.saturating_sub(1));
        } else {
            self.active_mut().cursor.col = (col + 1).min(self.cols);
        }

        if self.active().cursor.col >= self.cols {
            self.active_mut().cursor.col = 0;
            let row = self.active().cursor.row;
            if row == scroll_bottom {
                self.scroll_up(1, scroll_top, scroll_bottom);
            } else {
                self.active_mut().cursor.row = (row + 1).min(self.rows.saturating_sub(1));
            }
        }

        self.mark_dirty(self.active().cursor.row);
        if width == 2 {
            self.mark_dirty(row);
        }
    }

    pub fn set_fg(&mut self, color: Rgb) {
        self.active_mut().attrs.fg = color;
    }

    pub fn set_bg(&mut self, color: Rgb) {
        self.active_mut().attrs.bg = color;
    }

    pub fn reset_attrs(&mut self) {
        self.active_mut().attrs = AttrState::default();
    }

    pub fn attrs_mut(&mut self) -> &mut AttrState {
        &mut self.active_mut().attrs
    }

    pub fn reset(&mut self) {
        self.primary = Buffer::new(self.cols, self.rows);
        self.alternate = Buffer::new(self.cols, self.rows);
        self.use_alternate = false;
        self.mark_all_dirty();
    }

    pub fn extract_selection_text(&self, start: (usize, usize), end: (usize, usize)) -> String {
        let ((start_row, start_col), (end_row, end_col)) = if start <= end {
            (start, end)
        } else {
            (end, start)
        };

        let mut out = String::new();
        let active = self.active();

        for row in start_row..=end_row {
            let line = active.line(row);
            let mut from = 0;
            let mut to = self.cols.saturating_sub(1);
            if row == start_row {
                from = start_col.min(self.cols.saturating_sub(1));
            }
            if row == end_row {
                to = end_col.min(self.cols.saturating_sub(1));
            }

            let mut row_text = String::new();
            for cell in &line[from..=to] {
                if cell.flags.contains(CellFlags::WIDE_CONT) {
                    continue;
                }
                row_text.push(cell.ch);
            }
            out.push_str(row_text.trim_end_matches(' '));
            if row != end_row {
                out.push('\n');
            }
        }

        out
    }
}

pub fn ansi_256_to_rgb(code: u16) -> Rgb {
    match code {
        0 => Rgb::new(0, 0, 0),
        1 => Rgb::new(205, 0, 0),
        2 => Rgb::new(0, 205, 0),
        3 => Rgb::new(205, 205, 0),
        4 => Rgb::new(0, 0, 238),
        5 => Rgb::new(205, 0, 205),
        6 => Rgb::new(0, 205, 205),
        7 => Rgb::new(229, 229, 229),
        8 => Rgb::new(127, 127, 127),
        9 => Rgb::new(255, 0, 0),
        10 => Rgb::new(0, 255, 0),
        11 => Rgb::new(255, 255, 0),
        12 => Rgb::new(92, 92, 255),
        13 => Rgb::new(255, 0, 255),
        14 => Rgb::new(0, 255, 255),
        15 => Rgb::new(255, 255, 255),
        16..=231 => {
            let c = code - 16;
            let r = c / 36;
            let g = (c % 36) / 6;
            let b = c % 6;
            let scale = |v: u16| if v == 0 { 0 } else { (v * 40 + 55) as u8 };
            Rgb::new(scale(r), scale(g), scale(b))
        }
        232..=255 => {
            let gray = ((code - 232) * 10 + 8) as u8;
            Rgb::new(gray, gray, gray)
        }
        _ => DEFAULT_FG,
    }
}

pub fn basic_ansi_to_rgb(code: u16, bright: bool) -> Rgb {
    match (code, bright) {
        (0, false) => Rgb::new(0, 0, 0),
        (1, false) => Rgb::new(205, 49, 49),
        (2, false) => Rgb::new(13, 188, 121),
        (3, false) => Rgb::new(229, 229, 16),
        (4, false) => Rgb::new(36, 114, 200),
        (5, false) => Rgb::new(188, 63, 188),
        (6, false) => Rgb::new(17, 168, 205),
        (7, false) => Rgb::new(229, 229, 229),
        (0, true) => Rgb::new(102, 102, 102),
        (1, true) => Rgb::new(241, 76, 76),
        (2, true) => Rgb::new(35, 209, 139),
        (3, true) => Rgb::new(245, 245, 67),
        (4, true) => Rgb::new(59, 142, 234),
        (5, true) => Rgb::new(214, 112, 214),
        (6, true) => Rgb::new(41, 184, 219),
        (7, true) => Rgb::new(255, 255, 255),
        _ => DEFAULT_FG,
    }
}
