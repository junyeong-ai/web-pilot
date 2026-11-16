//! The bound page's frames across renderer processes.
//!
//! Site isolation renders a cross-site iframe in a process of its own, and the
//! page's session sees such a frame only from outside: `Page.getFrameTree`
//! leaves it out, and its documents' execution contexts never reach the page's
//! `Runtime`. Chrome lists each one as a target of type `iframe` whose id is its
//! frame id, naming its parent target, and a session attached to that target
//! reaches its frames the way the page's session reaches the page's. So every
//! frame has a [`Host`] — the page's session, or the session of the remote frame
//! whose process renders it — and a command that acts in a frame sends to its
//! host: its contexts, the objects they hand out and their coordinates all
//! belong to that session.
//!
//! Remote sessions are attached when a command needs them — the frame forest
//! (a listing, a switch, the subframe count) or a scoped command whose frame no
//! known context names — and live as long as the page binding. A dialog a
//! remote frame raises still reaches the page's session, whose responder
//! answers it.

use std::collections::HashMap;
use std::sync::Arc;

use anyhow::Result;
use serde_json::{Value, json};
use webpilot::WebPilotError;
use webpilot::types::FrameInfo;

use super::{LocalTransport, MAX_FRAME_DEPTH};
use crate::cdp::CdpSession;

/// The session a frame's documents are reached through.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Host {
    /// The bound page's own session.
    Page,
    /// The session attached to the remote frame with this id, which is also its
    /// target id.
    Remote(String),
}

/// A frame's execution context and the host that reaches it.
#[derive(Debug, Clone)]
pub(crate) struct FrameContext {
    pub(crate) host: Host,
    pub(crate) unique_id: String,
}

/// A cross-site iframe rendered in a process of its own.
pub(crate) struct RemoteFrame {
    session: CdpSession,
    /// The host of the document that holds this frame's `<iframe>`.
    parent: Host,
}

impl RemoteFrame {
    pub(super) async fn reemit_execution_contexts(&self) {
        super::reemit_execution_contexts(&self.session).await;
    }
}

/// One frame of the forest, with the host of the document that embeds it.
pub(super) struct HostedFrame {
    pub(super) info: FrameInfo,
    pub(super) owner_host: Host,
}

/// The `<iframe>` of a remote frame, seen from the document that holds it.
pub(super) struct Boundary {
    /// The host of that document.
    pub(super) host: Host,
    /// The `<iframe>` element's `backendNodeId` there.
    pub(super) owner: Value,
    /// The origin of the `<iframe>`'s content box in that host's viewport,
    /// which is where the remote frame's own viewport starts.
    pub(super) origin: (f64, f64),
}

/// A host's session: borrowed for the page, shared for a remote frame.
pub(super) enum HostSession<'a> {
    Page(&'a CdpSession),
    Remote(Arc<RemoteFrame>),
}

impl std::ops::Deref for HostSession<'_> {
    type Target = CdpSession;

    fn deref(&self) -> &CdpSession {
        match self {
            Self::Page(session) => session,
            Self::Remote(frame) => &frame.session,
        }
    }
}

