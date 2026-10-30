// SPDX-License-Identifier: Apache-2.0
//! A screen, drawn: the thread-drawn frame, the map of steps, the page and
//! what it asks. Nothing here reads keys or keeps state between frames.
use super::look::{Ink, Look};
use crate::setup::{
    Step,
    flow::{Ask, Block, Choice, Mark, Screen, Spot, Tick, exact},
};
use ratatui::{
    Frame,
    buffer::Buffer,
    layout::{Position, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

/// The full screen, with its frame and map, needs this much room.
pub(crate) const FULL: (u16, u16) = (80, 24);
/// Below this there is no room to set up in.
pub(crate) const LEAST: (u16, u16) = (40, 12);
/// The widest the page reads well at.
const MEASURE: u16 = 80;
/// The column the divider between the map and the page is drawn in.
const DIVIDER: u16 = 21;
/// How many of a page's rows are kept for it when what it asks is long.
const LEAST_PAGE: usize = 3;
/// The loader, beat by beat.
const BEATS: [&str; 4] = ["●──", "─●─", "──●", "─●─"];
/// "Øphio", drawn in threads.
const WORDMARK: [&str; 8] = [
    "  ╭────╮ ╱         ╷                ",
    "╭─╯    ╰╱╮         │       ◆        ",
    "│      ╱ │  ╭───╮  ├───╮   ╷  ╭───╮ ",
    "│     ╱  │  │   │  │   │   │  │   │ ",
    "│    ╱   │  │   │  │   │   │  │   │ ",
    "╰╮  ╱  ╭─╯  ├───╯  ╵   ╵   ╵  ╰───╯ ",
    " ╰─╱───╯    │                       ",
    "  ╱         ╵                       ",
];
/// Rows of the terminal outside the page, in either layout.
const BEYOND: u16 = 4;
/// The quiet margin around a QR code, in modules.
const QUIET: usize = 2;

/// Something on the screen that can be picked.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Item {
    Tick(usize),
    /// Going on with the ticks as they stand.
    GoOn,
    Choice(&'static str),
}

/// What can be picked on `screen`, in order.
pub(crate) fn items(screen: &Screen) -> Vec<Item> {
    let choices = |choices: &[Choice]| -> Vec<Item> {
        choices
            .iter()
            .map(|choice| Item::Choice(choice.id))
            .collect()
    };
    match &screen.ask {
        Ask::Nothing | Ask::Text { .. } => Vec::new(),
        Ask::Choose(list) => choices(list),
        Ask::Ticks {
            ticks,
            choices: list,
        } => (0..ticks.len())
            .map(Item::Tick)
            .chain([Item::GoOn])
            .chain(choices(list))
            .collect(),
    }
}

/// What is said beneath commands drawn with escapes, whose rows are marked.
pub(crate) const MARKED: &str =
    "Shown with escapes beside ┆: each \\u{…} is one character, and \\\\ is a backslash.";

/// What the person has done on the screen shown.
#[derive(Debug, Clone, Default, PartialEq)]
pub(crate) struct View {
    pub selected: Option<Item>,
    /// The ticks as they stand.
    pub ticks: Vec<bool>,
    pub text: String,
    /// Rows the page is scrolled down by.
    pub scroll: u16,
    /// How far a new step has come in, in thousandths.
    pub entered: u16,
    /// The loader's beat.
    pub beat: usize,
}

impl View {
    /// A screen as it first shows.
    pub fn new(screen: &Screen) -> Self {
        let ticks = match &screen.ask {
            Ask::Ticks { ticks, .. } => ticks.iter().map(|tick| tick.on).collect(),
            _ => Vec::new(),
        };
        let recommended = screen
            .choices()
            .iter()
            .find(|choice| choice.recommended)
            .map(|choice| Item::Choice(choice.id));
        let selected = match &screen.ask {
            Ask::Ticks { .. } => Some(Item::GoOn),
            // Where nothing is recommended, as with allowing a command or
            // pairing a device, Enter does nothing until one is picked.
            _ => recommended.or_else(|| match items(screen)[..] {
                [only] => Some(only),
                _ => None,
            }),
        };
        Self {
            selected,
            ticks,
            entered: 1000,
            ..Self::default()
        }
    }

    fn on(&self, tick: usize) -> bool {
        self.ticks.get(tick).copied().unwrap_or(false)
    }

    fn entering(&self) -> bool {
        self.entered < 1000
    }

    /// `n` scaled by how far the step has come in.
    fn grown(&self, n: usize) -> usize {
        (n * usize::from(self.entered.min(1000))).div_ceil(1000)
    }
}

/// Where a screen was drawn, for keys and clicks.
#[derive(Debug, Clone, Default, PartialEq)]
pub(crate) struct Drawn {
    /// Rows that pick an item when clicked.
    pub hits: Vec<(Rect, Item)>,
    pub links: Vec<LinkSpot>,
    pub cursor: Option<Position>,
    /// How far the page can scroll.
    pub most: u16,
    /// How many of the page's rows show at once.
    pub page: u16,
    /// Whether what the screen asks was drawn: a window too small shows
    /// none of it.
    pub asks: bool,
}

/// A link as drawn, to be made clickable.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct LinkSpot {
    pub at: Position,
    pub text: String,
    pub url: String,
    pub style: Style,
}

/// A row of the page or of what it asks.
#[derive(Debug, Clone, Default)]
struct Row {
    spans: Vec<Span<'static>>,
    /// The style of the whole row, beneath its spans.
    fill: Option<Style>,
    item: Option<Item>,
    /// A link on the row: the column it starts at, its text and address.
    link: Option<(u16, String, String, Style)>,
}

fn row(spans: Vec<Span<'static>>) -> Row {
    Row {
        spans,
        ..Row::default()
    }
}

fn blank() -> Row {
    Row::default()
}

fn span(text: impl Into<String>, style: Style) -> Span<'static> {
    Span::styled(text.into(), style)
}

