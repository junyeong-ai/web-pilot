/// Stitch screenshot tiles into a single image.
pub fn stitch_tiles(
    tiles: &[serde_json::Value],
    output_dir: &std::path::Path,
) -> Result<std::path::PathBuf, String> {
    use std::io::Cursor;

    if tiles.is_empty() {
        return Err("no tiles".into());
    }

    let mut images: Vec<image::DynamicImage> = Vec::new();
    for (i, tile) in tiles.iter().enumerate() {
        let b64 = tile.as_str().ok_or(format!("tile {i} not a string"))?;
        let bytes = base64::Engine::decode(&base64::engine::general_purpose::STANDARD, b64)
            .map_err(|e| format!("tile {i} decode: {e}"))?;
        let img = image::ImageReader::new(Cursor::new(&bytes))
            .with_guessed_format()
            .map_err(|e| format!("tile {i} format: {e}"))?
            .decode()
            .map_err(|e| format!("tile {i} image: {e}"))?;
        images.push(img);
    }

    if images.is_empty() {
        return Err("no valid tiles".into());
    }

    let width = images[0].width();
    if images.iter().any(|i| i.width() != width) {
        return Err("Tile width mismatch — all tiles must have the same width".into());
    }
    let total_height: u32 = images.iter().map(|i| i.height()).sum();

    let mut canvas = image::RgbaImage::new(width, total_height);
    let mut y_offset = 0u32;
    for img in &images {
        image::imageops::overlay(&mut canvas, &img.to_rgba8(), 0, y_offset as i64);
        y_offset += img.height();
    }

    std::fs::create_dir_all(output_dir).map_err(|e| e.to_string())?;

    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis();
    let path = output_dir.join(format!("capture_full_{ts}.png"));

    image::DynamicImage::ImageRgba8(canvas)
        .save(&path)
        .map_err(|e| format!("save: {e}"))?;

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
