//! Complete partially masked ink on plain balloon paper before either fill tier samples it.
use image::{GrayImage, Luma, RgbImage, imageops};
use imageproc::{
    distance_transform::Norm,
    morphology::dilate,
    region_labelling::{Connectivity, connected_components},
};
use koharu_scene::{Block, Hash, Layer, MaskMode, Offset, Rect, Role, Tiles};
use kurbo::{BezPath, Point, Shape as _};

use crate::stages::mask_hash;

/// The original bitmap is authoritative for new detections. A project saved before
/// fingerprints has one other witness: the alpha written by its last automatic fill.
pub(super) fn automatic(
    layer: &Layer,
    tiles: &Tiles,
    cleanup: &Tiles,
    rect: Rect,
    mask: &GrayImage,
) -> bool {
    if tiles.offset != Offset::default()
        || tiles
            .tiles
            .iter()
            .any(|t| t.pixels.iter().any(|&v| v != 0 && v != 255))
    {
        return false;
    }
    if let Some(value) = layer
        .attributes
        .get("koharu")
        .and_then(|m| m.get("automatic_mask"))
    {
        return serde_json::from_value::<Hash>(value.clone())
            .is_ok_and(|hash| hash == mask_hash(rect, mask));
    }
    layer
        .attributes
        .get("c2pa")
        .is_some_and(|actions| actions.contains_key("inpainting"))
        && cleanup.offset == tiles.offset
        && cleanup.tiles.len() == tiles.tiles.len()
        && tiles.tiles.iter().zip(&cleanup.tiles).all(|(m, c)| {
            m.x == c.x
                && m.y == c.y
                && m.pixels
                    .iter()
                    .copied()
                    .eq(c.pixels.chunks_exact(4).map(|p| p[3]))
        })
}

pub(super) fn eligible(layer: &Layer, block: &Block) -> Option<(kurbo::Rect, f64)> {
    let style = &block.source.style.chars;
    let em = style.size?;
    let meta = layer.attributes.get("koharu")?;
    let paper: [u8; 3] =
        serde_json::from_value(meta.get("detection")?.get("paper")?.clone()).ok()?;
    if block.role != Role::Text
        || block.mask.locked
        || block.cleanup.content.offset != Offset::default()
        || !block.mask.visible
        || block.mask.content.mode != MaskMode::Add
        || block.source.content.text().trim().is_empty()
        || style.stroke.is_some()
        || !em.is_finite()
        || em <= 0.0
        || paper.iter().any(|v| *v < 245)
    {
        return None;
    }
    Some((block.source.bounds()?, em))
}

