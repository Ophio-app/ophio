// SPDX-License-Identifier: Apache-2.0
//! Setup on the whole terminal: a frame drawn in threads, the map of steps
//! beside each page, answered with the arrow keys, a letter or a click.
//! `--plain` gets [`super::plain`] instead, as do terminals too basic or too
//! small for this.
pub(crate) mod draw;
pub(crate) mod look;

use super::flow::{Answer, Ask, Block, Mark, Screen, Ui};
use crate::setup::Step;
use crossterm::{
    cursor::{Hide, MoveTo, Show},
    event::{
        self, DisableBracketedPaste, DisableMouseCapture, EnableBracketedPaste, EnableMouseCapture,
        Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseButton, MouseEventKind,
    },
    execute, queue,
    style::{Attribute, Print, ResetColor, SetAttribute, SetBackgroundColor, SetForegroundColor},
    terminal::{self, EnterAlternateScreen, LeaveAlternateScreen},
};
use draw::{Drawn, Item, View};
use look::{Depth, Look};
use ratatui::{
    Terminal,
    backend::{Backend, CrosstermBackend, IntoCrossterm},
    layout::Position,
    style::{Color, Modifier},
};
use std::{
    io::{Stdout, Write},
    sync::{
        Once,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};

/// How long a new step takes to come in.
const ENTERING: Duration = Duration::from_millis(240);
const FRAME: Duration = Duration::from_millis(40);
/// How long each beat of the loader lasts.
const BEAT: Duration = Duration::from_millis(150);
/// The longest a read of the keyboard blocks for.
const READ: Duration = Duration::from_millis(250);
/// Rows a turn of the mouse wheel scrolls.
const WHEEL: u16 = 3;

/// What waiting for an event came to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Got {
    Event(Event),
    /// None came in time.
    Nothing,
    /// The input ended, or Ctrl-C came as a signal.
    Ended,
}

/// Where keys, clicks and resizes come from.
pub(crate) trait Events {
    async fn next(&mut self, wait: Duration) -> Got;
}

/// This terminal's events. Each is read only once asked for, so nothing is
/// read while a program setup runs has the terminal.
#[derive(Debug)]
pub(crate) struct Keyboard;

impl Events for Keyboard {
    async fn next(&mut self, wait: Duration) -> Got {
        let wait = wait.min(READ);
        let read = tokio::task::spawn_blocking(move || -> std::io::Result<Option<Event>> {
            if event::poll(wait)? {
                event::read().map(Some)
            } else {
                Ok(None)
            }
        });
        tokio::select! {
            read = read => match read {
                Ok(Ok(Some(event))) => Got::Event(event),
                Ok(Ok(None)) => Got::Nothing,
                _ => Got::Ended,
            },
            _ = tokio::signal::ctrl_c() => Got::Ended,
        }
    }
}

/// Whether this terminal is in the modes setup draws in.
static ENTERED: AtomicBool = AtomicBool::new(false);

/// Raw keys, the whole screen, clicks and pastes.
fn enter() -> std::io::Result<()> {
    terminal::enable_raw_mode()?;
    ENTERED.store(true, Ordering::SeqCst);
    let mut out = std::io::stdout();
    execute!(out, EnterAlternateScreen, EnableMouseCapture, Hide)?;
    // The console Windows had before its terminal can't take pastes whole.
    let _ = execute!(out, EnableBracketedPaste);
    Ok(())
}

/// The terminal as setup found it.
fn leave() {
    if ENTERED.swap(false, Ordering::SeqCst) {
        let mut out = std::io::stdout();
        let _ = execute!(out, DisableBracketedPaste);
        let _ = execute!(out, DisableMouseCapture, LeaveAlternateScreen, Show);
        let _ = terminal::disable_raw_mode();
    }
}

/// A panic leaves the terminal usable, and its message readable.
fn leave_on_panic() {
    static HOOK: Once = Once::new();
    HOOK.call_once(|| {
        let earlier = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            leave();
            earlier(info);
        }));
    });
}

