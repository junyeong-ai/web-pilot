//! Capture: DOM extraction, screenshot, PDF, accessibility tree.

use anyhow::{Context, Result};
use serde_json::json;
use webpilot::WebPilotError;
use webpilot::capture::{CaptureField, CaptureOpts};
use webpilot::dirs;
use webpilot::protocol::ResponseData;
use webpilot::types::DomSnapshot;

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

        let downloads = match url {
            Some(url) => self.navigate_reconnect(&url).await?,
            None => Vec::new(),
        };

        // Annotation overlays use page-viewport coordinates, so they only line
        // up on the main frame. Refuse `--annotate` while an iframe is active
        // rather than drawing boxes at frame-relative coordinates onto a
        // viewport screenshot — browser mode skips them silently; both now
        // fail loud, identically.
        if opts.annotate {
            self.require_main_frame("capture --annotate").await?;
        }
        // `Page.printToPDF` is inherently a top-level operation — CDP has no
        // frame-scoped print — so a PDF taken while an iframe is active would
        // silently capture the TOP page, not the iframe the agent switched into
        // (while the DOM/header describe the iframe). Refuse it like `--annotate`
        // so the agent switches back to main rather than receiving the wrong page.
        if include.contains(&CaptureField::Pdf) {
            self.require_main_frame("capture --include pdf").await?;
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
        let mut screenshot_width: Option<u32> = None;
        let mut screenshot_height: Option<u32> = None;
        let mut screenshot_scale: Option<f64> = None;
        let mut screenshot_error: Option<String> = None;
        let mut text_content: Option<String> = None;
        let mut text_truncated = false;
        let mut pdf_path: Option<String> = None;
        // Captured in memory and committed to disk only at the very end, after
        // every fallible CDP step — so a capture that fails partway never leaves
        // an orphaned artifact file behind.
        let mut screenshot_b64: Option<String> = None;
        let mut pdf_bytes: Option<Vec<u8>> = None;

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

        // `Page.captureScreenshot` is top-level (no frame-scoped capture), so a
        // screenshot while an iframe is active would be TOP-page pixels under an
        // iframe-labelled header — the wrong image with correct-looking
        // metadata. When the screenshot is the request's ONLY output, refuse
        // loud (success with no artifact would be a lie); when it rides along a
        // frame-scoped DOM/text/AX request, it degrades through the standing
        // `screenshot_error` channel so the valid outputs still return.
        // (`--annotate` in a frame was already refused above; pdf likewise.)
        let frame_scoped = self.active_frame_id.lock().await.is_some();
        let want_screenshot = want_screenshot && !frame_scoped;
        if want(CaptureField::Screenshot) && frame_scoped {
            if !want_dom && !want_text && !want_ax {
                self.require_main_frame("capture --include screenshot")
                    .await?;
            }
            screenshot_error = Some(
                "screenshots are main-frame only and an iframe is active. Switch back first: webpilot frame main".into(),
            );
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
                        // Skip a zero-size box: a 0×0 annotation is a degenerate
                        // dot, never a useful target marker. Mirrors browser mode's
                        // `w > 0 && h > 0` keep-filter (capture.js) so the two modes
                        // annotate the identical element set (`w`/`h` are u32, so a
                        // zero dimension is `== 0`).
                        if b.w == 0 || b.h == 0 {
                            return None;
                        }
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
                // Hold the encoded image; it is written to disk only at the end,
                // after every fallible CDP step, so a later failure can't orphan it.
                Ok(b64) => screenshot_b64 = Some(b64),
                Err(e) => {
                    // A screenshot that failed because the TAB closed mid-command
                    // is tab-gone truth, not a degradable capture error: the
                    // `screenshot_error` note exists for a live page whose image
                    // pipeline failed, and burying a vanished tab there reports
                    // "success, no image" for a page that no longer exists.
                    // Typed TabNotFound (exit 4 → recover via `tab`) instead.
                    if self.target_absent(self.target_id.as_str()).await {
                        return Err(WebPilotError::TabNotFound {
                            tab_id: self.target_id.clone(),
                        }
                        .into());
                    }
                    screenshot_error = Some(e.to_string());
                }
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
            // Decode now (a hard error on malformed data), but defer the disk
            // write to the end so a later AX/metadata CDP failure can't orphan a
            // half-finished capture's PDF.
            pdf_bytes = Some(base64::Engine::decode(
                &base64::engine::general_purpose::STANDARD,
                data,
            )?);
        }

        let mut ax_tree_json: Option<String> = None;
        if want_ax {
            // Scope the AX tree to the active frame, like DOM/screenshot/metadata
            // do: with an iframe switched in, an unscoped getFullAXTree returns the
            // ROOT document's tree while the footer/URL report the iframe — the
            // agent would read accessibility for a frame it isn't looking at.
            // Bind the clone FIRST: a `match` scrutinee's temporaries live for
            // the whole match, so locking inline would hold the frame-pin
            // mutex across `bridge_context_id().await` — which locks it again.
            let active = self.active_frame_id.lock().await.clone();
            let params = match active {
                Some(fid) => {
                    // Validate the pinned frame through the same resolver every
                    // bridge call uses: a dead pin (the frame navigated away
                    // out-of-band) is a typed FrameNotFound (exit 4 → recapture
                    // or `frame main`), never the generic CDP error a raw
                    // getFullAXTree against a gone frame would surface as
                    // Other — the split browser mode's live context resolution
                    // already makes.
                    self.bridge_context_id().await?;
                    Some(json!({ "frameId": fid }))
                }
                None => None,
            };
            let r = self
                .page
                .send("Accessibility.getFullAXTree", params)
                .await?;
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
            // iframes are nested inside it (and so not shown) so the agent knows
            // `frame switch` is the way deeper. Scoped per active frame inside
            // `count_http_subframes` — correct from the main frame AND a switched
            // one.
            s.subframes = self.count_http_subframes().await;
        }

        // Every fallible CDP step is done — only now commit artifacts to disk, so
        // a capture that failed partway (a PDF/AX/metadata error after the image
        // was taken) never leaves an orphaned file, the same "nothing outlives a
        // failed capture" rule the annotation overlay follows. PDF first (a hard
        // error): writing it before the screenshot means a PDF write failure can't
        // orphan a just-saved image, while the screenshot save still degrades to
        // `screenshot_error` rather than failing the whole capture.
        if let Some(bytes) = pdf_bytes {
            let path = dirs::artifact_path("capture", "pdf");
            std::fs::write(&path, &bytes)?;
            pdf_path = Some(path.to_string_lossy().into_owned());
        }
        if let Some(b64) = screenshot_b64 {
            match save_screenshot(&b64) {
                Ok(info) => {
                    screenshot_path = Some(info.path.to_string_lossy().into_owned());
                    screenshot_width = Some(info.width);
                    screenshot_height = Some(info.height);
                    // Surface the downscale only when one happened — pixel
                    // coordinates on the saved image map to page pixels via
                    // `coord / scale`, and a silent resize breaks that math.
                    if info.scale != 1.0 {
                        screenshot_scale = Some(info.scale);
                    }
                }
                Err(e) => screenshot_error = Some(e.to_string()),
            }
        }

        Ok(ResponseData::Capture {
            dom: snapshot,
            screenshot_path,
            screenshot_width,
            screenshot_height,
            screenshot_scale,
            screenshot_error,
            pdf_path,
            // Headless writes files directly; the inline-bytes path is browser-only.
            pdf_b64: None,
            page_url,
            page_title,
            downloads,
        })
    }

    /// Number of HTTP(S) subframes in the page's frame tree (main excluded).
    /// HTTP iframes nested inside the ACTIVE frame's document but not included in
    /// its capture — the count behind the "N iframe(s) not shown" hint. Scoped to
    /// the active frame: from the main frame it counts every HTTP iframe in the
    /// page; from a switched iframe it counts that frame's own HTTP descendants,
    /// so going deeper stays discoverable (without this a nested iframe inside a
    /// switched frame was invisible, contradicting the field's contract).
    pub(super) async fn count_http_subframes(&self) -> u32 {
        let Ok(tree) = self.page.send("Page.getFrameTree", None).await else {
            return 0;
        };
        // The node whose `/frame/id` matches `fid`, anywhere in the tree. Bounded
        // by the same depth cap as the counting walk: the tree is browser-supplied,
        // so a pathological (or corrupted) depth must degrade to "not found", never
        // overflow the stack.
        fn find<'a>(
            node: &'a serde_json::Value,
            fid: &str,
            depth: u32,
        ) -> Option<&'a serde_json::Value> {
            if depth > super::MAX_FRAME_DEPTH {
                return None;
            }
            if node.pointer("/frame/id").and_then(|v| v.as_str()) == Some(fid) {
                return Some(node);
            }
            node.get("childFrames")
                .and_then(|v| v.as_array())
                .and_then(|kids| kids.iter().find_map(|k| find(k, fid, depth + 1)))
        }
        // Count this node and its descendants that are HTTP frames.
        fn count_http(node: &serde_json::Value, depth: u32, count: &mut u32) {
            if depth > super::MAX_FRAME_DEPTH {
                return;
            }
            if node
                .pointer("/frame/url")
                .and_then(|v| v.as_str())
                .is_some_and(|u| u.starts_with("http"))
            {
                *count += 1;
            }
            if let Some(children) = node.get("childFrames").and_then(|v| v.as_array()) {
                for child in children {
                    count_http(child, depth + 1, count);
                }
            }
        }

        let Some(root) = tree.get("frameTree") else {
            return 0;
        };
        // The subtree to look under: the active frame's node, or the whole tree
        // from the main frame. Count the HTTP frames strictly BELOW it — the
        // subtree root is the frame the capture already shows.
        let active = self.active_frame_id.lock().await.clone();
        let subtree = match &active {
            Some(fid) => find(root, fid, 0),
            None => Some(root),
        };
        let mut count = 0;
        if let Some(node) = subtree
            && let Some(children) = node.get("childFrames").and_then(|v| v.as_array())
        {
            // Each child is the entry of its own count, so it starts at depth 0 —
            // same as every other frame walk's entry, so the shared depth bound
            // fires at the same level across all of them (the depth only bounds the
            // stack; it never affects the count).
            for child in children {
                count_http(child, 0, &mut count);
            }
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
        scroll: None,
        scroll_percent: 0,
        extraction_ms: 0,
        subframes: 0,
        shadow_truncated: false,
        elements_truncated: false,
        text_content: None,
        text_truncated: false,
        accessibility_tree: None,
    }
}

/// Resize via the same pipeline browser-mode uses (long-edge clamp at
/// MAX_LONG_EDGE), so headless and browser screenshots are tokenwise
/// interchangeable. Returns the full result — the caller surfaces the saved
/// dimensions (and any downscale ratio) to the agent, not just the path.
fn save_screenshot(b64: &str) -> Result<webpilot::screenshot::ScreenshotResult> {
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
    Ok(info)
}
