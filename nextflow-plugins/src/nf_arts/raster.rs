//! nf-arts' pictures drawn into pixels and written as an RGB PNG, by uwe's own small encoder.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::f64::consts::TAU;
use std::io::Write;

use super::image::{Picture, Shape};

impl Picture {
    /// The picture as an RGB PNG without metadata, each pixel sampled at its centre. Text is
    /// not drawn: uwe has no font to draw it with.
    pub(super) fn png(&self) -> std::io::Result<Vec<u8>> {
        let (width, height) = (self.width as usize, self.height as usize);
        let mut pixels: Vec<[f64; 3]> = vec![self.background.map(f64::from); width * height];
        for shape in &self.shapes {
            paint(shape, &mut pixels, width, height);
        }
        let mut rows = Vec::with_capacity((width * 3 + 1) * height);
        for row in pixels.chunks(width) {
            rows.push(0);
            for pixel in row {
                rows.extend(pixel.map(|channel| channel.round().clamp(0.0, 255.0) as u8));
            }
        }
        let mut encoder =
            flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::default());
        encoder.write_all(&rows)?;
        let data = encoder.finish()?;
        let mut header = Vec::new();
        header.extend(self.width.to_be_bytes());
        header.extend(self.height.to_be_bytes());
        header.extend([8, 2, 0, 0, 0]);
        let mut png = b"\x89PNG\r\n\x1a\n".to_vec();
        for (kind, content) in [(b"IHDR", &header), (b"IDAT", &data), (b"IEND", &Vec::new())] {
            png.extend((content.len() as u32).to_be_bytes());
            let mut checked = kind.to_vec();
            checked.extend(content);
            let mut crc = flate2::Crc::new();
            crc.update(&checked);
            png.extend(checked);
            png.extend(crc.sum().to_be_bytes());
        }
        Ok(png)
    }
}

/// Blends a shape into the pixels it covers, over its bounding box.
fn paint(shape: &Shape, pixels: &mut [[f64; 3]], width: usize, height: usize) {
    let (bounds, color, opacity) = match shape {
        Shape::Arc {
            center,
            radius,
            width,
            color,
            opacity,
            ..
        } => {
            let reach = radius + width / 2.0;
            (
                (
                    center.0 - reach,
                    center.1 - reach,
                    center.0 + reach,
                    center.1 + reach,
                ),
                color,
                opacity,
            )
        }
        Shape::Line {
            points,
            width,
            color,
            opacity,
        } => {
            let xs = points.iter().map(|point| point.0);
            let ys = points.iter().map(|point| point.1);
            let half = width / 2.0;
            let low = (
                xs.clone().fold(f64::MAX, f64::min),
                ys.clone().fold(f64::MAX, f64::min),
            );
            let high = (xs.fold(f64::MIN, f64::max), ys.fold(f64::MIN, f64::max));
            (
                (low.0 - half, low.1 - half, high.0 + half, high.1 + half),
                color,
                opacity,
            )
        }
        Shape::Dot {
            center,
            radius,
            color,
            opacity,
            ..
        } => (
            (
                center.0 - radius,
                center.1 - radius,
                center.0 + radius,
                center.1 + radius,
            ),
            color,
            opacity,
        ),
        Shape::Block {
            corner,
            size,
            color,
            opacity,
            ..
        } => (
            (corner.0, corner.1, corner.0 + size.0, corner.1 + size.1),
            color,
            opacity,
        ),
    };
    let column = |value: f64, limit: usize| (value.max(0.0) as usize).min(limit);
    for y in column(bounds.1.floor(), height)..column(bounds.3.ceil(), height) {
        for x in column(bounds.0.floor(), width)..column(bounds.2.ceil(), width) {
            if covers(shape, (x as f64 + 0.5, y as f64 + 0.5)) {
                let pixel = &mut pixels[y * width + x];
                for channel in 0..3 {
                    let mix = f64::from(color[channel]) * opacity;
                    pixel[channel] = mix + pixel[channel] * (1.0 - opacity);
                }
            }
        }
    }
}

/// Whether a shape covers a point.
fn covers(shape: &Shape, point: (f64, f64)) -> bool {
    let distance = |a: (f64, f64), b: (f64, f64)| (a.0 - b.0).hypot(a.1 - b.1);
    match shape {
        Shape::Arc {
            center,
            radius,
            turns,
            width,
            ..
        } => {
            let (dx, dy) = (point.0 - center.0, point.1 - center.1);
            let turn = dx.atan2(-dy).rem_euclid(TAU) / TAU;
            (distance(point, *center) - radius).abs() <= width / 2.0
                && turn >= turns.0
                && turn <= turns.1
        }
        Shape::Line { points, width, .. } => points.windows(2).any(|pair| {
            let (a, b) = (pair[0], pair[1]);
            let length = (b.0 - a.0).powi(2) + (b.1 - a.1).powi(2);
            let along = match length > 0.0 {
                true => (((point.0 - a.0) * (b.0 - a.0) + (point.1 - a.1) * (b.1 - a.1)) / length)
                    .clamp(0.0, 1.0),
                false => 0.0,
            };
            let nearest = (a.0 + along * (b.0 - a.0), a.1 + along * (b.1 - a.1));
            distance(point, nearest) <= width / 2.0
        }),
        Shape::Dot {
            center,
            radius,
            hollow,
            ..
        } => {
            let gap = distance(point, *center);
            match hollow {
                true => (gap - radius).abs() <= (radius / 8.0).max(0.5),
                false => gap <= *radius,
            }
        }
        Shape::Block {
            corner,
            size,
            hollow,
            ..
        } => {
            let inside = point.0 >= corner.0
                && point.0 <= corner.0 + size.0
                && point.1 >= corner.1
                && point.1 <= corner.1 + size.1;
            let edge = [
                point.0 - corner.0,
                corner.0 + size.0 - point.0,
                point.1 - corner.1,
            ]
            .into_iter()
            .chain([corner.1 + size.1 - point.1])
            .any(|gap| gap <= 1.0);
            inside && (!hollow || edge)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn writes_png() {
        let picture = Picture {
            width: 4,
            height: 2,
            background: [0x0b, 0x0b, 0x12],
            shapes: vec![Shape::Block {
                corner: (0.0, 0.0),
                size: (2.0, 2.0),
                hollow: false,
                color: [255, 0, 0],
                opacity: 1.0,
            }],
            caption: Vec::new(),
            ink: [0xee; 3],
        };
        let png = picture.png().expect("encodes");
        assert_eq!(&png[..8], b"\x89PNG\r\n\x1a\n");
        assert_eq!(&png[12..16], b"IHDR");
        assert_eq!(&png[16..25], &[0, 0, 0, 4, 0, 0, 0, 2, 8]);
        assert_eq!(png[25], 2, "RGB");
        let start = 8 + 25;
        let length = u32::from_be_bytes(png[start..start + 4].try_into().expect("4 bytes"));
        let data = &png[start + 8..start + 8 + length as usize];
        let mut rows = Vec::new();
        std::io::Read::read_to_end(&mut flate2::read::ZlibDecoder::new(data), &mut rows)
            .expect("inflates");
        assert_eq!(
            &rows[..13],
            &[0, 255, 0, 0, 255, 0, 0, 11, 11, 18, 11, 11, 18]
        );
        assert!(png.ends_with(&[0, 0, 0, 0, b'I', b'E', b'N', b'D', 0xae, 0x42, 0x60, 0x82]));
    }
}
