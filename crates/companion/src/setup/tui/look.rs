// SPDX-License-Identifier: Apache-2.0
//! The app's dark palette in a terminal, at whatever depth of colour the
//! terminal has.
use ratatui::style::{Color, Modifier, Style};

/// How many colours the terminal shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Depth {
    /// Any colour.
    True,
    /// xterm's 256.
    Many,
    /// The 16 named colours, as the terminal's own theme has them.
    Few,
    /// None: `NO_COLOR` is set, or the terminal is too basic.
    None,
}

impl Depth {
    pub fn detect() -> Self {
        Self::detect_from(|name| std::env::var(name).ok(), cfg!(windows))
    }

    /// The depth the environment `var` describes, on Windows or not.
    pub fn detect_from(var: impl Fn(&str) -> Option<String>, windows: bool) -> Self {
        // Set but empty counts as unset, as https://no-color.org has it.
        let var = |name: &str| var(name).filter(|value| !value.is_empty());
        if var("NO_COLOR").is_some() {
            return Depth::None;
        }
        let term = var("TERM").unwrap_or_default().to_ascii_lowercase();
        if term == "dumb" {
            return Depth::None;
        }
        let colorterm = var("COLORTERM").unwrap_or_default().to_ascii_lowercase();
        let program = var("TERM_PROGRAM").unwrap_or_default().to_ascii_lowercase();
        let truecolor = matches!(colorterm.as_str(), "truecolor" | "24bit")
            || var("WT_SESSION").is_some()
            || var("KITTY_WINDOW_ID").is_some()
            || ["kitty", "ghostty", "wezterm"]
                .iter()
                .any(|name| term.contains(name) || program.contains(name));
        if truecolor {
            Depth::True
        } else if term.contains("256color") || windows {
            Depth::Many
        } else {
            Depth::Few
        }
    }
}

/// The app's colours, by what they are for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Ink {
    Bg,
    Text,
    Soft,
    Faint,
    Accent,
    AccentSoft,
    Bloom,
    Filament,
    Line,
    LineStrong,
    Warn,
    Stop,
    Ok,
}

impl Ink {
    /// As `client/src/design/tokens.css` has it, dark.
    const fn rgb(self) -> (u8, u8, u8) {
        match self {
            Ink::Bg => (0x0a, 0x06, 0x05),
            Ink::Text => (0xf1, 0xee, 0xe6),
            Ink::Soft => (0xc7, 0xb9, 0xc9),
            Ink::Faint => (0xb7, 0xaa, 0xa6),
            Ink::Accent => (0xc3, 0xff, 0x24),
            Ink::AccentSoft => (0x24, 0x2d, 0x13),
            Ink::Bloom => (0xc2, 0x91, 0xef),
            Ink::Filament => (0x76, 0x52, 0x8b),
            Ink::Line => (0x49, 0x37, 0x4f),
            Ink::LineStrong => (0x87, 0x70, 0x8f),
            Ink::Warn => (0xff, 0xb8, 0x5c),
            Ink::Stop => (0xff, 0x8f, 0x85),
            Ink::Ok => (0x7b, 0xd3, 0x89),
        }
    }

    /// The nearest of the 16, which the terminal's theme may change: text
    /// keeps the theme's own colour, so it is readable on a light one too.
    const fn named(self) -> Color {
        match self {
            Ink::Bg | Ink::Text | Ink::AccentSoft => Color::Reset,
            Ink::Soft | Ink::Faint | Ink::LineStrong => Color::Gray,
            Ink::Accent => Color::LightGreen,
            Ink::Bloom => Color::LightMagenta,
            Ink::Filament => Color::Magenta,
            Ink::Line => Color::DarkGray,
            Ink::Warn => Color::Yellow,
            Ink::Stop => Color::LightRed,
            Ink::Ok => Color::Green,
        }
    }
}

/// The nearest of xterm's 256: its 6×6×6 cube or its 24 greys.
fn indexed((r, g, b): (u8, u8, u8)) -> Color {
    const LEVELS: [u8; 6] = [0, 95, 135, 175, 215, 255];
    let nearest = |c: u8| {
        (0..LEVELS.len())
            .min_by_key(|&i| LEVELS[i].abs_diff(c))
            .unwrap_or(0)
    };
    let distance = |(x, y, z): (u8, u8, u8)| {
        [(x, r), (y, g), (z, b)]
            .iter()
            .map(|&(a, b)| u32::from(a.abs_diff(b)).pow(2))
            .sum::<u32>()
    };
    let (ri, gi, bi) = (nearest(r), nearest(g), nearest(b));
    let cube = (LEVELS[ri], LEVELS[gi], LEVELS[bi]);
    let mean = (u32::from(r) + u32::from(g) + u32::from(b)) / 3;
    // Greys run 8, 18, … 238.
    let grey = (mean.saturating_sub(3) / 10).min(23);
    let level = u8::try_from(8 + grey * 10).unwrap_or(238);
    let index = |n: usize| u8::try_from(n).unwrap_or(0);
    if distance((level, level, level)) < distance(cube) {
        Color::Indexed(232 + index(grey as usize))
    } else {
        Color::Indexed(16 + index(36 * ri + 6 * gi + bi))
    }
}

