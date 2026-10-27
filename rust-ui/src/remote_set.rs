//! Every joined network, each driven by its own `Remote` (own bridge, sidefiles, retry and attempt state). The set
//! only routes: Batch submissions go to the owning network, offers/claims are serialized across networks (round
//! robin), and the pickers see one merged list of PCs. Assignments are namespaced `network id/PC id` (never an index).
use crate::netlink::Source;
use crate::network_setup::ConnectionInfo;
use crate::queue::Entry;
use crate::remote::{Action, Conn, Env, Remote};
use crate::settings::Settings;
use eframe::egui::{self, RichText, Ui};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

const DIM: egui::Color32 = egui::Color32::from_rgb(169, 187, 175);

pub struct RemoteSet {
    source: Source,
    remotes: Vec<Remote>,
    /// Round robin start for the next claim.
    rr: usize,
    /// Sent jobs of removed PCs, read from history.json at most once a second (and after a Clear).
    history: Option<(Instant, Vec<Entry>)>,
    /// The last Clear that could not be saved.
    history_err: Option<String>,
}

fn conn_of(s: &Settings, _sp: Option<&Path>) -> Vec<(Conn, bool)> {
    let name = s.network.name.trim().to_owned(); // empty = do not rename (an existing host name is never overwritten)
    let mut seen = std::collections::HashSet::new();
    s.network.connections().iter().filter(|c| !c.credentials.trim().is_empty() && seen.insert(c.credentials.trim().to_owned())).map(|c| {
        let key = c.credentials.trim().to_owned();
        let default = s.network.default_submit.trim() == key;
        let mut endpoints = c.endpoints.clone();
        let p = c.preferred.trim();
        if !p.is_empty() {
            endpoints.retain(|u| u.as_str() != p);
            endpoints.insert(0, p.to_owned());
        }
        (Conn { creds: Some(PathBuf::from(&key)), enabled: c.enabled, receive: c.receive, name: name.clone(), may_claim: false, endpoints, preferred: p.to_owned(), target_host: c.host_installation_id.trim().to_owned(), local_host: c.local_host }, default)
    }).collect()
}

fn key_of(c: &Conn) -> String {
    c.creds.as_ref().map(|p| p.display().to_string()).unwrap_or_default()
}

impl RemoteSet {
    pub fn new(source: Source) -> Self {
        Self { source, remotes: vec![], rr: 0, history: None, history_err: None }
    }
    pub fn available(&self) -> bool {
        self.source != Source::Off
    }
    pub fn online(&self) -> bool {
        self.remotes.iter().any(Remote::online)
    }
    /// Online opted-in PCs, using the Board's assignment IDs. One row per PC: the first wins when two client rows reach the same network.
    pub fn automatic_candidates(&self, s: &Settings, sp: Option<&Path>) -> Vec<(String, String)> {
        let own: Vec<String> = self.remotes.iter().filter_map(Remote::me).map(str::to_owned).collect();
        let mut seen = std::collections::HashSet::new();
        conn_of(s, sp).iter().flat_map(|(c, _)| {
            self.remotes.iter().find(|r| r.key == key_of(c) && r.accepts_work(c)).into_iter().flat_map(|r| {
                let ns = r.ns();
                r.other_installations(&own).into_iter().filter(|i| i.gpu.is_some()).filter_map(move |i| {
                    ns.as_ref().map(|ns| (format!("network:{ns}/{}", i.id), i.name))
                })
            })
        }).filter(|c| seen.insert(c.0.clone())).collect()
    }
    /// The Automatic candidates that are free right now (see `Remote::is_free`).
    pub fn free_candidates(&self, s: &Settings, sp: Option<&Path>) -> Vec<String> {
        self.automatic_candidates(s, sp).into_iter().map(|c| c.0).filter(|id| {
            let Some((ns, iid)) = id.strip_prefix("network:").and_then(|x| x.split_once('/')) else { return false };
            // Two connections can reach one network: free only if none of them has unfinished work of ours on that PC.
            let same = || self.remotes.iter().filter(|r| r.ns().as_deref() == Some(ns));
            same().any(|r| r.is_free(iid)) && !same().any(|r| r.has_unfinished(iid))
        }).collect()
    }
    fn get(&mut self, creds: &str) -> Option<&mut Remote> {
        self.remotes.iter_mut().find(|r| r.key == creds)
    }

