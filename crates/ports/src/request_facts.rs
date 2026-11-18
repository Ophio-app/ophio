// SPDX-License-Identifier: Apache-2.0
//! What a request would do, worked out by the computer from the exact
//! request, never from the agent's own account of it: "changes
//! Info.ios.plist, makes a git commit, connects to build-box".
//!
//! A shell command is read by a small, bounded tokenizer that never runs
//! anything and never reads the disk. Where it can't follow the command
//! (eval, code written into it, a command too long or nested too deep, a
//! program it works out as it runs) it says so and marks that risky. It
//! never says a command is harmless: no facts means only that it found
//! nothing to point out, and the person can always read the command.

use protocol::model::ApprovalFact;
use std::path::{Component, Path, PathBuf};

/// Longest command read; a longer one is said to be too long to check.
const MAX_CHARS: usize = 8000;
/// Words read across the whole command, nested parts included.
const MAX_WORDS: usize = 800;
/// Commands inside commands (`$(…)`, `bash -c`, ssh's remote command).
const MAX_DEPTH: usize = 5;
/// Facts shown; the rest are summed up in one more.
const MAX_FACTS: usize = 6;
/// Names listed in one fact before the rest are counted.
const MAX_NAMES: usize = 3;
/// Characters of one path or name in a fact.
const NAME_WIDTH: usize = 44;

/// The exact effect a request asks for, as its harness reads it.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum Subject {
    #[default]
    None,
    /// A shell command, run in `cwd` when the harness says where; otherwise
    /// in the project.
    Command {
        command: String,
        cwd: Option<String>,
    },
    /// Files a native tool would change, and the folder it asks to change
    /// anything in, when it asks for one.
    Files {
        changes: Vec<FileChange>,
        grant_root: Option<String>,
    },
    /// A file or folder a tool would read: one outside the project is worth
    /// saying.
    Reads(String),
    /// A web address a tool would fetch.
    Url(String),
}

/// One file a native file tool would change.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FileChange {
    Change(String),
    Delete(String),
    Move { from: String, to: String },
}

/// Where a request runs: the project it belongs to, and the home folder `~`
/// stands for.
#[derive(Debug, Clone, Default)]
pub struct Place {
    pub project: Option<PathBuf>,
    /// How a fact names the project: its name, or "your home folder".
    pub project_name: String,
    pub home: Option<PathBuf>,
}

/// What `subject` would do, risky facts first.
pub fn facts(subject: &Subject, place: &Place) -> Vec<ApprovalFact> {
    let mut out = Out::default();
    let ctx = Ctx {
        place,
        cwd: place.project.clone(),
        on: None,
        depth: 0,
    };
    match subject {
        Subject::None => return Vec::new(),
        Subject::Command { command, cwd } => {
            let mut ctx = ctx;
            if let Some(cwd) = cwd {
                ctx.cwd = resolve_dir(cwd, &ctx);
            }
            read_text(command, &mut ctx, &mut out);
        }
        Subject::Files {
            changes,
            grant_root,
        } => {
            for change in changes {
                match change {
                    FileChange::Change(path) => {
                        if let Some(t) = locate(&plain(path), &ctx) {
                            out.push(Effect::Change(t), &ctx);
                        }
                    }
                    FileChange::Delete(path) => {
                        if let Some(t) = locate(&plain(path), &ctx) {
                            out.push(Effect::Delete(t), &ctx);
                        }
                    }
                    FileChange::Move { from, to } => {
                        if let Some(t) = locate(&plain(from), &ctx) {
                            out.push(Effect::Move(t), &ctx);
                        }
                        if let Some(t) = locate(&plain(to), &ctx)
                            && t.at.reaches_out()
                        {
                            out.push(Effect::Change(t), &ctx);
                        }
                    }
                }
            }
            if let Some(root) = grant_root
                && let Some(t) = locate(&plain(root), &ctx)
            {
                out.push(Effect::WriteAccess(t), &ctx);
            }
        }
        Subject::Reads(path) => {
            if let Some(t) = locate(&plain(path), &ctx) {
                out.push(Effect::Read(t), &ctx);
            }
        }
        Subject::Url(url) => out.push(Effect::Connect(host_of(url)), &ctx),
    }
    render(out, place)
}

// ---------------------------------------------------------------------------
// Reading a command

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Fail {
    TooLong,
    Unreadable,
    TooDeep,
}

/// One word as the shell would pass it, quotes removed. Expansions are kept
/// as written, since their values are only known when it runs.
#[derive(Debug, Clone, Default)]
struct Word {
    text: String,
    /// Holds an expansion or substitution.
    dynamic: bool,
    /// Holds an unquoted glob or brace pattern.
    glob: bool,
    /// Byte offset in `text` of the first expansion or pattern.
    open_at: Option<usize>,
    /// Command and process substitutions inside it, run before it.
    subs: Vec<Script>,
    /// Backquoted commands inside it, as written.
    ticks: Vec<String>,
    /// Exactly one process substitution, `<(…)` or `>(…)`.
    process: bool,
    /// Some part of it was quoted or escaped.
    quoted: bool,
    /// Written with `$'…'` escapes, which can spell anything.
    encoded: bool,
    /// How a fact names it, when not as written: "the files it finds".
    shown: Option<String>,
    /// A command hidden where Ophio doesn't follow it: inside `${…}` or a
    /// calculation. Says where.
    hidden: Option<&'static str>,
}

impl Word {
    fn open(&mut self) {
        self.open_at.get_or_insert(self.text.len());
    }
    fn expansion(&mut self) {
        self.open();
        self.dynamic = true;
    }
}

#[derive(Debug, Clone)]
enum Redirect {
    /// A file written to, and whether it takes the standard output.
    Write(Word, bool),
    Read(Word),
    /// Index into the heredoc bodies.
    Heredoc(usize),
    /// `<<< text`: the text is the input.
    String(Word),
    /// A copied descriptor: no file.
    Other,
}

#[derive(Debug, Clone, Default)]
struct Simple {
    words: Vec<Word>,
    redirects: Vec<Redirect>,
}

#[derive(Debug, Clone)]
enum Node {
    Simple(Simple),
    Group(Script),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Sep {
    Then,
    Pipe,
}

#[derive(Debug, Clone, Default)]
struct Script(Vec<(Node, Sep)>);

#[derive(Debug, Clone, Default)]
struct Heredoc {
    body: String,
    /// Its delimiter was quoted, so the body is taken as written.
    literal: bool,
    /// A here-string worked out as it runs: its text isn't known.
    unknown: bool,
}

impl Heredoc {
    /// Its text as the program reads it, when the command shows it.
    fn known(&self) -> Option<&str> {
        let plain = self.literal || !(self.body.contains('$') || self.body.contains('`'));
        (!self.unknown && plain).then_some(self.body.as_str())
    }
}

struct Parser {
    s: Vec<char>,
    i: usize,
    words: usize,
    depth: usize,
    heredocs: Vec<Heredoc>,
    /// Heredocs whose bodies start after the next newline.
    pending: Vec<(usize, String, bool)>,
}

impl Parser {
    fn new(text: &str, depth: usize) -> Self {
        Self {
            s: text.chars().collect(),
            i: 0,
            words: 0,
            depth,
            heredocs: Vec::new(),
            pending: Vec::new(),
        }
    }
    fn peek(&self) -> Option<char> {
        self.s.get(self.i).copied()
    }
    fn at(&self, k: usize) -> Option<char> {
        self.s.get(self.i + k).copied()
    }
    fn blank(&mut self) {
        loop {
            match self.peek() {
                Some(' ' | '\t' | '\r') => self.i += 1,
                Some('\\') if self.at(1) == Some('\n') => self.i += 2,
                _ => break,
            }
        }
    }
    fn comment(&mut self) {
        while self.peek().is_some_and(|c| c != '\n') {
            self.i += 1;
        }
    }
    fn bodies(&mut self) {
        for (index, delimiter, tabs) in std::mem::take(&mut self.pending) {
            let mut body = String::new();
            while self.i < self.s.len() {
                let start = self.i;
                while self.peek().is_some_and(|c| c != '\n') {
                    self.i += 1;
                }
                let line: String = self.s[start..self.i].iter().collect();
                if self.peek() == Some('\n') {
                    self.i += 1;
                }
                let check = if tabs {
                    line.trim_start_matches('\t')
                } else {
                    &line
                };
                if check == delimiter {
                    break;
                }
                body.push_str(&line);
                body.push('\n');
            }
            self.heredocs[index].body = body;
        }
    }

    /// Commands up to the end, or up to the `)` that closes a substitution
    /// or subshell when `closing`.
    fn list(&mut self, closing: bool) -> Result<Script, Fail> {
        self.depth += 1;
        if self.depth > MAX_DEPTH {
            return Err(Fail::TooDeep);
        }
        let mut items: Vec<(Node, Sep)> = Vec::new();
        let then = |items: &mut Vec<(Node, Sep)>, sep: Sep| {
            if let Some(last) = items.last_mut() {
                last.1 = sep;
            }
        };
        loop {
            self.blank();
            let Some(c) = self.peek() else {
                if closing {
                    return Err(Fail::Unreadable);
                }
                break;
            };
            match c {
                '#' => self.comment(),
                '\n' => {
                    self.i += 1;
                    self.bodies();
                    then(&mut items, Sep::Then);
                }
                ';' if self.at(1) == Some(';') => return Err(Fail::Unreadable),
                ';' => {
                    self.i += 1;
                    then(&mut items, Sep::Then);
                }
                '&' if self.at(1) == Some('&') => {
                    self.i += 2;
                    then(&mut items, Sep::Then);
                }
                '&' if self.at(1) != Some('>') => {
                    self.i += 1;
                    then(&mut items, Sep::Then);
                }
                '|' if self.at(1) == Some('|') => {
                    self.i += 2;
                    then(&mut items, Sep::Then);
                }
                '|' => {
                    self.i += 1;
                    if self.peek() == Some('&') {
                        self.i += 1;
                    }
                    then(&mut items, Sep::Pipe);
                }
                ')' if closing => {
                    self.i += 1;
                    break;
                }
                ')' => return Err(Fail::Unreadable),
                '(' if self.at(1) == Some('(') => {
                    // An arithmetic command runs nothing, unless a command
                    // is written inside it.
                    if self.arithmetic()? {
                        return Err(Fail::Unreadable);
                    }
                }
                '(' => {
                    self.i += 1;
                    let script = self.list(true)?;
                    items.push((Node::Group(script), Sep::Then));
                }
                _ => {
                    let simple = self.simple()?;
                    if !simple.words.is_empty() || !simple.redirects.is_empty() {
                        items.push((Node::Simple(simple), Sep::Then));
                    }
                }
            }
        }
        self.depth -= 1;
        Ok(Script(items))
    }

    /// Past a `((…))` or `$((…))` whose first `(` is next; true when a
    /// command is written inside it.
    fn arithmetic(&mut self) -> Result<bool, Fail> {
        let start = self.i;
        let mut open = 0usize;
        while let Some(c) = self.peek() {
            self.i += 1;
            match c {
                '(' => open += 1,
                ')' => {
                    open -= 1;
                    if open == 0 {
                        return Ok(runs_inside(&self.s[start + 2..self.i]));
                    }
                }
                _ => {}
            }
        }
        Err(Fail::Unreadable)
    }

    fn simple(&mut self) -> Result<Simple, Fail> {
        let mut simple = Simple::default();
        loop {
            self.blank();
            match self.peek() {
                None | Some('\n' | ';' | '|' | ')') => break,
                Some('&') if self.at(1) != Some('>') => break,
                Some('#') => {
                    self.comment();
                    break;
                }
                _ => {}
            }
            if let Some(redirect) = self.redirect()? {
                simple.redirects.push(redirect);
                continue;
            }
            let word = self.word()?;
            simple.words.push(word);
        }
        Ok(simple)
    }

    fn redirect(&mut self) -> Result<Option<Redirect>, Fail> {
        let mut k = 0;
        while self.at(k).is_some_and(|c| c.is_ascii_digit()) {
            k += 1;
        }
        let op = (self.at(k), self.at(k + 1), self.at(k + 2));
        let (len, kind) = match op {
            (Some('<' | '>'), Some('('), _) if k == 0 => return Ok(None),
            (Some('&'), Some('>'), Some('>')) if k == 0 => (3, "write"),
            (Some('&'), Some('>'), _) if k == 0 => (2, "write"),
            (Some('>'), Some('>'), _) => (2, "write"),
            (Some('>'), Some('|'), _) => (2, "write"),
            (Some('>'), Some('&'), _) => (2, "dup"),
            (Some('>'), _, _) => (1, "write"),
            (Some('<'), Some('<'), Some('<')) => (3, "string"),
            (Some('<'), Some('<'), Some('-')) => (3, "heredoc-tabs"),
            (Some('<'), Some('<'), _) => (2, "heredoc"),
            (Some('<'), Some('&'), _) => (2, "dup"),
            (Some('<'), Some('>'), _) => (2, "write"),
            (Some('<'), _, _) => (1, "read"),
            _ => return Ok(None),
        };
        // `>`, `1>`, `&>` and `>&` all take the standard output.
        let stdout = op.0 != Some('<') && (k == 0 || (k == 1 && self.peek() == Some('1')));
        self.i += k + len;
        self.blank();
        if self.peek().is_none_or(|c| "\n;|&)<>".contains(c)) {
            return Err(Fail::Unreadable);
        }
        let word = self.word()?;
        Ok(Some(match kind {
            "write" => Redirect::Write(word, stdout),
            "read" => Redirect::Read(word),
            "dup" if word.text == "-" || word.text.chars().all(|c| c.is_ascii_digit()) => {
                Redirect::Other
            }
            "dup" => Redirect::Write(word, stdout),
            "string" => Redirect::String(word),
            _ => {
                let index = self.heredocs.len();
                self.heredocs.push(Heredoc {
                    body: String::new(),
                    literal: word.quoted,
                    unknown: false,
                });
                self.pending
                    .push((index, word.text.clone(), kind == "heredoc-tabs"));
                Redirect::Heredoc(index)
            }
        }))
    }

    fn word(&mut self) -> Result<Word, Fail> {
        self.words += 1;
        if self.words > MAX_WORDS {
            return Err(Fail::TooLong);
        }
        let mut w = Word::default();
        if matches!(self.peek(), Some('<' | '>')) && self.at(1) == Some('(') {
            self.i += 2;
            let script = self.list(true)?;
            w.subs.push(script);
            w.process = true;
            w.expansion();
            w.text.push_str("<(…)");
            return Ok(w);
        }
        let start = self.i;
        while let Some(c) = self.peek() {
            match c {
                ' ' | '\t' | '\r' | '\n' | ';' | '&' | '|' | '<' | '>' | ')' => break,
                '(' => return Err(Fail::Unreadable),
                '\\' => {
                    self.i += 1;
                    match self.peek() {
                        Some('\n') => self.i += 1,
                        Some(n) => {
                            w.quoted = true;
                            w.text.push(n);
                            self.i += 1;
                        }
                        None => {}
                    }
                }
                '\'' => {
                    self.i += 1;
                    w.quoted = true;
                    loop {
                        match self.peek() {
                            None => return Err(Fail::Unreadable),
                            Some('\'') => {
                                self.i += 1;
                                break;
                            }
                            Some(ch) => {
                                w.text.push(ch);
                                self.i += 1;
                            }
                        }
                    }
                }
                '"' => {
                    self.i += 1;
                    w.quoted = true;
                    self.double(&mut w)?;
                }
                '$' => self.dollar(&mut w, false)?,
                '`' => self.backtick(&mut w)?,
                '*' | '?' | '[' | '{' => {
                    if !(c == '{' && self.at(1).is_none_or(|n| " \t\n;".contains(n))) {
                        w.open();
                        w.glob = true;
                    }
                    w.text.push(c);
                    self.i += 1;
                }
                _ => {
                    w.text.push(c);
                    self.i += 1;
                }
            }
        }
        if self.i == start {
            return Err(Fail::Unreadable);
        }
        Ok(w)
    }

    fn double(&mut self, w: &mut Word) -> Result<(), Fail> {
        loop {
            match self.peek() {
                None => return Err(Fail::Unreadable),
                Some('"') => {
                    self.i += 1;
                    return Ok(());
                }
                Some('\\') => {
                    self.i += 1;
                    match self.peek() {
                        Some('\n') => self.i += 1,
                        Some(n @ ('$' | '`' | '"' | '\\')) => {
                            w.text.push(n);
                            self.i += 1;
                        }
                        Some(n) => {
                            w.text.push('\\');
                            w.text.push(n);
                            self.i += 1;
                        }
                        None => return Err(Fail::Unreadable),
                    }
                }
                Some('$') => self.dollar(w, true)?,
                Some('`') => self.backtick(w)?,
                Some(c) => {
                    w.text.push(c);
                    self.i += 1;
                }
            }
        }
    }