fn width(spans: &[Span<'_>]) -> usize {
    spans.iter().map(Span::width).sum()
}

fn columns(n: usize) -> u16 {
    u16::try_from(n).unwrap_or(u16::MAX)
}

/// Text with its pieces styled.
type Piece = (String, Style);

/// `text` in `style`, with `code` in the accent (or kept in its backticks
/// when there is no colour) and the arrows of a path, such as Settings › Apps,
/// in bloom.
fn rich(text: &str, style: Style, look: Look) -> Vec<Piece> {
    let mut pieces = Vec::new();
    let paired = text.matches('`').count().is_multiple_of(2);
    for (index, part) in text.split('`').enumerate() {
        if paired && index % 2 == 1 {
            if look.mono() {
                pieces.push((format!("`{part}`"), style));
            } else {
                pieces.push((part.to_owned(), look.ink(Ink::Accent)));
            }
            continue;
        }
        let part = if paired || index == 0 {
            part.to_owned()
        } else {
            format!("`{part}")
        };
        for (n, bit) in part.split('›').enumerate() {
            if n > 0 {
                pieces.push(("›".into(), style.patch(look.ink(Ink::Bloom))));
            }
            if !bit.is_empty() {
                pieces.push((bit.to_owned(), style));
            }
        }
    }
    pieces
}

/// Pieces wrapped to `width` columns: the first line after `first`, the
/// rest after `rest`. A word too long for a line is broken where it reaches
/// the edge.
fn wrap(
    pieces: &[Piece],
    width: usize,
    first: Vec<Span<'static>>,
    rest: &[Span<'static>],
) -> Vec<Vec<Span<'static>>> {
    enum Token<'a> {
        Word(&'a str, Style),
        Space(Style),
        Break,
    }
    let mut tokens = Vec::new();
    for (text, style) in pieces {
        for (n, line) in text.split('\n').enumerate() {
            if n > 0 {
                tokens.push(Token::Break);
            }
            let mut start = None;
            for (at, c) in line.char_indices() {
                if c.is_whitespace() {
                    if let Some(from) = start.take() {
                        tokens.push(Token::Word(&line[from..at], *style));
                    }
                    tokens.push(Token::Space(*style));
                } else if start.is_none() {
                    start = Some(at);
                }
            }
            if let Some(from) = start {
                tokens.push(Token::Word(&line[from..], *style));
            }
        }
    }
    let rest_width = width_of(rest);
    let mut lines = Vec::new();
    let mut used = width_of(&first);
    let mut line = first;
    let mut empty = true;
    let mut space: Option<Style> = None;
    let push = |line: &mut Vec<Span<'static>>, text: &str, style: Style| match line.last_mut() {
        Some(last) if last.style == style => last.content.to_mut().push_str(text),
        _ => line.push(Span::styled(text.to_owned(), style)),
    };
    for token in tokens {
        match token {
            Token::Space(style) => {
                if !empty {
                    space = Some(style);
                }
            }
            Token::Break => {
                lines.push(std::mem::replace(&mut line, rest.to_vec()));
                used = rest_width;
                empty = true;
                space = None;
            }
            Token::Word(word, style) => {
                let wide = word.width();
                let gap = usize::from(space.is_some() && !empty);
                if used + gap + wide <= width {
                    if let (Some(space), false) = (space, empty) {
                        push(&mut line, " ", space);
                    }
                    push(&mut line, word, style);
                    used += gap + wide;
                } else if !empty && rest_width + wide <= width {
                    lines.push(std::mem::replace(&mut line, rest.to_vec()));
                    push(&mut line, word, style);
                    used = rest_width + wide;
                } else {
                    if let (Some(space), false) = (space, empty)
                        && used < width
                    {
                        push(&mut line, " ", space);
                        used += 1;
                    }
                    for c in word.chars() {
                        let wide = c.width().unwrap_or(0);
                        if used + wide > width && used > rest_width {
                            lines.push(std::mem::replace(&mut line, rest.to_vec()));
                            used = rest_width;
                        }
                        push(&mut line, c.encode_utf8(&mut [0; 4]), style);
                        used += wide;
                    }
                }
                empty = false;
                space = None;
            }
        }
    }
    lines.push(line);
    lines
}

/// `text` exactly as it is, such as a command as it would run, wrapped to
/// `width` columns after `first` and then `rest`. Every space is kept, and it
/// breaks only where a line runs out. Spaces that would fall where it breaks,
/// or end it, are drawn as faint dots, so none goes unseen: `text` is to be
/// written with [`exact`], which leaves no dot of its own to be taken for one.
fn literal(
    text: &str,
    style: Style,
    dots: Style,
    width: usize,
    first: Vec<Span<'static>>,
    rest: &[Span<'static>],
) -> Vec<Vec<Span<'static>>> {
    struct Lines<'a> {
        done: Vec<Vec<Span<'static>>>,
        line: Vec<Span<'static>>,
        used: usize,
        width: usize,
        rest: &'a [Span<'static>],
        rest_width: usize,
    }
    impl Lines<'_> {
        fn put(&mut self, text: &str, wide: usize, style: Style) {
            if self.used + wide > self.width && self.used > self.rest_width {
                self.done
                    .push(std::mem::replace(&mut self.line, self.rest.to_vec()));
                self.used = self.rest_width;
            }
            match self.line.last_mut() {
                Some(last) if last.style == style => last.content.to_mut().push_str(text),
                _ => self.line.push(Span::styled(text.to_owned(), style)),
            }
            self.used += wide;
        }
        fn spaces(&mut self, n: usize, style: Style, shown: &str) {
            for _ in 0..n {
                self.put(shown, 1, style);
            }
        }
    }
    let mut lines = Lines {
        done: Vec::new(),
        used: width_of(&first),
        line: first,
        width,
        rest,
        rest_width: width_of(rest),
    };
    let mut spaces = 0;
    for c in text.chars() {
        if c == ' ' {
            spaces += 1;
            continue;
        }
        let wide = c.width().unwrap_or(0);
        if spaces > 0 {
            if lines.used + spaces + wide <= width {
                lines.spaces(spaces, style, " ");
            } else {
                lines.spaces(spaces, dots, "·");
            }
            spaces = 0;
        }
        lines.put(c.encode_utf8(&mut [0; 4]), wide, style);
    }
    lines.spaces(spaces, dots, "·");
    lines.done.push(lines.line);
    lines.done
}