    /// Which network a submission belongs to. `Err` = nothing is sent (never a guess).
    fn route(&self, target: &Option<String>, default: &str, conns: &[(Conn, bool)]) -> Result<(usize, Option<String>), String> {
        match target {
            Some(t) => {
                if let Some((ns, iid)) = t.split_once('/') {
                    let i = self.remotes.iter().enumerate().position(|(i, r)| r.ns().as_deref() == Some(ns) && r.accepts_work(&conns[i].0)).ok_or("that PC's network is not set up on this computer, or no enabled connection to it takes work")?;
                    return Ok((i, Some(iid.to_owned())));
                }
                Err("that PC is not known on any joined network; pick it again".into())
            }
            None => self.remotes.iter().position(|r| !default.is_empty() && r.key == default && r.online())
                .or_else(|| self.remotes.iter().position(Remote::online))
                .map(|i| (i, None))
                .ok_or_else(|| "no network is connected".into()),
        }
    }

    /// Per frame (UI thread or driver thread): sync connections, route Batch submissions once, run every network,
    /// then publish the merged targets and `online`.
    pub fn tick(&mut self, now: Instant, env: &mut Env) {
        let conns = conn_of(env.settings, env.settings_path);
        self.remotes.retain(|r| conns.iter().any(|(c, _)| key_of(c) == r.key));
        for (c, _) in &conns {
            if !self.remotes.iter().any(|r| r.key == key_of(c)) {
                self.remotes.push(Remote::new(self.source.clone(), key_of(c)));
            }
        }
        self.remotes.sort_by_key(|r| conns.iter().position(|(c, _)| key_of(c) == r.key));
        let free = self.free_candidates(env.settings, env.settings_path);
        env.ai.auto_clients = self.automatic_candidates(env.settings, env.settings_path).len();
        env.ai.dispatch_auto(&free, env.metrics);
        let default = env.settings.network.default_submit.trim().to_owned();
        for mut n in env.ai.take_net_outbox() {
            match self.route(&n.target, &default, &conns) {
                Ok((i, t)) => {
                    n.target = t;
                    env.cfg = conns[i].0.clone();
                    self.remotes[i].enqueue(n, env);
                }
                Err(e) => env.ai.notice = Some(format!("{}: not sent, {e}.", n.name)),
            }
        }
        // After routing, so this tick's new sends are already followed.
        env.ai.keep_sent(|sid| self.remotes.iter().any(|r| r.tracks(sid)));
        // One offer/claim/attempt at a time across networks; the next claimer is the next ready network after the last.
        let n = self.remotes.len();
        let chosen = if self.remotes.iter().any(Remote::claim_busy) {
            None
        } else {
            (0..n).map(|k| (self.rr + k) % n).find(|&i| self.remotes[i].claim_ready(now, &conns[i].0))
        };
        for (i, r) in self.remotes.iter_mut().enumerate() {
            env.cfg = conns[i].0.clone();
            env.cfg.may_claim = Some(i) == chosen;
            r.tick(now, env);
        }
        if let Some(i) = chosen.filter(|&i| self.remotes[i].claiming()) {
            self.rr = i + 1;
        }
        let many = self.remotes.len() > 1;
        // A connection to this computer's own network offers the same PCs as the local one: count those once.
        let own: Vec<String> = self.remotes.iter().zip(&conns).filter(|(_, (c, _))| c.local_host).filter_map(|(r, _)| r.ns()).collect();
        env.ai.net_targets = self.remotes.iter().filter(|r| !r.ns().is_some_and(|n| own.contains(&n))).flat_map(|r| r.targets(many.then(|| r.label()).as_deref())).collect();
        env.ai.net_online = self.online();
    }

