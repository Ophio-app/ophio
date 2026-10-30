// SPDX-License-Identifier: Apache-2.0
//! Bounded, latest-frame-wins TDV1 desktop encoding.
mod image;
mod stream;
mod tiles;

use ports::desktop::{CapturedFrame, FrameStream, PlatformError};
use ports::media::{EncodedStill, EncodedStream, MediaEncoder, StillFormat};
use protocol::media::StreamParams;
pub use tiles::{TILE_EDGE, TileState};

#[derive(Debug, Default, Clone, Copy)]
pub struct TileEncoder;

impl MediaEncoder for TileEncoder {
    fn start(&self, frames: Box<dyn FrameStream>, params: StreamParams) -> EncodedStream {
        stream::start(frames, params)
    }

    fn encode_still(
        &self,
        frame: &CapturedFrame,
        format: StillFormat,
        max_dim: Option<u32>,
    ) -> Result<EncodedStill, PlatformError> {
        let bound = max_dim.unwrap_or(u16::MAX.into()).min(u16::MAX.into());
        let (width, height, data) = image::rgb(frame, bound, bound)?;
        let (bytes, media_type) = match format {
            StillFormat::Jpeg { quality } => {
                if !(1..=100).contains(&quality) {
                    return Err(image::invalid("invalid JPEG quality"));
                }
                (
                    image::jpeg(&data, width as u16, height as u16, quality)?,
                    "image/jpeg",
                )
            }
            StillFormat::Png => {
                let mut bytes = Vec::new();
                {
                    let mut encoder = png::Encoder::new(&mut bytes, width, height);
                    encoder.set_color(png::ColorType::Rgb);
                    encoder.set_depth(png::BitDepth::Eight);
                    let mut writer = encoder
                        .write_header()
                        .map_err(|_| image::invalid("PNG header failed"))?;
                    writer
                        .write_image_data(&data)
                        .map_err(|_| image::invalid("PNG encoding failed"))?;
                }
                (bytes, "image/png")
            }
        };
        Ok(EncodedStill {
            bytes: bytes.into(),
            media_type,
            width,
            height,
        })
    }
}
