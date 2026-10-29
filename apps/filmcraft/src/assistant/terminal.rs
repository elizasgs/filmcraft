//! A terminal in an egui panel: `alacritty_terminal` owns the PTY and the escape-sequence state,
//! this file draws its grid and turns egui input into the bytes a terminal program expects.

use std::borrow::Cow;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::mpsc::{Receiver, Sender, channel};

use alacritty_terminal::event::{Event, EventListener, WindowSize};
use alacritty_terminal::event_loop::{EventLoop, EventLoopSender, Msg};
use alacritty_terminal::grid::{Dimensions, Scroll};
use alacritty_terminal::index::{Column, Line, Point, Side};
use alacritty_terminal::selection::{Selection, SelectionType};
use alacritty_terminal::sync::FairMutex;
use alacritty_terminal::term::cell::Flags;
use alacritty_terminal::term::{Config, Term, TermMode};
use alacritty_terminal::tty::{self, Options, Shell};
use alacritty_terminal::vte::ansi::{Color, CursorShape, NamedColor, Rgb};
use egui::{Align2, Color32, FontFamily, FontId, Key, Modifiers, Pos2, Rect, Sense, Stroke, Vec2, vec2};

/// What the terminal program is run with.
pub struct Launch {
    pub program: PathBuf,
    pub args: Vec<String>,
    pub env: HashMap<String, String>,
    pub cwd: Option<PathBuf>,
}

/// Forwards the emulator's events to the UI thread, which handles them in [`Terminal::pump`].
#[derive(Clone)]
struct Listener {
    events: Sender<Event>,
    ctx: egui::Context,
}