fn width_of(spans: &[Span<'static>]) -> usize {
    width(spans)
}

fn indent(n: usize) -> Vec<Span<'static>> {
    if n == 0 {
        Vec::new()
    } else {
        vec![Span::raw(" ".repeat(n))]
    }
}

fn rows(lines: Vec<Vec<Span<'static>>>) -> Vec<Row> {
    lines.into_iter().map(row).collect()
}

/// A paragraph.
fn paragraph(text: &str, style: Style, look: Look, width: usize, hang: usize) -> Vec<Row> {
    rows(wrap(
        &rich(text, style, look),
        width,
        Vec::new(),
        &indent(hang),
    ))
}

fn mark(mark: Mark, beat: usize, look: Look) -> Span<'static> {
    match mark {
        Mark::Ok => span("✓ ", look.bold(Ink::Ok)),
        Mark::Warn => span("! ", look.bold(Ink::Warn)),
        Mark::Stop => span("✗ ", look.bold(Ink::Stop)),
        Mark::Info => span("· ", look.ink(Ink::Faint)),
        Mark::Wait => span(
            format!("{} ", BEATS[beat % BEATS.len()]),
            look.ink(Ink::Bloom),
        ),
    }
}

/// A run of facts, their details lined up.
fn facts(run: &[(Mark, &str, &str)], view: &View, look: Look, width: usize) -> Vec<Row> {
    let marks: Vec<Span<'static>> = run
        .iter()
        .map(|(m, _, _)| mark(*m, view.beat, look))
        .collect();
    let column = run
        .iter()
        .zip(&marks)
        .map(|((_, label, _), mark)| mark.width() + label.width())
        .max()
        .unwrap_or(0)
        .min(width / 2)
        + 2;
    let mut out = Vec::new();
    for ((_, label, detail), mark) in run.iter().zip(marks) {
        let hang = mark.width();
        let lead = vec![mark, span(*label, look.bold(Ink::Text))];
        let used = width_of(&lead);
        let detail = rich(detail, look.ink(Ink::Soft), look);
        if detail.is_empty() {
            out.extend(rows(wrap(&[], width, lead, &indent(hang))));
        } else if used + 2 <= column && width >= column + 16 {
            let mut first = lead;
            first.push(Span::raw(" ".repeat(column - used)));
            out.extend(rows(wrap(&detail, width, first, &indent(column))));
        } else {
            out.extend(rows(wrap(&[], width, lead, &indent(hang))));
            out.extend(rows(wrap(&detail, width, indent(hang), &indent(hang))));
        }
    }
    out
}

/// Numbered instructions.
fn steps(steps: &[String], look: Look, width: usize) -> Vec<Row> {
    let wide = steps.len().to_string().len();
    let mut out = Vec::new();
    for (n, step) in steps.iter().enumerate() {
        let number = span(format!("{:>wide$}  ", n + 1), look.bold(Ink::Bloom));
        let text = rich(step, look.ink(Ink::Text), look);
        out.extend(rows(wrap(&text, width, vec![number], &indent(wide + 2))));
    }
    out
}

/// Instructions on a phone's outline, as the phone shows the way.
fn phone(list: &[String], look: Look, width: usize) -> Vec<Row> {
    let outer = width.clamp(24, 46);
    let inner = outer - 2;
    let edge = look.ink(Ink::LineStrong);
    let framed = |mut spans: Vec<Span<'static>>| {
        let used = width_of(&spans);
        spans.insert(0, span("│", edge));
        spans.push(Span::raw(" ".repeat(inner.saturating_sub(used))));
        spans.push(span("│", edge));
        row(spans)
    };
    let centred = |spans: Vec<Span<'static>>| {
        let used = width_of(&spans);
        let mut out = indent((inner.saturating_sub(used)) / 2);
        out.extend(spans);
        framed(out)
    };
    let mut out = vec![row(vec![span(format!("╭{}╮", "─".repeat(inner)), edge)])];
    out.push(centred(vec![
        span("╶────╴", look.ink(Ink::Line)),
        span("  ●", look.ink(Ink::Bloom)),
    ]));
    out.push(framed(Vec::new()));
    for line in steps(list, look, inner - 4) {
        let mut spans = indent(2);
        spans.extend(line.spans);
        out.push(framed(spans));
    }
    out.push(framed(Vec::new()));
    out.push(centred(vec![span("────────", look.ink(Ink::LineStrong))]));
    out.push(row(vec![span(format!("╰{}╯", "─".repeat(inner)), edge)]));
    out
}

