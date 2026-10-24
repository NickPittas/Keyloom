//! Network setup: host a per-user network service on this PC and/or join a network by address or invitation, all
//! from the UI. It owns its own lazy bridge link (separate from `remote::Remote`, which only runs while paired and
//! enabled), so setup works unpaired/offline and never touches the client's credentials lock. Nothing runs until the
//! user presses a button (or opens the page, which only reads the service status). A join writes a NEW credentials
//! file under `<settings>/network/` (or reuses the saved one of the same network); the App adds it to the retained
//! connections and never switches or resets an existing session. Secrets never pass through here: the bridge writes
//! credentials and invitations itself.
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
    /// Addresses the network certificate covers (PCs already joined trust it, so hosting must use one of them).
    pub cert_addresses: Vec<String>,
    pub error_code: Option<String>,
    pub message: String,
}

enum Pend {
    Status,
    Service(&'static str),
    /// `name` is the PC name captured when the join was queued.
    Join { creds: PathBuf, name: String },
}

/// A finished join waiting for the App to switch to it.
#[derive(Debug)]
pub struct Joined {
    pub creds: String,
    /// The bridge reused the existing connection (same network): nothing new was registered.
    pub reused: bool,
    pub network_id: Option<String>,
    pub name: String,
}

/// One saved connection, as the App sees it.
#[derive(Clone)]
pub struct ConnectionInfo {
    pub credentials: String,
    pub label: String,
    pub network_id: Option<String>,
    pub online: bool,
}

/// What the UI needs from the App each frame.
pub struct Ctx<'a> {
    pub settings: &'a Settings,
    pub settings_path: Option<&'a Path>,
    pub connected: bool,
    pub connections: &'a [ConnectionInfo],
    pub invite_note: Option<&'a str>,
}

