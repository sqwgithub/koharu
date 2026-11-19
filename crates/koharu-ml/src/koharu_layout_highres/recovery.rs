//! A page-scale query can miss a short column, or cover an aside together with a
//! much larger outlined effect. Reuse the detector at balloon scale before the
//! usual glyph extraction, mask ownership and reading-order pass.
use anyhow::Result;
use image::{DynamicImage, GrayImage, Luma, Rgb, RgbImage, imageops};
use imageproc::{
    distance_transform::Norm, drawing::draw_filled_rect_mut, morphology::erode, rect::Rect,
};

use super::{Detected, Detection, measure, order::share, suppress::area};

impl Detected {
    /// Recover additional text queries on local crops using the caller's detector
    /// and thresholds. Panel and balloon geometry always comes from the page pass.
    pub fn recover_balloon_text(
        &mut self,
        page: &RgbImage,
        mut infer: impl FnMut(&DynamicImage) -> Result<Detected>,
    ) -> Result<()> {
        let mut recovered = Vec::<Detection>::new();
        let mut searched_effects = Vec::new();
        let balloons: Vec<_> = self.0.iter().filter(|d| d.label == "bubble").collect();
        let crosses = |b: &Detection, d: &Detection| {
            share(b.bbox, d.bbox) > 0.8 && share(d.bbox, b.bbox) < 0.7
        };
        for balloon in &balloons {
            // A coarse box extending well past the balloon may own large effects and
            // small dialogue together. Blanking that box would erase the evidence.
            let effect = self
                .0
                .iter()
                .enumerate()
                .filter(|(_, d)| lettering(d) && crosses(balloon, d))
                .min_by(|(_, a), (_, b)| area(a.bbox).total_cmp(&area(b.bbox)))
                .map(|(i, _)| i);
            let (bounds, margin) = if let Some(i) = effect {
                if searched_effects.contains(&i) {
                    continue;
                }
                searched_effects.push(i);
                let b = self.0[i].bbox;
                (b, ((b[2] - b[0]).min(b[3] - b[1]) * 0.03).ceil())
            } else {
                (balloon.bbox, 0.)
            };
            let x = (bounds[0] - margin).floor().max(0.) as u32;
            let y = (bounds[1] - margin).floor().max(0.) as u32;
            let right = ((bounds[2] + margin).ceil() as u32).min(page.width());
            let bottom = ((bounds[3] + margin).ceil() as u32).min(page.height());
            let (w, h) = (right.saturating_sub(x), bottom.saturating_sub(y));
            // A page-sized panel mislabelled as a balloon offers no gain in scale.
            if w < 12
                || h < 12
                || u64::from(w) * u64::from(h)
                    > u64::from(page.width()) * u64::from(page.height()) / 2
            {
                continue;
            }
            let mut crop = imageops::crop_imm(page, x, y, w, h).to_image();
            if effect.is_none() {
                let mut interior = GrayImage::from_fn(w, h, |cx, cy| {
                    Luma([if balloon.mask.contains(x + cx, y + cy) {
                        255
                    } else {
                        0
                    }])
                });
                measure::fill_holes(&mut interior);
                let mut interior = erode(&interior, Norm::LInf, 4);
                for d in self
                    .0
                    .iter()
                    .chain(&recovered)
                    .filter(|d| lettering(d) && share(d.bbox, bounds) > 0.)
                {
                    let [l, t, r, b] = d.bbox;
                    // The detector's glyph masks grow three pixels beyond the source box.
                    let left = l.floor() as i32 - 3;
                    let top = t.floor() as i32 - 3;
                    let rect = Rect::at(left - x as i32, top - y as i32).of_size(
                        (r.ceil() as i32 + 3 - left) as u32,
                        (b.ceil() as i32 + 3 - top) as u32,
                    );
                    draw_filled_rect_mut(&mut interior, rect, Luma([0]));
                }
                let (mut visible, mut dark, mut paper) = (0usize, 0usize, 0usize);
                for (p, inside) in crop.pixels_mut().zip(interior.pixels()) {
                    if inside.0[0] == 0 {
                        *p = Rgb([255; 3]);
                    } else {
                        visible += 1;
                        dark += usize::from(p.0.iter().all(|&c| c < 150));
                        paper += usize::from(p.0.iter().all(|&c| c >= 235));
                    }
                }
                if dark < 24 || paper < visible - visible / 4 {
                    continue;
                }
            }
            let crop = DynamicImage::ImageRgb8(crop);
            let pixels = crop.as_rgb8().expect("RGB crop");
            let local = infer(&crop)?;
            for mut candidate in local.0.into_iter().filter(|d| d.label == "text") {
                let [l, t, r, b] = candidate.bbox;
                // A cut glyph at the crop boundary cannot establish a new block.
                if l < 1. || t < 1. || r >= w as f32 - 1. || b >= h as f32 - 1. {
                    continue;
                }
                if let Some(i) = effect {
                    let owner = self.0[i].bbox;
                    let box_on_page = [l + x as f32, t + y as f32, r + x as f32, b + y as f32];
                    let style = measure::glyph_mask(&candidate.mask, candidate.bbox, pixels)
                        .typography(pixels);
                    if share(box_on_page, owner) < 0.95
                        || !balloons
                            .iter()
                            .any(|b| crosses(b, &self.0[i]) && share(box_on_page, b.bbox) >= 0.75)
                        || (r - l) * (b - t) > 0.2 * (owner[2] - owner[0]) * (owner[3] - owner[1])
                        || !style.is_some_and(|s| {
                            s.size < 0.25 * (owner[2] - owner[0]).min(owner[3] - owner[1])
                                && s.outline.is_none()
                                && s.paper.is_some_and(|c| c.iter().all(|&v| v >= 220))
                        })
                    {
                        continue;
                    }
                }
                let ink = candidate
                    .mask
                    .pixels
                    .iter()
                    .enumerate()
                    .filter(|(_, v)| **v != 0)
                    .map(|(i, _)| {
                        (
                            candidate.mask.x + i as u32 % candidate.mask.width,
                            candidate.mask.y + i as u32 / candidate.mask.width,
                        )
                    })
                    .filter(|&(cx, cy)| {
                        (l..r).contains(&(cx as f32))
                            && (t..b).contains(&(cy as f32))
                            && pixels
                                .get_pixel_checked(cx, cy)
                                .is_some_and(|v| v.0.iter().all(|&c| c < 150))
                    })
                    .take(12)
                    .count();
                if ink < 12 {
                    continue;
                }
                candidate.bbox = [l + x as f32, t + y as f32, r + x as f32, b + y as f32];
                candidate.mask.x += x;
                candidate.mask.y += y;
                // Only the much larger lettering crossing this balloon can enclose this
                // new text. The normal disjoint pass gives the small block its glyphs.
                if self.0.iter().chain(&recovered).enumerate().any(|(i, old)| {
                    effect != Some(i)
                        && lettering(old)
                        && (share(candidate.bbox, old.bbox) > 0.2
                            || share(old.bbox, candidate.bbox) > 0.5)
                }) {
                    continue;
                }
                recovered.push(candidate);
            }
        }
        self.0.extend(recovered);
        Ok(())
    }
}

