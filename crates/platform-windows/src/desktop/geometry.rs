// SPDX-License-Identifier: Apache-2.0
//! All positions are physical desktop pixels under per-monitor DPI awareness.
//! Capture frames are already oriented; rotation metadata never rotates input twice.
use ports::desktop::{CapturedFrame, DamageRect, PixelFormat, PlatformError};
use protocol::control::{TargetGeometry, TargetPoint};

pub(crate) fn invalid() -> PlatformError {
    PlatformError::Native("invalid desktop geometry or input".into())
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Monitor {
    pub name: String,
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
    pub dpi: u32,
    pub rotation: u16,
}
impl Monitor {
    pub fn geometry(&self, revision: u64) -> TargetGeometry {
        TargetGeometry {
            width: self.width,
            height: self.height,
            scale: self.dpi as f32 / 96.0,
            rotation_degrees: self.rotation,
            revision,
        }
    }
    pub fn logical_size(&self) -> (f64, f64) {
        (
            f64::from(self.width) * 96.0 / f64::from(self.dpi),
            f64::from(self.height) * 96.0 / f64::from(self.dpi),
        )
    }
    pub fn contains(&self, x: i32, y: i32) -> bool {
        i64::from(x) >= i64::from(self.x)
            && i64::from(y) >= i64::from(self.y)
            && i64::from(x) < i64::from(self.x) + i64::from(self.width)
            && i64::from(y) < i64::from(self.y) + i64::from(self.height)
    }
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Layout {
    pub(crate) monitors: Vec<Monitor>,
    pub(crate) desktop: Monitor,
}
impl Layout {
    pub fn monitors(&self) -> &[Monitor] {
        &self.monitors
    }
    pub fn desktop(&self) -> &Monitor {
        &self.desktop
    }
    pub fn new(mut monitors: Vec<Monitor>) -> Result<Self, PlatformError> {
        if monitors.is_empty() {
            return Err(PlatformError::TemporarilyUnavailable(
                "no active monitors".into(),
            ));
        }
        if monitors.len() > 64 {
            return Err(invalid());
        }
        monitors.sort_by(|a, b| a.name.cmp(&b.name));
        for m in &monitors {
            if m.name.is_empty()
                || m.name == super::COMPOSITE
                || m.width == 0
                || m.height == 0
                || m.dpi == 0
                || ![0, 90, 180, 270].contains(&m.rotation)
                || u64::from(m.width) * u64::from(m.height) > 64 * 1024 * 1024
            {
                return Err(invalid());
            }
        }
        if monitors.windows(2).any(|w| w[0].name == w[1].name) {
            return Err(invalid());
        }
        let x = monitors.iter().map(|m| m.x).min().ok_or_else(invalid)?;
        let y = monitors.iter().map(|m| m.y).min().ok_or_else(invalid)?;
        let right = monitors
            .iter()
            .map(|m| i64::from(m.x) + i64::from(m.width))
            .max()
            .ok_or_else(invalid)?;
        let bottom = monitors
            .iter()
            .map(|m| i64::from(m.y) + i64::from(m.height))
            .max()
            .ok_or_else(invalid)?;
        let width = u32::try_from(right - i64::from(x)).map_err(|_| invalid())?;
        let height = u32::try_from(bottom - i64::from(y)).map_err(|_| invalid())?;
        if right > i64::from(i32::MAX)
            || bottom > i64::from(i32::MAX)
            || width > 65536
            || height > 65536
            || u64::from(width) * u64::from(height) > 64 * 1024 * 1024
        {
            return Err(invalid());
        }
        Ok(Self {
            monitors,
            desktop: Monitor {
                name: super::COMPOSITE.into(),
                x,
                y,
                width,
                height,
                dpi: 96,
                rotation: 0,
            },
        })
    }
    pub fn physical(
        &self,
        target: &Monitor,
        point: TargetPoint,
    ) -> Result<(i32, i32), PlatformError> {
        if !self.monitors.contains(target) && target != &self.desktop {
            return Err(invalid());
        }
        if !point.x.is_finite()
            || !point.y.is_finite()
            || !(0.0..=1.0).contains(&point.x)
            || !(0.0..=1.0).contains(&point.y)
        {
            return Err(invalid());
        }
        let x = target.x + (point.x * f64::from(target.width - 1)).round() as i32;
        let y = target.y + (point.y * f64::from(target.height - 1)).round() as i32;
        if !self.monitors.iter().any(|m| m.contains(x, y)) {
            return Err(invalid());
        }
        Ok((x, y))
    }
    /// Pixel centers avoid quantization onto a neighboring physical pixel.
    pub fn absolute(&self, x: i32, y: i32) -> Result<(i32, i32), PlatformError> {
        if !self.monitors.iter().any(|m| m.contains(x, y)) {
            return Err(invalid());
        }
        let map = |p: i32, origin: i32, size: u32| {
            (((i64::from(p) - i64::from(origin)) * 2 + 1) * 65536 / (i64::from(size) * 2)) as i32
        };
        Ok((
            map(x, self.desktop.x, self.desktop.width),
            map(y, self.desktop.y, self.desktop.height),
        ))
    }
    pub fn physical_from_absolute(&self, x: i32, y: i32) -> Result<(i32, i32), PlatformError> {
        if !(0..=65535).contains(&x) || !(0..=65535).contains(&y) {
            return Err(invalid());
        }
        Ok((
            self.desktop.x + (i64::from(x) * i64::from(self.desktop.width) / 65536) as i32,
            self.desktop.y + (i64::from(y) * i64::from(self.desktop.height) / 65536) as i32,
        ))
    }
}

pub fn check_geometry(
    expected: &TargetGeometry,
    actual: &TargetGeometry,
) -> Result<(), PlatformError> {
    if expected != actual {
        Err(PlatformError::GeometryChanged)
    } else {
        Ok(())
    }
}
pub fn validate_frame(frame: &CapturedFrame) -> Result<(), PlatformError> {
    if frame.width == 0
        || frame.height == 0
        || u64::from(frame.stride) < u64::from(frame.width) * 4
        || u64::from(frame.stride) * u64::from(frame.height) > 256 * 1024 * 1024
        || (frame.data.len() as u64) < u64::from(frame.stride) * u64::from(frame.height)
    {
        return Err(invalid());
    }
    Ok(())
}
/// Clips and coalesces damage. Excess fragmentation becomes one full rectangle.
pub fn merge_damage(rects: &[DamageRect], width: u32, height: u32) -> Vec<DamageRect> {
    if rects.len() > 256 {
        return vec![DamageRect {
            x: 0,
            y: 0,
            width,
            height,
        }];
    }
    let mut out: Vec<DamageRect> = Vec::new();
    for rect in rects {
        let right = rect.x.saturating_add(rect.width).min(width);
        let bottom = rect.y.saturating_add(rect.height).min(height);
        if right <= rect.x || bottom <= rect.y {
            continue;
        }
        let mut r = DamageRect {
            x: rect.x,
            y: rect.y,
            width: right - rect.x,
            height: bottom - rect.y,
        };
        let mut i = 0;
        while i < out.len() {
            let o = out[i];
            if r.x <= o.x + o.width
                && o.x <= r.x + r.width
                && r.y <= o.y + o.height
                && o.y <= r.y + r.height
            {
                let right = (r.x + r.width).max(o.x + o.width);
                let bottom = (r.y + r.height).max(o.y + o.height);
                r.x = r.x.min(o.x);
                r.y = r.y.min(o.y);
                r.width = right - r.x;
                r.height = bottom - r.y;
                out.swap_remove(i);
                i = 0;
            } else {
                i += 1;
            }
        }
        out.push(r);
    }
    out
}
/// Rotate a packed BGRA DXGI surface into physical monitor orientation.
pub fn rotate_bgra(
    data: &[u8],
    width: u32,
    height: u32,
    stride: u32,
    rotation: u16,
) -> Result<(Vec<u8>, u32, u32), PlatformError> {
    if width == 0
        || height == 0
        || u64::from(stride) < u64::from(width) * 4
        || u64::from(stride) * u64::from(height) > 256 * 1024 * 1024
        || u64::from(stride) * u64::from(height) > data.len() as u64
        || u64::from(width) * u64::from(height) > 64 * 1024 * 1024
    {
        return Err(invalid());
    }
    let (w, h) = match rotation {
        0 | 180 => (width, height),
        90 | 270 => (height, width),
        _ => return Err(invalid()),
    };
    let mut out = vec![0; w as usize * h as usize * 4];
    for y in 0..height {
        for x in 0..width {
            let (dx, dy) = match rotation {
                0 => (x, y),
                90 => (height - 1 - y, x),
                180 => (width - 1 - x, height - 1 - y),
                270 => (y, width - 1 - x),
                _ => unreachable!(),
            };
            let src = (y * stride + x * 4) as usize;
            let dst = ((dy * w + dx) * 4) as usize;
            out[dst..dst + 4].copy_from_slice(&data[src..src + 4]);
        }
    }
    Ok((out, w, h))
}
pub fn composite(
    layout: &Layout,
    frames: &[(Monitor, CapturedFrame)],
    geometry: TargetGeometry,
) -> Result<CapturedFrame, PlatformError> {
    let d = &layout.desktop;
    if frames.len() != layout.monitors.len()
        || layout.monitors.iter().any(|m| {
            frames
                .iter()
                .filter(|(candidate, _)| candidate == m)
                .count()
                != 1
        })
    {
        return Err(invalid());
    }
    let mut data = vec![0; d.width as usize * d.height as usize * 4];
    for p in data.as_chunks_mut::<4>().0 {
        p[3] = 255;
    }
    for (monitor, frame) in frames {
        validate_frame(frame)?;
        if !layout.monitors.contains(monitor)
            || frame.width != monitor.width
            || frame.height != monitor.height
        {
            return Err(PlatformError::GeometryChanged);
        }
        let x = (monitor.x - d.x) as u32;
        let y = (monitor.y - d.y) as u32;
        for row in 0..frame.height {
            for col in 0..frame.width {
                let src = (row * frame.stride + col * 4) as usize;
                let dst = (((y + row) * d.width + x + col) * 4) as usize;
                let p = &frame.data[src..src + 4];
                data[dst..dst + 4].copy_from_slice(&match frame.format {
                    PixelFormat::Bgra8 | PixelFormat::Bgrx8 => [p[0], p[1], p[2], 255],
                    _ => [p[2], p[1], p[0], 255],
                });
            }
        }
    }
    Ok(CapturedFrame {
        width: d.width,
        height: d.height,
        stride: d.width * 4,
        format: PixelFormat::Bgra8,
        data,
        geometry,
        cursor: None,
        damage: None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    fn monitor(x: i32, w: u32, h: u32, dpi: u32, rotation: u16) -> Monitor {
        Monitor {
            name: format!("monitor-{x}"),
            x,
            y: 0,
            width: w,
            height: h,
            dpi,
            rotation,
        }
    }
    #[test]
    fn mapping_round_trips_mixed_dpi_negative_and_portrait() {
        let configurations = vec![
            vec![monitor(0, 1920, 1080, 96, 0)],
            vec![
                monitor(0, 3840, 2160, 144, 0),
                monitor(-1920, 1920, 1080, 96, 0),
            ],
            vec![monitor(0, 1080, 1920, 96, 90)],
        ];
        for monitors in configurations {
            let layout = Layout::new(monitors).unwrap();
            for m in &layout.monitors {
                for x in [0.0, 0.123, 0.5, 1.0] {
                    for y in [0.0, 0.717, 1.0] {
                        let physical = layout.physical(m, TargetPoint { x, y }).unwrap();
                        let absolute = layout.absolute(physical.0, physical.1).unwrap();
                        assert_eq!(
                            layout
                                .physical_from_absolute(absolute.0, absolute.1)
                                .unwrap(),
                            physical
                        );
                    }
                }
            }
        }
        assert_eq!(
            monitor(0, 3840, 2160, 144, 0).logical_size(),
            (2560.0, 1440.0)
        );
    }
    #[test]
    fn stale_or_forged_geometry_and_invalid_points_rejected() {
        let layout = Layout::new(vec![monitor(0, 1920, 1080, 96, 0)]).unwrap();
        let g = layout.desktop.geometry(2);
        assert!(matches!(
            check_geometry(&TargetGeometry { revision: 1, ..g }, &g),
            Err(PlatformError::GeometryChanged)
        ));
        assert!(check_geometry(&TargetGeometry { scale: 2.0, ..g }, &g).is_err());
        for x in [f64::NAN, f64::INFINITY, -0.01, 1.01] {
            assert!(
                layout
                    .physical(&layout.desktop, TargetPoint { x, y: 0.5 })
                    .is_err()
            );
        }
    }
    #[test]
    fn rotation_and_damage_respect_padding_and_bounds() {
        let source = [
            1, 0, 0, 255, 2, 0, 0, 255, 0, 0, 0, 0, 3, 0, 0, 255, 4, 0, 0, 255, 0, 0, 0, 0,
        ];
        let (pixels, w, h) = rotate_bgra(&source, 2, 2, 12, 90).unwrap();
        assert_eq!((w, h), (2, 2));
        assert_eq!(
            pixels
                .as_chunks::<4>()
                .0
                .iter()
                .map(|p| p[0])
                .collect::<Vec<_>>(),
            vec![3, 1, 4, 2]
        );
        assert_eq!(
            merge_damage(
                &[
                    DamageRect {
                        x: 1,
                        y: 1,
                        width: 3,
                        height: 3
                    },
                    DamageRect {
                        x: 3,
                        y: 3,
                        width: u32::MAX,
                        height: 9
                    }
                ],
                10,
                10
            ),
            vec![DamageRect {
                x: 1,
                y: 1,
                width: 9,
                height: 9
            }]
        );
    }
}
