// SPDX-License-Identifier: Apache-2.0
//! The full-screen setup, drawn on a test terminal and answered with
//! scripted keys and clicks.
use super::*;
use crate::setup::{
    flow::Spot,
    tui::{
        Events, Got, Tui,
        draw::{self, Drawn, View},
        linkable,
        look::{Depth, Look},
    },
};
use crossterm::event::{
    Event, KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
};
use ratatui::{
    Terminal,
    backend::TestBackend,
    buffer::Buffer,
    style::{Color, Modifier},
};
use unicode_width::UnicodeWidthStr;

const TRUE: Look = Look { depth: Depth::True };
/// The sizes screens are kept at: the least the full screen takes, a
/// roomier one, and one that gets the compact layout.
const SIZES: [(u16, u16); 3] = [(80, 24), (120, 40), (60, 20)];

/// Keys and clicks in turn; once they run out, nothing comes before each
/// wait passes.
struct Keys {
    events: VecDeque<Got>,
    idle: usize,
}

impl Keys {
    fn of(events: impl IntoIterator<Item = Got>) -> Self {
        Self {
            events: events.into_iter().collect(),
            idle: 0,
        }
    }
}

impl Events for Keys {
    async fn next(&mut self, wait: Duration) -> Got {
        if let Some(got) = self.events.pop_front() {
            return got;
        }
        self.idle += 1;
        assert!(self.idle < 40, "waited for keys that never came");
        tokio::time::sleep(wait).await;
        Got::Nothing
    }
}

fn key(code: KeyCode) -> Got {
    Got::Event(Event::Key(KeyEvent::new(code, KeyModifiers::NONE)))
}
fn ctrl(c: char) -> Got {
    Got::Event(Event::Key(KeyEvent::new(
        KeyCode::Char(c),
        KeyModifiers::CONTROL,
    )))
}
fn typed(text: &str) -> Vec<Got> {
    text.chars().map(|c| key(KeyCode::Char(c))).collect()
}
fn mouse(kind: MouseEventKind, column: u16, row: u16) -> Got {
    Got::Event(Event::Mouse(MouseEvent {
        kind,
        column,
        row,
        modifiers: KeyModifiers::NONE,
    }))
}

fn tui(
    events: impl IntoIterator<Item = Got>,
    (width, height): (u16, u16),
) -> Tui<TestBackend, Keys> {
    let terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
    Tui::new(terminal, Keys::of(events), TRUE, true)
}

fn render(
    screen: &Screen,
    view: &View,
    look: Look,
    (width, height): (u16, u16),
) -> (Buffer, Drawn) {
    let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
    let mut drawn = Drawn::default();
    terminal
        .draw(|frame| drawn = draw::draw(frame, screen, view, look))
        .unwrap();
    (terminal.backend().buffer().clone(), drawn)
}

/// The text of `buffer`, each row's trailing spaces trimmed.
fn shown(buffer: &Buffer) -> String {
    let mut out = String::new();
    for y in 0..buffer.area.height {
        let mut line = String::new();
        let mut hidden = 0;
        for x in 0..buffer.area.width {
            if hidden > 0 {
                hidden -= 1;
                continue;
            }
            let symbol = buffer[(x, y)].symbol();
            hidden = symbol.width().saturating_sub(1);
            line.push_str(symbol);
        }
        out.push_str(line.trim_end());
        out.push('\n');
    }
    out
}

fn map(here: Step) -> Vec<(Step, Spot)> {
    Step::NUMBERED
        .iter()
        .map(|&step| {
            let spot = match step.cmp(&here) {
                std::cmp::Ordering::Less => Spot::Done,
                std::cmp::Ordering::Equal => Spot::Here,
                std::cmp::Ordering::Greater => Spot::Ahead,
            };
            (step, spot)
        })
        .collect()
}

fn choose() -> Screen {
    Screen {
        map: map(Step::Reach),
        ..plain_screen(
            Step::Reach,
            vec![Block::Text("Your phone reaches this computer.".into())],
            Ask::Choose(vec![
                Choice::new('n', "next", "Continue").recommended(),
                Choice::new('w', "wifi", "Allow Wi‑Fi").detail("Only on this network."),
                Choice::new('b', "back", "Back"),
            ]),
        )
    }
}