    pub fn take_finished(&mut self) -> Vec<String> {
        self.remotes.iter_mut().flat_map(Remote::take_finished).collect()
    }
    pub fn shutdown_requested(&mut self) {
        self.remotes.iter_mut().for_each(Remote::shutdown_requested);
    }
    pub fn quiescent(&self) -> bool {
        self.remotes.iter().all(Remote::quiescent)
    }

    /// What Setup needs to name and offer each connection.
    pub fn infos(&self) -> Vec<ConnectionInfo> {
        self.remotes.iter().map(|r| ConnectionInfo { credentials: r.key.clone(), label: r.label(), network_id: r.ns(), online: r.online(), url: r.url() }).collect()
    }

    pub fn cancel_job(&mut self, creds: &str, job_id: &str) {
        if let Some(r) = self.get(creds) {
            r.cancel_job(job_id);
        }
    }
    /// Take back a send its PC never accepted (see `Remote::take_unsent`).
    pub fn take_unsent(&mut self, creds: &str, submission_id: &str) -> bool {
        self.get(creds).is_some_and(|r| r.take_unsent(submission_id))
    }
    pub fn clear_finished(&mut self) {
        self.remotes.iter_mut().for_each(Remote::clear_finished);
    }
    pub fn forget_attempt(&mut self, creds: &str) {
        if let Some(r) = self.get(creds) {
            r.forget_attempt(Some(Path::new(creds)));
        }
    }
    /// Why this connection may not be removed now (`None` = nothing would be orphaned).
    pub fn remove_blocker(&self, creds: &str) -> Option<String> {
        self.remotes.iter().find(|r| r.key == creds)?.remove_blocker(Some(Path::new(creds)))
    }
    /// Start Remove's leave request; false = not sent (offline), the caller removes locally at once.
    pub fn leave(&mut self, creds: &str) -> bool {
        self.get(creds).is_some_and(Remote::leave)
    }
    /// Leave requests that just ended, by connection.
    pub fn take_left(&mut self) -> Vec<(String, crate::remote::Leave)> {
        self.remotes.iter_mut().filter_map(|r| Some((r.key.clone(), r.take_left()?))).collect()
    }
    /// Drop a connection's session (the caller checked `remove_blocker`); its saved state files stay untouched.
    pub fn remove(&mut self, creds: &str) {
        self.remotes.retain(|r| r.key != creds);
    }

    /// All networks' queued and sent jobs, grouped by the PC that owns them.
    pub fn queue_ui(&mut self, ui: &mut Ui) -> Vec<Action> {
        let many = self.remotes.iter().filter(|r| r.has_queue()).count() > 1;
        let mut acts = vec![];
        for r in &mut self.remotes {
            let label = many.then(|| r.label());
            acts.extend(r.queue_ui(ui, label.as_deref()));
        }
        self.history_ui(ui);
        acts
    }

    /// Jobs of PCs that were removed: one collapsed History section, grouped by PC. Kept until the user clears them.
    fn history_ui(&mut self, ui: &mut Ui) {
        if self.history.as_ref().is_none_or(|(at, _)| at.elapsed() >= Duration::from_secs(1)) {
            let live: Vec<String> = self.remotes.iter().map(|r| r.key.clone()).collect();
            self.history = Some((Instant::now(), crate::queue::orphaned(&live)));
        }
        let Some((_, orphans)) = &self.history else { return };
        if orphans.is_empty() {
            return;
        }
        let mut groups: BTreeMap<String, Vec<&Entry>> = BTreeMap::new();
        for e in orphans {
            let pc = e.job.as_ref().and_then(|j| j["on"].as_str()).filter(|s| !s.is_empty()).unwrap_or("Unknown PC");
            groups.entry(pc.to_owned()).or_default().push(e);
        }
        let err = self.history_err.clone();
        let mut clear: Vec<String> = vec![];
        egui::CollapsingHeader::new(RichText::new(format!("History ({})", orphans.len())).color(DIM).size(11.0))
            .id_salt("queue_history")
            .default_open(false)
            .show(ui, |ui| {
                if let Some(e) = &err {
                    ui.colored_label(egui::Color32::from_rgb(225, 110, 110), e);
                }
                for (pc, es) in &groups {
                    ui.horizontal(|ui| {
                        ui.label(RichText::new(pc).strong());
                        ui.label(RichText::new(format!("{} jobs", es.len())).color(DIM).size(11.0));
                        if ui.small_button("Clear").clicked() {
                            clear.extend(es.iter().map(|e| e.id.clone()));
                        }
                    });
                    for e in es {
                        let job = e.job.as_ref();
                        let field = |k: &str| job.and_then(|j| j[k].as_str()).filter(|s| !s.is_empty()).unwrap_or("");
                        let reason = field("reason");
                        let mut parts = vec![field("name"), field("state"), reason];
                        parts.retain(|p| !p.is_empty() && *p != "complete");
                        ui.label(RichText::new(parts.join(" · ")).color(DIM).size(11.0));
                    }
                }
                if ui.button("Clear all").clicked() {
                    clear.extend(orphans.iter().map(|e| e.id.clone()));
                }
            });
        if !clear.is_empty() {
            self.history_err = crate::queue::forget(&clear).err();
            self.history = None;
        }
    }

