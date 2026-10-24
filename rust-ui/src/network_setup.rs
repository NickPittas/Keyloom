//! Network setup: this PC hosts its own per-user network service automatically (installed once at startup unless
//! the user removed it) and the Clients page lists the other Keyloom PCs found on every LAN / Tailscale / ZeroTier
//! interface. It owns its own lazy bridge link (separate from `remote::Remote`, which only runs while a connection
//! is enabled), so setup works unpaired/offline and never touches a client's credentials lock. Nothing scans in the
//! background: a scan runs only when Clients is first opened or when ReScan is pressed. A join writes a NEW
//! credentials file under `<settings>/network/` (or reuses the saved one of the same network); the App adds it to
//! the retained connections and never switches or resets an existing session. Secrets never pass through here: the
//! bridge writes credentials and invitations itself.
//!
//! Every bridge request has a deadline (service operations 45 s, scans 35 s). When it passes, the bridge process is
//! dropped (killed and reaped) and the UI says the state is unknown until Refresh; a request is never retried.
use crate::netlink::{Link, Op, Source, State};
use crate::remote::{err_text, nlog};
use crate::settings::{Connection, Settings};
use crate::ui::picker::PickKind;
use eframe::egui::{self, Color32, RichText, Ui};
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

const DIM: Color32 = Color32::from_rgb(169, 187, 175);
const WARN: Color32 = Color32::from_rgb(224, 176, 80);
const GOOD: Color32 = Color32::from_rgb(150, 190, 150);
const IDLE_DROP: Duration = Duration::from_secs(30);
const SERVICE_DEADLINE: Duration = Duration::from_secs(45);
const SCAN_DEADLINE: Duration = Duration::from_secs(35);
const MAX_ENDPOINTS: usize = 16;
const MAX_KNOWN: usize = 64;

#[derive(Clone, Debug, Default)]
pub struct Svc {
    pub installed: bool,
    pub running: bool,
    pub port: u64,
    pub allow_join: bool,
    /// The saved join policy (`None` = no service config yet).
    pub policy: Option<bool>,
    /// The user removed the host (tombstone): startup must not bring it back.
    pub removed: bool,
    pub network_id: Option<String>,
    /// `(url, kind)` of every address the service answers on.
    pub endpoints: Vec<(String, String)>,
    pub discovery: bool,
    pub host_installation_id: Option<String>,
    pub receiver_online: bool,
    pub name: String,
    pub error_code: Option<String>,
    pub message: String,
}

#[derive(Clone, Debug)]
pub struct Ep {
    pub url: String,
    pub kind: String,
    pub reachable: bool,
    pub rtt_ms: Option<f64>,
    pub error: Option<String>,
}

/// One PC found by a scan. Joining trusts the full certificate fingerprint, never the name:
/// two PCs may share a name.
#[derive(Clone, Debug)]
pub struct Pc {
    pub network_id: String,
    pub fingerprint_sha256: String,
    pub name: String,
    pub host_iid: Option<String>,
    pub join: bool,
    pub receiver_online: bool,
    pub is_self: bool,
    pub endpoints: Vec<Ep>,
}

impl Pc {
    fn join_args(&self, url: &str, name: &str) -> Value {
        json!({"url": url, "name": name, "fingerprint_sha256": self.fingerprint_sha256, "endpoints": self.urls()})
    }

    fn invitation_route(&self) -> Value {
        json!({"fingerprint_sha256": self.fingerprint_sha256, "endpoints": self.urls()})
    }

    /// Reachable addresses first, then the rest (<= 16, https only).
    fn urls(&self) -> Vec<String> {
        let mut e: Vec<&Ep> = self.endpoints.iter().filter(|e| e.url.starts_with("https://")).collect();
        e.sort_by_key(|e| !e.reachable);
        let mut seen = HashSet::new();
        e.into_iter().filter(|e| seen.insert(e.url.clone())).map(|e| e.url.clone()).take(MAX_ENDPOINTS).collect()
    }
}

#[derive(Clone, Debug, Default)]
pub struct Scan {
    pub elapsed_ms: u64,
    pub partial: bool,
    pub interfaces: u64,
    pub tailscale: String,
    pub neighbors: String,
    pub pcs: Vec<Pc>,
}

/// Fresh addresses for a saved connection, to be written into the settings by the App.
#[derive(Clone, Debug)]
pub struct ScanUpdate {
    pub network_id: String,
    pub host_iid: Option<String>,
    pub name: String,
    pub endpoints: Vec<String>,
}

#[derive(Clone, Debug, Default)]
struct JoinMeta {
    creds: PathBuf,
    name: String,
    host_name: String,
    local: bool,
    iid: Option<String>,
    eps: Vec<String>,
}

enum Pend {
    Status { ensure: bool },
    Service(&'static str),
    Join(JoinMeta),
}

struct Item {
    op: Op,
    args: Value,
    p: Pend,
    at: Instant,
}

/// The startup "make sure this PC hosts" sequence.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Ensure {
    /// Not started yet (also: waiting for Start after the user removed the host).
    Idle,
    Busy,
    Done,
}

/// A finished join waiting for the App to switch to it.
#[derive(Debug)]
pub struct Joined {
    pub creds: String,
    /// The bridge reused the existing connection (same network): nothing new was registered.
    pub reused: bool,
    pub network_id: Option<String>,
    pub name: String,
    pub endpoints: Vec<String>,
    pub host_installation_id: Option<String>,
    /// This PC's own service (joined automatically).
    pub local_host: bool,
    /// The host's PC name as discovery showed it (empty = unknown).
    pub host_name: String,
}

/// One saved connection, as the App sees it.
#[derive(Clone)]
pub struct ConnectionInfo {
    pub credentials: String,
    pub label: String,
    pub network_id: Option<String>,
    pub online: bool,
    /// Coordinator address from the credentials file (known to discovery).
    pub url: Option<String>,
}