fn ticks() -> Screen {
    let tick = |id: &'static str, label: &str, on: bool| Tick {
        id,
        label: label.into(),
        detail: Some(format!("What {label} lets it do.")),
        on,
    };
    Screen {
        map: map(Step::Options),
        ..plain_screen(
            Step::Options,
            vec![Block::Text("Tick any you want to set up now.".into())],
            Ask::Ticks {
                ticks: vec![
                    tick("voice", "Voice", false),
                    tick("voice_control", "Voice control", true),
                    tick("projects", "Projects", false),
                ],
                choices: vec![Choice::new('l', "later", "Later")],
            },
        )
    }
}

fn folder() -> Screen {
    Screen {
        map: map(Step::Options),
        ..plain_screen(
            Step::Options,
            vec![Block::Text("Add the folders your projects are in.".into())],
            Ask::Text {
                prompt: "Folder (Enter alone when done)".into(),
            },
        )
    }
}

/// Every different screen a few setups show, in order.
async fn screens() -> Vec<Screen> {
    let mut screens: Vec<Screen> = Vec::new();
    let mut keep = |ui: Scripted| {
        for screen in ui.screens {
            if !screens.contains(&screen) {
                screens.push(screen);
            }
        }
    };

    let mut host = Fake {
        service: Service {
            running: false,
            at_login: false,
        },
        waiting: Script::of([None, None, Some(prompt(true))]),
        ..Fake::default()
    };
    let answers = vec![
        c("start"),
        c("login"),
        c("next"),
        c("next"),
        c("next"),
        c("have"),
        c("on"),
        c("pair"),
        c("choose"),
        Answer::Ticks(vec!["view"]),
        c("next"),
        Answer::Ticks(vec!["projects"]),
        c("type"),
        text("/work/app"),
        text("/missing"),
        text(""),
        c("next"),
        c("finish"),
    ];
    let (_, ui) = go(&mut host, Start::First(Progress::default()), answers).await;
    keep(ui);

    let claude = HarnessKind::ClaudeCode;
    let mut host = Fake {
        agents: Script::of([
            vec![agent(claude, None), agent(HarnessKind::Codex, None)],
            vec![agent(claude, Some(signed_out()))],
            vec![agent(claude, Some(signed_in()))],
        ]),
        stages: Script::of([
            None,
            Some(SignInStage::Code {
                code: "ABCD-1234".into(),
                url_host: "claude.ai".into(),
            }),
            Some(SignInStage::Done {
                identity: Some("me@example.com".into()),
            }),
        ]),
        ..Fake::default()
    };
    let answers = vec![
        c("start"),
        c("install-claude"),
        c("run"),
        c("next"),
        c("sign-in-claude"),
        c("next"),
        Answer::Quit,
    ];
    let (_, ui) = go(&mut host, at(Step::Agent), answers).await;
    keep(ui);

    let mut host = Fake {
        views: Script::of([
            TaskView {
                status: Some("Checking for a package manager".into()),
                said: Some(
                    "whisper.cpp is in your package manager, so I'll install it from there.".into(),
                ),
                doing: vec![
                    (Mark::Ok, "Read the voice guide".into()),
                    (Mark::Wait, "pacman -Si whisper.cpp".into()),
                ],
                ..asking("sudo pacman -S --needed whisper.cpp")
            },
            question(&["~/work/app", "~/work/site", "~/notes"], true),
            view(TaskState::Completed),
        ]),
        ..Fake::default()
    };
    let answers = vec![
        c("start"),
        Answer::Ticks(vec!["voice"]),
        c("agent"),
        c("start"),
        c("allow"),
        Answer::Ticks(vec!["1"]),
        c("leave"),
        Answer::Quit,
    ];
    let (_, ui) = go(&mut host, with_agent(Step::Options), answers).await;
    keep(ui);
    screens
}

fn snapshot(name: &str, actual: &str) {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("src/setup/tests/screens")
        .join(name);
    if std::env::var_os("UPDATE_SNAPSHOTS").is_some() {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, actual).unwrap();
        return;
    }
    let expected = std::fs::read_to_string(&path)
        .unwrap_or_else(|_| panic!("no {name}; run with UPDATE_SNAPSHOTS=1 to make it"));
    for (n, (expected, actual)) in expected.lines().zip(actual.lines()).enumerate() {
        assert_eq!(
            actual,
            expected,
            "{name} differs at line {}; run with UPDATE_SNAPSHOTS=1 if it should",
            n + 1
        );
    }
    assert_eq!(expected.lines().count(), actual.lines().count(), "{name}");
}