/// Whether links can be made clickable: most terminals can, and those that
/// can't ignore the asking, but the Linux console and the console Windows
/// had before its terminal show it.
fn hyperlinks() -> bool {
    let set = |name: &str| std::env::var_os(name).is_some_and(|value| !value.is_empty());
    let term = std::env::var("TERM").unwrap_or_default();
    if term == "linux" {
        return false;
    }
    !cfg!(windows) || set("WT_SESSION")
}

/// Whether `url` is safe to put in a terminal's link: no escapes in it, and
/// an address of the web or of the app.
pub(crate) fn linkable(url: &str) -> bool {
    !url.chars().any(char::is_control)
        && ["https://", "http://", "ophio://"]
            .iter()
            .any(|scheme| url.starts_with(scheme))
}

/// Whether the terminal is big enough for this, or plain lines do better.
pub(crate) fn fits() -> bool {
    terminal::size().is_ok_and(|(columns, rows)| columns >= draw::LEAST.0 && rows >= draw::LEAST.1)
}

pub(crate) struct Tui<B: Backend, E> {
    terminal: Terminal<B>,
    events: E,
    look: Look,
    /// No animation.
    still: bool,
    /// Whether this is the real terminal, whose modes are setup's to set.
    real: bool,
    /// Whether to make links clickable.
    links: bool,
    view: View,
    /// The screen shown.
    last: Option<Screen>,
    /// Whether the screen shown is being asked again, as it waits for
    /// something, so what was done on it is kept.
    carry: bool,
    drawn: Drawn,
    /// Whether the links are to be written again, as the terminal was
    /// cleared.
    relink: bool,
    /// The step last come in to, and when it began coming in.
    step: Option<Step>,
    entering: Option<Instant>,
    started: Instant,
}

impl Tui<CrosstermBackend<Stdout>, Keyboard> {
    /// Setup on this terminal, which it takes over until dropped.
    pub fn terminal(still: bool) -> std::io::Result<Self> {
        leave_on_panic();
        if let Err(error) = enter() {
            leave();
            return Err(error);
        }
        let terminal = match Terminal::new(CrosstermBackend::new(std::io::stdout())) {
            Ok(terminal) => terminal,
            Err(error) => {
                leave();
                return Err(error);
            }
        };
        let mut tui = Self::new(
            terminal,
            Keyboard,
            Look {
                depth: Depth::detect(),
            },
            still,
        );
        tui.real = true;
        tui.links = hyperlinks();
        Ok(tui)
    }
}

impl<B: Backend, E: Events> Tui<B, E> {
    pub fn new(terminal: Terminal<B>, events: E, look: Look, still: bool) -> Self {
        Self {
            terminal,
            events,
            look,
            still,
            real: false,
            links: false,
            view: View::default(),
            last: None,
            carry: false,
            drawn: Drawn::default(),
            relink: true,
            step: None,
            entering: None,
            started: Instant::now(),
        }
    }

    #[cfg(test)]
    pub fn backend(&self) -> &B {
        self.terminal.backend()
    }

    #[cfg(test)]
    pub fn backend_mut(&mut self) -> &mut B {
        self.terminal.backend_mut()
    }

    #[cfg(test)]
    pub fn events(&mut self) -> &mut E {
        &mut self.events
    }