/// What the UI needs from the App each frame.
pub struct Ctx<'a> {
    pub settings: &'a Settings,
    pub settings_path: Option<&'a Path>,
    pub connected: bool,
    pub connections: &'a [ConnectionInfo],
    pub invite_note: Option<&'a str>,
}

/// Work for the App (it owns the picker, the settings and the connected clients).
pub enum Cmd {
    /// Save the global PC name (the App pushes it to every joined host).
    SaveName(String),
    /// Write an invitation for the network of the connection with these credentials.
    Invite { connection: String, path: String },
    Browse(PickKind),
    /// The row's checkbox changed on a saved connection (`endpoints` empty = keep the saved ones).
    UsePc { credentials: String, network_id: String, host_iid: Option<String>, name: String, endpoints: Vec<String>, on: bool },
    /// Persist `network.host_removed`.
    HostRemoved(bool),
    /// Persist `network.use_invitation`.
    UseInvitation(bool),
}

pub struct Setup {
    source: Source,
    link: Option<Link>,
    pend: HashMap<String, (Pend, Instant)>,
    queue: VecDeque<Item>,
    idle_since: Option<Instant>,
    seen: Option<Instant>,
    svc: Option<Svc>,
    svc_unknown: bool,
    note: Option<String>,
    joined: Option<Joined>,
    name: String,
    name_loaded: bool,
    invitation: String,
    invite_path: String,
    remove_armed: bool,
    dir: String,
    net_dir: Option<PathBuf>,
    ensure: Ensure,
    installed_once: bool,
    // Mirrors of the settings, refreshed every frame by `ensure`.
    host_removed: bool,
    use_inv: bool,
    reg_name: String,
    existing: Vec<String>,
    known_local: Vec<String>,
    known_urls: Vec<String>,
    scanned: bool,
    scan: Option<Scan>,
    scan_started: Option<Instant>,
    /// The scan has its own bridge so service work never waits for it (and cancelling it touches nothing else).
    scan_link: Option<Link>,
    scan_item: Option<Value>,
    scan_id: Option<String>,
    /// Name / join policy the user changed but the service has not been told yet (coalesced, flushed when idle).
    pend_name: Option<String>,
    pend_allow: Option<bool>,
    inv_dirty: bool,
    policy_synced: bool,
    out: Vec<Cmd>,
    updates: Vec<ScanUpdate>,
}

fn state_dir(sp: Option<&Path>) -> Option<PathBuf> {
    sp.and_then(Path::parent).map(|d| d.join("network").join("server"))
}

/// `host` or `[v6]` of an https URL (no port).
fn host_of(url: &str) -> String {
    let r = url.strip_prefix("https://").unwrap_or(url);
    let r = r.split('/').next().unwrap_or(r);
    if r.starts_with('[') {
        return r.find(']').map_or(r, |i| &r[..=i]).to_owned();
    }
    r.rsplit_once(':').map_or(r, |x| x.0).to_owned()
}

fn short(url: &str) -> String {
    url.trim_start_matches("https://").trim_end_matches('/').to_owned()
}

fn endpoint_urls(v: Option<&Value>) -> Vec<String> {
    let mut seen = HashSet::new();
    v.and_then(Value::as_array).into_iter().flatten()
        .filter_map(|e| e.as_str().or_else(|| e.get("url").and_then(Value::as_str)))
        .filter(|u| u.starts_with("https://") && seen.insert(u.to_string()))
        .map(str::to_owned).take(MAX_ENDPOINTS).collect()
}

fn text(v: &Value, k: &str) -> Option<String> {
    v.get(k).and_then(Value::as_str).filter(|s| !s.is_empty()).map(str::to_owned)
}

fn parse_svc(v: &Value) -> Svc {
    let b = |k: &str| v.get(k).and_then(Value::as_bool).unwrap_or(false);
    Svc {
        installed: b("installed"),
        running: b("running"),
        port: v.get("port").and_then(Value::as_u64).unwrap_or(0),
        allow_join: b("allow_address_join"),
        policy: v.get("allow_address_join").and_then(Value::as_bool),
        removed: b("removed"),
        network_id: text(v, "network_id").or_else(|| text(v, "fingerprint_sha256")),
        endpoints: v.get("endpoints").and_then(Value::as_array).into_iter().flatten()
            .filter_map(|e| Some((e.get("url")?.as_str()?.to_owned(), e.get("kind").and_then(Value::as_str).unwrap_or("").to_owned()))).take(MAX_ENDPOINTS).collect(),
        discovery: b("discovery"),
        host_installation_id: text(v, "host_installation_id"),
        receiver_online: b("receiver_online"),
        name: v.get("name").and_then(Value::as_str).unwrap_or("").to_owned(),
        error_code: text(v, "error_code"),
        message: v.get("message").and_then(Value::as_str).unwrap_or("").to_owned(),
    }
}

