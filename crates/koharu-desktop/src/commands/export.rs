//! Export every page to a folder, a CBZ of PNGs, or a ZIP of JPEGs.

use std::{
    io::{BufWriter, Write},
    path::Path,
    sync::Arc,
};

use anyhow::Context as _;
use fast_image_resize::Resizer;
use image::{
    ExtendedColorType, ImageEncoder as _, Pixel as _, RgbImage, Rgba, RgbaImage,
    codecs::{
        jpeg::JpegEncoder,
        png::{CompressionType, PngEncoder},
    },
};
use koharu_psd::Labels;
use koharu_renderer::Renderer;
use koharu_scene::{PageId, Session};
use parking_lot::Mutex;
use serde::{Deserialize, Deserializer, Serialize};
use specta::Type;
use tauri::{State, Window};
use tauri_runtime_cef::CefRuntime;

use super::Result;
use crate::app::App;

#[derive(Clone, Debug, Serialize, Deserialize, Type)]
#[serde(default)]
pub struct OutputConfig {
    #[serde(deserialize_with = "percent")]
    #[specta(type = u8)]
    jpeg_quality: u8,
    #[serde(deserialize_with = "percent")]
    #[specta(type = u8)]
    scale_percent: u8,
    auto_compress: bool,
}

impl Default for OutputConfig {
    fn default() -> Self {
        Self {
            jpeg_quality: 95,
            scale_percent: 100,
            auto_compress: false,
        }
    }
}

impl koharu_config::Section for OutputConfig {
    const NAME: &'static str = "output";
}

fn percent<'de, D: Deserializer<'de>>(input: D) -> std::result::Result<u8, D::Error> {
    let value = u8::deserialize(input)?;
    if (1..=100).contains(&value) {
        Ok(value)
    } else {
        Err(serde::de::Error::custom(
            "expected an integer from 1 to 100",
        ))
    }
}

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
        let output = koharu_config::get()?;
        write(
            &session,
            &app.renderer,
            format,
            &labels,
            &destination,
            &output,
        )
    })
    .await??)
}

fn write(
    session: &Mutex<Session>,
    renderer: &Renderer,
    format: ExportFormat,
    labels: &Labels,
    destination: &Path,
    output: &OutputConfig,
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
            if matches!(format, ExportFormat::Psd) {
                let psd = koharu_psd::export(&session, renderer, page, labels)?;
                drop(session);
                fs_err::write(destination.join(format!("{number}.psd")), psd)?;
                continue;
            }
            renderer.render(&session, page)?
        };
        let rgba = output.scale(frame.rasterize()?.image)?;
        if let Some(archive) = archive.as_mut() {
            let options = ::zip::write::SimpleFileOptions::default()
                .compression_method(::zip::CompressionMethod::Stored);
            if matches!(format, ExportFormat::Zip { .. }) {
                archive.start_file(format!("{number}.jpg"), options)?;
                output.jpeg(&rgba, archive)?;
            } else {
                archive.start_file(format!("{number}.png"), options)?;
                output.png(&rgba, archive)?;
            }
        } else {
            let mut bytes = Vec::new();
            output.png(&rgba, &mut bytes)?;
            fs_err::write(destination.join(format!("{number}.png")), bytes)?;
        }
    }
    if let Some(archive) = archive {
        archive.finish()?.into_inner()?.persist(destination)?;
    }
    Ok(())
}

impl OutputConfig {
    fn scale(&self, image: RgbaImage) -> anyhow::Result<RgbaImage> {
        if self.scale_percent == 100 {
            return Ok(image);
        }
        let scaled = |n| ((u64::from(n) * u64::from(self.scale_percent) + 50) / 100).max(1) as u32;
        let mut result = RgbaImage::new(scaled(image.width()), scaled(image.height()));
        Resizer::new().resize(&image, &mut result, None)?;
        Ok(result)
    }

    fn png(&self, image: &RgbaImage, writer: impl Write) -> image::ImageResult<()> {
        let compression = if self.auto_compress {
            CompressionType::Best
        } else {
            CompressionType::default()
        };
        PngEncoder::new_with_quality(writer, compression, Default::default()).write_image(
            image,
            image.width(),
            image.height(),
            ExtendedColorType::Rgba8,
        )
    }

