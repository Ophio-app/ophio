// SPDX-License-Identifier: Apache-2.0
use async_trait::async_trait;
use media::{TileEncoder, TileState};
use ports::{
    desktop::{CapturedFrame, DamageRect, FrameStream, PixelFormat, PlatformError},
    media::{EncodedOut, MediaEncoder, StillFormat},
};
use protocol::{
    control::TargetGeometry,
    media::{SourceRegion, StreamParams, StreamRegion, parse_tile_frame},
};
use std::{io::Cursor, time::Duration};
use tokio::sync::mpsc;

fn frame(value: u8) -> CapturedFrame {
    CapturedFrame {
        width: 256,
        height: 128,
        stride: 1024,
        format: PixelFormat::Bgra8,
        data: [value, value, value, 255].repeat(256 * 128),
        geometry: TargetGeometry {
            width: 256,
            height: 128,
            scale: 1.0,
            rotation_degrees: 0,
            revision: 1,
        },
        cursor: None,
        damage: None,
    }
}
fn params() -> StreamParams {
    StreamParams {
        max_width: 256,
        max_height: 128,
        max_fps: 120,
        quality: 85,
        window: 1,
        reopen: None,
        region: None,
    }
}
struct Frames(mpsc::Receiver<Result<CapturedFrame, PlatformError>>);
#[async_trait]
impl FrameStream for Frames {
    async fn next_frame(&mut self) -> Result<Option<CapturedFrame>, PlatformError> {
        self.0.recv().await.transpose()
    }
}
async fn next(rx: &mut mpsc::Receiver<EncodedOut>) -> EncodedOut {
    tokio::time::timeout(Duration::from_secs(3), rx.recv())
        .await
        .expect("encoder stalled")
        .unwrap()
}
async fn encoded(rx: &mut mpsc::Receiver<EncodedOut>) -> bytes::Bytes {
    match next(rx).await {
        EncodedOut::Frame(bytes) => bytes,
        other => panic!("expected frame, got {other:?}"),
    }
}

#[test]
fn tiles_roundtrip_scaling_stride_and_colour() {
    let mut f = frame(0);
    for row in f.data.as_chunks_mut::<1024>().0 {
        for pixel in row.as_chunks_mut::<4>().0 {
            pixel.copy_from_slice(&[40, 80, 160, 255]);
        }
    }
    let p = StreamParams {
        max_width: 128,
        max_height: 128,
        ..params()
    };
    let bytes = TileState::default()
        .encode(&f, p, 1, true)
        .unwrap()
        .unwrap();
    let (header, tiles) = parse_tile_frame(&bytes).unwrap();
    assert_eq!((header.enc_width, header.enc_height), (128, 64));
    assert_eq!(header.count, 1);
    let decoded = jpeg_decoder::Decoder::new(Cursor::new(tiles[0].jpeg))
        .decode()
        .unwrap();
    for pixel in decoded.as_chunks::<3>().0 {
        for (actual, expected) in pixel.iter().zip([160u8, 80, 40]) {
            assert!(actual.abs_diff(expected) <= 3);
        }
    }
    let mut state = TileState::default();
    state.encode(&f, p, 1, true).unwrap();
    assert!(state.encode(&f, p, 2, false).unwrap().is_none());
    f.damage = Some(Vec::new());
    assert!(state.encode(&f, p, 2, false).unwrap().is_none());
    f.cursor = Some((255, 127));
    let bytes = state.encode(&f, p, 2, false).unwrap().unwrap();
    let (h, tiles) = parse_tile_frame(&bytes).unwrap();
    assert_eq!(h.cursor, Some((127, 63)));
    assert!(tiles.is_empty());
}