    /// Takes `screen` up, keeping what was done on it while it is asked
    /// again.
    fn show(&mut self, screen: &Screen) {
        // What was done on a screen about something else is not carried.
        let same = self.last.as_ref().map(|last| &last.about) == Some(&screen.about);
        if !self.carry || !same {
            self.view = View::new(screen);
        } else if self.last.as_ref() != Some(screen) {
            let mut view = View::new(screen);
            if draw::items(screen)
                .iter()
                .any(|item| Some(*item) == self.view.selected)
            {
                view.selected = self.view.selected;
            }
            if view.ticks.len() == self.view.ticks.len() {
                view.ticks = std::mem::take(&mut self.view.ticks);
            }
            view.text = std::mem::take(&mut self.view.text);
            view.scroll = self.view.scroll;
            self.view = view;
        }
        if self.step != Some(screen.step) {
            self.step = Some(screen.step);
            // A screen shown while something happens is shown whole.
            let animate = !self.still && screen.ask != Ask::Nothing;
            self.entering = animate.then(Instant::now);
        }
        self.last = Some(screen.clone());
    }

    /// Moves the animations on; how soon the next frame is due.
    fn tick(&mut self, screen: &Screen) -> Option<Duration> {
        let mut due = None;
        self.view.entered = 1000;
        if let Some(start) = self.entering {
            let gone = start.elapsed();
            if gone < ENTERING {
                let part = gone.as_millis() * 1000 / ENTERING.as_millis();
                self.view.entered = u16::try_from(part).unwrap_or(1000);
                due = Some(FRAME);
            } else {
                self.entering = None;
            }
        }
        let waiting = screen.blocks.iter().any(|block| {
            matches!(
                block,
                Block::Fact {
                    mark: Mark::Wait,
                    ..
                }
            )
        });
        if waiting && !self.still {
            let gone = self.started.elapsed().as_millis();
            let beat = BEAT.as_millis();
            self.view.beat = usize::try_from(gone / beat).unwrap_or(0);
            let next = Duration::from_millis(u64::try_from(beat - gone % beat).unwrap_or(1));
            due = Some(due.map_or(next, |due: Duration| due.min(next)));
        }
        due
    }

    /// Draws `screen` as it stands; false if the terminal can't be drawn on.
    fn draw(&mut self, screen: &Screen) -> bool {
        let (look, view) = (self.look, &self.view);
        let mut drawn = Drawn::default();
        let done = self.terminal.draw(|frame| {
            drawn = draw::draw(frame, screen, view, look);
            if let Some(cursor) = drawn.cursor {
                frame.set_cursor_position(cursor);
            }
        });
        if done.is_err() {
            return false;
        }
        self.view.scroll = self.view.scroll.min(drawn.most);
        if self.links && (self.relink || drawn.links != self.drawn.links) {
            // Drawing again only what changed, ratatui leaves these be.
            let _ = self.link(&drawn);
            self.relink = false;
        }
        self.drawn = drawn;
        true
    }

    /// Makes each link drawn clickable, as OSC 8 asks terminals to.
    fn link(&self, drawn: &Drawn) -> std::io::Result<()> {
        let mut out = std::io::stdout();
        let colour = |color: Option<Color>| -> crossterm::style::Color {
            color.unwrap_or(Color::Reset).into_crossterm()
        };
        let base = self.look.base();
        if drawn.cursor.is_some() {
            queue!(out, Hide)?;
        }
        for spot in drawn.links.iter().filter(|spot| linkable(&spot.url)) {
            let style = base.patch(spot.style);
            queue!(
                out,
                MoveTo(spot.at.x, spot.at.y),
                SetForegroundColor(colour(style.fg)),
                SetBackgroundColor(colour(style.bg)),
            )?;
            if style.add_modifier.contains(Modifier::BOLD) {
                queue!(out, SetAttribute(Attribute::Bold))?;
            }
            if style.add_modifier.contains(Modifier::UNDERLINED) {
                queue!(out, SetAttribute(Attribute::Underlined))?;
            }
            let url = &spot.url;
            queue!(
                out,
                Print(format!("\x1b]8;;{url}\x1b\\{}\x1b]8;;\x1b\\", spot.text)),
                SetAttribute(Attribute::Reset),
                ResetColor,
            )?;
        }
        if let Some(Position { x, y }) = drawn.cursor {
            queue!(out, MoveTo(x, y), Show)?;
        }
        out.flush()
    }

