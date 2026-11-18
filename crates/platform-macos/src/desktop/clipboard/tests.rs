// SPDX-License-Identifier: Apache-2.0
//! Native fixture uses only an unnamed, private pasteboard. It never requests
//! generalPasteboard, permissions, desktop input or any installed application.
use super::*;
use std::time::Duration;

struct Private(Object);
impl Private {
    fn new() -> Self {
        // SAFETY: system factory returns a private pasteboard unique to this test.
        let board = unsafe {
            object(
                objc_getClass(c"NSPasteboard".as_ptr()),
                c"pasteboardWithUniqueName",
            )
        };
        assert!(!board.is_null());
        Self(board)
    }
    fn contains(&self, text: &str) -> bool {
        let kind = StringObject::new("public.utf8-plain-text").unwrap();
        let expected = StringObject::new(text).unwrap();
        // SAFETY: only this private fixture's supplied text is accessed; all
        // NSStrings are live for the pool's lifetime and the ABIs match selectors.
        unsafe {
            let read: unsafe extern "C" fn(Object, Object, Object) -> Object =
                std::mem::transmute(objc_msgSend as *const ());
            let actual = read(self.0, sel_registerName(c"stringForType:".as_ptr()), kind.0);
            let equal: unsafe extern "C" fn(Object, Object, Object) -> i8 =
                std::mem::transmute(objc_msgSend as *const ());
            !actual.is_null()
                && equal(
                    actual,
                    sel_registerName(c"isEqualToString:".as_ptr()),
                    expected.0,
                ) != 0
        }
    }
}
impl Drop for Private {
    fn drop(&mut self) {
        // SAFETY: releases only the unique fixture pasteboard server allocation.
        unsafe {
            let send: unsafe extern "C" fn(Object, Object) =
                std::mem::transmute(objc_msgSend as *const ());
            send(self.0, sel_registerName(c"releaseGlobally".as_ptr()));
        }
    }
}
#[test]
fn private_pasteboard_unicode_multiline_and_concurrent_replacement() {
    let _pool = Pool::new().unwrap();
    let board = Private::new();
    let text = "Café 日本語\nline 😀\r\nfinal";
    let before = replace_on(board.0, text, Instant::now() + Duration::from_secs(2)).unwrap();
    assert!(board.contains(text));
    check_on(board.0, Some(before)).unwrap();
    let after = replace_on(
        board.0,
        "fixture concurrent copy",
        Instant::now() + Duration::from_secs(2),
    )
    .unwrap();
    assert_ne!(before, after);
    assert!(check_on(board.0, Some(before)).is_err());
    assert!(board.contains("fixture concurrent copy"));
    check_on(board.0, Some(after)).unwrap();
}
#[test]
fn private_pasteboard_expired_write_preserves_existing_fixture() {
    let _pool = Pool::new().unwrap();
    let board = Private::new();
    let generation = replace_on(
        board.0,
        "fixture prior",
        Instant::now() + Duration::from_secs(2),
    )
    .unwrap();
    assert!(
        replace_on(
            board.0,
            "must not replace",
            Instant::now() - Duration::from_secs(1)
        )
        .is_err()
    );
    check_on(board.0, Some(generation)).unwrap();
    assert!(board.contains("fixture prior"));
}