    fn dollar(&mut self, w: &mut Word, quoted: bool) -> Result<(), Fail> {
        self.i += 1;
        match self.peek() {
            Some('(') if self.at(1) == Some('(') => {
                w.expansion();
                if self.arithmetic()? {
                    w.hidden = Some(IN_SUM);
                }
                w.text.push_str("$((…))");
            }
            Some('(') => {
                self.i += 1;
                w.expansion();
                let script = self.list(true)?;
                w.subs.push(script);
                w.text.push_str("$(…)");
            }
            Some('{') => {
                w.expansion();
                let start = self.i;
                let mut open = 0usize;
                loop {
                    match self.peek() {
                        // Quotes and escapes inside it change where it ends
                        // in ways this count doesn't follow: `${x:-"}"}`.
                        None | Some('"' | '\'' | '\\') => return Err(Fail::Unreadable),
                        Some('{') => open += 1,
                        Some('}') => {
                            open -= 1;
                            if open == 0 {
                                self.i += 1;
                                break;
                            }
                        }
                        _ => {}
                    }
                    self.i += 1;
                }
                if runs_inside(&self.s[start..self.i]) {
                    w.hidden = Some(IN_VARIABLE);
                }
                w.text.push('$');
                w.text.extend(&self.s[start..self.i]);
            }
            Some('\'') if !quoted => {
                self.i += 1;
                w.quoted = true;
                loop {
                    match self.peek() {
                        None => return Err(Fail::Unreadable),
                        Some('\'') => {
                            self.i += 1;
                            break;
                        }
                        Some('\\') => {
                            w.encoded = true;
                            w.expansion();
                            w.text.push('\\');
                            self.i += 1;
                            if let Some(n) = self.peek() {
                                w.text.push(n);
                                self.i += 1;
                            }
                        }
                        Some(c) => {
                            w.text.push(c);
                            self.i += 1;
                        }
                    }
                }
            }
            Some('"') if !quoted => {
                self.i += 1;
                w.quoted = true;
                self.double(w)?;
            }
            Some(c) if c.is_ascii_alphabetic() || c == '_' => {
                w.expansion();
                w.text.push('$');
                while let Some(c) = self
                    .peek()
                    .filter(|c| c.is_ascii_alphanumeric() || *c == '_')
                {
                    w.text.push(c);
                    self.i += 1;
                }
            }
            Some(c) if c.is_ascii_digit() || "@*#?$!-".contains(c) => {
                w.expansion();
                w.text.push('$');
                w.text.push(c);
                self.i += 1;
            }
            _ => w.text.push('$'),
        }
        Ok(())
    }

    fn backtick(&mut self, w: &mut Word) -> Result<(), Fail> {
        self.i += 1;
        w.expansion();
        let mut inner = String::new();
        loop {
            match self.peek() {
                None => return Err(Fail::Unreadable),
                Some('`') => {
                    self.i += 1;
                    break;
                }
                Some('\\') if matches!(self.at(1), Some('`' | '\\' | '$')) => {
                    inner.push(self.at(1).unwrap_or_default());
                    self.i += 2;
                }
                Some(c) => {
                    inner.push(c);
                    self.i += 1;
                }
            }
        }
        w.ticks.push(inner);
        w.text.push_str("`…`");
        Ok(())
    }
}

/// Whether text the parser skips over holds a command: `$(…)` or `` `…` ``.
fn runs_inside(text: &[char]) -> bool {
    text.contains(&'`') || text.windows(2).any(|w| w == ['$', '('])
}

// ---------------------------------------------------------------------------
// What it does

/// Where a path leads, as far as the command shows.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Where {
    /// The project folder itself.
    Root,
    Inside,
    /// A temporary folder: outside the project, but made for this.
    Temp,
    Outside,
    /// Only known when it runs.
    Unknown,
    /// On the computer an ssh command reaches.
    Remote,
    /// A program found on the search path.
    Program,
}

impl Where {
    fn reaches_out(&self) -> bool {
        matches!(self, Where::Outside | Where::Unknown)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Target {
    shown: String,
    at: Where,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Effect {
    Unclear(String),
    Sudo,
    Downloaded(String),
    /// Starts a program an earlier step downloaded.
    DownloadedProgram(String),
    Built,
    Install,
    Uninstall,
    Delete(Target),
    DeletesWith(String),
    Discard,
    History,
    DeleteBranch,
    Push {
        remote: String,
        force: bool,
    },
    Publish(String),
    PostsGitHub,
    Power,
    Disk,
    Schedule,
    GlobalSettings(&'static str),
    WriteAccess(Target),
    Change(Target),
    Unpack(Target),
    Read(Target),
    Use(Target),
    Move(Target),
    Commit(Target),
    Branch,
    Pull(String),
    Copy {
        host: String,
        to: bool,
    },
    Connect(String),
    Download(String),
    Send(String),
    Fetches(String),
    Runs(Target),
    Stops,
}

const TOO_LONG: &str = "Ophio can't check a command this long";
const UNREADABLE: &str = "Ophio can't read how this command is written";
const TOO_DEEP: &str = "Ophio can't check commands nested this deep";
const WORKED_OUT: &str = "Ophio can't check a command it works out as it runs";
const IN_VARIABLE: &str = "Ophio can't check a command written inside a variable";
const IN_SUM: &str = "Ophio can't check a command written inside a calculation";
const RUNS_OTHER_CODE: &str =
    "Ophio can't check a setting in it that makes programs run other code";

#[derive(Default)]
struct Out {
    effects: Vec<(Effect, Option<String>)>,
    /// Every program it starts, for when there is nothing else to say.
    programs: Vec<String>,
    /// What the last process substitution read downloads, if it does.
    process_download: Option<String>,
    /// Files and folders an earlier step downloaded, and where from: what
    /// runs them later runs what was downloaded.
    fetched: Vec<(PathBuf, String)>,
}

impl Out {
    fn push(&mut self, effect: Effect, ctx: &Ctx) {
        let on = ctx.on.clone();
        if !self.effects.iter().any(|(e, o)| *e == effect && *o == on) {
            self.effects.push((effect, on));
        }
    }
    fn unclear(&mut self, why: impl Into<String>, ctx: &Ctx) {
        self.push(Effect::Unclear(why.into()), ctx);
    }
    /// Remembers a file or folder a download puts at `word`.
    fn fetch(&mut self, word: &Word, host: &str, ctx: &Ctx) {
        if let Some(path) = resolve(word, ctx) {
            self.fetched.push((path, host.to_string()));
        }
    }
    /// Where the file at `word` was downloaded from, if an earlier step
    /// downloaded it.
    fn fetched_from(&self, word: &Word, ctx: &Ctx) -> Option<String> {
        let path = resolve(word, ctx)?;
        self.fetched
            .iter()
            .rev()
            .find(|(at, _)| path.starts_with(at))
            .map(|(_, host)| host.clone())
    }
    fn program(&mut self, name: &str, ctx: &Ctx) {
        if BUILTINS.contains(&name) {
            return;
        }
        let name = match &ctx.on {
            Some(host) => format!("{name} on {host}"),
            None => name.to_string(),
        };
        if !self.programs.contains(&name) {
            self.programs.push(name);
        }
    }
}

#[derive(Clone)]
struct Ctx<'p> {
    place: &'p Place,
    /// None once a `cd` goes where the command doesn't show.
    cwd: Option<PathBuf>,
    /// The host an ssh command runs this part on.
    on: Option<String>,
    depth: usize,
}

/// What a command's input is.
#[derive(Default, Clone, Copy)]
struct Input<'a> {
    /// A heredoc or here-string.
    heredoc: Option<&'a Heredoc>,
    /// A file it reads with `<`.
    file: Option<&'a Word>,
    /// Fed by an earlier stage of a pipeline: Some(host) when that stage
    /// downloads what it passes on.
    piped: Option<Option<&'a str>>,
}

fn read_text(text: &str, ctx: &mut Ctx, out: &mut Out) {
    if text.chars().count() > MAX_CHARS {
        out.unclear(TOO_LONG, ctx);
        return;
    }
    if ctx.depth >= MAX_DEPTH {
        out.unclear(TOO_DEEP, ctx);
        return;
    }
    let mut parser = Parser::new(text, ctx.depth);
    match parser.list(false) {
        Ok(script) => {
            let heredocs = std::mem::take(&mut parser.heredocs);
            let mut inner = ctx.clone();
            inner.depth += 1;
            let _ = script_effects(&script, &heredocs, &mut inner, out);
            // A `cd` at the top level carries on to what follows it there.
            if ctx.depth == 0 {
                ctx.cwd = inner.cwd;
            }
        }
        Err(Fail::TooLong) => out.unclear(TOO_LONG, ctx),
        Err(Fail::TooDeep) => out.unclear(TOO_DEEP, ctx),
        Err(Fail::Unreadable) => out.unclear(UNREADABLE, ctx),
    }
}

/// Returns the host whose download the last pipeline writes out, if any.
fn script_effects(
    script: &Script,
    heredocs: &[Heredoc],
    ctx: &mut Ctx,
    out: &mut Out,
) -> Option<String> {
    let items = &script.0;
    let mut start = 0;
    let mut last = None;
    while start < items.len() {
        let mut end = start;
        while end + 1 < items.len() && items[end].1 == Sep::Pipe {
            end += 1;
        }
        let stages = &items[start..=end];
        let mut feed: Option<Option<String>> = None;
        for (node, _) in stages {
            let mut own;
            // A pipeline's stages each run in a subshell of their own.
            let here: &mut Ctx = if stages.len() == 1 {
                ctx
            } else {
                own = ctx.clone();
                &mut own
            };
            let made = match node {
                Node::Simple(simple) => simple_effects(
                    simple,
                    heredocs,
                    here,
                    out,
                    feed.as_ref().map(|f| f.as_deref()),
                ),
                Node::Group(script) => {
                    let mut inner = here.clone();
                    script_effects(script, heredocs, &mut inner, out)
                }
            };
            // What a download passes on stays a download down the pipe.
            feed = Some(made.or(feed.flatten()));
        }
        last = feed.flatten();
        start = end + 1;
    }
    last
}

fn word_subs(word: &Word, heredocs: &[Heredoc], ctx: &Ctx, out: &mut Out) {
    if let Some(why) = word.hidden {
        out.unclear(why, ctx);
    }
    for sub in &word.subs {
        let mut inner = ctx.clone();
        inner.depth += 1;
        if inner.depth > MAX_DEPTH {
            out.unclear(TOO_DEEP, ctx);
        } else {
            let fed = script_effects(sub, heredocs, &mut inner, out);
            if word.process {
                out.process_download = fed;
            }
        }
    }
    for tick in &word.ticks {
        let mut inner = ctx.clone();
        inner.depth += 1;
        read_text(tick, &mut inner, out);
    }
}

/// Returns the host whose download it writes to its output, if any.
fn simple_effects(
    simple: &Simple,
    heredocs: &[Heredoc],
    ctx: &mut Ctx,
    out: &mut Out,
    piped: Option<Option<&str>>,
) -> Option<String> {
    for word in &simple.words {
        word_subs(word, heredocs, ctx, out);
    }
    // The last redirect of the input is what it reads.
    let mut heredoc: Option<&Heredoc> = None;
    let mut string: Option<Heredoc> = None;
    let mut file: Option<&Word> = None;
    let mut stdout: Option<&Word> = None;
    for redirect in &simple.redirects {
        match redirect {
            Redirect::Write(word, to_stdout) => {
                word_subs(word, heredocs, ctx, out);
                if *to_stdout {
                    stdout = Some(word);
                }
                if let Some(t) = locate(word, ctx) {
                    out.push(Effect::Change(t), ctx);
                }
            }
            Redirect::Read(word) => {
                word_subs(word, heredocs, ctx, out);
                read(word, ctx, out);
                (heredoc, string, file) = (None, None, Some(word));
            }
            Redirect::Heredoc(index) => {
                (heredoc, string, file) = (heredocs.get(*index), None, None);
                // An unquoted body runs the commands written inside it.
                if let Some(h) = heredoc
                    && !h.literal
                    && (h.body.contains("$(") || h.body.contains('`'))
                {
                    out.unclear(
                        "Ophio can't check commands inside the text it passes on",
                        ctx,
                    );
                }
            }
            Redirect::String(word) => {
                word_subs(word, heredocs, ctx, out);
                // A here-string is read like a heredoc's body.
                let text = Heredoc {
                    body: format!("{}\n", word.text),
                    literal: !word.dynamic,
                    unknown: word.dynamic || word.encoded,
                };
                (heredoc, string, file) = (None, Some(text), None);
            }
            Redirect::Other => {}
        }
    }
    let input = Input {
        heredoc: string.as_ref().or(heredoc),
        file,
        piped,
    };
    let made = command(&simple.words, ctx, out, input)?;
    // A download written out to a file is in that file, not passed on.
    match stdout {
        Some(word) => {
            out.fetch(word, &made, ctx);
            None
        }
        None => Some(made),
    }
}

/// Variables that make the programs started after them run other code.
const LOADS_CODE: &[&str] = &[
    "LD_PRELOAD",
    "LD_LIBRARY_PATH",
    "LD_AUDIT",
    "DYLD_INSERT_LIBRARIES",
    "DYLD_LIBRARY_PATH",
    "BASH_ENV",
    "ENV",
    "PROMPT_COMMAND",
    "GIT_SSH_COMMAND",
    "GIT_SSH",
    "GIT_EXTERNAL_DIFF",
    "GIT_EDITOR",
    "GIT_SEQUENCE_EDITOR",
    "PAGER",
    "NODE_OPTIONS",
    "PYTHONSTARTUP",
    "PERL5OPT",
    "RUBYOPT",
    "GIT_PAGER",
    "GIT_EXEC_PATH",
    "GIT_CONFIG_PARAMETERS",
    "GIT_CONFIG_COUNT",
    "GIT_CONFIG_GLOBAL",
    "GIT_CONFIG_SYSTEM",
    "GIT_ASKPASS",
    "SSH_ASKPASS",
    "GIT_PROXY_COMMAND",
    "GIT_TEMPLATE_DIR",
    "LESSOPEN",
];

/// Values that make a pager, editor or hook setting do nothing of note.
const HARMLESS_VALUES: &[&str] = &[
    "",
    "cat",
    "true",
    "false",
    ":",
    "less",
    "more",
    "/dev/null",
    "0",
    "1",
    "yes",
    "no",
    "on",
    "off",
];

/// Whether `word`, an assignment to `name`, makes later programs run code.
fn loads_code(word: &Word, name: &str) -> bool {
    let value = &word.text[name.len() + 1..];
    LOADS_CODE.contains(&name)
        && (word.dynamic
            || !(matches!(
                name,
                "PAGER" | "GIT_PAGER" | "GIT_EDITOR" | "GIT_SEQUENCE_EDITOR"
            ) && HARMLESS_VALUES.contains(&value.trim())))
}

fn assignment(word: &Word) -> Option<&str> {
    let (name, _) = word.text.split_once('=')?;
    let first = name.chars().next()?;
    (!word.text.starts_with(['"', '\''])
        && (first.is_ascii_alphabetic() || first == '_')
        && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_'))
    .then_some(name)
}

/// Shell builtins that start no program of their own.
const BUILTINS: &[&str] = &[
    "cd", "pushd", "popd", "export", "unset", "set", "shopt", "read", "wait", "exit", "return",
    ":", "true", "false", "test", "[", "[[", "]]", "declare", "local", "typeset", "readonly",
    "let", "shift", "break", "continue", "hash", "ulimit", "umask", "source", ".", "eval", "exec",
    "command", "builtin", "echo", "printf", "pwd", "type", "dirs", "jobs", "disown",
];

/// Folders a program is found in; one started from there is that program.
const BIN_DIRS: &[&str] = &[
    "/bin",
    "/sbin",
    "/usr/bin",
    "/usr/sbin",
    "/usr/local/bin",
    "/usr/local/sbin",
    "/opt/homebrew/bin",
    "/opt/homebrew/sbin",
];

/// Programs that only read: the files they're given are worth saying only
/// when outside the project.
const READS_FILES: &[&str] = &[
    "cat",
    "head",
    "tail",
    "less",
    "more",
    "wc",
    "sort",
    "uniq",
    "cut",
    "diff",
    "cmp",
    "file",
    "stat",
    "md5sum",
    "sha1sum",
    "sha256sum",
    "sha512sum",
    "shasum",
    "b2sum",
    "cksum",
    "xxd",
    "od",
    "hexdump",
    "strings",
    "tac",
    "nl",
    "column",
    "bat",
    "ls",
    "tree",
    "du",
    "realpath",
    "readlink",
    "fmt",
    "fold",
    "paste",
    "join",
    "comm",
    "base64",
    "iconv",
    "lsof",
    "exa",
    "eza",
    "fd",
    "xmllint",
    "zcat",
    "zless",
    "bzcat",
    "xzcat",
    "otool",
    "nm",
    "objdump",
    "ldd",
];
/// Programs whose first plain argument is a pattern or program, not a file.
const READS_AFTER_PATTERN: &[&str] = &["grep", "egrep", "fgrep", "rg", "ag", "ack", "jq", "yq"];
/// Programs whose arguments aren't files and that change nothing.
const NO_FILES: &[&str] = &[
    "echo",
    "printf",
    "true",
    "false",
    "pwd",
    "export",
    "unset",
    "set",
    "shopt",
    "read",
    "sleep",
    "wait",
    "date",
    "whoami",
    "id",
    "uname",
    "hostname",
    "which",
    "type",
    "printenv",
    "locale",
    "ps",
    "pgrep",
    "uptime",
    "free",
    "df",
    "nproc",
    "tput",
    "clear",
    "history",
    "jobs",
    "exit",
    "return",
    ":",
    "seq",
    "yes",
    "expr",
    "test",
    "[",
    "[[",
    "]]",
    "ulimit",
    "umask",
    "basename",
    "dirname",
    "tr",
    "bc",
    "dc",
    "cal",
    "getconf",
    "logname",
    "tty",
    "lsb_release",
    "sw_vers",
    "arch",
    "hash",
    "declare",
    "local",
    "typeset",
    "readonly",
    "let",
    "shift",
    "break",
    "continue",
    "whereis",
    "sync",
    "top",
    "htop",
    "vm_stat",
    "sysctl",
    "dmesg",
    "journalctl",
    "env",
    "fc",
    "times",
    "getopts",
    "disown",
    "dirs",
];
/// Package managers: "install", "add" and the like install software.
const PACKAGES: &[&str] = &[
    "npm",
    "pnpm",
    "yarn",
    "bun",
    "cnpm",
    "cargo",
    "go",
    "brew",
    "pip",
    "pip3",
    "pipx",
    "uv",
    "poetry",
    "conda",
    "mamba",
    "micromamba",
    "gem",
    "apt",
    "apt-get",
    "aptitude",
    "dnf",
    "yum",
    "zypper",
    "apk",
    "snap",
    "flatpak",
    "composer",
    "bundle",
    "rustup",
    "cpanm",
    "cpan",
    "luarocks",
    "dotnet",
    "port",
    "choco",
    "winget",
    "scoop",
    "nix-env",
    "opam",
    "mas",
    "sdkmanager",
    "pkg",
    "deno",
    "rye",
    "pdm",
    "hatch",
];
const INSTALLS: &[&str] = &[
    "install",
    "i",
    "ci",
    "add",
    "update",
    "upgrade",
    "up",
    "reinstall",
    "get",
    "require",
    "sync",
    "link",
    "tap",
    "isntall",
];
const REMOVES: &[&str] = &[
    "uninstall",
    "remove",
    "rm",
    "un",
    "r",
    "erase",
    "purge",
    "autoremove",
    "unlink",
];
const DELETES: &[&str] = &[
    "delete",
    "destroy",
    "drop",
    "prune",
    "rm",
    "rmi",
    "remove",
    "purge",
    "uninstall",
    "wipe",
    "reset",
    "nuke",
    "clean-all",
];
const PUBLISHES: &[&str] = &["publish", "deploy", "release", "push", "upload", "promote"];

fn command(mut args: &[Word], ctx: &mut Ctx, out: &mut Out, input: Input) -> Option<String> {
    while let Some(first) = args.first()
        && let Some(name) = assignment(first)
    {
        if loads_code(first, name) {
            out.unclear(RUNS_OTHER_CODE, ctx);
        }
        args = &args[1..];
    }
    loop {
        let first = args.first()?;
        // `{rm,-rf,~}` and `./bin/*` name their program only as they run.
        let pattern = first.glob && !matches!(first.text.as_str(), "[" | "[[");
        if first.dynamic || first.encoded || pattern {
            out.unclear(WORKED_OUT, ctx);
            return None;
        }
        let rest = &args[1..];
        let (name, path) = program_name(&first.text);
        let name = name.as_str();
        if path
            && !BIN_DIRS
                .iter()
                .any(|d| first.text.starts_with(&format!("{d}/")))
        {
            out.program(&first.text, ctx);
            runs_file(first, false, ctx, out);
            uses(rest, ctx, out);
            return None;
        }
        match name {
            "if" | "then" | "elif" | "else" | "do" | "while" | "until" | "!" | "{" | "}"
            | "time" | "fi" | "done" | "nohup" | "builtin" | "setsid" | "unbuffer"
            | "caffeinate" | "chronic" | "noglob" => {
                args = rest;
                continue;
            }
            "for" | "select" | "esac" => return None,
            "case" | "function" | "coproc" | "trap" | "alias" => {
                out.unclear("Ophio can't follow part of this script", ctx);
                return None;
            }
            "sudo" | "doas" | "run0" | "pkexec" => {
                out.push(Effect::Sudo, ctx);
                args = skip_options(
                    rest,
                    &["-u", "-g", "-C", "-D", "-h", "-p", "-r", "-t", "-U", "-T"],
                );
                continue;
            }
            "su" => {
                out.push(Effect::Sudo, ctx);
                if let Some(at) = rest.iter().position(|w| w.text == "-c")
                    && let Some(code) = rest.get(at + 1)
                {
                    inline_shell(code, ctx, out);
                }
                return None;
            }
            "env" => {
                let mut at = 0;
                while let Some(w) = rest.get(at) {
                    if w.text == "-u"
                        || w.text == "-C"
                        || w.text == "--chdir"
                        || w.text == "--unset"
                    {
                        at += 2;
                    } else if w.text == "-S" || w.text.starts_with("--split-string") {
                        out.unclear(WORKED_OUT, ctx);
                        return None;
                    } else if w.text.starts_with('-') || assignment(w).is_some() {
                        if let Some(name) = assignment(w)
                            && loads_code(w, name)
                        {
                            out.unclear(RUNS_OTHER_CODE, ctx);
                        }
                        at += 1;
                    } else {
                        break;
                    }
                }
                args = &rest[at.min(rest.len())..];
                if args.is_empty() {
                    out.program("env", ctx);
                    return None;
                }
                continue;
            }
            "nice" | "ionice" | "stdbuf" | "taskset" | "chrt" | "exec" | "command" | "flock"
            | "timeout" | "gtimeout" | "watch" | "xargs" | "time-limit" | "systemd-run"
            | "firejail" | "bwrap" | "strace" | "ltrace" | "ccache" | "sccache" => {
                if name == "command"
                    && rest
                        .first()
                        .is_some_and(|w| w.text == "-v" || w.text == "-V")
                {
                    return None;
                }
                let takes: &[&str] = match name {
                    "nice" => &["-n", "--adjustment"],
                    "ionice" => &["-c", "-n", "-p", "-t"],
                    "exec" => &["-a"],
                    "watch" => &["-n", "--interval", "-d"],
                    "xargs" => &[
                        "-n",
                        "-L",
                        "-P",
                        "-s",
                        "-d",
                        "-E",
                        "-I",
                        "-a",
                        "--max-args",
                        "--max-procs",
                        "--delimiter",
                        "--arg-file",
                        "-i",
                    ],
                    "flock" => &["-w", "-E", "-c"],
                    "timeout" | "gtimeout" => &["-k", "-s", "--signal", "--kill-after"],
                    "systemd-run" => &[
                        "-p",
                        "--property",
                        "-u",
                        "--unit",
                        "-E",
                        "--setenv",
                        "--slice",
                    ],
                    "taskset" | "chrt" => &[],
                    _ => &[],
                };
                if name == "systemd-run" || name == "bwrap" || name == "firejail" {
                    out.program(name, ctx);
                }
                if name == "flock"
                    && let Some(at) = rest.iter().position(|w| w.text == "-c")
                    && let Some(code) = rest.get(at + 1)
                {
                    inline_shell(code, ctx, out);
                    return None;
                }
                let mut next = skip_options(rest, takes);
                // The duration, the lock file or the CPU list comes first.
                if matches!(name, "timeout" | "gtimeout" | "flock" | "taskset" | "chrt")
                    && !next.is_empty()
                {
                    next = &next[1..];
                }
                if name == "bwrap" {
                    // Its options take varying values; what follows `--` runs.
                    match rest.iter().position(|w| w.text == "--") {
                        Some(at) => next = &rest[at + 1..],
                        None => {
                            out.unclear("Ophio can't follow part of this script", ctx);
                            return None;
                        }
                    }
                }
                if name == "xargs" {
                    // What it reads from its input becomes the last words,
                    // or stands wherever -I says.
                    let mark = rest
                        .iter()
                        .position(|w| w.text == "-I" || w.text == "-i")
                        .and_then(|at| rest.get(at + 1))
                        .map(|w| w.text.clone());
                    let given = || Word {
                        text: "$input".into(),
                        dynamic: true,
                        open_at: Some(0),
                        shown: Some("what its input lists".into()),
                        ..Word::default()
                    };
                    let mut words: Vec<Word> = next
                        .iter()
                        .map(|w| match &mark {
                            Some(mark) if w.text.contains(mark.as_str()) => given(),
                            _ => w.clone(),
                        })
                        .collect();
                    if words.is_empty() {
                        return None;
                    }
                    if mark.is_none() {
                        words.push(given());
                    }
                    return command(&words, ctx, out, Input::default());
                }
                if name == "watch" && next.len() == 1 && next[0].text.contains(' ') {
                    inline_shell(&next[0], ctx, out);
                    return None;
                }
                if next.is_empty() {
                    out.program(name, ctx);
                    return None;
                }
                args = next;
                continue;
            }
            _ => {}
        }
        if name != "git" {
            out.program(name, ctx);
        }
        return run(name, rest, ctx, out, input);
    }
}

/// The program a command word names, and whether it was written as a path.
fn program_name(text: &str) -> (String, bool) {
    match text.rsplit_once('/') {
        Some((_, base)) => (base.to_string(), true),
        None => (text.to_string(), false),
    }
}

/// The words after a program's options, skipping each option's value.
fn skip_options<'w>(args: &'w [Word], takes: &[&str]) -> &'w [Word] {
    let mut at = 0;
    while let Some(w) = args.get(at) {
        if w.text == "--" {
            return &args[at + 1..];
        }
        if !w.text.starts_with('-') || w.text == "-" {
            break;
        }
        at += if takes.contains(&w.text.as_str()) {
            2
        } else {
            1
        };
    }
    &args[at.min(args.len())..]
}

/// The plain arguments, skipping options and their values.
fn positional<'w>(args: &'w [Word], takes: &[&str]) -> Vec<&'w Word> {
    let mut found = Vec::new();
    let mut at = 0;
    let mut ended = false;
    while let Some(w) = args.get(at) {
        if ended || !w.text.starts_with('-') || w.text == "-" {
            found.push(w);
        } else if w.text == "--" {
            ended = true;
        } else if takes.contains(&w.text.as_str()) {
            at += 1;
        }
        at += 1;
    }
    found
}

