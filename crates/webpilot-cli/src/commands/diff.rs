use anyhow::{Context, Result};
use clap::Args;
use std::path::PathBuf;

use crate::output::OutputMode;

#[derive(Args)]
pub struct DiffArgs {
    /// Diff two DOM snapshots (JSON files)
    #[arg(long)]
    dom: bool,

    /// Diff two screenshots (PNG/JPEG files)
    #[arg(long)]
    screenshot: bool,

    /// First file (before)
    pub file_a: PathBuf,

    /// Second file (after)
    pub file_b: PathBuf,
}

pub async fn run(args: DiffArgs, output_mode: OutputMode) -> Result<()> {
    if args.dom {
        diff_dom(&args.file_a, &args.file_b, output_mode)
    } else if args.screenshot {
        diff_screenshot(&args.file_a, &args.file_b, output_mode)
    } else {
        // Default: detect by extension
        let ext = args
            .file_a
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("");
        match ext {
            "json" => diff_dom(&args.file_a, &args.file_b, output_mode),
            "png" | "jpg" | "jpeg" => diff_screenshot(&args.file_a, &args.file_b, output_mode),
            _ => anyhow::bail!("Cannot detect file type. Use --dom or --screenshot."),
        }
    }
}

fn diff_dom(a: &PathBuf, b: &PathBuf, output_mode: OutputMode) -> Result<()> {
    let text_a = std::fs::read_to_string(a).context("Cannot read file A")?;
    let text_b = std::fs::read_to_string(b).context("Cannot read file B")?;

    let diff = similar::TextDiff::from_lines(&text_a, &text_b);

    let mut added = 0u32;
    let mut removed = 0u32;
    let mut unchanged = 0u32;

    match output_mode {
        OutputMode::Human => {
            for change in diff.iter_all_changes() {
                let sign = match change.tag() {
                    similar::ChangeTag::Delete => {
                        removed += 1;
                        "-"
                    }
                    similar::ChangeTag::Insert => {
                        added += 1;
                        "+"
                    }
                    similar::ChangeTag::Equal => {
                        unchanged += 1;
                        " "
                    }
                };
                print!("{sign}{change}");
            }
            eprintln!("\n+{added} -{removed} ={unchanged}");
        }
        OutputMode::Json => {
            for change in diff.iter_all_changes() {
                match change.tag() {
                    similar::ChangeTag::Delete => removed += 1,
                    similar::ChangeTag::Insert => added += 1,
                    similar::ChangeTag::Equal => unchanged += 1,
                }
            }
            println!(
                "{}",
                serde_json::json!({
                    "added": added,
                    "removed": removed,
                    "unchanged": unchanged,
                    "diff": diff.unified_diff().header("before", "after").to_string(),
                })
            );
        }
    }

    Ok(())
}

fn diff_screenshot(a: &PathBuf, b: &PathBuf, output_mode: OutputMode) -> Result<()> {
    let img_a = image::open(a).context("Cannot open image A")?;
    let img_b = image::open(b).context("Cannot open image B")?;

    let (w, h) = (
        img_a.width().min(img_b.width()),
        img_a.height().min(img_b.height()),
    );
    let rgba_a = img_a.to_rgba8();
    let rgba_b = img_b.to_rgba8();

    let mut diff_count = 0u64;
    let total = (w as u64) * (h as u64);
    let mut diff_img = image::RgbaImage::new(w, h);

    for y in 0..h {
        for x in 0..w {
            let pa = rgba_a.get_pixel(x, y);
            let pb = rgba_b.get_pixel(x, y);
            let dist = ((pa[0] as i32 - pb[0] as i32).pow(2)
                + (pa[1] as i32 - pb[1] as i32).pow(2)
                + (pa[2] as i32 - pb[2] as i32).pow(2)) as f64;
            let dist = dist.sqrt();

            if dist > 30.0 {
                diff_count += 1;
                diff_img.put_pixel(x, y, image::Rgba([255, 0, 0, 200]));
            } else {
                let gray = ((pa[0] as u16 + pa[1] as u16 + pa[2] as u16) / 3) as u8;
                diff_img.put_pixel(x, y, image::Rgba([gray, gray, gray, 100]));
            }
        }
    }

    let pct = if total > 0 {
        (diff_count as f64 / total as f64) * 100.0
    } else {
        0.0
    };

    // Save diff image
    let diff_path = a.with_file_name("diff.png");
    diff_img
        .save(&diff_path)
        .context("Cannot save diff image")?;

    match output_mode {
        OutputMode::Human => {
            eprintln!("Changed: {:.1}% ({diff_count}/{total} pixels)", pct);
            eprintln!("Diff image: {}", diff_path.display());
        }
        OutputMode::Json => {
            println!(
                "{}",
                serde_json::json!({
                    "changed_percent": format!("{:.1}", pct),
                    "changed_pixels": diff_count,
                    "total_pixels": total,
                    "diff_image": diff_path.to_string_lossy(),
                })
            );
        }
    }

    Ok(())
}