#[tokio::test]
async fn every_screen_looks_as_it_did() {
    let screens = screens().await;
    assert!(screens.len() > 20, "only {} screens", screens.len());
    for size in SIZES {
        let mut all = String::new();
        for (n, screen) in screens.iter().enumerate() {
            let (buffer, _) = render(screen, &View::new(screen), TRUE, size);
            all.push_str(&format!("── {} · {} ──\n", n + 1, screen.title));
            all.push_str(&shown(&buffer));
        }
        snapshot(&format!("{}x{}.txt", size.0, size.1), &all);
    }
}

#[tokio::test]
async fn every_screen_fits_and_shows_what_it_asks() {
    for screen in screens().await {
        for size in SIZES {
            let (buffer, drawn) = render(&screen, &View::new(&screen), TRUE, size);
            let text = shown(&buffer);
            for choice in screen.choices() {
                assert!(
                    text.contains(&choice.label),
                    "{} at {size:?} hides {}:\n{text}",
                    screen.title,
                    choice.label
                );
            }
            if let Ask::Ticks { ticks, .. } = &screen.ask {
                assert!(text.contains("Go on with these"));
                assert!(text.contains(&ticks[0].label), "{text}");
            }
            if let Ask::Text { prompt } = &screen.ask {
                assert!(text.contains(prompt.as_str()));
                assert!(drawn.cursor.is_some());
            }
            // What can be clicked is all on the screen.
            for (area, _) in &drawn.hits {
                assert!(area.right() <= size.0 && area.bottom() <= size.1);
            }
        }
    }
}

#[tokio::test]
async fn sixteen_colours_are_the_terminals_own() {
    let few = Look { depth: Depth::Few };
    for screen in screens().await {
        let (buffer, _) = render(&screen, &View::new(&screen), few, (80, 24));
        for cell in &buffer.content {
            for color in [cell.fg, cell.bg] {
                assert!(
                    !matches!(color, Color::Rgb(..) | Color::Indexed(_)),
                    "{} uses {color:?}",
                    screen.title
                );
            }
        }
    }
    let (buffer, _) = render(&choose(), &View::new(&choose()), few, (80, 24));
    assert!(
        buffer
            .content
            .iter()
            .any(|cell| cell.modifier.contains(Modifier::REVERSED))
    );
}

#[tokio::test]
async fn without_colour_shapes_and_words_say_it_all() {
    let none = Look { depth: Depth::None };
    let screens = screens().await;
    for screen in &screens {
        let (buffer, _) = render(screen, &View::new(screen), none, (120, 40));
        for cell in &buffer.content {
            assert_eq!((cell.fg, cell.bg), (Color::Reset, Color::Reset));
        }
    }
    let (buffer, _) = render(&ticks(), &View::new(&ticks()), none, (80, 24));
    let text = shown(&buffer);
    assert!(text.contains("[x] 2 Voice control"), "{text}");
    assert!(text.contains("[ ] 1 Voice"));
    assert!(
        buffer
            .content
            .iter()
            .any(|cell| cell.modifier.contains(Modifier::REVERSED))
    );
    // The QR code is drawn in blocks, dark on the terminal's own background.
    let pairing = screens
        .iter()
        .find(|screen| {
            screen
                .blocks
                .iter()
                .any(|block| matches!(block, Block::Qr { .. }))
        })
        .unwrap();
    let (buffer, _) = render(pairing, &View::new(pairing), none, (120, 40));
    assert!(shown(&buffer).contains('█'));
}

#[test]
fn a_terminal_too_small_says_so() {
    let (buffer, drawn) = render(&choose(), &View::new(&choose()), TRUE, (39, 11));
    assert!(shown(&buffer).contains("Make this window bigger"));
    assert!(drawn.hits.is_empty());
}

#[tokio::test]
async fn a_window_too_small_to_show_what_is_asked_takes_no_answer() {
    let wait = Some(Duration::from_millis(30));
    let mut ui = tui([key(KeyCode::Enter), typed("w").remove(0)], (39, 11));
    assert_eq!(ui.ask(&choose(), wait).await, Answer::Waited);
    // Made big enough, it is answered again.
    ui.backend_mut().resize(80, 24);
    let events = &mut ui.events().events;
    events.push_back(Got::Event(Event::Resize(80, 24)));
    events.push_back(key(KeyCode::Enter));
    assert_eq!(ui.ask(&choose(), None).await, c("next"));
    // Shrunk while it is asked, a paste and Enter do nothing either.
    let mut ui = tui([], (80, 24));
    assert_eq!(ui.ask(&folder(), wait).await, Answer::Waited);
    ui.backend_mut().resize(39, 11);
    let events = &mut ui.events().events;
    events.push_back(Got::Event(Event::Resize(39, 11)));
    events.push_back(Got::Event(Event::Paste("/work".into())));
    events.push_back(key(KeyCode::Enter));
    assert_eq!(ui.ask(&folder(), wait).await, Answer::Waited);
    // It can still be left.
    let mut ui = tui([ctrl('c')], (39, 11));
    assert_eq!(ui.ask(&choose(), None).await, Answer::Quit);
}

