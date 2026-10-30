// SPDX-License-Identifier: Apache-2.0
//! The only raw PipeWire buffer boundary; owned pixel copies leave this module.
use crate::native::Output;
use pipewire::{self as pw, spa};
use ports::desktop::{CapturedFrame, DamageRect, PixelFormat};

#[allow(unsafe_code)]
pub(crate) fn copy(
    stream: &pw::stream::Stream,
    format: &spa::param::video::VideoInfoRaw,
    output: &Output,
) -> Option<CapturedFrame> {
    // SAFETY: Called only from this stream's process callback, on its owning loop thread.
    let raw = unsafe { stream.dequeue_raw_buffer() };
    if raw.is_null() {
        return None;
    }
    struct ReturnBuffer<'a> {
        stream: &'a pw::stream::Stream,
        raw: *mut pw::sys::pw_buffer,
    }
    impl Drop for ReturnBuffer<'_> {
        fn drop(&mut self) {
            // SAFETY: This guard owns exactly one dequeued buffer and returns it exactly once to its originating stream.
            unsafe {
                self.stream.queue_raw_buffer(self.raw);
            }
        }
    }
    let _return_buffer = ReturnBuffer { stream, raw };
    // SAFETY: PipeWire owns the dequeued buffer until the guard queues it; all buffer and metadata pointers below are used only within that lifetime.
    let buffer = unsafe { (*raw).buffer.as_ref() }?;
    if buffer.n_datas != 1 || buffer.datas.is_null() {
        return None;
    }
    // SAFETY: n_datas is one and the PipeWire buffer supplies a live spa_data array.
    let data = unsafe { &*buffer.datas };
    // SAFETY: The spa_data's chunk belongs to the same held buffer. Null is rejected.
    let chunk = unsafe { data.chunk.as_ref() }?;
    let (w, h) = (format.size().width, format.size().height);
    let stride = u32::try_from(chunk.stride).ok()?;
    let len = u64::from(stride) * u64::from(h);
    if w == 0
        || h == 0
        || u64::from(stride) < u64::from(w) * 4
        || len > 256 * 1024 * 1024
        || u64::from(chunk.size) < len
        || u64::from(chunk.offset) + len > u64::from(data.maxsize)
        || data.data.is_null()
    {
        return None;
    }
    let pixel_format = match format.format() {
        spa::param::video::VideoFormat::BGRx => PixelFormat::Bgrx8,
        spa::param::video::VideoFormat::BGRA => PixelFormat::Bgra8,
        spa::param::video::VideoFormat::RGBA => PixelFormat::Rgba8,
        spa::param::video::VideoFormat::RGBx => PixelFormat::Rgbx8,
        _ => return None,
    };
    // SAFETY: MAP_BUFFERS makes data readable; the checked offset/length is inside maxsize and the guard retains ownership throughout the copy.
    let pixels = unsafe {
        std::slice::from_raw_parts(
            data.data.cast::<u8>().add(chunk.offset as usize),
            len as usize,
        )
    }
    .to_vec();
    let mut damage = None;
    let mut cursor = None;
    if buffer.n_metas <= 1024 && buffer.n_metas > 0 && !buffer.metas.is_null() {
        // SAFETY: PipeWire provides an array of n_metas records belonging to this held buffer.
        let metas = unsafe { std::slice::from_raw_parts(buffer.metas, buffer.n_metas as usize) };
        for meta in metas {
            if meta.data.is_null() || meta.size > 65536 {
                continue;
            }
            // SAFETY: Each metadata record describes its own readable region, valid while the buffer is held.
            let bytes =
                unsafe { std::slice::from_raw_parts(meta.data.cast::<u8>(), meta.size as usize) };
            if meta.type_ == spa::sys::SPA_META_VideoDamage {
                damage = Some(parse_damage(bytes));
            }
            if meta.type_ == spa::sys::SPA_META_Cursor && bytes.len() >= 16 {
                let id = u32::from_ne_bytes(bytes[0..4].try_into().ok()?);
                let x = i32::from_ne_bytes(bytes[8..12].try_into().ok()?);
                let y = i32::from_ne_bytes(bytes[12..16].try_into().ok()?);
                if id != 0 && x >= 0 && y >= 0 && (x as u32) < w && (y as u32) < h {
                    cursor = Some((x as u32, y as u32));
                }
            }
        }
    }
    Some(CapturedFrame {
        width: w,
        height: h,
        stride,
        format: pixel_format,
        data: pixels,
        geometry: protocol::control::TargetGeometry {
            width: w,
            height: h,
            scale: output.scale,
            rotation_degrees: output.rotation,
            revision: 0,
        },
        damage,
        cursor,
    })
}
fn parse_damage(bytes: &[u8]) -> Vec<DamageRect> {
    let mut rects = Vec::new();
    for rect in bytes.as_chunks::<16>().0 {
        let x = i32::from_ne_bytes(rect[0..4].try_into().expect("fixed field"));
        let y = i32::from_ne_bytes(rect[4..8].try_into().expect("fixed field"));
        let width = u32::from_ne_bytes(rect[8..12].try_into().expect("fixed field"));
        let height = u32::from_ne_bytes(rect[12..16].try_into().expect("fixed field"));
        if width == 0 || height == 0 {
            break;
        }
        let right = (i64::from(x) + i64::from(width)).max(0) as u32;
        let bottom = (i64::from(y) + i64::from(height)).max(0) as u32;
        let x = x.max(0) as u32;
        let y = y.max(0) as u32;
        rects.push(DamageRect {
            x,
            y,
            width: right.saturating_sub(x),
            height: bottom.saturating_sub(y),
        });
    }
    rects
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn damage_metadata_clips_negative_origin_and_stops_at_empty() {
        let bytes: [u8; 32] = [-2i32, 3, 10, 20, 0, 0, 0, 0]
            .map(i32::to_ne_bytes)
            .concat()
            .try_into()
            .unwrap();
        assert_eq!(
            parse_damage(&bytes),
            [DamageRect {
                x: 0,
                y: 3,
                width: 8,
                height: 20
            }]
        );
    }
}
