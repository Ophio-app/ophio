// SPDX-License-Identifier: Apache-2.0
//! Setup as plain lines of text: each screen printed once, answered by typing
//! a key and Enter. It suits screen readers and terminals that can't do more,
//! and is what `--plain` and a terminal too small for the full screen get.
use super::{
    Step,
    flow::{Answer, Ask, Block, Choice, ESCAPED, Mark, Screen, Tick, Ui, exact},
};
use std::{io::Write, time::Duration};

/// What reading a line came to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Input {
    Line(String),
    /// The input ended.
    End,
    /// Ctrl-C.
    Interrupt,
    /// The wait ran out; the line is still being read.
    Waited,
}

/// Where typed lines come from.
pub(crate) trait Lines {
    async fn next(&mut self, wait: Option<Duration>) -> Input;
    /// Whether a line asked for has not arrived yet.
    fn reading(&self) -> bool;
}

/// Lines from this terminal. A thread reads each line only once it is asked
/// for, so nothing is read while a program setup runs has the terminal.
pub(crate) struct Terminal {
    ask: std::sync::mpsc::Sender<()>,
    lines: tokio::sync::mpsc::UnboundedReceiver<Option<String>>,
    reading: bool,
}

impl Terminal {
    fn new() -> Self {
        let (ask, asked) = std::sync::mpsc::channel::<()>();
        let (send, lines) = tokio::sync::mpsc::unbounded_channel();
        std::thread::spawn(move || {
            let stdin = std::io::stdin();
            while asked.recv().is_ok() {
                let mut line = String::new();
                let line = match stdin.read_line(&mut line) {
                    Ok(0) | Err(_) => None,
                    Ok(_) => Some(line),
                };
                let ended = line.is_none();
                if send.send(line).is_err() || ended {
                    break;
                }
            }
        });
        Self {
            ask,
            lines,
            reading: false,
        }
    }
}

impl Lines for Terminal {
    async fn next(&mut self, wait: Option<Duration>) -> Input {
        if !self.reading {
            if self.ask.send(()).is_err() {
                return Input::End;
            }
            self.reading = true;
        }
        let timer = async {
            match wait {
                Some(wait) => tokio::time::sleep(wait).await,
                None => std::future::pending().await,
            }
        };
        tokio::select! {
            line = self.lines.recv() => {
                self.reading = false;
                match line {
                    Some(Some(line)) => Input::Line(line),
                    _ => Input::End,
                }
            }
            _ = tokio::signal::ctrl_c() => Input::Interrupt,
            () = timer => Input::Waited,
        }
    }

    fn reading(&self) -> bool {
        self.reading
    }
}

pub(crate) struct Plain<L, W> {
    lines: L,
    out: W,
    /// The screen last printed, which is not printed again.
    last: Option<Screen>,
    /// Ticks as they stand while they are being chosen.
    ticks: Vec<Tick>,
    /// What was asked when the line being read was asked for. A line begun
    /// on one question is never taken as the answer to another, such as an
    /// approval that took the place of the one it was typed for.
    asked: Option<Question>,
}

/// What a screen asks, as far as a typed line goes.
#[derive(Debug, Clone, PartialEq)]
struct Question {
    step: Step,
    about: Option<String>,
    keys: Vec<char>,
    text: bool,
}
impl Question {
    fn of(screen: &Screen) -> Self {
        Self {
            step: screen.step,
            about: screen.about.clone(),
            keys: screen.choices().iter().map(|choice| choice.key).collect(),
            text: matches!(screen.ask, Ask::Text { .. }),
        }
    }
}

impl Plain<Terminal, std::io::Stdout> {
    pub fn terminal() -> Self {
        Self::new(Terminal::new(), std::io::stdout())
    }
}

impl<L: Lines, W: Write> Plain<L, W> {
    pub fn new(lines: L, out: W) -> Self {
        Self {
            lines,
            out,
            last: None,
            ticks: Vec::new(),
            asked: None,
        }
    }

    #[cfg(test)]
    pub fn out(&self) -> &W {
        &self.out
    }

