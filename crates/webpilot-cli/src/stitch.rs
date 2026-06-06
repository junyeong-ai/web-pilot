/// Stitch full-page screenshot tiles into a single image.
///
/// The extension captures each tile as a full viewport via `captureVisibleTab`
/// and scrolls one viewport between shots. The browser clamps the final scroll
/// at the page bottom, so the last tile overlaps the previous one. Given the
/// page's CSS `viewport_height` and `total_height`, each tile is placed at its
/// true scroll offset (`min(i·viewport, total − viewport)`) in device pixels,
/// and the canvas is cropped to the real page height — overlapping rows are
/// overwritten with identical content rather than duplicated.
///
/// Without that metadata (uniform tiles, no clamp info) the tiles are stacked
/// edge to edge.
pub fn stitch_tiles(
    tiles: &[serde_json::Value],
    viewport_height: Option<f64>,
    total_height: Option<f64>,
    output_dir: &std::path::Path,
) -> anyhow::Result<std::path::PathBuf> {
    use std::io::Cursor;

    anyhow::ensure!(!tiles.is_empty(), "no tiles");

    let mut images: Vec<image::RgbaImage> = Vec::new();
    for (i, tile) in tiles.iter().enumerate() {
        let b64 = tile
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("tile {i} not a string"))?;
        let bytes = base64::Engine::decode(&base64::engine::general_purpose::STANDARD, b64)
            .map_err(|e| anyhow::anyhow!("tile {i} decode: {e}"))?;
        let img = image::ImageReader::new(Cursor::new(&bytes))
            .with_guessed_format()?
            .decode()?;
        images.push(img.to_rgba8());
    }

    anyhow::ensure!(!images.is_empty(), "no valid tiles");

    let width = images[0].width();
    anyhow::ensure!(
        images.iter().all(|i| i.width() == width),
        "tile width mismatch"
    );

    let (canvas, height) = match tile_layout(&images, viewport_height, total_height) {
        Some(layout) => layout,
        None => stack_layout(&images, width),
    };
    let total_height = height;

    std::fs::create_dir_all(output_dir)?;

    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis();
    let path = output_dir.join(format!("capture_full_{ts}.png"));

    image::DynamicImage::ImageRgba8(canvas).save(&path)?;

    eprintln!(
        "Stitched {} tiles → {}x{} ({}KB)",
        tiles.len(),
        width,
        total_height,
        std::fs::metadata(&path)
            .map(|m| m.len() / 1024)
            .unwrap_or(0)
    );

    Ok(path)
}

/// Place each full-viewport tile at its true (clamped) scroll offset, cropping
/// the canvas to the real page height. Returns `None` when the metadata is
/// absent or unusable so the caller falls back to edge-to-edge stacking.
fn tile_layout(
    images: &[image::RgbaImage],
    viewport_height: Option<f64>,
    total_height: Option<f64>,
) -> Option<(image::RgbaImage, u32)> {
    let viewport = viewport_height?;
    let total = total_height?;
    if viewport <= 0.0 || total <= 0.0 {
        return None;
    }

    let width = images[0].width();
    let tile_px = images[0].height();
    // Uniform tiles are the contract; bail to stacking if the device clipped one.
    if tile_px == 0 || !images.iter().all(|i| i.height() == tile_px) {
        return None;
    }

    // Precise cropping only applies when the tiles actually span the page: the
    // extension caps tiling (and a tile can fail to capture), so a short set
    // covers only the top and must be stacked, not cropped to the full height
    // (which would leave a transparent tail).
    let expected = (total / viewport).ceil() as usize;
    if images.len() != expected {
        return None;
    }

    // Device pixels per CSS pixel (devicePixelRatio), derived from the tile.
    let scale = tile_px as f64 / viewport;
    let canvas_height = ((total * scale).round() as u32).max(1);
    let max_offset = (total - viewport).max(0.0);

    let mut canvas = image::RgbaImage::new(width, canvas_height);
    for (i, img) in images.iter().enumerate() {
        let css_offset = (i as f64 * viewport).min(max_offset);
        let y = (css_offset * scale).round() as i64;
        image::imageops::overlay(&mut canvas, img, 0, y);
    }
    Some((canvas, canvas_height))
}

