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
        // Clamp to at least 1px: an extreme aspect ratio can round the short
        // edge to 0, which would make the resize step fail.
        (
            ((orig_w as f64 * scale).round() as u32).max(1),
            ((orig_h as f64 * scale).round() as u32).max(1),
        )
    } else {
        (orig_w, orig_h)
    };

    // Always emit PNG bytes for the `.png` artifact — browser-mode captures
    // arrive as JPEG, so writing the raw bytes would mislabel the file.
    let final_bytes = if new_w != orig_w || new_h != orig_h {
        resize_png(&img, new_w, new_h)?
    } else {
        encode_png(&img)?
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

/// Encode a decoded image as PNG without resizing, so a `.png` artifact is
/// always a valid PNG even when the source bytes were JPEG.
fn encode_png(img: &image::DynamicImage) -> Result<Vec<u8>, ScreenshotError> {
    use image::codecs::png::PngEncoder;
    let mut buf = Vec::new();
    img.write_with_encoder(PngEncoder::new(&mut buf))
        .map_err(|e| ScreenshotError::Save(e.to_string()))?;
    Ok(buf)
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

#[cfg(test)]
mod tests {
    use super::*;
    use base64::Engine;

    fn encode_b64(img: &image::DynamicImage, format: image::ImageFormat) -> String {
        let mut buf = Cursor::new(Vec::new());
        img.write_to(&mut buf, format).unwrap();
        base64::engine::general_purpose::STANDARD.encode(buf.into_inner())
    }

    /// Browser mode captures JPEG; the saved `.png` artifact must be real PNG.
    #[test]
    fn jpeg_capture_is_saved_as_png() {
        let dir = std::env::temp_dir().join(format!("wp_shot_jpeg_{}", std::process::id()));
        let img = image::DynamicImage::ImageRgb8(image::RgbImage::from_pixel(
            120,
            80,
            image::Rgb([200, 100, 50]),
        ));
        let b64 = encode_b64(&img, image::ImageFormat::Jpeg);

        let info = process_and_save(&b64, &dir).unwrap();

        let bytes = std::fs::read(&info.path).unwrap();
        assert_eq!(
            image::guess_format(&bytes).unwrap(),
            image::ImageFormat::Png,
            "JPEG capture must be re-encoded as PNG, not written with a .png label"
        );
        assert!(info.path.extension().is_some_and(|e| e == "png"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// An extreme aspect ratio must not round the short edge to 0.
    #[test]
    fn extreme_aspect_ratio_keeps_min_dimension() {
        let dir = std::env::temp_dir().join(format!("wp_shot_thin_{}", std::process::id()));
        let img = image::DynamicImage::ImageRgb8(image::RgbImage::from_pixel(
            5000,
            1,
            image::Rgb([0, 0, 0]),
        ));
        let b64 = encode_b64(&img, image::ImageFormat::Png);

        let info = process_and_save(&b64, &dir).unwrap();

        assert_eq!(info.width, MAX_LONG_EDGE);
        assert!(info.height >= 1, "short edge must clamp to at least 1px");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// An oversized capture is downscaled to the long-edge cap and stays PNG.
    #[test]
    fn oversized_capture_is_resized_and_png() {
        let dir = std::env::temp_dir().join(format!("wp_shot_big_{}", std::process::id()));
        let img = image::DynamicImage::ImageRgb8(image::RgbImage::from_pixel(
            3000,
            1000,
            image::Rgb([10, 20, 30]),
        ));
        let b64 = encode_b64(&img, image::ImageFormat::Png);

        let info = process_and_save(&b64, &dir).unwrap();

        assert_eq!(info.width, MAX_LONG_EDGE);
        let bytes = std::fs::read(&info.path).unwrap();
        assert_eq!(
            image::guess_format(&bytes).unwrap(),
            image::ImageFormat::Png
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
