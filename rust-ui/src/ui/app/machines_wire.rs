//! App side of the Machines page: builds its snapshot from the app's existing owners (settings, connections, setup,
//! receiving, scheduler, metrics) and routes the page's acts to the existing handlers. No requests and no IO while
//! building; every act goes through the same calls the old Clients screen made.
use super::*;
use crate::network_setup::{Cmd, Ctx, InstallsState, PageAction};
use crate::remote::{Action, ConnRow};
use crate::scheduler::{Placement, TargetStatus};
use crate::ui::board::FxMachine;
use crate::ui::machines_page::{self as page, Act};

fn meter(pct: f32, text: &str) -> page::Meter {
    page::Meter { fraction: pct.is_finite().then(|| (pct / 100.0).clamp(0.0, 1.0)), text: text.to_owned() }
}

fn meters(m: &FxMachine) -> page::Meters {
    page::Meters { gpu: meter(m.gpu, ""), vram: meter(m.vram, &m.vramt), cpu: meter(m.cpu, ""), ram: meter(m.ram, &m.ramt) }
}

/// The Board's assignment id of a connection's machine (`network:<namespace>/<installation>`), when known.
fn placement_id(cr: &ConnRow) -> Option<String> {
    cr.default_target.clone().or_else(|| cr.namespace.as_ref().zip(cr.installation_ids.first()).map(|(ns, i)| format!("network:{ns}/{i}")))
}

fn short_url(u: &str) -> String {
    u.trim_start_matches("https://").trim_end_matches('/').to_owned()
}

impl App {
    /// This computer's own network id and installation id (as the old Clients table passed them to the row filter).
    fn own_net_ids(&self) -> (Option<String>, Option<String>) {
        self.setup.svc().map(|v| (v.network_id.clone(), v.host_installation_id.clone())).unwrap_or_default()
    }

    /// Called when Machines is entered from another page: the one page object lives for the session, so its one-shot
    /// open requests (service status, scan seed, joined list) are re-armed here.
    pub(super) fn machines_entered(&mut self) {
        self.machines_page.reset_entry();
        let default = self.settings.saved().network.default_submit.clone();
        if !default.is_empty() {
            let (own_net, own_iid) = self.own_net_ids();
            let rows = self.remote.conn_rows(self.settings.saved(), own_net.as_deref(), own_iid.as_deref());
            match rows.iter().find(|r| r.creds.to_string_lossy() == default).and_then(placement_id) {
                Some(id) => self.ai.placement = Placement::Target(id),
                None => self.machines_page.set_notice(Some("The saved default connection has no known target machine yet.".into())),
            }
        }
    }

