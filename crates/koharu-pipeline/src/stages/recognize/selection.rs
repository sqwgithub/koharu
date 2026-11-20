use std::sync::Arc;

use anyhow::{Result, ensure};
use image::{DynamicImage, Rgb};
use koharu_scene::{Content, Layer, Op, Page, Ref};
use kurbo::{BezPath, Rect, Shape as _};
use serde_json::json;

use crate::stages::{AreaWork, Task, action, mask_hash};
use crate::{Area, PipelineConfig, Stage};

/// A manual selection supplies the text region even when layout detection found nothing.
pub(crate) async fn selection(
    task: &Task,
    config: &PipelineConfig,
    work: Arc<AreaWork>,
) -> Result<()> {
    let member = &task.members[0];
    let page_id = member.page;
    let read = member.clone();
    let selected = work.clone();
    let Some((background, crop)) = tokio::task::spawn_blocking(move || -> Result<_> {
        let Some((project, scan)) = read.scan()? else {
            return Ok(None);
        };
        let page = project.page(page_id)?;
        let Some(crop) = crop(&scan, &selected.area)? else {
            return Ok(None);
        };
        Ok(Some((page.background.map(|scan| scan.file), crop)))
    })
    .await??
    else {
        return Ok(());
    };
    let (texts, language) = super::infer(task, config, &[member], vec![crop]).await?;
    let model = config.recognizer.model.to_string();
    let text = texts.into_iter().next().expect("one crop was recognized");
    member
        .apply(Stage::Recognize, move |project| {
            let page = project.page(page_id)?;
            if text.trim().is_empty() || page.background.map(|scan| scan.file) != background {
                return Ok(Vec::new());
            }
            source_block(page, &work.area, &text, language, &model)
        })
        .await
}

fn crop(scan: &DynamicImage, area: &Area) -> Result<Option<DynamicImage>> {
    let r = area.rect;
    ensure!(
        r.x >= 0
            && r.y >= 0
            && i64::from(r.x) + i64::from(r.width) <= i64::from(scan.width())
            && i64::from(r.y) + i64::from(r.height) <= i64::from(scan.height()),
        "OCR selection is outside the scan"
    );
    if r.width == 0 || r.height == 0 {
        return Ok(None);
    }
    let mut image = scan
        .crop_imm(r.x as u32, r.y as u32, r.width, r.height)
        .into_rgb8();
    let mut first = None;
    let mut varied = false;
    for (pixel, &coverage) in image.pixels_mut().zip(&area.pixels) {
        if coverage == 0 {
            *pixel = Rgb([255; 3]);
        } else {
            varied |= first.get_or_insert(*pixel) != pixel;
        }
    }
    Ok(varied.then_some(DynamicImage::ImageRgb8(image)))
}

