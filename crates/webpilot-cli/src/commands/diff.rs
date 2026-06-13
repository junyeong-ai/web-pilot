use anyhow::{Context, Result};
use clap::Args;
use std::path::{Path, PathBuf};

use crate::output::CommandOutput;

/// Per-pixel Euclidean RGB distance above which a pixel counts toward the
/// NOISE-FILTERED report (`pixels_above_noise` / `percent_above_noise` and the
/// red overlay). Coarse by design — anti-aliasing and JPEG artifacts sit just
/// below typical edge deltas. The `changed` verdict itself keys on EXACT pixel
/// inequality, never this threshold.
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
    #[arg(long, conflicts_with = "screenshot")]
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
        // Default: detect by extension — from BOTH files, so a mismatched pair
        // (`a.json b.png`) is a typed error rather than silently treating PNG
        // bytes as DOM text. Case-insensitive (`.PNG` is an image).
        match (detect_kind(&args.file_a), detect_kind(&args.file_b)) {
            (Some(Kind::Dom), Some(Kind::Dom)) => diff_dom(&args.file_a, &args.file_b),
            (Some(Kind::Screenshot), Some(Kind::Screenshot)) => {
                diff_screenshot(&args.file_a, &args.file_b)
            }
            (Some(_), Some(_)) => Err(webpilot::WebPilotError::InvalidArgument {
                detail: "both files must be the same kind (two JSON snapshots or two images)"
                    .into(),
            }
            .into()),
            _ => Err(webpilot::WebPilotError::InvalidArgument {
                detail: "cannot detect file type from the extensions — use --dom or --screenshot"
                    .into(),
            }
            .into()),
        }
    }
}

enum Kind {
    Dom,
    Screenshot,
}

fn detect_kind(path: &Path) -> Option<Kind> {
    match path.extension()?.to_str()?.to_ascii_lowercase().as_str() {
        "json" => Some(Kind::Dom),
        "png" | "jpg" | "jpeg" => Some(Kind::Screenshot),
        _ => None,
    }
}

fn diff_dom(a: &Path, b: &Path) -> Result<CommandOutput> {
    let text_a =
        String::from_utf8(read_capped(a, "file A")?).context("file A is not valid UTF-8")?;
    let text_b =
        String::from_utf8(read_capped(b, "file B")?).context("file B is not valid UTF-8")?;

    // A `--dom` diff compares DOM snapshots, which are JSON. Parse both first so
    // a truncated or non-snapshot file fails loud instead of producing a
    // meaningless line diff, and re-emit canonically so two snapshots that
    // differ only in whitespace or key order don't read as changed.
    let canon = |text: &str, label: &str| -> Result<String> {
        let mut value: serde_json::Value =
            serde_json::from_str(text).map_err(|e| webpilot::WebPilotError::InvalidArgument {
                detail: format!("{label} is not a valid DOM snapshot (JSON): {e}"),
            })?;
        // `extraction_ms` is wall-clock extraction latency — run-to-run
        // measurement noise with no bearing on "did the DOM change?". Strip it so
        // two captures of the SAME page don't read as changed on timing jitter
        // alone; every semantically-meaningful field (scroll_x/scroll_y, which are
        // real page state, included) is kept.
        if let Some(obj) = value.as_object_mut() {
            obj.remove("extraction_ms");
        }
        Ok(serde_json::to_string_pretty(&value).expect("re-serialize parsed json"))
    };
    let text_a = canon(&text_a, "file A")?;
    let text_b = canon(&text_b, "file B")?;

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
        // An explicit verdict so a caller checks one field instead of inferring
        // change from the counts. The exit code stays 0 (the command succeeded);
        // a WebPilot exit code names an error class, never a domain result.
        "changed": added > 0 || removed > 0,
        "added": added,
        "removed": removed,
        "unchanged": unchanged,
        "diff": diff.unified_diff().header("before", "after").to_string(),
    });

    Ok(CommandOutput::Content {
        stdout,
        json,
        note: None,
    })
}

