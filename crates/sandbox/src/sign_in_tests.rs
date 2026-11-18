// SPDX-License-Identifier: Apache-2.0
use super::*;
use std::{
    cell::RefCell,
    os::unix::fs::{PermissionsExt, symlink},
    path::PathBuf,
};

struct Place {
    _dir: tempfile::TempDir,
    /// The conversation's state folder, out of the agent's reach.
    state: PathBuf,
    /// Where the agent's sign-in is, in its own folder.
    leftover: PathBuf,
    /// The person's own sign-in file.
    real: PathBuf,
    outside: PathBuf,
}

fn place() -> Place {
    let dir = tempfile::tempdir().unwrap();
    let base = dir.path().canonicalize().unwrap();
    let state = base.join("state");
    let config = base.join("home/.codex");
    let outside = base.join("outside");
    for folder in [&state.join("native"), &config, &outside] {
        std::fs::create_dir_all(folder).unwrap();
    }
    let real = config.join("auth.json");
    std::fs::write(&real, "given").unwrap();
    std::fs::write(outside.join("private"), "private").unwrap();
    Place {
        _dir: dir,
        leftover: state.join("native/auth.json"),
        state,
        real,
        outside,
    }
}

/// A run that renewed its sign-in and wasn't finished: the note of what it
/// was given, and the renewed sign-in where the link was.
fn unfinished(p: &Place, kind: Kind) {
    remember(&p.state, kind, Some(digest(b"given"))).unwrap();
    std::fs::write(&p.leftover, "renewed").unwrap();
}

/// What became of it, in short.
#[derive(Debug, PartialEq)]
enum Was {
    Nothing,
    Back(Vec<u8>),
    Kept(Digest),
}
fn was(recovered: Recovered) -> Was {
    match recovered {
        Recovered::Nothing => Was::Nothing,
        Recovered::Back(renewed) => Was::Back(renewed),
        Recovered::Kept { given, .. } => Was::Kept(given),
    }
}
fn back(renewed: &str) -> Was {
    Was::Back(renewed.as_bytes().to_vec())
}
fn kept() -> Was {
    Was::Kept(digest(b"given"))
}

/// A keychain that holds `held` and takes a renewal if `takes`, and what was
/// put into it.
fn keychain_taking(p: &Place, held: Held, takes: bool) -> (Was, Option<Vec<u8>>) {
    let put = RefCell::new(None);
    let recovered = recover(
        &p.state,
        &p.leftover,
        Kind::Keychain,
        LIMIT,
        || held,
        |renewed| {
            if !takes {
                return Err(io::Error::other("refused"));
            }
            *put.borrow_mut() = Some(renewed.to_vec());
            Ok(())
        },
    );
    (was(recovered), put.into_inner())
}
fn keychain(p: &Place, held: Option<&[u8]>) -> (Was, Option<Vec<u8>>) {
    keychain_taking(p, held.map_or(Held::Gone, |h| Held::Is(digest(h))), true)
}
fn file(p: &Place) -> Was {
    was(recover_file(&p.state, &p.leftover, &p.real))
}

#[test]
fn a_sign_in_renewed_in_an_unfinished_run_goes_back_before_the_next_starts() {
    // A sign-in file: carried back, then the next run is linked to it.
    let p = place();
    unfinished(&p, Kind::File);
    assert_eq!(file(&p), back("renewed"));
    assert_eq!(std::fs::read_to_string(&p.real).unwrap(), "renewed");
    let root = crate::sys::folder(&p.state.join("native")).unwrap();
    crate::setup::link_within(&root, Path::new("auth.json"), &p.real, true).unwrap();
    assert_eq!(std::fs::read_link(&p.leftover).unwrap(), p.real);
    assert_eq!(std::fs::read_to_string(&p.leftover).unwrap(), "renewed");

    // The keychain: carried back while it still holds what the run was given,
    // and the next run is given what was put there.
    let p = place();
    unfinished(&p, Kind::Keychain);
    assert_eq!(
        keychain(&p, Some(b"given")),
        (back("renewed"), Some(b"renewed".to_vec()))
    );
}

#[test]
fn a_sign_in_made_again_in_between_stays() {
    let p = place();
    unfinished(&p, Kind::File);
    std::fs::write(&p.real, "signed in again").unwrap();
    assert_eq!(file(&p), Was::Nothing);
    assert_eq!(std::fs::read_to_string(&p.real).unwrap(), "signed in again");
    // Signed out: the file is gone, and stays gone.
    std::fs::remove_file(&p.real).unwrap();
    assert_eq!(file(&p), Was::Nothing);
    assert!(!p.real.exists());

    let p = place();
    unfinished(&p, Kind::Keychain);
    assert_eq!(keychain(&p, Some(b"signed in again")), (Was::Nothing, None));
    assert_eq!(keychain(&p, None), (Was::Nothing, None));
}

