// SPDX-License-Identifier: Apache-2.0
//! The `PATH` a program started now would get, rather than the one this
//! process started with.
//!
//! A Windows process keeps the environment it was started with. An installer
//! that adds its folder to `PATH` (Node.js's does, for everyone) writes it to
//! the registry and tells new programs; one already running, such as setup,
//! doesn't see it until it is started again. Setup reads `PATH` afresh from
//! the registry, as Explorer does for a new window, so that a program
//! installed while it runs is found.
//!
//! Merging and expanding are written for any computer, so that they can be
//! tested anywhere; only Windows reads the registry.
#[cfg(windows)]
pub use crate::win::environment::fresh_path;

/// Expands `%NAME%` references in `text` the way Windows does for a registry
/// value of type `REG_EXPAND_SZ`: with the value `var` gives for `NAME`, and
/// a reference to a name without one, or a lone `%`, left as written.
pub fn expand(text: &str, var: impl Fn(&str) -> Option<String>) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(start) = rest.find('%') {
        out.push_str(&rest[..start]);
        let after = &rest[start + 1..];
        match after.find('%') {
            Some(end) if end > 0 => match var(&after[..end]) {
                Some(value) => {
                    out.push_str(&value);
                    rest = &after[end + 1..];
                }
                None => {
                    // Kept as written; its closing `%` may open the next.
                    out.push('%');
                    out.push_str(&after[..end]);
                    rest = &after[end..];
                }
            },
            _ => {
                out.push('%');
                rest = after;
            }
        }
    }
    out.push_str(rest);
    out
}

/// The `PATH` for a program started now: the computer's own folders
/// (`machine`), then the person's (`user`), as Windows gives a new program,
/// then any folder this process's `current` one has that neither does, so
/// that nothing it could already start is lost. Each folder appears once,
/// compared as Windows does: ignoring case and a trailing `\`.
pub fn merged(machine: Option<&str>, user: Option<&str>, current: Option<&str>) -> String {
    let mut seen: Vec<String> = Vec::new();
    let mut folders: Vec<&str> = Vec::new();
    for folder in [machine, user, current]
        .into_iter()
        .flatten()
        .flat_map(|path| path.split(';'))
        .map(str::trim)
        .filter(|folder| !folder.is_empty())
    {
        let key = folder.trim_end_matches(['\\', '/']).to_lowercase();
        if !seen.contains(&key) {
            seen.push(key);
            folders.push(folder);
        }
    }
    folders.join(";")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn vars(name: &str) -> Option<String> {
        match name.to_ascii_uppercase().as_str() {
            "USERPROFILE" => Some(r"C:\Users\you".into()),
            "SYSTEMROOT" => Some(r"C:\Windows".into()),
            _ => None,
        }
    }

    #[test]
    fn expands_what_is_set_and_keeps_the_rest_as_written() {
        assert_eq!(
            expand(r"%SystemRoot%\system32;%USERPROFILE%\.local\bin", vars),
            r"C:\Windows\system32;C:\Users\you\.local\bin"
        );
        assert_eq!(expand(r"%NOPE%\bin", vars), r"%NOPE%\bin");
        assert_eq!(expand("50%", vars), "50%");
        assert_eq!(expand("%%", vars), "%%");
        assert_eq!(
            expand(r"%NOPE%USERPROFILE%\x", vars),
            r"%NOPEC:\Users\you\x"
        );
        assert_eq!(expand(r"C:\plain", vars), r"C:\plain");
    }

    #[test]
    fn a_new_install_comes_before_what_the_process_had_and_nothing_is_lost() {
        let machine = r"C:\Windows\system32;C:\Windows;C:\Program Files\nodejs\";
        let user =
            r"C:\Users\you\AppData\Local\Microsoft\WindowsApps;C:\Users\you\AppData\Roaming\npm";
        let current = r"C:\WINDOWS\system32;C:\Windows;C:\Users\you\AppData\Local\Microsoft\WindowsApps;C:\Tools";
        assert_eq!(
            merged(Some(machine), Some(user), Some(current)),
            [
                r"C:\Windows\system32",
                r"C:\Windows",
                r"C:\Program Files\nodejs\",
                r"C:\Users\you\AppData\Local\Microsoft\WindowsApps",
                r"C:\Users\you\AppData\Roaming\npm",
                r"C:\Tools",
            ]
            .join(";")
        );
    }

    #[test]
    fn missing_or_empty_parts_are_skipped() {
        assert_eq!(merged(None, None, Some(r"C:\a;;C:\b;")), r"C:\a;C:\b");
        assert_eq!(merged(Some(r"C:\a"), None, None), r"C:\a");
        assert_eq!(merged(None, None, None), "");
    }
}