    /// What `event` does on `screen`: an answer, or a change to the view.
    fn handle(&mut self, screen: &Screen, event: Event) -> Option<Answer> {
        if !self.drawn.asks {
            // Nothing is chosen on a screen too small to show what it asks;
            // it can still be left, or made bigger.
            return match event {
                Event::Key(key)
                    if key.modifiers.contains(KeyModifiers::CONTROL)
                        && matches!(key.code, KeyCode::Char('c' | 'd')) =>
                {
                    Some(Answer::Quit)
                }
                Event::Resize(..) => {
                    self.relink = true;
                    None
                }
                _ => None,
            };
        }
        match event {
            Event::Key(key) if key.kind != KeyEventKind::Release => self.key(screen, key),
            Event::Paste(text) if matches!(screen.ask, Ask::Text { .. }) => {
                self.view
                    .text
                    .extend(text.chars().filter(|c| !c.is_control()));
                None
            }
            Event::Mouse(mouse) => {
                let at = Position::new(mouse.column, mouse.row);
                match mouse.kind {
                    MouseEventKind::Down(MouseButton::Left) => {
                        let hit = self
                            .drawn
                            .hits
                            .iter()
                            .find(|(area, _)| area.contains(at))
                            .map(|(_, item)| *item);
                        hit.and_then(|item| {
                            self.view.selected = Some(item);
                            self.activate(item)
                        })
                    }
                    MouseEventKind::ScrollDown => {
                        self.scroll(i32::from(WHEEL));
                        None
                    }
                    MouseEventKind::ScrollUp => {
                        self.scroll(-i32::from(WHEEL));
                        None
                    }
                    _ => None,
                }
            }
            Event::Resize(..) => {
                self.relink = true;
                None
            }
            _ => None,
        }
    }

    fn key(&mut self, screen: &Screen, key: KeyEvent) -> Option<Answer> {
        let control = key.modifiers.contains(KeyModifiers::CONTROL);
        let page = i32::from(self.drawn.page.max(1));
        match key.code {
            KeyCode::Char('c' | 'd') if control => return Some(Answer::Quit),
            KeyCode::PageDown => {
                self.scroll(page);
                return None;
            }
            KeyCode::PageUp => {
                self.scroll(-page);
                return None;
            }
            _ => {}
        }
        if let Ask::Text { .. } = screen.ask {
            return self.typed(key, control);
        }
        let items = draw::items(screen);
        let at = self
            .view
            .selected
            .and_then(|item| items.iter().position(|i| *i == item));
        let mut select = |n: usize| {
            self.view.selected = items.get(n).copied();
            None
        };
        let last = items.len().saturating_sub(1);
        match key.code {
            KeyCode::Up => select(at.map_or(0, |at| at.saturating_sub(1))),
            KeyCode::Down => select(at.map_or(0, |at| (at + 1).min(last))),
            KeyCode::Tab => select(at.map_or(0, |at| if at >= last { 0 } else { at + 1 })),
            KeyCode::BackTab => select(at.map_or(last, |at| at.checked_sub(1).unwrap_or(last))),
            KeyCode::Home => select(0),
            KeyCode::End => select(last),
            KeyCode::Enter => self.view.selected.and_then(|item| self.activate(item)),
            KeyCode::Char(' ') => {
                if let Some(Item::Tick(n)) = self.view.selected {
                    self.toggle(n);
                }
                None
            }
            KeyCode::Esc => screen
                .choices()
                .iter()
                .any(|choice| choice.id == "back")
                .then_some(Answer::Choice("back")),
            KeyCode::Char(c) if !control => {
                let c = c.to_ascii_lowercase();
                if let Some(choice) = screen.choices().iter().find(|choice| choice.key == c) {
                    return Some(Answer::Choice(choice.id));
                }
                let ticks = match &screen.ask {
                    Ask::Ticks { ticks, .. } => ticks.len(),
                    _ => 0,
                };
                if let Some(n) = c.to_digit(10).and_then(|n| usize::try_from(n).ok())
                    && (1..=9).contains(&n)
                    && n <= ticks
                {
                    self.view.selected = Some(Item::Tick(n - 1));
                    self.toggle(n - 1);
                }
                None
            }
            _ => None,
        }
    }