#[test]
fn region_is_cropped_before_scaling() {
    // Left half dark, right half light.
    let mut f = frame(40);
    for row in f.data.as_chunks_mut::<1024>().0 {
        row[512..].fill(200);
    }
    let whole = StreamParams {
        max_width: 128,
        max_height: 128,
        ..params()
    };
    let right = StreamParams {
        region: Some(StreamRegion {
            x: 0.5,
            y: 0.0,
            width: 0.5,
            height: 1.0,
        }),
        ..whole
    };
    let mut state = TileState::default();
    let (h, _) = parse_tile_frame(&state.encode(&f, whole, 1, true).unwrap().unwrap())
        .map(|(h, t)| (h, t.len()))
        .unwrap();
    assert_eq!((h.enc_width, h.enc_height, h.region), (128, 64, None));

    // The right half keeps its own pixels, and a new region is a keyframe.
    f.cursor = Some((200, 64));
    let bytes = state.encode(&f, right, 2, false).unwrap().unwrap();
    let (h, tiles) = parse_tile_frame(&bytes).unwrap();
    assert!(h.keyframe);
    assert_eq!((h.enc_width, h.enc_height), (128, 128));
    assert_eq!((h.src_width, h.src_height), (256, 128));
    assert_eq!(
        h.region,
        Some(SourceRegion {
            x: 128,
            y: 0,
            width: 128,
            height: 128
        })
    );
    assert_eq!(h.cursor, Some((72, 64)));
    assert_eq!(tiles.len(), 1);
    let pixels = jpeg_decoder::Decoder::new(Cursor::new(tiles[0].jpeg))
        .decode()
        .unwrap();
    assert!(pixels.iter().all(|p| p.abs_diff(200) <= 3));

    // Damage and a cursor outside the region send nothing new but the cursor.
    for row in f.data.as_chunks_mut::<1024>().0 {
        row[..512].fill(90);
    }
    f.damage = Some(vec![DamageRect {
        x: 0,
        y: 0,
        width: 128,
        height: 128,
    }]);
    f.cursor = Some((10, 10));
    let (h, tiles) = parse_tile_frame(&state.encode(&f, right, 3, false).unwrap().unwrap())
        .map(|(h, t)| (h, t.len()))
        .unwrap();
    assert_eq!((h.keyframe, h.cursor, tiles), (false, None, 0));
    assert!(state.encode(&f, right, 4, false).unwrap().is_none());

    // Damage that crosses into the region is sent.
    for row in f.data.as_chunks_mut::<1024>().0 {
        row[..520].fill(10);
    }
    f.damage = Some(vec![DamageRect {
        x: 100,
        y: 0,
        width: 30,
        height: 4,
    }]);
    let (_, tiles) = parse_tile_frame(&state.encode(&f, right, 5, false).unwrap().unwrap())
        .map(|(h, t)| (h, t.len()))
        .unwrap();
    assert_eq!(tiles, 1);

    let outside = StreamParams {
        region: Some(StreamRegion {
            x: 0.8,
            y: 0.0,
            width: 0.5,
            height: 1.0,
        }),
        ..whole
    };
    assert!(state.encode(&f, outside, 6, false).is_err());
}

#[test]
fn region_pixels_round_outward_and_stay_inside() {
    let r = StreamRegion {
        x: 0.125,
        y: 0.25,
        width: 0.25,
        height: 0.5,
    };
    assert_eq!(
        r.pixels(1000, 7),
        SourceRegion {
            x: 125,
            y: 1,
            width: 250,
            height: 5
        }
    );
    let edge = StreamRegion {
        x: 0.999_999_9,
        y: 0.0,
        width: 0.000_000_1,
        height: 1.000_000_5,
    };
    assert!(edge.valid());
    assert_eq!(
        edge.pixels(100, 100),
        SourceRegion {
            x: 99,
            y: 0,
            width: 1,
            height: 100
        }
    );
    for bad in [
        StreamRegion { x: -0.1, ..r },
        StreamRegion { width: 0.0, ..r },
        StreamRegion { x: 0.8, ..r },
        StreamRegion { y: f64::NAN, ..r },
    ] {
        assert!(!bad.valid(), "{bad:?}");
    }
}

#[test]
fn still_png_exact_and_invalid_buffers_rejected() {
    let mut f = frame(127);
    let still = TileEncoder
        .encode_still(&f, StillFormat::Png, Some(128))
        .unwrap();
    assert_eq!((still.width, still.height), (128, 64));
    let mut decoder = png::Decoder::new(Cursor::new(still.bytes))
        .read_info()
        .unwrap();
    let mut output = vec![0; decoder.output_buffer_size().unwrap()];
    decoder.next_frame(&mut output).unwrap();
    assert!(output.iter().all(|x| *x == 127));
    f.stride = 3;
    assert!(
        TileEncoder
            .encode_still(&f, StillFormat::Png, None)
            .is_err()
    );
    assert!(
        TileEncoder
            .encode_still(&frame(0), StillFormat::Png, Some(0))
            .is_err()
    );
}