fn parse_scan(v: &Value) -> Scan {
    let src = v.get("sources").cloned().unwrap_or(Value::Null);
    let pcs = v.get("pcs").and_then(Value::as_array).into_iter().flatten().filter_map(|p| {
        let fingerprint_sha256 = text(p, "fingerprint_sha256")?.to_ascii_lowercase();
        if fingerprint_sha256.len() != 64 || !fingerprint_sha256.bytes().all(|c| c.is_ascii_hexdigit()) {
            return None;
        }
        let network_id = text(p, "network_id").unwrap_or_else(|| fingerprint_sha256[..16].to_owned());
        let endpoints = p.get("endpoints").and_then(Value::as_array).into_iter().flatten().filter_map(|e| Some(Ep {
            url: e.get("url")?.as_str()?.to_owned(),
            kind: e.get("kind").and_then(Value::as_str).unwrap_or("").to_owned(),
            reachable: e.get("reachable").and_then(Value::as_bool).unwrap_or(false),
            rtt_ms: e.get("rtt_ms").and_then(Value::as_f64),
            error: text(e, "error"),
        })).collect();
        Some(Pc {
            network_id,
            fingerprint_sha256,
            name: p.get("name").and_then(Value::as_str).unwrap_or("").chars().filter(|c| !c.is_control()).take(64).collect(),
            host_iid: text(p, "host_installation_id"),
            join: p.get("join").and_then(Value::as_bool).unwrap_or(false),
            receiver_online: p.get("receiver_online").and_then(Value::as_bool).unwrap_or(false),
            is_self: p.get("self").and_then(Value::as_bool).unwrap_or(false),
            endpoints,
        })
    }).collect();
    let s = |k: &str| src.get(k).and_then(Value::as_str).unwrap_or("").to_owned();
    Scan { elapsed_ms: v.get("elapsed_ms").and_then(Value::as_u64).unwrap_or(0), partial: v.get("partial").and_then(Value::as_bool).unwrap_or(false), interfaces: src.get("broadcast_ifaces").and_then(Value::as_u64).unwrap_or(0), tailscale: s("tailscale"), neighbors: s("neighbors"), pcs }
}

/// The network a saved connection belongs to: its own record, or the one the live connection learned (older entries).
fn conn_nid(k: &Connection, infos: &[ConnectionInfo]) -> String {
    if !k.network_id.is_empty() {
        return k.network_id.clone();
    }
    infos.iter().find(|i| i.credentials == k.credentials).and_then(|i| i.network_id.clone()).unwrap_or_default()
}