#[test]
fn a_new_step_comes_in_from_nothing() {
    let screen = choose();
    let mut view = View::new(&screen);
    view.entered = 0;
    let (buffer, _) = render(&screen, &view, TRUE, (80, 24));
    let text = shown(&buffer);
    assert!(!text.contains("Your phone reaches"));
    // What it asks shows at once.
    assert!(text.contains("Continue"));
    view.entered = 1000;
    let (buffer, _) = render(&screen, &view, TRUE, (80, 24));
    assert!(shown(&buffer).contains("Your phone reaches"));
}

#[test]
fn links_are_kept_whole_for_clicking() {
    let url = "https://example.com/a/very/long/address/that/has/to/be/broken/over/rows/to/fit";
    let screen = Screen {
        map: map(Step::Phone),
        ..plain_screen(
            Step::Phone,
            vec![Block::Link {
                label: "Or open".into(),
                url: url.into(),
            }],
            Ask::Choose(vec![Choice::new('n', "next", "Continue")]),
        )
    };
    let (_, drawn) = render(&screen, &View::new(&screen), TRUE, (80, 24));
    assert!(drawn.links.len() > 1);
    let whole: String = drawn.links.iter().map(|spot| spot.text.as_str()).collect();
    assert_eq!(whole, url);
    assert!(drawn.links.iter().all(|spot| spot.url == url));
    assert!(linkable(url));
    assert!(linkable("ophio://pair?x=1"));
    assert!(!linkable("file:///etc/passwd"));
    assert!(!linkable("https://example.com/\x1b]8;;x"));
}

#[tokio::test]
async fn keys_move_and_choose() {
    let size = (80, 24);
    let mut ui = tui([key(KeyCode::Enter)], size);
    assert_eq!(ui.ask(&choose(), None).await, c("next"));
    let mut ui = tui([key(KeyCode::Down), key(KeyCode::Enter)], size);
    assert_eq!(ui.ask(&choose(), None).await, c("wifi"));
    // Up stops at the top; Tab goes round.
    let mut ui = tui(
        [key(KeyCode::Up), key(KeyCode::BackTab), key(KeyCode::Enter)],
        size,
    );
    assert_eq!(ui.ask(&choose(), None).await, c("back"));
    let mut ui = tui(typed("W"), size);
    assert_eq!(ui.ask(&choose(), None).await, c("wifi"));
    let mut ui = tui([key(KeyCode::Esc)], size);
    assert_eq!(ui.ask(&choose(), None).await, c("back"));
    let mut ui = tui([ctrl('c')], size);
    assert_eq!(ui.ask(&choose(), None).await, Answer::Quit);
    let mut ui = tui([Got::Ended], size);
    assert_eq!(ui.ask(&choose(), None).await, Answer::Quit);
    // A key that is not on the screen does nothing.
    let mut ui = tui([key(KeyCode::Char('z')), key(KeyCode::Enter)], size);
    assert_eq!(ui.ask(&choose(), None).await, c("next"));
    // Released keys are not pressed again.
    let mut release = KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE);
    release.kind = crossterm::event::KeyEventKind::Release;
    let mut ui = tui(
        [Got::Event(Event::Key(release)), typed("b").remove(0)],
        size,
    );
    assert_eq!(ui.ask(&choose(), None).await, c("back"));
}

#[tokio::test]
async fn ticks_are_ticked_and_gone_on_with() {
    let size = (80, 24);
    let mut ui = tui([key(KeyCode::Enter)], size);
    assert_eq!(
        ui.ask(&ticks(), None).await,
        Answer::Ticks(vec!["voice_control"])
    );
    let mut ui = tui(
        [
            key(KeyCode::Char('1')),
            key(KeyCode::Char('2')),
            key(KeyCode::Home),
            key(KeyCode::Down),
            key(KeyCode::Down),
            key(KeyCode::Char(' ')),
            key(KeyCode::End),
            key(KeyCode::Up),
            key(KeyCode::Enter),
        ],
        size,
    );
    assert_eq!(
        ui.ask(&ticks(), None).await,
        Answer::Ticks(vec!["voice", "projects"])
    );
    let mut ui = tui(typed("4l"), size);
    assert_eq!(ui.ask(&ticks(), None).await, c("later"));
}

