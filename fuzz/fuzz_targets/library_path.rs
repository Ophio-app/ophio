// SPDX-License-Identifier: Apache-2.0
//! Library paths a phone sends. An accepted path is already in normal form
//! and names nothing outside its root.
#![no_main]
use libfuzzer_sys::fuzz_target;

fuzz_target!(|path: &str| {
    let Ok(normal) = library::normalize(path) else {
        return;
    };
    assert_eq!(library::normalize(&normal).ok().as_ref(), Some(&normal));
    assert!(!normal.starts_with('/'));
    assert!(!normal.contains('\\') && !normal.contains('\0'));
    assert!(normal.split('/').all(|part| part != ".." && part != "."));
});