/// A link: its label, then its address, broken where it reaches the edge.
fn link(label: &str, url: &str, look: Look, width: usize) -> Vec<Row> {
    let style = look.ink(Ink::Bloom).add_modifier(Modifier::UNDERLINED);
    let lead = format!("{label}  ");
    let mut column = lead.width();
    if column + 12 > width {
        column = 0;
    }
    let mut out = Vec::new();
    let mut first = true;
    let mut piece = String::new();
    let mut used = 0;
    let finish = |piece: &mut String, first: &mut bool, out: &mut Vec<Row>| {
        let mut spans = Vec::new();
        let at = if *first && column > 0 {
            spans.push(span(lead.clone(), look.bold(Ink::Soft)));
            column
        } else if *first {
            out.push(row(vec![span(label.to_owned(), look.bold(Ink::Soft))]));
            0
        } else {
            column
        };
        if !*first || column == 0 {
            spans.extend(indent(at));
        }
        spans.push(span(piece.clone(), style));
        out.push(Row {
            link: Some((columns(at), std::mem::take(piece), url.to_owned(), style)),
            ..row(spans)
        });
        *first = false;
    };
    for c in url.chars() {
        let wide = c.width().unwrap_or(0);
        if used + wide > width - column && !piece.is_empty() {
            finish(&mut piece, &mut first, &mut out);
            used = 0;
        }
        piece.push(c);
        used += wide;
    }
    finish(&mut piece, &mut first, &mut out);
    out
}

/// A QR code, two modules to a row: black on white with a quiet margin, so
/// a phone can read it whatever the terminal's colours.
fn qr(data: &str, look: Look) -> Option<Vec<Row>> {
    use qrcode::{EcLevel, QrCode};
    let code = QrCode::with_error_correction_level(data.as_bytes(), EcLevel::L).ok()?;
    let size = code.width();
    let modules = code.to_colors();
    let side = size + 2 * QUIET;
    // Outside the code and its margin, as the page is.
    let dark = |x: usize, y: usize| -> Option<bool> {
        if x >= side || y >= side {
            return None;
        }
        let inside = (QUIET..QUIET + size).contains(&x) && (QUIET..QUIET + size).contains(&y);
        Some(inside && modules[(y - QUIET) * size + (x - QUIET)] == qrcode::Color::Dark)
    };
    let (black, white) = match look.depth {
        super::look::Depth::True => (Color::Rgb(0, 0, 0), Color::Rgb(255, 255, 255)),
        super::look::Depth::Many => (Color::Indexed(16), Color::Indexed(231)),
        _ => (Color::Black, Color::White),
    };
    let page = look.base().bg.unwrap_or(Color::Reset);
    let mut out = Vec::new();
    for y in (0..side).step_by(2) {
        let mut spans = Vec::new();
        for x in 0..side {
            let (top, bottom) = (dark(x, y), dark(x, y + 1));
            if look.mono() {
                // Dark modules as blocks, as `ophio pair` prints them.
                let c = match (top == Some(true), bottom == Some(true)) {
                    (true, true) => "█",
                    (true, false) => "▀",
                    (false, true) => "▄",
                    (false, false) => " ",
                };
                spans.push(Span::raw(c));
                continue;
            }
            let color = |module: Option<bool>| match module {
                Some(true) => black,
                Some(false) => white,
                None => page,
            };
            spans.push(span("▀", Style::new().fg(color(top)).bg(color(bottom))));
        }
        out.push(row(spans));
    }
    Some(out)
}

fn qr_caption(caption: &str, look: Look, width: usize, code: usize) -> Vec<Row> {
    let mut out = paragraph(caption, look.ink(Ink::Soft), look, width, 0);
    for line in &mut out {
        let used = width_of(&line.spans);
        if used < code {
            line.spans
                .insert(0, Span::raw(" ".repeat((code - used) / 2)));
        }
    }
    out
}

/// What the page says, laid out `width` wide. `art` draws instructions on a
/// phone outline.
struct Page<'a> {
    screen: &'a Screen,
    view: &'a View,
    look: Look,
    width: usize,
    art: bool,
    /// Rows the page has, and the terminal has beyond them: to say how
    /// tall it needs to be for a QR code.
    room: (usize, usize),
    /// Whether the QR code is drawn beside the page instead.
    beside: bool,
}

impl Page<'_> {
    fn rows(&self) -> Vec<Row> {
        let (screen, look, width) = (self.screen, self.look, self.width);
        let mut out: Vec<Row> = Vec::new();
        let mut blocks = screen.blocks.iter().peekable();
        let mut last_fact = false;
        while let Some(block) = blocks.next() {
            let is_fact = matches!(block, Block::Fact { .. });
            if self.beside && matches!(block, Block::Qr { .. }) {
                continue;
            }
            if !out.is_empty() && !(is_fact && last_fact) {
                out.push(blank());
            }
            last_fact = is_fact;
            match block {
                Block::Text(text) => {
                    out.extend(paragraph(text, look.ink(Ink::Text), look, width, 0));
                }
                Block::Fact {
                    mark,
                    label,
                    detail,
                } => {
                    let mut run = vec![(*mark, label.as_str(), detail.as_str())];
                    while let Some(Block::Fact {
                        mark,
                        label,
                        detail,
                    }) = blocks.peek()
                    {
                        run.push((*mark, label.as_str(), detail.as_str()));
                        blocks.next();
                    }
                    out.extend(facts(&run, self.view, look, width));
                }
                Block::Command(command) => {
                    // Commands together are drawn one way. Every row of one
                    // drawn with escapes is marked, so a row scrolled apart
                    // from what explains the escapes still says it has them.
                    let mut run = vec![command.as_str()];
                    while let Some(Block::Command(command)) = blocks.peek() {
                        run.push(command);
                        blocks.next();
                    }
                    let (run, escaped) = exact(&run);
                    let (style, dots) = (look.ink(Ink::Accent), look.ink(Ink::Faint));
                    let (first, rest) = if escaped {
                        let mark = look.ink(Ink::Bloom);
                        (
                            vec![span("▸", look.ink(Ink::Filament)), span("┆ ", mark)],
                            vec![span(" ┆ ", mark)],
                        )
                    } else {
                        (vec![span("▸ ", look.ink(Ink::Filament))], indent(2))
                    };
                    for (n, command) in run.iter().enumerate() {
                        if n > 0 {
                            out.push(blank());
                        }
                        out.extend(rows(literal(
                            command,
                            style,
                            dots,
                            width,
                            first.clone(),
                            &rest,
                        )));
                    }
                    if escaped {
                        out.extend(paragraph(MARKED, look.ink(Ink::Faint), look, width, 0));
                    }
                }
                Block::Qr { data, caption } => out.extend(self.code(data, caption)),
                Block::Link { label, url } => out.extend(link(label, url, look, width)),
                Block::Steps(list) if self.art && screen.step == Step::Phone => {
                    out.extend(phone(list, look, width));
                }
                Block::Steps(list) => out.extend(steps(list, look, width)),
            }
        }
        out
    }

    /// A QR code with its caption, or what to do if it can't fit.
    fn code(&self, data: &str, caption: &str) -> Vec<Row> {
        let Some(mut code) = qr(data, self.look) else {
            return Vec::new();
        };
        let wide = code.first().map_or(0, |row| width_of(&row.spans));
        let (rows, beyond) = self.room;
        if wide > self.width || code.len() > rows {
            let need = code.len() + beyond;
            let say = if wide > self.width {
                format!(
                    "Make this window wider, at least {} columns, to show the code here.",
                    wide + 30
                )
            } else {
                format!("Make this window taller, at least {need} rows, to show the code here.")
            };
            return facts(
                &[(Mark::Warn, "Code", say.as_str())],
                self.view,
                self.look,
                self.width,
            );
        }
        code.extend(qr_caption(caption, self.look, self.width, wide));
        code
    }
}

