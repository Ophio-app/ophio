// SPDX-License-Identifier: Apache-2.0
//! Bounded, app-private clip staging. Only opaque native job ids cross IPC.
use crate::{
    error::{Error, Result},
    ipc::AppCore,
};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use serde::{Deserialize, Serialize};
use serde_json::json;
use tauri::{
    State,
    ipc::{Request, Response},
};

const MAX_BYTES: u64 = 128 * 1024 * 1024;
fn valid_source(size: u64, sha256: &str) -> Result<()> {
    if size == 0
        || size > MAX_BYTES
        || sha256.len() != 64
        || !sha256
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    {
        return Err(Error::Invalid(
            "Choose a clip smaller than 128 MB with a verified fingerprint.",
        ));
    }
    Ok(())
}
fn valid_range(start: u64, end: u64) -> Result<()> {
    if end <= start || end - start > 60_000 || end > protocol::MAX_SAFE_INTEGER {
        return Err(Error::Invalid("Choose a clip range of up to 60 seconds."));
    }
    Ok(())
}
#[tauri::command]
pub async fn clip_begin(core: State<'_, AppCore>, size: u64, sha256: String) -> Result<String> {
    valid_source(size, &sha256)?;
    core.device
        .call("clipBegin", json!({"size":size,"sha256":sha256}))
        .await
}
#[tauri::command]
pub async fn clip_write(core: State<'_, AppCore>, request: Request<'_>) -> Result<()> {
    let id = request
        .headers()
        .get("x-clip-id")
        .and_then(|h| h.to_str().ok())
        .ok_or(Error::Invalid("Missing clip id."))?;
    let bytes = crate::ipc::binary_body(request.body())?;
    core.device
        .call(
            "clipWrite",
            json!({"id":id,"bytes":URL_SAFE_NO_PAD.encode(bytes)}),
        )
        .await
}
#[derive(Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TrimmedClip {
    start_ms: f64,
    end_ms: f64,
    size: u64,
    sha256: String,
}
#[tauri::command]
pub async fn clip_trim(
    core: State<'_, AppCore>,
    id: String,
    start_ms: u64,
    end_ms: u64,
) -> Result<TrimmedClip> {
    valid_range(start_ms, end_ms)?;
    core.device
        .call(
            "clipTrim",
            json!({"id":id,"startMs":start_ms,"endMs":end_ms}),
        )
        .await
}
#[derive(Deserialize)]
struct EncodedBytes {
    bytes: String,
}
#[tauri::command]
pub async fn clip_read(core: State<'_, AppCore>, id: String, offset: u64) -> Result<Response> {
    if offset >= MAX_BYTES {
        return Err(Error::Invalid("Invalid clip offset."));
    }
    let result: EncodedBytes = core
        .device
        .call("clipRead", json!({"id":id,"offset":offset}))
        .await?;
    let bytes = URL_SAFE_NO_PAD
        .decode(result.bytes)
        .map_err(|_| Error::Invalid("The clip could not be read."))?;
    if bytes.len() > protocol::transfer::MAX_CHUNK_BYTES {
        return Err(Error::Invalid("Invalid clip chunk."));
    }
    Ok(Response::new(bytes))
}
#[tauri::command]
pub async fn clip_discard(core: State<'_, AppCore>, id: String) -> Result<()> {
    core.device.call("clipDiscard", json!({"id":id})).await
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn bounds_and_hashes_are_checked_before_native_calls() {
        assert!(valid_source(12, &"a".repeat(64)).is_ok());
        for (size, hash) in [
            (0, "a".repeat(64)),
            (MAX_BYTES + 1, "a".repeat(64)),
            (12, "A".repeat(64)),
            (12, "f".repeat(63)),
        ] {
            assert!(valid_source(size, &hash).is_err());
        }
        assert!(valid_range(2000, 5000).is_ok());
        for (start, end) in [(0, 0), (5000, 2000), (0, 60_001), (u64::MAX - 1, u64::MAX)] {
            assert!(valid_range(start, end).is_err());
        }
    }
}
