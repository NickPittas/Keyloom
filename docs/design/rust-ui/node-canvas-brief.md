# Rust UI: real node canvas + readable controls (implementation brief)

Status: contract, 2026-10-22. **Supersedes** the fixed graph *strip* (`selected-design.md` §1 row "Graph UNDER EACH PAGE",
the "Fixed topology" paragraph, and §2.3 "Graph rules"). User's words: "node graph is not actually a nodegraph just a
a->b->c->d … actual nodes and connections" and "checkboxes/buttons exact same color as background, cannot distinguish".

## 1. Scope

**Now:** the existing 7 nodes only (Media, Hint, Key, FG, Matte, Comp, Processed) drawn as real cards with typed
sockets and bezier connections showing the real data flow; draggable nodes; pan, zoom, fit; resizable/collapsible lower
panel; readable buttons/checkboxes. Node selection → Properties, double-click/Enter → tab, ←/→ navigation, Bypass Hint:
unchanged behaviour.

**Not now (data model must allow, UI must not fake):** luminance key → hint keyframes, add/delete nodes, merge alpha,
roto, paint, any graph execution engine, user rewiring. Sockets are not drag sources; dragging a socket does nothing.
Processing still comes from the existing clip settings, not from the graph.

## 2. Topology (real edges, not a chain)

| Node | Inputs (socket: type) | Outputs (socket: type) |
|---|---|---|
| Media | — | `rgb: Image` |
| Hint | `rgb: Image` | `hint: Matte` |
| Key | `rgb: Image`, `hint: Matte` (optional) | `fg: Image`, `matte: Matte`, `comp: Image`, `processed: Image` |
| FG / Matte / Comp / Processed | `in` (type of its source) | — (write to disk) |

Edges: `Media.rgb→Hint.rgb`, `Media.rgb→Key.rgb`, `Hint.hint→Key.hint`, `Key.fg→FG.in`, `Key.matte→Matte.in`,
`Key.comp→Comp.in`, `Key.processed→Processed.in`. Media fans out to two nodes, Key merges two inputs and fans out to four.

**Bypass Hint:** the `Hint.hint→Key.hint` edge and the Hint card are drawn dashed at reduced alpha. No edge is
added or removed; `Media→Key` already exists. Export node with output **Off**: card and its edge dimmed, badge "Off".

Edge/socket colour by type: Image = light gray `#C8C8C8`, Matte = cyan `#5EC8D8`. Types are also written as socket labels.

## 3. Canvas behaviour

- Default layout (graph units, top-left of card): Media (0,90), Hint (200,0), Key (400,90), FG (640,0), Matte (640,70),
  Comp (640,140), Processed (640,210). Card width 150. Header 22 px with title; body shows the status badge
  (`selected-design.md` §2.3 badges kept). Key card is tall enough for 2 inputs and 4 outputs, 18 px socket pitch.
- Drag a card's header → move node. Drag empty canvas or middle-drag → pan. Ctrl+wheel (and pinch) → zoom about the
  cursor, clamped 0.5–2.0. Plain wheel pans vertically (do not steal viewer scroll outside the canvas).
- **Fit** button in the panel header and double-click on empty canvas → fit all cards with 24 px margin. Fit runs
  automatically on first show and when the panel is resized while untouched by the user.
- Positions, pan and zoom are session state (in memory). No project writes for layout now.
- Selected card: 2 px accent border. Keyboard focus: amber ring (§5). Hover on a socket: tooltip `name: Type`.
- Lower panel: resizable `Panel::bottom`, default 220 px, min 120 px, max = whatever keeps the Hint viewer drawing
  area ≥ 300 px at 1440×900. Header row (28 px) always visible: **Node graph**, selected node name, **Fit**, collapse
  toggle (▾/▸). Collapsed = header only. Height/collapsed state shared by all five tabs. `G` still focuses the graph
  (expands it if collapsed).

## 4. Module API (second implementor owns only new files)

Owner split: graph/theme implementor writes `src/ui/graph.rs`, `src/ui/theme.rs` and their tests, and adds two lines to
`src/ui/mod.rs` (`pub mod graph; pub mod theme;`). **Does not touch `app.rs`.** The import implementor integrates
afterwards: replaces `setup_style` body with `theme::apply(ctx)`, replaces `fn graph` with `graph::show`, and maps
`graph::NodeKind` to its existing `Node`. Neither module depends on `app.rs`, project, or decode code; only `egui`.