    /// The Clients table's rows for the other networks' PCs. This computer's own rows are `local_controls` and the
    /// Local row, never these. Nothing when networking is off.
    pub fn rows(&self, ui: &mut Ui, s: &Settings, sp: Option<&Path>, own_net: Option<&str>, own_iid: Option<&str>) -> Vec<Action> {
        if !self.available() {
            return vec![];
        }
        let own: Vec<String> = self.remotes.iter().filter_map(Remote::me).map(str::to_owned).chain(own_iid.map(str::to_owned)).collect();
        let mut acts = vec![];
        for (c, default) in conn_of(s, sp) {
            let Some(r) = self.remotes.iter().find(|r| r.key == key_of(&c)) else { continue };
            let blocker = r.remove_blocker(c.creds.as_deref());
            // `own_net` unknown (None) must not match a connection whose network is unknown too. A leftover
            // self-connection is skipped only when idle and removable; otherwise its controls must stay reachable.
            let own_net_conn = own_net.is_some_and(|n| r.ns().as_deref() == Some(n));
            if c.local_host || (own_net_conn && !c.enabled && r.receiving().is_none() && blocker.is_none()) {
                continue;
            }
            acts.extend(r.row(ui, &c, default, blocker, &own));
            ui.end_row();
        }
        acts
    }

    /// This computer's own connection controls (its local-host connection), or five empty cells when there is none.
    pub fn local_controls(&self, ui: &mut Ui, s: &Settings, sp: Option<&Path>) -> Vec<Action> {
        let conns = conn_of(s, sp);
        if let Some((c, default)) = conns.iter().find(|(c, _)| c.local_host) {
            if let Some(r) = self.remotes.iter().find(|r| r.key == key_of(c)) {
                return r.controls(ui, c, *default, r.remove_blocker(c.creds.as_deref()));
            }
        }
        for _ in 0..5 {
            ui.label("");
        }
        vec![]
    }

    /// This computer's own connection status and attempt message, drawn in the Local row's Status cell.
    pub fn local_status(&self, ui: &mut Ui, s: &Settings, sp: Option<&Path>) -> Vec<Action> {
        let conns = conn_of(s, sp);
        match conns.iter().find(|(c, _)| c.local_host).and_then(|(c, _)| self.remotes.iter().find(|r| r.key == key_of(c)).map(|r| (r, c))) {
            Some((r, c)) => r.status_cell(ui, c),
            None => vec![],
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::settings::Connection;

    #[test]
    fn preferred_goes_first() {
        let (u1, u2, u3) = ("https://192.168.1.2:8443", "https://100.64.1.2:8443", "https://10.0.0.9:8443");
        let mut s = Settings::default();
        let mut c = Connection { credentials: "/c.json".into(), endpoints: vec![u1.into(), u2.into()], preferred: u2.into(), ..Default::default() };
        s.network.connections = vec![c.clone()];
        assert_eq!(conn_of(&s, None)[0].0.endpoints, [u2, u1]);
        c.preferred = u3.into();
        s.network.connections = vec![c];
        assert_eq!(conn_of(&s, None)[0].0.endpoints, [u3, u1, u2]);
    }
}