fn has_flag(args: &[Word], long: &[&str], short: char) -> bool {
    args.iter().any(|w| {
        let t = w.text.as_str();
        long.iter()
            .any(|l| t == *l || t.starts_with(&format!("{l}=")))
            || (t.starts_with('-') && !t.starts_with("--") && t[1..].contains(short))
    })
}

fn read(word: &Word, ctx: &Ctx, out: &mut Out) {
    if let Some(t) = locate(word, ctx)
        && t.at.reaches_out()
    {
        out.push(Effect::Read(t), ctx);
    }
}

/// Paths an unfamiliar program is given that lead outside the project.
fn uses(args: &[Word], ctx: &Ctx, out: &mut Out) {
    for w in args {
        let value = match w.text.split_once('=') {
            Some((key, value)) if key.starts_with("--") => value,
            _ if w.text.starts_with('-') => continue,
            _ => w.text.as_str(),
        };
        if !looks_like_path(value) {
            continue;
        }
        let mut shown = w.clone();
        shown.text = value.to_string();
        if let Some(open) = w.open_at {
            shown.open_at = open.checked_sub(w.text.len() - value.len());
        }
        if let Some(t) = locate(&shown, ctx)
            && t.at.reaches_out()
        {
            out.push(Effect::Use(t), ctx);
        }
    }
}

fn looks_like_path(text: &str) -> bool {
    !text.contains("://")
        && (text.starts_with('/')
            || text == "~"
            || text.starts_with("~/")
            || text.starts_with("$HOME")
            || text.starts_with("${HOME}")
            || text == ".."
            || text.starts_with("../"))
}

fn inline_shell(code: &Word, ctx: &Ctx, out: &mut Out) {
    if code.dynamic {
        out.unclear(WORKED_OUT, ctx);
    } else {
        let mut inner = ctx.clone();
        read_text(&code.text, &mut inner, out);
    }
}

