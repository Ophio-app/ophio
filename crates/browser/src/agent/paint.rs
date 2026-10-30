// SPDX-License-Identifier: Apache-2.0
//! Painting over the fields Ophio filled in pictures of a page, so a password
//! a page shows, or a "show password" button reveals, is never seen.
use std::io::Cursor;

/// A filled field's place on the page, in page pixels.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(super) struct Rect {
    pub x: f64,
    pub y: f64,
    pub w: f64,
    pub h: f64,
}

/// A decoded picture, four bytes a pixel.
pub(super) struct Rgba {
    pub width: u32,
    pub height: u32,
    pub data: Vec<u8>,
}

/// Page pixels of margin painted around each field, for its border and focus
/// ring, and for a field that moves a little between looks.
const MARGIN: f64 = 3.;
pub(super) const FILL: [u8; 4] = [38, 38, 44, 255];

pub(super) fn decode(png: &[u8]) -> Option<Rgba> {
    let mut decoder = png::Decoder::new(Cursor::new(png));
    decoder.set_transformations(png::Transformations::normalize_to_color8());
    let mut reader = decoder.read_info().ok()?;
    let mut buffer = vec![0; reader.output_buffer_size()?];
    let info = reader.next_frame(&mut buffer).ok()?;
    buffer.truncate(info.buffer_size());
    let pixels = (info.width as usize).checked_mul(info.height as usize)?;
    let data = match info.color_type {
        png::ColorType::Rgba => buffer,
        png::ColorType::Rgb => buffer
            .as_chunks::<3>()
            .0
            .iter()
            .flat_map(|[r, g, b]| [*r, *g, *b, 255])
            .collect(),
        png::ColorType::GrayscaleAlpha => buffer
            .as_chunks::<2>()
            .0
            .iter()
            .flat_map(|[g, a]| [*g, *g, *g, *a])
            .collect(),
        png::ColorType::Grayscale => buffer.iter().flat_map(|g| [*g, *g, *g, 255]).collect(),
        png::ColorType::Indexed => return None,
    };
    (data.len() == pixels * 4).then_some(Rgba {
        width: info.width,
        height: info.height,
        data,
    })
}

pub(super) fn encode(picture: &Rgba) -> Option<Vec<u8>> {
    let mut bytes = Vec::new();
    {
        let mut encoder = png::Encoder::new(&mut bytes, picture.width, picture.height);
        encoder.set_color(png::ColorType::Rgba);
        encoder.set_depth(png::BitDepth::Eight);
        let mut writer = encoder.write_header().ok()?;
        writer.write_image_data(&picture.data).ok()?;
    }
    Some(bytes)
}

/// Paints over `rects`, given in page pixels, where `scale` is picture pixels
/// per page pixel.
pub(super) fn cover(picture: &mut Rgba, rects: &[Rect], scale: f64) {
    let (width, height) = (f64::from(picture.width), f64::from(picture.height));
    for rect in rects {
        let left = ((rect.x - MARGIN) * scale).floor().clamp(0., width) as usize;
        let top = ((rect.y - MARGIN) * scale).floor().clamp(0., height) as usize;
        let right = ((rect.x + rect.w + MARGIN) * scale).ceil().clamp(0., width) as usize;
        let bottom = ((rect.y + rect.h + MARGIN) * scale)
            .ceil()
            .clamp(0., height) as usize;
        for y in top..bottom {
            let row = y * picture.width as usize * 4;
            for x in left..right {
                picture.data[row + x * 4..row + x * 4 + 4].copy_from_slice(&FILL);
            }
        }
    }
}

/// The picture at half size, each pixel the average of four.
pub(super) fn halve(picture: &Rgba) -> Rgba {
    let width = (picture.width / 2).max(1);
    let height = (picture.height / 2).max(1);
    let source = picture.width as usize * 4;
    let mut data = Vec::with_capacity(width as usize * height as usize * 4);
    for y in 0..height as usize {
        for x in 0..width as usize {
            let at = |dx: usize, dy: usize| {
                let sx = (x * 2 + dx).min(picture.width as usize - 1);
                let sy = (y * 2 + dy).min(picture.height as usize - 1);
                sy * source + sx * 4
            };
            for channel in 0..4 {
                let sum: u32 = [at(0, 0), at(1, 0), at(0, 1), at(1, 1)]
                    .iter()
                    .map(|i| u32::from(picture.data[i + channel]))
                    .sum();
                data.push((sum / 4) as u8);
            }
        }
    }
    Rgba {
        width,
        height,
        data,
    }
}

/// A screenshot with `rects` painted over, as a PNG no larger than `limit`
/// where halving can make it so, and its picture pixels per page pixel.
pub(super) fn picture(
    png: &[u8],
    rects: &[Rect],
    page_width: u32,
    limit: usize,
) -> Option<(Vec<u8>, f64)> {
    let mut picture = decode(png)?;
    let mut scale = f64::from(picture.width) / f64::from(page_width.max(1));
    cover(&mut picture, rects, scale);
    let mut bytes = encode(&picture)?;
    while bytes.len() > limit && picture.width > 160 {
        picture = halve(&picture);
        scale /= 2.;
        bytes = encode(&picture)?;
    }
    Some((bytes, scale))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn solid(width: u32, height: u32, pixel: [u8; 4]) -> Rgba {
        Rgba {
            width,
            height,
            data: pixel.repeat((width * height) as usize),
        }
    }

    #[test]
    fn covers_fields_with_their_margin_and_nothing_else() {
        let mut picture = solid(40, 20, [255; 4]);
        cover(
            &mut picture,
            &[Rect {
                x: 10.,
                y: 5.,
                w: 4.,
                h: 2.,
            }],
            1.,
        );
        let at = |x: usize, y: usize| &picture.data[(y * 40 + x) * 4..(y * 40 + x) * 4 + 4];
        assert_eq!(at(7, 2), FILL);
        assert_eq!(at(16, 9), FILL);
        assert_eq!(at(6, 2), [255; 4]);
        assert_eq!(at(17, 10), [255; 4]);
    }

    #[test]
    fn a_field_reaching_past_the_edges_is_clamped() {
        let mut picture = solid(10, 10, [0; 4]);
        cover(
            &mut picture,
            &[Rect {
                x: -50.,
                y: 8.,
                w: 500.,
                h: 50.,
            }],
            1.,
        );
        // Rows from 8 less the margin.
        assert!(picture.data[(5 * 10) * 4..].chunks(4).all(|p| p == FILL));
        assert!(picture.data[..(5 * 10) * 4].chunks(4).all(|p| p == [0; 4]));
    }

    #[test]
    fn a_large_picture_is_halved_until_it_fits_and_stays_covered() {
        // Noise doesn't compress, so the first encoding is too large.
        let mut seed = 7u32;
        let data = (0..640 * 400 * 4)
            .map(|_| {
                seed = seed.wrapping_mul(1_103_515_245).wrapping_add(12345);
                (seed >> 16) as u8
            })
            .collect();
        let png = encode(&Rgba {
            width: 640,
            height: 400,
            data,
        })
        .unwrap();
        let field = Rect {
            x: 100.,
            y: 100.,
            w: 200.,
            h: 40.,
        };
        let (smaller, scale) = picture(&png, &[field], 640, png.len() / 3).unwrap();
        assert!(smaller.len() <= png.len() / 3);
        assert!(scale < 1.);
        let decoded = decode(&smaller).unwrap();
        let (x, y) = ((200. * scale) as usize, (120. * scale) as usize);
        let i = (y * decoded.width as usize + x) * 4;
        assert_eq!(decoded.data[i..i + 4], FILL);
    }
}
