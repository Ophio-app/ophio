// SPDX-License-Identifier: Apache-2.0
use fast_image_resize::{PixelType, ResizeAlg, ResizeOptions, Resizer, images::Image};
use ports::desktop::{CapturedFrame, PixelFormat, PlatformError};
use protocol::media::SourceRegion;

pub(crate) const MAX_PIXELS: u64 = 64 * 1024 * 1024;

pub(crate) fn invalid(message: &str) -> PlatformError {
    PlatformError::Native(message.to_owned())
}

pub(crate) fn dimensions(
    w: u32,
    h: u32,
    max_w: u32,
    max_h: u32,
) -> Result<(u32, u32), PlatformError> {
    if w == 0 || h == 0 || max_w == 0 || max_h == 0 || u64::from(w) * u64::from(h) > MAX_PIXELS {
        return Err(invalid("invalid image dimensions"));
    }
    let ratio = (max_w as f64 / w as f64)
        .min(max_h as f64 / h as f64)
        .min(1.0);
    Ok((
        (w as f64 * ratio).floor().max(1.0) as u32,
        (h as f64 * ratio).floor().max(1.0) as u32,
    ))
}

pub(crate) fn rgb(
    frame: &CapturedFrame,
    max_w: u32,
    max_h: u32,
) -> Result<(u32, u32, Vec<u8>), PlatformError> {
    let whole = SourceRegion {
        x: 0,
        y: 0,
        width: frame.width,
        height: frame.height,
    };
    rgb_part(frame, whole, max_w, max_h)
}

/// `rgb` for one part of the frame, scaled on its own so a small part keeps
/// its pixels.
pub(crate) fn rgb_part(
    frame: &CapturedFrame,
    part: SourceRegion,
    max_w: u32,
    max_h: u32,
) -> Result<(u32, u32, Vec<u8>), PlatformError> {
    dimensions(frame.width, frame.height, max_w, max_h)?;
    if u64::from(part.x) + u64::from(part.width) > u64::from(frame.width)
        || u64::from(part.y) + u64::from(part.height) > u64::from(frame.height)
    {
        return Err(invalid("region outside the image"));
    }
    let (w, h) = dimensions(part.width, part.height, max_w, max_h)?;
    let row = frame.width as usize * 4;
    let required = (frame.height as usize - 1)
        .checked_mul(frame.stride as usize)
        .and_then(|n| n.checked_add(row))
        .ok_or_else(|| invalid("image size overflow"))?;
    if (frame.stride as usize) < row || frame.data.len() < required {
        return Err(invalid("truncated capture buffer or invalid stride"));
    }
    let bgr = matches!(frame.format, PixelFormat::Bgra8 | PixelFormat::Bgrx8);
    let mut data = Vec::with_capacity(part.width as usize * part.height as usize * 3);
    let left = part.x as usize * 4;
    let right = left + part.width as usize * 4;
    for y in part.y as usize..(part.y + part.height) as usize {
        let start = y * frame.stride as usize;
        for pixel in frame.data[start + left..start + right].as_chunks::<4>().0 {
            if bgr {
                data.extend_from_slice(&[pixel[2], pixel[1], pixel[0]]);
            } else {
                data.extend_from_slice(&pixel[..3]);
            }
        }
    }
    if (w, h) == (part.width, part.height) {
        return Ok((w, h, data));
    }
    let source = Image::from_vec_u8(part.width, part.height, data, PixelType::U8x3)
        .map_err(|_| invalid("invalid resize source"))?;
    let mut dest = Image::new(w, h, PixelType::U8x3);
    // Nearest-neighbour preserves sharp desktop pixels and makes damage mapping exact.
    Resizer::new()
        .resize(
            &source,
            &mut dest,
            &ResizeOptions::new().resize_alg(ResizeAlg::Nearest),
        )
        .map_err(|_| invalid("image resize failed"))?;
    Ok((w, h, dest.into_vec()))
}

pub(crate) fn jpeg(
    data: &[u8],
    width: u16,
    height: u16,
    quality: u8,
) -> Result<Vec<u8>, PlatformError> {
    #[cfg(all(feature = "turbojpeg", target_os = "linux"))]
    {
        let image = turbojpeg::Image {
            pixels: data,
            width: width.into(),
            pitch: width as usize * 3,
            height: height.into(),
            format: turbojpeg::PixelFormat::RGB,
        };
        turbojpeg::compress(image, quality.into(), turbojpeg::Subsamp::None)
            .map(|buffer| buffer.to_vec())
            .map_err(|_| invalid("JPEG compression failed"))
    }
    #[cfg(not(all(feature = "turbojpeg", target_os = "linux")))]
    {
        let mut output = Vec::new();
        let mut encoder = jpeg_encoder::Encoder::new(&mut output, quality);
        // 4:4:4 avoids coloured text bleeding into its background.
        encoder.set_sampling_factor(jpeg_encoder::SamplingFactor::F_1_1);
        encoder
            .encode(data, width, height, jpeg_encoder::ColorType::Rgb)
            .map_err(|_| invalid("JPEG compression failed"))?;
        Ok(output)
    }
}
