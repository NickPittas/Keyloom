//! Workspace persistence glue: restores the saved page/clip/view/frame/graph of a project once when it
//! becomes active (validated against the clips that exist now), then saves whenever it changes.
use super::*;
use crate::workspace::{GraphState, Workspace};
use std::time::Duration;

fn node_name(n: Node) -> String {
    match n {
        Node::Media => "media".into(),
        Node::Hint => "hint".into(),
        Node::Key => "key".into(),
        Node::Out(v) => v.label().to_lowercase(),
    }
}
fn node_parse(s: &str) -> Option<Node> {
    Some(match s {
        "media" => Node::Media,
        "hint" => Node::Hint,
        "key" => Node::Key,
        other => Node::Out(View::ALL.into_iter().find(|v| {
            v.label().eq_ignore_ascii_case(other) && !matches!(v, View::Input | View::Hint)
        })?),
    })
}
fn view_parse(s: &str) -> Option<View> {
    View::ALL
        .into_iter()
        .find(|v| v.label().eq_ignore_ascii_case(s))
}

#[derive(Default)]
pub struct WsState {
    root: Option<PathBuf>,
    last: Option<Workspace>,
    saved_at: Option<Instant>,
    failed: bool,
}

impl App {
    pub(super) fn workspace_current(&self) -> Option<Workspace> {
        let p = self.project.as_ref()?;
        let mut w = Workspace {
            tab: self.tab.label().to_lowercase(),
            node: node_name(self.node),
            graph_collapsed: self.graph_collapsed,
            selected: self
                .selected
                .iter()
                .filter_map(|i| p.clips.get(*i))
                .map(|c| c.folder.clone())
                .collect(),
            ..Default::default()
        };
        if let (Some(i), Some(v)) = (self.open, &self.viewer) {
            w.open = p.clips.get(i).map(|c| c.folder.clone());
            w.view = v.view.label().to_lowercase();
            w.ab = v.ab.then(|| v.b_view.label().to_lowercase());
            w.frame = v.index.stems.get(v.playhead).cloned();
        }
        for (k, (g, gv)) in &self.graphs {
            if p.clips.iter().any(|c| &c.folder == k) {
                w.graphs.insert(
                    k.clone(),
                    GraphState {
                        pan: [gv.pan.x, gv.pan.y],
                        zoom: gv.zoom,
                        nodes: g.nodes.iter().map(|n| [n.pos.x, n.pos.y]).collect(),
                    },
                );
            }
        }
        // Restored graphs of clips never opened this session are carried over, not forgotten.
        if let Some(old) = &self.ws.last {
            for (k, g) in &old.graphs {
                w.graphs.entry(k.clone()).or_insert_with(|| g.clone());
            }
        }
        Some(w)
    }

    /// Called every frame. Cheap: a comparison, and at most one atomic write per second.
    pub(super) fn workspace_sync(&mut self) {
        let Some(root) = self.project.as_ref().map(|p| p.root.clone()) else {
            self.ws = WsState::default();
            return;
        };
        if self.ws.root.as_ref() != Some(&root) {
            self.ws = WsState {
                root: Some(root.clone()),
                ..Default::default()
            };
            self.workspace_restore(&root);
            self.ws.last = self.workspace_current();
            return;
        }
        let Some(now) = self.workspace_current() else {
            return;
        };
        if self.ws.last.as_ref() == Some(&now) {
            return;
        }
        if self
            .ws
            .saved_at
            .is_some_and(|t| t.elapsed() < Duration::from_secs(1))
        {
            self.last_ctx
                .as_ref()
                .inspect(|c| c.request_repaint_after(Duration::from_secs(1)));
            return; // playback moves the playhead every frame: write at most once a second
        }
        self.ws.saved_at = Some(Instant::now());
        match now.save(&root) {
            Ok(()) => {
                self.ws.failed = false;
                self.ws.last = Some(now);
            }
            Err(e) => {
                if !self.ws.failed {
                    self.ai.notice = Some(format!("Workspace not saved: {e}"));
                }
                self.ws.failed = true;
                self.ws.last = Some(now); // do not retry every second; the next real change retries
            }
        }
    }

    fn workspace_restore(&mut self, root: &std::path::Path) {
        let Some(p) = &self.project else { return };
        let clips: Vec<String> = p.clips.iter().map(|c| c.folder.clone()).collect();
        let Some(w) = Workspace::load(root).map(|w| w.sanitized(&clips)) else {
            return;
        };
        self.ws.last = Some(w.clone()); // carries stored graphs of clips not opened yet
        self.graph_collapsed = w.graph_collapsed;
        self.selected = w
            .selected
            .iter()
            .filter_map(|f| clips.iter().position(|c| c == f))
            .collect();
        for (k, g) in &w.graphs {
            let mut graph = graph::Graph::corridorkey();
            if g.nodes.len() == graph.nodes.len() {
                for (n, p) in graph.nodes.iter_mut().zip(&g.nodes) {
                    n.pos = egui::pos2(p[0], p[1]);
                }
            }
            self.graphs.insert(
                k.clone(),
                (
                    graph,
                    graph::View::restored(Vec2::new(g.pan[0], g.pan[1]), g.zoom),
                ),
            );
        }
        // Explicit command-line choices beat the saved workspace.
        if self.opts.open_clip.is_some() || self.opts.tab.is_some() {
            return;
        }
        let (Some(ctx), Some(open)) = (self.last_ctx.clone(), w.open.as_ref()) else {
            return;
        };
        let Some(i) = clips.iter().position(|c| c == open) else {
            return;
        };
        self.open_clip(&ctx, i);
        if let Some(t) = Tab::parse(&w.tab) {
            self.switch_tab(t);
        }
        if let Some(n) = node_parse(&w.node) {
            if n.tab() == self.tab || (self.tab == Tab::Review && matches!(n, Node::Out(_))) {
                self.node = n;
            }
        }
        if let Some(v) = self.viewer.as_mut() {
            if let Some(vw) = view_parse(&w.view) {
                v.view = vw;
            }
            if let Some(b) = w.ab.as_deref().and_then(view_parse) {
                v.ab = true;
                v.b_view = b;
            }
            if let Some(i) = w.frame.as_deref().and_then(|s| v.index.index_of(s)) {
                v.set_playhead(i);
            }
        }
    }
}
