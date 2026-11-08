// SPDX-License-Identifier: Apache-2.0
//! The computer's name as every paired phone and tablet shows it: the one
//! the owner chose, kept with the computer's other settings, or else its
//! hostname.

use crate::error;
use protocol::{ErrorCode, Event, ProtocolError, model::HostInfo};

/// Where the owner's choice is kept, under `meta`. Absent means the hostname.
const KEY: &str = "computer_name";
/// The most characters a name may have.
pub(crate) const MAX_CHARS: usize = 60;

/// The name to show, and whether the owner chose it.
pub(crate) fn effective(r: &store::Read<'_>, hostname: &str) -> store::Result<(String, bool)> {
    Ok(match r.get::<String>("meta", KEY)? {
        Some(chosen) => (chosen, true),
        None => (hostname.to_owned(), false),
    })
}

/// A character that would change how the name, or text beside it, reads
/// without showing itself: a control character, which could rewrite what a
/// terminal prints; a line or paragraph separator; or a direction mark,
/// override or isolate, which could make a name read as another's or turn
/// a path next to it around.
fn hidden(c: char) -> bool {
    c.is_control() || matches!(c, '\u{2028}' | '\u{2029}') || ports::library::direction_control(c)
}

/// The name as it is kept: trimmed, 1 to 60 characters, none of them hidden.
pub(crate) fn valid(name: &str) -> Result<&str, ProtocolError> {
    let name = name.trim();
    let count = name.chars().count();
    if count == 0 || count > MAX_CHARS {
        return Err(error(
            ErrorCode::InvalidRequest,
            "A computer's name must be 1 to 60 characters.",
        ));
    }
    if name.chars().any(hidden) {
        return Err(error(
            ErrorCode::InvalidRequest,
            "A computer's name can't contain line breaks, control characters or characters that change the direction of text.",
        ));
    }
    Ok(name)
}

/// Keeps the owner's choice and tells every phone at once. The hostname
/// itself puts the computer back on it, so a later hostname is followed.
pub(crate) fn rename(tx: &mut store::Write<'_>, hostname: &str, name: &str) -> store::Result<()> {
    let name = valid(name)?;
    if name == hostname {
        tx.delete("meta", KEY)?;
    } else {
        tx.put("meta", KEY, &name)?;
    }
    let (name, renamed) = effective(&tx.read(), hostname)?;
    // The host is written at every start, before any command is admitted.
    if let Some(mut host) = tx.read().get::<HostInfo>("meta", "host")?
        && (host.name != name || host.renamed != renamed)
    {
        host.name = name;
        host.renamed = renamed;
        tx.put("meta", "host", &host)?;
        tx.emit(Event::HostUpdated {
            host: Box::new(host),
        })?;
    }
    Ok(())
}