fn lettering(d: &Detection) -> bool {
    matches!(d.label.as_str(), "text" | "onomatopoeia")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::koharu_layout_highres::Mask;
    use imageproc::drawing::draw_filled_rect_mut;

    fn detection(label: &str, x: u32, y: u32, w: u32, h: u32) -> Detection {
        Detection {
            label: label.into(),
            score: 0.8,
            bbox: [x as f32, y as f32, (x + w) as f32, (y + h) as f32],
            mask: Mask {
                x,
                y,
                width: w,
                height: h,
                pixels: vec![255; (w * h) as usize],
            },
        }
    }

    #[test]
    fn a_missing_column_joins_its_balloon_once_in_reading_order() -> Result<()> {
        let mut page = RgbImage::from_pixel(160, 160, Rgb([255; 3]));
        let mut scene = Detected(vec![
            detection("bubble", 25, 35, 80, 85),
            detection("text", 82, 48, 10, 38),
            detection("text", 36, 64, 10, 38),
        ]);
        for left in [36, 60, 82] {
            for top in [68, 80] {
                draw_filled_rect_mut(&mut page, Rect::at(left, top).of_size(8, 6), Rgb([0; 3]));
            }
        }
        scene.recover_balloon_text(&page, |crop| {
            let pixels = crop.as_rgb8().unwrap();
            assert_eq!(pixels.get_pixel(12, 35).0, [255; 3]);
            assert_eq!(pixels.get_pixel(36, 35).0, [0; 3]);
            Ok(Detected(vec![
                detection("text", 33, 30, 14, 25),
                detection("text", 56, 14, 12, 38),
                detection("text", 25, 6, 15, 12),
            ]))
        })?;
        scene.recover_balloon_text(&page, |_| panic!("no remaining ink to search"))?;
        assert_eq!(scene.0.len(), 4);
        let result = scene.layout(&page);
        let middle = &result.lettering[1];
        assert_eq!(middle.bbox, [58., 65., 72., 90.]);
        assert_eq!(middle.bubble, Some(0));
        assert!(middle.mask.contains(63, 70));
        Ok(())
    }
}