fn diff_screenshot(a: &Path, b: &Path) -> Result<CommandOutput> {
    let img_a =
        image::load_from_memory(&read_capped(a, "image A")?).context("Cannot decode image A")?;
    let img_b =
        image::load_from_memory(&read_capped(b, "image B")?).context("Cannot decode image B")?;

    // When the two images differ in size, compare their overlapping region —
    // but report it: a 100%-of-overlap match between mismatched canvases must
    // not read as "identical" when one image is taller than the other.
    let (w, h) = (
        img_a.width().min(img_b.width()),
        img_a.height().min(img_b.height()),
    );
    let dimensions_differ = (img_a.width(), img_a.height()) != (img_b.width(), img_b.height());
    let rgba_a = img_a.to_rgba8();
    let rgba_b = img_b.to_rgba8();

    let mut diff_count = 0u64;
    let mut exact_count = 0u64;
    let total = (w as u64) * (h as u64);
    let mut diff_img = image::RgbaImage::new(w, h);

    for y in 0..h {
        for x in 0..w {
            let pa = rgba_a.get_pixel(x, y);
            let pb = rgba_b.get_pixel(x, y);
            // EXACT inequality drives the `changed` verdict: a page whose every
            // pixel shifted subtly (all below the noise threshold) HAS changed,
            // and reporting "0/total" there would be an identity claim, not
            // coarse reporting. The threshold stays what the overlay and the
            // noise-filtered count key on.
            if pa != pb {
                exact_count += 1;
            }
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
    // where two diffs would silently clobber each other. One naming authority
    // (`dirs::artifact_path`), so the name carries the pid and is unique even
    // across two concurrent diffs in different processes.
    let diff_path = webpilot::dirs::artifact_path("diff", "png");
    diff_img
        .save(&diff_path)
        .context("Cannot save diff image")?;

    let mut human = format!(
        "Changed: {} — {exact_count} px differ exactly; {:.1}% above the noise threshold ({diff_count}/{total})\nDiff image: {}",
        if exact_count > 0 || dimensions_differ {
            "yes"
        } else {
            "no"
        },
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
            // Explicit verdict from EXACT pixel inequality (or a size change —
            // two images of different dimensions are not the same image even if
            // their overlap matches): every mitigating field used to derive
            // from the same noise threshold, so a page whose pixels all shifted
            // subtly read "changed: false, 0/total" — an identity claim. The
            // threshold remains a reporting aid: `pixels_above_noise` and the
            // red overlay key on it. Exit code stays 0 (success) — it names an
            // error class, not a domain result.
            "changed": exact_count > 0 || dimensions_differ,
            "changed_pixels": exact_count,
            "pixels_above_noise": diff_count,
            "percent_above_noise": format!("{:.1}", pct),
            "total_pixels": total,
            "diff_image": diff_path.to_string_lossy(),
            "compared_region": { "width": w, "height": h },
            "dimensions_differ": dimensions_differ,
        }),
        human,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn diff_dom_validates_json_inputs() {
        let dir = std::env::temp_dir().join(format!("wp-diff-dom-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let good = dir.join("good.json");
        let bad = dir.join("bad.json");
        std::fs::write(&good, br#"{"elements":[],"total_nodes":1}"#).unwrap();
        std::fs::write(&bad, b"truncated{{{ not json").unwrap();

        // Two valid snapshots diff without error.
        assert!(diff_dom(&good, &good).is_ok());

        // A malformed snapshot is rejected loudly, not line-diffed into garbage.
        match diff_dom(&good, &bad) {
            Ok(_) => panic!("malformed JSON must be rejected, not diffed"),
            Err(e) => assert!(
                e.to_string().contains("not a valid DOM snapshot"),
                "expected a typed rejection of malformed JSON, got: {e}"
            ),
        }

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn diff_dom_ignores_extraction_ms_noise() {
        let dir = std::env::temp_dir().join(format!("wp-diff-ms-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let a = dir.join("a.json");
        let b = dir.join("b.json");
        // Two captures of the SAME page differ only in extraction_ms (wall-clock
        // latency, pure measurement noise). The diff must read unchanged, not a
        // false positive on timing jitter.
        std::fs::write(
            &a,
            br#"{"elements":[{"index":1,"tag":"button"}],"extraction_ms":18,"total_nodes":5}"#,
        )
        .unwrap();
        std::fs::write(
            &b,
            br#"{"elements":[{"index":1,"tag":"button"}],"extraction_ms":0,"total_nodes":5}"#,
        )
        .unwrap();
        let out = diff_dom(&a, &b).expect("diff ok");
        let _ = std::fs::remove_dir_all(&dir);
        match out {
            CommandOutput::Content { json, .. } => assert_eq!(
                json["changed"], false,
                "two captures differing only in extraction_ms must read as unchanged: {json}"
            ),
            _ => panic!("expected Content output"),
        }
    }
}
