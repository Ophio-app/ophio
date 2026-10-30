// SPDX-License-Identifier: Apache-2.0
//! The phone reads binary frames in TypeScript (client/src/protocol/binary.ts)
//! and the companion in Rust. Both are held to the same cases, well-formed and
//! not, so the two readers cannot drift apart.
use protocol::{
    media::{TileFrameError, parse_tile_frame},
    terminal::split_output_frame,
};
use serde_json::{Value, json};

fn cases() -> Value {
    serde_json::from_str(include_str!("fixtures/binary-frames.json")).unwrap()
}
fn bytes(hex: &str) -> Vec<u8> {
    (0..hex.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&hex[i..i + 2], 16).unwrap())
        .collect()
}
fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

#[test]
fn tile_frames_read_as_the_shared_cases_say() {
    for case in cases()["tile_frames"].as_array().unwrap() {
        let name = case["name"].as_str().unwrap();
        let data = bytes(case["hex"].as_str().unwrap());
        match (parse_tile_frame(&data), case.get("error")) {
            (Ok((header, tiles)), None) => {
                let mut read = json!({
                    "keyframe": header.keyframe,
                    "cursor": header.cursor.map(|(x, y)| [x, y]),
                    "tile": header.tile,
                    "seq": header.seq,
                    "geometry_revision": header.geometry_revision,
                    "enc_width": header.enc_width,
                    "enc_height": header.enc_height,
                    "src_width": header.src_width,
                    "src_height": header.src_height,
                    "count": header.count,
                    "tiles": tiles.iter().map(|t| json!([t.col, t.row, hex(t.jpeg)])).collect::<Vec<_>>(),
                });
                if let Some(r) = header.region {
                    read["region"] = json!([r.x, r.y, r.width, r.height]);
                }
                assert_eq!(read, case["frame"], "{name}");
            }
            (Err(error), Some(expected)) => {
                let kind = match error {
                    TileFrameError::Short => "short",
                    TileFrameError::Magic => "magic",
                    TileFrameError::Truncated(_) => "truncated",
                    TileFrameError::Trailing => "trailing",
                    TileFrameError::Region => "region",
                };
                assert_eq!(kind, expected, "{name}");
            }
            (read, _) => panic!("{name}: {read:?}"),
        }
    }
}

#[test]
fn terminal_frames_read_as_the_shared_cases_say() {
    for case in cases()["terminal_frames"].as_array().unwrap() {
        let name = case["name"].as_str().unwrap();
        let data = bytes(case["hex"].as_str().unwrap());
        let read = split_output_frame(&data).map(|(offset, output)| (offset, hex(output)));
        let expected = case["offset"]
            .as_u64()
            .map(|offset| (offset, case["bytes"].as_str().unwrap().to_owned()));
        assert_eq!(read, expected, "{name}");
    }
}
