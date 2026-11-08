// SPDX-License-Identifier: Apache-2.0
//! Which installed app a spoken "open …" means. The app list never leaves the
//! computer: Jev only says that the person asked to open something, and the
//! words naming it are matched here against the computer's own app list.

/// A kind of app a person may ask for without naming it: "the browser",
/// "a terminal".
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum AppRole {
    Browser,
    Terminal,
    FileManager,
    TextEditor,
    Email,
    Calculator,
    Settings,
    Music,
    Video,
    Images,
    Documents,
    Calendar,
}
impl AppRole {
    /// What a person calls it, for "No … is installed".
    pub fn noun(self) -> &'static str {
        match self {
            Self::Browser => "web browser",
            Self::Terminal => "terminal",
            Self::FileManager => "file manager",
            Self::TextEditor => "text editor",
            Self::Email => "email app",
            Self::Calculator => "calculator",
            Self::Settings => "settings app",
            Self::Music => "music player",
            Self::Video => "video player",
            Self::Images => "image viewer",
            Self::Documents => "document viewer",
            Self::Calendar => "calendar",
        }
    }
}
/// Kinds a desktop names its own choice for, which a request for the kind
/// always follows.
const PREFERRED: [AppRole; 3] = [AppRole::Browser, AppRole::Terminal, AppRole::FileManager];
/// Words that name a role, after [`normalize`].
const ROLES: &[(&str, AppRole)] = &[
    ("browser", AppRole::Browser),
    ("web browser", AppRole::Browser),
    ("internet browser", AppRole::Browser),
    ("internet", AppRole::Browser),
    ("web", AppRole::Browser),
    ("terminal", AppRole::Terminal),
    ("terminal emulator", AppRole::Terminal),
    ("console", AppRole::Terminal),
    ("shell", AppRole::Terminal),
    ("command line", AppRole::Terminal),
    ("command prompt", AppRole::Terminal),
    ("file manager", AppRole::FileManager),
    ("files", AppRole::FileManager),
    ("file browser", AppRole::FileManager),
    ("file explorer", AppRole::FileManager),
    ("explorer", AppRole::FileManager),
    ("folders", AppRole::FileManager),
    ("my files", AppRole::FileManager),
    ("home folder", AppRole::FileManager),
    ("text editor", AppRole::TextEditor),
    ("editor", AppRole::TextEditor),
    ("notepad", AppRole::TextEditor),
    ("email", AppRole::Email),
    ("e mail", AppRole::Email),
    ("mail", AppRole::Email),
    ("email client", AppRole::Email),
    ("mail client", AppRole::Email),
    ("calculator", AppRole::Calculator),
    ("settings", AppRole::Settings),
    ("system settings", AppRole::Settings),
    ("control panel", AppRole::Settings),
    ("preferences", AppRole::Settings),
    ("music", AppRole::Music),
    ("music player", AppRole::Music),
    ("audio player", AppRole::Music),
    ("video player", AppRole::Video),
    ("videos", AppRole::Video),
    ("media player", AppRole::Video),
    ("movie player", AppRole::Video),
    ("image viewer", AppRole::Images),
    ("photos", AppRole::Images),
    ("pictures", AppRole::Images),
    ("photo viewer", AppRole::Images),
    ("pdf viewer", AppRole::Documents),
    ("pdf reader", AppRole::Documents),
    ("document viewer", AppRole::Documents),
    ("calendar", AppRole::Calendar),
];
/// One app the computer could open.
#[derive(Debug, Clone, PartialEq)]
pub struct LaunchApp {
    /// The platform's own identity for it: a desktop entry id on Linux.
    pub id: String,
    /// Its name as the app list shows it.
    pub name: String,
    /// Other names it answers to: its generic name, keywords, program name.
    pub aliases: Vec<String>,
    /// Kinds of app it is, from its own categories.
    pub roles: Vec<AppRole>,
    /// Kinds the desktop has chosen it as the default for.
    pub defaults: Vec<AppRole>,
    /// Shown in the app list. A default the list hides can still open by
    /// its role, but never by name.
    pub listed: bool,
    /// Where it comes from, to tell apart two apps of one name: "Flatpak".
    pub detail: Option<String>,
}
/// Which app a phrase means.
#[derive(Debug, Clone, PartialEq)]
pub enum Resolution {
    One(LaunchApp),
    /// Several equally good matches, best first, at most three.
    Several(Vec<LaunchApp>),
    /// Nothing installed matches; the reason says so in plain words.
    None(String),
}
/// Lowercase words, with punctuation as spaces, and without the words
/// people put around an app's name ("the", "app", "for me").
pub fn normalize(text: &str) -> String {
    let lower: String = text
        .to_lowercase()
        .chars()
        .map(|c| if c.is_alphanumeric() { c } else { ' ' })
        .collect();
    let mut words: Vec<&str> = lower.split_whitespace().collect();
    const LEADING: &[&str] = &["the", "a", "an", "my", "new", "another", "up", "me"];
    const TRAILING: &[&str] = &[
        "app",
        "application",
        "program",
        "window",
        "please",
        "now",
        "for",
        "me",
        "thanks",
        "thank",
        "you",
        "again",
        "up",
    ];
    while words.first().is_some_and(|w| LEADING.contains(w)) {
        words.remove(0);
    }
    while words.len() > 1 && words.last().is_some_and(|w| TRAILING.contains(w)) {
        words.pop();
    }
    words.join(" ")
}
/// Spoken lead-ins before the command itself.
const POLITE: &[&str] = &[
    "please", "can", "could", "would", "will", "you", "hey", "jev", "ok", "okay", "now", "and",
    "then", "i", "want", "to", "like", "go", "ahead", "just",
];
/// The words after "open", "launch" or "start" that name what to open,
/// copied from the transcript. None when nothing follows.
pub fn launch_phrase(text: &str) -> Option<String> {
    let words = normalize_keep(text);
    const VERBS: &[&[&str]] = &[
        &["open", "up"],
        &["open"],
        &["launch"],
        &["start", "up"],
        &["start"],
        &["run"],
        &["bring", "up"],
        &["fire", "up"],
        &["pull", "up"],
        &["load"],
        &["show", "me"],
        &["switch", "to"],
    ];
    let mut at = 0;
    while words.get(at).is_some_and(|w| POLITE.contains(&w.as_str())) {
        at += 1;
    }
    let rest = &words[at..];
    let rest = VERBS
        .iter()
        .find(|verb| rest.len() >= verb.len() && rest.iter().zip(verb.iter()).all(|(a, b)| a == b))
        .map_or(rest, |verb| &rest[verb.len()..]);
    let phrase = normalize(&rest.join(" "));
    (!phrase.is_empty()).then_some(phrase)
}
/// Lowercase words with punctuation as spaces; nothing removed.
fn normalize_keep(text: &str) -> Vec<String> {
    text.to_lowercase()
        .chars()
        .map(|c| if c.is_alphanumeric() { c } else { ' ' })
        .collect::<String>()
        .split_whitespace()
        .map(str::to_owned)
        .collect()
}
/// The role a phrase names, if it names one rather than an app.
pub fn role(phrase: &str) -> Option<AppRole> {
    let phrase = normalize(phrase);
    ROLES
        .iter()
        .find(|(words, _)| *words == phrase)
        .map(|(_, role)| *role)
}
/// How well `phrase` names `app`: 0 is not at all.
fn score(phrase: &str, app: &LaunchApp) -> u32 {
    let compact = |s: &str| s.replace(' ', "");
    let name = normalize(&app.name);
    let aliases: Vec<String> = app.aliases.iter().map(|a| normalize(a)).collect();
    if name == phrase || compact(&name) == compact(phrase) {
        return 100;
    }
    if aliases
        .iter()
        .any(|a| a == phrase || compact(a) == compact(phrase))
    {
        return 90;
    }
    let words: Vec<&str> = phrase.split(' ').collect();
    let starts = |of: &str| {
        let of: Vec<&str> = of.split(' ').collect();
        of.len() > words.len() && of[..words.len()] == words[..]
    };
    if starts(&name) {
        return 80;
    }
    if aliases.iter().any(|a| starts(a)) {
        return 70;
    }
    let close = |of: &str| {
        let (a, b) = (compact(of), compact(phrase));
        let n = a.chars().count().min(b.chars().count());
        let allowed = if n >= 8 {
            2
        } else if n >= 4 {
            1
        } else {
            0
        };
        allowed > 0 && distance(&a, &b) <= allowed
    };
    if close(&name) || aliases.iter().any(|a| close(a)) {
        return 60;
    }
    0
}
/// Levenshtein distance, for a misheard letter or two.
fn distance(a: &str, b: &str) -> usize {
    let b: Vec<char> = b.chars().collect();
    let mut row: Vec<usize> = (0..=b.len()).collect();
    for (i, ca) in a.chars().enumerate() {
        let mut previous = row[0];
        row[0] = i + 1;
        for (j, cb) in b.iter().enumerate() {
            let current = row[j + 1];
            row[j + 1] = (previous + usize::from(ca != *cb))
                .min(row[j] + 1)
                .min(row[j + 1] + 1);
            previous = current;
        }
    }
    row[b.len()]
}
/// The app `phrase` means among `apps`. Asking for the browser, a terminal
/// or the file manager follows the desktop's own choice first, even when an
/// app is named that (GNOME's terminal is "Terminal"); other kinds give way
/// to an app named exactly what was said. A kind picks the desktop's
/// default for it, or the one app of that kind.
pub fn resolve(phrase: &str, apps: &[LaunchApp]) -> Resolution {
    let phrase = normalize(phrase);
    if phrase.is_empty() {
        return Resolution::None("Say which app to open, such as “open the browser”".into());
    }
    let mut ranked: Vec<(u32, &LaunchApp)> = apps
        .iter()
        .filter(|a| a.listed)
        .map(|a| (score(&phrase, a), a))
        .filter(|(s, _)| *s > 0)
        .collect();
    ranked.sort_by_key(|(s, _)| std::cmp::Reverse(*s));
    if let Some(role) = role(&phrase)
        && (PREFERRED.contains(&role) || ranked.first().is_none_or(|(s, _)| *s < 100))
    {
        if let Some(app) = apps.iter().find(|a| a.defaults.contains(&role)) {
            return Resolution::One(app.clone());
        }
        let kind: Vec<&LaunchApp> = apps
            .iter()
            .filter(|a| a.listed && a.roles.contains(&role))
            .collect();
        match kind.as_slice() {
            [] if ranked.is_empty() => {
                return Resolution::None(format!(
                    "No {} is installed on the computer",
                    role.noun()
                ));
            }
            // Nothing of that kind: an app named that may still be meant.
            [] => {}
            [one] => return Resolution::One((*one).clone()),
            several => return Resolution::Several(distinct(several.iter().copied())),
        }
    }
    let Some(&(best, _)) = ranked.first() else {
        return Resolution::None(format!(
            "No app called “{phrase}” is installed on the computer. Say its name as the app list shows it"
        ));
    };
    let top = distinct(ranked.iter().filter(|(s, _)| *s == best).map(|(_, a)| *a));
    if top.len() == 1 {
        Resolution::One(top[0].clone())
    } else {
        Resolution::Several(top)
    }
}
/// At most three apps, one per desktop entry, in order. Two of one name
/// (a native and a Flatpak Firefox) are both kept: [`label`] tells them
/// apart.
fn distinct<'a>(apps: impl Iterator<Item = &'a LaunchApp>) -> Vec<LaunchApp> {
    let mut seen = std::collections::HashSet::new();
    apps.filter(|a| seen.insert(a.id.clone()))
        .take(3)
        .cloned()
        .collect()
}
/// How `app` reads among `others`: its name, and where it comes from when
/// another has the same name.
pub fn label(app: &LaunchApp, others: &[LaunchApp]) -> String {
    let shared = others
        .iter()
        .any(|o| o.id != app.id && o.name.eq_ignore_ascii_case(&app.name));
    match (&app.detail, shared) {
        (_, false) => app.name.clone(),
        (Some(detail), true) => format!("{} ({detail})", app.name),
        // The one without a source of its own is the system's.
        (None, true)
            if others.iter().any(|o| {
                o.id != app.id && o.name.eq_ignore_ascii_case(&app.name) && o.detail.is_some()
            }) =>
        {
            app.name.clone()
        }
        (None, true) => format!("{} ({})", app.name, app.id),
    }
}
/// Whether opening this app could be consequential by its own name: power,
/// session and removal tools are confirmed first.
pub fn risky(app: &LaunchApp) -> bool {
    let words = normalize_keep(&format!(
        "{} {} {}",
        app.name,
        app.id,
        app.aliases.join(" ")
    ));
    let has = |w: &str| words.iter().any(|x| x == w);
    [
        "shutdown",
        "poweroff",
        "reboot",
        "restart",
        "logout",
        "suspend",
        "hibernate",
        "uninstall",
        "uninstaller",
        "remove",
        "delete",
        "reset",
        "wipe",
        "format",
        "erase",
    ]
    .iter()
    .any(|w| has(w))
        || words.windows(2).any(|p| {
            matches!(
                (p[0].as_str(), p[1].as_str()),
                ("shut", "down") | ("power", "off") | ("log", "out") | ("sign", "out")
            )
        })
}