#[tokio::test]
async fn text_is_typed_and_sent() {
    let size = (80, 24);
    let mut keys = typed("~/work/ap");
    keys.extend([key(KeyCode::Backspace), key(KeyCode::Backspace)]);
    keys.extend(typed("app  "));
    keys.push(key(KeyCode::Enter));
    let mut ui = tui(keys, size);
    assert_eq!(ui.ask(&folder(), None).await, text("~/work/app"));

    let mut keys = typed("one two");
    keys.push(ctrl('w'));
    keys.push(Got::Event(Event::Paste("three\nfour".into())));
    keys.push(key(KeyCode::Enter));
    let mut ui = tui(keys, size);
    assert_eq!(ui.ask(&folder(), None).await, text("one threefour"));

    // Esc clears what was typed; with nothing typed, it sends nothing.
    let mut keys = typed("oops");
    keys.extend([key(KeyCode::Esc), key(KeyCode::Esc)]);
    let mut ui = tui(keys, size);
    assert_eq!(ui.ask(&folder(), None).await, text(""));
    let mut keys = typed("abc");
    keys.extend([ctrl('u'), key(KeyCode::Enter)]);
    let mut ui = tui(keys, size);
    assert_eq!(ui.ask(&folder(), None).await, text(""));
    // Letters are typed, not chosen with.
    let mut keys = typed("b");
    keys.push(key(KeyCode::Enter));
    let mut ui = tui(keys, size);
    assert_eq!(ui.ask(&folder(), None).await, text("b"));
}

#[tokio::test]
async fn a_click_picks_what_it_lands_on() {
    let size = (80, 24);
    let screen = choose();
    let (_, drawn) = render(&screen, &View::new(&screen), TRUE, size);
    let (area, _) = drawn
        .hits
        .iter()
        .find(|(_, item)| *item == draw::Item::Choice("wifi"))
        .unwrap();
    let click = MouseEventKind::Down(MouseButton::Left);
    let mut ui = tui([mouse(click, area.x + 5, area.y)], size);
    assert_eq!(ui.ask(&screen, None).await, c("wifi"));

    let screen = ticks();
    let (_, drawn) = render(&screen, &View::new(&screen), TRUE, size);
    let at = |wanted: draw::Item| {
        let (area, _) = drawn.hits.iter().find(|(_, item)| *item == wanted).unwrap();
        mouse(click, area.x + 2, area.y)
    };
    let mut ui = tui([at(draw::Item::Tick(0)), at(draw::Item::GoOn)], size);
    assert_eq!(
        ui.ask(&screen, None).await,
        Answer::Ticks(vec!["voice", "voice_control"])
    );
    // A click on nothing picks nothing.
    let mut ui = tui([mouse(click, 0, 0), key(KeyCode::Char('l'))], size);
    assert_eq!(ui.ask(&screen, None).await, c("later"));
}

#[tokio::test]
async fn a_long_page_scrolls_and_what_it_asks_stays() {
    let size = (80, 24);
    let mut blocks: Vec<Block> = (1..=30)
        .map(|n| Block::Fact {
            mark: Mark::Info,
            label: format!("Line {n}"),
            detail: String::new(),
        })
        .collect();
    blocks.insert(0, Block::Text("The top of the page.".into()));
    let screen = Screen { blocks, ..choose() };
    let wait = Some(Duration::from_millis(30));
    let wheel = |kind| mouse(kind, 40, 10);
    let mut ui = tui([wheel(MouseEventKind::ScrollDown)], size);
    assert_eq!(ui.ask(&screen, wait).await, Answer::Waited);
    let text = shown(ui.backend().buffer());
    assert!(!text.contains("The top of the page."));
    assert!(text.contains("Line 3"));
    assert!(text.contains("Continue"));
    assert!(text.contains("pgup pgdn for more"));
    ui.events().events.push_back(key(KeyCode::PageDown));
    assert_eq!(ui.ask(&screen, wait).await, Answer::Waited);
    let text = shown(ui.backend().buffer());
    assert!(!text.contains("Line 3\n"), "{text}");
    assert!(text.contains("Continue"));
    ui.events()
        .events
        .extend([key(KeyCode::PageUp), key(KeyCode::PageUp)]);
    assert_eq!(ui.ask(&screen, wait).await, Answer::Waited);
    let text = shown(ui.backend().buffer());
    assert!(text.contains("The top of the page."));
    assert!(text.contains("╴pgdn for more╶"));
}