    /// Waits for Enter, as a window that would close does.
    #[cfg_attr(not(windows), allow(dead_code))]
    pub async fn wait_for_enter(&mut self) {
        self.say("\nPress Enter to close this window.");
        self.flush();
        self.lines.next(None).await;
    }

    fn say(&mut self, line: &str) {
        // A terminal that can't be written to can't be told so either.
        let _ = writeln!(self.out, "{line}");
    }

    fn flush(&mut self) {
        let _ = self.out.flush();
    }

    fn print(&mut self, screen: &Screen) {
        self.say("");
        match screen.step.number() {
            Some(number) => {
                let of = super::Step::NUMBERED.len();
                self.say(&format!("Step {number} of {of}: {}", screen.title));
            }
            None => self.say(&screen.title),
        }
        self.say("");
        let mut blocks = screen.blocks.iter().peekable();
        while let Some(block) = blocks.next() {
            if let Block::Command(command) = block {
                // Commands together are said one way, and the escapes are
                // explained once after them.
                let mut run = vec![command.as_str()];
                while let Some(Block::Command(command)) = blocks.peek() {
                    run.push(command);
                    blocks.next();
                }
                let (run, escaped) = exact(&run);
                for command in run {
                    self.say(&format!("    {command}"));
                }
                if escaped {
                    self.say(ESCAPED);
                }
                continue;
            }
            self.block(block);
        }
        match &screen.ask {
            Ask::Choose(choices) => self.choices(choices),
            Ask::Ticks { ticks, choices } => {
                self.ticks.clone_from(ticks);
                self.print_ticks();
                self.choices(choices);
            }
            Ask::Nothing | Ask::Text { .. } => {}
        }
    }

    fn block(&mut self, block: &Block) {
        match block {
            Block::Text(text) => self.say(text),
            Block::Fact {
                mark,
                label,
                detail,
            } => {
                let mark = match mark {
                    Mark::Ok => "OK: ",
                    Mark::Warn => "Attention: ",
                    Mark::Stop => "Problem: ",
                    Mark::Info | Mark::Wait => "",
                };
                if detail.is_empty() {
                    self.say(&format!("{mark}{label}"));
                } else {
                    self.say(&format!("{mark}{label}: {detail}"));
                }
            }
            // Said with the commands beside them, in `print`.
            Block::Command(_) => {}
            Block::Qr { data, caption } => {
                if let Ok(code) = crate::cli::pairing_qr(data) {
                    self.say(&code);
                }
                if !caption.is_empty() {
                    self.say(caption);
                }
            }
            Block::Link { label, url } => self.say(&format!("{label}: {url}")),
            Block::Steps(steps) => {
                for (number, step) in steps.iter().enumerate() {
                    self.say(&format!("  {}. {step}", number + 1));
                }
            }
        }
    }

    fn choices(&mut self, choices: &[Choice]) {
        if choices.is_empty() {
            return;
        }
        self.say("");
        for choice in choices {
            let recommended = if choice.recommended {
                " (recommended)"
            } else {
                ""
            };
            self.say(&format!("  {}  {}{recommended}", choice.key, choice.label));
            if let Some(detail) = &choice.detail {
                self.say(&format!("     {detail}"));
            }
        }
    }

    fn print_ticks(&mut self) {
        let lines: Vec<String> = self
            .ticks
            .iter()
            .enumerate()
            .flat_map(|(index, tick)| {
                let on = if tick.on { "[x]" } else { "[ ]" };
                let mut lines = vec![format!("  {on} {}  {}", index + 1, tick.label)];
                lines.extend(
                    tick.detail
                        .as_ref()
                        .map(|detail| format!("         {detail}")),
                );
                lines
            })
            .collect();
        for line in lines {
            self.say(&line);
        }
    }

    fn prompt(&mut self, screen: &Screen) {
        let prompt = match &screen.ask {
            Ask::Nothing => return,
            Ask::Choose(choices) if choices.is_empty() => return,
            Ask::Choose(choices) => match choices.iter().find(|c| c.recommended) {
                Some(choice) => {
                    format!("Type a key and press Enter (Enter alone: {}): ", choice.key)
                }
                None => "Type a key and press Enter: ".into(),
            },
            Ask::Text { prompt } => format!("{prompt}: "),
            Ask::Ticks { .. } => {
                "Type numbers to tick or untick them, Enter alone to go on, or a key: ".into()
            }
        };
        let _ = write!(self.out, "\n{prompt}");
        self.flush();
    }

