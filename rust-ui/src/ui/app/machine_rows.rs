//! Machine rows and meters shared by the Board (and later the Machines page): local targets and network machines with
//! their telemetry and current job. Needs no open project; the caller passes the queue and the network jobs it already has.
use super::*;

impl App {
    /// The local CPU/RAM/GPU/VRAM meters for the status bar; `None` until a full sample (with a GPU) exists.
    pub(super) fn local_meters(&self, now: Instant) -> Option<super::super::board::FxMeters> {
        let view = self.metrics.view(now);
        let sample = match &view {
            crate::metrics::View::Live(s) | crate::metrics::View::Stale { sample: s, .. } => s,
            _ => return None,
        };
        let gpu = sample.gpus.as_ref()?.first()?;
        let (cpu,ru,rt,gp,vu,vt) = (sample.cpu_percent?,sample.ram_used?,sample.ram_total?,gpu.util?,gpu.vram_used?,gpu.vram_total?);
        let gb = 1024.0_f32.powi(3);
        Some(super::super::board::FxMeters { cpu, ram_used: ru as f32 / gb, ram_total: rt as f32 / gb, gpu_pct: gp, gpu_label: gpu.name.clone().unwrap_or_default(), vram_used: vu as f32 / gb, vram_total: vt as f32 / gb })
    }

    /// One row per local target, then one per Automatic-eligible network machine (state, current job, telemetry).
    pub(super) fn machine_rows(&self, now: Instant, queue: &[crate::ui::ai::QueueSection], remote_jobs: &[crate::remote::JobSnapshot]) -> Vec<super::super::board::FxMachine> {
        let view = self.metrics.view(now);
        let (local_sample, local_telemetry) = match &view {
            crate::metrics::View::Live(s) => (Some(s), String::new()),
            crate::metrics::View::Stale { sample, age } => (Some(sample), format!("stale {} s", age.as_secs())),
            _ => (None, String::new()),
        };
        let machine_data = |sample: Option<&crate::metrics::Sample>, gpu_index: Option<u32>| {
            let gb = 1024.0_f32.powi(3);
            let gpu = sample.and_then(|s| s.gpus.as_ref()).and_then(|g| gpu_index.and_then(|i| g.iter().find(|x| x.index == Some(i))).or_else(|| g.first()));
            let cpu = sample.and_then(|s| s.cpu_percent).unwrap_or(f32::NAN);
            let ru = sample.and_then(|s| s.ram_used).map(|n| n as f32 / gb);
            let rt = sample.and_then(|s| s.ram_total).map(|n| n as f32 / gb);
            let ram = ru.zip(rt).filter(|(_, t)| *t > 0.0).map(|(u,t)| u * 100.0 / t).unwrap_or(f32::NAN);
            let vu = gpu.and_then(|g| g.vram_used).map(|n| n as f32 / gb);
            let vt = gpu.and_then(|g| g.vram_total).map(|n| n as f32 / gb);
            let vram = vu.zip(vt).filter(|(_, t)| *t > 0.0).map(|(u,t)| u * 100.0 / t).unwrap_or(f32::NAN);
            let gp = gpu.and_then(|g| g.util).unwrap_or(f32::NAN);
            let name = gpu.and_then(|g| g.name.clone()).unwrap_or_default();
            let memory = |u: Option<f32>, t: Option<f32>| u.zip(t).map_or_else(String::new, |(u,t)| format!("{u:.1} / {t:.1} GB"));
            (gp, vram, cpu, ram, memory(vu,vt), memory(ru,rt), name)
        };
        let remote_machines = self.remote.machines_snapshot();
        let run_job = |client: &str| queue.iter().flat_map(|s| s.runs.iter().filter(|r| r.client == client && r.active).flat_map(move |r| r.rows.iter().filter(|x| x.executing).map(move |x| format!("{} - {} {}", s.title, x.heading, x.cell.text)))).next().unwrap_or_default();
        let targets = self.ai.scheduler().targets();
        let mut machines = Vec::new();
        for target in &targets {
            let offline = !target.enabled || matches!(&target.status, crate::scheduler::TargetStatus::Down(_) | crate::scheduler::TargetStatus::Refused(_) | crate::scheduler::TargetStatus::Stopping | crate::scheduler::TargetStatus::Stopped);
            let job = run_job(&target.id);
            let state = if target.busy_with.is_some() || !job.is_empty() { "busy" } else if offline { "offline" } else { "idle" };
            let gpu = target.id.strip_prefix("local-").and_then(|s| s.parse().ok());
            let (gp, vr, cpu, ram, vramt, ramt, _) = machine_data(local_sample, gpu);
            machines.push(super::super::board::FxMachine { id: target.id.clone(), name: if targets.iter().filter(|t| t.local).count() > 1 { format!("This machine - {}", target.id) } else { "This machine".into() }, local: 1, state: state.into(), job, gpu: gp, vram: vr, cpu, ram, vramt, ramt, telemetry_status: local_telemetry.clone() });
        }
        let candidates = self.remote.automatic_candidates(self.settings.saved(), self.settings.store_path());
        for (id, name) in candidates {
            let Some((namespace, iid)) = id.strip_prefix("network:").and_then(|x| x.split_once('/')) else { continue };
            let install = remote_machines.iter().find(|i| i.namespace.as_deref() == Some(namespace) && i.installation == iid);
            let telemetry = install.map(|i| &i.telemetry);
            let sample = telemetry.and_then(|t| if let crate::metrics::ClientTelemetry::Reported { sample, .. } = t { Some(sample) } else { None });
            let telemetry_status = telemetry.map_or_else(String::new, |t| crate::remote::telemetry_status_at(t, now));
            let (gp, vr, cpu, ram, vramt, ramt, _) = machine_data(sample, None);
            let connected = telemetry.is_some_and(|t| !matches!(t, crate::metrics::ClientTelemetry::NotConnected));
            let busy = install.is_some_and(|i| i.busy == Some(true));
            let display = if machines.iter().any(|m| m.name == name) { format!("{name} - {iid}") } else { name };
            let remote_job = remote_jobs.iter().filter(|j| j.active && j.namespace.as_deref() == Some(namespace) && j.installation.as_deref() == Some(iid)).max_by_key(|j| j.submitted_at);
            let job = remote_job.map_or_else(String::new, |j| format!("{} · {} {}", j.name, j.stage.as_deref().unwrap_or(&j.state), j.progress.map_or_else(String::new, |p| format!("{:.0}%", p * 100.0))));
            machines.push(super::super::board::FxMachine { id: id.clone(), name: display, local: 0, state: if !connected { "offline" } else if busy { "busy" } else { "idle" }.into(), job, gpu: gp, vram: vr, cpu, ram, vramt, ramt, telemetry_status });
        }
        machines
    }
}
