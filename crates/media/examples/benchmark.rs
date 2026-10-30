// SPDX-License-Identifier: Apache-2.0
//! Reproducible synthetic desktop workloads; never captures the local desktop.
use media::TileState;
use ports::desktop::{CapturedFrame, DamageRect, PixelFormat};
use protocol::{control::TargetGeometry, media::StreamParams};
use std::time::Instant;
fn main() {
    let photo = std::env::args().nth(1).map(|path| {
        let mut decoder = jpeg_decoder::Decoder::new(std::io::BufReader::new(
            std::fs::File::open(path).expect("photo file"),
        ));
        let pixels = decoder.decode().expect("photo JPEG");
        let info = decoder.info().expect("photo metadata");
        assert_eq!(info.pixel_format, jpeg_decoder::PixelFormat::RGB24);
        fast_image_resize::images::Image::from_vec_u8(
            info.width.into(),
            info.height.into(),
            pixels,
            fast_image_resize::PixelType::U8x3,
        )
        .expect("photo image")
    });
    println!("workload,size,quality,keyframe_ms,delta_ms,keyframe_bytes");
    for (w, h) in [(1920u32, 1080u32), (2560, 1440)] {
        for kind in if photo.is_some() {
            vec!["nasa_photo"]
        } else {
            vec!["text_ui", "code_editor", "photo_like"]
        } {
            let mut pixels = Vec::with_capacity((w * h * 4) as usize);
            let mut noise = 1234567u32;
            for y in 0..h {
                for x in 0..w {
                    noise ^= noise << 13;
                    noise ^= noise >> 17;
                    noise ^= noise << 5;
                    let p = match kind {
                        "text_ui" => {
                            let ink = y % 24 < 12
                                && x % 9 < 5
                                && ((x / 9 * 11 + y / 24 * 7) >> (y % 7)) & 1 == 1;
                            if y < 60 {
                                [45, 55, 70]
                            } else if x < 240 {
                                [205, 210, 215]
                            } else if ink {
                                [25, 30, 40]
                            } else {
                                [240, 242, 245]
                            }
                        }
                        "code_editor" => {
                            let ink = y % 20 < 11
                                && x % 8 < 5
                                && ((x / 8 * 19 + y / 20 * 13) >> (y % 5)) & 1 == 1;
                            if ink {
                                if x / 80 % 2 == 0 {
                                    [190, 130, 80]
                                } else {
                                    [90, 160, 210]
                                }
                            } else {
                                [25, 28, 34]
                            }
                        }
                        _ => {
                            let n = (noise % 23) as f64;
                            let a = (x as f64 / 170.0).sin() * 35.0;
                            let b = (y as f64 / 110.0).cos() * 45.0;
                            [
                                (110.0 + a + b + n) as u8,
                                (110.0 + b + n) as u8,
                                (110.0 + a + n) as u8,
                            ]
                        }
                    };
                    pixels.extend_from_slice(&[p[0], p[1], p[2], 255]);
                }
            }
            if let Some(source) = &photo {
                let mut image =
                    fast_image_resize::images::Image::new(w, h, fast_image_resize::PixelType::U8x3);
                fast_image_resize::Resizer::new()
                    .resize(
                        source,
                        &mut image,
                        &fast_image_resize::ResizeOptions::new().resize_alg(
                            fast_image_resize::ResizeAlg::Convolution(
                                fast_image_resize::FilterType::Bilinear,
                            ),
                        ),
                    )
                    .unwrap();
                pixels.clear();
                for p in image.buffer().as_chunks::<3>().0 {
                    pixels.extend_from_slice(&[p[0], p[1], p[2], 255]);
                }
            }
            let frame = CapturedFrame {
                width: w,
                height: h,
                stride: w * 4,
                format: PixelFormat::Rgba8,
                data: pixels,
                geometry: TargetGeometry {
                    width: w,
                    height: h,
                    scale: 1.0,
                    rotation_degrees: 0,
                    revision: 1,
                },
                cursor: None,
                damage: None,
            };
            for quality in [60, 75, 85] {
                let p = StreamParams {
                    max_width: w as u16,
                    max_height: h as u16,
                    quality,
                    max_fps: 20,
                    window: 2,
                    reopen: None,
                    region: None,
                };
                let mut state = TileState::default();
                state.encode(&frame, p, 1, true).unwrap();
                let mut times = Vec::new();
                let mut delta_times = Vec::new();
                let mut bytes = 0;
                for run in 0..9 {
                    let start = Instant::now();
                    bytes = state
                        .encode(&frame, p, run * 2 + 2, true)
                        .unwrap()
                        .unwrap()
                        .len();
                    times.push(start.elapsed().as_secs_f64() * 1000.0);
                    let mut delta = frame.clone();
                    for y in 100..116 {
                        for x in 100..116 {
                            let i = ((y * w + x) * 4) as usize;
                            delta.data[i..i + 3].fill(240);
                        }
                    }
                    delta.damage = Some(vec![DamageRect {
                        x: 100,
                        y: 100,
                        width: 16,
                        height: 16,
                    }]);
                    let start = Instant::now();
                    state.encode(&delta, p, run * 2 + 3, false).unwrap();
                    delta_times.push(start.elapsed().as_secs_f64() * 1000.0);
                }
                times.sort_by(f64::total_cmp);
                delta_times.sort_by(f64::total_cmp);
                println!(
                    "{kind},{w}x{h},{quality},{:.3},{:.3},{bytes}",
                    times[4], delta_times[4]
                );
            }
        }
    }
}