impl LocalTransport {
    /// The session that reaches `host`, or `None` once the binding no longer
    /// holds that remote frame — its iframe went away, or now renders in its
    /// parent's process.
    pub(super) async fn host_session(&self, host: &Host) -> Option<HostSession<'_>> {
        match host {
            Host::Page => Some(HostSession::Page(&self.page)),
            Host::Remote(id) => self
                .remote_frames
                .lock()
                .await
                .get(id)
                .filter(|frame| frame.session.is_alive())
                .cloned()
                .map(HostSession::Remote),
        }
    }

    /// As [`Self::host_session`], for a command that cannot act without it.
    pub(super) async fn require_host_session(&self, host: &Host) -> Result<HostSession<'_>> {
        self.host_session(host).await.ok_or_else(|| {
            let Host::Remote(id) = host else {
                unreachable!("the page's session is always held")
            };
            WebPilotError::FrameNotFound {
                selector: format!("frame {id}"),
            }
            .into()
        })
    }

    /// Every frame of the bound page, each listed after its parent.
    pub(super) async fn frame_forest(&self) -> Result<Vec<HostedFrame>> {
        let tree = self.page.send("Page.getFrameTree", None).await?;
        let mut frames = Vec::new();
        if let Some(root) = tree.get("frameTree") {
            collect_hosted(root, &Host::Page, &Host::Page, 0, &mut frames);
        }
        for (id, remote) in self.sync_remote_frames().await? {
            // A frame that went away since the target list was read lists nothing.
            let Ok(tree) = remote.session.send("Page.getFrameTree", None).await else {
                continue;
            };
            if let Some(root) = tree.get("frameTree") {
                collect_hosted(root, &Host::Remote(id), &remote.parent, 0, &mut frames);
            }
        }
        Ok(in_tree_order(frames))
    }

    /// Attach each remote frame below the bound page that the binding does not
    /// hold yet, and drop the ones that went away; returns those held. Chrome's
    /// target list decides: an `iframe` target belongs to this page when its
    /// chain of parent targets reaches the page.
    pub(super) async fn sync_remote_frames(&self) -> Result<Vec<(String, Arc<RemoteFrame>)>> {
        let targets = self.browser.get_targets().await?;
        let parents: HashMap<&str, &str> = targets
            .iter()
            .filter(|t| t.get("type").and_then(Value::as_str) == Some("iframe"))
            .filter_map(|t| Some((t.get("targetId")?.as_str()?, t.get("parentId")?.as_str()?)))
            .collect();
        let below_page = |id: &str| {
            let mut current = id;
            for _ in 0..=MAX_FRAME_DEPTH {
                match parents.get(current) {
                    Some(&parent) if parent == self.target_id => return true,
                    Some(&parent) => current = parent,
                    None => return false,
                }
            }
            false
        };

        let mut remotes = self.remote_frames.lock().await;
        let gone: Vec<String> = remotes
            .iter()
            .filter(|(id, frame)| !frame.session.is_alive() || !below_page(id))
            .map(|(id, _)| id.clone())
            .collect();
        for id in &gone {
            remotes.remove(id);
            self.forget_host_contexts(&Host::Remote(id.clone())).await;
        }
        for (&id, &parent) in &parents {
            if remotes.contains_key(id) || !below_page(id) {
                continue;
            }
            let parent = if parent == self.target_id {
                Host::Page
            } else {
                Host::Remote(parent.to_owned())
            };
            // A frame that went away between the list and the attach has nothing
            // to attach to.
            if let Ok(frame) = self.attach_remote_frame(id, parent).await {
                remotes.insert(id.to_owned(), Arc::new(frame));
            }
        }
        Ok(remotes
            .iter()
            .map(|(id, frame)| (id.clone(), frame.clone()))
            .collect())
    }

    /// Attach to remote frame `id` and prime it as the page is primed: its
    /// contexts feed the shared maps under its own host, and its documents get
    /// the bridge world and the dialog handler.
    async fn attach_remote_frame(&self, id: &str, parent: Host) -> Result<RemoteFrame> {
        let mut session = self.browser.attach(id).await?;
        let listener = super::spawn_frame_context_listener(
            &session,
            Host::Remote(id.to_owned()),
            self.frame_contexts.clone(),
            self.bridge_contexts.clone(),
        );
        session.track(listener);
        super::install_bridge_world(&session).await?;
        super::state::install_dialog_handler(&session).await?;
        super::reemit_execution_contexts(&session).await;
        Ok(RemoteFrame { session, parent })
    }

    /// Drop every known context `host` reached.
    pub(super) async fn forget_host_contexts(&self, host: &Host) {
        self.frame_contexts
            .lock()
            .await
            .retain(|_, context| &context.host != host);
        self.bridge_contexts
            .lock()
            .await
            .retain(|_, context| &context.host != host);
    }

    /// The `<iframe>` boundaries between `host` and the page, innermost first.
    pub(super) async fn frame_boundaries(&self, host: &Host) -> Result<Vec<Boundary>> {
        let mut boundaries = Vec::new();
        let mut current = host.clone();
        while let Host::Remote(id) = current {
            let parent = {
                let remotes = self.remote_frames.lock().await;
                let frame = remotes
                    .get(&id)
                    .ok_or_else(|| WebPilotError::FrameNotFound {
                        selector: format!("frame {id}"),
                    })?;
                frame.parent.clone()
            };
            let session = self.require_host_session(&parent).await?;
            let owner = session
                .send("DOM.getFrameOwner", Some(json!({ "frameId": id })))
                .await?
                .get("backendNodeId")
                .cloned()
                .ok_or_else(|| WebPilotError::FrameNotFound {
                    selector: format!("frame {id}"),
                })?;
            let model = session
                .send("DOM.getBoxModel", Some(json!({ "backendNodeId": owner })))
                .await?;
            let content: Vec<f64> = model
                .pointer("/model/content")
                .cloned()
                .and_then(|quad| serde_json::from_value(quad).ok())
                .filter(|quad: &Vec<f64>| quad.len() == 8)
                .ok_or_else(|| WebPilotError::InvalidArgument {
                    detail: "the frame's <iframe> has no layout box".into(),
                })?;
            boundaries.push(Boundary {
                host: parent.clone(),
                owner,
                origin: (content[0], content[1]),
            });
            current = parent;
        }
        Ok(boundaries)
    }
}