/// Stack tiles edge to edge — used when no clamp metadata is available.
fn stack_layout(images: &[image::RgbaImage], width: u32) -> (image::RgbaImage, u32) {
    let height: u32 = images.iter().map(|i| i.height()).sum();
    let mut canvas = image::RgbaImage::new(width, height);
    let mut y = 0i64;
    for img in images {
        image::imageops::overlay(&mut canvas, img, 0, y);
        y += img.height() as i64;
    }
    (canvas, height)
}

#[cfg(test)]
mod tests {
    use super::stitch_tiles;
    use base64::Engine;

    fn png_tile(w: u32, h: u32) -> serde_json::Value {
        let img = image::RgbaImage::from_pixel(w, h, image::Rgba([10, 20, 30, 255]));
        let mut buf = std::io::Cursor::new(Vec::new());
        image::DynamicImage::ImageRgba8(img)
            .write_to(&mut buf, image::ImageFormat::Png)
            .unwrap();
        serde_json::Value::String(
            base64::engine::general_purpose::STANDARD.encode(buf.into_inner()),
        )
    }

    #[test]
    fn stacks_tiles_when_no_metadata() {
        let dir = std::env::temp_dir().join(format!("wp_stitch_{}", std::process::id()));
        let tiles = vec![png_tile(40, 30), png_tile(40, 20)];
        let out = stitch_tiles(&tiles, None, None, &dir).unwrap();
        let img = image::open(&out).unwrap();
        assert_eq!(img.width(), 40);
        assert_eq!(img.height(), 50); // 30 + 20 stacked edge to edge
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn crops_clamped_overlap_to_page_height() {
        // 3 full-viewport tiles (600px) over a 1500px page at dpr 1.0. The last
        // scroll clamps at 900px, so tile 3 overlaps tile 2 by 300px. The
        // stitched canvas must be exactly 1500px, not 1800px.
        let dir = std::env::temp_dir().join(format!("wp_crop_{}", std::process::id()));
        let tiles = vec![png_tile(40, 600), png_tile(40, 600), png_tile(40, 600)];
        let out = stitch_tiles(&tiles, Some(600.0), Some(1500.0), &dir).unwrap();
        let img = image::open(&out).unwrap();
        assert_eq!(img.width(), 40);
        assert_eq!(img.height(), 1500);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn crops_with_device_pixel_ratio() {
        // dpr 2.0: a 500px-tall page captured in one 1000px-device-pixel tile.
        let dir = std::env::temp_dir().join(format!("wp_dpr_{}", std::process::id()));
        let tiles = vec![png_tile(80, 1000)];
        let out = stitch_tiles(&tiles, Some(500.0), Some(500.0), &dir).unwrap();
        let img = image::open(&out).unwrap();
        assert_eq!(img.height(), 1000); // 500 css * dpr 2.0
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn crops_short_page_below_viewport() {
        // A 300px page captured in one 600px viewport tile must crop to 300px,
        // not stay at the full viewport height.
        let dir = std::env::temp_dir().join(format!("wp_short_{}", std::process::id()));
        let tiles = vec![png_tile(40, 600)];
        let out = stitch_tiles(&tiles, Some(600.0), Some(300.0), &dir).unwrap();
        let img = image::open(&out).unwrap();
        assert_eq!(img.height(), 300);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn stacks_when_tiles_do_not_span_page() {
        // The extension caps tiling: a 5000px page reported but only 2 tiles
        // captured must stack (no transparent tail), not crop to 5000px.
        let dir = std::env::temp_dir().join(format!("wp_capped_{}", std::process::id()));
        let tiles = vec![png_tile(40, 600), png_tile(40, 600)];
        let out = stitch_tiles(&tiles, Some(600.0), Some(5000.0), &dir).unwrap();
        let img = image::open(&out).unwrap();
        assert_eq!(img.height(), 1200); // 2 tiles stacked, page height ignored
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn rejects_width_mismatch() {
        let dir = std::env::temp_dir();
        assert!(stitch_tiles(&[png_tile(40, 10), png_tile(50, 10)], None, None, &dir).is_err());
    }

    #[test]
    fn rejects_empty() {
        assert!(stitch_tiles(&[], None, None, &std::env::temp_dir()).is_err());
    }
}