/// Work for the App (it owns the picker and the connected client).
pub enum Cmd {
    /// Save the global PC name (the App pushes it to every joined host).
    SaveName(String),
    /// Write an invitation for the network of the connection with these credentials.
    Invite { connection: String, path: String },
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
    name_loaded: bool,
    host_loaded: bool,
    invitation: String,
    invite_path: String,
    invite_conn: String,
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
            name_loaded: false,
            host_loaded: false,
            invitation: String::new(),
            invite_path: String::new(),
            invite_conn: String::new(),
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
                    cert_addresses: v.get("certificate_addresses").and_then(Value::as_array).map_or_else(Vec::new, |a| a.iter().filter_map(Value::as_str).map(str::to_owned).collect()),
                    error_code: v.get("error_code").and_then(Value::as_str).map(str::to_owned),
                    message: v.get("message").and_then(Value::as_str).unwrap_or("").to_owned(),
                };
                // Prefill the existing host configuration once; later refreshes never overwrite what the user is editing.
                if !self.host_loaded && svc.installed {
                    (self.address, self.port, self.allow_join, self.host_loaded) = (svc.address.clone(), svc.port.to_string(), svc.allow_join, true);
                } else if !self.host_loaded && self.address.is_empty() {
                    if let Some(a) = svc.cert_addresses.first() {
                        self.address.clone_from(a);
                    }
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
            (Pend::Join { creds, name }, Ok(v)) => {
                nlog("setup join ok");
                let reused = v.get("reused").and_then(Value::as_bool).unwrap_or(false);
                let path = v.get("credentials_path").and_then(Value::as_str).map_or_else(|| creds.display().to_string(), str::to_owned);
                self.note = Some(if reused { "This computer was already on that network; keeping its connection.".into() } else { "Joined the network. Your other networks stay connected.".into() });
                let network_id = v.get("network_id").and_then(Value::as_str).map(str::to_owned);
                self.joined = Some(Joined { creds: path, reused, network_id, name });
            }
            (Pend::Join { .. }, Err(e)) => self.note = Some(format!("Join failed; existing connections are unchanged ({})", err_text(&e))),
        }
    }

    /// Start a join. A fresh credentials file is offered; `existing` (every saved connection) lets the bridge reuse the
    /// one of the same network instead of registering this PC again.
    fn join(&mut self, sp: Option<&Path>, existing: &[ConnectionInfo], op: Op, mut args: Value) {
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
        // Candidates: every saved connection plus any credentials file left in the folder (a removed network keeps its
        // files), so rejoining reuses that identity instead of registering a second one.
        let mut cands: Vec<String> = existing.iter().map(|c| c.credentials.clone()).collect();
        match std::fs::read_dir(&dir) {
            Ok(rd) => {
                let mut found: Vec<String> = rd.flatten().filter(|e| e.file_type().is_ok_and(|t| t.is_file())).filter_map(|e| {
                    let n = e.file_name().to_string_lossy().into_owned();
                    (n.starts_with("credentials") && n.ends_with(".json") && !n.trim_end_matches(".json").contains(".json")).then(|| e.path().display().to_string())
                }).collect();
                found.sort();
                cands.extend(found);
            }
            Err(e) => {
                self.note = Some(format!("Join not started: the credentials folder could not be listed ({e}), so a saved identity for this network could not be looked for."));
                return;
            }
        }
        let mut seen = std::collections::HashSet::new();
        cands.retain(|c| !c.trim().is_empty() && seen.insert(c.clone()));
        if cands.len() > 64 {
            self.note = Some(format!("Join not started: {} saved identities exceed the supported limit of 64. Existing connections and identity files have been kept.", cands.len()));
            return;
        }
        if !cands.is_empty() {
            args["existing_credentials_paths"] = json!(cands);
        }
        let name = args["name"].as_str().unwrap_or("").to_owned();
        self.note = Some("Joining…".into());
        self.push(op, args, Pend::Join { creds, name });
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
        if !self.name_loaded {
            self.name_loaded = true;
            if !c.settings.network.name.trim().is_empty() {
                self.name.clone_from(&c.settings.network.name);
            }
        }
        let saved: Vec<ConnectionInfo> = c.connections.iter().filter(|k| !k.credentials.is_empty()).cloned().collect();
        let idle = !self.busy();
        let port_ok = self.port.trim().parse::<u16>().is_ok_and(|p| p > 0);
        let svc = self.svc.clone().unwrap_or_default();
        let name_ok = !self.name.trim().is_empty() && self.name.trim().chars().count() <= 64 && !self.name.chars().any(char::is_control);
        let online: Vec<&ConnectionInfo> = c.connections.iter().filter(|k| k.online).collect();
        if !online.iter().any(|k| k.credentials == self.invite_conn) {
            self.invite_conn = online.first().map(|k| k.credentials.clone()).unwrap_or_default();
        }
        ui.add_space(8.0);
        egui::CollapsingHeader::new("Network setup").default_open(saved.is_empty()).show(ui, |ui| {
            ui.label(RichText::new("Hosting a network on this PC and joining other networks are independent: one PC can host its own network and stay connected to several others at once. No command line is needed.").color(DIM).size(11.5));
            ui.add_space(4.0);
            ui.horizontal_wrapped(|ui| {
                ui.label("This computer’s name");
                ui.add(egui::TextEdit::singleline(&mut self.name).desired_width(180.0).char_limit(64));
                if ui.add_enabled(name_ok && self.name.trim() != c.settings.network.name, egui::Button::new("Save")).clicked() {
                    cmds.push(Cmd::SaveName(self.name.trim().to_owned()));
                }
            });
            ui.label(RichText::new("The name is shown to every host this PC has joined; saving it updates all of them.").color(DIM).size(11.0));
            ui.add_space(6.0);
            ui.strong("Host on THIS PC");
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
            let covered = svc.cert_addresses.iter().any(|a| a.eq_ignore_ascii_case(self.address.trim()));
            let editable = !svc.running;
            ui.horizontal_wrapped(|ui| {
                ui.label("Address");
                ui.add_enabled(editable, egui::TextEdit::singleline(&mut self.address).desired_width(170.0).hint_text("LAN or Tailscale IP"));
                ui.label("Port");
                ui.add_enabled(editable, egui::TextEdit::singleline(&mut self.port).desired_width(60.0));
                ui.add_enabled_ui(editable, |ui| crate::ui::theme::checkbox(ui, &mut self.allow_join, "Trusted network: allow joining by address"));
            });
            if svc.error_code.as_deref() == Some("cert_mismatch") || (editable && !svc.cert_addresses.is_empty() && !covered && !self.address.trim().is_empty()) {
                ui.label(RichText::new("This PC’s network certificate does not cover that address. PCs that already joined trust the certificate, so it is kept; host on one of the addresses it covers:").color(egui::Color32::from_rgb(224, 176, 80)).size(11.5));
                ui.horizontal_wrapped(|ui| {
                    for a in &svc.cert_addresses {
                        if ui.add_enabled(editable, egui::Button::new(format!("Use address {a}"))).clicked() {
                            self.address.clone_from(a);
                        }
                    }
                });
            }
            ui.horizontal_wrapped(|ui| {
                let changed = svc.installed && (self.address.trim() != svc.address || self.port.trim() != svc.port.to_string() || self.allow_join != svc.allow_join);
                if (!svc.installed || (editable && changed)) && ui.add_enabled(idle && port_ok && !self.address.trim().is_empty(), egui::Button::new(if svc.installed { "Apply and start" } else { "Install and start" })).clicked() {
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
            if svc.running {
                ui.horizontal_wrapped(|ui| {
                    if ui.add_enabled(name_ok && !self.joining(), egui::Button::new("Also join this PC’s own network from this PC")).clicked() {
                        let a = json!({"state_dir": dir, "name": self.name.trim()});
                        self.join(sp, &saved, Op::ServiceLocalJoin, a);
                    }
                });
            }
            ui.label(RichText::new("To use another PC’s network, use “Add network” below; hosting here does not change your connections.").color(DIM).size(11.0));
            ui.add_space(6.0);
            ui.strong("Add network");
            ui.horizontal_wrapped(|ui| {
                ui.label("Host");
                ui.add(egui::TextEdit::singleline(&mut self.join_host).desired_width(170.0).hint_text("LAN or Tailscale IP"));
                ui.label("Port");
                ui.add(egui::TextEdit::singleline(&mut self.join_port).desired_width(60.0));
                let ok = !self.join_host.trim().is_empty() && self.join_port.trim().parse::<u16>().is_ok_and(|p| p > 0) && name_ok;
                if ui.add_enabled(!self.joining() && ok, egui::Button::new("Join by address")).clicked() {
                    let (h, p) = (self.join_host.trim().to_owned(), self.join_port.trim().to_owned());
                    let a = json!({"url": url(&h, &p), "name": self.name.trim()});
                    self.join(sp, &saved, Op::JoinAddress, a);
                }
            });
            ui.horizontal_wrapped(|ui| {
                ui.label("Invitation file (optional)");
                ui.add(egui::TextEdit::singleline(&mut self.invitation).desired_width(240.0));
                if ui.button("Browse…").clicked() { cmds.push(Cmd::Browse(PickKind::NetInvitationOpen)); }
                if ui.add_enabled(!self.joining() && !self.invitation.trim().is_empty() && name_ok, egui::Button::new("Join with invitation")).clicked() {
                    let a = json!({"invitation_path": self.invitation.trim(), "name": self.name.trim()});
                    self.join(sp, &saved, Op::Pair, a);
                }
            });
            ui.label(RichText::new("Joining adds a network; every network you already joined stays connected.").color(DIM).size(11.0));
            ui.add_space(6.0);
            ui.strong("Invite other PCs");
            ui.horizontal_wrapped(|ui| {
                ui.label("Network");
                let sel = online.iter().find(|k| k.credentials == self.invite_conn).map_or("none connected".to_owned(), |k| k.label.clone());
                egui::ComboBox::from_id_salt("invite_conn").selected_text(sel).show_ui(ui, |ui| {
                    for k in &online {
                        ui.selectable_value(&mut self.invite_conn, k.credentials.clone(), &k.label);
                    }
                });
            });
            ui.horizontal_wrapped(|ui| {
                ui.label("Save invitation to");
                ui.add(egui::TextEdit::singleline(&mut self.invite_path).desired_width(240.0));
                if ui.button("Browse…").clicked() { cmds.push(Cmd::Browse(PickKind::NetInvitationSave)); }
                if ui.add_enabled(!self.invite_conn.is_empty() && !self.invite_path.trim().is_empty(), egui::Button::new("Create invitation")).clicked() {
                    cmds.push(Cmd::Invite { connection: self.invite_conn.clone(), path: self.invite_path.trim().to_owned() });
                }
            });
            if online.is_empty() {
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