/// Putting it back failed: the renewed sign-in stays where it was, as the
/// next run's own, with the note as it was, so that run's end tries again.
#[test]
fn a_sign_in_that_cant_go_back_yet_stays_for_the_next_run() {
    let p = place();
    unfinished(&p, Kind::Keychain);
    assert_eq!(
        keychain_taking(&p, Held::Is(digest(b"given")), false),
        (kept(), None)
    );
    assert_eq!(std::fs::read_to_string(&p.leftover).unwrap(), "renewed");
    assert_eq!(recorded(&p.state), Some((Kind::Keychain, digest(b"given"))));

    // A sign-in file whose folder can't take it.
    let p = place();
    unfinished(&p, Kind::File);
    let config = p.real.parent().unwrap();
    std::fs::set_permissions(config, std::fs::Permissions::from_mode(0o555)).unwrap();
    let recovered = file(&p);
    std::fs::set_permissions(config, std::fs::Permissions::from_mode(0o755)).unwrap();
    assert_eq!(recovered, kept());
    assert_eq!(std::fs::read_to_string(&p.real).unwrap(), "given");
    assert_eq!(std::fs::read_to_string(&p.leftover).unwrap(), "renewed");
    assert_eq!(recorded(&p.state), Some((Kind::File, digest(b"given"))));
}

/// A keychain or sign-in file that can't be read says nothing about whether
/// the person signed out: the renewed sign-in stays, and nothing is put.
#[test]
fn a_sign_in_that_cant_be_read_is_not_taken_for_a_sign_out() {
    let p = place();
    unfinished(&p, Kind::Keychain);
    assert_eq!(keychain_taking(&p, Held::Unknown, true), (kept(), None));
    assert_eq!(std::fs::read_to_string(&p.leftover).unwrap(), "renewed");

    // The person's sign-in file with a second name isn't read.
    let p = place();
    unfinished(&p, Kind::File);
    std::fs::hard_link(&p.real, p.outside.join("second")).unwrap();
    assert_eq!(file(&p), kept());
    assert_eq!(std::fs::read_to_string(&p.real).unwrap(), "given");
    assert_eq!(held_file(&p.outside.join("missing")), Held::Gone);
}

#[test]
fn nothing_is_read_through_what_the_agent_left_in_place_of_its_sign_in() {
    for kind in [Kind::File, Kind::Keychain] {
        // A link to a private file.
        let p = place();
        remember(&p.state, kind, Some(digest(b"given"))).unwrap();
        let private = p.outside.join("private");
        symlink(&private, &p.leftover).unwrap();
        assert_eq!(file(&p), Was::Nothing);
        assert_eq!(keychain(&p, Some(b"given")), (Was::Nothing, None));
        // A second name for it.
        std::fs::remove_file(&p.leftover).unwrap();
        std::fs::hard_link(&private, &p.leftover).unwrap();
        assert_eq!(file(&p), Was::Nothing);
        assert_eq!(keychain(&p, Some(b"given")), (Was::Nothing, None));
        // An empty file, and one too big to be a sign-in.
        std::fs::remove_file(&p.leftover).unwrap();
        std::fs::write(&p.leftover, "").unwrap();
        assert_eq!(keychain(&p, Some(b"given")), (Was::Nothing, None));
        std::fs::write(&p.leftover, vec![b'x'; (LIMIT + 1) as usize]).unwrap();
        assert_eq!(keychain(&p, Some(b"given")), (Was::Nothing, None));
        assert_eq!(file(&p), Was::Nothing);
        assert_eq!(std::fs::read_to_string(&p.real).unwrap(), "given");
        assert_eq!(std::fs::read_to_string(&private).unwrap(), "private");
    }
    // Over the most the caller carries.
    let p = place();
    unfinished(&p, Kind::Keychain);
    let recovered = recover(
        &p.state,
        &p.leftover,
        Kind::Keychain,
        3,
        || Held::Is(digest(b"given")),
        |_| panic!("nothing is put"),
    );
    assert_eq!(was(recovered), Was::Nothing);
}

#[test]
fn with_no_note_of_what_the_run_was_given_nothing_goes_back() {
    let p = place();
    std::fs::write(&p.leftover, "renewed").unwrap();
    assert_eq!(file(&p), Was::Nothing);
    assert_eq!(keychain(&p, Some(b"given")), (Was::Nothing, None));
    // A note for the other kind of sign-in, or one that can't be read.
    remember(&p.state, Kind::Keychain, Some(digest(b"given"))).unwrap();
    assert_eq!(file(&p), Was::Nothing);
    assert_eq!(recorded_given(&p.state, Kind::File), None);
    assert_eq!(
        recorded_given(&p.state, Kind::Keychain),
        Some(digest(b"given"))
    );
    std::fs::write(p.state.join(RECORD), "keychain zz\n").unwrap();
    assert_eq!(keychain(&p, Some(b"given")), (Was::Nothing, None));
    assert_eq!(std::fs::read_to_string(&p.real).unwrap(), "given");
}

#[test]
fn the_note_holds_a_digest_only_and_goes_when_forgotten() {
    let p = place();
    remember(&p.state, Kind::File, Some(digest(b"given"))).unwrap();
    let note = std::fs::read_to_string(p.state.join(RECORD)).unwrap();
    assert!(
        note.starts_with("file ") && !note.contains("given"),
        "{note}"
    );
    assert_eq!(recorded(&p.state), Some((Kind::File, digest(b"given"))));
    forget(&p.state).unwrap();
    assert!(!p.state.join(RECORD).exists());
    forget(&p.state).unwrap();
    // Nothing known leaves no note.
    remember(&p.state, Kind::File, Some(digest(b"given"))).unwrap();
    remember(&p.state, Kind::File, None).unwrap();
    assert!(!p.state.join(RECORD).exists());
}