#[tokio::test]
async fn what_was_done_is_kept_while_the_screen_waits() {
    let size = (80, 24);
    let wait = Some(Duration::from_millis(30));
    let mut ui = tui([key(KeyCode::Down)], size);
    assert_eq!(ui.ask(&choose(), wait).await, Answer::Waited);
    // The screen changes as it waits; the choice moved to stays.
    let mut changed = choose();
    changed.blocks.push(Block::Fact {
        mark: Mark::Wait,
        label: "Waiting for your phone".into(),
        detail: String::new(),
    });
    ui.events().events.push_back(key(KeyCode::Enter));
    assert_eq!(ui.ask(&changed, wait).await, c("wifi"));
    // Once answered, the next screen starts afresh.
    ui.events().events.push_back(key(KeyCode::Enter));
    assert_eq!(ui.ask(&choose(), None).await, c("next"));
}

#[tokio::test]
async fn nothing_done_is_carried_to_a_screen_about_something_else() {
    let size = (80, 24);
    let wait = Some(Duration::from_millis(30));
    let asking = |about: &str| Screen {
        about: Some(about.into()),
        ..plain_screen(
            Step::Agent,
            vec![Block::Text("It asks to run a command.".into())],
            Ask::Choose(vec![
                Choice::new('a', "allow", "Allow"),
                Choice::new('d', "deny", "Don't allow"),
            ]),
        )
    };
    let mut ui = tui([key(KeyCode::Down)], size);
    assert_eq!(ui.ask(&asking("approval A 1"), wait).await, Answer::Waited);
    // Another approval, looking the same, takes its place: Allow, picked
    // for the first, is not picked for it.
    ui.events().events.push_back(key(KeyCode::Enter));
    assert_eq!(ui.ask(&asking("approval B 1"), wait).await, Answer::Waited);
    // Nor are ticks carried to another question with as many options.
    let ticking = |about: &str| Screen {
        about: Some(about.into()),
        ..ticks()
    };
    let mut ui = tui([key(KeyCode::Char('1'))], size);
    assert_eq!(ui.ask(&ticking("approval A 1"), wait).await, Answer::Waited);
    let enter = KeyCode::Enter;
    ui.events().events.push_back(key(enter));
    let Answer::Ticks(on) = ui.ask(&ticking("approval B 1"), None).await else {
        panic!("the ticks were not gone on with");
    };
    assert_eq!(on, vec!["voice_control"]);
}

#[tokio::test]
async fn a_screen_shown_while_something_happens_is_answered_at_once() {
    let busy = Screen {
        map: map(Step::Background),
        ..plain_screen(
            Step::Background,
            vec![Block::Fact {
                mark: Mark::Wait,
                label: "Starting Øphio…".into(),
                detail: String::new(),
            }],
            Ask::Nothing,
        )
    };
    // No key is read for it.
    let mut ui = tui([key(KeyCode::Enter)], (80, 24));
    assert_eq!(ui.ask(&busy, None).await, Answer::Waited);
    assert!(shown(ui.backend().buffer()).contains("Starting Øphio…"));
    assert_eq!(ui.ask(&choose(), None).await, c("next"));
}

#[tokio::test]
async fn the_screen_is_drawn_again_after_a_program_had_the_terminal() {
    let mut ui = tui([key(KeyCode::Down)], (80, 24));
    assert_eq!(
        ui.ask(&choose(), Some(Duration::from_millis(10))).await,
        Answer::Waited
    );
    ui.suspend().await;
    ui.resume().await;
    assert!(!shown(ui.backend().buffer()).contains("Continue"));
    ui.events().events.push_back(key(KeyCode::Enter));
    assert_eq!(ui.ask(&choose(), None).await, c("wifi"));
    assert!(shown(ui.backend().buffer()).contains("Continue"));
}