fn run(name: &str, rest: &[Word], ctx: &mut Ctx, out: &mut Out, input: Input) -> Option<String> {
    let name = name.strip_suffix(".exe").unwrap_or(name);
    // PowerShell's names, in any case.
    match name.to_ascii_lowercase().as_str() {
        "iwr" | "irm" | "invoke-webrequest" | "invoke-restmethod" => {
            return web_request(rest, ctx, out);
        }
        "iex" | "invoke-expression" => {
            match input.piped {
                Some(Some(host)) => out.push(Effect::Downloaded(host.to_string()), ctx),
                Some(None) => out.push(Effect::Built, ctx),
                None if rest.is_empty() => {}
                None => out.unclear("Ophio can't check what Invoke-Expression runs", ctx),
            }
            return None;
        }
        _ => {}
    }
    match name {
        "cd" | "pushd" => {
            let target = positional(rest, &[]).into_iter().next();
            ctx.cwd = match target {
                None => ctx.place.home.clone(),
                Some(w) if w.text == "-" || w.dynamic => None,
                Some(w) => resolve_dir(&w.text, ctx),
            };
            if ctx.on.is_some() {
                ctx.cwd = None;
            }
        }
        "popd" => ctx.cwd = None,
        "rm" | "rmdir" | "unlink" | "shred" | "trash" | "trash-put" | "srm" | "gio" => {
            let args = if name == "gio" {
                match rest.first() {
                    Some(w) if w.text == "trash" || w.text == "remove" => &rest[1..],
                    _ => return None,
                }
            } else {
                rest
            };
            for w in positional(args, &[]) {
                if let Some(t) = locate(w, ctx) {
                    out.push(Effect::Delete(t), ctx);
                }
            }
        }
        "mv" | "cp" | "install" | "ln" | "rsync" | "ditto" => {
            if name == "rsync" {
                // `-e` names the program rsync starts to reach the other end.
                for (i, w) in rest.iter().enumerate() {
                    let program = match w.text.as_str() {
                        "-e" | "--rsh" => rest.get(i + 1).cloned(),
                        t if t.starts_with("--rsh=") => part_of(w, "--rsh=".len(), t.len()),
                        _ => None,
                    };
                    if let Some(program) = program {
                        inline_shell(&program, ctx, out);
                    }
                }
                if rsync_remote(rest, ctx, out) {
                    return None;
                }
            }
            let takes: &[&str] = match name {
                "install" => &[
                    "-m",
                    "-o",
                    "-g",
                    "--mode",
                    "--owner",
                    "--group",
                    "-S",
                    "-t",
                    "--target-directory",
                ],
                "rsync" => &[
                    "-e",
                    "--rsh",
                    "--exclude",
                    "--include",
                    "--filter",
                    "-f",
                    "--files-from",
                    "--port",
                    "--log-file",
                    "--backup-dir",
                    "--suffix",
                    "--chmod",
                    "--chown",
                    "--rsync-path",
                    "--password-file",
                    "--exclude-from",
                    "--include-from",
                ],
                _ => &["-t", "--target-directory", "-S", "--suffix"],
            };
            let mut files = positional(rest, takes);
            let target_dir = rest
                .iter()
                .position(|w| w.text == "-t" || w.text == "--target-directory")
                .and_then(|at| rest.get(at + 1));
            let dest = match target_dir {
                Some(dir) => Some(dir),
                None if files.len() >= 2 => files.pop(),
                // `ln -s target` makes its link here.
                None if name == "ln" => {
                    if let Some(t) = cwd_target(ctx) {
                        out.push(Effect::Change(t), ctx);
                    }
                    None
                }
                None => None,
            };
            if let Some(dest) = dest
                && let Some(t) = locate(dest, ctx)
            {
                out.push(Effect::Change(t), ctx);
            }
            if name == "rsync"
                && has_flag(
                    rest,
                    &[
                        "--delete",
                        "--delete-after",
                        "--delete-before",
                        "--delete-during",
                        "--delete-excluded",
                        "--del",
                    ],
                    '\0',
                )
                && let Some(dest) = dest
                && let Some(mut t) = locate(dest, ctx)
            {
                t.shown = format!("files in {} that aren't in the source", t.shown);
                out.push(Effect::Delete(t), ctx);
            }
            for w in files {
                if name == "mv" {
                    if let Some(t) = locate(w, ctx) {
                        out.push(Effect::Move(t), ctx);
                    }
                } else if name != "ln" {
                    read(w, ctx, out);
                }
            }
        }
        "touch" | "mkdir" | "truncate" | "mkfifo" | "tee" | "chmod" | "chown" | "chgrp"
        | "chattr" | "setfacl" | "xattr" | "setfattr" => {
            let takes: &[&str] = match name {
                "truncate" => &["-s", "--size", "-r", "--reference"],
                "mkdir" => &["-m", "--mode"],
                "touch" => &["-d", "-t", "-r", "--date", "--reference"],
                "setfacl" => &["-m", "-x", "-M", "-X"],
                "xattr" | "setfattr" => &["-w", "-d", "-n", "-v", "-x"],
                _ => &[],
            };
            let mut files = positional(rest, takes);
            if matches!(name, "chmod" | "chown" | "chgrp" | "chattr") && !files.is_empty() {
                files.remove(0);
            }
            for w in files {
                if let Some(t) = locate(w, ctx) {
                    out.push(Effect::Change(t), ctx);
                }
                // `curl … | tee file` saves the download there.
                if name == "tee"
                    && let Some(Some(host)) = input.piped
                {
                    out.fetch(w, host, ctx);
                }
            }
        }
        "sed" | "gsed" => {
            let in_place = rest.iter().any(|w| {
                w.text.starts_with("--in-place")
                    || (w.text.starts_with('-')
                        && !w.text.starts_with("--")
                        && w.text[1..].contains('i'))
            });
            let scripted = rest
                .iter()
                .any(|w| matches!(w.text.as_str(), "-e" | "-f" | "--expression" | "--file"));
            let mut files = positional(
                rest,
                &["-e", "-f", "--expression", "--file", "-l", "--line-length"],
            );
            if !scripted && !files.is_empty() {
                let program = files.remove(0);
                sed_program(&program.text, ctx, out);
            }
            for (at, w) in rest.iter().enumerate() {
                if matches!(w.text.as_str(), "-e" | "--expression")
                    && let Some(program) = rest.get(at + 1)
                {
                    sed_program(&program.text, ctx, out);
                }
            }
            for w in files {
                if in_place {
                    if let Some(t) = locate(w, ctx) {
                        out.push(Effect::Change(t), ctx);
                    }
                } else {
                    read(w, ctx, out);
                }
            }
        }
        "awk" | "gawk" | "mawk" | "nawk" => {
            let scripted = rest.iter().any(|w| w.text == "-f" || w.text == "--file");
            let mut files = positional(
                rest,
                &[
                    "-f",
                    "--file",
                    "-v",
                    "-F",
                    "--assign",
                    "--field-separator",
                    "-i",
                    "--include",
                ],
            );
            if !scripted && !files.is_empty() {
                let program = files.remove(0).text.as_str();
                if program.contains("system")
                    || program.contains('|')
                    || program.contains("getline")
                    || program.contains('>')
                {
                    out.unclear("Ophio can't check what its awk program does", ctx);
                }
            } else if scripted {
                out.unclear("Ophio can't check what its awk program does", ctx);
            }
            let in_place = rest
                .windows(2)
                .any(|w| w[0].text == "-i" && w[1].text == "inplace");
            for w in files {
                if in_place {
                    if let Some(t) = locate(w, ctx) {
                        out.push(Effect::Change(t), ctx);
                    }
                } else {
                    read(w, ctx, out);
                }
            }
        }
        "find" | "gfind" => find(rest, ctx, out, input),
        "git" => git(rest, ctx, out),
        "gh" => gh(rest, ctx, out),
        "ssh" | "autossh" | "mosh" | "et" => ssh(rest, ctx, out, input),
        "scp" | "sftp" => {
            let files = positional(
                rest,
                &[
                    "-c", "-F", "-i", "-J", "-l", "-o", "-P", "-S", "-X", "-D", "-B", "-b",
                ],
            );
            if name == "sftp" {
                if let Some(dest) = files.first() {
                    out.push(
                        Effect::Connect(
                            remote_host(&dest.text).unwrap_or_else(|| host_name(&dest.text)),
                        ),
                        ctx,
                    );
                }
                return None;
            }
            copy_between(&files, ctx, out);
        }
        "curl" | "wget" | "http" | "https" | "xh" | "xhs" | "httpie" | "aria2c" => {
            return download(name, rest, ctx, out);
        }
        "nc" | "ncat" | "netcat" | "telnet" | "ftp" | "ping" | "ping6" | "dig" | "nslookup"
        | "host" | "traceroute" | "mtr" | "socat" | "openssl" | "websocat" | "grpcurl" => {
            if name == "openssl" && !rest.first().is_some_and(|w| w.text == "s_client") {
                return None;
            }
            let hosts = positional(
                rest,
                &[
                    "-p",
                    "-w",
                    "-c",
                    "-i",
                    "-t",
                    "-W",
                    "-s",
                    "-q",
                    "-connect",
                    "-servername",
                ],
            );
            let named = rest
                .iter()
                .position(|w| w.text == "-connect")
                .and_then(|at| rest.get(at + 1))
                .or(hosts.into_iter().find(|w| !w.text.starts_with('@')));
            if let Some(host) = named {
                out.push(Effect::Connect(host_name(&host.text)), ctx);
            }
        }
        "sh" | "bash" | "zsh" | "dash" | "ksh" | "fish" | "ash" | "mksh" | "busybox" | "tcsh"
        | "csh" => shell(name, rest, ctx, out, input),
        "python" | "python2" | "python3" | "pypy" | "pypy3" | "node" | "nodejs" | "perl"
        | "ruby" | "php" | "lua" | "luajit" | "Rscript" | "osascript" | "pwsh" | "powershell"
        | "tclsh" | "swift" | "groovy" | "irb" | "jshell" | "julia" => {
            interpreter(name, rest, ctx, out, input)
        }
        n if n.starts_with("python3.") => interpreter("python3", rest, ctx, out, input),
        "source" | "." => match positional(rest, &[]).first() {
            Some(file) if file.process => out.push(Effect::Built, ctx),
            Some(file) => runs_file(file, true, ctx, out),
            None => out.unclear(UNREADABLE, ctx),
        },
        "eval" => out.unclear("Ophio can't check what eval runs", ctx),
        // `export GIT_SEQUENCE_EDITOR=…` sets it for what runs later.
        "export" | "declare" | "typeset" | "local" | "readonly" => {
            if rest
                .iter()
                .any(|w| assignment(w).is_some_and(|name| loads_code(w, name)))
            {
                out.unclear(RUNS_OTHER_CODE, ctx);
            }
        }
        "script" => {
            // util-linux: `script -c 'command' file`, also `-qc` and
            // `--command=…`; BSD: `script file command…`.
            let (code, words) = script_words(rest);
            if let Some(file) = words.first()
                && let Some(t) = locate(file, ctx)
            {
                out.push(Effect::Change(t), ctx);
            }
            if code.is_empty() && words.len() > 1 {
                let words: Vec<Word> = words[1..].iter().map(|w| (*w).clone()).collect();
                command(&words, &mut ctx.clone(), out, Input::default());
            }
            for code in &code {
                inline_shell(code, ctx, out);
            }
        }
        "vi" | "vim" | "nvim" | "gvim" | "view" | "ex" | "emacs" | "nano" | "micro" | "hx" => {
            // Editor commands given to it can run anything: vim's `!`,
            // emacs's lisp.
            let scripted = name == "ex"
                || rest.iter().any(|w| {
                    let t = w.text.as_str();
                    if name == "emacs" {
                        matches!(
                            t,
                            "--eval" | "-eval" | "--load" | "-l" | "-f" | "--funcall" | "--script"
                        ) || t.starts_with("--eval=")
                            || t.starts_with("--load=")
                            || t.starts_with("--funcall=")
                    } else {
                        // `-c`, `--cmd` and `+cmd` give commands; `-S`, `-s`
                        // and `-u` read them from a file; `-e` and `-E`
                        // take them from the input.
                        matches!(
                            t,
                            "-c" | "--cmd" | "-S" | "-s" | "-u" | "-e" | "-E" | "-es" | "-Es"
                        ) || (t.starts_with('+') && !t[1..].chars().all(|c| c.is_ascii_digit()))
                    }
                });
            if scripted && !matches!(name, "nano" | "micro" | "hx") {
                out.unclear("Ophio can't check the editor commands in it", ctx);
            }
            tool(name, rest, ctx, out);
        }
        "parallel" => out.unclear("Ophio can't check what parallel runs", ctx),
        "npx" | "bunx" | "pnpx" | "uvx" => {
            let package = positional(
                rest,
                &["-p", "--package", "-c", "--call", "--from", "--with"],
            )
            .into_iter()
            .next();
            match package {
                Some(p) if !p.dynamic => out.push(Effect::Fetches(p.text.clone()), ctx),
                _ => out.unclear(WORKED_OUT, ctx),
            }
        }
        "pacman" | "yay" | "paru" | "pikaur" => {
            let flags = rest
                .iter()
                .find(|w| w.text.starts_with('-') && !w.text.starts_with("--"));
            match flags.map(|w| w.text.as_str()) {
                Some(f)
                    if f.starts_with("-S") && !f.contains('s') && !f.contains('i')
                        || f.starts_with("-U") =>
                {
                    out.push(Effect::Install, ctx)
                }
                Some(f) if f.starts_with("-R") => out.push(Effect::Uninstall, ctx),
                Some(f) if f.starts_with("-Q") || f.starts_with("-S") || f.starts_with("-F") => {}
                _ if name != "pacman" && flags.is_none() => out.push(Effect::Install, ctx),
                _ => out.push(runs_program(name, ctx), ctx),
            }
        }
        "reboot" | "shutdown" | "poweroff" | "halt" => out.push(Effect::Power, ctx),
        "dd" => {
            for w in rest {
                if let Some(file) = w.text.strip_prefix("of=") {
                    if file.starts_with("/dev/") && !is_null_device(file) {
                        out.push(Effect::Disk, ctx);
                    } else {
                        let mut f = w.clone();
                        f.text = file.to_string();
                        f.open_at = w.open_at.and_then(|o| o.checked_sub(3));
                        if let Some(t) = locate(&f, ctx) {
                            out.push(Effect::Change(t), ctx);
                        }
                    }
                } else if let Some(file) = w.text.strip_prefix("if=") {
                    let mut f = w.clone();
                    f.text = file.to_string();
                    f.open_at = w.open_at.and_then(|o| o.checked_sub(3));
                    read(&f, ctx, out);
                }
            }
        }
        n if n.starts_with("mkfs") => out.push(Effect::Disk, ctx),
        "fdisk" | "sfdisk" | "gdisk" | "sgdisk" | "parted" | "wipefs" | "cryptsetup" | "mkswap"
        | "blkdiscard" => out.push(Effect::Disk, ctx),
        "diskutil" => match rest.first().map(|w| w.text.to_ascii_lowercase()) {
            Some(sub)
                if sub.contains("erase")
                    || sub.contains("partition")
                    || sub.contains("zero")
                    || sub == "apfs" =>
            {
                out.push(Effect::Disk, ctx)
            }
            _ => {}
        },
        "crontab" => {
            if !rest.iter().any(|w| w.text == "-l") {
                out.push(Effect::Schedule, ctx);
            }
        }
        "kill" | "pkill" | "killall" | "xkill" => out.push(Effect::Stops, ctx),
        "tar" | "bsdtar" | "gtar" => {
            // Old-style `tar xzf file` puts its letters first, without a dash.
            let old = rest
                .first()
                .filter(|w| !w.text.starts_with('-'))
                .map(|w| w.text.clone());
            let flags: String = rest
                .iter()
                .filter(|w| w.text.starts_with('-') && !w.text.starts_with("--"))
                .map(|w| w.text.clone())
                .chain(old.clone())
                .collect();
            let file_at = rest
                .iter()
                .position(|w| {
                    w.text == "--file"
                        || (w.text.starts_with('-')
                            && !w.text.starts_with("--")
                            && w.text.ends_with('f'))
                })
                .map(|at| at + 1)
                .or_else(|| old.as_ref().filter(|o| o.contains('f')).map(|_| 1));
            let inline_file = rest.iter().find_map(|w| {
                w.text
                    .strip_prefix("--file=")
                    .and_then(|_| part_of(w, "--file=".len(), w.text.len()))
            });
            // `-` is its input or output, not a file.
            let file = file_at
                .and_then(|at| rest.get(at))
                .cloned()
                .or(inline_file)
                .filter(|w| w.text != "-");
            let file = file.as_ref();
            if flags.contains('x')
                || rest
                    .iter()
                    .any(|w| w.text == "--extract" || w.text == "--get")
            {
                let dir = rest
                    .iter()
                    .position(|w| w.text == "-C" || w.text == "--directory")
                    .and_then(|at| rest.get(at + 1));
                let into = match dir {
                    Some(dir) => locate(dir, ctx),
                    None => cwd_target(ctx),
                };
                if let Some(t) = into {
                    out.push(Effect::Unpack(t), ctx);
                }
                if let Some(file) = file {
                    read(file, ctx, out);
                }
                unpacks_download(file, dir, ctx, out, input);
            } else if flags.contains('c')
                || flags.contains('r')
                || flags.contains('u')
                || rest
                    .iter()
                    .any(|w| matches!(w.text.as_str(), "--create" | "--append" | "--update"))
            {
                if let Some(file) = file
                    && let Some(t) = locate(file, ctx)
                {
                    out.push(Effect::Change(t), ctx);
                }
                tar_inputs(rest, old.is_some(), file_at, ctx, out);
            }
        }
        "unzip" => {
            let dir = rest
                .iter()
                .position(|w| w.text == "-d")
                .and_then(|at| rest.get(at + 1));
            let into = match dir {
                Some(dir) => locate(dir, ctx),
                None => cwd_target(ctx),
            };
            if let Some(t) = into {
                out.push(Effect::Unpack(t), ctx);
            }
            let archive = positional(rest, &["-d", "-P"]).into_iter().next();
            if let Some(archive) = archive {
                read(archive, ctx, out);
            }
            unpacks_download(archive, dir, ctx, out, input);
        }
        "7z" | "7za" | "7zr" | "7zz" | "unrar" => archiver(name, rest, ctx, out, input),
        "zip" => {
            // `zip [options] archive files…`; `-d` deletes names from it.
            let words = positional(
                rest,
                &[
                    "-b",
                    "--temp-path",
                    "-n",
                    "--suffixes",
                    "-t",
                    "--from-date",
                    "-tt",
                    "--before-date",
                    "-O",
                    "--output-file",
                    "-P",
                    "--password",
                    "-Z",
                    "--compression-method",
                    "-s",
                    "--split-size",
                    "-x",
                    "--exclude",
                    "-i",
                    "--include",
                ],
            );
            let output = rest
                .iter()
                .position(|w| w.text == "-O" || w.text == "--output-file")
                .and_then(|at| rest.get(at + 1));
            if let Some((archive, files)) = words.split_first() {
                // `-` is its output, not a file.
                for changed in output.into_iter().chain([*archive]) {
                    if changed.text != "-"
                        && let Some(t) = locate(changed, ctx)
                    {
                        out.push(Effect::Change(t), ctx);
                    }
                }
                if !rest.iter().any(|w| w.text == "-d" || w.text == "--delete") {
                    for w in files {
                        read(w, ctx, out);
                    }
                }
            }
        }
        "gzip" | "gunzip" | "bunzip2" | "unxz" | "xz" | "bzip2" | "zstd" => {
            if let Some(archive) = positional(rest, &["-S", "--suffix", "-o"]).first()
                && let Some(t) = locate(archive, ctx)
            {
                out.push(Effect::Change(t), ctx);
            }
        }
        "plutil" => {
            let reads = rest.iter().any(|w| {
                matches!(
                    w.text.as_str(),
                    "-p" | "-lint" | "-extract" | "-help" | "-type"
                )
            });
            let files = positional(
                rest,
                &[
                    "-replace",
                    "-insert",
                    "-remove",
                    "-extract",
                    "-convert",
                    "-o",
                    "-e",
                    "-type",
                    "-key",
                    "-append",
                    "-json",
                    "-xml",
                    "-string",
                    "-integer",
                    "-bool",
                    "-date",
                    "-data",
                    "-float",
                    "-dictionary",
                    "-array",
                ],
            );
            let output = rest
                .iter()
                .position(|w| w.text == "-o")
                .and_then(|at| rest.get(at + 1));
            for w in files {
                if reads && output.is_none() {
                    read(w, ctx, out);
                } else if let Some(t) = locate(w, ctx) {
                    out.push(Effect::Change(t), ctx);
                }
            }
            if let Some(o) = output
                && let Some(t) = locate(o, ctx)
            {
                out.push(Effect::Change(t), ctx);
            }
        }
        "PlistBuddy" => {
            let writes = rest.iter().any(|w| {
                let t = w.text.trim_start();
                ["Set", "Add", "Delete", "Merge", "Import", "Clear"]
                    .iter()
                    .any(|v| t.starts_with(v))
            });
            if let Some(file) = positional(rest, &["-c"]).last() {
                if writes {
                    if let Some(t) = locate(file, ctx) {
                        out.push(Effect::Change(t), ctx);
                    }
                } else {
                    read(file, ctx, out);
                }
            }
        }
        "defaults" => {
            if rest.first().is_some_and(|w| {
                matches!(w.text.as_str(), "write" | "delete" | "import" | "rename")
            }) {
                out.push(
                    Effect::GlobalSettings("settings for apps on the computer"),
                    ctx,
                );
            }
        }
        "open" | "xdg-open" | "start" => out.push(runs_program(name, ctx), ctx),
        "systemctl" | "launchctl" | "service" => {
            let sub = positional(
                rest,
                &[
                    "-H",
                    "--host",
                    "-M",
                    "--machine",
                    "-p",
                    "--property",
                    "-t",
                    "--type",
                    "-s",
                    "--signal",
                ],
            )
            .into_iter()
            .next()
            .map(|w| w.text.clone())
            .unwrap_or_default();
            if matches!(
                sub.as_str(),
                "reboot" | "poweroff" | "halt" | "kexec" | "suspend" | "hibernate"
            ) {
                out.push(Effect::Power, ctx);
            } else if !matches!(
                sub.as_str(),
                "status"
                    | "show"
                    | "list-units"
                    | "list-unit-files"
                    | "is-active"
                    | "is-enabled"
                    | "cat"
                    | "list"
                    | "print"
                    | "list-timers"
                    | ""
            ) {
                out.push(runs_program(&format!("{name} {sub}"), ctx), ctx);
            }
        }
        _ if READS_FILES.contains(&name) => {
            for w in positional(
                rest,
                &[
                    "-n", "-c", "-t", "-k", "-o", "-d", "-f", "-s", "-w", "-I", "-L", "--output",
                ],
            ) {
                read(w, ctx, out);
            }
            if name == "sort"
                && let Some(o) = rest
                    .iter()
                    .position(|w| w.text == "-o")
                    .and_then(|at| rest.get(at + 1))
                && let Some(t) = locate(o, ctx)
            {
                out.push(Effect::Change(t), ctx);
            }
        }
        _ if READS_AFTER_PATTERN.contains(&name) => {
            let given = rest
                .iter()
                .any(|w| matches!(w.text.as_str(), "-e" | "-f" | "--regexp" | "--file"));
            let mut files = positional(
                rest,
                &[
                    "-e",
                    "-f",
                    "-m",
                    "-A",
                    "-B",
                    "-C",
                    "-g",
                    "--glob",
                    "-t",
                    "--type",
                    "--regexp",
                    "--file",
                    "--arg",
                    "--argjson",
                    "--slurpfile",
                    "--rawfile",
                    "-T",
                    "--max-count",
                    "--include",
                    "--exclude",
                ],
            );
            if !given && !files.is_empty() {
                files.remove(0);
            }
            for w in files {
                read(w, ctx, out);
            }
        }
        _ if NO_FILES.contains(&name) => {}
        _ => tool(name, rest, ctx, out),
    }
    None
}

fn runs_program(name: &str, ctx: &Ctx) -> Effect {
    Effect::Runs(Target {
        shown: name.to_string(),
        at: if ctx.on.is_some() {
            Where::Remote
        } else {
            Where::Program
        },
    })
}

fn cwd_target(ctx: &Ctx) -> Option<Target> {
    let mut here = Word {
        text: ".".into(),
        ..Word::default()
    };
    if ctx.cwd.is_none() {
        here.expansion();
    }
    locate(&here, ctx)
}

