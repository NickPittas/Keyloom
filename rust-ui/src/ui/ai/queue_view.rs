//! Board queue, grouped per clip: one bold title, then the clip's actual stages in fixed order (Pre-matte, Hint,
//! Key, Export) with one status line each. Display only: every state comes from the scheduler's own jobs; a
//! percent is shown only for real, bounded worker progress (queued/waiting/starting show words, never a number).
use super::*;
use egui::Vec2;
use crate::hints::HINT_DIR;
use crate::tasks::TaskKind;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Stage {
    Prepare,
    Pre,
    Hint,
    Key,
    Export,
}

impl Stage {
    fn heading(self) -> &'static str {
        match self {
            Stage::Prepare => "Prepare",
            Stage::Pre => "Pre-matte",
            Stage::Hint => "Hint",
            Stage::Key => "Keying",
            Stage::Export => "Export",
        }
    }
}

/// What one job is doing, reduced to what the display needs.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Phase {
    Queued,
    /// Queued behind an earlier step of the same chain.
    Waiting,
    Starting,
    /// Bounded worker progress, `None` while the total is unknown.
    Running(Option<f32>),
    Done,
    Failed,
    Cancelled,
    Lost,
}

pub fn phase_of(s: &JobState, waiting: bool) -> Phase {
    use crate::worker::Outcome;
    match s {
        JobState::Queued if waiting => Phase::Waiting,
        JobState::Queued => Phase::Queued,
        JobState::Dispatching { .. } => Phase::Starting,
        JobState::Running { done, total, .. } => Phase::Running((*total > 0).then(|| (*done).min(*total) as f32 / *total as f32)),
        JobState::Done { info, .. } => match info.outcome {
            Outcome::Complete => Phase::Done,
            Outcome::Cancelled => Phase::Cancelled,
            Outcome::Failed => Phase::Failed,
        },
        JobState::Rejected { .. } | JobState::Failed { .. } => Phase::Failed,
        JobState::Cancelled => Phase::Cancelled,
        JobState::Lost { .. } => Phase::Lost,
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Cell {
    pub text: String,
    /// Bar fill: 0 until real progress exists, 1 only when every job completed.
    pub frac: f32,
    pub bad: bool,
}

/// The status of a stage made of one or more jobs (several exports aggregate into one).
pub fn cell_of(p: &[Phase]) -> Cell {
    let n = p.len().max(1) as f32;
    let has = |f: fn(&Phase) -> bool| p.iter().any(f);
    // Never 100% until every job really completed; a running job is capped just below.
    let mean = p
        .iter()
        .map(|x| match x {
            Phase::Done => 1.0,
            Phase::Running(Some(f)) => f.clamp(0.0, 0.99),
            _ => 0.0,
        })
        .sum::<f32>()
        / n;
    let word = |t: &str, bad| Cell { text: t.into(), frac: mean, bad };
    if has(|x| *x == Phase::Lost) {
        return word("Lost", true);
    }
    if has(|x| *x == Phase::Failed) {
        return word("Failed", true);
    }
    if has(|x| *x == Phase::Cancelled) {
        return word("Cancelled", true);
    }
    if !p.is_empty() && p.iter().all(|x| *x == Phase::Done) {
        return Cell { text: "100%".into(), frac: 1.0, bad: false };
    }
    let known = has(|x| matches!(x, Phase::Done | Phase::Running(Some(_))));
    if known {
        return Cell { text: format!("{}%", (mean * 100.0).floor() as u32), frac: mean, bad: false };
    }
    let w = if has(|x| matches!(x, Phase::Running(None) | Phase::Starting)) {
        "Starting"
    } else if has(|x| *x == Phase::Queued) {
        "Queued"
    } else {
        "Waiting"
    };
    word(w, false)
}

#[derive(Clone, Debug)]
pub struct QueueRow {
    pub stage: Stage,
    /// "Pre-matte · SAM", "Keying", "Export".
    pub heading: String,
    pub cell: Cell,
    /// Export products with their own status, shown only when there are several.
    pub products: Vec<(String, String)>,
    pub ids: Vec<String>,
    /// A pre-matte that already existed on the clip: no job, no percent.
    pub precomputed: bool,
    /// Something the user may need to act on (failed, lost, cancel pending, stalled).
    pub attention: bool,
    pub active: bool,
}

#[derive(Clone, Debug)]
pub struct QueueRun {
    pub client: String,
    pub rows: Vec<QueueRow>,
    pub active: bool,
}

#[derive(Clone, Debug)]
pub struct QueueSection {
    pub root: PathBuf,
    pub title: String,
    pub runs: Vec<QueueRun>,
}

impl QueueSection {
    pub fn active(&self) -> bool {
        self.runs.iter().any(|r| r.active)
    }
}

struct Fact {
    client: String,
    id: String,
    root: PathBuf,
    after: Vec<String>,
    stage: Stage,
    tool: String,
    state: JobState,
    cancel_requested: bool,
    stalled: bool,
}

fn product_label(p: &str) -> String {
    match p {
        "fg" => "FG".into(),
        "matte" => "Matte".into(),
        "comp" => "Comp".into(),
        "processed" => "Processed".into(),
        o => o.into(),
    }
}

/// Stage and tool of a task from its actual kind and publish folder, never from its display label.
fn classify(j: &crate::tasks::TaskJob) -> (Stage, String) {
    let into = |d: &str| j.publish_dir.as_deref().and_then(|p| p.file_name()).is_some_and(|n| n == d);
    let producer = |tool: &str| (if into(HINT_DIR) { Stage::Hint } else { Stage::Pre }, tool.to_string());
    match j.kind {
        TaskKind::Gvm => producer("GVM"),
        TaskKind::Birefnet => producer("BiRefNet"),
        TaskKind::Chroma => producer("Chroma"),
        TaskKind::SamTrack => producer("SAM"),
        TaskKind::SamPreview => (Stage::Pre, "SAM preview".into()),
        TaskKind::Matanyone2 => (Stage::Hint, "MatAnyone2".into()),
        TaskKind::Videomama => (Stage::Hint, "VideoMaMa".into()),
        TaskKind::PrematteHint => (Stage::Hint, "Use pre-matte".into()),
        TaskKind::ImportHint => (Stage::Hint, "Import".into()),
        TaskKind::PrepareSource => (Stage::Prepare, "Extract frames".into()),
        TaskKind::ExportVideo => (Stage::Export, product_label(j.params.get("product").and_then(Value::as_str).unwrap_or("video"))),
    }
}

fn client_of(state: &JobState, fallback: String) -> String {
    match state {
        JobState::Dispatching { target } | JobState::Running { target, .. } | JobState::Done { target, .. } | JobState::Rejected { target, .. } | JobState::Lost { target, .. } => target.clone(),
        _ => fallback,
    }
}

impl AiUi {
    fn facts(&self) -> Vec<(usize, Fact)> {
        let stalled: Vec<String> = self.sched.stalled(STALL).into_iter().map(|s| s.job_id).collect();
        self.tracked
            .iter()
            .enumerate()
            .filter_map(|(i, t)| {
                let id = t.job_id.clone();
                let stalled = stalled.contains(&id);
                let f = if let Some(e) = self.sched.job(&id) {
                    let fallback = match &e.placement { Placement::Target(id) => id.clone(), Placement::Auto => "Automatic".into() };
                    Fact { client: client_of(&e.state, fallback), id, root: e.job.clip_root.clone(), after: e.after.clone(), stage: Stage::Key, tool: String::new(), state: e.state.clone(), cancel_requested: e.cancel_requested, stalled }
                } else {
                    let e = self.sched.task(&id)?;
                    let (stage, tool) = classify(&e.job);
                    let local = self.sched.targets().iter().find(|t| t.local).map(|t| t.id.clone()).unwrap_or_else(|| "This computer".into());
                    Fact { client: client_of(&e.state, local), id, root: e.job.clip_root.clone(), after: e.after.clone(), stage, tool, state: e.state.clone(), cancel_requested: e.cancel_requested, stalled }
                };
                Some((i, f))
            })
            .collect()
    }

    /// Clips in the order their first job was queued; each clip's runs (chains linked by `after`) separately, so a
    /// finished earlier run never mixes into the latest one. Pure data: the drawing and the tests both read this.
    pub fn queue_view(&self) -> Vec<QueueSection> {
        let facts = self.facts();
        let unresolved = self.unresolved();
        let at = |id: &str| facts.iter().position(|(_, f)| f.id == id);
        let mut comp: Vec<usize> = (0..facts.len()).collect();
        for (k, (_, f)) in facts.iter().enumerate() {
            for a in &f.after {
                if let Some(j) = at(a) {
                    let (from, to) = (comp[j], comp[k]);
                    comp.iter_mut().filter(|c| **c == from).for_each(|c| *c = to);
                }
            }
        }
        let mut sections: Vec<QueueSection> = vec![];
        let mut seen: Vec<(PathBuf, usize)> = vec![]; // (root, component) of runs already emitted
        for (k, (i, f)) in facts.iter().enumerate() {
            if seen.contains(&(f.root.clone(), comp[k])) {
                continue;
            }
            seen.push((f.root.clone(), comp[k]));
            let mine: Vec<&Fact> = facts.iter().enumerate().filter(|(j, g)| comp[*j] == comp[k] && g.1.root == f.root).map(|(_, (_, f))| f).collect();
            let run = self.run_view(&mine, &unresolved);
            match sections.iter_mut().find(|s| s.root == f.root) {
                Some(s) => s.runs.push(run),
                None => sections.push(QueueSection { root: f.root.clone(), title: self.tracked[*i].name.clone(), runs: vec![run] }),
            }
        }
        sections
    }

    fn run_view(&self, jobs: &[&Fact], unresolved: &[String]) -> QueueRun {
        let waiting = |f: &Fact| f.after.iter().any(|a| self.sched.state_of(a).is_some_and(|s| !s.is_terminal()));
        let mut rows: Vec<QueueRow> = vec![];
        for stage in [Stage::Prepare, Stage::Pre, Stage::Hint, Stage::Key, Stage::Export] {
            let of: Vec<&&Fact> = jobs.iter().filter(|f| f.stage == stage).collect();
            if of.is_empty() {
                continue;
            }
            let phases: Vec<Phase> = of.iter().map(|f| phase_of(&f.state, waiting(f))).collect();
            let mut tools: Vec<String> = vec![];
            for f in &of {
                if !f.tool.is_empty() && !tools.contains(&f.tool) {
                    tools.push(f.tool.clone());
                }
            }
            let heading = match (stage, tools.is_empty()) {
                (Stage::Export, _) if of.len() == 1 => format!("Export · {}", of[0].tool),
                (Stage::Export, _) | (_, true) => stage.heading().to_string(),
                _ => format!("{} · {}", stage.heading(), tools.join(" + ")),
            };
            let products = if stage == Stage::Export && of.len() > 1 {
                of.iter().zip(&phases).map(|(f, p)| (f.tool.clone(), cell_of(&[*p]).text)).collect()
            } else {
                vec![]
            };
            rows.push(QueueRow {
                stage,
                heading,
                cell: cell_of(&phases),
                products,
                ids: of.iter().map(|f| f.id.clone()).collect(),
                precomputed: false,
                attention: of.iter().zip(&phases).any(|(f, p)| f.stalled || (f.cancel_requested && !f.state.is_terminal()) || unresolved.contains(&f.id) || matches!(p, Phase::Failed | Phase::Lost)),
                active: of.iter().any(|f| !f.state.is_terminal()),
            });
        }
        // A manual SAM/Chroma pre-matte used as it is: shown as such, with no job behind it.
        if let Some(label) = jobs.iter().find_map(|f| self.precomputed.get(&f.id)) {
            if !rows.iter().any(|r| r.stage == Stage::Pre) {
                let at = rows.iter().position(|r| r.stage > Stage::Pre).unwrap_or(rows.len());
                rows.insert(
                    at,
                    QueueRow {
                        stage: Stage::Pre,
                        heading: format!("Pre-matte · {label}"),
                        cell: Cell { text: "Precomputed".into(), frac: 0.0, bad: false },
                        products: vec![],
                        ids: vec![],
                        precomputed: true,
                        attention: false,
                        active: false,
                    },
                );
            }
        }
        let active = rows.iter().any(|r| r.active) || jobs.iter().any(|f| unresolved.contains(&f.id));
        QueueRun { client: jobs.first().map(|f| f.client.clone()).unwrap_or_else(|| "This computer".into()), rows, active }
    }

    /// Board queue: one block per clip. Active, queued and unresolved clips are always shown; only finished
    /// whole clips are limited (newest 12).
    pub fn queue_ui(&mut self, ui: &mut Ui, local: &crate::metrics::View, active: &[u32], telemetry: &std::collections::BTreeMap<String, crate::metrics::ClientTelemetry>) {
        // Never let the content ask the panel for more than it has been given.
        let w = ui.available_width();
        ui.set_max_width(w);
        let all = self.queue_view();
        let mut clients = vec![];
        for run in all.iter().flat_map(|s| &s.runs) {
            if !clients.contains(&run.client) { clients.push(run.client.clone()); }
        }
        if clients.is_empty() {
            clients.push(self.sched.targets().iter().find(|t| t.local).map(|t| t.id.clone()).unwrap_or_else(|| "This computer".into()));
        }
        for client in clients {
            let target = self.sched.targets().into_iter().find(|t| t.id == client);
            let is_local = target.as_ref().is_some_and(|t| t.local);
            ui.horizontal(|ui| {
                let (dot, _) = ui.allocate_exact_size(egui::vec2(10.0, 14.0), egui::Sense::hover());
                ui.painter().circle_filled(dot.center(), 3.0, super::super::theme::ACCENT);
                ui.label(RichText::new(if is_local { "This computer" } else { &client }).strong());
            });
            let t = if is_local { crate::metrics::ClientTelemetry::Local(local.clone()) } else {
                telemetry.get(&client).cloned().unwrap_or_else(|| crate::metrics::ClientTelemetry::Unreported("No statistics reported".into()))
            };
            crate::ui::metrics_bar::machine_meters(ui, &t, if is_local { active } else { &[] });
            let all: Vec<_> = all.iter().cloned().filter_map(|mut s| {
                s.runs.retain(|r| r.client == client);
                (!s.runs.is_empty()).then_some(s)
            }).collect();
            if all.is_empty() { ui.label(RichText::new("Idle").color(DIM).size(11.0)); }
        let finished = all.iter().filter(|s| !s.active()).count();
        let mut skip = finished.saturating_sub(12);
        let mut shown = 0;
        for s in all {
            if !s.active() && skip > 0 {
                skip -= 1;
                continue;
            }
            if shown > 0 { ui.add_space(12.0); }
            shown += 1;
            egui::Frame::new().fill(super::super::theme::CARD)
                .stroke(egui::Stroke::new(1.0, Color32::from_rgb(77, 94, 82)))
                .corner_radius(5).inner_margin(14).show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.add(egui::Label::new(RichText::new(&s.title).strong().size(12.5)).truncate()).on_hover_text(s.root.display().to_string());
                ui.add_space(10.0);
                let n = s.runs.len();
                for (r, run) in s.runs.iter().enumerate() {
                    if n - r > 2 && !run.active && !run.rows.iter().any(|x| x.attention) { continue; }
                    if r + 1 < n && n > 1 {
                        ui.label(RichText::new("Earlier run").color(DIM).size(11.0));
                    }
                    self.run_ui(ui, run);
                }
            });
        }
        ui.add_space(10.0);
        }
    }

    fn run_ui(&mut self, ui: &mut Ui, run: &QueueRun) {
        let mut cancel: Vec<String> = vec![];
        for row in &run.rows {
            // Full method name on its own line, then the bar (and Cancel) underneath: nothing is truncated at 268 px.
            let live = row.active && !row.attention_cancel_pending(self);
            // Widths fixed from the panel BEFORE the row is laid out, so the bar can never push the panel wider.
            let (avail, gap) = (ui.available_width(), ui.spacing().item_spacing.x);
            // Wide panel: stage, bar and Cancel on one line. Narrow: the full stage name above, bar below.
            let one_line = avail >= 290.0;
            let label_w = if one_line { 136.0 + gap } else { 0.0 };
            let cancel_w = if live { 54.0 + gap } else { 0.0 };
            let w = (avail - label_w - cancel_w - 2.0).max(40.0);
            if !one_line {
                ui.add(egui::Label::new(RichText::new(&row.heading).size(12.0)).wrap());
            }
            ui.allocate_ui_with_layout(Vec2::new(avail, 18.0), egui::Layout::left_to_right(egui::Align::Center), |ui| {
                ui.set_max_width(avail);
                if one_line {
                    let (rect, response) = ui.allocate_exact_size(Vec2::new(136.0, 20.0), egui::Sense::hover());
                    ui.painter().with_clip_rect(rect).text(rect.left_center(), egui::Align2::LEFT_CENTER, &row.heading, egui::FontId::proportional(12.0), ui.visuals().text_color());
                    response.on_hover_text(&row.heading);
                }
                let fill = if row.cell.bad { BAD } else if row.precomputed { DIM } else { super::super::theme::ACCENT };
                ui.add_sized([w, 16.0], egui::ProgressBar::new(row.cell.frac).desired_width(w).desired_height(16.0).fill(fill).text(RichText::new(&row.cell.text).size(11.0)));
                if live && ui.add_sized([54.0, 16.0], egui::Button::new("Cancel").small()).clicked() {
                    cancel.extend(row.ids.iter().cloned());
                }
            });
            ui.add_space(4.0);
            if !row.products.is_empty() {
                let t = row.products.iter().map(|(p, s)| format!("{p} {s}")).collect::<Vec<_>>().join(" · ");
                ui.add(egui::Label::new(RichText::new(t).color(DIM).size(11.0)).wrap());
            }
        }
        let ids: Vec<(Stage, String)> = run.rows.iter().flat_map(|r| r.ids.iter().map(|i| (r.stage, i.clone()))).collect();
        if !ids.is_empty() {
            let open = run.rows.iter().any(|r| r.attention).then_some(true);
            let first = ids[0].1.clone();
            egui::CollapsingHeader::new(RichText::new("Details").color(DIM).size(11.0)).id_salt(("qd", first)).open(open).show(ui, |ui| {
                for (stage, id) in &ids {
                    ui.label(RichText::new(stage.heading()).color(DIM).size(11.0));
                    self.job_row(ui, id, false);
                }
            });
        }
        for id in cancel {
            if self.sched.state_of(&id).is_some_and(|s| !s.is_terminal()) {
                self.cancel(&id);
            }
        }
    }
}