    /// A key while text is asked for.
    fn typed(&mut self, key: KeyEvent, control: bool) -> Option<Answer> {
        let text = &mut self.view.text;
        match key.code {
            KeyCode::Enter => return Some(Answer::Text(text.trim().to_owned())),
            KeyCode::Esc if text.is_empty() => return Some(Answer::Text(String::new())),
            KeyCode::Esc => text.clear(),
            KeyCode::Backspace => {
                text.pop();
            }
            KeyCode::Char('u') if control => text.clear(),
            KeyCode::Char('w') if control => {
                let kept = text.trim_end().len();
                let word = text[..kept]
                    .rfind(char::is_whitespace)
                    .map_or(0, |at| at + 1);
                text.truncate(word);
            }
            KeyCode::Char(c)
                if !control && !key.modifiers.contains(KeyModifiers::ALT) && !c.is_control() =>
            {
                text.push(c);
            }
            _ => {}
        }
        None
    }

    /// Clicking `item`, or Enter on it.
    fn activate(&mut self, item: Item) -> Option<Answer> {
        match item {
            Item::Tick(n) => {
                self.toggle(n);
                None
            }
            Item::GoOn => {
                let Some(Screen {
                    ask: Ask::Ticks { ticks, .. },
                    ..
                }) = &self.last
                else {
                    return None;
                };
                let on = ticks
                    .iter()
                    .zip(&self.view.ticks)
                    .filter(|(_, on)| **on)
                    .map(|(tick, _)| tick.id)
                    .collect();
                Some(Answer::Ticks(on))
            }
            Item::Choice(id) => Some(Answer::Choice(id)),
        }
    }

    fn toggle(&mut self, tick: usize) {
        if let Some(on) = self.view.ticks.get_mut(tick) {
            *on = !*on;
        }
    }

    fn scroll(&mut self, by: i32) {
        let to = i32::from(self.view.scroll) + by;
        let most = i32::from(self.drawn.most);
        self.view.scroll = u16::try_from(to.clamp(0, most)).unwrap_or(0);
    }
}

impl<B: Backend, E: Events> Ui for Tui<B, E> {
    async fn ask(&mut self, screen: &Screen, wait: Option<Duration>) -> Answer {
        self.show(screen);
        if screen.ask == Ask::Nothing {
            self.tick(screen);
            self.carry = false;
            return if self.draw(screen) {
                Answer::Waited
            } else {
                Answer::Quit
            };
        }
        let until = wait.map(|wait| Instant::now() + wait);
        loop {
            let due = self.tick(screen);
            if !self.draw(screen) {
                self.carry = false;
                return Answer::Quit;
            }
            let mut wait = due.unwrap_or(READ);
            if let Some(until) = until {
                let now = Instant::now();
                if now >= until {
                    self.carry = true;
                    return Answer::Waited;
                }
                wait = wait.min(until - now);
            }
            let answer = match self.events.next(wait).await {
                Got::Ended => Some(Answer::Quit),
                Got::Nothing => None,
                Got::Event(event) => self.handle(screen, event),
            };
            if let Some(answer) = answer {
                self.carry = false;
                return answer;
            }
        }
    }

    async fn suspend(&mut self) {
        if self.real {
            leave();
        }
    }

    async fn resume(&mut self) {
        if self.real {
            // Without these, keys come a line at a time; setup still goes on.
            let _ = enter();
        }
        let _ = self.terminal.clear();
        self.relink = true;
    }
}

impl<B: Backend, E> Drop for Tui<B, E> {
    fn drop(&mut self) {
        if self.real {
            leave();
        }
    }
}
