use crate::commands;
use crate::output::OutputMode;
use anyhow::Result;

use super::{HeadlessContext, call_bridge, navigate_reconnect};

pub(crate) async fn run(
    ctx: &mut HeadlessContext,
    args: commands::capture::CaptureArgs,
    output_mode: OutputMode,
) -> Result<()> {
    // Validate flag conflicts
    if args.annotate && args.full_page {
        anyhow::bail!(
            "--annotate and --full-page cannot be combined. Annotations are viewport-only."
        );
    }

    // Navigate if URL provided (handles cross-origin renderer process swap)
    if let Some(ref url) = args.url {
        let new_cdp = navigate_reconnect(&ctx.browser, &ctx.ws_url, &ctx.page, url).await?;
        ctx.page = new_cdp;
    }
    let cdp = &ctx.page;

    let mut out = serde_json::Map::new();

    // DOM extraction
    let dom = args.dom || args.annotate || (!args.screenshot && !args.text && !args.accessibility);
    let bounds = args.bounds || args.annotate;

    if dom {
        let opts = serde_json::json!({"bounds": bounds, "occlusion": args.occlusion});
        let result = call_bridge(
            cdp,
            &serde_json::json!({"type": "extractDOM", "options": opts}).to_string(),
        )
        .await?;
        if let Some(obj) = result.as_object() {
            for (k, v) in obj {
                out.insert(k.clone(), v.clone());
            }
        }
    }

    // Text
    if args.text {
        let result =
            call_bridge(cdp, &serde_json::json!({"type": "extractText"}).to_string()).await?;
        if let Some(text) = result.get("text").and_then(|v| v.as_str()) {
            out.insert("text_content".into(), serde_json::json!(text));
        }
    }

    // Screenshot
    let screenshot = args.screenshot || args.annotate;
    if screenshot {
        // Add annotations if requested
        if args.annotate
            && let Some(elements) = out.get("elements").and_then(|v| v.as_array())
        {
            let annotations: Vec<_> = elements
                .iter()
                .filter(|e| {
                    e.get("in_viewport")
                        .and_then(|v| v.as_bool())
                        .unwrap_or(false)
                        && e.get("bounds").is_some()
                })
                .filter_map(|e| {
                    let b = e.get("bounds")?;
                    Some(serde_json::json!({
                        "index": e.get("index")?,
                        "x": b.get("x")?, "y": b.get("y")?,
                        "w": b.get("w")?, "h": b.get("h")?,
                    }))
                })
                .collect();
            if !annotations.is_empty() {
                call_bridge(
                    cdp,
                    &serde_json::json!({"type": "addAnnotations", "elements": annotations})
                        .to_string(),
                )
                .await?;
                tokio::time::sleep(crate::timeouts::post_navigate()).await;
            }
        }

        let b64 = cdp.screenshot().await?;

        // Remove annotations
        if args.annotate {
            let _ = call_bridge(
                cdp,
                &serde_json::json!({"type": "removeAnnotations"}).to_string(),
            )
            .await;
        }

        // Save screenshot
        let output_dir = std::path::Path::new(webpilot::OUTPUT_DIR);
        let _ = std::fs::create_dir_all(output_dir);
        let bytes = base64::Engine::decode(&base64::engine::general_purpose::STANDARD, &b64)?;
        let ts = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis();
        let path = output_dir.join(format!("capture_{ts}.png"));
        std::fs::write(&path, &bytes)?;
        out.insert(
            "screenshot_path".into(),
            serde_json::json!(path.to_string_lossy()),
        );
    }

    // PDF generation
    if args.pdf {
        let pdf_result = cdp
            .send(
                "Page.printToPDF",
                Some(serde_json::json!({
                    "landscape": false,
                    "printBackground": true,
                    "preferCSSPageSize": true,
                })),
            )
            .await?;
        let data = pdf_result
            .get("data")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!("PDF generation failed: no data returned"))?;
        let bytes = base64::Engine::decode(&base64::engine::general_purpose::STANDARD, data)?;
        let output_dir = std::path::Path::new(webpilot::OUTPUT_DIR);
        std::fs::create_dir_all(output_dir)?;
        let ts = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis();
        let pdf_path = output_dir.join(format!("capture_{ts}.pdf"));
        std::fs::write(&pdf_path, &bytes)?;
        out.insert(
            "pdf_path".into(),
            serde_json::json!(pdf_path.to_string_lossy()),
        );
    }

    // Output
    match output_mode {
        OutputMode::Human => {
            if let Some(elements) = out.get("elements").and_then(|v| v.as_array())
                && !elements.is_empty()
            {
                let snapshot: webpilot::types::DomSnapshot =
                    serde_json::from_value(serde_json::Value::Object(out.clone()))?;
                print!("{}", webpilot::types::serialize_dom(&snapshot));
            }
            if let Some(path) = out.get("screenshot_path").and_then(|v| v.as_str()) {
                eprintln!("Screenshot: {path}");
            }
            if let Some(path) = out.get("pdf_path").and_then(|v| v.as_str()) {
                eprintln!("PDF: {path}");
            }
        }
        OutputMode::Json => {
            println!("{}", serde_json::Value::Object(out));
        }
    }
    Ok(())
}
