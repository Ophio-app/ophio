fn main() {
    // No WebView permissions: key and microphone operations are Rust-only.
    tauri_plugin::Builder::new(&[])
        .android_path("android")
        .ios_path("ios")
        .build();
}
