// SPDX-License-Identifier: Apache-2.0
//! Native storage qualification without desktop access or real secret material.
#![cfg(windows)]
use platform_windows::{dpapi, fs};
use std::{
    io::{Read, Write},
    path::PathBuf,
};
struct Directory(PathBuf);
impl Directory {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!(
            "companion-storage-test-{}",
            protocol::ids::HostId::new()
        ));
        fs::private_dir(&path).unwrap();
        Self(path)
    }
}
impl Drop for Directory {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
#[test]
fn dpapi_round_trip_and_tamper_refusal() {
    let plaintext = b"synthetic identity fixture";
    let mut sealed = dpapi::protect(plaintext).unwrap();
    assert!(!sealed.windows(plaintext.len()).any(|w| w == plaintext));
    assert_eq!(&**dpapi::unprotect(&sealed).as_ref().unwrap(), plaintext);
    let last = sealed.len() - 1;
    sealed[last] ^= 1;
    assert!(dpapi::unprotect(&sealed).is_err());
    assert!(dpapi::unprotect(plaintext).is_err());
}
#[test]
fn secured_storage_rejects_links_bounds_and_accidental_replacement() {
    let directory = Directory::new();
    let path = directory.0.join("state");
    let mut file = fs::private_open(&path).unwrap();
    file.write_all(b"fixture").unwrap();
    drop(file);
    assert_eq!(
        &**fs::protected_read(&path, 7).as_ref().unwrap(),
        b"fixture"
    );
    assert!(fs::protected_read(&path, 6).is_err());
    let link = directory.0.join("link");
    std::fs::hard_link(&path, &link).unwrap();
    assert!(fs::private_open(&path).is_err());
    assert!(fs::protected_read(&link, 100).is_err());
    std::fs::remove_file(&link).unwrap();
    assert!(fs::atomic_write(&path, b"replacement", false).is_err());
    // The refused write leaves no temporary file beside the state.
    assert_eq!(std::fs::read_dir(&directory.0).unwrap().count(), 1);
    fs::atomic_write(&path, b"replacement", true).unwrap();
    assert_eq!(std::fs::read_dir(&directory.0).unwrap().count(), 1);
    let mut actual = Vec::new();
    fs::private_open(&path)
        .unwrap()
        .read_to_end(&mut actual)
        .unwrap();
    assert_eq!(actual, b"replacement");
}
#[test]
fn identity_store_seals_creation_and_rotation() {
    let directory = Directory::new();
    let path = directory.0.join("identity");
    let store = dpapi::SecretStore::new(path.clone());
    assert!(store.load().unwrap().is_none());
    store.save(b"synthetic first identity", false).unwrap();
    assert!(store.save(b"must not replace", false).is_err());
    assert_eq!(
        &**store.load().unwrap().as_ref().unwrap(),
        b"synthetic first identity"
    );
    store.save(b"synthetic rotation", true).unwrap();
    assert_eq!(
        &**store.load().unwrap().as_ref().unwrap(),
        b"synthetic rotation"
    );
    assert!(
        !std::fs::read(path)
            .unwrap()
            .windows(9)
            .any(|w| w == b"synthetic")
    );
}
/// Run by hand, as two Windows accounts signed in with their passwords, with
/// `OPHIO_DPAPI_FIXTURE` naming a file both can read. `seal` writes a sealed
/// synthetic secret there; `open` and `refuse` copy it into a store this
/// account owns, so its protection admits this account, and check it opens
/// for the account that sealed it and for no other.
#[test]
#[ignore = "qualification across two accounts, run by hand"]
fn sealed_fixture_opens_only_for_its_account() {
    const PLAINTEXT: &[u8] = b"synthetic cross-account fixture";
    let fixture = PathBuf::from(std::env::var_os("OPHIO_DPAPI_FIXTURE").unwrap());
    let step = std::env::var("OPHIO_DPAPI_STEP").unwrap();
    if step == "seal" {
        std::fs::write(&fixture, dpapi::protect(PLAINTEXT).unwrap()).unwrap();
        return;
    }
    // This account's own secrets open, so a refusal is not DPAPI being
    // unavailable to this logon.
    let own = dpapi::protect(b"own").unwrap();
    assert_eq!(&**dpapi::unprotect(&own).unwrap(), b"own");
    let directory = Directory::new();
    let path = directory.0.join("identity");
    let sealed = std::fs::read(&fixture).unwrap();
    fs::atomic_write(&path, &sealed, false).unwrap();
    assert_eq!(*fs::protected_read(&path, sealed.len()).unwrap(), sealed);
    let loaded = dpapi::SecretStore::new(path).load();
    match step.as_str() {
        "open" => assert_eq!(&**loaded.unwrap().unwrap(), PLAINTEXT),
        "refuse" => assert!(loaded.is_err(), "another account opened the fixture"),
        other => panic!("unknown step {other}"),
    }
}
