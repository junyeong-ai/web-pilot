/// Stitch screenshot tiles into a single image.
pub fn stitch_tiles(
    tiles: &[serde_json::Value],
    output_dir: &std::path::Path,
) -> anyhow::Result<std::path::PathBuf> {
    use std::io::Cursor;

    anyhow::ensure!(!tiles.is_empty(), "no tiles");

    let mut images: Vec<image::DynamicImage> = Vec::new();
    for (i, tile) in tiles.iter().enumerate() {
        let b64 = tile
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("tile {i} not a string"))?;
        let bytes = base64::Engine::decode(&base64::engine::general_purpose::STANDARD, b64)
            .map_err(|e| anyhow::anyhow!("tile {i} decode: {e}"))?;
        let img = image::ImageReader::new(Cursor::new(&bytes))
            .with_guessed_format()?
            .decode()?;
        images.push(img);
    }

    anyhow::ensure!(!images.is_empty(), "no valid tiles");

    let width = images[0].width();
    anyhow::ensure!(
        images.iter().all(|i| i.width() == width),
        "tile width mismatch"
    );
    let total_height: u32 = images.iter().map(|i| i.height()).sum();

    let mut canvas = image::RgbaImage::new(width, total_height);
    let mut y_offset = 0u32;
    for img in &images {
        image::imageops::overlay(&mut canvas, &img.to_rgba8(), 0, y_offset as i64);
        y_offset += img.height();
    }

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