/// Colours and styles at one depth.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Look {
    pub depth: Depth,
}

impl Look {
    pub fn color(self, ink: Ink) -> Color {
        let (r, g, b) = ink.rgb();
        match self.depth {
            Depth::True => Color::Rgb(r, g, b),
            Depth::Many => indexed((r, g, b)),
            Depth::Few => ink.named(),
            Depth::None => Color::Reset,
        }
    }

    /// Whether colour carries nothing, so shapes and words have to.
    pub fn mono(self) -> bool {
        self.depth == Depth::None
    }

    /// Whether the app's own background is painted: only where it is exact
    /// enough to be the app's.
    pub fn painted(self) -> bool {
        matches!(self.depth, Depth::True | Depth::Many)
    }

    pub fn ink(self, ink: Ink) -> Style {
        Style::new().fg(self.color(ink))
    }

    pub fn bold(self, ink: Ink) -> Style {
        self.ink(ink).add_modifier(Modifier::BOLD)
    }

    /// The whole screen beneath everything.
    pub fn base(self) -> Style {
        if self.painted() {
            Style::new()
                .fg(self.color(Ink::Text))
                .bg(self.color(Ink::Bg))
        } else {
            Style::new()
        }
    }

    /// The row a choice is selected on.
    pub fn selected(self) -> Style {
        if self.painted() {
            Style::new().bg(self.color(Ink::AccentSoft))
        } else {
            Style::new().add_modifier(Modifier::REVERSED | Modifier::BOLD)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn depth(vars: &[(&str, &str)], windows: bool) -> Depth {
        let vars: HashMap<String, String> = vars
            .iter()
            .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
            .collect();
        Depth::detect_from(|name| vars.get(name).cloned(), windows)
    }

    #[test]
    fn the_terminal_says_how_many_colours_it_has() {
        assert_eq!(depth(&[("COLORTERM", "truecolor")], false), Depth::True);
        assert_eq!(depth(&[("COLORTERM", "24bit")], false), Depth::True);
        assert_eq!(depth(&[("WT_SESSION", "x")], true), Depth::True);
        assert_eq!(depth(&[("TERM", "xterm-kitty")], false), Depth::True);
        assert_eq!(depth(&[("TERM", "xterm-ghostty")], false), Depth::True);
        assert_eq!(depth(&[("TERM_PROGRAM", "WezTerm")], false), Depth::True);
        assert_eq!(depth(&[("TERM", "xterm-256color")], false), Depth::Many);
        assert_eq!(depth(&[], true), Depth::Many);
        assert_eq!(depth(&[("TERM", "xterm")], false), Depth::Few);
        assert_eq!(depth(&[("TERM", "linux")], false), Depth::Few);
        assert_eq!(depth(&[("TERM", "dumb")], false), Depth::None);
    }

    #[test]
    fn no_color_wins_unless_it_is_empty() {
        let truecolor = ("COLORTERM", "truecolor");
        assert_eq!(depth(&[truecolor, ("NO_COLOR", "1")], false), Depth::None);
        assert_eq!(depth(&[truecolor, ("NO_COLOR", "")], false), Depth::True);
    }

    #[test]
    fn the_palette_keeps_to_the_256() {
        assert_eq!(indexed((0, 0, 0)), Color::Indexed(16));
        assert_eq!(indexed((255, 255, 255)), Color::Indexed(231));
        // Near-black greys find the grey ramp.
        assert_eq!(indexed((0x0a, 0x06, 0x05)), Color::Indexed(232));
        // The accent stays a bright yellow-green.
        assert_eq!(
            indexed(Ink::Accent.rgb()),
            Color::Indexed(16 + 36 * 3 + 6 * 5)
        );
        let look = Look { depth: Depth::Many };
        assert!(matches!(look.color(Ink::Bloom), Color::Indexed(_)));
    }

    #[test]
    fn sixteen_colours_and_none_use_no_exact_colour() {
        for depth in [Depth::Few, Depth::None] {
            let look = Look { depth };
            for ink in [
                Ink::Bg,
                Ink::Text,
                Ink::Soft,
                Ink::Faint,
                Ink::Accent,
                Ink::AccentSoft,
                Ink::Bloom,
                Ink::Filament,
                Ink::Line,
                Ink::LineStrong,
                Ink::Warn,
                Ink::Stop,
                Ink::Ok,
            ] {
                let color = look.color(ink);
                assert!(!matches!(color, Color::Rgb(..) | Color::Indexed(_)));
                if depth == Depth::None {
                    assert_eq!(color, Color::Reset);
                }
            }
            assert!(look.selected().add_modifier.contains(Modifier::REVERSED));
        }
    }
}