/// Match the saved network ID and host installation; joining separately verifies the full certificate pin.
/// Names are never compared.
fn find_conn<'a>(conns: &'a [Connection], infos: &[ConnectionInfo], nid: &str, iid: Option<&str>) -> Option<&'a Connection> {
    conns.iter().filter(|k| !k.local_host).find(|k| {
        conn_nid(k, infos).eq_ignore_ascii_case(nid) && (k.host_installation_id.is_empty() || iid.is_none_or(|i| i.is_empty() || i == k.host_installation_id))
    })
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
            svc_unknown: false,
            note: None,
            joined: None,
            name: crate::remote::default_name(),
            name_loaded: false,
            invitation: String::new(),
            invite_path: String::new(),
            remove_armed: false,
            dir: String::new(),
            net_dir: None,
            ensure: Ensure::Idle,
            installed_once: false,
            host_removed: false,
            use_inv: false,
            reg_name: crate::remote::default_name(),
            existing: vec![],
            known_local: vec![],
            known_urls: vec![],
            scanned: false,
            scan: None,
            scan_started: None,
            scan_link: None,
            scan_item: None,
            scan_id: None,
            pend_name: None,
            pend_allow: None,
            inv_dirty: false,
            policy_synced: false,
            out: vec![],
            updates: vec![],
        }
    }

    pub fn take_joined(&mut self) -> Option<Joined> {
        self.joined.take()
    }
    /// Fresh addresses from the last scan (once).
    pub fn take_updates(&mut self) -> Vec<ScanUpdate> {
        std::mem::take(&mut self.updates)
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
        self.pend.values().any(|p| matches!(p.0, Pend::Join(_))) || self.queue.iter().any(|q| matches!(q.p, Pend::Join(_)))
    }
    fn scanning(&self) -> bool {
        self.scan_started.is_some()
    }
    fn push(&mut self, op: Op, mut args: Value, p: Pend) {
        if matches!(op, Op::ServiceStatus | Op::ServiceInstall | Op::ServiceStart | Op::ServiceStop | Op::ServiceRemove | Op::ServiceConfigure | Op::ServiceLocalJoin) {
            args["state_dir"] = json!(self.dir);
        }
        self.queue.push_back(Item { op, args, p, at: Instant::now() });
    }

    fn set_dir(&mut self, sp: Option<&Path>) {
        if let Some(d) = state_dir(sp) {
            self.dir = d.display().to_string();
        }
        self.net_dir = sp.and_then(Path::parent).map(|d| d.join("network"));
        if let Some(d) = &self.net_dir {
            crate::remote::set_log_file(d.join("service.log"));
        }
    }

    /// Every frame (also while Clients is closed): mirror the settings, and once per run make sure this PC hosts its
    /// own network: install the service unless the user removed it, then join it from this PC. Stop is never undone
    /// here; the service comes back only through Start or the OS sign-in.
    pub fn ensure(&mut self, s: &Settings, sp: Option<&Path>) {
        self.set_dir(sp);
        let conns = s.network.effective_connections(sp);
        self.existing = conns.iter().map(|c| c.credentials.clone()).collect();
        self.known_local = conns.iter().filter(|c| c.local_host).map(|c| c.network_id.clone()).collect();
        self.host_removed = s.network.host_removed;
        self.use_inv = s.network.use_invitation;
        let n: String = s.network.name.trim().chars().filter(|c| !c.is_control()).take(64).collect();
        self.reg_name = if n.is_empty() { crate::remote::default_name().chars().filter(|c| !c.is_control()).take(64).collect() } else { n };
        self.known_urls = conns.iter().flat_map(|c| c.endpoints.iter().cloned()).collect();
        if self.ensure != Ensure::Idle || self.host_removed || sp.is_none() {
            return;
        }
        if self.source == Source::Off {
            self.ensure = Ensure::Done;
            return;
        }
        self.ensure = Ensure::Busy;
        self.push(Op::ServiceStatus, json!({}), Pend::Status { ensure: true });
    }

    fn ensure_step(&mut self) {
        let svc = self.svc.clone().unwrap_or_default();
        if svc.removed {
            self.ensure = Ensure::Done;
            return; // the tombstone outlives lost settings; only Start undoes it
        }
        if !svc.installed {
            if self.installed_once {
                self.ensure = Ensure::Done;
                self.note = Some("This PC's host service could not be installed; press Start to try again.".into());
                nlog("host service could not be installed");
                return;
            }
            self.installed_once = true;
            self.install("Install");
            return;
        }
        self.ensure = Ensure::Done;
        let known = svc.network_id.as_ref().is_none_or(|n| self.known_local.iter().any(|k| k.eq_ignore_ascii_case(n)));
        if svc.running && !known && !self.joining() {
            let meta = JoinMeta { name: self.reg_name.clone(), host_name: svc.name.clone(), local: true, iid: svc.host_installation_id.clone(), ..Default::default() };
            self.begin_join(Op::ServiceLocalJoin, json!({"name": self.reg_name}), meta);
        }
    }

    fn install(&mut self, what: &'static str) {
        self.push(Op::ServiceInstall, json!({"allow_address_join": self.svc.as_ref().and_then(|s| s.policy).unwrap_or(!self.use_inv), "name": self.reg_name}), Pend::Service(what));
    }

    /// Per frame: start the link when something is queued, send, collect replies, enforce deadlines, drop the link when idle.
    pub fn tick(&mut self, repo: Option<&Path>, python: Option<&Path>, wake: &Arc<dyn Fn() + Send + Sync>) {
        self.tick_scan(repo, python, wake);
        self.flush_cfg();
        let now = Instant::now();
        let late = |t: Instant| now.saturating_duration_since(t) > SERVICE_DEADLINE;
        if self.pend.values().any(|(_, t)| late(*t)) || self.queue.iter().any(|q| late(q.at)) {
            self.drop_link("The request did not finish in time. The service state is unknown: press Refresh before trying again.", true);
            return;
        }
        if self.link.is_none() && !self.queue.is_empty() {
            match self.source.command(repo, python) {
                Ok(Some(cmd)) => match Link::start(cmd, wake.clone()) {
                    Ok(l) => self.link = Some(l),
                    Err(e) => self.drop_link(&format!("Network setup is unavailable: {e}"), false),
                },
                Ok(None) => self.drop_link("Network setup is unavailable: network is not available in this build", false),
                Err(e) => self.drop_link(&format!("Network setup is unavailable: {e}"), false),
            }
        }
        let Some(l) = &mut self.link else { return };
        if l.state() == State::Ready {
            while let Some(it) = self.queue.pop_front() {
                match l.request(it.op, it.args) {
                    Ok(id) => {
                        self.pend.insert(id, (it.p, it.at));
                    }
                    Err(e) => {
                        let why = format!("Network link failed ({})", err_text(&e));
                        self.drop_link(&why, true);
                        return;
                    }
                }
            }
        }
        let Some(l) = &mut self.link else { return };
        let replies = l.poll();
        let ended = matches!(l.state(), State::Ended(_));
        for r in replies {
            if let Some((p, _)) = r.id.and_then(|id| self.pend.remove(&id)) {
                self.on_reply(p, r.outcome);
            }
        }
        if ended {
            self.drop_link("The setup bridge stopped before confirming the operation; refresh the service status before trying again.", true);
        } else if self.busy() {
            self.idle_since = None;
        } else if self.idle_since.get_or_insert_with(Instant::now).elapsed() > IDLE_DROP {
            self.link = None; // Drop kills and reaps the bridge
            self.idle_since = None;
        }
    }

    /// The scan's own link: start it on demand, send the one request, collect the answer, enforce the deadline.
    fn tick_scan(&mut self, repo: Option<&Path>, python: Option<&Path>, wake: &Arc<dyn Fn() + Send + Sync>) {
        let Some(t0) = self.scan_started else { return };
        if t0.elapsed() > SCAN_DEADLINE {
            self.end_scan("The scan did not finish in time and was stopped.");
            return;
        }
        if self.scan_link.is_none() {
            match self.source.command(repo, python) {
                Ok(Some(cmd)) => match Link::start(cmd, wake.clone()) {
                    Ok(l) => self.scan_link = Some(l),
                    Err(e) => return self.end_scan(&format!("Network setup is unavailable: {e}")),
                },
                Ok(None) => return self.end_scan("Network setup is unavailable: network is not available in this build"),
                Err(e) => return self.end_scan(&format!("Network setup is unavailable: {e}")),
            }
        }
        let Some(l) = &mut self.scan_link else { return };
        if l.state() == State::Ready {
            if let Some(args) = self.scan_item.take() {
                match l.request(Op::Discover, args) {
                    Ok(id) => self.scan_id = Some(id),
                    Err(e) => return self.end_scan(&format!("Network link failed ({})", err_text(&e))),
                }
            }
        }
        let replies = l.poll();
        let ended = matches!(l.state(), State::Ended(_));
        for r in replies {
            if r.id.is_some() && r.id == self.scan_id {
                self.scan_done(r.outcome);
                return;
            }
        }
        if ended {
            self.end_scan("The scan bridge stopped before finishing.");
        }
    }

    /// Stop the scan only: its bridge is killed, service and join work is untouched.
    fn end_scan(&mut self, why: &str) {
        self.scan_link = None;
        self.scan_item = None;
        self.scan_id = None;
        self.scan_started = None;
        self.note = Some(why.to_owned());
    }

    fn scan_done(&mut self, out: Result<Value, crate::netlink::LinkError>) {
        self.scan_link = None;
        self.scan_id = None;
        self.scan_started = None;
        match out {
            Ok(v) => {
                let scan = parse_scan(&v);
                self.updates = scan.pcs.iter().filter(|p| !p.is_self).map(|p| ScanUpdate { network_id: p.network_id.clone(), host_iid: p.host_iid.clone(), name: p.name.clone(), endpoints: p.urls() }).collect();
                self.scan = Some(scan);
            }
            Err(e) => {
                nlog(&format!("scan failed: {} ({})", e.code, err_text(&e)));
                self.note = Some(format!("Scan failed ({})", err_text(&e)));
            }
        }
    }

    /// Tell the running service about a changed name / join policy once its link is free. Several edits collapse into
    /// one request; nothing is lost when a status poll or another operation was in flight at the click.
    fn flush_cfg(&mut self) {
        if self.pend_name.is_none() && self.pend_allow.is_none() {
            return;
        }
        let Some(svc) = &self.svc else { return };
        if !svc.installed {
            self.pend_name = None; // the settings are saved and a later install reads them
            self.pend_allow = None;
            self.inv_dirty = false;
            return;
        }
        if self.busy() {
            return;
        }
        let allow = self.pend_allow.take().unwrap_or(svc.allow_join);
        let mut args = json!({"allow_address_join": allow});
        if let Some(n) = self.pend_name.take() {
            args["name"] = json!(n);
        }
        self.push(Op::ServiceConfigure, args, Pend::Service("Settings"));
    }

    /// Cut the bridge (Drop kills and reaps it) and forget everything in flight. A request that was already sent MAY
    /// have been processed, so `unknown` makes the UI ask for a Refresh.
    fn drop_link(&mut self, why: &str, unknown: bool) {
        self.link = None;
        self.queue.clear();
        self.pend.clear();
        if self.ensure == Ensure::Busy {
            self.ensure = Ensure::Done;
        }
        self.svc_unknown |= unknown;
        self.note = Some(why.to_owned());
    }

    fn cancel_scan(&mut self) {
        self.end_scan("Scan cancelled.");
    }

    fn rescan(&mut self) {
        if self.scanning() {
            return;
        }
        let mut seen = HashSet::new();
        let known: Vec<String> = self.known_urls.iter().filter(|u| u.starts_with("https://") && seen.insert(u.to_string())).take(MAX_KNOWN).cloned().collect();
        self.scan_started = Some(Instant::now());
        self.scan_item = Some(json!({"timeout_s": 20, "known": known}));
        self.note = None;
    }

    fn on_reply(&mut self, p: Pend, out: Result<Value, crate::netlink::LinkError>) {
        match (p, out) {
            (Pend::Status { ensure }, Ok(v)) => {
                let svc = parse_svc(&v);
                // The service is the truth for the join policy: adopt it once, unless the user just changed it.
                if !self.policy_synced && !self.inv_dirty {
                    self.policy_synced = true;
                    if let Some(a) = svc.policy.filter(|a| *a == self.use_inv) {
                        self.out.push(Cmd::UseInvitation(!a));
                    }
                }
                self.svc = Some(svc);
                self.svc_unknown = false;
                if ensure {
                    self.ensure_step();
                }
            }
            (Pend::Status { ensure }, Err(e)) => {
                nlog(&format!("status failed: {} ({})", e.code, err_text(&e)));
                self.note = Some(format!("Could not read the service status ({})", err_text(&e)));
                if ensure {
                    self.ensure = Ensure::Done;
                }
            }
            (Pend::Service(what), Ok(_)) => {
                nlog(&format!("setup {what} ok"));
                if what == "Settings" {
                    self.inv_dirty = self.pend_allow.is_some();
                }
                self.note = Some(format!("{what}: done."));
                match what {
                    "Install" => self.push(Op::ServiceStatus, json!({}), Pend::Status { ensure: true }),
                    _ => {
                        if what == "Start" {
                            self.ensure = Ensure::Idle; // the next frame joins this PC's own network if that is missing
                            self.installed_once = false;
                        }
                        self.push(Op::ServiceStatus, json!({}), Pend::Status { ensure: false });
                    }
                }
            }
            (Pend::Service(what), Err(e)) => {
                nlog(&format!("{what} failed: {} ({})", e.code, err_text(&e)));
                self.note = Some(format!("{what} failed ({})", err_text(&e)));
                if what == "Settings" {
                    self.inv_dirty = self.pend_allow.is_some();
                }
                if what == "Install" {
                    self.ensure = Ensure::Done;
                }
                self.push(Op::ServiceStatus, json!({}), Pend::Status { ensure: false });
            }
            (Pend::Join(m), Ok(v)) => {
                nlog("setup join ok");
                let reused = v.get("reused").and_then(Value::as_bool).unwrap_or(false);
                let path = v.get("credentials_path").and_then(Value::as_str).map_or_else(|| m.creds.display().to_string(), str::to_owned);
                self.note = Some(if reused { "This computer was already on that network; keeping its connection.".into() } else if m.local { "This PC now uses its own network.".into() } else { "Joined. Your other connections stay connected.".into() });
                let mut endpoints = endpoint_urls(v.get("endpoints"));
                if endpoints.is_empty() {
                    endpoints = m.eps.clone();
                }
                self.joined = Some(Joined {
                    creds: path,
                    reused,
                    network_id: text(&v, "network_id"),
                    name: m.name,
                    endpoints,
                    host_installation_id: text(&v, "host_installation_id").or(m.iid),
                    local_host: m.local,
                    host_name: m.host_name,
                });
            }
            (Pend::Join(_), Err(e)) => {
                let why = if e.code == "paired_not_saved" || e.code == "pair_uncertain" { "the result is uncertain; check Other PCs before trying again" } else { "existing connections are unchanged" };
                nlog(&format!("join failed: {} ({})", e.code, err_text(&e)));
                self.note = Some(format!("Join failed; {why} ({})", err_text(&e)));
            }
        }
    }

    /// Start a join. A fresh credentials file is offered; `existing` (every saved connection plus any credentials file
    /// left in the folder) lets the bridge reuse the one of the same network instead of registering this PC again.
    fn begin_join(&mut self, op: Op, mut args: Value, mut meta: JoinMeta) {
        let Some(dir) = self.net_dir.clone() else {
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
        let mut cands = self.existing.clone();
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
        let mut seen = HashSet::new();
        cands.retain(|c| !c.trim().is_empty() && seen.insert(c.clone()));
        if cands.len() > MAX_KNOWN {
            self.note = Some(format!("Join not started: {} saved identities exceed the supported limit of {MAX_KNOWN}. Existing connections and identity files have been kept.", cands.len()));
            return;
        }
        if !cands.is_empty() {
            args["existing_credentials_paths"] = json!(cands);
        }
        meta.creds = creds;
        self.note = Some("Joining…".into());
        self.push(op, args, Pend::Join(meta));
    }

    /// Tick a discovered PC that has no saved connection yet: join it by its pinned fingerprint.
    fn join_pc(&mut self, pc: &Pc) {
        let urls = pc.urls();
        let Some(url) = pc.endpoints.iter().find(|e| e.reachable && e.url.starts_with("https://")).map(|e| e.url.clone()).or_else(|| urls.first().cloned()) else {
            self.note = Some("That PC has no usable address in the last scan; press ReScan.".into());
            return;
        };
        let args = pc.join_args(&url, &self.reg_name);
        let meta = JoinMeta { name: self.reg_name.clone(), host_name: pc.name.clone(), iid: pc.host_iid.clone(), eps: urls, ..Default::default() };
        self.begin_join(Op::JoinAddress, args, meta);
    }

    pub fn ui(&mut self, ui: &mut Ui, c: &Ctx) -> Vec<Cmd> {
        let mut cmds = std::mem::take(&mut self.out);
        let sp = c.settings_path;
        if state_dir(sp).is_none() {
            return cmds;
        }
        self.set_dir(sp);
        if self.seen.is_none_or(|t| t.elapsed() > Duration::from_secs(2)) && !self.busy() {
            self.push(Op::ServiceStatus, json!({}), Pend::Status { ensure: false });
        }
        self.seen = Some(Instant::now());
        if !self.name_loaded {
            self.name_loaded = true;
            if !c.settings.network.name.trim().is_empty() {
                self.name.clone_from(&c.settings.network.name);
            }
        }
        // The first time Clients opens, look for other PCs once (never in the background).
        if !self.scanned && self.source != Source::Off {
            self.scanned = true;
            self.known_urls.extend(c.connections.iter().filter_map(|k| k.url.clone()));
            self.rescan();
        }
        let conns = c.settings.network.effective_connections(sp);
        let idle = !self.busy();
        let svc = self.svc.clone().unwrap_or_default();
        let name_ok = !self.name.trim().is_empty() && self.name.trim().chars().count() <= 64 && !self.name.chars().any(char::is_control);

        ui.add_space(8.0);
        ui.heading("This PC");
        ui.horizontal_wrapped(|ui| {
            ui.label("Name");
            ui.add(egui::TextEdit::singleline(&mut self.name).desired_width(180.0).char_limit(64));
            if ui.add_enabled(name_ok && self.name.trim() != c.settings.network.name, egui::Button::new("Save")).clicked() {
                cmds.push(Cmd::SaveName(self.name.trim().to_owned()));
                self.pend_name = Some(self.name.trim().to_owned());
            }
        });
        ui.label(RichText::new("The name is shown to every PC you connect to. Two PCs may share a name; they are told apart by their certificate.").color(DIM).size(11.0));
        let (line, colour) = match (&self.svc, svc.installed, svc.running) {
            _ if self.svc_unknown => ("Service state unknown after a timeout. Press Refresh.".to_owned(), WARN),
            (None, ..) => ("Checking the service…".to_owned(), DIM),
            (_, false, _) => ("Not installed. Start installs it and makes this PC visible to other PCs.".into(), DIM),
            (_, true, false) => ("Stopped. Other PCs cannot reach this one until it is started (it also starts at sign-in).".into(), WARN),
            (_, true, true) => ("Running; other PCs can find this PC on every network it is connected to.".into(), GOOD),
        };
        ui.colored_label(colour, line);
        if svc.running {
            if svc.endpoints.is_empty() {
                ui.label(RichText::new("Addresses: none reported").color(DIM).size(11.5));
            } else {
                let list: Vec<String> = svc.endpoints.iter().map(|(u, k)| if k.is_empty() { short(u) } else { format!("{} ({k})", short(u)) }).collect();
                ui.add(egui::Label::new(RichText::new(format!("Addresses: {}", list.join(" · "))).color(DIM).size(11.5)).wrap());
            }
            ui.label(RichText::new(format!("Discovery: {} · Receiver: {}", if svc.discovery { "on" } else { "off" }, if svc.receiver_online { "online" } else { "offline" })).color(DIM).size(11.5))
                .on_hover_text("This reports the service coordinator only. This PC accepts work only while Keyloom is open or running in the background.");
        }
        if let Some(id) = &svc.network_id {
            ui.label(RichText::new(format!("Certificate: {id}")).color(DIM).size(10.5));
        }
        if let Some(code) = &svc.error_code {
            ui.colored_label(WARN, format!("{code}: {}", svc.message.chars().take(200).collect::<String>()));
        }
        ui.horizontal_wrapped(|ui| {
            if ui.add_enabled(idle && !svc.running, egui::Button::new("Start")).clicked() {
                cmds.push(Cmd::HostRemoved(false));
                if svc.installed {
                    self.push(Op::ServiceStart, json!({}), Pend::Service("Start"));
                } else {
                    self.install("Start");
                }
            }
            if ui.add_enabled(idle && svc.running, egui::Button::new("Stop")).clicked() {
                self.push(Op::ServiceStop, json!({}), Pend::Service("Stop"));
            }
            let label = if self.remove_armed { "Click again to remove" } else { "Remove" };
            if ui.add_enabled(idle && svc.installed, egui::Button::new(label)).clicked() {
                if self.remove_armed {
                    cmds.push(Cmd::HostRemoved(true));
                    self.push(Op::ServiceRemove, json!({}), Pend::Service("Remove"));
                }
                self.remove_armed = !self.remove_armed;
            }
            if ui.add_enabled(idle, egui::Button::new("Refresh")).clicked() {
                self.push(Op::ServiceStatus, json!({}), Pend::Status { ensure: false });
            }
        });
        // Invitation: off = any PC on your networks may join by address; on = joins need an invitation file.
        let mut inv = if svc.installed && self.svc.is_some() { !svc.allow_join } else { c.settings.network.use_invitation };
        if crate::ui::theme::checkbox(ui, &mut inv, "Use invitation").changed() {
            cmds.push(Cmd::UseInvitation(inv));
            self.pend_allow = Some(!inv);
            self.inv_dirty = true;
        }
        if !inv {
            ui.label(RichText::new("Any PC that can reach this one on your LAN, Tailscale or ZeroTier can join without an invitation.").color(DIM).size(11.0));
        } else {
            ui.label(RichText::new("Other PCs need an invitation file to join this PC.").color(DIM).size(11.0));
            let online = c.connections.iter().find(|k| k.online && conns.iter().any(|x| x.local_host && x.credentials == k.credentials));
            ui.horizontal_wrapped(|ui| {
                ui.label("Save invitation to");
                ui.add(egui::TextEdit::singleline(&mut self.invite_path).desired_width(240.0));
                if ui.button("Browse…").clicked() { cmds.push(Cmd::Browse(PickKind::NetInvitationSave)); }
                if ui.add_enabled(online.is_some() && !self.invite_path.trim().is_empty(), egui::Button::new("Create invitation")).clicked() {
                    if let Some(k) = online {
                        cmds.push(Cmd::Invite { connection: k.credentials.clone(), path: self.invite_path.trim().to_owned() });
                    }
                }
            });
            if online.is_none() {
                ui.label(RichText::new("Start this PC's service first to create an invitation.").color(DIM).size(11.5));
            }
            if let Some(n) = c.invite_note {
                ui.label(RichText::new(n).color(DIM).size(11.5));
            }
            ui.horizontal_wrapped(|ui| {
                ui.label("Import invitation");
                ui.add(egui::TextEdit::singleline(&mut self.invitation).desired_width(240.0));
                if ui.button("Browse…").clicked() { cmds.push(Cmd::Browse(PickKind::NetInvitationOpen)); }
                if ui.add_enabled(!self.joining() && !self.invitation.trim().is_empty(), egui::Button::new("Join with invitation")).clicked() {
                    let meta = JoinMeta { name: self.reg_name.clone(), ..Default::default() };
                    // The invitation file stays with the bridge; only the routes this scan found are offered for its network.
                    let routes: Vec<Value> = self.scan.iter().flat_map(|s| s.pcs.iter()).filter(|p| !p.is_self).map(|p| (p, p.urls())).filter(|(_, u)| !u.is_empty())
                        .take(MAX_KNOWN).map(|(p, _)| p.invitation_route()).collect();
                    let mut args = json!({"invitation_path": self.invitation.trim(), "name": self.reg_name});
                    if !routes.is_empty() {
                        args["discovery_endpoints"] = json!(routes);
                    }
                    self.begin_join(Op::Pair, args, meta);
                }
            });
        }

        ui.add_space(14.0);
        ui.horizontal(|ui| {
            ui.heading("Other PCs");
            if self.scanning() {
                let secs = self.scan_started.map_or(0, |t| t.elapsed().as_secs());
                ui.spinner();
                ui.label(RichText::new(format!("Scanning… {secs} s")).color(DIM));
                if ui.button("Cancel").clicked() {
                    self.cancel_scan();
                }
                ui.ctx().request_repaint_after(Duration::from_millis(500));
            } else if ui.button("ReScan").clicked() {
                self.known_urls.extend(c.connections.iter().filter_map(|k| k.url.clone()));
                self.rescan();
            }
        });
        if let Some(s) = &self.scan {
            let mut line = format!("Scanned {} network interface(s) in {:.1} s", s.interfaces, s.elapsed_ms as f64 / 1000.0);
            if s.partial {
                line.push_str(" · Scan incomplete; some addresses could not be checked in time");
            }
            for (what, state) in [("Tailscale", &s.tailscale), ("Neighbours", &s.neighbors)] {
                if state == "error" {
                    line.push_str(&format!(" · {what} lookup failed"));
                }
            }
            ui.label(RichText::new(line).color(DIM).size(11.5));
        }
        let pcs: Vec<&Pc> = self.scan.iter().flat_map(|s| s.pcs.iter()).filter(|p| !p.is_self).collect();
        let mut rows = 0;
        let mut joins = vec![];
        egui::Grid::new("other_pcs").num_columns(5).striped(true).spacing(egui::vec2(18.0, 8.0)).show(ui, |ui| {
            for h in ["PC", "Addresses", "Type", "Reach", "Status"] {
                ui.label(RichText::new(h).strong());
            }
            ui.end_row();
            let mut shown: Vec<&str> = vec![];
            for pc in &pcs {
                rows += 1;
                let k = find_conn(&conns, c.connections, &pc.network_id, pc.host_iid.as_deref());
                if let Some(k) = k {
                    shown.push(&k.credentials);
                }
                let mut on = k.is_some_and(|k| k.enabled);
                let can = k.is_some() || (pc.join && !self.joining());
                let label = if pc.name.is_empty() { "Unnamed PC".to_owned() } else { pc.name.clone() };
                let changed = ui.add_enabled_ui(can, |ui| crate::ui::theme::checkbox(ui, &mut on, &label).on_hover_text(format!("Certificate {}", pc.network_id)).changed()).inner;
                if changed {
                    match k {
                        Some(k) => cmds.push(Cmd::UsePc { credentials: k.credentials.clone(), network_id: pc.network_id.clone(), host_iid: pc.host_iid.clone(), name: pc.name.clone(), endpoints: pc.urls(), on }),
                        None if on => joins.push((*pc).clone()),
                        None => {}
                    }
                }
                let mut hosts: Vec<String> = vec![];
                let mut kinds: Vec<&str> = vec![];
                for e in &pc.endpoints {
                    let h = host_of(&e.url);
                    if !hosts.contains(&h) { hosts.push(h); }
                    if !e.kind.is_empty() && !kinds.contains(&e.kind.as_str()) { kinds.push(&e.kind); }
                }
                ui.add(egui::Label::new(RichText::new(hosts.join(", ")).size(11.5)).wrap());
                ui.label(kinds.join(", "));
                let up = pc.endpoints.iter().filter(|e| e.reachable).count();
                let best = pc.endpoints.iter().filter(|e| e.reachable).filter_map(|e| e.rtt_ms).fold(f64::INFINITY, f64::min);
                let reach = ui.colored_label(if up > 0 { GOOD } else { WARN }, if up > 0 && best.is_finite() { format!("{up}/{} · {best:.0} ms", pc.endpoints.len()) } else { format!("{up}/{}", pc.endpoints.len()) });
                if let Some(e) = pc.endpoints.iter().find_map(|e| e.error.as_ref()).filter(|_| up == 0) {
                    reach.on_hover_text(e.chars().take(200).collect::<String>());
                }
                let online = k.is_some_and(|k| c.connections.iter().any(|i| i.credentials == k.credentials && i.online));
                let status = match k {
                    Some(k) if k.enabled && online => ("Connected".to_owned(), GOOD),
                    Some(k) if k.enabled => ("Enabled, not connected".into(), WARN),
                    Some(_) => ("Not used".into(), DIM),
                    None if pc.join => ("Available".into(), DIM),
                    None => ("Needs invitation".into(), WARN),
                };
                ui.horizontal(|ui| {
                    ui.colored_label(status.1, status.0);
                    if !pc.receiver_online {
                        ui.label(RichText::new("receiver offline").color(DIM).size(11.0)).on_hover_text("The PC's service coordinator is reachable, but its Keyloom app is not receiving work right now.");
                    }
                });
                ui.end_row();
            }
            // Saved PCs the scan did not find stay visible (and can still be switched off).
            for k in conns.iter().filter(|k| !k.local_host && !shown.contains(&k.credentials.as_str())) {
                let info = c.connections.iter().find(|i| i.credentials == k.credentials);
                rows += 1;
                let nid = conn_nid(k, c.connections);
                let name = if !k.host_name.is_empty() { k.host_name.clone() } else { info.map_or_else(|| "Saved PC".to_owned(), |i| i.label.clone()) };
                let mut on = k.enabled;
                if ui.add_enabled_ui(true, |ui| crate::ui::theme::checkbox(ui, &mut on, &name).changed()).inner {
                    cmds.push(Cmd::UsePc { credentials: k.credentials.clone(), network_id: nid, host_iid: None, name: String::new(), endpoints: vec![], on });
                }
                let addr: Vec<String> = k.endpoints.iter().map(|u| host_of(u)).collect();
                ui.add(egui::Label::new(RichText::new(if addr.is_empty() { info.and_then(|i| i.url.as_deref()).map(host_of).unwrap_or_default() } else { addr.join(", ") }).size(11.5)).wrap());
                ui.label("");
                ui.colored_label(WARN, "not found");
                ui.colored_label(if info.is_some_and(|i| i.online) { GOOD } else { DIM }, if info.is_some_and(|i| i.online) { "Connected" } else if k.enabled { "Saved, not connected" } else { "Saved" });
                ui.end_row();
            }
        });
        if rows == 0 {
            ui.label(RichText::new(if self.scan.is_some() { "No other Keyloom PCs were found. Check that the other PC is running Keyloom, then ReScan." } else { "Press ReScan to look for other PCs." }).color(DIM));
        }
        for pc in joins {
            self.join_pc(&pc);
        }
        if self.busy() { ui.label(RichText::new("Working…").color(DIM)); }
        if let Some(n) = &self.note { ui.add(egui::Label::new(RichText::new(n).color(DIM).size(12.0)).selectable(true)); }
        cmds
    }
}

#[cfg(test)]
mod discovery_contract_tests {
    use super::*;

    #[test]
    fn checkbox_and_invitation_send_full_certificate_pin() {
        let fp = "abcdef01".repeat(8);
        let scan = parse_scan(&json!({"pcs": [{"network_id": &fp[..16], "fingerprint_sha256": fp,
            "endpoints": [{"url": "https://100.64.1.2:8443", "reachable": true}]}]}));
        let pc = &scan.pcs[0];
        assert_eq!(pc.network_id.len(), 16);
        for args in [pc.join_args("https://100.64.1.2:8443", "PC"), pc.invitation_route()] {
            assert_eq!(args["fingerprint_sha256"], fp);
            assert_eq!(args["endpoints"][0], "https://100.64.1.2:8443");
        }
        assert!(parse_scan(&json!({"pcs": [{"network_id": &fp[..16], "fingerprint_sha256": &fp[..16]}]})).pcs.is_empty());
    }
}