#[tokio::test]
async fn window_coalesces_disjoint_damage_and_ignores_future_acks() {
    let (tx, rx) = mpsc::channel(4);
    let mut stream = TileEncoder.start(Box::new(Frames(rx)), params());
    tx.send(Ok(frame(0))).await.unwrap();
    let first = encoded(&mut stream.output).await;
    let seq = parse_tile_frame(&first).unwrap().0.seq;
    stream.control.ack(seq + 10).await;
    let mut changed = frame(0);
    for y in 0..128 {
        for x in 0..128 {
            let i = (y * 256 + x) * 4;
            changed.data[i..i + 3].fill(80);
        }
    }
    changed.damage = Some(vec![DamageRect {
        x: 0,
        y: 0,
        width: 128,
        height: 128,
    }]);
    tx.send(Ok(changed.clone())).await.unwrap();
    changed.data.fill(220);
    changed.damage = Some(vec![DamageRect {
        x: 128,
        y: 0,
        width: 128,
        height: 128,
    }]);
    tx.send(Ok(changed)).await.unwrap();
    tokio::time::sleep(Duration::from_millis(30)).await;
    assert_eq!(stream.output.len(), 0);
    stream.control.ack(seq).await;
    let bytes = encoded(&mut stream.output).await;
    let (_, tiles) = parse_tile_frame(&bytes).unwrap();
    assert_eq!(tiles.len(), 2);
    for tile in tiles {
        let pixels = jpeg_decoder::Decoder::new(Cursor::new(tile.jpeg))
            .decode()
            .unwrap();
        assert!(pixels.iter().all(|p| p.abs_diff(220) <= 3));
    }
    stream.control.close().await;
}

/// A screen nobody changes is still reported while the source keeps capturing
/// it, marked unchanged or not; a source that goes quiet, as a stalled one
/// does, is not vouched for. An unanswered frame still holds the window shut.
#[tokio::test(start_paused = true)]
async fn an_unchanged_screen_still_says_so_and_a_stalled_one_does_not() {
    let (tx, rx) = mpsc::channel(4);
    let mut stream = TileEncoder.start(Box::new(Frames(rx)), params());
    tx.send(Ok(frame(10))).await.unwrap();
    let first = parse_tile_frame(&encoded(&mut stream.output).await)
        .unwrap()
        .0;
    stream.control.ack(first.seq).await;
    let unchanged = |bytes: &[u8], seq| {
        let (h, tiles) = parse_tile_frame(bytes).unwrap();
        assert!(tiles.is_empty() && !h.keyframe);
        assert_eq!(h.seq, seq);
        assert_eq!((h.enc_width, h.enc_height), (256, 128));
        assert_eq!(h.geometry_revision, first.geometry_revision);
    };
    tokio::time::sleep(Duration::from_secs(5)).await;
    assert_eq!(
        stream.output.len(),
        0,
        "a source that captured nothing was reported unchanged"
    );
    let mut repeated = frame(10);
    tx.send(Ok(repeated.clone())).await.unwrap();
    unchanged(&encoded(&mut stream.output).await, first.seq + 1);
    // Nothing more until that one is answered.
    repeated.damage = Some(Vec::new());
    tx.send(Ok(repeated.clone())).await.unwrap();
    tokio::time::sleep(Duration::from_secs(5)).await;
    assert_eq!(stream.output.len(), 0);
    stream.control.ack(first.seq + 1).await;
    unchanged(&encoded(&mut stream.output).await, first.seq + 2);
    stream.control.ack(first.seq + 2).await;
    for _ in 0..3 {
        tx.send(Ok(repeated.clone())).await.unwrap();
        tokio::time::sleep(Duration::from_millis(300)).await;
    }
    assert_eq!(stream.output.len(), 0, "spoke before a second had passed");
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(
        stream.output.len(),
        0,
        "the second alone vouched for the picture"
    );
    tx.send(Ok(repeated.clone())).await.unwrap();
    unchanged(&encoded(&mut stream.output).await, first.seq + 3);
    stream.control.close().await;
}

#[tokio::test]
async fn keyframe_adjust_geometry_and_end_order() {
    let (tx, rx) = mpsc::channel(1);
    let mut stream = TileEncoder.start(Box::new(Frames(rx)), params());
    tx.send(Ok(frame(10))).await.unwrap();
    let bytes = encoded(&mut stream.output).await;
    let seq = parse_tile_frame(&bytes).unwrap().0.seq;
    stream.control.ack(seq).await;
    stream.control.request_keyframe().await;
    let bytes = encoded(&mut stream.output).await;
    let h = parse_tile_frame(&bytes).unwrap().0;
    assert!(h.keyframe);
    stream.control.ack(h.seq).await;
    stream
        .control
        .adjust(StreamParams {
            max_width: 64,
            max_height: 64,
            ..params()
        })
        .await;
    let bytes = encoded(&mut stream.output).await;
    let h = parse_tile_frame(&bytes).unwrap().0;
    assert!(h.keyframe);
    assert_eq!((h.enc_width, h.enc_height), (64, 32));
    stream.control.ack(h.seq).await;
    let mut f = frame(20);
    f.geometry.revision = 2;
    tx.send(Ok(f)).await.unwrap();
    assert!(
        matches!(next(&mut stream.output).await, EncodedOut::GeometryChanged(g) if g.revision == 2)
    );
    let bytes = encoded(&mut stream.output).await;
    let h = parse_tile_frame(&bytes).unwrap().0;
    assert!(h.keyframe);
    assert_eq!(h.geometry_revision, 2);
    drop(tx);
    assert!(matches!(
        next(&mut stream.output).await,
        EncodedOut::Ended { .. }
    ));
}