/// The QR code a screen shows, if it shows one.
fn qr_block(screen: &Screen) -> Option<(&str, &str)> {
    screen.blocks.iter().find_map(|block| match block {
        Block::Qr { data, caption } => Some((data.as_str(), caption.as_str())),
        _ => None,
    })
}

/// The rows of what a screen asks, as they are made.
struct Asking<'a> {
    view: &'a View,
    look: Look,
    width: usize,
    /// Only the selected item's details.
    tight: bool,
    out: Vec<Row>,
}

impl Asking<'_> {
    /// `[mark] `, the mark in `ink`.
    fn bracket(&self, mark: impl Into<String>, ink: Ink) -> Vec<Span<'static>> {
        let look = self.look;
        vec![
            span("[", look.ink(Ink::Faint)),
            span(mark, look.bold(ink)),
            span("] ", look.ink(Ink::Faint)),
        ]
    }

    /// An item: `first` and its label, then its detail, `hang` in.
    fn entry(
        &mut self,
        item: Item,
        mut first: Vec<Span<'static>>,
        label: Vec<Piece>,
        detail: Option<&str>,
        hang: usize,
    ) {
        let look = self.look;
        let selected = self.view.selected == Some(item);
        first.insert(
            0,
            if selected {
                span("╾─◆ ", look.bold(Ink::Bloom))
            } else {
                Span::raw("    ")
            },
        );
        let label: Vec<Piece> = label
            .into_iter()
            .map(|(text, style)| {
                if selected {
                    (text, style.add_modifier(Modifier::BOLD))
                } else {
                    (text, style)
                }
            })
            .collect();
        for (n, line) in wrap(&label, self.width, first, &indent(hang))
            .into_iter()
            .enumerate()
        {
            self.out.push(Row {
                fill: (selected && n == 0).then(|| look.selected()),
                item: Some(item),
                ..row(line)
            });
        }
        if let Some(detail) = detail.filter(|_| !self.tight || selected) {
            let detail = rich(detail, look.ink(Ink::Faint), look);
            for line in wrap(&detail, self.width, indent(hang), &indent(hang)) {
                self.out.push(Row {
                    item: Some(item),
                    ..row(line)
                });
            }
        }
    }

    fn choices(&mut self, choices: &[Choice]) {
        for choice in choices {
            let mut label = vec![(choice.label.clone(), self.look.ink(Ink::Text))];
            if choice.recommended && choices.len() > 1 {
                label.push(("  · recommended".into(), self.look.ink(Ink::Faint)));
            }
            let key = self.bracket(choice.key, Ink::Accent);
            let detail = choice.detail.as_deref();
            self.entry(Item::Choice(choice.id), key, label, detail, 8);
        }
    }

    fn ticks(&mut self, ticks: &[Tick]) {
        for (n, tick) in ticks.iter().enumerate() {
            let mark = match (self.view.on(n), self.look.mono()) {
                (true, true) => "x",
                (true, false) => "✓",
                (false, _) => " ",
            };
            let number = if n < 9 {
                (n + 1).to_string()
            } else {
                " ".into()
            };
            let mut first = self.bracket(mark, Ink::Accent);
            first.push(span(format!("{number} "), self.look.ink(Ink::Faint)));
            let label = vec![(tick.label.clone(), self.look.ink(Ink::Text))];
            self.entry(Item::Tick(n), first, label, tick.detail.as_deref(), 10);
        }
        let first = self.bracket("↵", Ink::Accent);
        let label = vec![("Go on with these".into(), self.look.ink(Ink::Text))];
        self.entry(Item::GoOn, first, label, None, 8);
    }

    /// The prompt, then what has been typed: its end, if it is wider than
    /// the row.
    fn text(&mut self, prompt: &str) {
        let look = self.look;
        let soft = look.ink(Ink::Soft);
        self.out
            .extend(paragraph(prompt, soft, look, self.width, 0));
        let room = self.width.saturating_sub(3);
        let mut shown = String::new();
        let mut used = 0;
        for c in self.view.text.chars().rev() {
            let wide = c.width().unwrap_or(0);
            if used + wide > room {
                break;
            }
            used += wide;
            shown.insert(0, c);
        }
        self.out.push(row(vec![
            span("› ", look.bold(Ink::Bloom)),
            span(shown, look.ink(Ink::Text)),
        ]));
    }
}