/// What `tar` puts into an archive: the files it reads, from wherever
/// `-C` last moved it.
fn tar_inputs(rest: &[Word], old: bool, file_at: Option<usize>, ctx: &Ctx, out: &mut Out) {
    const TAKES: &[&str] = &[
        "-C",
        "--directory",
        "-T",
        "--files-from",
        "-X",
        "--exclude-from",
        "--exclude",
        "-b",
        "--blocking-factor",
        "-H",
        "--format",
        "-I",
        "--use-compress-program",
        "--owner",
        "--group",
        "--mode",
        "--transform",
        "--xform",
        "-V",
        "--label",
        "-N",
        "--newer",
        "--after-date",
        "--mtime",
        "--file",
    ];
    let mut here = ctx.clone();
    let mut at = usize::from(old);
    let mut ended = false;
    while let Some(w) = rest.get(at) {
        let t = w.text.as_str();
        if Some(at) == file_at {
            // The archive itself.
        } else if ended || !t.starts_with('-') || t == "-" {
            if t != "-" {
                read(w, &here, out);
            }
        } else if t == "--" {
            ended = true;
        } else if let Some(value) = rest.get(at + 1).filter(|_| TAKES.contains(&t)) {
            match t {
                "-C" | "--directory" => {
                    here.cwd = if value.dynamic {
                        None
                    } else {
                        resolve_dir(&value.text, &here)
                    };
                }
                "-T" | "--files-from" | "-X" | "--exclude-from" => read(value, &here, out),
                _ => {}
            }
            at += 1;
        }
        at += 1;
    }
}

/// What `script` is told to run with `-c`, however its flags are written,
/// and its other plain words: the file it writes, then BSD's command.
fn script_words(rest: &[Word]) -> (Vec<Word>, Vec<&Word>) {
    // Short options that take a value, attached or as the next word.
    const SHORT: &str = "cEIOBTtmF";
    const LONG: &[&str] = &[
        "command",
        "echo",
        "log-in",
        "log-out",
        "log-io",
        "log-timing",
        "logging-format",
    ];
    let mut code = Vec::new();
    let mut words = Vec::new();
    let mut ended = false;
    let mut at = 0;
    while let Some(w) = rest.get(at) {
        let t = w.text.as_str();
        if ended || !t.starts_with('-') || t == "-" {
            words.push(w);
            // BSD's command starts at the word after the file, and its own
            // options are its own.
            ended = words.len() >= 2;
        } else if t == "--" {
            ended = true;
        } else if let Some(long) = t.strip_prefix("--") {
            let (key, inline) = match long.split_once('=') {
                Some((k, v)) => (k, Some(v)),
                None => (long, None),
            };
            let value = match inline {
                Some(v) => part_of(w, t.len() - v.len(), t.len()),
                None if LONG.contains(&key) => {
                    at += 1;
                    rest.get(at).cloned()
                }
                None => None,
            };
            if key == "command" {
                code.extend(value);
            }
        } else {
            for (k, c) in t.char_indices().skip(1) {
                if SHORT.contains(c) {
                    let after = k + c.len_utf8();
                    let value = if after < t.len() {
                        part_of(w, after, t.len())
                    } else {
                        at += 1;
                        rest.get(at).cloned()
                    };
                    if c == 'c' {
                        code.extend(value);
                    }
                    break;
                }
            }
        }
        at += 1;
    }
    (code, words)
}

/// `7z <command> [switches] archive [files]` and `unrar <command> [switches]
/// archive [files] [folder/]`: what it packs, changes or unpacks.
fn archiver(name: &str, rest: &[Word], ctx: &Ctx, out: &mut Out, input: Input) {
    let mut ended = false;
    let mut words: Vec<&Word> = Vec::new();
    // 7z's switches carry their values: `-o/tmp/x`, `-pSecret`.
    let mut into: Option<Word> = None;
    for w in rest {
        let t = w.text.as_str();
        if ended || !t.starts_with('-') || t == "-" {
            words.push(w);
        } else if t == "--" {
            ended = true;
        } else if t.starts_with("-o") && name != "unrar" {
            into = part_of(w, 2, t.len());
        }
    }
    let Some((what, words)) = words.split_first() else {
        return;
    };
    let (archive, files) = match words.split_first() {
        Some((archive, files)) => (Some(*archive), files),
        None => (None, &[][..]),
    };
    let changes = |out: &mut Out| {
        if let Some(t) = archive.and_then(|a| locate(a, ctx)) {
            out.push(Effect::Change(t), ctx);
        }
    };
    match what.text.to_ascii_lowercase().as_str() {
        "a" | "u" => {
            changes(out);
            for w in files {
                read(w, ctx, out);
            }
        }
        "d" | "rn" => changes(out),
        "x" | "e" => {
            // unrar names the folder last, ending in `/`.
            if name == "unrar" {
                into = files
                    .last()
                    .filter(|w| w.text.ends_with('/'))
                    .map(|w| (*w).clone());
            }
            let target = match &into {
                Some(dir) => locate(dir, ctx),
                None => cwd_target(ctx),
            };
            if let Some(t) = target {
                out.push(Effect::Unpack(t), ctx);
            }
            if let Some(archive) = archive {
                read(archive, ctx, out);
            }
            unpacks_download(archive, into.as_ref(), ctx, out, input);
        }
        _ => {
            if let Some(archive) = archive {
                read(archive, ctx, out);
            }
        }
    }
}

/// An archive an earlier step downloaded unpacks what was downloaded: what
/// runs from where it unpacks may be that download.
fn unpacks_download(
    archive: Option<&Word>,
    into: Option<&Word>,
    ctx: &Ctx,
    out: &mut Out,
    input: Input,
) {
    let host = match archive.or(input.file) {
        Some(file) => out.fetched_from(file, ctx),
        None => input.piped.flatten().map(str::to_string),
    };
    if let Some(host) = host {
        let here = plain(".");
        out.fetch(into.unwrap_or(&here), &host, ctx);
    }
}

/// GNU sed's `e` command and `w` file run or write as it goes.
fn sed_program(program: &str, ctx: &Ctx, out: &mut Out) {
    let executes = program.lines().any(|line| {
        let line = line.trim_start();
        line.starts_with('e') || line.starts_with('w') || line.starts_with('W')
    }) || program.contains("/e")
        || program.contains("/w ")
        || program.contains(";e")
        || program.contains("; e");
    if executes {
        out.unclear("Ophio can't check what its sed program does", ctx);
    }
}

/// A program Ophio has no particular reading of: what it is, and whether it
/// installs, deletes or publishes.
fn tool(name: &str, rest: &[Word], ctx: &Ctx, out: &mut Out) {
    let package = PACKAGES.contains(&name);
    let subcommands = package
        || matches!(
            name,
            "docker"
                | "podman"
                | "kubectl"
                | "helm"
                | "make"
                | "gmake"
                | "just"
                | "gradle"
                | "gradlew"
                | "mvn"
                | "swift"
                | "flutter"
                | "xcrun"
                | "terraform"
                | "tofu"
                | "pulumi"
                | "fly"
                | "flyctl"
                | "vercel"
                | "netlify"
                | "firebase"
                | "wrangler"
                | "heroku"
                | "gcloud"
                | "aws"
                | "az"
                | "sf"
                | "tauri"
                | "docker-compose"
                | "nix"
                | "fastlane"
                | "pod"
                | "ansible-playbook"
                | "supabase"
                | "railway"
                | "doctl"
                | "kamal"
                | "twine"
                | "mix"
                | "lein"
                | "sbt"
                | "zig"
                | "meson"
                | "ninja"
                | "cmake"
                | "bazel"
                | "turbo"
                | "nx"
                | "lerna"
                | "goreleaser"
                | "ophio"
        );
    // A subcommand reads as a plain word: `cargo test`, `docker system prune`.
    let words: Vec<&str> = rest
        .iter()
        .filter(|w| !w.text.starts_with('-'))
        .take(2)
        .take_while(|w| {
            !w.dynamic
                && !w.text.is_empty()
                && w.text
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || "-_:".contains(c))
        })
        .map(|w| w.text.as_str())
        .collect();
    let sub = words.first().copied();
    let second = words.get(1).copied();
    let shown = match (sub, second) {
        (Some(sub), Some(next))
            if subcommands
                && (matches!(sub, "run" | "run-script" | "exec" | "x")
                    || DELETES.contains(&next)
                    || PUBLISHES.contains(&next)) =>
        {
            format!("{name} {sub} {next}")
        }
        (Some(sub), _) if subcommands => format!("{name} {sub}"),
        _ => name.to_string(),
    };
    let either = |set: &[&str]| {
        sub.is_some_and(|s| set.contains(&s)) || second.is_some_and(|s| set.contains(&s))
    };
    // `yarn` or `bun` alone installs what the project lists.
    let bare_install = matches!(name, "yarn" | "bun") && sub.is_none();
    let installs_with = |what: &str| rest.iter().any(|w| w.text == what);
    match sub {
        Some(s)
            if package && (INSTALLS.contains(&s) || (s == "pip" && installs_with("install"))) =>
        {
            out.push(Effect::Install, ctx)
        }
        Some("tool" | "toolchain" | "component" | "target" | "global" | "self" | "package")
            if package
                && (installs_with("install")
                    || installs_with("add")
                    || installs_with("update")
                    || installs_with("upgrade")) =>
        {
            out.push(Effect::Install, ctx)
        }
        Some(s) if package && REMOVES.contains(&s) => out.push(Effect::Uninstall, ctx),
        Some(_) if subcommands && either(DELETES) => out.push(Effect::DeletesWith(shown), ctx),
        Some(_) if subcommands && either(PUBLISHES) => out.push(Effect::Publish(shown), ctx),
        Some("install") if subcommands => out.push(Effect::Install, ctx),
        _ if bare_install => out.push(Effect::Install, ctx),
        _ => out.push(runs_program(&shown, ctx), ctx),
    }
    uses(rest, ctx, out);
}

fn find(rest: &[Word], ctx: &mut Ctx, out: &mut Out, input: Input) {
    let roots: Vec<&Word> = rest
        .iter()
        .take_while(|w| {
            !w.text.starts_with('-') && w.text != "(" && w.text != "!" && w.text != "\\("
        })
        .collect();
    for root in &roots {
        read(root, ctx, out);
    }
    let mut at = roots.len();
    while at < rest.len() {
        let t = rest[at].text.as_str();
        match t {
            "-delete" => {
                let base = roots.first().copied();
                let target = match base {
                    Some(root) => locate(root, ctx),
                    None => cwd_target(ctx),
                };
                if let Some(mut t) = target {
                    t.shown = format!("files it finds in {}", t.shown);
                    out.push(Effect::Delete(t), ctx);
                }
            }
            "-exec" | "-execdir" | "-ok" | "-okdir" => {
                let start = at + 1;
                let mut end = start;
                while end < rest.len() && rest[end].text != ";" && rest[end].text != "+" {
                    end += 1;
                }
                // Each file found stands where `{}` is: somewhere under a root.
                let root = match roots.as_slice() {
                    [only] if !only.dynamic => only.text.trim_end_matches('/').to_string(),
                    [] => ".".to_string(),
                    _ => String::new(),
                };
                let words: Vec<Word> = rest[start..end]
                    .iter()
                    .map(|w| {
                        let mut w = w.clone();
                        if w.text == "{}" && !root.is_empty() {
                            let place = locate(&plain(&root), ctx).map(|t| t.shown);
                            w.text = format!("{root}/*");
                            w.glob = true;
                            w.open_at = Some(root.len() + 1);
                            w.shown = place.map(|p| format!("files it finds in {p}"));
                        } else if w.text.contains("{}") {
                            w.text = w.text.replace("{}", "$found");
                            w.dynamic = true;
                            w.open_at = Some(0);
                        }
                        w
                    })
                    .collect();
                let mut inner = ctx.clone();
                command(&words, &mut inner, out, input);
                at = end;
            }
            "-fprint" | "-fprint0" | "-fprintf" | "-fls" => {
                if let Some(file) = rest.get(at + 1)
                    && let Some(t) = locate(file, ctx)
                {
                    out.push(Effect::Change(t), ctx);
                }
                at += 1;
            }
            _ => {}
        }
        at += 1;
    }
}

fn git(rest: &[Word], ctx: &mut Ctx, out: &mut Out) {
    let mut at = 0;
    let mut dir: Option<&Word> = None;
    while let Some(w) = rest.get(at) {
        match w.text.as_str() {
            "-C" => {
                dir = rest.get(at + 1);
                at += 2;
            }
            "-c" => {
                if let Some(setting) = rest.get(at + 1) {
                    let (key, value) = setting
                        .text
                        .split_once('=')
                        .unwrap_or((&setting.text, "true"));
                    if setting.dynamic || git_setting_runs(key, value) {
                        out.unclear(RUNS_OTHER_CODE, ctx);
                    }
                }
                at += 2;
            }
            "--git-dir" | "--work-tree" | "--namespace" | "--config-env" => {
                if w.text == "--config-env" {
                    out.unclear(RUNS_OTHER_CODE, ctx);
                }
                at += 2;
            }
            // Where git finds its own commands: any of them could be other code.
            t if t.starts_with("--exec-path=") => {
                out.unclear(RUNS_OTHER_CODE, ctx);
                at += 1;
            }
            t if t.starts_with("--config-env=") => {
                out.unclear(RUNS_OTHER_CODE, ctx);
                at += 1;
            }
            t if t.starts_with('-') => at += 1,
            _ => break,
        }
    }
    let Some(sub) = rest.get(at) else {
        out.program("git", ctx);
        return;
    };
    out.program(&format!("git {}", clip(&sub.text)), ctx);
    let args = &rest[at + 1..];
    let mut here = ctx.clone();
    if let Some(dir) = dir {
        here.cwd = if dir.dynamic {
            None
        } else {
            resolve_dir(&dir.text, ctx)
        };
    }
    let repo = || cwd_target(&here);
    let remote = |words: &[Word]| -> String {
        positional(
            words,
            &[
                "-o",
                "--push-option",
                "--repo",
                "--receive-pack",
                "--exec",
                "--upload-pack",
                "--depth",
                "-b",
                "--branch",
                "--origin",
                "-j",
                "--jobs",
                "--reference",
                "--separate-git-dir",
                "--filter",
                "--recurse-submodules",
                "-c",
                "--config",
                "--shallow-since",
                "--template",
            ],
        )
        .first()
        .map(|w| remote_name(&w.text))
        .unwrap_or_else(|| "its remote".into())
    };
    match sub.text.as_str() {
        "commit" | "cherry-pick" | "revert" | "am" | "merge" => {
            if sub.text == "commit" && has_flag(args, &["--amend"], '\0') {
                out.push(Effect::History, ctx);
            }
            if let Some(t) = repo() {
                out.push(Effect::Commit(t), ctx);
            }
        }
        "push" => {
            let force = has_flag(
                args,
                &[
                    "--force",
                    "--force-with-lease",
                    "--force-if-includes",
                    "--mirror",
                ],
                'f',
            ) || positional(args, &[])
                .iter()
                .skip(1)
                .any(|w| w.text.starts_with('+'));
            if has_flag(args, &["--delete", "--prune"], 'd')
                || positional(args, &[])
                    .iter()
                    .skip(1)
                    .any(|w| w.text.starts_with(':'))
            {
                out.push(Effect::DeleteBranch, ctx);
            }
            out.push(
                Effect::Push {
                    remote: remote(args),
                    force,
                },
                ctx,
            );
        }
        "pull" => {
            out.push(Effect::Pull(remote(args)), ctx);
            if let Some(t) = repo()
                && t.at.reaches_out()
            {
                out.push(Effect::Change(t), ctx);
            }
        }
        "fetch" | "ls-remote" => out.push(Effect::Download(remote(args)), ctx),
        "clone" => {
            let words = positional(
                args,
                &[
                    "-b",
                    "--branch",
                    "--depth",
                    "-o",
                    "--origin",
                    "-c",
                    "--config",
                    "--reference",
                    "--separate-git-dir",
                    "--filter",
                    "-j",
                    "--jobs",
                    "--template",
                    "-u",
                    "--upload-pack",
                    "--shallow-since",
                    "--recurse-submodules",
                ],
            );
            if let Some(url) = words.first() {
                out.push(Effect::Download(remote_name(&url.text)), ctx);
                let dest = match words.get(1) {
                    Some(dest) => locate(dest, &here),
                    None => {
                        let name = url.text.trim_end_matches('/').trim_end_matches(".git");
                        let name = name.rsplit(['/', ':']).next().unwrap_or(name);
                        // Its name comes from the address, so it is where
                        // the address says only when the address is known.
                        let mut w = plain(name);
                        if url.dynamic {
                            w.dynamic = true;
                            w.open_at = Some(0);
                        }
                        locate(&w, &here)
                    }
                };
                if let Some(t) = dest {
                    out.push(Effect::Change(t), ctx);
                }
                // What it puts there is downloaded: running it later runs that.
                let folder = match words.get(1) {
                    Some(dest) => (*dest).clone(),
                    None => {
                        let name = url.text.trim_end_matches('/').trim_end_matches(".git");
                        plain(name.rsplit(['/', ':']).next().unwrap_or(name))
                    }
                };
                out.fetch(&folder, &remote_name(&url.text), &here);
            }
        }
        "bisect" if args.first().is_some_and(|w| w.text == "run") => {
            // Runs the command after `run` at each step.
            command(&args[1..], &mut here.clone(), out, Input::default());
        }
        // `git submodule [--quiet] [--cached] foreach …`
        "submodule"
            if skip_options(args, &[])
                .first()
                .is_some_and(|w| w.text == "foreach") =>
        {
            let code = skip_options(&skip_options(args, &[])[1..], &[]);
            match code {
                [one] => inline_shell(one, &here, out),
                [] => {}
                many => {
                    command(many, &mut here.clone(), out, Input::default());
                }
            }
        }
        "reset" => {
            if has_flag(args, &["--hard", "--merge", "--keep"], '\0') {
                out.push(Effect::Discard, ctx);
            } else if positional(args, &[]).iter().any(|w| {
                w.text.contains(['~', '^'])
                    || w.text.starts_with("HEAD@")
                    || w.text.starts_with("origin/")
                    || (w.text.len() >= 7 && w.text.chars().all(|c| c.is_ascii_hexdigit()))
            }) {
                out.push(Effect::History, ctx);
            }
        }
        "checkout" | "switch" => {
            let words = positional(args, &["-b", "-B", "-c", "-C", "--orphan", "--conflict"]);
            let discards = if sub.text == "checkout" {
                args.iter().any(|w| w.text == "--" || w.text == ".")
                    || has_flag(args, &["--force"], 'f')
            } else {
                has_flag(args, &["--discard-changes", "--force"], 'f')
            };
            if discards {
                out.push(Effect::Discard, ctx);
            } else if !words.is_empty()
                || args
                    .iter()
                    .any(|w| matches!(w.text.as_str(), "-b" | "-B" | "-c" | "-C" | "--orphan"))
            {
                out.push(Effect::Branch, ctx);
            }
        }
        "restore" => {
            // `--staged` alone only takes changes out of the next commit.
            let staged = args.iter().any(|w| w.text == "--staged" || w.text == "-S");
            let worktree = args
                .iter()
                .any(|w| w.text == "--worktree" || w.text == "-W");
            if !staged || worktree {
                out.push(Effect::Discard, ctx);
            }
        }
        "clean" => {
            if has_flag(args, &["--force"], 'f')
                && !has_flag(args, &["--dry-run"], 'n')
                && let Some(mut t) = repo()
            {
                t.shown = format!("files git doesn't track in {}", t.shown);
                out.push(Effect::Delete(t), ctx);
            }
        }
        "stash" => {
            if args
                .first()
                .is_some_and(|w| w.text == "drop" || w.text == "clear")
            {
                out.push(Effect::Discard, ctx);
            } else if !args
                .first()
                .is_some_and(|w| matches!(w.text.as_str(), "list" | "show"))
            {
                out.push(runs_program("git stash", ctx), ctx);
            }
        }
        "rebase" | "filter-branch" | "filter-repo" | "replace" => {
            out.push(Effect::History, ctx);
            // `rebase --exec` runs a command after each commit it makes.
            if sub.text == "rebase" {
                for code in rebase_execs(args) {
                    inline_shell(&code, &here, out);
                }
            }
        }
        "branch" | "tag" => {
            if has_flag(args, &["--delete"], 'd') || args.iter().any(|w| w.text == "-D") {
                out.push(Effect::DeleteBranch, ctx);
            } else if has_flag(args, &["--force"], 'f') || args.iter().any(|w| w.text == "-M") {
                out.push(Effect::History, ctx);
            }
        }
        "rm" => {
            if !has_flag(args, &["--cached", "--dry-run"], 'n') {
                for w in positional(args, &[]) {
                    if let Some(t) = locate(w, &here) {
                        out.push(Effect::Delete(t), ctx);
                    }
                }
            }
        }
        "mv" => {
            let mut words = positional(args, &[]);
            if words.len() >= 2 {
                let dest = words.pop();
                if let Some(dest) = dest
                    && let Some(t) = locate(dest, &here)
                    && t.at.reaches_out()
                {
                    out.push(Effect::Change(t), ctx);
                }
                for w in words {
                    if let Some(t) = locate(w, &here) {
                        out.push(Effect::Move(t), ctx);
                    }
                }
            }
        }
        "config" => {
            let global = has_flag(args, &["--global", "--system"], '\0');
            let reading = has_flag(
                args,
                &[
                    "--get",
                    "--get-all",
                    "--list",
                    "--get-regexp",
                    "--show-origin",
                ],
                'l',
            ) || positional(args, &["--file", "-f", "--type", "--default"]).len() < 2
                && !has_flag(
                    args,
                    &[
                        "--unset",
                        "--unset-all",
                        "--add",
                        "--replace-all",
                        "--remove-section",
                        "--rename-section",
                    ],
                    '\0',
                );
            if !reading {
                // `git config set key value` and `git config key value`.
                let words = positional(args, &["--file", "-f", "--type", "--default", "--value"]);
                let words = match words.first() {
                    Some(w) if matches!(w.text.as_str(), "set" | "unset") => &words[1..],
                    _ => &words[..],
                };
                let unsets = has_flag(args, &["--unset", "--unset-all", "--remove-section"], '\0')
                    || positional(args, &[])
                        .first()
                        .is_some_and(|w| w.text == "unset");
                if let Some(key) = words.first().filter(|_| !unsets) {
                    let value = words.get(1).map_or("", |w| w.text.as_str());
                    if key.dynamic
                        || words.get(1).is_some_and(|w| w.dynamic)
                        || git_setting_runs(&key.text, value)
                    {
                        out.unclear(RUNS_OTHER_CODE, ctx);
                    }
                }
                if global {
                    out.push(
                        Effect::GlobalSettings("git settings for the whole computer"),
                        ctx,
                    );
                } else {
                    out.push(runs_program("git config", ctx), ctx);
                }
            }
        }
        "add" | "status" | "log" | "diff" | "show" | "blame" | "grep" | "ls-files" | "ls-tree"
        | "rev-parse" | "describe" | "shortlog" | "reflog" | "show-ref" | "cat-file"
        | "name-rev" | "merge-base" | "whatchanged" | "help" | "version" | "rev-list"
        | "for-each-ref" | "check-ignore" | "remote" | "var" | "count-objects" | "fsck"
        | "symbolic-ref" | "worktree"
            if !(sub.text == "remote"
                && args.first().is_some_and(|w| {
                    matches!(
                        w.text.as_str(),
                        "add" | "remove" | "rm" | "set-url" | "rename"
                    )
                }))
                && !(sub.text == "worktree"
                    && args
                        .first()
                        .is_some_and(|w| matches!(w.text.as_str(), "remove" | "prune"))) => {}
        "worktree" => {
            out.push(
                Effect::DeletesWith(format!(
                    "git worktree {}",
                    args.first().map(|w| w.text.as_str()).unwrap_or_default()
                )),
                ctx,
            );
        }
        other => out.push(runs_program(&format!("git {other}"), ctx), ctx),
    }
    // A repository outside the project is worth saying whatever happens in it.
    if let Some(t) = cwd_target(&here)
        && t.at.reaches_out()
        && dir.is_some()
    {
        out.push(Effect::Use(t), ctx);
    }
}