impl EventListener for Listener {
    fn send_event(&self, event: Event) {
        let _ = self.events.send(event);
        self.ctx.request_repaint();
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
struct GridSize {
    columns: usize,
    lines: usize,
}

impl Dimensions for GridSize {
    fn total_lines(&self) -> usize {
        self.lines
    }
    fn screen_lines(&self) -> usize {
        self.lines
    }
    fn columns(&self) -> usize {
        self.columns
    }
}

/// Colours of the terminal, tuned to sit next to FilmCraft's dark theme.
pub struct Palette {
    pub background: Color32,
    pub foreground: Color32,
    pub cursor: Color32,
    pub selection: Color32,
    ansi: [Color32; 16],
}

impl Palette {
    pub fn dark() -> Self {
        let c = |r, g, b| Color32::from_rgb(r, g, b);
        Palette {
            background: c(0x0e, 0x10, 0x15),
            foreground: c(0xd7, 0xdb, 0xe4),
            cursor: c(0xd9, 0x8b, 0x5f),
            selection: Color32::from_rgba_unmultiplied(0x7a, 0x9c, 0xff, 70),
            ansi: [
                c(0x1b, 0x1e, 0x27),
                c(0xee, 0x6b, 0x7b),
                c(0x8f, 0xd1, 0x8a),
                c(0xe5, 0xc0, 0x7b),
                c(0x7a, 0x9c, 0xff),
                c(0xc6, 0x8a, 0xf0),
                c(0x62, 0xcf, 0xd6),
                c(0xc5, 0xca, 0xd6),
                c(0x4a, 0x50, 0x60),
                c(0xff, 0x8a, 0x98),
                c(0xa9, 0xe6, 0xa4),
                c(0xf5, 0xd7, 0x9a),
                c(0x9a, 0xb6, 0xff),
                c(0xdd, 0xa6, 0xff),
                c(0x86, 0xe3, 0xe9),
                c(0xf1, 0xf3, 0xf8),
            ],
        }
    }

    fn indexed(&self, i: u8) -> Color32 {
        match i {
            0..=15 => self.ansi.get(usize::from(i)).copied().unwrap_or(self.foreground),
            16..=231 => {
                let n = i - 16;
                let level = |v: u8| if v == 0 { 0 } else { 55 + v * 40 };
                Color32::from_rgb(level(n / 36), level((n / 6) % 6), level(n % 6))
            }
            232..=255 => {
                let v = 8 + (i - 232) * 10;
                Color32::from_rgb(v, v, v)
            }
        }
    }

    fn resolve(&self, color: Color, overrides: &alacritty_terminal::term::color::Colors, bold: bool) -> Color32 {
        let rgb = |c: Rgb| Color32::from_rgb(c.r, c.g, c.b);
        match color {
            Color::Spec(c) => rgb(c),
            Color::Indexed(i) => match overrides[usize::from(i)] {
                Some(c) => rgb(c),
                // Bold text in the eight dark colours uses the bright ones.
                None if bold && i < 8 => self.indexed(i + 8),
                None => self.indexed(i),
            },
            Color::Named(name) => {
                if let Some(c) = overrides[name as usize] {
                    return rgb(c);
                }
                match name {
                    NamedColor::Foreground | NamedColor::BrightForeground | NamedColor::DimForeground => self.foreground,
                    NamedColor::Background => self.background,
                    NamedColor::Cursor => self.cursor,
                    other => {
                        let n = other as usize;
                        match n {
                            0..=7 if bold => self.indexed(n as u8 + 8),
                            0..=15 => self.indexed(n as u8),
                            // Dim colours start at `DimBlack`: the dark colour, muted.
                            _ => self.indexed((n.saturating_sub(NamedColor::DimBlack as usize) % 8) as u8).gamma_multiply(0.7),
                        }
                    }
                }
            }
        }
    }
}

pub struct Terminal {
    term: Arc<FairMutex<Term<Listener>>>,
    sender: EventLoopSender,
    events: Receiver<Event>,
    size: GridSize,
    cell: Vec2,
    exited: Option<String>,
    selecting: bool,
}

impl Terminal {
    pub fn spawn(launch: Launch, ctx: &egui::Context) -> Result<Terminal, String> {
        let size = GridSize { columns: 80, lines: 24 };
        let (tx, events) = channel();
        let listener = Listener { events: tx, ctx: ctx.clone() };
        let term = Arc::new(FairMutex::new(Term::new(Config::default(), &size, listener.clone())));
        let window = WindowSize { num_lines: 24, num_cols: 80, cell_width: 8, cell_height: 16 };
        let options = Options {
            shell: Some(Shell::new(launch.program.to_string_lossy().into_owned(), launch.args)),
            working_directory: launch.cwd,
            drain_on_exit: true,
            env: launch.env,
        };
        let pty = tty::new(&options, window, 0).map_err(|e| format!("could not start the terminal: {e}"))?;
        let event_loop =
            EventLoop::new(Arc::clone(&term), listener, pty, options.drain_on_exit, false).map_err(|e| format!("could not start the terminal: {e}"))?;
        let sender = event_loop.channel();
        // The thread ends with the child; its handle is not needed.
        drop(event_loop.spawn());
        Ok(Terminal { term, sender, events, size, cell: vec2(8.0, 16.0), exited: None, selecting: false })
    }

    /// Why the program ended, once it has.
    pub fn exited(&self) -> Option<&str> {
        self.exited.as_deref()
    }

    fn write(&self, bytes: impl Into<Cow<'static, [u8]>>) {
        if self.exited.is_none() {
            let _ = self.sender.send(Msg::Input(bytes.into()));
        }
    }

    /// Handle what the emulator reported since the last frame.
    fn pump(&mut self, ctx: &egui::Context) {
        while let Ok(event) = self.events.try_recv() {
            match event {
                Event::PtyWrite(text) => self.write(text.into_bytes()),
                Event::ClipboardStore(_, text) => ctx.copy_text(text),
                Event::ChildExit(status) => self.exited = Some(status.to_string()),
                Event::Exit if self.exited.is_none() => self.exited = Some("closed".into()),
                _ => {}
            }
        }
    }

    fn resize(&mut self, columns: usize, lines: usize) {
        let size = GridSize { columns: columns.max(8), lines: lines.max(2) };
        if size == self.size {
            return;
        }
        self.size = size;
        self.term.lock().resize(size);
        let window = WindowSize {
            num_lines: u16::try_from(size.lines).unwrap_or(u16::MAX),
            num_cols: u16::try_from(size.columns).unwrap_or(u16::MAX),
            cell_width: self.cell.x as u16,
            cell_height: self.cell.y as u16,
        };
        let _ = self.sender.send(Msg::Resize(window));
    }

    /// Draw the terminal into `rect` and handle its input. `id` makes it focusable.
    pub fn show(&mut self, ui: &mut egui::Ui, rect: Rect, id: egui::Id, palette: &Palette) {
        self.pump(ui.ctx());
        let font = FontId::new(13.0, FontFamily::Monospace);
        let (cell_w, cell_h) = ui.fonts_mut(|f| (f.glyph_width(&font, 'M'), f.row_height(&font)));
        self.cell = vec2(cell_w.max(1.0), (cell_h + 1.0).max(1.0));
        self.resize((rect.width() / self.cell.x) as usize, (rect.height() / self.cell.y) as usize);

        let response = ui.interact(rect, id, Sense::click_and_drag());
        if response.clicked() || response.drag_started() {
            response.request_focus();
        }
        let focused = response.has_focus();
        if focused {
            ui.memory_mut(|m| {
                m.set_focus_lock_filter(id, egui::EventFilter { tab: true, horizontal_arrows: true, vertical_arrows: true, escape: true });
            });
            self.handle_keys(ui);
        }
        self.handle_pointer(ui, &response, rect);

        let painter = ui.painter_at(rect);
        painter.rect_filled(rect, 0.0, palette.background);
        let term = self.term.lock();
        let mode = *term.mode();
        let content = term.renderable_content();
        let offset = content.display_offset as i32;
        let origin = rect.min;
        let at = |line: i32, column: usize| Pos2::new(origin.x + column as f32 * self.cell.x, origin.y + line as f32 * self.cell.y);

        // Runs of cells with the same colours are one text draw call.
        struct Run {
            line: i32,
            column: usize,
            text: String,
            fg: Color32,
            underline: bool,
            strike: bool,
        }
        let mut run: Option<Run> = None;
        let flush = |run: &mut Option<Run>, painter: &egui::Painter| {
            if let Some(r) = run.take() {
                let pos = at(r.line, r.column);
                let trimmed = r.text.trim_end();
                if !trimmed.is_empty() {
                    painter.text(pos, Align2::LEFT_TOP, trimmed, font.clone(), r.fg);
                }
                let width = r.text.chars().count() as f32 * self.cell.x;
                if r.underline {
                    let y = pos.y + self.cell.y - 2.0;
                    painter.line_segment([Pos2::new(pos.x, y), Pos2::new(pos.x + width, y)], Stroke::new(1.0, r.fg));
                }
                if r.strike {
                    let y = pos.y + self.cell.y * 0.5;
                    painter.line_segment([Pos2::new(pos.x, y), Pos2::new(pos.x + width, y)], Stroke::new(1.0, r.fg));
                }
            }
        };
        let selection = content.selection;
        for indexed in content.display_iter {
            let cell = indexed.cell;
            if cell.flags.contains(Flags::WIDE_CHAR_SPACER) {
                continue;
            }
            let line = indexed.point.line.0 + offset;
            let column = indexed.point.column.0;
            let bold = cell.flags.contains(Flags::BOLD);
            let (mut fg, mut bg) = (palette.resolve(cell.fg, content.colors, bold), palette.resolve(cell.bg, content.colors, false));
            if cell.flags.contains(Flags::INVERSE) {
                std::mem::swap(&mut fg, &mut bg);
            }
            if cell.flags.contains(Flags::DIM) {
                fg = fg.gamma_multiply(0.65);
            }
            let width = if cell.flags.contains(Flags::WIDE_CHAR) { 2.0 } else { 1.0 };
            if bg != palette.background {
                painter.rect_filled(Rect::from_min_size(at(line, column), vec2(self.cell.x * width, self.cell.y)), 0.0, bg);
            }
            if selection.is_some_and(|s| s.contains(indexed.point)) {
                painter.rect_filled(Rect::from_min_size(at(line, column), vec2(self.cell.x * width, self.cell.y)), 0.0, palette.selection);
            }
            let ch = if cell.flags.contains(Flags::HIDDEN) { ' ' } else { cell.c };
            let underline = cell.flags.intersects(Flags::ALL_UNDERLINES);
            let strike = cell.flags.contains(Flags::STRIKEOUT);
            let continues = run
                .as_ref()
                .is_some_and(|r| r.line == line && r.fg == fg && r.underline == underline && r.strike == strike && r.column + r.text.chars().count() == column);
            if !continues {
                flush(&mut run, &painter);
                run = Some(Run { line, column, text: String::new(), fg, underline, strike });
            }
            if let Some(r) = run.as_mut() {
                r.text.push(ch);
                if width > 1.0 {
                    // The spacer cell after a wide glyph is skipped above; keep columns aligned.
                    r.text.push('\u{200B}');
                }
            }
        }
        flush(&mut run, &painter);

        if content.cursor.shape != CursorShape::Hidden && mode.contains(TermMode::SHOW_CURSOR) {
            let p = content.cursor.point;
            let cursor = Rect::from_min_size(at(p.line.0 + offset, p.column.0), vec2(self.cell.x, self.cell.y));
            if focused {
                painter.rect_filled(cursor, 1.0, palette.cursor.gamma_multiply(0.85));
            } else {
                painter.rect_stroke(cursor.shrink(0.5), 1.0, Stroke::new(1.0, palette.cursor), egui::StrokeKind::Inside);
            }
        }
    }

    fn handle_pointer(&mut self, ui: &egui::Ui, response: &egui::Response, rect: Rect) {
        let scroll = ui.input(|i| i.smooth_scroll_delta.y);
        if response.hovered() && scroll.abs() > 0.5 {
            let lines = (scroll / self.cell.y).round() as i32;
            if lines != 0 {
                self.term.lock().scroll_display(Scroll::Delta(lines));
            }
        }
        let point_at = |term: &Term<Listener>, pos: Pos2| {
            let column = (((pos.x - rect.min.x) / self.cell.x).max(0.0) as usize).min(self.size.columns.saturating_sub(1));
            let line = (((pos.y - rect.min.y) / self.cell.y).max(0.0) as i32).min(self.size.lines as i32 - 1);
            let side = if ((pos.x - rect.min.x) / self.cell.x).fract() < 0.5 { Side::Left } else { Side::Right };
            (Point::new(Line(line - term.grid().display_offset() as i32), Column(column)), side)
        };
        let Some(pos) = response.interact_pointer_pos() else { return };
        if response.drag_started() {
            let mut term = self.term.lock();
            let (point, side) = point_at(&term, pos);
            term.selection = Some(Selection::new(SelectionType::Simple, point, side));
            self.selecting = true;
        } else if response.dragged() && self.selecting {
            let mut term = self.term.lock();
            let (point, side) = point_at(&term, pos);
            if let Some(selection) = term.selection.as_mut() {
                selection.update(point, side);
            }
        } else if response.clicked() {
            self.term.lock().selection = None;
        }
        if response.drag_stopped() {
            self.selecting = false;
        }
    }

    fn handle_keys(&mut self, ui: &egui::Ui) {
        let mode = *self.term.lock().mode();
        let events = ui.input(|i| i.events.clone());
        for event in events {
            match event {
                egui::Event::Text(text) => self.write(text.into_bytes()),
                egui::Event::Paste(text) => {
                    if mode.contains(TermMode::BRACKETED_PASTE) {
                        self.write(format!("\x1b[200~{}\x1b[201~", text.replace('\x1b', "")).into_bytes());
                    } else {
                        self.write(text.replace("\r\n", "\r").replace('\n', "\r").into_bytes());
                    }
                }
                egui::Event::Copy | egui::Event::Cut => {
                    let selected = self.term.lock().selection_to_string();
                    if let Some(text) = selected.filter(|t| !t.is_empty()) {
                        ui.ctx().copy_text(text);
                    }
                }
                egui::Event::Key { key, pressed: true, modifiers, .. } => {
                    if let Some(bytes) = key_bytes(key, modifiers, mode) {
                        self.term.lock().scroll_display(Scroll::Bottom);
                        self.write(bytes);
                    }
                }
                _ => {}
            }
        }
    }
}

/// The bytes a terminal program expects for a key that is not plain text.
fn key_bytes(key: Key, m: Modifiers, mode: TermMode) -> Option<Vec<u8>> {
    let app_cursor = mode.contains(TermMode::APP_CURSOR);
    let csi = |code: &str| format!("\x1b[{code}").into_bytes();
    let arrow = |letter: char| {
        if m.shift || m.alt || m.ctrl {
            let modifier = 1 + u8::from(m.shift) + 2 * u8::from(m.alt) + 4 * u8::from(m.ctrl);
            format!("\x1b[1;{modifier}{letter}").into_bytes()
        } else if app_cursor {
            format!("\x1bO{letter}").into_bytes()
        } else {
            format!("\x1b[{letter}").into_bytes()
        }
    };
    Some(match key {
        Key::Enter if m.shift || m.alt => b"\x1b\r".to_vec(),
        Key::Enter => b"\r".to_vec(),
        Key::Backspace if m.alt => b"\x1b\x7f".to_vec(),
        Key::Backspace => b"\x7f".to_vec(),
        Key::Tab if m.shift => csi("Z"),
        Key::Tab => b"\t".to_vec(),
        Key::Escape => b"\x1b".to_vec(),
        Key::ArrowUp => arrow('A'),
        Key::ArrowDown => arrow('B'),
        Key::ArrowRight => arrow('C'),
        Key::ArrowLeft => arrow('D'),
        Key::Home => csi("H"),
        Key::End => csi("F"),
        Key::PageUp => csi("5~"),
        Key::PageDown => csi("6~"),
        Key::Delete => csi("3~"),
        Key::Insert => csi("2~"),
        Key::F1 => b"\x1bOP".to_vec(),
        Key::F2 => b"\x1bOQ".to_vec(),
        Key::F3 => b"\x1bOR".to_vec(),
        Key::F4 => b"\x1bOS".to_vec(),
        // Control letters: Ctrl+A is 0x01 … Ctrl+Z is 0x1A. (Command is not Control on macOS.)
        k if m.ctrl && !m.command => {
            let letter = k.name().chars().next().filter(char::is_ascii_alphabetic)?;
            vec![(letter.to_ascii_lowercase() as u8) & 0x1f]
        }
        // Option+letter is Meta: ESC then the letter.
        k if m.alt && !m.command => {
            let letter = k.name().chars().next().filter(char::is_ascii_alphanumeric)?;
            vec![0x1b, letter.to_ascii_lowercase() as u8]
        }
        _ => return None,
    })
}