/// The command a drawing stands for: each dot is a space and, when it says
/// it is shown with escapes, each escape the character it stands for. Nothing
/// else drawn may be anything but plain.
/// Each command on a drawn screen, its rows joined, and whether every one
/// of its rows is marked as shown with escapes.
fn commands_drawn(text: &str) -> Vec<(String, bool)> {
    let rows: Vec<Vec<char>> = text.lines().map(|row| row.chars().collect()).collect();
    let mut commands: Vec<(String, bool)> = Vec::new();
    // Where the command being read starts, and the width of its margin.
    let mut at: Option<(usize, usize)> = None;
    for row in &rows {
        if let Some(start) = row.iter().position(|c| *c == '▸') {
            let marked = row.get(start + 1) == Some(&'┆');
            commands.push((String::new(), marked));
            at = Some((start, if marked { 3 } else { 2 }));
        } else if let Some((start, margin)) = at {
            let marked = commands.last().is_some_and(|(_, marked)| *marked);
            let continued: &[char] = if marked {
                &[' ', '┆', ' ']
            } else {
                &[' ', ' ']
            };
            if row.get(start..start + margin) != Some(continued) {
                at = None;
            }
        }
        let Some((start, margin)) = at else { continue };
        let part: String = row
            .get(start + margin..)
            .unwrap_or_default()
            .iter()
            .collect();
        let part = part.trim_end_matches([' ', '│']);
        if part.is_empty() {
            at = None;
            continue;
        }
        if let Some((command, _)) = commands.last_mut() {
            command.push_str(part);
        }
    }
    commands
}

fn read_back(seen: &str, escaped: bool) -> String {
    let mut out = String::new();
    let mut chars = seen.chars();
    while let Some(c) = chars.next() {
        match c {
            '·' => out.push(' '),
            '\\' if escaped => match chars.next() {
                Some('\\') => out.push('\\'),
                Some('u') => {
                    assert_eq!(chars.next(), Some('{'), "{seen}");
                    let hex: String = chars.by_ref().take_while(|c| *c != '}').collect();
                    let code = u32::from_str_radix(&hex, 16).expect("an escape");
                    out.push(char::from_u32(code).expect("a character"));
                }
                other => panic!("a backslash before {other:?} in {seen}"),
            },
            c => {
                assert!(matches!(c, ' '..='~'), "{c:?} drawn as it is in {seen}");
                out.push(c);
            }
        }
    }
    out
}

#[test]
fn commands_shown_together_are_drawn_one_way() {
    // Every command drawn, its rows joined, and whether the screen says
    // they are shown with escapes.
    let drawn = |group: &[&str]| {
        let screen = Screen {
            map: map(Step::Agent),
            ..plain_screen(
                Step::Agent,
                group.iter().map(|&c| Block::Command(c.into())).collect(),
                Ask::Choose(vec![Choice::new('a', "allow", "Allow")]),
            )
        };
        let (buffer, _) = render(&screen, &View::new(&screen), TRUE, (80, 24));
        let text = shown(&buffer);
        let commands = commands_drawn(&text);
        let escaped = commands.iter().all(|(_, marked)| *marked);
        assert!(commands.iter().all(|(_, marked)| *marked == escaped));
        assert_eq!(escaped, text.contains("Shown with escapes"));
        (
            commands
                .into_iter()
                .map(|(command, _)| command)
                .collect::<Vec<_>>(),
            escaped,
        )
    };
    // A backslash in one command is never taken for an escape another
    // command beside it needed.
    let groups = [
        ["rm -- '/tmp/P\\u{b7}old'", "echo '\u{b7}'"],
        ["rm -- '/tmp/P\u{b7}old'", "echo '\u{b7}'"],
    ];
    let mut seen = Vec::new();
    for group in groups {
        let (commands, escaped) = drawn(&group);
        assert!(escaped);
        assert_eq!(commands.len(), 2, "{commands:?}");
        for (command, drawn) in group.iter().zip(&commands) {
            assert_eq!(&read_back(drawn, escaped), command);
        }
        seen.push(commands);
    }
    assert_ne!(seen[0], seen[1]);
    assert_eq!(seen[0][0], "rm -- '/tmp/P\\\\u{b7}old'");
    // Plain commands beside each other are drawn as they are.
    let (commands, escaped) = drawn(&["ls -- 'a\\b'", "echo ok"]);
    assert_eq!(
        (commands, escaped),
        (vec!["ls -- 'a\\b'".to_owned(), "echo ok".into()], false)
    );
}