/// The rows of what a screen asks; `tight` shows the details of only the
/// item selected.
fn asked(screen: &Screen, view: &View, look: Look, width: usize, tight: bool) -> Vec<Row> {
    let mut asking = Asking {
        view,
        look,
        width,
        tight,
        out: Vec::new(),
    };
    match &screen.ask {
        Ask::Nothing => {}
        Ask::Choose(choices) => asking.choices(choices),
        Ask::Ticks { ticks, choices } => {
            asking.ticks(ticks);
            asking.choices(choices);
        }
        Ask::Text { prompt } => asking.text(prompt),
    }
    asking.out
}

/// Draws `rows` from the top of `area`, those from `from` on.
fn put(buf: &mut Buffer, area: Rect, rows: &[Row], from: usize, drawn: &mut Drawn) {
    for (row, y) in rows.iter().skip(from).zip(area.y..area.bottom()) {
        let line = Rect::new(area.x, y, area.width, 1);
        if let Some(fill) = row.fill {
            buf.set_style(line, fill);
        }
        buf.set_line(area.x, y, &Line::from(row.spans.clone()), area.width);
        if let Some(item) = row.item {
            drawn.hits.push((line, item));
        }
        if let Some((at, text, url, style)) = &row.link {
            drawn.links.push(LinkSpot {
                at: Position::new(area.x.saturating_add(*at), y),
                text: text.clone(),
                url: url.clone(),
                style: *style,
            });
        }
    }
}

/// The page and what it asks, in `area`.
struct Laid {
    /// Whether more of the page is below, or above.
    more: bool,
    scrolled: bool,
    /// The row of the title's underline, for the map's thread to meet.
    underline: Option<u16>,
}

/// Lays out and draws the page in `area`: a title unless `titled` is
/// false, the page's blocks (which scroll) and what it asks (which stays
/// put at the bottom).
fn page(
    buf: &mut Buffer,
    area: Rect,
    screen: &Screen,
    view: &View,
    look: Look,
    titled: bool,
    drawn: &mut Drawn,
) -> Laid {
    let width = usize::from(area.width);
    let height = usize::from(area.height);
    // What it asks: every detail if there is room, else the selected one's.
    let full = asked(screen, view, look, width, false);
    let tight = asked(screen, view, look, width, true);
    let gap = |rows: &[Row]| usize::from(!rows.is_empty());
    // The QR code goes beside the page where it fits there.
    let code = qr_block(screen).and_then(|(data, caption)| Some((qr(data, look)?, caption)));
    let code_wide = code
        .as_ref()
        .and_then(|(code, _)| code.first())
        .map_or(0, |row| width_of(&row.spans));
    let beside = code.is_some() && width >= code_wide + 3 + 34;
    let body_width = if beside {
        width - code_wide - 3
    } else {
        width.min(usize::from(MEASURE))
    };
    let title_rows = if titled { 3 } else { 0 };
    let mark_room = WORDMARK.len() + 1;
    let lay = |art: bool, ask: &[Row], marked: bool| {
        let header = title_rows + if marked { mark_room } else { 0 };
        let rows_left = height.saturating_sub(header + ask.len() + gap(ask));
        let page = Page {
            screen,
            view,
            look,
            width: body_width,
            art,
            room: (rows_left, usize::from(BEYOND) + height - rows_left),
            beside,
        };
        let rows = page.rows();
        let fits = header + rows.len() + gap(ask) + ask.len() <= height;
        (rows, fits)
    };
    let welcome = titled && screen.step == Step::Welcome && width >= WORDMARK[0].width();
    // The richest layout that fits without scrolling, else the leanest.
    let mut chosen = None;
    for (marked, art, ask) in [
        (welcome, true, &full),
        (false, true, &full),
        (false, false, &full),
        (false, false, &tight),
    ] {
        let (rows, fits) = lay(art, ask, marked);
        if fits {
            chosen = Some((rows, marked, ask));
            break;
        }
    }
    let (rows, marked, ask) = chosen.unwrap_or_else(|| {
        let (rows, _) = lay(false, &tight, false);
        (rows, false, &tight)
    });
    let header = title_rows + if marked { mark_room } else { 0 };
    // What it asks gets the room it needs, short of a few rows of the page.
    let kept = rows.len().min(LEAST_PAGE);
    let ask_room = height.saturating_sub(header + kept + 1);
    let ask_height = ask.len().min(ask_room.max(1));
    let body_height = height.saturating_sub(header + ask_height + gap(ask));
    let mut y = area.y;
    let mut underline = None;
    if marked {
        let left = area.x + (area.width.saturating_sub(columns(WORDMARK[0].width()))) / 2;
        for line in WORDMARK {
            let spans: Vec<Span<'static>> = line
                .chars()
                .enumerate()
                .map(|(n, c)| {
                    let ink = match c {
                        '╱' | '◆' => Ink::Bloom,
                        _ if n < 10 => Ink::Accent,
                        _ => Ink::Text,
                    };
                    span(c.to_string(), look.bold(ink))
                })
                .collect();
            buf.set_line(left, y, &Line::from(spans), area.width);
            y += 1;
        }
        y += 1;
    }
    if titled {
        let title = &screen.title;
        buf.set_line(
            area.x,
            y,
            &Line::from(span(title.clone(), look.bold(Ink::Text))),
            area.width,
        );
        let long = view.grown(title.width() + 2).min(width);
        let thread = format!("{}╴", "─".repeat(long.saturating_sub(1)));
        buf.set_string(area.x, y + 1, thread, look.ink(Ink::Filament));
        underline = Some(y + 1);
        y += 3;
    }
    // The page, scrolled, coming in a row at a time with a new step.
    let body = Rect::new(area.x, y, columns(body_width), columns(body_height));
    let most = rows.len().saturating_sub(body_height);
    let from = usize::from(view.scroll).min(most);
    let showing = view.grown(rows.len());
    let shown: Vec<Row> = rows.into_iter().take(showing.max(from)).collect();
    put(buf, body, &shown, from, drawn);
    if beside && let Some((code, caption)) = code {
        let x = area.x + columns(body_width) + 3;
        let at = Rect::new(x, y, columns(code_wide), columns(body_height));
        if code.len() <= body_height {
            let mut rows = code;
            rows.extend(qr_caption(caption, look, code_wide, code_wide));
            put(buf, at, &rows, 0, drawn);
        } else {
            let need = code.len() + usize::from(BEYOND) + height - body_height;
            let say =
                format!("Make this window taller, at least {need} rows, to show the code here.");
            let rows = facts(&[(Mark::Warn, "Code", say.as_str())], view, look, code_wide);
            put(buf, at, &rows, 0, drawn);
        }
    }
    // What it asks, scrolled to keep the selected item in sight.
    let ask_area = Rect::new(
        area.x,
        area.bottom() - columns(ask_height),
        area.width,
        columns(ask_height),
    );
    let selected = ask
        .iter()
        .position(|row| row.item.is_some() && row.item == view.selected)
        .unwrap_or(0);
    let ask_from = (selected + 1).saturating_sub(ask_height);
    put(buf, ask_area, ask, ask_from, drawn);
    if let Ask::Text { .. } = screen.ask {
        let typed = ask.last().map_or(0, |row| width_of(&row.spans));
        drawn.cursor = Some(Position::new(
            area.x + columns(typed).min(area.width.saturating_sub(1)),
            area.bottom().saturating_sub(1),
        ));
    }
    drawn.most = columns(most);
    drawn.page = columns(body_height);
    Laid {
        more: from < most,
        scrolled: from > 0,
        underline,
    }
}

