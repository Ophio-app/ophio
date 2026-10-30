// SPDX-License-Identifier: Apache-2.0
//! Binary frames: TDV1 tile deltas and terminal output. A frame that parses
//! rebuilds into one that parses the same.
#![no_main]
use libfuzzer_sys::fuzz_target;
use protocol::{media, terminal};

fuzz_target!(|data: &[u8]| {
    if let Ok((header, tiles)) = media::parse_tile_frame(data) {
        assert_eq!(usize::from(header.count), tiles.len());
        let parts: Vec<_> = tiles.iter().map(|t| (t.col, t.row, t.jpeg)).collect();
        let rebuilt = media::build_tile_frame(header, &parts);
        let (again, again_tiles) = media::parse_tile_frame(&rebuilt).expect("rebuilt frame parses");
        assert_eq!(again, header);
        assert_eq!(again_tiles, tiles);
    }
    if let Some((offset, bytes)) = terminal::split_output_frame(data) {
        assert_eq!(terminal::output_frame(offset, bytes), data);
    }
});
