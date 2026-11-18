// SPDX-License-Identifier: Apache-2.0
//! Native allocation fixture only: never opens or modifies a clipboard and
//! never creates a window, sends input, starts an app or reads user data.
use super::*;
#[test]
fn native_clipboard_memory_is_unicode_multiline_and_nul_terminated() {
    for text in ["", "Café 日本語\nsecond 😀\r\nthird"] {
        let expected: Vec<u16> = text.encode_utf16().chain(std::iter::once(0)).collect();
        let memory = text_memory(text).unwrap();
        assert_eq!(memory.bytes, expected.len() * size_of::<u16>());
        // SAFETY: this is fixture-owned global memory, sized and filled above.
        let pointer = unsafe { GlobalLock(memory.handle) };
        assert!(!pointer.is_null());
        // Do not print contents even on a failure; only synthetic text exists.
        let matches = unsafe { std::slice::from_raw_parts(pointer.cast::<u16>(), expected.len()) }
            == expected;
        // SAFETY: matches the successful fixture GlobalLock.
        unsafe {
            GlobalUnlock(memory.handle);
        }
        assert!(matches);
    }
}