    /// Rebuild the page's data from what the app already holds (cheap; every frame the page is open). Sends nothing.
    pub(super) fn refresh_machines_page(&mut self) {
        let now = Instant::now();
        let infos = self.remote.infos();
        let saved = self.settings.saved();
        let cx = Ctx { settings: saved, settings_path: self.settings.store_path(), connected: self.remote.online(), connections: &infos };
        let snap = self.setup.page_snapshot(&cx);
        let (own_net, own_iid) = self.own_net_ids();
        let conns = self.remote.conn_rows(saved, own_net.as_deref(), own_iid.as_deref());
        let queue = self.ai.queue_view();
        let remote_jobs = self.remote.jobs_snapshot();
        let machines = self.machine_rows(now, &queue, &remote_jobs);
        let targets = self.ai.scheduler().targets();
        let placement = self.ai.placement.clone();
        let ready_tasks: Vec<String> = self.ai.local_caps().map(|c| c.tasks.into_iter().filter(|(_, ready)| *ready).map(|(k, _)| k).collect()).unwrap_or_default();

        let locals: Vec<_> = targets.iter().filter(|t| t.local).collect();
        let local_targets = locals.iter().map(|t| page::LocalTarget {
            id: t.id.clone(),
            label: if locals.len() > 1 { format!("This machine - {}", t.id) } else { "This machine".into() },
            state_text: match &t.status {
                TargetStatus::Refused(why) | TargetStatus::Down(why) if t.enabled => why.to_string(),
                _ => String::new(),
            },
            ready_tasks: ready_tasks.clone(),
            can_recheck: t.busy_with.is_none(),
            default: placement == Placement::Target(t.id.clone()),
        }).collect();
        let gpus = locals.iter().filter_map(|t| t.ready.as_ref()?.gpu.as_ref()).map(|g| page::GpuInfo { name: g.name.clone(), vram_text: format!("{:.0} GB", g.vram_gb) }).collect();

        let inactive = self.inbound.inactive.clone();
        let running_text = if inactive.is_some() { String::new() } else if !snap.addresses.is_empty() { "Receiving on".to_owned() } else { snap.status_line.clone() };
        let local = page::LocalLive {
            name: if snap.name.is_empty() { saved.network.name.clone() } else { snap.name.clone() },
            allow_joins: snap.allow_join,
            service_state_text: running_text,
            service_error: snap.error.clone(),
            discovery_error: snap.discovery_error.clone(),
            service_mode: match snap.mode {
                crate::network_setup::ServiceMode::AppHosted => page::ServiceMode::AppHosted,
                crate::network_setup::ServiceMode::Starting | crate::network_setup::ServiceMode::Checking => page::ServiceMode::Checking,
                crate::network_setup::ServiceMode::Unknown => page::ServiceMode::Unknown,
                crate::network_setup::ServiceMode::NotInstalled => page::ServiceMode::NotInstalled,
                crate::network_setup::ServiceMode::Stopped => page::ServiceMode::Legacy { running: false },
                crate::network_setup::ServiceMode::Running => page::ServiceMode::Legacy { running: true },
            },
            can_start: snap.can_start,
            can_stop: snap.can_stop,
            can_remove: snap.can_remove,
            can_refresh: snap.can_refresh,
            addresses: snap.addresses.clone(),
            receive_jobs: saved.network.receives(),
            endpoint_inactive: inactive,
            receive_summary: self.inbound.status_lines(),
            held_attempts: self.inbound.held_items().into_iter().map(|h| page::HeldItem { attempt_id: h.attempt_id, text: h.text, actionable: h.actionable, foreign: h.foreign }).collect(),
            local_targets,
            gpus,
            meters: machines.iter().find(|m| m.local == 1).map(meters).unwrap_or_default(),
            installation_id: snap.host_installation_id.clone().unwrap_or_default(),
            certificate_id: snap.certificate.clone().unwrap_or_default(),
        };

        let mut notes: Vec<(String, bool)> = snap.note.iter().cloned().map(|n| (n, snap.note_is_error)).collect();
        let mut others = Vec::new();
        for cr in &conns {
            let key = cr.creds.to_string_lossy().into_owned();
            let pid = placement_id(cr);
            let fx = pid.as_ref().and_then(|p| machines.iter().find(|m| &m.id == p));
            notes.extend(cr.notes.iter().map(|n| (format!("{}: {n}", cr.name), true)));
            others.push(page::OtherRow {
                kind_local: saved.network.connections().iter().any(|c| c.credentials == key && c.local_host),
                name: cr.name.clone(),
                status_text: cr.status_text.clone(),
                status_detail: cr.status_detail.replace('\n', " · "),
                address: cr.live_url.as_deref().or(cr.endpoints.first().map(String::as_str)).map(short_url).unwrap_or_default(),
                address_choices: cr.endpoints.clone(),
                preferred: cr.preferred.clone(),
                connect: cr.enabled,
                accept_work: cr.receive,
                default: cr.default_submit || pid.as_ref().is_some_and(|p| placement == Placement::Target(p.clone())),
                joined: true,
                join_allowed: true,
                joining: false,
                gpus: cr.gpus.iter().map(|n| page::GpuInfo { name: n.clone(), vram_text: String::new() }).collect(),
                meters: fx.map(meters).unwrap_or_default(),
                leaving: cr.leaving,
                ready_tasks: Vec::new(), // remote task readiness is not exposed: shown as none/unknown
                installation_id: cr.installation_ids.first().cloned().unwrap_or_default(),
                last_seen_text: String::new(),
                key,
            });
        }
        // Discovered PCs that are not saved connections (matched by certificate + host identity, never by name).
        for p in snap.pcs.iter().filter(|p| !p.saved) {
            let status_text = if !p.receiver_online { "Receiver offline".to_owned() } else if p.reach_up == 0 { "Not reachable".to_owned() } else { format!("Reachable on {}/{} addresses", p.reach_up, p.reach_total) };
            let mut detail = vec![format!("{}/{} addresses reachable", p.reach_up, p.reach_total)];
            detail.extend(p.best_rtt_ms.map(|r| format!("RTT {r:.0} ms")));
            detail.extend(p.reach_error.clone());
            others.push(page::OtherRow {
                key: p.key.clone(),
                name: p.name.clone(),
                kind_local: p.kinds.iter().any(|k| k == "Local"),
                status_text,
                status_detail: detail.join(" · "),
                address: p.hosts.first().cloned().unwrap_or_default(),
                address_choices: p.urls.clone(),
                join_allowed: p.join_allowed,
                joining: snap.joining_key.as_deref() == Some(p.key.as_str()),
                installation_id: p.host_iid.clone().unwrap_or_default(),
                ..Default::default()
            });
        }
        let default_automatic = placement == Placement::Auto && !others.iter().any(|o| o.default);
        let joined_state = match &snap.installs_state {
            InstallsState::Loading => page::JoinedState::Loading,
            InstallsState::Loaded => page::JoinedState::Loaded,
            InstallsState::Failed(e) => page::JoinedState::Failed(e.clone()),
        };
        let joined = snap.installs.iter().map(|i| page::JoinedRow { id: i.id.clone(), name: i.name.clone(), joined_text: i.joined.clone(), last_seen_text: i.last_seen.clone(), online: i.connected, in_use: i.in_use, is_self: i.is_this_pc }).collect();
        let scan = page::ScanLive {
            scanning: snap.scanning,
            elapsed_secs: snap.scan_started.filter(|_| snap.scanning).map(|t| t.elapsed().as_secs()),
            summary: snap.scan_summary.clone().unwrap_or_default(),
            warnings: snap.scan_warnings.clone(),
        };
        self.machines_page.update(page::Live { local, scan, others, default_automatic, joined, joined_state, notes });
    }