impl QueueRow {
    /// Every live job already has its cancel requested: the inline button would do nothing.
    fn attention_cancel_pending(&self, ai: &AiUi) -> bool {
        self.ids.iter().filter(|i| ai.sched.state_of(i).is_some_and(|s| !s.is_terminal())).all(|i| {
            ai.sched.job(i).map(|e| e.cancel_requested).or_else(|| ai.sched.task(i).map(|e| e.cancel_requested)).unwrap_or(false)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use Phase::*;

    #[test]
    fn percent_is_real_progress_only() {
        let t = |p: &[Phase]| cell_of(p).text;
        assert_eq!(t(&[Queued]), "Queued");
        assert_eq!(t(&[Waiting]), "Waiting");
        assert_eq!(t(&[Starting]), "Starting");
        assert_eq!(t(&[Running(None)]), "Starting");
        assert_eq!(t(&[Running(Some(0.5))]), "50%");
        assert_eq!(t(&[Running(Some(1.0))]), "99%", "a running job is never 100%");
        assert_eq!(t(&[Done]), "100%");
        // Aggregate export: two of three done, the third waiting.
        assert_eq!(t(&[Done, Done, Waiting]), "66%");
        assert_eq!(cell_of(&[Queued]).frac, 0.0);
    }

    #[test]
    fn failures_are_never_complete() {
        for (p, w) in [(Failed, "Failed"), (Cancelled, "Cancelled"), (Lost, "Lost")] {
            let c = cell_of(&[Done, p]);
            assert_eq!((c.text.as_str(), c.bad), (w, true));
            assert!(c.frac < 1.0);
        }
        assert_eq!(cell_of(&[Failed, Lost]).text, "Lost");
    }
}
