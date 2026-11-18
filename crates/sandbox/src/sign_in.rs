// SPDX-License-Identifier: Apache-2.0
//! A sign-in the agent renewed during a run that Øphio didn't get to finish.
//!
//! On a Mac a renewed sign-in waits in the conversation's own folder until the
//! end of the run carries it back. If Øphio stops first, the next run in that
//! conversation carries it back before it sets up, under the same rule: only
//! over the sign-in the unfinished run was given. What that was is kept here,
//! as a digest only, beside the record of shared places and out of the
//! agent's reach. One that can't go back yet stays as that run's own sign-in,
//! with the note as it was, and its end tries again.

#![cfg_attr(not(target_os = "macos"), allow(dead_code))]

use std::{
    io::{self, Write},
    path::Path,
};

/// Which sign-in the last run in this conversation was given, by its digest.
pub const RECORD: &str = "sign-in-given";
/// The most of a sign-in that is read; a bigger one isn't a sign-in.
const LIMIT: u64 = 1 << 20;

/// Where a run's sign-in came from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    /// A file in the agent's settings folder, reached by a link.
    File,
    /// Claude Code's entry in the login keychain, copied into a file.
    Keychain,
}
impl Kind {
    fn word(self) -> &'static str {
        match self {
            Kind::File => "file",
            Kind::Keychain => "keychain",
        }
    }
}

pub type Digest = [u8; 32];

pub fn digest(bytes: &[u8]) -> Digest {
    use sha2::Digest as _;
    sha2::Sha256::digest(bytes).into()
}

/// A regular file with one name, reached through no link, and no bigger than
/// a sign-in can be. Anything else is refused, so nothing the agent left is
/// followed: a link or a second name for a private file would carry that file
/// out to where the person's sign-in is kept.
pub fn read_own(path: &Path) -> io::Result<Vec<u8>> {
    read_at_most(path, LIMIT)
}

/// [`read_own`], for a sign-in no bigger than `most` bytes.
pub fn read_at_most(path: &Path, most: u64) -> io::Result<Vec<u8>> {
    let file = crate::sys::history_file(path, false)?;
    let mut bytes = Vec::new();
    io::Read::read_to_end(&mut io::Read::take(file, most + 1), &mut bytes)?;
    if bytes.len() as u64 > most {
        return Err(io::Error::other("too big to be a sign-in"));
    }
    Ok(bytes)
}

/// Notes which sign-in this run was given, replacing the last run's note.
/// Nothing known (`None`) leaves no note.
pub fn remember(state: &Path, kind: Kind, given: Option<Digest>) -> io::Result<()> {
    let Some(given) = given else {
        return forget(state);
    };
    let hex: String = given.iter().map(|b| format!("{b:02x}")).collect();
    crate::sys::replace_history(&state.join(RECORD), |file| {
        file.write_all(format!("{} {hex}\n", kind.word()).as_bytes())
    })
}

/// Removes the note once the run's sign-in is settled.
pub fn forget(state: &Path) -> io::Result<()> {
    match std::fs::remove_file(state.join(RECORD)) {
        Err(e) if e.kind() != io::ErrorKind::NotFound => Err(e),
        _ => Ok(()),
    }
}

fn recorded(state: &Path) -> Option<(Kind, Digest)> {
    let text = String::from_utf8(read_own(&state.join(RECORD)).ok()?).ok()?;
    let (word, hex) = text.trim_end().split_once(' ')?;
    let kind = [Kind::File, Kind::Keychain]
        .into_iter()
        .find(|k| k.word() == word)?;
    if hex.len() != 64 {
        return None;
    }
    let mut given = [0; 32];
    for (i, byte) in given.iter_mut().enumerate() {
        *byte = u8::from_str_radix(hex.get(i * 2..i * 2 + 2)?, 16).ok()?;
    }
    Some((kind, given))
}

/// The kind of sign-in the note says the last run was given, if there is one.
pub fn recorded_kind(state: &Path) -> Option<Kind> {
    recorded(state).map(|(kind, _)| kind)
}

/// The digest of the sign-in the note says the last run was given, if it
/// was of this `kind`.
pub fn recorded_given(state: &Path, kind: Kind) -> Option<Digest> {
    recorded(state).and_then(|(was, given)| (was == kind).then_some(given))
}

/// What the person's own sign-in is now.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Held {
    /// This one, by its digest.
    Is(Digest),
    /// None: signed out, or the file removed.
    Gone,
    /// It couldn't be read, so whether it changed isn't known.
    Unknown,
}

/// What became of a sign-in an unfinished run renewed.
#[derive(Debug)]
pub enum Recovered {
    /// Nothing to carry back: no note of a run given this kind of sign-in,
    /// nothing renewed at `leftover`, or the person's sign-in changed since
    /// (signed in again, or out). What is at `leftover` is the caller's to clear.
    Nothing,
    /// Carried back: the person's sign-in is now this.
    Back(Vec<u8>),
    /// Still owed: `put` failed, or the person's sign-in couldn't be read to
    /// tell. The renewed sign-in stays at `leftover` as the next run's own,
    /// the note stays as it is, and that run's end tries again over `given`.
    Kept { given: Digest, why: io::Error },
}

/// Carries back a renewed sign-in an unfinished run left at `leftover`, before
/// the next run clears that place. It goes back only if the note says the
/// unfinished run was given a sign-in of this `kind`, and `held`, the person's
/// sign-in now, is still the one it was given: one signed in again or signed
/// out since stays as it is. A link, a second name, an empty file, one bigger
/// than `most` bytes, or no note, carries nothing.
pub fn recover(
    state: &Path,
    leftover: &Path,
    kind: Kind,
    most: u64,
    held: impl FnOnce() -> Held,
    put: impl FnOnce(&[u8]) -> io::Result<()>,
) -> Recovered {
    let Some((was, given)) = recorded(state) else {
        return Recovered::Nothing;
    };
    if was != kind {
        return Recovered::Nothing;
    }
    let Ok(renewed) = read_at_most(leftover, most) else {
        return Recovered::Nothing;
    };
    if renewed.is_empty() || digest(&renewed) == given {
        return Recovered::Nothing;
    }
    match held() {
        Held::Is(now) if now == given => match put(&renewed) {
            Ok(()) => Recovered::Back(renewed),
            Err(why) => Recovered::Kept { given, why },
        },
        Held::Is(_) | Held::Gone => Recovered::Nothing,
        Held::Unknown => Recovered::Kept {
            given,
            why: io::Error::other("the sign-in on this computer couldn't be read"),
        },
    }
}

/// The person's sign-in file now. Only its absence is a sign-out.
pub fn held_file(real: &Path) -> Held {
    match read_own(real) {
        Ok(bytes) => Held::Is(digest(&bytes)),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Held::Gone,
        Err(_) => Held::Unknown,
    }
}

/// [`recover`] for a sign-in file: carried back over `real`.
pub fn recover_file(state: &Path, leftover: &Path, real: &Path) -> Recovered {
    recover(
        state,
        leftover,
        Kind::File,
        LIMIT,
        || held_file(real),
        |renewed| crate::sys::replace_history(real, |file| file.write_all(renewed)),
    )
}

#[cfg(test)]
#[path = "sign_in_tests.rs"]
mod tests;