/// The commands `git rebase` is told to run after each commit: `-x cmd`,
/// `-x'cmd'`, `-ix cmd`, `--exec cmd`, `--exec=cmd` and the shortened
/// `--exe` and `--ex`.
fn rebase_execs(args: &[Word]) -> Vec<Word> {
    let mut found = Vec::new();
    for (i, w) in args.iter().enumerate() {
        let t = w.text.as_str();
        if t == "--" {
            break;
        }
        if let Some(long) = t.strip_prefix("--") {
            let (key, inline) = match long.split_once('=') {
                Some((k, v)) => (k, Some(v)),
                None => (long, None),
            };
            if key.len() >= 2 && "exec".starts_with(key) {
                found.extend(match inline {
                    Some(v) => part_of(w, t.len() - v.len(), t.len()),
                    None => args.get(i + 1).cloned(),
                });
            }
        } else if t.starts_with('-') {
            // Short flags may share one word; one that takes a value ends it.
            for (k, c) in t.char_indices().skip(1) {
                match c {
                    'x' => {
                        let after = k + c.len_utf8();
                        found.extend(if after < t.len() {
                            part_of(w, after, t.len())
                        } else {
                            args.get(i + 1).cloned()
                        });
                        break;
                    }
                    's' | 'X' | 'C' | 'S' => break,
                    _ => {}
                }
            }
        }
    }
    found
}

/// Whether setting git's `key` to `value` makes git run a command of that
/// setting's choosing: a pager, editor, hook, alias, helper or filter.
fn git_setting_runs(key: &str, value: &str) -> bool {
    let key = key.to_ascii_lowercase();
    let value = value.trim();
    let first = key.split('.').next().unwrap_or_default();
    let last = key.rsplit('.').next().unwrap_or_default();
    // An alias can stand for any git command, or with `!` any command.
    if first == "alias" {
        return true;
    }
    // `ext::` addresses run a command.
    if matches!(key.as_str(), "protocol.allow" | "protocol.ext.allow") {
        return value != "never";
    }
    if HARMLESS_VALUES.contains(&value) {
        return false;
    }
    // `submodule.<name>.update=!command` runs the command on update.
    if first == "submodule" && last == "update" && value.starts_with('!') {
        return true;
    }
    matches!(first, "pager" | "include" | "includeif")
        || matches!(last, "path" | "hookspath" | "templatedir" | "smtpserver")
        || [
            "command",
            "cmd",
            "pager",
            "editor",
            "program",
            "helper",
            "hook",
            "fsmonitor",
            "askpass",
            "gitproxy",
            "external",
            "difffilter",
            "textconv",
            "driver",
            "clean",
            "smudge",
            "process",
            "uploadpack",
            "receivepack",
        ]
        .iter()
        .any(|s| last.ends_with(s))
}

fn gh(rest: &[Word], ctx: &Ctx, out: &mut Out) {
    let words = positional(
        rest,
        &[
            "-R",
            "--repo",
            "--hostname",
            "-X",
            "--method",
            "-f",
            "-F",
            "--field",
            "--raw-field",
            "-H",
            "--header",
            "--input",
            "-q",
            "--jq",
            "-t",
            "--template",
            "--json",
            "-b",
            "--body",
            "-B",
            "--base",
            "--head",
            "-l",
            "--label",
            "-a",
            "--assignee",
            "-m",
            "--milestone",
            "--title",
            "-T",
            "-r",
            "--reviewer",
            "-p",
            "--project",
            "-L",
            "--limit",
            "-s",
            "--state",
            "-A",
            "--author",
            "-w",
            "--web",
        ],
    );
    let group = words.first().map(|w| w.text.as_str()).unwrap_or_default();
    let action = words.get(1).map(|w| w.text.as_str()).unwrap_or_default();
    let posts = match group {
        "api" => {
            let method = rest
                .iter()
                .position(|w| w.text == "-X" || w.text == "--method")
                .and_then(|at| rest.get(at + 1))
                .map(|w| w.text.to_ascii_uppercase());
            matches!(method.as_deref(), Some(m) if m != "GET")
                || (method.is_none()
                    && rest.iter().any(|w| {
                        matches!(
                            w.text.as_str(),
                            "-f" | "-F" | "--field" | "--raw-field" | "--input"
                        )
                    }))
        }
        "auth" | "config" | "extension" | "alias" => {
            out.push(runs_program(&format!("gh {group}"), ctx), ctx);
            return;
        }
        _ => matches!(
            action,
            "create"
                | "comment"
                | "merge"
                | "close"
                | "reopen"
                | "edit"
                | "review"
                | "ready"
                | "delete"
                | "upload"
                | "fork"
                | "rename"
                | "archive"
                | "unarchive"
                | "set"
                | "remove"
                | "enable"
                | "disable"
                | "run"
                | "rerun"
                | "cancel"
                | "lock"
                | "unlock"
                | "transfer"
                | "pin"
                | "unpin"
                | "develop"
                | "sync"
                | "deploy"
                | "add"
                | "clone"
        ),
    };
    if group == "repo" && action == "clone" {
        out.push(Effect::Download("GitHub".into()), ctx);
        if let Some(t) = cwd_target(ctx) {
            out.push(Effect::Change(t), ctx);
        }
    } else if posts {
        out.push(Effect::PostsGitHub, ctx);
    } else {
        out.push(Effect::Connect("GitHub".into()), ctx);
    }
}

fn ssh(rest: &[Word], ctx: &Ctx, out: &mut Out, input: Input) {
    const TAKES: &[&str] = &[
        "-b", "-c", "-D", "-E", "-e", "-F", "-I", "-i", "-J", "-L", "-l", "-m", "-O", "-o", "-p",
        "-Q", "-R", "-S", "-W", "-w", "-B", "--port", "--ssh",
    ];
    let mut at = 0;
    while let Some(w) = rest.get(at) {
        let t = w.text.as_str();
        if !t.starts_with('-') {
            break;
        }
        let option = if t == "-o" {
            rest.get(at + 1).map(|w| w.text.as_str())
        } else {
            t.strip_prefix("-o")
        };
        if let Some(option) = option {
            let key = option
                .split(['=', ' '])
                .next()
                .unwrap_or_default()
                .to_ascii_lowercase();
            if matches!(
                key.as_str(),
                "proxycommand"
                    | "localcommand"
                    | "permitlocalcommand"
                    | "knownhostscommand"
                    | "remotecommand"
                    | "match"
            ) {
                out.unclear(
                    "Ophio can't check the command ssh is told to run on this computer",
                    ctx,
                );
            }
        }
        at += if TAKES.contains(&t) { 2 } else { 1 };
    }
    let Some(dest) = rest.get(at) else { return };
    if dest.dynamic {
        out.unclear(WORKED_OUT, ctx);
        return;
    }
    let host = host_name(&dest.text);
    out.push(Effect::Connect(host.clone()), ctx);
    let remote_words = &rest[at + 1..];
    let mut far = ctx.clone();
    far.on = Some(host);
    far.cwd = None;
    if !remote_words.is_empty() {
        let text = remote_words
            .iter()
            .map(|w| w.text.as_str())
            .collect::<Vec<_>>()
            .join(" ");
        far.depth += 1;
        read_text(&text, &mut far, out);
        // A remote shell or interpreter given no script reads it from the
        // input: `ssh host bash`, `ssh host "bash -s -- arg"`.
        let tokens: Vec<&str> = text.split_whitespace().collect();
        let program = tokens
            .first()
            .map(|t| program_name(t).0)
            .unwrap_or_default();
        let shell = matches!(program.as_str(), "bash" | "sh" | "zsh" | "dash");
        let flags: Vec<&str> = tokens
            .iter()
            .skip(1)
            .take_while(|t| t.starts_with('-') && **t != "--")
            .copied()
            .collect();
        let reads_input = (shell
            || matches!(
                program.as_str(),
                "python" | "python3" | "perl" | "ruby" | "node"
            ))
            && !flags.iter().any(|t| t.contains('c'))
            && (flags.len() + 1 == tokens.len()
                || (shell && flags.iter().any(|t| t[1..].contains('s'))));
        if reads_input {
            match (input.heredoc, input.file, input.piped) {
                (Some(heredoc), _, _) if shell => heredoc_script(heredoc, &mut far, out),
                (Some(_), _, _) => out.unclear("Ophio can't check the code written into it", &far),
                (None, Some(_), _) | (None, None, Some(None)) => out.push(Effect::Built, &far),
                (None, None, Some(Some(from))) => {
                    out.push(Effect::Downloaded(from.to_string()), &far)
                }
                (None, None, None) => {}
            }
        }
    } else if let Some(heredoc) = input.heredoc {
        heredoc_script(heredoc, &mut far, out);
    } else if input.piped.is_some() || input.file.is_some() {
        out.push(Effect::Built, &far);
    }
}

fn heredoc_script(heredoc: &Heredoc, ctx: &mut Ctx, out: &mut Out) {
    match heredoc.known() {
        Some(body) => {
            ctx.depth += 1;
            read_text(body, ctx, out);
        }
        None => out.unclear("Ophio can't check a script written into it", ctx),
    }
}

/// `scp`-style copies: the destination last, any of them `host:path`.
fn copy_between(files: &[&Word], ctx: &Ctx, out: &mut Out) {
    let Some((dest, sources)) = files.split_last() else {
        return;
    };
    match remote_host(&dest.text) {
        Some(host) => out.push(Effect::Copy { host, to: true }, ctx),
        None => {
            if let Some(t) = locate(dest, ctx) {
                out.push(Effect::Change(t), ctx);
            }
        }
    }
    for source in sources {
        match remote_host(&source.text) {
            Some(host) => out.push(Effect::Copy { host, to: false }, ctx),
            None => read(source, ctx, out),
        }
    }
}

fn rsync_remote(rest: &[Word], ctx: &Ctx, out: &mut Out) -> bool {
    let files = positional(
        rest,
        &[
            "-e",
            "--rsh",
            "--exclude",
            "--include",
            "--filter",
            "-f",
            "--files-from",
            "--port",
            "--log-file",
            "--backup-dir",
            "--suffix",
            "--chmod",
            "--chown",
            "--rsync-path",
            "--password-file",
            "--exclude-from",
            "--include-from",
        ],
    );
    if !files.iter().any(|w| remote_host(&w.text).is_some()) {
        return false;
    }
    copy_between(&files, ctx, out);
    let deletes = rest.iter().any(|w| w.text.starts_with("--del"));
    if deletes && let Some(dest) = files.last() {
        match remote_host(&dest.text) {
            Some(host) => out.push(
                Effect::Delete(Target {
                    shown: format!("files on {host} that aren't in the source"),
                    at: Where::Remote,
                }),
                ctx,
            ),
            None => {
                if let Some(mut t) = locate(dest, ctx) {
                    t.shown = format!("files in {} that aren't in the source", t.shown);
                    out.push(Effect::Delete(t), ctx);
                }
            }
        }
    }
    true
}

/// The host of an `scp`-style `[user@]host:path`, or None for a local path.
fn remote_host(text: &str) -> Option<String> {
    if let Some(rest) = text
        .strip_prefix("scp://")
        .or(text.strip_prefix("rsync://"))
        .or(text.strip_prefix("sftp://"))
    {
        return Some(host_of(&format!("x://{rest}")));
    }
    let (before, _) = text.split_once(':')?;
    (!before.is_empty() && !before.contains('/') && !text.starts_with(['/', '.', '~']))
        .then(|| host_name(before))
}

/// `user@host` or `host:port` as the host.
fn host_name(text: &str) -> String {
    if text.contains("://") {
        return host_of(text);
    }
    let host = text.rsplit_once('@').map_or(text, |(_, h)| h);
    let host = host.split(':').next().unwrap_or(host);
    clip(host)
}

fn remote_name(text: &str) -> String {
    if text.contains("://") {
        host_of(text)
    } else if let Some(host) = remote_host(text) {
        host
    } else {
        clip(text)
    }
}

/// The host a web address reaches.
fn host_of(url: &str) -> String {
    let rest = url.split_once("://").map_or(url, |(_, r)| r);
    let authority = rest.split(['/', '?', '#']).next().unwrap_or_default();
    let host = authority.rsplit_once('@').map_or(authority, |(_, h)| h);
    let host = if host.starts_with('[') {
        host.split_inclusive(']').next().unwrap_or(host)
    } else {
        host.split(':').next().unwrap_or(host)
    };
    if host.is_empty() || host.contains('$') {
        "a web address it works out as it runs".into()
    } else {
        clip(host)
    }
}

