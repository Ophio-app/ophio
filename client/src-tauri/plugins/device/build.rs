// SPDX-License-Identifier: Apache-2.0
fn main() {
    // No WebView permissions: key and microphone operations are Rust-only.
    tauri_plugin::Builder::new(&[])
        .android_path("android")
        .build();
}
