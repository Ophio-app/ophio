// SPDX-License-Identifier: Apache-2.0
//! Channel messages reach the page in the script that carries them.
//!
//! Tauri numbers each message on a channel, and the page's `Channel` hands
//! them over strictly in that order. A message of 8 KB of JSON or 1 KB of
//! bytes is not carried in the script: the page is asked to fetch it, and a
//! failed fetch is only logged. On Android that fetch is retried by another
//! route, which finds the message already taken. Its number never arrives, so
//! the page holds every later message for good. A link then kept signing in
//! to the computer while the app it served showed Offline until restarted.
//!
//! [`deliver`] takes over every channel message and evaluates it whole, with
//! the same number, so nothing the page waits for can go missing.
use base64::{Engine, engine::general_purpose::STANDARD};
use tauri::{
    Runtime, Webview,
    ipc::{CallbackFn, InvokeResponseBody},
};

/// The channel interceptor: every message is consumed here.
pub fn deliver<R: Runtime>(
    webview: &Webview<R>,
    callback: CallbackFn,
    index: usize,
    body: &InvokeResponseBody,
) -> bool {
    // An evaluation fails only once the page is gone, and nothing is left
    // to receive the message.
    let _ = webview.eval(script(callback, index, body));
    true
}

/// The script that hands message `index` of a channel to the page.
fn script(callback: CallbackFn, index: usize, body: &InvokeResponseBody) -> String {
    let message = match body {
        InvokeResponseBody::Json(json) => json.clone(),
        InvokeResponseBody::Raw(bytes) => format!(
            "Uint8Array.from(atob(\"{}\"), (c) => c.charCodeAt(0)).buffer",
            STANDARD.encode(bytes)
        ),
    };
    format!(
        "window.__TAURI_INTERNALS__.runCallback({}, {{ message: {message}, index: {index} }})",
        callback.0
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_large_message_is_carried_in_its_script() {
        let text = format!(
            "{{\"type\":\"text\",\"text\":\"{}\"}}",
            "a".repeat(64 * 1024)
        );
        let js = script(CallbackFn(7), 3, &InvokeResponseBody::Json(text.clone()));
        assert_eq!(
            js,
            format!("window.__TAURI_INTERNALS__.runCallback(7, {{ message: {text}, index: 3 }})")
        );
        assert!(!js.contains("invoke("), "no message waits on a fetch");
    }

    #[test]
    fn bytes_of_any_size_arrive_as_an_array_buffer() {
        for len in [0, 1, 1023, 1024, 256 * 1024] {
            let bytes: Vec<u8> = (0..len).map(|i| (i % 256) as u8).collect();
            let js = script(CallbackFn(9), 0, &InvokeResponseBody::Raw(bytes.clone()));
            let encoded = js
                .split('"')
                .nth(1)
                .expect("the bytes are quoted in the script");
            assert_eq!(STANDARD.decode(encoded).unwrap(), bytes);
            assert!(js.starts_with(
                "window.__TAURI_INTERNALS__.runCallback(9, { message: Uint8Array.from(atob(\""
            ));
            assert!(js.ends_with(").buffer, index: 0 })"));
        }
    }
}