/// Keys worth knowing on this screen.
fn hints(screen: &Screen) -> Vec<(&'static str, &'static str)> {
    let mut hints = match &screen.ask {
        Ask::Nothing => Vec::new(),
        Ask::Choose(choices) if choices.is_empty() => Vec::new(),
        // The key in each choice's brackets says itself.
        Ask::Choose(choices) if choices.len() == 1 => vec![("enter", "choose")],
        Ask::Choose(_) => vec![("↑↓", "move"), ("enter", "choose")],
        Ask::Ticks { .. } => vec![("↑↓", "move"), ("space", "tick"), ("enter", "choose")],
        Ask::Text { .. } => vec![("enter", "send"), ("esc", "clear")],
    };
    if screen.choices().iter().any(|choice| choice.id == "back") {
        hints.push(("esc", "back"));
    }
    hints.push(("ctrl-c", "leave"));
    hints
}

/// The keys that scroll to more of the page, if there is more.
fn scroll_keys(laid: &Laid) -> Option<&'static str> {
    match (laid.scrolled, laid.more) {
        (true, true) => Some("pgup pgdn"),
        (false, true) => Some("pgdn"),
        (true, false) => Some("pgup"),
        (false, false) => None,
    }
}

fn hint_spans(hints: &[(&str, &str)], look: Look, room: usize) -> Vec<Span<'static>> {
    let all: usize = hints
        .iter()
        .map(|(key, what)| key.width() + what.width() + 4)
        .sum();
    // The arrows go without saying first, then whatever is last.
    let hints: Vec<_> = hints
        .iter()
        .filter(|(key, _)| all <= room + 3 || *key != "↑↓")
        .collect();
    let mut spans: Vec<Span<'static>> = Vec::new();
    for (n, (key, what)) in hints.into_iter().enumerate() {
        let mut more = Vec::new();
        if n > 0 {
            more.push(span(" · ", look.ink(Ink::Line)));
        }
        more.push(span(*key, look.bold(Ink::Soft)));
        more.push(span(format!(" {what}"), look.ink(Ink::Faint)));
        if width(&spans) + width(&more) > room {
            break;
        }
        spans.extend(more);
    }
    spans
}

/// Draws `screen` on the whole frame.
pub(crate) fn draw(frame: &mut Frame<'_>, screen: &Screen, view: &View, look: Look) -> Drawn {
    let area = frame.area();
    let buf = frame.buffer_mut();
    buf.set_style(area, look.base());
    let mut drawn = Drawn::default();
    if area.width < LEAST.0 || area.height < LEAST.1 {
        let say = "Make this window bigger to set up, or run setup with --plain.";
        let rows = paragraph(say, look.ink(Ink::Text), look, usize::from(area.width), 0);
        put(buf, area, &rows, 0, &mut drawn);
        return drawn;
    }
    drawn.asks = true;
    if area.width >= FULL.0 && area.height >= FULL.1 {
        full(buf, area, screen, view, look, &mut drawn);
    } else {
        compact(buf, area, screen, view, look, &mut drawn);
    }
    drawn
}

fn step_label(screen: &Screen) -> Option<String> {
    screen
        .step
        .number()
        .map(|n| format!("Step {n} of {}", Step::NUMBERED.len()))
}

/// Below the full size: no frame, map or wordmark.
fn compact(
    buf: &mut Buffer,
    area: Rect,
    screen: &Screen,
    view: &View,
    look: Look,
    drawn: &mut Drawn,
) {
    let x = area.x + 1;
    let width = area.width - 2;
    let mut head = Vec::new();
    if let Some(label) = step_label(screen) {
        head.push(span(label, look.ink(Ink::Soft)));
        head.push(span(" · ", look.ink(Ink::Line)));
    }
    head.push(span(screen.title.clone(), look.bold(Ink::Text)));
    buf.set_line(x, area.y, &Line::from(head), width);
    let long = view.grown(usize::from(width));
    buf.set_string(x, area.y + 1, "─".repeat(long), look.ink(Ink::Filament));
    let body = Rect::new(x, area.y + 3, width, area.height - 4);
    let laid = page(buf, body, screen, view, look, false, drawn);
    let mut hints = hints(screen);
    if let Some(keys) = scroll_keys(&laid) {
        hints.insert(0, (keys, "for more"));
    }
    let spans = hint_spans(&hints, look, usize::from(width));
    buf.set_line(x, area.bottom() - 1, &Line::from(spans), width);
}