fn download(name: &str, rest: &[Word], ctx: &Ctx, out: &mut Out) -> Option<String> {
    // Options that take a value, for curl and wget alike.
    const CURL: &str = "odFHXuAeTwmxbcKrEYyzCDUQ";
    const CURL_LONG: &[&str] = &[
        "--output",
        "--data",
        "--data-raw",
        "--data-binary",
        "--data-urlencode",
        "--data-ascii",
        "--json",
        "--form",
        "--form-string",
        "--header",
        "--request",
        "--user",
        "--user-agent",
        "--referer",
        "--upload-file",
        "--write-out",
        "--max-time",
        "--connect-timeout",
        "--proxy",
        "--cookie",
        "--cookie-jar",
        "--config",
        "--range",
        "--cert",
        "--key",
        "--cacert",
        "--resolve",
        "--url",
        "--retry",
        "--retry-delay",
        "--limit-rate",
        "--interface",
        "--dns-servers",
        "--output-dir",
        "--dump-header",
        "--trace",
        "--trace-ascii",
        "--stderr",
        "--continue-at",
        "--max-filesize",
        "--proto",
        "--proto-redir",
        "--proxy-user",
        "--output-document",
        "--output-file",
        "--append-output",
        "--directory-prefix",
        "--post-data",
        "--post-file",
        "--body-data",
        "--body-file",
        "--method",
        "--execute",
        "--input-file",
        "--tries",
        "--timeout",
        "--wait",
        "--password",
        "--quota",
        "--level",
        "--accept",
        "--reject",
        "--domains",
        "--exclude-domains",
        "--auth",
        "--session",
        "--dir",
        "--out",
        "--max-redirects",
    ];
    const WGET: &str = "OoaPUeitTwQlARDIX";
    let short = if name == "wget" { WGET } else { CURL };
    let mut urls: Vec<&Word> = Vec::new();
    let mut output: Option<Word> = None;
    let mut written: Vec<Word> = Vec::new();
    let mut named_output = false;
    let mut into_dir: Option<Word> = None;
    // Files whose contents it sends.
    let mut uploads: Vec<Word> = Vec::new();
    let mut sends = false;
    let mut at = 0;
    while let Some(w) = rest.get(at) {
        let t = w.text.as_str();
        let value = |at: &mut usize, inline: Option<&str>| -> Option<Word> {
            match inline {
                Some(v) if !v.is_empty() => {
                    let mut v2 = w.clone();
                    v2.text = v.to_string();
                    v2.open_at = None;
                    v2.dynamic = w.dynamic && v.contains('$');
                    Some(v2)
                }
                _ => {
                    *at += 1;
                    rest.get(*at).cloned()
                }
            }
        };
        if let Some(long) = t.strip_prefix("--") {
            let (key, inline) = match long.split_once('=') {
                Some((k, v)) => (k, Some(v)),
                None => (long, None),
            };
            let key = format!("--{key}");
            if CURL_LONG.contains(&key.as_str()) {
                let v = value(&mut at, inline);
                match key.as_str() {
                    "--output" | "--output-document" | "--out" => output = v,
                    "--directory-prefix" | "--dir" | "--output-dir" => {
                        into_dir = v.clone();
                        written.extend(v);
                    }
                    "--dump-header" | "--cookie-jar" | "--trace" | "--trace-ascii"
                    | "--output-file" | "--append-output" | "--session" => written.extend(v),
                    "--data" | "--data-raw" | "--data-binary" | "--data-urlencode"
                    | "--data-ascii" | "--json" | "--form" | "--form-string" | "--upload-file"
                    | "--post-data" | "--post-file" | "--body-data" | "--body-file" => {
                        sends = true;
                        if let Some(v) = &v {
                            uploads.extend(sent_file(&key, v));
                        }
                    }
                    "--request" | "--method" => {
                        sends |= v.is_some_and(|v| {
                            !v.text.eq_ignore_ascii_case("GET")
                                && !v.text.eq_ignore_ascii_case("HEAD")
                        })
                    }
                    "--config" | "--execute" => {
                        out.unclear("Ophio can't check the settings file it reads", ctx)
                    }
                    "--url" => {
                        if let Some(u) = rest.get(at).filter(|_| inline.is_none()) {
                            urls.push(u);
                        }
                    }
                    _ => {}
                }
            } else if matches!(
                key.as_str(),
                "--remote-name" | "--remote-name-all" | "--remote-header-name" | "--download"
            ) {
                named_output = true;
            }
        } else if let Some(flags) = t.strip_prefix('-').filter(|f| !f.is_empty()) {
            for (i, c) in flags.char_indices() {
                if short.contains(c) {
                    let inline = &flags[i + c.len_utf8()..];
                    let v = value(&mut at, Some(inline));
                    let method = |v: &Option<Word>| {
                        v.as_ref().is_some_and(|v| {
                            !v.text.eq_ignore_ascii_case("GET")
                                && !v.text.eq_ignore_ascii_case("HEAD")
                        })
                    };
                    match (name == "wget", c) {
                        (true, 'O') | (false, 'o') => output = v,
                        (true, 'o' | 'a') | (false, 'D' | 'c') => written.extend(v),
                        (true, 'i') => {
                            out.unclear("Ophio can't check the addresses it reads from a file", ctx)
                        }
                        (true, 'P') => {
                            into_dir = v.clone();
                            written.extend(v);
                        }
                        (false, 'd' | 'F' | 'T') => {
                            sends = true;
                            let key = match c {
                                'd' => "--data",
                                'F' => "--form",
                                _ => "--upload-file",
                            };
                            if let Some(v) = &v {
                                uploads.extend(sent_file(key, v));
                            }
                        }
                        (false, 'X') => sends |= method(&v),
                        (false, 'K') => {
                            out.unclear("Ophio can't check the settings file it reads", ctx)
                        }
                        _ => {}
                    }
                    break;
                }
                if c == 'O' && name != "wget" {
                    named_output = true;
                }
            }
        } else if matches!(name, "http" | "https" | "xh" | "xhs" | "httpie")
            && urls.is_empty()
            && matches!(
                t.to_ascii_uppercase().as_str(),
                "GET" | "POST" | "PUT" | "PATCH" | "DELETE" | "HEAD" | "OPTIONS"
            )
        {
            sends = !matches!(t.to_ascii_uppercase().as_str(), "GET" | "HEAD" | "OPTIONS");
        } else if matches!(name, "http" | "https" | "xh" | "xhs" | "httpie") && !urls.is_empty() {
            // Request items: `name=value` and `name:=json` send data;
            // `name@file`, `name=@file` and `name:=@file` send a file.
            if t.contains('=') || t.contains("@") {
                sends = true;
            }
            if let Some(sep) = t.find(['=', ':', '@']) {
                let after = &t[sep..];
                let skip = ["@", "=@", ":=@"].iter().find(|p| after.starts_with(**p));
                if let Some(skip) = skip {
                    uploads.extend(part_of(w, sep + skip.len(), t.len()));
                }
            }
        } else {
            urls.push(w);
        }
        at += 1;
    }
    let to_stdout = match name {
        "wget" => output.as_ref().is_some_and(|o| o.text == "-"),
        "aria2c" => false,
        _ => output.as_ref().is_none_or(|o| o.text == "-") && !named_output,
    };
    for file in output.iter().chain(&written) {
        if file.text != "-"
            && let Some(t) = locate(file, ctx)
        {
            out.push(Effect::Change(t), ctx);
        }
    }
    for file in &uploads {
        read(file, ctx, out);
    }
    // Where what it downloads is saved, for whatever runs it later.
    let named = match name {
        "wget" | "aria2c" => output.is_none(),
        _ => named_output && output.is_none(),
    };
    let mut first = None;
    for url in urls {
        let host = if url.dynamic && url.text.starts_with('$') {
            "a web address it works out as it runs".to_string()
        } else {
            host_of(&url.text)
        };
        first.get_or_insert(host.clone());
        if let Some(file) = output.as_ref().filter(|o| o.text != "-") {
            out.fetch(file, &host, ctx);
        } else if named
            && !url.dynamic
            && let Some(file) = url_file(&url.text)
        {
            let file = match &into_dir {
                Some(dir) => {
                    let mut w = dir.clone();
                    w.text = format!("{}/{file}", dir.text.trim_end_matches('/'));
                    w
                }
                None => plain(&file),
            };
            out.fetch(&file, &host, ctx);
        }
        out.push(
            if sends {
                Effect::Send(host)
            } else {
                Effect::Download(host)
            },
            ctx,
        );
    }
    if to_stdout {
        Some(first.unwrap_or_else(|| "the internet".into()))
    } else {
        None
    }
}

/// PowerShell's `Invoke-WebRequest` and `Invoke-RestMethod`: returns the
/// host whose download it passes on, if it does.
fn web_request(rest: &[Word], ctx: &Ctx, out: &mut Out) -> Option<String> {
    const SWITCHES: &[&str] = &[
        "usebasicparsing",
        "usedefaultcredentials",
        "skipcertificatecheck",
        "passthru",
        "disablekeepalive",
        "resume",
        "skipheadervalidation",
        "skiphttperrorcheck",
        "allowunencryptedauthentication",
        "preservehttpmethodonredirect",
        "allowinsecureredirect",
        "noproxy",
    ];
    // Its parameters may be shortened: `-Out` for `-OutFile`.
    let is = |given: &str, full: &str| given.len() >= 2 && full.starts_with(given);
    let mut url: Option<Word> = None;
    let mut output = None;
    let mut sends = false;
    let mut at = 0;
    while let Some(w) = rest.get(at) {
        let Some(param) = w.text.strip_prefix('-') else {
            url.get_or_insert_with(|| w.clone());
            at += 1;
            continue;
        };
        let (param, inline) = match param.split_once(':') {
            Some((p, v)) => (p.to_ascii_lowercase(), Some(v)),
            None => (param.to_ascii_lowercase(), None),
        };
        let value = match inline {
            Some(v) => part_of(w, w.text.len() - v.len(), w.text.len()),
            None if SWITCHES.contains(&param.as_str()) => None,
            None => {
                at += 1;
                rest.get(at).cloned()
            }
        };
        if is(&param, "uri") {
            url = value.or(url);
        } else if is(&param, "outfile") {
            output = value;
        } else if is(&param, "infile") {
            sends = true;
            if let Some(file) = &value {
                read(file, ctx, out);
            }
        } else if is(&param, "body") || is(&param, "form") {
            sends = true;
        } else if is(&param, "method") {
            sends |= value.is_some_and(|v| {
                !v.text.eq_ignore_ascii_case("GET") && !v.text.eq_ignore_ascii_case("HEAD")
            });
        }
        at += 1;
    }
    let host = match url {
        Some(u) if u.dynamic && u.text.starts_with('$') => {
            "a web address it works out as it runs".to_string()
        }
        Some(u) => host_of(&u.text),
        None => "a web address it works out as it runs".to_string(),
    };
    out.push(
        if sends {
            Effect::Send(host.clone())
        } else {
            Effect::Download(host.clone())
        },
        ctx,
    );
    match output {
        Some(file) => {
            if let Some(t) = locate(&file, ctx) {
                out.push(Effect::Change(t), ctx);
            }
            out.fetch(&file, &host, ctx);
            None
        }
        None => Some(host),
    }
}

/// The file a curl or wget option sends, if it names one: `-d @file`,
/// `-F name=@file`, `--data-urlencode name@file`, `-T file`,
/// `--post-file=file`. Never `-`, which is its input.
fn sent_file(key: &str, value: &Word) -> Option<Word> {
    let t = value.text.as_str();
    let (from, to) = match key {
        "--data" | "--data-binary" | "--data-ascii" | "--json" => {
            (t.starts_with('@').then_some(1)?, t.len())
        }
        "--data-urlencode" => {
            let at = t.find(['=', '@'])?;
            (t[at..].starts_with('@').then_some(at + 1)?, t.len())
        }
        "--form" => {
            let eq = t.find('=')?;
            if !t[eq + 1..].starts_with(['@', '<']) {
                return None;
            }
            let end = t[eq + 2..].find(';').map_or(t.len(), |e| eq + 2 + e);
            (eq + 2, end)
        }
        "--upload-file" | "--post-file" | "--body-file" => (0, t.len()),
        _ => return None,
    };
    part_of(value, from, to)
}

/// The part of `word` from byte `from` to `to`, as a word of its own; None
/// when it is empty or `-`.
fn part_of(word: &Word, from: usize, to: usize) -> Option<Word> {
    let text = word.text.get(from..to)?;
    if text.is_empty() || text == "-" || text == "." {
        return None;
    }
    let mut part = word.clone();
    part.text = text.to_string();
    match word.open_at {
        // The first pattern or expansion comes after this part: it holds
        // neither.
        Some(open) if open >= to => {
            part.open_at = None;
            part.glob = false;
        }
        open => part.open_at = open.and_then(|o| o.checked_sub(from)),
    }
    Some(part)
}

/// The name a download is saved under when none is given: the last part of
/// its address.
fn url_file(url: &str) -> Option<String> {
    let rest = url.split_once("://").map_or(url, |(_, r)| r);
    let path = rest.split(['?', '#']).next()?;
    let (_, path) = path.split_once('/')?;
    let name = path.rsplit('/').next()?;
    (!name.is_empty() && name != "." && name != "..").then(|| name.to_string())
}

fn shell(name: &str, rest: &[Word], ctx: &Ctx, out: &mut Out, input: Input) {
    let mut at = 0;
    let mut inline = false;
    // `-s` reads the script from its input; the words after are its
    // arguments.
    let mut from_input = false;
    while let Some(w) = rest.get(at) {
        let t = w.text.as_str();
        if t == "--" {
            at += 1;
            break;
        }
        if t.starts_with("--") {
            at += if matches!(t, "--rcfile" | "--init-file" | "--init-command") {
                2
            } else {
                1
            };
            if t == "--command" {
                inline = true;
            }
            continue;
        }
        if (t.starts_with('-') || t.starts_with('+')) && t.len() > 1 {
            let flags = &t[1..];
            if flags.contains('c') {
                inline = true;
            }
            if t.starts_with('-') && flags.contains('s') {
                from_input = true;
            }
            at += if flags.ends_with('o') || flags.ends_with('O') {
                2
            } else {
                1
            };
            continue;
        }
        break;
    }
    if name == "busybox" {
        // `busybox sh …`: the applet names the program.
        let mut inner = ctx.clone();
        command(rest, &mut inner, out, input);
        return;
    }
    if inline {
        match rest.get(at) {
            Some(code) => inline_shell(code, ctx, out),
            None => out.unclear(UNREADABLE, ctx),
        }
        return;
    }
    if from_input {
        stdin_script(name, ctx, out, input);
        return;
    }
    if let Some(script) = rest.get(at) {
        if script.process {
            match out.process_download.clone() {
                Some(host) => out.push(Effect::Downloaded(host), ctx),
                None => out.push(Effect::Built, ctx),
            }
        } else if script.text == "-" || script.text == "/dev/stdin" {
            stdin_script(name, ctx, out, input);
        } else {
            runs_file(script, true, ctx, out);
        }
        return;
    }
    stdin_script(name, ctx, out, input);
}

/// Runs the file `word` names: a script it reads, or a program it starts.
/// One an earlier step downloaded is said to be that download.
fn runs_file(word: &Word, script: bool, ctx: &Ctx, out: &mut Out) {
    if let Some(host) = out.fetched_from(word, ctx) {
        let effect = if script {
            Effect::Downloaded(host)
        } else {
            Effect::DownloadedProgram(host)
        };
        out.push(effect, ctx);
        return;
    }
    match locate(word, ctx) {
        Some(t) => out.push(Effect::Runs(t), ctx),
        None => out.unclear(WORKED_OUT, ctx),
    }
}

fn stdin_script(name: &str, ctx: &Ctx, out: &mut Out, input: Input) {
    if let Some(heredoc) = input.heredoc {
        let mut inner = ctx.clone();
        heredoc_script(heredoc, &mut inner, out);
        return;
    }
    if let Some(file) = input.file {
        runs_file(file, true, ctx, out);
        return;
    }
    match input.piped {
        Some(Some(host)) => out.push(Effect::Downloaded(host.to_string()), ctx),
        Some(None) => out.push(Effect::Built, ctx),
        None => out.push(runs_program(name, ctx), ctx),
    }
}