    /// Route one page act to the existing handler (the same calls the old Clients screen made).
    pub(super) fn handle_machines_act(&mut self, ctx: &egui::Context, a: Act) {
        let setup = match a {
            Act::SaveName(n) => Some(PageAction::SaveName(n)),
            Act::AllowJoin(on) => Some(PageAction::AllowJoin(on)),
            Act::OpenPage => Some(PageAction::OpenPage),
            Act::Rescan => Some(PageAction::Rescan),
            Act::CancelScan => Some(PageAction::CancelScan),
            Act::Join(key) => Some(PageAction::Join(key)),
            Act::RefreshService => Some(PageAction::RefreshService),
            Act::StartService => Some(PageAction::StartService),
            Act::StopService => Some(PageAction::StopService),
            Act::RemoveServiceConfirmed => Some(PageAction::RemoveServiceConfirmed),
            Act::OpenJoined => Some(PageAction::OpenJoined),
            Act::RefreshJoined => Some(PageAction::RefreshJoined),
            Act::Retire { ids, unused_only } => Some(PageAction::RetireConfirmed { ids, unused_only }),
            other => {
                self.machines_net_act(other);
                None
            }
        };
        if let Some(act) = setup {
            let infos = self.remote.infos();
            let cx = Ctx { settings: self.settings.saved(), settings_path: self.settings.store_path(), connected: self.remote.online(), connections: &infos };
            let cmds = self.setup.handle(act, &cx);
            self.apply_setup_cmds(cmds);
        }
        ctx.request_repaint();
    }

