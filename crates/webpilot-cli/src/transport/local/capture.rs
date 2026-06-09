//! Capture: DOM extraction, screenshot, PDF, accessibility tree.

use anyhow::{Context, Result};
use serde_json::json;
use webpilot::WebPilotError;
use webpilot::capture::{CaptureField, CaptureOpts};
use webpilot::dirs;
use webpilot::protocol::ResponseData;
use webpilot::types::{DomSnapshot, ScrollInfo};

use super::LocalTransport;

impl LocalTransport {
    pub(super) async fn do_capture(
        &mut self,
        include: Vec<CaptureField>,
        opts: CaptureOpts,
        url: Option<String>,
    ) -> Result<ResponseData> {
        opts.validate()
            .map_err(|m| WebPilotError::InvalidArgument {
                detail: m.to_owned(),
            })?;

        if let Some(url) = url {
            self.navigate_reconnect(&url).await?;
        }

        // Annotation overlays use page-viewport coordinates, so they only line
        // up on the main frame. Refuse `--annotate` while an iframe is active
        // rather than drawing boxes at frame-relative coordinates onto a
        // viewport screenshot — browser mode skips them silently; both now
        // fail loud, identically.
        if opts.annotate {
            self.require_main_frame("capture --annotate").await?;
        }

        let want = |f: CaptureField| include.contains(&f);
        let want_dom = want(CaptureField::Dom) || opts.annotate;
        let want_text = want(CaptureField::Text);
        let want_screenshot = want(CaptureField::Screenshot) || opts.annotate;
        let want_pdf = want(CaptureField::Pdf);
        let want_ax = want(CaptureField::Accessibility);
        let bounds = opts.bounds || opts.annotate;

        let mut snapshot: Option<DomSnapshot> = None;
        let mut screenshot_path: Option<String> = None;
        let mut screenshot_error: Option<String> = None;
        let mut text_content: Option<String> = None;
        let mut text_truncated = false;
        let mut pdf_path: Option<String> = None;

        if want_dom {
            let dom = self
                .invoke_bridge(&json!({
                    "type": "extractDom",
                    "options": {"bounds": bounds, "occlusion": opts.occlusion},
                }))
                .await?;
            // A bridge-side extraction failure comes back as a typed
            // `{success:false, error}` — surface it rather than feeding it to
            // the snapshot parser as if it were a (suspiciously empty) page.
            let dom = Self::parse_bridge_response(dom)?;
            snapshot = Some(serde_json::from_value(dom).context("Failed to parse DOM snapshot")?);
        }

        if want_text {
            let r = self.invoke_bridge(&json!({"type": "extractText"})).await?;
            if let Some(text) = r.get("text").and_then(|v| v.as_str()) {
                text_content = Some(text.to_string());
                text_truncated = r
                    .get("truncated")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false);
            }
        }

        if want_screenshot {
            if opts.annotate
                && let Some(ref snap) = snapshot
            {
                let annotations: Vec<_> = snap
                    .elements
                    .iter()
                    .filter(|e| e.spatial.in_viewport == Some(true) && e.spatial.bounds.is_some())
                    .filter_map(|e| {
                        let b = e.spatial.bounds.as_ref()?;
                        Some(json!({
                            "index": e.index, "x": b.x, "y": b.y, "w": b.w, "h": b.h,
                        }))
                    })
                    .collect();
                if !annotations.is_empty() {
                    // The overlay is attached to the DOM by the bridge before
                    // its response returns, so a transport failure here can
                    // still have drawn it. Clean up before surfacing the error,
                    // honouring the invariant below: no overlay outlives the
                    // capture that drew it.
                    if let Err(e) = self
                        .invoke_bridge(&json!({
                            "type": "addAnnotations", "elements": annotations,
                        }))
                        .await
                    {
                        let _ = self
                            .invoke_bridge(&json!({"type": "removeAnnotations"}))
                            .await;
                        return Err(e);
                    }
                    tokio::time::sleep(webpilot::settings::timeouts().annotation_paint).await;
                }
            }

            let shot = if opts.full_page {
                self.page.screenshot_full_page().await
            } else {
                self.page.screenshot().await
            };
            // Always strip annotations once the shot is taken — a capture error
            // must not leave overlays in the live page for the next command.
            if opts.annotate {
                let _ = self
                    .invoke_bridge(&json!({"type": "removeAnnotations"}))
                    .await;
            }
            match shot {
                Ok(b64) => match save_screenshot(&b64) {
                    Ok(p) => screenshot_path = Some(p),
                    Err(e) => screenshot_error = Some(e.to_string()),
                },
                Err(e) => screenshot_error = Some(e.to_string()),
            }
        }

        if want_pdf {
            let r = self
                .page
                .send(
                    "Page.printToPDF",
                    Some(json!({
                        "landscape": false,
                        "printBackground": true,
                        "preferCSSPageSize": true,
                    })),
                )
                .await?;
            let data = r
                .get("data")
                .and_then(|v| v.as_str())
                .ok_or_else(|| anyhow::anyhow!("PDF generation failed: no data returned"))?;
            let bytes = base64::Engine::decode(&base64::engine::general_purpose::STANDARD, data)?;
            let path = dirs::artifact_path("capture", "pdf");
            std::fs::write(&path, &bytes)?;
            pdf_path = Some(path.to_string_lossy().into_owned());
        }