fn interpreter(name: &str, rest: &[Word], ctx: &Ctx, out: &mut Out, input: Input) {
    let language = match name {
        n if n.starts_with("python") || n.starts_with("pypy") => "Python",
        "node" | "nodejs" => "JavaScript",
        "perl" => "Perl",
        "ruby" | "irb" => "Ruby",
        "php" => "PHP",
        "lua" | "luajit" => "Lua",
        "Rscript" => "R",
        "osascript" => "AppleScript",
        "pwsh" | "powershell" => "PowerShell",
        "swift" => "Swift",
        "julia" => "Julia",
        _ => "program",
    };
    let code = format!("Ophio can't check the {language} code written into it");
    let inline_flags: &[&str] = match name {
        n if n.starts_with("python") || n.starts_with("pypy") => &["-c"],
        "node" | "nodejs" => &["-e", "-p", "--eval", "--print", "-pe"],
        "php" => &["-r"],
        "pwsh" | "powershell" => &[
            "-c",
            "-Command",
            "-command",
            "-EncodedCommand",
            "-e",
            "-enc",
        ],
        "osascript" | "Rscript" | "lua" | "luajit" | "julia" | "tclsh" => &["-e"],
        _ => &[],
    };
    let mut at = 0;
    let mut in_place = false;
    while let Some(w) = rest.get(at) {
        let t = w.text.as_str();
        if t == "--" {
            at += 1;
            break;
        }
        if !t.starts_with('-') || t == "-" {
            break;
        }
        let perlish = matches!(name, "perl" | "ruby");
        if inline_flags.contains(&t)
            || (perlish && !t.starts_with("--") && (t.contains('e') || t.contains('E')))
            || (language == "Python" && !t.starts_with("--") && t.ends_with('c'))
        {
            if perlish && t.contains('i') {
                in_place = true;
            }
            out.unclear(code.clone(), ctx);
            if in_place {
                for w in positional(&rest[at + 1..], &["-e", "-E", "-I", "-M"])
                    .into_iter()
                    .skip(1)
                {
                    if let Some(t) = locate(w, ctx) {
                        out.push(Effect::Change(t), ctx);
                    }
                }
            }
            return;
        }
        if perlish && !t.starts_with("--") && t.contains('i') {
            in_place = true;
        }
        if language == "Python" && t == "-m" {
            let module = rest
                .get(at + 1)
                .map(|w| w.text.as_str())
                .unwrap_or_default();
            let then = rest
                .get(at + 2)
                .map(|w| w.text.as_str())
                .unwrap_or_default();
            if module == "pip" && matches!(then, "install" | "download") {
                out.push(Effect::Install, ctx);
            } else if module == "pip" && then == "uninstall" {
                out.push(Effect::Uninstall, ctx);
            } else if module == "http.server" {
                out.push(runs_program("a web server", ctx), ctx);
            } else {
                out.push(runs_program(&format!("{name} -m {module}"), ctx), ctx);
            }
            uses(&rest[(at + 2).min(rest.len())..], ctx, out);
            return;
        }
        at += if matches!(
            t,
            "-W" | "-X" | "-r" | "--require" | "-I" | "-M" | "--import" | "--loader"
        ) {
            2
        } else {
            1
        };
    }
    match rest.get(at) {
        Some(script) if script.text != "-" => {
            if script.process {
                out.push(Effect::Built, ctx);
            } else {
                runs_file(script, true, ctx, out);
            }
            uses(&rest[at + 1..], ctx, out);
        }
        _ => {
            if input.heredoc.is_some() {
                out.unclear(code.clone(), ctx);
            } else if let Some(file) = input.file {
                runs_file(file, true, ctx, out);
            } else {
                match input.piped {
                    Some(Some(host)) => out.push(Effect::Downloaded(host.to_string()), ctx),
                    Some(None) => out.unclear(code.clone(), ctx),
                    None => out.push(runs_program(name, ctx), ctx),
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Paths

fn plain(text: &str) -> Word {
    Word {
        text: text.to_string(),
        ..Word::default()
    }
}

fn is_null_device(text: &str) -> bool {
    matches!(
        text,
        "/dev/null"
            | "/dev/stdout"
            | "/dev/stderr"
            | "/dev/tty"
            | "/dev/zero"
            | "/dev/stdin"
            | "/dev/random"
            | "/dev/urandom"
    ) || text.starts_with("/dev/fd/")
}

fn normal(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for part in path.components() {
        match part {
            Component::ParentDir => {
                if !out.pop() {
                    out.push("/");
                }
            }
            Component::CurDir => {}
            other => out.push(other),
        }
    }
    out
}

fn home_path(text: &str, home: Option<&Path>) -> Option<Option<PathBuf>> {
    let rest = if text == "~" || text == "$HOME" || text == "${HOME}" {
        ""
    } else {
        text.strip_prefix("~/")
            .or(text.strip_prefix("$HOME/"))
            .or(text.strip_prefix("${HOME}/"))?
    };
    Some(home.map(|h| h.join(rest)))
}

fn resolve_dir(text: &str, ctx: &Ctx) -> Option<PathBuf> {
    if text.contains('$') && home_path(text, None).is_none() {
        return None;
    }
    let path = match home_path(text, ctx.place.home.as_deref()) {
        Some(home) => home?,
        None if text.starts_with('/') || Path::new(text).is_absolute() => PathBuf::from(text),
        None => ctx.cwd.as_ref()?.join(text),
    };
    Some(normal(&path))
}

/// The full path `word` names on this computer, when the command shows it.
fn resolve(word: &Word, ctx: &Ctx) -> Option<PathBuf> {
    let text = word.text.as_str();
    let home_only = home_path(text, None).is_some() && text.matches('$').count() <= 1;
    if ctx.on.is_some() || word.glob || (word.dynamic && !home_only) || text == "-" {
        return None;
    }
    let path = match home_path(text, ctx.place.home.as_deref()) {
        Some(home) => home?,
        None if text.starts_with('/') || Path::new(text).is_absolute() => PathBuf::from(text),
        None => ctx.cwd.as_ref()?.join(text),
    };
    Some(normal(&path))
}

/// Where `word`, a path, leads; None for a device that discards or
/// passes on what is written to it.
fn locate(word: &Word, ctx: &Ctx) -> Option<Target> {
    let mut found = locate_as_written(word, ctx)?;
    if let Some(shown) = &word.shown {
        found.shown = shown.clone();
    }
    Some(found)
}

fn locate_as_written(word: &Word, ctx: &Ctx) -> Option<Target> {
    let text = word.text.as_str();
    if is_null_device(text) {
        return None;
    }
    if ctx.on.is_some() {
        return Some(Target {
            shown: clip(text),
            at: Where::Remote,
        });
    }
    let unknown = || Target {
        shown: clip(text),
        at: Where::Unknown,
    };
    // A pattern stays within the folder before it; a variable could hold
    // anything, `..` included.
    let home_only = (text.starts_with("$HOME") || text.starts_with("${HOME}"))
        && text.matches('$').count() == 1
        && word.ticks.is_empty()
        && word.subs.is_empty();
    if word.dynamic && !home_only {
        return Some(unknown());
    }
    // A pattern stays in its folder unless it climbs out with `..` or a
    // brace offers a path of its own: `{a,~/x}`, `{a,/etc}`.
    if word.glob
        && let Some(pattern) = word.open_at.and_then(|open| text.get(open..))
        && (pattern.split(['/', ',', '{', '}']).any(|part| part == "..")
            || ["{/", ",/", "{~", ",~", "{$", ",$"]
                .iter()
                .any(|p| pattern.contains(p)))
    {
        return Some(unknown());
    }
    let fixed = match word.open_at {
        // A position that isn't in the text can't say where the pattern
        // starts.
        Some(open) if word.glob && !home_only => match text.get(..open) {
            Some(before) => match before.rfind('/') {
                Some(slash) => &text[..=slash],
                None => ".",
            },
            None => return Some(unknown()),
        },
        _ => text,
    };
    let path = match home_path(fixed, ctx.place.home.as_deref()) {
        Some(Some(home)) => home,
        Some(None) => return Some(unknown()),
        None if fixed.starts_with('/') || Path::new(fixed).is_absolute() => PathBuf::from(fixed),
        None => match &ctx.cwd {
            Some(cwd) => cwd.join(fixed),
            None => return Some(unknown()),
        },
    };
    let path = normal(&path);
    let Some(project) = &ctx.place.project else {
        return Some(unknown());
    };
    let project = normal(project);
    let glob_shown = || clip(text);
    if let Ok(inside) = path.strip_prefix(&project) {
        let rel = inside.to_string_lossy().to_string();
        if word.glob {
            let shown = match fixed.rfind('/') {
                Some(_) => text.to_string(),
                None => text.to_string(),
            };
            return Some(Target {
                shown: clip(&shown),
                at: Where::Inside,
            });
        }
        if rel.is_empty() {
            return Some(Target {
                shown: project_label(ctx.place),
                at: Where::Root,
            });
        }
        let shown = if rel.chars().count() <= 32 {
            rel
        } else {
            path.file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or(rel)
        };
        return Some(Target {
            shown: clip(&shown),
            at: Where::Inside,
        });
    }
    let temp = [
        "/tmp",
        "/var/tmp",
        "/private/tmp",
        "/private/var/folders",
        "/var/folders",
        "/dev/shm",
    ]
    .iter()
    .any(|t| path.starts_with(t));
    let shown = if word.glob {
        glob_shown()
    } else {
        far_shown(&path, ctx.place.home.as_deref())
    };
    Some(Target {
        shown,
        at: if temp { Where::Temp } else { Where::Outside },
    })
}

/// A path outside the project, from the home folder where it is in it.
fn far_shown(path: &Path, home: Option<&Path>) -> String {
    let text = match home.and_then(|h| path.strip_prefix(h).ok()) {
        Some(rel) if rel.as_os_str().is_empty() => "your home folder".to_string(),
        Some(rel) => format!("~/{}", rel.to_string_lossy()),
        None => path.to_string_lossy().to_string(),
    };
    if text.chars().count() <= NAME_WIDTH {
        return text;
    }
    let parts: Vec<_> = path
        .iter()
        .map(|p| p.to_string_lossy().to_string())
        .collect();
    let tail = parts[parts.len().saturating_sub(2)..].join("/");
    clip(&format!("…/{tail}"))
}

fn clip(text: &str) -> String {
    let text = crate::tool_approval::visible(text);
    let text: String = text
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect();
    if text.chars().count() <= NAME_WIDTH {
        text
    } else {
        text.chars().take(NAME_WIDTH - 1).chain(['…']).collect()
    }
}

// ---------------------------------------------------------------------------
// Saying it

struct Line {
    order: u8,
    risky: bool,
    /// Lines with the same lead and tail list their names together.
    lead: String,
    name: Option<String>,
    tail: String,
}

fn list(names: &[String]) -> String {
    match names {
        [] => String::new(),
        [one] => one.clone(),
        [a, b] => format!("{a} and {b}"),
        _ if names.len() <= MAX_NAMES => {
            let (last, rest) = names.split_last().unwrap_or((&names[0], &[]));
            format!("{} and {last}", rest.join(", "))
        }
        _ => format!("{}, {} and {} more", names[0], names[1], names.len() - 2),
    }
}

fn line(effect: &Effect, on: Option<&str>, project: &str) -> Option<Line> {
    let away = on.map(|h| format!(" on {h}")).unwrap_or_default();
    let fixed = |order: u8, risky: bool, text: &str| Line {
        order,
        risky,
        lead: format!("{text}{away}"),
        name: None,
        tail: String::new(),
    };
    let named = |order: u8, risky: bool, lead: &str, name: &str, tail: String| Line {
        order,
        risky,
        lead: lead.to_string(),
        name: Some(name.to_string()),
        tail,
    };
    // The tail a path's place adds, and whether that place is risky.
    let place = |t: &Target| -> (String, bool) {
        match t.at {
            Where::Outside => (format!(" outside {project}"), true),
            Where::Unknown => (format!(", which may be outside {project}"), true),
            Where::Remote => (away.clone(), false),
            _ => (String::new(), false),
        }
    };
    Some(match effect {
        Effect::Unclear(text) => fixed(0, true, text),
        Effect::Sudo => fixed(1, true, "uses administrator rights"),
        Effect::Downloaded(host) => named(
            2,
            true,
            "runs a script downloaded from ",
            host,
            away.clone(),
        ),
        Effect::DownloadedProgram(host) => named(
            2,
            true,
            "runs a program downloaded from ",
            host,
            away.clone(),
        ),
        Effect::Built => Line {
            order: 3,
            risky: true,
            lead: match on {
                Some(host) => format!("runs a script on {host} that Ophio can't see"),
                None => "runs a script Ophio can't see".into(),
            },
            name: None,
            tail: String::new(),
        },
        Effect::Install => fixed(4, true, "installs software"),
        Effect::Uninstall => fixed(5, true, "removes software"),
        Effect::Delete(t) => {
            let (tail, _) = place(t);
            let shown = if t.at == Where::Root && t.shown == project {
                format!("all of {project}")
            } else {
                t.shown.clone()
            };
            named(6, true, "deletes ", &shown, tail)
        }
        Effect::DeletesWith(what) => named(7, true, "deletes with ", what, away.clone()),
        Effect::Discard => fixed(8, true, "throws away uncommitted changes"),
        Effect::History => fixed(9, true, "rewrites git history"),
        Effect::DeleteBranch => fixed(10, true, "deletes a git branch or tag"),
        Effect::Push { remote, force } => named(
            11,
            true,
            if *force {
                "force-pushes to "
            } else {
                "pushes to "
            },
            remote,
            away.clone(),
        ),
        Effect::Publish(what) => named(12, true, "publishes with ", what, away.clone()),
        Effect::PostsGitHub => fixed(13, true, "posts on GitHub as you"),
        Effect::Power => fixed(14, true, "restarts or shuts down the computer"),
        Effect::Disk => fixed(15, true, "writes to a disk directly"),
        Effect::Schedule => fixed(16, true, "changes scheduled jobs"),
        Effect::GlobalSettings(what) => fixed(17, true, &format!("changes {what}")),
        Effect::WriteAccess(t) => {
            let (tail, risky) = place(t);
            let shown = if t.at == Where::Root {
                project
            } else {
                &t.shown
            };
            named(18, risky, "can change anything in ", shown, tail)
        }
        Effect::Change(t) => {
            let (tail, risky) = place(t);
            let shown = if t.at == Where::Root && t.shown == project {
                format!("files in {project}")
            } else {
                t.shown.clone()
            };
            named(19, risky, "changes ", &shown, tail)
        }
        Effect::Unpack(t) => {
            let (tail, risky) = place(t);
            named(19, risky, "unpacks files into ", &t.shown, tail)
        }
        Effect::Read(t) => {
            let (tail, risky) = place(t);
            if !risky {
                return None;
            }
            named(20, risky, "reads ", &t.shown, tail)
        }
        Effect::Use(t) => {
            let (tail, risky) = place(t);
            if !risky {
                return None;
            }
            named(21, risky, "uses ", &t.shown, tail)
        }
        Effect::Move(t) => {
            let (tail, risky) = place(t);
            named(22, risky, "moves ", &t.shown, tail)
        }
        Effect::Commit(t) => match t.at {
            Where::Outside | Where::Unknown => {
                named(23, true, "makes a git commit in ", &t.shown, place(t).0)
            }
            _ => fixed(23, false, "makes a git commit"),
        },
        Effect::Branch => fixed(24, false, "switches git branch"),
        Effect::Pull(remote) => named(25, false, "pulls changes from ", remote, away.clone()),
        Effect::Copy { host, to: true } => {
            named(26, false, "copies files to ", host, String::new())
        }
        Effect::Copy { host, to: false } => {
            named(26, false, "copies files from ", host, String::new())
        }
        Effect::Connect(host) => named(27, false, "connects to ", host, away.clone()),
        Effect::Download(host) => named(28, false, "downloads from ", host, away.clone()),
        Effect::Send(host) => named(29, false, "sends data to ", host, away.clone()),
        Effect::Fetches(package) => {
            named(30, false, "may download and run ", package, away.clone())
        }
        Effect::Runs(t) => {
            let (tail, risky) = place(t);
            named(31, risky, "runs ", &t.shown, tail)
        }
        Effect::Stops => fixed(32, false, "stops running programs"),
    })
}

fn project_label(place: &Place) -> String {
    if place.project_name.trim().is_empty() {
        "the project".to_string()
    } else {
        clip(&place.project_name)
    }
}

fn render(out: Out, place: &Place) -> Vec<ApprovalFact> {
    let project = project_label(place);
    // A host something more particular already names isn't said twice.
    let named_hosts: Vec<String> = out
        .effects
        .iter()
        .filter_map(|(e, _)| match e {
            Effect::Copy { host, .. }
            | Effect::Download(host)
            | Effect::Send(host)
            | Effect::Pull(host) => Some(host.clone()),
            Effect::Push { remote, .. } => Some(remote.clone()),
            _ => None,
        })
        .collect();
    let ran: Vec<&String> = out
        .effects
        .iter()
        .filter_map(|(e, _)| match e {
            Effect::Downloaded(host) | Effect::DownloadedProgram(host) => Some(host),
            _ => None,
        })
        .collect();
    let mut lines: Vec<(Line, Vec<String>)> = Vec::new();
    for (effect, on) in &out.effects {
        if let Effect::Connect(host) = effect
            && on.is_none()
            && named_hosts.contains(host)
        {
            continue;
        }
        if let Effect::Download(host) = effect
            && ran.contains(&host)
        {
            continue;
        }
        // A file it runs isn't also said to be read.
        if let Effect::Read(t) = effect
            && out
                .effects
                .iter()
                .any(|(e, o)| *e == Effect::Runs(t.clone()) && o == on)
        {
            continue;
        }
        let Some(l) = line(effect, on.as_deref(), &project) else {
            continue;
        };
        match lines.iter_mut().find(|(o, _)| {
            o.order == l.order && o.risky == l.risky && o.lead == l.lead && o.tail == l.tail
        }) {
            Some((_, names)) => {
                if let Some(name) = l.name
                    && !names.contains(&name)
                {
                    names.push(name);
                }
            }
            None => {
                let names = l.name.iter().cloned().collect();
                lines.push((l, names));
            }
        }
    }
    let mut facts: Vec<(u8, bool, String)> = lines
        .into_iter()
        .map(|(l, names)| {
            (
                l.order,
                l.risky,
                format!("{}{}{}", l.lead, list(&names), l.tail),
            )
        })
        .collect();
    if facts.is_empty() && !out.programs.is_empty() {
        facts.push((
            31,
            false,
            format!(
                "runs {}",
                list(&out.programs.iter().map(|p| clip(p)).collect::<Vec<_>>())
            ),
        ));
    }
    facts.sort_by_key(|(order, risky, _)| (!risky, *order));
    facts.dedup_by(|a, b| a.2 == b.2);
    let mut shown: Vec<ApprovalFact> = Vec::new();
    let cut = facts.len() > MAX_FACTS;
    let keep = if cut { MAX_FACTS - 1 } else { facts.len() };
    for (_, risky, text) in facts.iter().take(keep) {
        shown.push(ApprovalFact {
            text: text.clone(),
            risky: *risky,
        });
    }
    if cut {
        let risky = facts[keep..].iter().any(|(_, r, _)| *r);
        shown.push(ApprovalFact {
            text: "does more; read the command".into(),
            risky,
        });
    }
    shown
}

#[cfg(test)]
#[path = "request_facts_tests.rs"]
mod tests;