    fn machines_net_act(&mut self, a: Act) {
        match a {
            Act::Connect { key, on } => self.net_action(Action::Enable { creds: key, on }),
            Act::AcceptWork { key, on } => {
                // Q2: accepting work on a Not-used connection also connects it, in one save. Unticking never disconnects.
                if let Some(c) = self.net_draft().connections.iter_mut().find(|c| c.credentials == key) {
                    c.receive = on;
                    c.enabled |= on;
                }
                self.net_save("Updated");
            }
            Act::LocalAcceptWork(on) => self.net_action(Action::ReceiveJobs(on)),
            Act::Prefer { key, url } => self.net_action(Action::Prefer { creds: key, url: url.unwrap_or_default() }),
            Act::RetryEndpoint => self.net_action(Action::RetryEndpoint),
            Act::ReleaseHeld(id) => self.net_action(Action::ReleaseHeld(id)),
            Act::DefaultMachine(id) => self.set_default_machine(id),
            Act::Recheck(id) => {
                if self.ai.scheduler().targets().iter().any(|t| t.id == id && t.busy_with.is_none()) {
                    self.ai.scheduler_mut().reset_target(&id);
                }
            }
            Act::RemoveConnectionConfirmed(creds) => self.remove_connection_confirmed(&creds),
            Act::DismissNotice => self.machines_page.dismiss_notice(),
            _ => {}
        }
    }

    /// Q1: one default for new shots. A local target id sets the session placement and clears the saved network default;
    /// a connection key sets the saved network default and the placement of its machine; `None` = Automatic clears both.
    fn set_default_machine(&mut self, id: Option<String>) {
        let (own_net, own_iid) = self.own_net_ids();
        let (placement, default_submit) = match id {
            None => (Placement::Auto, String::new()),
            Some(id) if self.ai.scheduler().targets().iter().any(|t| t.local && t.id == id) => (Placement::Target(id), String::new()),
            Some(key) => {
                let conns = self.remote.conn_rows(self.settings.saved(), own_net.as_deref(), own_iid.as_deref());
                let Some(machine) = conns.iter().find(|c| c.creds.to_string_lossy() == key).and_then(placement_id) else {
                    self.machines_page.set_notice(Some("That connection has no known machine to use as the default.".into()));
                    return;
                };
                (Placement::Target(machine), key)
            }
        };
        self.ai.placement = placement;
        if self.net_draft().default_submit != default_submit {
            self.net_action(Action::DefaultSubmit(default_submit));
        }
    }

    /// The confirmed part of the old Remove prompt (the Machines page asked already): re-check the blocker, then leave
    /// (finished by the app's tick) or remove at once when offline.
    fn remove_connection_confirmed(&mut self, creds: &str) {
        if !self.net_draft().connections.iter().any(|c| c.credentials == creds) {
            return;
        }
        let name = self.conn_name(creds);
        if let Some(why) = self.remote.remove_blocker(creds) {
            self.machines_page.set_notice(Some(format!("Connection not removed: {why}.")));
        } else if !self.remote.leave(creds) {
            self.finish_remove(creds, Some(format!("{name} still lists this PC; retire it from that PC's Clients page.")));
        }
    }

    /// What the old Clients branch did with setup's outputs.
    fn apply_setup_cmds(&mut self, cmds: Vec<Cmd>) {
        for c in cmds {
            match c {
                Cmd::SaveName(n) => {
                    self.net_draft().name = n;
                    self.net_save("Name changed");
                }
                Cmd::AllowJoin(allow) => {
                    self.net_draft().allow_join = allow;
                    self.net_save("Updated");
                }
                Cmd::UsePc { credentials, network_id, host_iid, name, endpoints, on } => {
                    self.net_use_pc(&credentials, &network_id, host_iid, &name, endpoints, on);
                }
                Cmd::HostRemoved(gone) => {
                    let n = self.net_draft();
                    n.host_removed = gone;
                    n.connections.iter_mut().filter(|c| c.local_host).for_each(|c| c.enabled = !gone);
                    self.net_save("Updated");
                }
            }
        }
    }
}
