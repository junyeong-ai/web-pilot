//! Screenshot processing: decode base64, resize, save to file.

use std::io::Cursor;
use std::path::{Path, PathBuf};
use thiserror::Error;

const MAX_LONG_EDGE: u32 = 1568;

#[derive(Debug, Error)]
pub enum ScreenshotError {
    #[error("decode failed: {0}")]
    Decode(String),
    #[error("resize failed: {0}")]
    Resize(String),
    #[error("save failed: {0}")]
    Save(String),
}

pub struct ScreenshotResult {
    pub path: PathBuf,
    pub width: u32,
    pub height: u32,
    pub bytes: usize,
    pub estimated_tokens: u32,
}

/// Process a base64-encoded screenshot: decode, resize, save to file.
pub fn process_and_save(
    b64_data: &str,
    output_dir: &Path,
) -> Result<ScreenshotResult, ScreenshotError> {
    // Decode base64
    let raw = base64::Engine::decode(&base64::engine::general_purpose::STANDARD, b64_data)
        .map_err(|e| ScreenshotError::Decode(e.to_string()))?;

    // Decode image
    let img = image::ImageReader::new(Cursor::new(&raw))
        .with_guessed_format()
        .map_err(|e| ScreenshotError::Decode(e.to_string()))?
        .decode()
        .map_err(|e| ScreenshotError::Decode(e.to_string()))?;

    let (orig_w, orig_h) = (img.width(), img.height());

    // Resize if needed
    let long_edge = orig_w.max(orig_h);
    let (new_w, new_h) = if long_edge > MAX_LONG_EDGE {
        let scale = MAX_LONG_EDGE as f64 / long_edge as f64;
        (
            (orig_w as f64 * scale).round() as u32,
            (orig_h as f64 * scale).round() as u32,
        )
    } else {
        (orig_w, orig_h)
    };

    let final_bytes = if new_w != orig_w || new_h != orig_h {
        resize_png(&img, new_w, new_h)?
    } else {
        raw
    };

    // Save to file
    std::fs::create_dir_all(output_dir).map_err(|e| ScreenshotError::Save(e.to_string()))?;

    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis();
    let filename = format!("capture_{ts}.png");
    let path = output_dir.join(&filename);

    std::fs::write(&path, &final_bytes).map_err(|e| ScreenshotError::Save(e.to_string()))?;

    let estimated_tokens = (new_w * new_h) / 750;

    Ok(ScreenshotResult {
        path,
        width: new_w,
        height: new_h,
        bytes: final_bytes.len(),
        estimated_tokens,
    })
}

fn resize_png(img: &image::DynamicImage, w: u32, h: u32) -> Result<Vec<u8>, ScreenshotError> {
    use fast_image_resize as fir;
    use image::codecs::png::PngEncoder;

    let src_rgba = img.to_rgba8();
    let (sw, sh) = src_rgba.dimensions();

    let src = fir::images::Image::from_vec_u8(sw, sh, src_rgba.into_raw(), fir::PixelType::U8x4)
        .map_err(|e| ScreenshotError::Resize(e.to_string()))?;

    let mut dst = fir::images::Image::new(w, h, fir::PixelType::U8x4);

    let mut resizer = fir::Resizer::new();
    resizer
        .resize(
            &src,
            &mut dst,
            &fir::ResizeOptions::new()
                .resize_alg(fir::ResizeAlg::Convolution(fir::FilterType::Lanczos3)),
        )
        .map_err(|e| ScreenshotError::Resize(e.to_string()))?;

    let rgba = image::RgbaImage::from_raw(w, h, dst.into_vec())
        .ok_or_else(|| ScreenshotError::Resize("buffer mismatch".into()))?;

    let mut buf = Vec::new();
    image::DynamicImage::ImageRgba8(rgba)
        .write_with_encoder(PngEncoder::new(&mut buf))
        .map_err(|e| ScreenshotError::Resize(e.to_string()))?;

    Ok(buf)
}