#[tokio::test]
async fn bounded_output_pause_resume_close() {
    let (tx, rx) = mpsc::channel(1);
    let mut stream = TileEncoder.start(
        Box::new(Frames(rx)),
        StreamParams {
            window: 16,
            ..params()
        },
    );
    tx.send(Err(PlatformError::TemporarilyUnavailable("locked".into())))
        .await
        .unwrap();
    assert!(matches!(
        next(&mut stream.output).await,
        EncodedOut::Paused { .. }
    ));
    tx.send(Ok(frame(0))).await.unwrap();
    assert!(matches!(
        next(&mut stream.output).await,
        EncodedOut::Resumed
    ));
    let _ = encoded(&mut stream.output).await;
    for n in 1..50 {
        tx.send(Ok(frame(n))).await.unwrap();
    }
    tokio::time::sleep(Duration::from_millis(30)).await;
    assert!(stream.output.len() <= 1);
    assert_eq!(stream.output.max_capacity(), 1);
    stream.control.close().await;
    while stream.output.recv().await.is_some() {}
}

#[test]
fn reconstructed_tiles_match_colour_pattern_with_jpeg_tolerance() {
    let mut f = frame(0);
    for y in 0..f.height as usize {
        for x in 0..f.width as usize {
            let i = y * f.stride as usize + x * 4;
            f.data[i..i + 4].copy_from_slice(&[(x / 8) as u8 * 7, (y / 8) as u8 * 13, 90, 255]);
        }
    }
    let bytes = TileState::default()
        .encode(&f, params(), 1, true)
        .unwrap()
        .unwrap();
    let (h, tiles) = parse_tile_frame(&bytes).unwrap();
    let mut rebuilt = vec![0; h.enc_width as usize * h.enc_height as usize * 3];
    for tile in tiles {
        let mut decoder = jpeg_decoder::Decoder::new(Cursor::new(tile.jpeg));
        let pixels = decoder.decode().unwrap();
        let info = decoder.info().unwrap();
        for y in 0..info.height as usize {
            let start = ((tile.row as usize * h.tile as usize + y) * h.enc_width as usize
                + tile.col as usize * h.tile as usize)
                * 3;
            rebuilt[start..start + info.width as usize * 3].copy_from_slice(
                &pixels[y * info.width as usize * 3..(y + 1) * info.width as usize * 3],
            );
        }
    }
    let mut error = 0u64;
    for (source, decoded) in f
        .data
        .as_chunks::<4>()
        .0
        .iter()
        .zip(rebuilt.as_chunks::<3>().0)
    {
        for (a, b) in [source[2], source[1], source[0]].iter().zip(decoded) {
            error += u64::from(a.abs_diff(*b));
        }
    }
    assert!(error as f64 / (rebuilt.len() as f64) < 3.0);
}

#[tokio::test]
async fn invalid_parameters_end_and_dropping_receiver_drops_capture() {
    let (tx, rx) = mpsc::channel(1);
    let mut stream = TileEncoder.start(
        Box::new(Frames(rx)),
        StreamParams {
            window: 0,
            ..params()
        },
    );
    assert!(matches!(
        next(&mut stream.output).await,
        EncodedOut::Ended { .. }
    ));
    assert!(stream.output.recv().await.is_none());
    tx.closed().await;
    let (tx, rx) = mpsc::channel(1);
    let stream = TileEncoder.start(Box::new(Frames(rx)), params());
    drop(stream.output);
    tokio::time::timeout(Duration::from_secs(1), tx.closed())
        .await
        .unwrap();
}

#[tokio::test]
async fn close_cancels_source_even_when_terminal_output_is_blocked() {
    let (tx, rx) = mpsc::channel(1);
    let mut stream = TileEncoder.start(Box::new(Frames(rx)), params());
    tx.send(Ok(frame(1))).await.unwrap();
    // Leave the encoded frame in the output slot, then make Ended wait for capacity.
    tokio::time::timeout(Duration::from_secs(2), async {
        while stream.output.is_empty() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    stream
        .control
        .adjust(StreamParams {
            window: 0,
            ..params()
        })
        .await;
    stream.control.close().await;
    tokio::time::timeout(Duration::from_secs(1), tx.closed())
        .await
        .unwrap();
    while stream.output.recv().await.is_some() {}
}