        let mut ax_tree_json: Option<String> = None;
        if want_ax {
            let r = self.page.send("Accessibility.getFullAXTree", None).await?;
            ax_tree_json = Some(serde_json::to_string_pretty(&r)?);
        }

        let (page_url, page_title) = match snapshot.as_ref() {
            Some(s) => (s.page_url.clone(), s.page_title.clone()),
            None => {
                // Stay in the active frame so the URL/title match the scope
                // that DOM extraction would have used. Without this, a
                // text-only or screenshot-only capture in an iframe context
                // would silently surface the main-page URL.
                let url = self.eval_in_active("location.href").await;
                // A scoped (non-main) frame whose metadata read fails has been
                // removed — surface FrameNotFound, not a stale-looking success
                // with empty URL/title. The DOM path already errors on a dead
                // frame; this brings screenshot/PDF/AX-only captures to parity
                // (and matches the browser capture's up-front frame check).
                if url.is_err()
                    && let Some(fid) = self.active_frame_id.lock().await.clone()
                {
                    return Err(WebPilotError::FrameNotFound {
                        selector: format!("frame {fid}"),
                    }
                    .into());
                }
                let u = url
                    .ok()
                    .and_then(|v| v.as_str().map(str::to_string))
                    .unwrap_or_default();
                let title = self
                    .eval_in_active("document.title")
                    .await
                    .ok()
                    .and_then(|v| v.as_str().map(str::to_string))
                    .unwrap_or_default();
                (u, title)
            }
        };

        // Text and accessibility tree are stored on `DomSnapshot`. When a
        // caller asks for *only* those (no DOM), we still need a snapshot
        // shell to carry the data — otherwise the response silently drops it.
        let needs_snapshot_shell =
            snapshot.is_none() && (text_content.is_some() || ax_tree_json.is_some());
        if needs_snapshot_shell {
            snapshot = Some(empty_snapshot(&page_url, &page_title));
        }
        if let Some(s) = snapshot.as_mut() {
            s.text_content = text_content;
            s.text_truncated = text_truncated;
            s.accessibility_tree = ax_tree_json;
            // Capture is scoped to the active frame; surface how many HTTP
            // iframes exist outside this scope so the agent knows `frame
            // switch` is the way in. Only meaningful from the main frame.
            if self.active_frame_id.lock().await.is_none() {
                s.subframes = self.count_http_subframes().await;
            }
        }

        Ok(ResponseData::Capture {
            dom: snapshot,
            screenshot_path,
            screenshot_error,
            pdf_path,
            // Headless writes files directly; the inline-bytes path is browser-only.
            pdf_b64: None,
            page_url,
            page_title,
        })
    }

    /// Number of HTTP(S) subframes in the page's frame tree (main excluded).
    pub(super) async fn count_http_subframes(&self) -> u32 {
        let Ok(tree) = self.page.send("Page.getFrameTree", None).await else {
            return 0;
        };
        // Bound the recursion. A frame tree is acyclic and Chrome caps real
        // nesting far below this, but the structure is browser-supplied, so a
        // depth limit well above any genuine page keeps a pathological tree from
        // overflowing the stack — it degrades to an undercount, never a crash.
        const MAX_FRAME_DEPTH: u32 = 256;
        fn walk(node: &serde_json::Value, is_root: bool, depth: u32, count: &mut u32) {
            if depth > MAX_FRAME_DEPTH {
                return;
            }
            if !is_root
                && node
                    .pointer("/frame/url")
                    .and_then(|v| v.as_str())
                    .is_some_and(|u| u.starts_with("http"))
            {
                *count += 1;
            }
            if let Some(children) = node.get("childFrames").and_then(|v| v.as_array()) {
                for child in children {
                    walk(child, false, depth + 1, count);
                }
            }
        }
        let mut count = 0;
        if let Some(root) = tree.get("frameTree") {
            walk(root, true, 0, &mut count);
        }
        count
    }
}

fn empty_snapshot(page_url: &str, page_title: &str) -> DomSnapshot {
    DomSnapshot {
        elements: Vec::new(),
        total_nodes: 0,
        page_url: page_url.to_string(),
        page_title: page_title.to_string(),
        scroll: ScrollInfo::default(),
        scroll_percent: 0,
        extraction_ms: 0,
        subframes: 0,
        shadow_truncated: false,
        text_content: None,
        text_truncated: false,
        accessibility_tree: None,
    }
}

/// Resize via the same pipeline browser-mode uses (long-edge clamp at
/// MAX_LONG_EDGE), so headless and browser screenshots are tokenwise
/// interchangeable.
fn save_screenshot(b64: &str) -> Result<String> {
    let info = webpilot::screenshot::process_and_save(b64, &dirs::artifact_path("capture", "png"))
        .map_err(|e| anyhow::anyhow!("screenshot save failed: {e}"))?;
    tracing::debug!(
        path = %info.path.display(),
        width = info.width,
        height = info.height,
        kb = info.bytes / 1024,
        tokens = info.estimated_tokens,
        "screenshot saved",
    );
    Ok(info.path.to_string_lossy().into_owned())
}