    fn jpeg(&self, image: &RgbaImage, mut writer: impl Write) -> anyhow::Result<()> {
        let rgb = RgbImage::from_fn(image.width(), image.height(), |x, y| {
            let mut paper = Rgba([255; 4]);
            paper.blend(image.get_pixel(x, y));
            paper.to_rgb()
        });
        if !self.auto_compress {
            JpegEncoder::new_with_quality(writer, self.jpeg_quality).encode_image(&rgb)?;
            return Ok(());
        }
        // A soft per-page target: keep the selected resolution, and limit the
        // quality reduction so that fine text survives on very dense pages.
        let mut quality = self.jpeg_quality;
        let floor = quality.min(60);
        let mut smallest = Vec::new();
        loop {
            let mut candidate = Vec::new();
            JpegEncoder::new_with_quality(&mut candidate, quality).encode_image(&rgb)?;
            if smallest.is_empty() || candidate.len() < smallest.len() {
                smallest = candidate;
            }
            if smallest.len() <= 1024 * 1024 || quality == floor {
                break;
            }
            quality = quality.saturating_sub(5).max(floor);
        }
        writer.write_all(&smallest)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::ImageFormat;
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
        let session = Mutex::new(session);
        let renderer = Renderer::new(Default::default());
        let labels = Labels {
            background: "Background".into(),
            cleanup: "Cleanup".into(),
            outline: "Outline".into(),
        };
        let export = |format, dest: &Path, output: &OutputConfig| {
            write(&session, &renderer, format, &labels, dest, output)
        };
        let zip = || ExportFormat::Zip {
            suffix: "koharu机翻".into(),
        };
        export(zip(), &destination, &OutputConfig::default())?;
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
        let output = OutputConfig {
            scale_percent: 50,
            jpeg_quality: 72,
            auto_compress: true,
        };
        export(ExportFormat::Png, dir.path(), &output)?;
        let png = fs_err::read(dir.path().join("002.png"))?;
        let scaled = image::load_from_memory(&png)?.into_rgba8();
        assert_eq!(scaled.dimensions(), (32, 16));
        assert_eq!(scaled.get_pixel(25, 8)[3], 0);
        let cbz = dir.path().join("scaled.cbz");
        export(ExportFormat::Cbz, &cbz, &output)?;
        let mut comics = zip::ZipArchive::new(std::fs::File::open(cbz)?)?;
        let mut bytes = Vec::new();
        comics.by_name("002.png")?.read_to_end(&mut bytes)?;
        assert_eq!(bytes, png);

        let resized_zip = dir.path().join("scaled.zip");
        export(zip(), &resized_zip, &output)?;
        let mut jpeg_archive = zip::ZipArchive::new(std::fs::File::open(resized_zip)?)?;
        bytes.clear();
        jpeg_archive.by_name("002.jpg")?.read_to_end(&mut bytes)?;
        let mut expected = Vec::new();
        OutputConfig {
            auto_compress: false,
            ..output.clone()
        }
        .jpeg(&scaled, &mut expected)?;
        assert_eq!(bytes, expected);
        let mut original_quality = Vec::new();
        OutputConfig::default().jpeg(&scaled, &mut original_quality)?;
        assert_ne!(expected, original_quality);

        export(ExportFormat::Psd, dir.path(), &output)?;
        let psd = fs_err::read(dir.path().join("002.psd"))?;
        assert_eq!(u32::from_be_bytes(psd[14..18].try_into()?), 32);
        assert_eq!(u32::from_be_bytes(psd[18..22].try_into()?), 64);
        Ok(())
    }
    #[test]
    fn auto_compression_reduces_an_oversized_jpeg_at_the_selected_resolution() -> anyhow::Result<()>
    {
        let mut random = 1_u32;
        let page = RgbaImage::from_fn(1024, 1024, |_, _| {
            random ^= random << 13;
            random ^= random >> 17;
            random ^= random << 5;
            let [r, g, b, _] = random.to_le_bytes();
            Rgba([r, g, b, 255])
        });
        let mut manual = Vec::new();
        let mut auto = Vec::new();
        OutputConfig::default().jpeg(&page, &mut manual)?;
        OutputConfig {
            auto_compress: true,
            ..Default::default()
        }
        .jpeg(&page, &mut auto)?;
        assert!(manual.len() > 1024 * 1024);
        assert!(auto.len() <= 1024 * 1024);
        assert_eq!(
            image::load_from_memory(&auto)?.into_rgb8().dimensions(),
            page.dimensions()
        );
        Ok(())
    }
}
