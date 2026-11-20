//! Export every page to a folder, a CBZ of PNGs, or a ZIP of JPEGs.

use std::{
    io::{BufWriter, Cursor, Write as _},
    path::Path,
    sync::Arc,
};

use anyhow::Context as _;
use image::{ImageFormat, Pixel as _, RgbImage, Rgba, codecs::jpeg::JpegEncoder};
use koharu_psd::Labels;
use koharu_renderer::{Frame, Renderer};
use koharu_scene::{PageId, Session};
use parking_lot::Mutex;
use serde::Deserialize;
use specta::Type;
use tauri::{State, Window};
use tauri_runtime_cef::CefRuntime;

use super::Result;
use crate::app::App;

#[derive(Debug, Deserialize, Type)]
#[serde(rename_all = "snake_case")]
pub enum ExportFormat {
    Png,
    Psd,
    Cbz,
    Zip { suffix: String },
}

/// Picks where the open project's pages go, and writes them there. `labels`
/// names a PSD's layers the scene names nothing, in the interface's language.
#[tauri::command]
#[specta::specta]
pub async fn export(
    app: State<'_, Arc<App>>,
    window: Window<CefRuntime>,
    format: ExportFormat,
    labels: Labels,
) -> Result<()> {
    let (name, session) = app
        .project
        .lock()
        .as_ref()
        .map(|project| (project.name.clone(), Arc::clone(&project.session)))
        .context("no project is open")?;
    let dialog = rfd::AsyncFileDialog::new().set_parent(&window);
    let destination = match &format {
        ExportFormat::Png | ExportFormat::Psd => dialog.pick_folder().await,
        ExportFormat::Cbz => {
            dialog
                .set_file_name(format!("{name}.cbz"))
                .add_filter("CBZ", &["cbz"])
                .save_file()
                .await
        }
        ExportFormat::Zip { suffix } => {
            dialog
                .set_file_name(format!("{name} [{suffix}].zip"))
                .add_filter("ZIP", &["zip"])
                .save_file()
                .await
        }
    };
    let Some(destination) = destination.map(|picked| picked.path().to_owned()) else {
        return Ok(());
    };
    let app = Arc::clone(&app);
    Ok(tokio::task::spawn_blocking(move || {
        write(&session, &app.renderer, format, &labels, &destination)
    })
    .await??)
}

fn write(
    session: &Mutex<Session>,
    renderer: &Renderer,
    format: ExportFormat,
    labels: &Labels,
    destination: &Path,
) -> anyhow::Result<()> {
    let pages: Vec<PageId> = session
        .lock()
        .project()
        .pages()
        .map(|page| page.id)
        .collect();
    let digits = pages.len().to_string().len().max(3);
    let mut archive = match format {
        ExportFormat::Cbz | ExportFormat::Zip { .. } => {
            let file = tempfile::NamedTempFile::new_in(
                destination.parent().context("missing archive directory")?,
            )?;
            Some(::zip::ZipWriter::new(BufWriter::new(file)))
        }
        ExportFormat::Png | ExportFormat::Psd => None,
    };
    for (index, &page) in pages.iter().enumerate() {
        let number = format!("{:0digits$}", index + 1);
        let frame = {
            let session = session.lock();
            let project = session.project();
            if let Some(background) = project.page(page)?.background
                && project.external_image(&background.file).is_some()
            {
                session.file(&background.file)?;
            }
            renderer.render(&session, page)?
        };
        match (&format, &mut archive) {
            (ExportFormat::Psd, _) => {
                let psd = koharu_psd::export(&session.lock(), renderer, page, labels)?;
                fs_err::write(destination.join(format!("{number}.psd")), psd)?;
            }
            (_, Some(archive)) => {
                let options = ::zip::write::SimpleFileOptions::default()
                    .compression_method(::zip::CompressionMethod::Stored);
                if matches!(format, ExportFormat::Zip { .. }) {
                    archive.start_file(format!("{number}.jpg"), options)?;
                    let rgba = frame.rasterize()?.image;
                    let rgb = RgbImage::from_fn(rgba.width(), rgba.height(), |x, y| {
                        let pixel = rgba.get_pixel(x, y);
                        let mut paper = Rgba([255; 4]);
                        paper.blend(pixel);
                        paper.to_rgb()
                    });
                    JpegEncoder::new_with_quality(archive, 95).encode_image(&rgb)?;
                } else {
                    archive.start_file(format!("{number}.png"), options)?;
                    archive.write_all(&png(&frame)?)?;
                }
            }
            (_, None) => {
                fs_err::write(destination.join(format!("{number}.png")), png(&frame)?)?;
            }
        }
    }
    if let Some(archive) = archive {
        archive.finish()?.into_inner()?.persist(destination)?;
    }
    Ok(())
}

fn png(frame: &Frame) -> anyhow::Result<Vec<u8>> {
    let raster = frame.rasterize()?;
    let mut png = Cursor::new(Vec::new());
    raster.image.write_to(&mut png, ImageFormat::Png)?;
    Ok(png.into_inner())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Read as _;

    #[test]
    fn jpeg_zip_numbers_rendered_pages_in_project_order_on_white() -> anyhow::Result<()> {
        let dir = tempfile::tempdir()?;
        let mut session = Session::create(dir.path().join("book.khr"))?;
        session.apply(serde_json::from_value(serde_json::json!({
            "label": "pages",
            "ops": [
                {"op":"add_page","id":"$ink","index":0,"size":{"width":64,"height":32}},
                {"op":"add_layer","page":"$ink","index":0,"layer":{"kind":"shape",
                 "path":"M0 0 H32 V32 H0 Z","fill":[0.0,0.0,0.0,0.5]}},
                {"op":"add_page","id":"$blank","index":0,"size":{"width":48,"height":24}}
            ]
        }))?)?;
        let destination = dir.path().join("test.zip");
        write(
            &Mutex::new(session),
            &Renderer::new(Default::default()),
            ExportFormat::Zip {
                suffix: "koharu机翻".into(),
            },
            &Labels {
                background: "Background".into(),
                cleanup: "Cleanup".into(),
                outline: "Outline".into(),
            },
            &destination,
        )?;
        let mut archive = zip::ZipArchive::new(std::fs::File::open(destination)?)?;
        assert_eq!(archive.len(), 2);
        for (i, size) in [(48, 24), (64, 32)].into_iter().enumerate() {
            let mut member = archive.by_index(i)?;
            assert_eq!(member.name()?, format!("{:03}.jpg", i + 1));
            let mut bytes = Vec::new();
            member.read_to_end(&mut bytes)?;
            assert_eq!(image::guess_format(&bytes)?, ImageFormat::Jpeg);
            let rgb = image::load_from_memory(&bytes)?.into_rgb8();
            assert_eq!(rgb.dimensions(), size);
            assert_eq!(rgb.get_pixel(40, 12).0, [255; 3]);
            if i == 1 {
                assert!(
                    rgb.get_pixel(16, 16)
                        .0
                        .iter()
                        .all(|v| (124..=131).contains(v))
                );
            }
        }
        Ok(())
    }
}