#[test]
fn every_row_of_a_command_says_how_it_is_drawn() {
    // A long request whose explanation of the escapes is scrolled out of
    // sight is still told apart from the one its escapes would spell.
    let tail = format!("; echo {}", "A".repeat(1000));
    let requests = [
        format!("rm -- '/tmp/P\\u{{b7}}old'{tail}"),
        format!("rm -- '/tmp/P\u{b7}old'{tail}"),
    ];
    let screen = |command: &str| Screen {
        map: map(Step::Agent),
        ..plain_screen(
            Step::Agent,
            vec![Block::Command(command.into())],
            Ask::Choose(vec![Choice::new('a', "allow", "Allow")]),
        )
    };
    for scroll in [0, 6, u16::MAX] {
        let mut seen = Vec::new();
        for (n, request) in requests.iter().enumerate() {
            let screen = screen(request);
            let view = View {
                scroll,
                ..View::new(&screen)
            };
            let (buffer, _) = render(&screen, &view, TRUE, (80, 24));
            let text = shown(&buffer);
            if scroll == 0 {
                assert!(!text.contains("Shown with escapes"), "{text}");
            }
            // Every row of the command in sight carries its mark, or none does.
            let rows: Vec<&str> = text.lines().filter(|row| row.contains("AAAA")).collect();
            assert!(rows.len() > 5, "{text}");
            for row in &rows {
                // The mark stands in the margin, before the command's text.
                let text = row.find("rm -- ").or(row.find("AAAA")).unwrap();
                let marked = row.find('┆').is_some_and(|mark| mark < text);
                assert_eq!(marked, n == 1, "{row:?}");
                assert_eq!(row.contains('┆'), marked, "{row:?}");
            }
            seen.push(text);
        }
        assert_ne!(seen[0], seen[1]);
    }
}

#[test]
fn commands_are_shown_exactly_as_they_would_run() {
    let screen = |command: &str| Screen {
        map: map(Step::Agent),
        ..plain_screen(
            Step::Agent,
            vec![Block::Command(command.into())],
            Ask::Choose(vec![Choice::new('a', "allow", "Allow")]),
        )
    };
    // The command as drawn, its rows joined, read back from the screen, and
    // whether its rows are marked as shown with escapes.
    let drawn = |buffer: &Buffer| {
        let mut commands = commands_drawn(&shown(buffer));
        assert_eq!(commands.len(), 1);
        commands.remove(0)
    };
    let quoted = "rm -- '/tmp/cache  old'";
    let (buffer, _) = render(&screen(quoted), &View::new(&screen(quoted)), TRUE, (80, 24));
    assert!(shown(&buffer).contains(quoted));
    assert!(!shown(&buffer).contains("Shown with escapes"));
    // A space where a row breaks and a middle dot in the name are told
    // apart, whatever the colours.
    let path = format!("/tmp/{}", "x".repeat(39));
    let (space, dot) = (format!("rm -- '{path} old'"), format!("rm -- '{path}·old'"));
    let mut seen = Vec::new();
    for command in [&space, &dot] {
        let (buffer, _) = render(
            &screen(command),
            &View::new(&screen(command)),
            TRUE,
            (80, 24),
        );
        let (drawn, escaped) = drawn(&buffer);
        assert_eq!(&read_back(&drawn, escaped), command);
        seen.push((drawn, escaped));
    }
    assert_eq!(seen[0], (format!("rm -- '{path}·old'"), false));
    assert_eq!(seen[1], (format!("rm -- '{path}\\u{{b7}}old'"), true));
    // However the rows break, no character is lost or mistaken for another;
    // spaces where a row breaks are drawn as dots, and those at the end as
    // escapes.
    let mut dotted = 0;
    for size in SIZES {
        for n in 0..120 {
            let (x, y) = ("x".repeat(n), "y".repeat(20));
            for command in [
                format!("cp '{x}  {y}' to"),
                format!("printf '{x}\\n  {y}'"),
                format!("cp '{x}  {y}' to   "),
                format!("cp '{x} ·{y}' \\u{{b7}}"),
                format!("printf '\\u{{b7}} {x}  {y}\\\\'\tdone"),
            ] {
                let (buffer, _) =
                    render(&screen(&command), &View::new(&screen(&command)), TRUE, size);
                let (seen, escaped) = drawn(&buffer);
                dotted += usize::from(seen.contains("··"));
                assert_eq!(read_back(&seen, escaped), command, "{size:?} {n}");
                let plain = command.chars().all(|c| matches!(c, ' '..='~'));
                assert_eq!(escaped, !plain || command.ends_with(' '), "{seen}");
                if command.ends_with(' ') {
                    assert!(seen.ends_with("to\\u{20}\\u{20}\\u{20}"), "{seen}");
                }
            }
        }
    }
    assert!(dotted >= SIZES.len(), "no row broke at the spaces");
}