    fn answer(&mut self, screen: &Screen, line: &str) -> Parsed {
        let line = line.trim();
        let key = |choices: &[Choice]| {
            let mut chars = line.chars();
            let (Some(c), None) = (chars.next(), chars.next()) else {
                return None;
            };
            let c = c.to_ascii_lowercase();
            choices
                .iter()
                .find(|choice| choice.key == c)
                .map(|choice| Answer::Choice(choice.id))
        };
        let found = match &screen.ask {
            Ask::Nothing => None,
            Ask::Text { .. } => Some(Answer::Text(line.to_owned())),
            Ask::Choose(choices) if line.is_empty() => choices
                .iter()
                .find(|c| c.recommended)
                .map(|c| Answer::Choice(c.id)),
            Ask::Choose(choices) => key(choices),
            Ask::Ticks { .. } if line.is_empty() => Some(Answer::Ticks(
                self.ticks.iter().filter(|t| t.on).map(|t| t.id).collect(),
            )),
            Ask::Ticks { choices, .. } => {
                if let Some(answer) = key(choices) {
                    return Parsed::Answer(answer);
                }
                let numbers: Option<Vec<usize>> = line
                    .split(|c: char| c == ',' || c.is_whitespace())
                    .filter(|part| !part.is_empty())
                    .map(|part| {
                        part.parse::<usize>()
                            .ok()
                            .filter(|n| (1..=self.ticks.len()).contains(n))
                    })
                    .collect();
                if let Some(numbers) = numbers {
                    for number in numbers {
                        let tick = &mut self.ticks[number - 1];
                        tick.on = !tick.on;
                    }
                    self.say("");
                    self.print_ticks();
                    self.prompt(screen);
                    return Parsed::Toggled;
                }
                None
            }
        };
        found.map_or(Parsed::Invalid, Parsed::Answer)
    }
}

enum Parsed {
    Answer(Answer),
    /// Ticks were ticked or unticked, and are shown again.
    Toggled,
    Invalid,
}

impl<L: Lines, W: Write> Ui for Plain<L, W> {
    async fn ask(&mut self, screen: &Screen, wait: Option<Duration>) -> Answer {
        if self.last.as_ref() != Some(screen) {
            self.print(screen);
            self.prompt(screen);
            self.last = Some(screen.clone());
        }
        if screen.ask == Ask::Nothing {
            self.flush();
            return Answer::Waited;
        }
        loop {
            if !self.lines.reading() {
                self.asked = Some(Question::of(screen));
            }
            let input = self.lines.next(wait).await;
            if let Input::Line(_) = input
                && self.asked.take() != Some(Question::of(screen))
            {
                // It was typed, at least in part, while something else was
                // asked.
                self.say("");
                self.say("What was asked changed before that answer came in, so it wasn't used.");
                self.prompt(screen);
                continue;
            }
            match input {
                Input::Waited => return Answer::Waited,
                Input::End | Input::Interrupt => {
                    self.say("");
                    return Answer::Quit;
                }
                Input::Line(line) => match self.answer(screen, &line) {
                    Parsed::Answer(answer) => {
                        // The next screen is printed, even if it is this one.
                        self.last = None;
                        return answer;
                    }
                    Parsed::Toggled => {}
                    Parsed::Invalid
                        if screen.choices().is_empty()
                            && !matches!(screen.ask, Ask::Ticks { .. }) => {}
                    Parsed::Invalid => {
                        self.say("That isn't one of the keys or numbers shown.");
                        self.prompt(screen);
                    }
                },
            }
        }
    }

    async fn suspend(&mut self) {
        if self.lines.reading() {
            // The line being read would take the program's first answer.
            self.say("\nPress Enter to continue.");
            self.flush();
            self.lines.next(None).await;
        }
        self.say("");
        self.flush();
    }

    async fn resume(&mut self) {
        self.last = None;
    }
}
