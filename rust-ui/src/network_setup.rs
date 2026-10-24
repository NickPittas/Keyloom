//! Network setup: host a per-user network service on this PC and/or join a network by address or invitation, all
//! from the UI. It owns its own lazy bridge link (separate from `remote::Remote`, which only runs while paired and
//! enabled), so setup works unpaired/offline and never touches the client's credentials lock. Nothing runs until the
//! user presses a button (or opens the page, which only reads the service status). A join writes a NEW credentials
//! file under `<settings>/network/`; the App switches to it only after success and only when nothing would be
//! orphaned. Secrets never pass through here: the bridge writes credentials and invitations itself.
use crate::netlink::{Link, Op, Source, State};
use crate::remote::{err_text, nlog};
use crate::settings::Settings;
use crate::ui::picker::PickKind;
use eframe::egui::{self, RichText, Ui};
use serde_json::{json, Value};
use std::collections::{HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

const DIM: egui::Color32 = egui::Color32::from_rgb(169, 187, 175);
const IDLE_DROP: Duration = Duration::from_secs(30);

#[derive(Clone, Debug, Default)]
pub struct Svc {
    pub installed: bool,
    pub running: bool,
    pub address: String,
    pub port: u64,
    pub allow_join: bool,
    pub network_id: Option<String>,
}

enum Pend {
    Status,
    Service(&'static str),
    Join { creds: PathBuf },
}

/// A finished join waiting for the App to switch to it.
#[derive(Debug)]
pub struct Joined {
    pub creds: String,
    /// The bridge reused the existing connection (same network): nothing new was registered.
    pub reused: bool,
}

/// What the UI needs from the App each frame.
pub struct Ctx<'a> {
    pub settings: &'a Settings,
    pub settings_path: Option<&'a Path>,
    pub connected: bool,
    pub blocker: Option<String>,
    pub invite_note: Option<&'a str>,
}

/// Work for the App (it owns the picker and the connected client).
pub enum Cmd {
    Invite(String),
    Browse(PickKind),
}

pub struct Setup {
    source: Source,
    link: Option<Link>,
    pend: HashMap<String, Pend>,
    queue: VecDeque<(Op, Value, Pend)>,
    idle_since: Option<Instant>,
    seen: Option<Instant>,
    svc: Option<Svc>,
    note: Option<String>,
    joined: Option<Joined>,
    address: String,
    port: String,
    allow_join: bool,
    join_host: String,
    join_port: String,
    name: String,
    invitation: String,
    invite_path: String,
    confirm: bool,
    remove_armed: bool,
    dir: String,
}

fn state_dir(sp: Option<&Path>) -> Option<PathBuf> {
    sp.and_then(Path::parent).map(|d| d.join("network").join("server"))
}

/// `https://host:port` (IPv6 literals bracketed).
fn url(host: &str, port: &str) -> String {
    let h = host.trim();
    if h.contains(':') && !h.starts_with('[') { format!("https://[{h}]:{}", port.trim()) } else { format!("https://{h}:{}", port.trim()) }
}

impl Setup {
    pub fn new(source: Source) -> Self {
        Self {
            source,
            link: None,
            pend: HashMap::new(),
            queue: VecDeque::new(),
            idle_since: None,
            seen: None,
            svc: None,
            note: None,
            joined: None,
            address: String::new(),
            port: "8443".into(),
            allow_join: true,
            join_host: String::new(),
            join_port: "8443".into(),
            name: crate::remote::default_name(),
            invitation: String::new(),
            invite_path: String::new(),
            confirm: false,
            remove_armed: false,
            dir: String::new(),
        }
    }

    pub fn take_joined(&mut self) -> Option<Joined> {
        self.joined.take()
    }
    pub fn set_note(&mut self, s: String) {
        self.note = Some(s);
    }
    /// The App's picker answered (`None` = cancelled).
    pub fn browse_done(&mut self, kind: PickKind, path: Option<PathBuf>) {
        if let Some(p) = path.map(|p| p.display().to_string()) {
            match kind {
                PickKind::NetInvitationOpen => self.invitation = p,
                _ => self.invite_path = p,
            }
        }
    }

    fn busy(&self) -> bool {
        !self.pend.is_empty() || !self.queue.is_empty()
    }
    fn joining(&self) -> bool {
        self.pend.values().any(|p| matches!(p, Pend::Join { .. })) || self.queue.iter().any(|q| matches!(q.2, Pend::Join { .. }))
    }
    fn push(&mut self, op: Op, mut args: Value, p: Pend) {
        if matches!(op, Op::ServiceStatus | Op::ServiceInstall | Op::ServiceStart | Op::ServiceStop | Op::ServiceRemove) {
            args["state_dir"] = json!(self.dir);
        }
        self.queue.push_back((op, args, p));
    }

    /// Per frame: start the link when something is queued, send, collect replies, drop the link when idle.
    pub fn tick(&mut self, repo: Option<&Path>, python: Option<&Path>, wake: &Arc<dyn Fn() + Send + Sync>) {
        if self.link.is_none() && !self.queue.is_empty() {
            match self.source.command(repo, python) {
                Ok(Some(cmd)) => match Link::start(cmd, wake.clone()) {
                    Ok(l) => self.link = Some(l),
                    Err(e) => self.fail_all(e),
                },
                Ok(None) => self.fail_all("network is not available in this build".into()),
                Err(e) => self.fail_all(e),
            }
        }
        let Some(l) = &mut self.link else { return };
        if l.state() == State::Ready {
            while let Some((op, args, p)) = self.queue.pop_front() {
                match l.request(op, args) {
                    Ok(id) => {
                        self.pend.insert(id, p);
                    }
                    Err(e) => {
                        self.queue.clear();
                        self.pend.clear();
                        self.note = Some(format!("Network link failed ({})", err_text(&e)));
                        break;
                    }
                }
            }
        }
        let replies = l.poll();
        let ended = matches!(l.state(), State::Ended(_));
        for r in replies {
            if let Some(p) = r.id.and_then(|id| self.pend.remove(&id)) {
                self.on_reply(p, r.outcome);
            }
        }
        if ended {
            self.link = None;
            self.fail_all("the setup bridge stopped before confirming the operation; refresh the service status before trying again".into());
        } else if self.busy() {
            self.idle_since = None;
        } else if self.idle_since.get_or_insert_with(Instant::now).elapsed() > IDLE_DROP {
            self.link = None; // Drop kills and reaps the bridge
            self.idle_since = None;
        }
    }

    fn fail_all(&mut self, why: String) {
        self.queue.clear();
        self.pend.clear();
        self.note = Some(format!("Network setup is unavailable: {why}"));
    }

    fn on_reply(&mut self, p: Pend, out: Result<Value, crate::netlink::LinkError>) {
        match (p, out) {
            (Pend::Status, Ok(v)) => {
                let g = |k: &str| v.get(k).and_then(Value::as_bool).unwrap_or(false);
                let svc = Svc {
                    installed: g("installed"),
                    running: g("running"),
                    address: v.get("address").and_then(Value::as_str).unwrap_or("").to_owned(),
                    port: v.get("port").and_then(Value::as_u64).unwrap_or(0),
                    allow_join: g("allow_address_join"),
                    network_id: v.get("network_id").or_else(|| v.get("fingerprint_sha256")).and_then(Value::as_str).map(str::to_owned),
                };
                if svc.installed {
                    (self.address, self.port, self.allow_join) = (svc.address.clone(), svc.port.to_string(), svc.allow_join);
                }
                self.svc = Some(svc);
            }
            (Pend::Status, Err(e)) => self.note = Some(format!("Could not read the service status ({})", err_text(&e))),
            (Pend::Service(what), Ok(_)) => {
                nlog(&format!("setup {what} ok"));
                self.note = Some(format!("{what}: done."));
                self.push(Op::ServiceStatus, json!({"state_dir": self.dir}), Pend::Status);
            }
            (Pend::Service(what), Err(e)) => self.note = Some(format!("{what} failed ({})", err_text(&e))),
            (Pend::Join { creds }, Ok(v)) => {
                nlog("setup join ok");
                let reused = v.get("reused").and_then(Value::as_bool).unwrap_or(false);
                let path = v.get("credentials_path").and_then(Value::as_str).map_or_else(|| creds.display().to_string(), str::to_owned);
                self.note = Some(if reused { "This computer was already on that network; keeping its connection.".into() } else { "Joined the network.".into() });
                self.joined = Some(Joined { creds: path, reused });
            }
            (Pend::Join { .. }, Err(e)) => self.note = Some(format!("Join failed; the current connection is unchanged ({})", err_text(&e))),
        }
    }

    /// Start a join. A fresh credentials file is offered; `existing` (the current connection) lets the bridge reuse
    /// it when the host is the same network instead of registering this PC again.
    fn join(&mut self, sp: Option<&Path>, existing: Option<PathBuf>, op: Op, mut args: Value) {
        let Some(dir) = sp.and_then(Path::parent).map(|d| d.join("network")) else {
            self.note = Some("Settings location is unknown; cannot save a connection.".into());
            return;
        };
        if let Err(e) = crate::remote::private_dir(&dir) {
            self.note = Some(format!("Join not started: the folder for credentials could not be made private ({e})"));
            return;
        }
        let stamp = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_millis());
        let creds = dir.join(format!("credentials-{stamp}.json"));
        args["credentials_path"] = json!(creds.display().to_string());
        if let Some(e) = existing {
            args["existing_credentials_path"] = json!(e.display().to_string());
        }
        self.note = Some("Joining…".into());
        self.push(op, args, Pend::Join { creds });
    }

    pub fn ui(&mut self, ui: &mut Ui, c: &Ctx) -> Vec<Cmd> {
        let mut cmds = vec![];
        let sp = c.settings_path;
        let Some(dir) = state_dir(sp).map(|d| d.display().to_string()) else { return cmds };
        self.dir.clone_from(&dir);
        if self.seen.is_none_or(|t| t.elapsed() > Duration::from_secs(2)) && !self.busy() {
            self.push(Op::ServiceStatus, json!({}), Pend::Status);
        }
        self.seen = Some(Instant::now());
        // Fill the state dir of every service request (kept out of the normal UI).
        for q in self.queue.iter_mut().filter(|q| matches!(q.2, Pend::Status | Pend::Service(_))) {
            q.1["state_dir"] = json!(dir);
        }
        let cur = crate::remote::credentials_path(c.settings, sp).filter(|p| p.exists());
        let paired = cur.is_some();
        let idle = !self.busy();
        let port_ok = self.port.trim().parse::<u16>().is_ok_and(|p| p > 0);
        let svc = self.svc.clone().unwrap_or_default();
        ui.add_space(8.0);
        egui::CollapsingHeader::new("Network setup").default_open(!paired).show(ui, |ui| {
            ui.label(RichText::new("Every PC that joins the same host network sees all the others. Hosting, submitting and receiving are independent: one PC can do all three. No command line is needed.").color(DIM).size(11.5));
            ui.add_space(4.0);
            ui.horizontal_wrapped(|ui| {
                ui.label("This computer’s name");
                ui.add(egui::TextEdit::singleline(&mut self.name).desired_width(180.0));
            });
            ui.strong("Host a network on this PC");
            let line = match (&self.svc, svc.installed, svc.running) {
                (None, ..) => "Checking the service…".to_owned(),
                (_, false, _) => "Not installed.".into(),
                (_, true, false) => "Installed, stopped.".into(),
                (_, true, true) => format!("Running on {}:{}{}.", svc.address, svc.port, if svc.allow_join { " (trusted-network joins by address allowed)" } else { " (invitation only)" }),
            };
            ui.label(RichText::new(line).color(DIM));
            if let Some(id) = &svc.network_id {
                ui.label(RichText::new(format!("Network ID: {id}")).color(DIM).size(11.0));
            }
            ui.horizontal_wrapped(|ui| {
                ui.label("Address");
                ui.add_enabled(!svc.installed, egui::TextEdit::singleline(&mut self.address).desired_width(170.0).hint_text("LAN or Tailscale IP"));
                ui.label("Port");
                ui.add_enabled(!svc.installed, egui::TextEdit::singleline(&mut self.port).desired_width(60.0));
                ui.add_enabled_ui(!svc.installed, |ui| crate::ui::theme::checkbox(ui, &mut self.allow_join, "Trusted network: allow joining by address"));
            });
            ui.horizontal_wrapped(|ui| {
                if !svc.installed && ui.add_enabled(idle && port_ok && !self.address.trim().is_empty(), egui::Button::new("Install and start")).clicked() {
                    let a = json!({"address": self.address.trim(), "port": self.port.trim().parse::<u16>().unwrap_or(8443), "allow_address_join": self.allow_join});
                    self.push(Op::ServiceInstall, a, Pend::Service("Install and start"));
                }
                if svc.installed {
                    if ui.add_enabled(idle && !svc.running, egui::Button::new("Start")).clicked() { self.push(Op::ServiceStart, json!({}), Pend::Service("Start")); }
                    if ui.add_enabled(idle && svc.running, egui::Button::new("Stop")).clicked() { self.push(Op::ServiceStop, json!({}), Pend::Service("Stop")); }
                    let label = if self.remove_armed { "Click again to remove the service" } else { "Remove" };
                    if ui.add_enabled(idle, egui::Button::new(label)).clicked() {
                        if self.remove_armed {
                            self.push(Op::ServiceRemove, json!({}), Pend::Service("Remove"));
                        }
                        self.remove_armed = !self.remove_armed;
                    }
                }
                if ui.add_enabled(idle, egui::Button::new("Refresh")).clicked() { self.push(Op::ServiceStatus, json!({}), Pend::Status); }
            });
            let gate = c.blocker.is_none() && (!paired || self.confirm) && !self.joining();
            if svc.running {
                ui.horizontal_wrapped(|ui| {
                    ui.label("Name");
                    ui.add(egui::TextEdit::singleline(&mut self.name).desired_width(160.0));
                    if ui.add_enabled(gate && !self.name.trim().is_empty(), egui::Button::new("Join this network on this computer")).clicked() {
                        let a = json!({"state_dir": dir, "name": self.name.trim()});
                        self.confirm = false;
                        self.join(sp, cur.clone(), Op::ServiceLocalJoin, a);
                    }
                });
            }
            ui.add_space(6.0);
            ui.strong("Join a network");
            ui.horizontal_wrapped(|ui| {
                ui.label("Host");
                ui.add(egui::TextEdit::singleline(&mut self.join_host).desired_width(170.0).hint_text("LAN or Tailscale IP"));
                ui.label("Port");
                ui.add(egui::TextEdit::singleline(&mut self.join_port).desired_width(60.0));
                let ok = !self.join_host.trim().is_empty() && self.join_port.trim().parse::<u16>().is_ok_and(|p| p > 0) && !self.name.trim().is_empty();
                if ui.add_enabled(gate && ok, egui::Button::new("Join by address")).clicked() {
                    let (h, p) = (self.join_host.trim().to_owned(), self.join_port.trim().to_owned());
                    let a = json!({"url": url(&h, &p), "name": self.name.trim()});
                    self.confirm = false;
                    self.join(sp, cur.clone(), Op::JoinAddress, a);
                }
            });
            ui.horizontal_wrapped(|ui| {
                ui.label("Invitation file (optional)");
                ui.add(egui::TextEdit::singleline(&mut self.invitation).desired_width(240.0));
                if ui.button("Browse…").clicked() { cmds.push(Cmd::Browse(PickKind::NetInvitationOpen)); }
                if ui.add_enabled(gate && !self.invitation.trim().is_empty() && !self.name.trim().is_empty(), egui::Button::new("Join with invitation")).clicked() {
                    let a = json!({"invitation_path": self.invitation.trim(), "name": self.name.trim()});
                    self.confirm = false;
                    self.join(sp, cur.clone(), Op::Pair, a);
                }
            });
            if paired {
                crate::ui::theme::checkbox(ui, &mut self.confirm, "Replace this computer's current network connection (the old network is no longer kept active here)");
            }
            if let Some(b) = &c.blocker {
                ui.label(RichText::new(format!("Cannot change network now: {b}.")).color(egui::Color32::from_rgb(224, 176, 80)).size(11.5));
            }
            ui.add_space(6.0);
            ui.strong("Invite other PCs");
            ui.horizontal_wrapped(|ui| {
                ui.label("Save invitation to");
                ui.add(egui::TextEdit::singleline(&mut self.invite_path).desired_width(240.0));
                if ui.button("Browse…").clicked() { cmds.push(Cmd::Browse(PickKind::NetInvitationSave)); }
                if ui.add_enabled(c.connected && !self.invite_path.trim().is_empty(), egui::Button::new("Create invitation")).clicked() {
                    cmds.push(Cmd::Invite(self.invite_path.trim().to_owned()));
                }
            });
            if !c.connected {
                ui.label(RichText::new("Connect to a network first to create an invitation.").color(DIM).size(11.5));
            }
            if let Some(n) = c.invite_note {
                ui.label(RichText::new(n).color(DIM).size(11.5));
            }
            if self.busy() { ui.label(RichText::new("Working…").color(DIM)); }
            if let Some(n) = &self.note { ui.label(RichText::new(n).color(DIM).size(12.0)); }
        });
        cmds
    }
}
