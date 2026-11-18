// SPDX-License-Identifier: Apache-2.0
//! DXGI pointer composition. Coordinates are the API's pointer top-left in the
//! oriented output image. Pixels are never included in Debug output.
use ports::desktop::{DamageRect, PlatformError};
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShapeKind {
    Color,
    MaskedColor,
    Monochrome,
}
#[derive(Clone)]
pub struct Shape {
    pub kind: ShapeKind,
    pub width: u32,
    pub height: u32,
    pub pitch: u32,
    pub bytes: Vec<u8>,
}
impl std::fmt::Debug for Shape {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CursorShape")
            .field("width", &self.width)
            .field("height", &self.height)
            .finish_non_exhaustive()
    }
}
impl Shape {
    /// `height` is the visible height; monochrome buffers contain both AND and
    /// XOR planes, each with that height and the same byte pitch.
    pub fn overlay(
        &self,
        frame: &mut [u8],
        width: u32,
        height: u32,
        x: i32,
        y: i32,
    ) -> Result<Option<DamageRect>, PlatformError> {
        let row = if self.kind == ShapeKind::Monochrome {
            self.width.div_ceil(8)
        } else {
            self.width
                .checked_mul(4)
                .ok_or_else(super::geometry::invalid)?
        };
        let required = u64::from(self.pitch)
            .checked_mul(u64::from(self.height))
            .and_then(|size| {
                size.checked_mul(if self.kind == ShapeKind::Monochrome {
                    2
                } else {
                    1
                })
            })
            .ok_or_else(super::geometry::invalid)?;
        let frame_bytes = u64::from(width)
            .checked_mul(u64::from(height))
            .and_then(|size| size.checked_mul(4))
            .ok_or_else(super::geometry::invalid)?;
        if self.width == 0
            || self.height == 0
            || self.pitch < row
            || required > self.bytes.len() as u64
            || required > 4 * 1024 * 1024
            || frame_bytes > frame.len() as u64
        {
            return Err(super::geometry::invalid());
        }
        let left = i64::from(x).max(0).min(i64::from(width));
        let top = i64::from(y).max(0).min(i64::from(height));
        let right = (i64::from(x) + i64::from(self.width))
            .max(0)
            .min(i64::from(width));
        let bottom = (i64::from(y) + i64::from(self.height))
            .max(0)
            .min(i64::from(height));
        if left >= right || top >= bottom {
            return Ok(None);
        }
        for dy in top..bottom {
            for dx in left..right {
                let sx = (dx - i64::from(x)) as usize;
                let sy = (dy - i64::from(y)) as usize;
                let dest = ((dy as usize * width as usize) + dx as usize) * 4;
                let pixel = &mut frame[dest..dest + 4];
                match self.kind {
                    ShapeKind::Monochrome => {
                        let index = sy * self.pitch as usize + sx / 8;
                        let bit = 0x80 >> (sx % 8);
                        let and = if self.bytes[index] & bit != 0 { 255 } else { 0 };
                        let xor = if self.bytes[index + self.pitch as usize * self.height as usize]
                            & bit
                            != 0
                        {
                            255
                        } else {
                            0
                        };
                        for value in &mut pixel[..3] {
                            *value = (*value & and) ^ xor;
                        }
                    }
                    ShapeKind::MaskedColor | ShapeKind::Color => {
                        let src = sy * self.pitch as usize + sx * 4;
                        let cursor = &self.bytes[src..src + 4];
                        if self.kind == ShapeKind::MaskedColor {
                            for i in 0..3 {
                                pixel[i] = if cursor[3] == 0 {
                                    cursor[i]
                                } else {
                                    pixel[i] ^ cursor[i]
                                };
                            }
                        } else {
                            let a = u32::from(cursor[3]);
                            for i in 0..3 {
                                pixel[i] = ((u32::from(cursor[i]) * a
                                    + u32::from(pixel[i]) * (255 - a)
                                    + 127)
                                    / 255) as u8;
                            }
                        }
                    }
                }
                pixel[3] = 255;
            }
        }
        Ok(Some(DamageRect {
            x: left as u32,
            y: top as u32,
            width: (right - left) as u32,
            height: (bottom - top) as u32,
        }))
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn color_alpha_masks_and_negative_clipping() {
        let mut frame = vec![10, 20, 30, 255, 10, 20, 30, 255];
        let shape = Shape {
            kind: ShapeKind::Color,
            width: 2,
            height: 1,
            pitch: 8,
            bytes: vec![255, 0, 0, 255, 110, 120, 130, 128],
        };
        assert_eq!(
            shape.overlay(&mut frame, 2, 1, -1, 0).unwrap(),
            Some(DamageRect {
                x: 0,
                y: 0,
                width: 1,
                height: 1
            })
        );
        assert_eq!(&frame[..4], &[60, 70, 80, 255]);
        let mono = Shape {
            kind: ShapeKind::Monochrome,
            width: 2,
            height: 1,
            pitch: 1,
            bytes: vec![0b1000_0000, 0b1100_0000],
        };
        mono.overlay(&mut frame, 2, 1, 0, 0).unwrap();
        assert_eq!(&frame[..4], &[195, 185, 175, 255]);
        assert_eq!(&frame[4..], &[255, 255, 255, 255]);
        let masked = Shape {
            kind: ShapeKind::MaskedColor,
            width: 1,
            height: 1,
            pitch: 4,
            bytes: vec![255, 0, 255, 255],
        };
        masked.overlay(&mut frame, 2, 1, 0, 0).unwrap();
        assert_eq!(&frame[..4], &[60, 185, 80, 255]);
    }
    #[test]
    fn malformed_native_cursor_extents_are_rejected_without_overflow() {
        let shape = Shape {
            kind: ShapeKind::Monochrome,
            width: 1,
            height: u32::MAX,
            pitch: u32::MAX,
            bytes: Vec::new(),
        };
        assert!(shape.overlay(&mut [], u32::MAX, u32::MAX, 0, 0).is_err());
    }
}