pub(super) fn complete(
    image: &RgbImage,
    owners: &GrayImage,
    rect: Rect,
    mask: &GrayImage,
    bounds: kurbo::Rect,
    em: f64,
    balloon: &BezPath,
) -> Option<(Rect, GrayImage)> {
    let window = super::flat::grown(
        rect,
        (em.ceil() as u32)
            .min(rect.width.max(rect.height))
            .saturating_add(4),
        image.width(),
        image.height(),
    );
    let (ox, oy) = (window.x as u32, window.y as u32);
    let ink = GrayImage::from_fn(window.width, window.height, |x, y| {
        Luma([u8::from(
            image.get_pixel(ox + x, oy + y).0.iter().all(|&v| v < 235),
        )])
    });
    let labels = connected_components(&ink, Connectivity::Eight, Luma([0]));
    let mut components = Vec::<Vec<(u32, u32)>>::new();
    for (x, y, label) in labels.enumerate_pixels() {
        let id = label.0[0] as usize;
        if id != 0 {
            if id > components.len() {
                components.resize_with(id, Vec::new);
            }
            components[id - 1].push((x, y));
        }
    }
    let mut result = GrayImage::new(window.width, window.height);
    imageops::replace(
        &mut result,
        mask,
        i64::from(rect.x - window.x),
        i64::from(rect.y - window.y),
    );
    let dx = (rect.x - window.x) as u32;
    let dy = (rect.y - window.y) as u32;
    let mut changed = false;
    for component in components.iter().filter(|c| !c.is_empty()) {
        let (mut left, mut top, mut right, mut bottom) = (window.width, window.height, 0, 0);
        let (mut own, mut other, mut dark) = (0, 0, false);
        for &(x, y) in component {
            let held = x >= dx
                && y >= dy
                && mask
                    .get_pixel_checked(x - dx, y - dy)
                    .is_some_and(|p| p.0[0] != 0);
            own += usize::from(held);
            other += usize::from(owners.get_pixel(ox + x, oy + y).0[0] > u8::from(held));
            dark |= image.get_pixel(ox + x, oy + y).0.iter().all(|&v| v < 128);
            left = left.min(x);
            top = top.min(y);
            right = right.max(x);
            bottom = bottom.max(y);
        }
        // A piece clipped at the search boundary could continue into a border or drawing.
        if !dark
            || own < component.len().div_ceil(4)
            || own == component.len()
            || other >= own
            || left < 4
            || top < 4
            || right + 4 >= window.width
            || bottom + 4 >= window.height
            || f64::from((right - left + 1).max(bottom - top + 1)) > 1.4 * em
            || !bounds.contains(Point::new(
                f64::from(ox) + f64::from(left + right + 1) / 2.,
                f64::from(oy) + f64::from(top + bottom + 1) / 2.,
            ))
        {
            continue;
        }
        let (lx, ly) = (left - 4, top - 4);
        let mut shape = GrayImage::new(right - left + 9, bottom - top + 9);
        for &(x, y) in component {
            shape.put_pixel(x - lx, y - ly, Luma([255]));
        }
        // An antialiased edge needs a small halo. Check the next ring for paper before
        // extending anything; a dark line on screentone can also intersect the mask.
        let halo = dilate(&shape, Norm::L1, 2);
        let ring = dilate(&halo, Norm::L1, 2);
        let mut samples = 0;
        let mut off_paper = 0;
        let mut additions = Vec::new();
        for (x, y, _) in ring.enumerate_pixels().filter(|(_, _, v)| v.0[0] != 0) {
            let (px, py) = (ox + lx + x, oy + ly + y);
            if halo.get_pixel(x, y).0[0] == 0 {
                samples += 1;
                off_paper += usize::from(image.get_pixel(px, py).0.iter().any(|&c| c < 245));
            } else if owners.get_pixel(px, py).0[0] == 0
                && (shape.get_pixel(x, y).0[0] != 0
                    || image.get_pixel(px, py).0.iter().all(|&v| v >= 235))
            {
                additions.push((lx + x, ly + y));
            }
        }
        if off_paper > samples / 20
            || additions.is_empty()
            || additions.iter().any(|&(x, y)| {
                !balloon.contains(Point::new(f64::from(ox + x) + 0.5, f64::from(oy + y) + 0.5))
            })
        {
            continue;
        }
        for (x, y) in additions {
            result.put_pixel(x, y, Luma([255]));
        }
        changed = true;
    }
    changed.then_some((window, result))
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::Rgb;

    #[test]
    fn detector_fingerprint_survives_tile_cropping_and_rejects_repainted_masks() {
        use koharu_scene::{PixelFormat, Tile, Uuid};
        use serde_json::json;

        let padded = Rect {
            x: 5,
            y: 7,
            width: 3,
            height: 3,
        };
        let hash = mask_hash(padded, &[0, 0, 0, 0, 255, 0, 0, 0, 0]);
        let rect = Rect {
            x: 6,
            y: 8,
            width: 1,
            height: 1,
        };
        let mask = GrayImage::from_pixel(1, 1, Luma([255]));
        let mut layer: Layer = serde_json::from_value(json!({
            "id": Uuid::nil(), "kind": "raster", "name": "", "visible": true, "locked": false,
            "opacity": 1.0, "blend": "normal", "attributes": {"koharu": {"automatic_mask": hash}}
        }))
        .unwrap();
        let mut bytes = vec![0; 64 * 64];
        bytes[8 * 64 + 6] = 255;
        let tiles = Tiles {
            offset: Offset::default(),
            format: PixelFormat::Gray8,
            tiles: vec![Tile {
                x: 0,
                y: 0,
                pixels: bytes,
            }],
        };
        let mut cleanup = Tiles {
            offset: Offset::default(),
            format: PixelFormat::Rgba8,
            tiles: Vec::new(),
        };
        assert!(automatic(&layer, &tiles, &cleanup, rect, &mask));
        assert!(!automatic(
            &layer,
            &tiles,
            &cleanup,
            Rect { x: 7, ..rect },
            &mask
        ));
        layer
            .attributes
            .get_mut("koharu")
            .unwrap()
            .remove("automatic_mask");
        layer
            .attributes
            .entry("c2pa".into())
            .or_default()
            .insert("inpainting".into(), json!({}));
        let mut rgba = vec![0; 64 * 64 * 4];
        rgba[(8 * 64 + 6) * 4 + 3] = 255;
        cleanup.tiles.push(Tile {
            x: 0,
            y: 0,
            pixels: rgba,
        });
        assert!(automatic(&layer, &tiles, &cleanup, rect, &mask));
        cleanup.tiles[0].pixels[3] = 255;
        assert!(!automatic(&layer, &tiles, &cleanup, rect, &mask));
        cleanup.tiles[0].pixels[3] = 0;
        layer
            .attributes
            .get_mut("koharu")
            .unwrap()
            .insert("automatic_mask".into(), json!(null));
        assert!(!automatic(&layer, &tiles, &cleanup, rect, &mask));
    }

    #[test]
    fn completes_only_seeded_ink_and_leaves_other_blocks_and_the_balloon_outline() {
        let mut image = RgbImage::from_pixel(90, 70, Rgb([255; 3]));
        let rect = Rect {
            x: 20,
            y: 12,
            width: 7,
            height: 42,
        };
        let mask = GrayImage::from_pixel(rect.width, rect.height, Luma([255]));
        let mut owners = GrayImage::new(90, 70);
        imageops::replace(
            &mut owners,
            &GrayImage::from_pixel(rect.width, rect.height, Luma([1])),
            20,
            12,
        );
        for x in 22..40 {
            for y in 20..23 {
                image.put_pixel(x, y, Rgb([0; 3]));
            }
        }
        // Separated ink has no seed. A border runs past the search window.
        for x in 0..85 {
            image.put_pixel(x, 47, Rgb([0; 3]));
        }
        image.put_pixel(33, 32, Rgb([0; 3]));
        owners.put_pixel(39, 21, Luma([1]));
        let bounds = kurbo::Rect::new(10., 10., 60., 60.);
        let balloon = bounds.to_path(0.);
        let (at, repaired) = complete(&image, &owners, rect, &mask, bounds, 24., &balloon).unwrap();
        let covers =
            |x: u32, y: u32| repaired.get_pixel(x - at.x as u32, y - at.y as u32).0[0] != 0;
        assert!(covers(38, 21));
        assert!(!covers(39, 21));
        assert!(!covers(33, 32));
        assert!(!covers(35, 47));
        for (x, y) in super::super::masked(at, &repaired) {
            owners.put_pixel(x, y, Luma([1]));
        }
        assert!(complete(&image, &owners, at, &repaired, bounds, 24., &balloon).is_none());
    }
}