/// The element at `(x, y)` of `session`'s viewport, as `DOM.getNodeForLocation`
/// reports it (`backendNodeId`, `frameId`).
pub(super) async fn node_at(session: &CdpSession, x: f64, y: f64) -> Result<Value> {
    let metrics = session.send("Page.getLayoutMetrics", None).await?;
    let scroll = |key: &str| {
        metrics["cssLayoutViewport"][key]
            .as_f64()
            .ok_or_else(|| anyhow::anyhow!("Page.getLayoutMetrics omitted cssLayoutViewport.{key}"))
    };
    let (page_x, page_y) = (scroll("pageX")?, scroll("pageY")?);
    session
        .send(
            "DOM.getNodeForLocation",
            Some(json!({
                "x": (x + page_x).round() as i64,
                "y": (y + page_y).round() as i64,
                "includeUserAgentShadowDOM": true,
            })),
        )
        .await
}

fn collect_hosted(
    node: &Value,
    host: &Host,
    root_owner_host: &Host,
    depth: u32,
    out: &mut Vec<HostedFrame>,
) {
    if depth > MAX_FRAME_DEPTH {
        return;
    }
    if let Some(frame) = node.get("frame") {
        let text = |key: &str| frame.get(key).and_then(Value::as_str).map(str::to_owned);
        out.push(HostedFrame {
            info: FrameInfo {
                frame_id: text("id").unwrap_or_default(),
                url: text("url").unwrap_or_default(),
                name: text("name"),
                parent_frame_id: text("parentId"),
                is_main: frame.get("parentId").is_none(),
            },
            // A tree's root is embedded by the document its host's parent
            // renders; every frame below it, by a document of the same host.
            owner_host: if depth == 0 {
                root_owner_host.clone()
            } else {
                host.clone()
            },
        });
    }
    for child in node
        .get("childFrames")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        collect_hosted(child, host, root_owner_host, depth + 1, out);
    }
}

/// Order `frames` depth-first from the main frame, each after its parent. A
/// frame whose parent the forest does not hold — it went away between reads —
/// is listed after the rest rather than dropped.
fn in_tree_order(frames: Vec<HostedFrame>) -> Vec<HostedFrame> {
    let mut children: HashMap<Option<String>, Vec<HostedFrame>> = HashMap::new();
    for frame in frames {
        children
            .entry(frame.info.parent_frame_id.clone())
            .or_default()
            .push(frame);
    }
    let mut ordered = Vec::new();
    let mut stack: Vec<HostedFrame> = children
        .remove(&None)
        .unwrap_or_default()
        .into_iter()
        .rev()
        .collect();
    while let Some(frame) = stack.pop() {
        if let Some(kids) = children.remove(&Some(frame.info.frame_id.clone())) {
            stack.extend(kids.into_iter().rev());
        }
        ordered.push(frame);
    }
    ordered.extend(children.into_values().flatten());
    ordered
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame(id: &str, parent: Option<&str>) -> HostedFrame {
        HostedFrame {
            info: FrameInfo {
                frame_id: id.into(),
                url: String::new(),
                name: None,
                parent_frame_id: parent.map(str::to_owned),
                is_main: parent.is_none(),
            },
            owner_host: Host::Page,
        }
    }

    #[test]
    fn a_remote_subtree_is_listed_under_its_parent() {
        // The page's tree first, then a remote frame's: the order a forest is
        // read in.
        let read = vec![
            frame("main", None),
            frame("a", Some("main")),
            frame("b", Some("main")),
            frame("remote", Some("a")),
            frame("inner", Some("remote")),
        ];
        let ids: Vec<String> = in_tree_order(read)
            .into_iter()
            .map(|f| f.info.frame_id)
            .collect();
        assert_eq!(ids, ["main", "a", "remote", "inner", "b"]);
    }

    #[test]
    fn a_frame_whose_parent_went_away_is_kept() {
        let ids: Vec<String> = in_tree_order(vec![frame("main", None), frame("x", Some("gone"))])
            .into_iter()
            .map(|f| f.info.frame_id)
            .collect();
        assert_eq!(ids, ["main", "x"]);
    }
}