/// The full screen: a frame of threads, the map of steps, the page.
fn full(buf: &mut Buffer, area: Rect, screen: &Screen, view: &View, look: Look, drawn: &mut Drawn) {
    let line = look.ink(Ink::Line);
    let (left, right, top, bottom) = (area.x, area.right() - 1, area.y, area.bottom() - 1);
    let mapped = screen.step != Step::Welcome;
    let divider = left + DIVIDER;
    // The page: beside the map, or alone and centred.
    let inner_top = top + 2;
    let height = area.height - 4;
    let page_area = if mapped {
        let x = divider + 3;
        let room = right.saturating_sub(2).saturating_sub(x);
        let wide = if qr_block(screen).is_some() {
            room
        } else {
            room.min(MEASURE)
        };
        Rect::new(x, inner_top, wide, height)
    } else {
        let room = area.width - 6;
        let wide = room.min(MEASURE - 4);
        Rect::new(left + 3 + (room - wide) / 2, inner_top, wide, height)
    };
    let laid = page(buf, page_area, screen, view, look, true, drawn);

    // The frame.
    for x in left + 1..right {
        buf.set_string(x, top, "─", line);
        buf.set_string(x, bottom, "─", line);
    }
    for y in top + 1..bottom {
        buf.set_string(left, y, "│", line);
        buf.set_string(right, y, "│", line);
    }
    buf.set_string(left, top, "╭", line);
    buf.set_string(right, top, "╮", line);
    buf.set_string(left, bottom, "╰", line);
    buf.set_string(right, bottom, "╯", line);
    let name = vec![
        span("╴", line),
        span("Ø", look.bold(Ink::Accent)),
        span("phio", look.bold(Ink::Text)),
        span(" setup", look.ink(Ink::Soft)),
        span("╶", line),
    ];
    buf.set_line(left + 2, top, &Line::from(name), area.width - 4);
    let label = match screen.step {
        Step::Done => Some("Done".to_owned()),
        _ => step_label(screen),
    };
    if let Some(label) = label {
        let wide = columns(label.width());
        let x = right - 3 - wide;
        buf.set_string(x, top, "╴", line);
        buf.set_string(x + 1, top, label, look.ink(Ink::Soft));
        buf.set_string(x + 1 + wide, top, "╶", line);
    }

    // The map, its threads grown as far as setup has come.
    let mut hints_from = left + 2;
    if mapped {
        buf.set_string(divider, top, "┬", line);
        buf.set_string(divider, bottom, "┴", line);
        for y in top + 1..bottom {
            buf.set_string(divider, y, "│", line);
        }
        let x = left + 3;
        for (n, (step, spot)) in screen.map.iter().enumerate() {
            let y = inner_top + columns(n * 2);
            if y >= bottom {
                break;
            }
            let (glyph, ink, name) = match spot {
                Spot::Done => ("●", Ink::Accent, look.ink(Ink::Soft)),
                Spot::Later => ("◌", Ink::Warn, look.ink(Ink::Soft)),
                Spot::Here => ("◉", Ink::Bloom, look.bold(Ink::Text)),
                Spot::Ahead => ("○", Ink::Line, look.ink(Ink::Faint)),
            };
            buf.set_string(x, y, glyph, look.bold(ink));
            buf.set_string(x + 2, y, step.name(), name);
            if *spot == Spot::Later && y + 1 < bottom {
                buf.set_string(x + 2, y + 1, "later", look.ink(Ink::Warn));
            }
            if *spot == Spot::Here {
                let from = x + 3 + columns(step.name().width());
                let long = view.grown(usize::from(divider.saturating_sub(from)));
                let thread = "─".repeat(long);
                buf.set_string(from, y, thread, look.ink(Ink::Bloom));
                if !view.entering() {
                    buf.set_string(divider, y, "┤", look.ink(Ink::Bloom));
                }
            }
            if let Some((_, next)) = screen.map.get(n + 1) {
                let reached = *next != Spot::Ahead;
                let (glyph, ink) = if reached {
                    ("│", Ink::Accent)
                } else {
                    ("┊", Ink::Line)
                };
                if y + 1 < bottom {
                    buf.set_string(x, y + 1, glyph, look.ink(ink));
                }
            }
        }
        if let Some(y) = laid.underline {
            buf.set_string(divider, y, "├", look.ink(Ink::Filament));
            let long = view.grown(2);
            buf.set_string(divider + 1, y, "─".repeat(long), look.ink(Ink::Filament));
        }
        hints_from = divider + 2;
    }

    // Keys along the bottom, and whether there is more to scroll to.
    let more = scroll_keys(&laid).map(|keys| format!("{keys} for more"));
    let more_width = more.as_ref().map_or(0, |more| more.width() + 4);
    let hints = hints(screen);
    let room = usize::from(right.saturating_sub(hints_from + 2)).saturating_sub(more_width + 2);
    let spans = hint_spans(&hints, look, room);
    if !spans.is_empty() {
        let mut row = vec![span("╴", line)];
        row.extend(spans);
        row.push(span("╶", line));
        buf.set_line(hints_from, bottom, &Line::from(row), right - hints_from);
    }
    if let Some(more) = more {
        let x = right - 1 - columns(more.width() + 2);
        let row = vec![
            span("╴", line),
            span(more, look.bold(Ink::Bloom)),
            span("╶", line),
        ];
        buf.set_line(x, bottom, &Line::from(row), right - x);
    }
}
