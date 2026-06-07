use anyhow::{Context, Result};
use clap::Args;
use std::path::{Path, PathBuf};

use crate::output::CommandOutput;

/// Per-pixel Euclidean RGB distance above which a pixel counts as changed.
/// Coarse by design — anti-aliasing and JPEG artifacts sit just below typical
/// edge deltas, so small thresholds drown real diffs in rendering noise. This
/// is a reporting aid, not a gate; treat `changed_percent` as approximate.
const PIXEL_DIFF_THRESHOLD: f64 = 30.0;

/// Refuse to load a diff input larger than this. The inputs are arbitrary
/// user-named files, and both the DOM diff and the image decode pull the whole
/// file into memory — a multi-GB path would OOM the process before any typed
/// error could be returned. A snapshot or screenshot never approaches this;
/// anything that does is not a WebPilot artifact.
const MAX_DIFF_INPUT_BYTES: u64 = 512 * 1024 * 1024;

/// Read a diff input fully, but never more than the cap. The read is bounded by
/// the OPENED handle (`take`), not a prior `stat`, so a file that grows between
/// a size check and the read can't slip past — the read itself stops at the
/// limit and fails typed.
fn read_capped(path: &std::path::Path, label: &str) -> Result<Vec<u8>> {
    use std::io::Read as _;
    let file = std::fs::File::open(path).with_context(|| format!("Cannot open {label}"))?;
    let mut buf = Vec::new();
    file.take(MAX_DIFF_INPUT_BYTES + 1)
        .read_to_end(&mut buf)
        .with_context(|| format!("Cannot read {label}"))?;
    if buf.len() as u64 > MAX_DIFF_INPUT_BYTES {
        return Err(webpilot::WebPilotError::InvalidArgument {
            detail: format!("{label} exceeds the {MAX_DIFF_INPUT_BYTES}-byte diff limit"),
        }
        .into());
    }
    Ok(buf)
}

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

pub async fn run(args: DiffArgs) -> Result<CommandOutput> {
    if args.dom {
        diff_dom(&args.file_a, &args.file_b)
    } else if args.screenshot {
        diff_screenshot(&args.file_a, &args.file_b)
    } else {
        // Default: detect by extension
        let ext = args
            .file_a
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("");
        match ext {
            "json" => diff_dom(&args.file_a, &args.file_b),
            "png" | "jpg" | "jpeg" => diff_screenshot(&args.file_a, &args.file_b),
            _ => anyhow::bail!("Cannot detect file type. Use --dom or --screenshot."),
        }
    }
}

fn diff_dom(a: &Path, b: &Path) -> Result<CommandOutput> {
    let text_a = String::from_utf8(read_capped(a, "file A")?)
        .context("file A is not valid UTF-8")?;
    let text_b = String::from_utf8(read_capped(b, "file B")?)
        .context("file B is not valid UTF-8")?;

    let diff = similar::TextDiff::from_lines(&text_a, &text_b);

    let mut added = 0u32;
    let mut removed = 0u32;
    let mut unchanged = 0u32;

    let mut stdout_lines = Vec::new();
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
        stdout_lines.push(format!("{sign}{change}"));
    }

    let stdout = stdout_lines.join("");
    let json = serde_json::json!({
        "added": added,
        "removed": removed,
        "unchanged": unchanged,
        "diff": diff.unified_diff().header("before", "after").to_string(),
    });

    Ok(CommandOutput::Content { stdout, json })
}

fn diff_screenshot(a: &Path, b: &Path) -> Result<CommandOutput> {
    let img_a = image::load_from_memory(&read_capped(a, "image A")?)
        .context("Cannot decode image A")?;
    let img_b = image::load_from_memory(&read_capped(b, "image B")?)
        .context("Cannot decode image B")?;

    // When the two images differ in size, compare their overlapping region —
    // but report it: a 100%-of-overlap match between mismatched canvases must
    // not read as "identical" when one image is taller than the other.
    let (w, h) = (
        img_a.width().min(img_b.width()),
        img_a.height().min(img_b.height()),
    );
    let dimensions_differ =
        (img_a.width(), img_a.height()) != (img_b.width(), img_b.height());
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

            if dist > PIXEL_DIFF_THRESHOLD {
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

    // The diff is an artifact like any other capture output: timestamped under
    // artifacts/, never written into the input's directory under a fixed name
    // where two diffs would silently clobber each other.
    let diff_path = webpilot::dirs::artifacts_dir().join(format!(
        "diff_{}.png",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    ));
    diff_img
        .save(&diff_path)
        .context("Cannot save diff image")?;

    let mut human = format!(
        "Changed: {:.1}% ({diff_count}/{total} pixels)\nDiff image: {}",
        pct,
        diff_path.display()
    );
    if dimensions_differ {
        human.push_str(&format!(
            "\nNote: images differ in size ({}x{} vs {}x{}); compared the {w}x{h} overlap",
            img_a.width(),
            img_a.height(),
            img_b.width(),
            img_b.height(),
        ));
    }

    Ok(CommandOutput::Data {
        json: serde_json::json!({
            "changed_percent": format!("{:.1}", pct),
            "changed_pixels": diff_count,
            "total_pixels": total,
            "diff_image": diff_path.to_string_lossy(),
            "compared_region": { "width": w, "height": h },
            "dimensions_differ": dimensions_differ,
        }),
        human,
    })
}
