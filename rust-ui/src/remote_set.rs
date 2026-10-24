//! Every joined network, each driven by its own `Remote` (own bridge, sidefiles, retry and attempt state). The set
//! only routes: Batch submissions go to the owning network, offers/claims are serialized across networks (round
//! robin), and the pickers see one merged list of PCs. Assignments are namespaced `network id/PC id` (never an index);
//! the legacy `network:{PC id}` form is honoured only while exactly one network knows that PC.
use crate::netlink::Source;
use crate::network_setup::ConnectionInfo;
use crate::remote::{Action, Conn, Env, Install, Remote};
use crate::settings::Settings;
use eframe::egui::{RichText, Ui};
use std::path::{Path, PathBuf};
use std::time::Instant;

pub struct RemoteSet {
    source: Source,
    remotes: Vec<Remote>,
    /// Round robin start for the next claim.
    rr: usize,
    invite_owner: Option<String>,
}

fn conn_of(s: &Settings, sp: Option<&Path>) -> Vec<(Conn, bool)> {
    let name = s.network.name.trim().to_owned(); // empty = do not rename (an existing host name is never overwritten)
    let mut seen = std::collections::HashSet::new();
    s.network.effective_connections(sp).into_iter().filter(|c| !c.credentials.trim().is_empty() && seen.insert(c.credentials.trim().to_owned())).map(|c| {
        let key = c.credentials.trim().to_owned();
        let default = s.network.default_submit.trim() == key;
        (Conn { creds: Some(PathBuf::from(&key)), enabled: c.enabled, receive: c.receive, name: name.clone(), may_claim: false }, default)
    }).collect()
}

fn key_of(c: &Conn) -> String {
    c.creds.as_ref().map(|p| p.display().to_string()).unwrap_or_default()
}

impl RemoteSet {
    pub fn new(source: Source) -> Self {
        Self { source, remotes: vec![], rr: 0, invite_owner: None }
    }
    pub fn available(&self) -> bool {
        self.source != Source::Off
    }
    pub fn online(&self) -> bool {
        self.remotes.iter().any(Remote::online)
    }
    fn get(&mut self, creds: &str) -> Option<&mut Remote> {
        self.remotes.iter_mut().find(|r| r.key == creds)
    }

    /// Which network a submission belongs to. `Err` = nothing is sent (never a guess).
    fn route(&self, target: &Option<String>, default: &str) -> Result<(usize, Option<String>), String> {
        match target {
            Some(t) => {
                if let Some((ns, iid)) = t.split_once('/') {
                    let i = self.remotes.iter().position(|r| r.ns().as_deref() == Some(ns)).ok_or("that PC's network is not set up on this computer")?;
                    return Ok((i, Some(iid.to_owned())));
                }
                let mut owners = self.remotes.iter().enumerate().filter(|(_, r)| r.has_install(t));
                match (owners.next(), owners.next()) {
                    (Some((i, _)), None) => Ok((i, Some(t.clone()))),
                    (None, _) => Err("this older assignment names a PC no connected network knows: pick the PC again".into()),
                    _ => Err("this older assignment matches PCs on several networks: pick the PC again".into()),
                }
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
        let default = env.settings.network.default_submit.trim().to_owned();
        for mut n in env.ai.take_net_outbox() {
            match self.route(&n.target, &default) {
                Ok((i, t)) => {
                    n.target = t;
                    env.cfg = conns[i].0.clone();
                    self.remotes[i].enqueue(n, env);
                }
                Err(e) => env.ai.notice = Some(format!("{}: not sent, {e}.", n.name)),
            }
        }
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
        env.ai.net_targets = self.remotes.iter().flat_map(|r| r.targets(many.then(|| r.label()).as_deref())).collect();
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
    pub fn has_queue(&self) -> bool {
        self.remotes.iter().any(Remote::has_queue)
    }
    pub fn other_installations(&self) -> Vec<Install> {
        let many = self.remotes.len() > 1;
        self.remotes.iter().flat_map(|r| r.other_installations(many.then(|| r.label()).as_deref())).collect()
    }

    /// What Setup needs to name and offer each connection (invitations pick one explicitly).
    pub fn infos(&self) -> Vec<ConnectionInfo> {
        self.remotes.iter().map(|r| ConnectionInfo { credentials: r.key.clone(), label: r.label(), network_id: r.ns(), online: r.online() }).collect()
    }
    /// `false` = that connection is no longer set up, so nothing was written.
    pub fn invite(&mut self, creds: &str, path: String) -> bool {
        self.invite_owner = Some(creds.to_owned());
        match self.get(creds) {
            Some(r) => {
                r.invite(path);
                true
            }
            None => {
                self.invite_owner = None;
                false
            }
        }
    }
    pub fn invite_note(&self) -> Option<&str> {
        let key = self.invite_owner.as_deref()?;
        self.remotes.iter().find(|r| r.key == key)?.invite_note.as_deref()
    }

    pub fn cancel_job(&mut self, creds: &str, job_id: &str) {
        if let Some(r) = self.get(creds) {
            r.cancel_job(job_id);
        }
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
        acts
    }

    /// The Clients page's network section: one card per joined network.
    pub fn ui(&mut self, ui: &mut Ui, s: &Settings, sp: Option<&Path>) -> Vec<Action> {
        if !self.available() {
            ui.label(RichText::new("Network rendering is not available in this build.").color(egui_dim()));
            return vec![];
        }
        let conns = conn_of(s, sp);
        if conns.is_empty() {
            ui.label(RichText::new("No networks joined yet. Join one below.").color(egui_dim()));
        }
        let mut acts = vec![];
        for (c, default) in conns {
            if let Some(r) = self.remotes.iter().find(|r| r.key == key_of(&c)) {
                let blocker = r.remove_blocker(c.creds.as_deref());
                acts.extend(r.card(ui, &c, default, blocker));
            }
        }
        acts
    }
}

fn egui_dim() -> eframe::egui::Color32 {
    eframe::egui::Color32::from_rgb(169, 187, 175)
}