fn source_block(
    page: &Page,
    area: &Area,
    text: &str,
    language: &str,
    model: &str,
) -> Result<Vec<Op>> {
    let rect = area.rect;
    let bounds = Rect::from_origin_size(
        (f64::from(rect.x), f64::from(rect.y)),
        (f64::from(rect.width), f64::from(rect.height)),
    );
    let center = bounds.center();
    let vertical = rect.height > rect.width;
    let mut parent = None;
    let mut siblings = page.layers.as_slice();
    // A selection inside a balloon belongs beside the other sources in that balloon.
    while let Some(layer) = siblings
        .iter()
        .filter(|l| l.visible)
        .filter_map(|l| {
            let path = match &l.content {
                Content::Panel(panel) => &panel.outline,
                Content::Balloon(balloon) => &balloon.outline,
                _ => return None,
            };
            let path = BezPath::from(path);
            path.contains(center).then(|| (l, path.area().abs()))
        })
        .min_by(|a, b| a.1.total_cmp(&b.1))
        .map(|(l, _)| l)
    {
        parent = Some(layer.id);
        siblings = &layer.children;
    }
    let frame = |layer: &Layer| match &layer.content {
        Content::Block(block) => block.source.bounds(),
        _ => None,
    };
    let selection = json!(mask_hash(rect, &area.pixels));
    if siblings.iter().any(|l| {
        frame(l) == Some(bounds)
            && l.attributes
                .get("koharu")
                .and_then(|attributes| attributes.get("ocr_selection"))
                == Some(&selection)
    }) {
        return Ok(Vec::new());
    }
    let index = siblings
        .iter()
        .position(|l| {
            frame(l).is_some_and(|other| {
                if vertical && other.y0 < bounds.y1 && bounds.y0 < other.y1 {
                    center.x > other.center().x
                } else if !vertical && other.y0 < center.y && center.y < other.y1 {
                    center.x < other.center().x
                } else {
                    center.y < other.center().y
                }
            })
        })
        .unwrap_or(siblings.len());
    Ok(vec![
        serde_json::from_value(json!({
            "op":"add_layer", "id":"$selection", "page":page.id, "parent":parent, "index":index,
            "layer":{"kind":"block","source":{"content":[{"insert":text}],"language":language,
                "direction":if vertical {"vertical"} else {"horizontal"}, "frame":{"box":{"bounds":rect}}},
                "attributes":{"koharu":{"ocr_selection":selection},"c2pa":{"ocr":action("c2pa.created",model,None)}}}
        }))?,
        Op::WritePixels {
            layer: Ref::try_from("$selection.mask".to_owned()).map_err(anyhow::Error::msg)?,
            rect,
            pixels: area.pixels.clone(),
        },
    ])
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::RgbImage;
    use koharu_scene::{Edit, Rect as PixelRect, Session};

    #[test]
    fn selection_ocr_sees_only_the_marked_scan_pixels() -> Result<()> {
        let mut scan = RgbImage::from_pixel(10, 8, Rgb([255; 3]));
        for x in [1, 3, 4] {
            scan.put_pixel(x, 3, Rgb([0; 3]));
        }
        let scan = DynamicImage::ImageRgb8(scan);
        let mut area = Area {
            rect: PixelRect {
                x: 3,
                y: 3,
                width: 3,
                height: 1,
            },
            pixels: vec![255, 0, 255],
        };
        let image = crop(&scan, &area)?.unwrap().into_rgb8();
        assert_eq!(image.dimensions(), (3, 1));
        assert_eq!(image.as_raw(), &[0, 0, 0, 255, 255, 255, 255, 255, 255]);
        area.pixels[0] = 0;
        assert!(crop(&scan, &area)?.is_none());
        Ok(())
    }

    #[test]
    fn selection_ocr_inserts_one_undoable_block_between_its_neighbors() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let mut session = Session::create(dir.path().join("test.khr"))?;
        let neighbor = |x, index| {
            json!({
                "op":"add_layer","page":"$page","parent":"$balloon","index":index,
                "layer":{"kind":"block","source":{"content":"existing","frame":{
                    "box":{"bounds":{"x":x,"y":20,"width":10,"height":30}}}}}
            })
        };
        let ops = [
            json!({"op":"add_page","id":"$page","index":0,"size":{"width":100,"height":100}}),
            json!({"op":"add_layer","id":"$balloon","page":"$page","index":0,
                "layer":{"kind":"balloon","outline":"M0 0L90 0L90 90L0 90Z"}}),
            neighbor(65, 0),
            neighbor(20, 1),
        ]
        .into_iter()
        .map(serde_json::from_value)
        .collect::<Result<Vec<_>, _>>()?;
        let page = session
            .apply(Edit {
                label: "setup".into(),
                ops,
            })?
            .pages
            .added[0];
        let before = session.project();
        let original = before.story(page)?;
        let area = Area {
            rect: PixelRect {
                x: 44,
                y: 20,
                width: 10,
                height: 30,
            },
            pixels: vec![255; 300],
        };
        let ops = source_block(before.page(page)?, &area, "sample", "ja", "model")?;
        session.apply(Edit {
            label: "recognize".into(),
            ops,
        })?;
        let after = session.project();
        let story = after.story(page)?;
        assert_eq!(story, [original[0], story[1], original[1]]);
        assert_eq!(after.page(page)?.layers[0].children[1].id, story[1]);
        for id in original {
            assert_eq!(before.layer(id)?, after.layer(id)?);
        }
        let Content::Block(block) = &after.layer(story[1])?.content else {
            panic!("expected a source block")
        };
        assert_eq!(block.source.content.text(), "sample");
        let (bounds, coverage) = session
            .tiles(block.mask.id, None)?
            .coverage(100, 100)
            .unwrap();
        assert_eq!(bounds, area.rect);
        assert_eq!(coverage.as_raw(), &area.pixels);
        assert!(source_block(after.page(page)?, &area, "again", "ja", "model")?.is_empty());
        session.undo()?;
        assert_eq!(session.project().page(page)?, before.page(page)?);
        Ok(())
    }
}