```rust
// graph.rs
pub enum PortType { Image, Matte }
pub enum OutKind { Fg, Matte, Comp, Processed }
pub enum NodeKind { Media, Hint, Key, Out(OutKind) }
pub struct NodeId(pub usize);                    // index into Graph::nodes
pub struct Port { pub name: &'static str, pub ty: PortType, pub optional: bool }
pub struct GNode { pub kind: NodeKind, pub pos: egui::Pos2, pub inputs: Vec<Port>, pub outputs: Vec<Port> }
pub struct Edge { pub from: (NodeId, usize), pub to: (NodeId, usize) }   // (node, output idx) → (node, input idx)
pub struct Graph { pub nodes: Vec<GNode>, pub edges: Vec<Edge> }
impl Graph {
    pub fn corridorkey() -> Self;                // §2 topology + §3 layout
    pub fn validate(&self) -> Result<(), String>; // types match, ≤1 edge per input, acyclic, indices valid
    pub fn find(&self, k: NodeKind) -> Option<NodeId>;
}
pub struct View { pub pan: egui::Vec2, pub zoom: f32, pub user_moved: bool }   // Default = fit pending
impl View { pub fn fit(&mut self, g: &Graph, rect: egui::Rect); pub fn to_screen(&self, p: egui::Pos2, rect: egui::Rect) -> egui::Pos2; pub fn to_graph(..) -> egui::Pos2; }
pub struct NodeLook { pub badge: String, pub dimmed: bool, pub dashed: bool }   // supplied by app per frame
pub enum Event { Select(NodeKind), Open(NodeKind) }
pub fn show(ui: &mut egui::Ui, g: &mut Graph, v: &mut View, selected: Option<NodeKind>,
            look: impl Fn(NodeKind) -> NodeLook) -> Option<Event>;
```

```rust
// theme.rs
pub const PANEL, CANVAS, FIELD, BUTTON, BUTTON_HOVER, BORDER, BORDER_HOVER, ACCENT, CHECKED, FOCUS, TEXT, TEXT_DISABLED: egui::Color32;
pub fn apply(ctx: &egui::Context);               // sets egui::Visuals from the tokens below
pub fn contrast(a: egui::Color32, b: egui::Color32) -> f32;   // WCAG relative-luminance ratio
```

Bypass toggle, Properties and keyboard stay in `app.rs`; `show` only reports clicks. `Graph` is the extension point:
later nodes (luminance key, merge, roto, paint) are new `NodeKind`s and ports; nothing executes graph edges now.

## 5. Readable controls on Nuke gray

| Token | sRGB | Use |
|---|---|---|
| PANEL | `#2C2C2E` (44,44,46) | panel background |
| CANVAS | `#1F1F21` | node canvas |
| FIELD | `#18181A` | checkbox box, text fields (darker than panel) |
| BUTTON / BUTTON_HOVER | `#484A4E` / `#575A5F` | button fill |
| BORDER / BORDER_HOVER | `#80848A` / `#A8ACB2` | 1 px border on every button, checkbox, field (egui default has **0 px** inactive stroke: the bug) |
| CHECKED | `#4C8BD0` | checked box fill, with white 2 px check mark |
| ACCENT | `#3E6EA0` | primary button fill, selected node border |
| FOCUS | `#E0B050` | 2 px focus ring (egui `active` stroke) |
| TEXT / TEXT_DISABLED | `#DCDCDC` / `#808080` | labels |
| disabled fill / border | `#323234` / `#545458` | disabled controls keep a visible border |

Measurable acceptance (unit test in `theme.rs` asserting `contrast()`):
- Control boundary vs PANEL ≥ 3:1 (WCAG 1.4.11): BORDER ≈ 3.7, CHECKED fill ≈ 3.9, FOCUS ≈ 7.0.
- Text: TEXT on BUTTON ≥ 4.5 (≈ 6.5), white on ACCENT ≥ 4.5 (≈ 5.3), TEXT_DISABLED on disabled fill ≥ 3.0 (≈ 3.2).
- Checked vs unchecked differs by fill **and** check mark, never colour alone; focused vs unfocused differs by the
  amber ring, distinct from selection blue.
- If egui's stock checkbox cannot draw CHECKED fill, add `theme::checkbox(ui, &mut bool, text) -> Response` and use it.

## 6. Acceptance

1. Graph test: `Graph::corridorkey().validate()` is Ok; exactly the 7 edges in §2; Media has 2 outgoing, Key 2 incoming
   and 4 outgoing; a type-mismatched or doubled input edge fails `validate`.
2. View test: `to_graph(to_screen(p)) == p`; zoom clamps to 0.5–2.0; after `fit`, every card rect is inside the canvas rect.
3. Theme test: all §5 ratios pass.
4. Manual at 1440×900: dragging a card moves it and its edges follow; pan/zoom/Fit work; collapse leaves a 28 px header;
   Hint viewer ≥ 300 px with the panel at default height; Bypass dashes Hint and its edge; buttons and checkboxes are
   visibly bordered on every panel.
5. `cargo test` and `cargo clippy -- -D warnings` clean.
