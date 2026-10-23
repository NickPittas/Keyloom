//! Latest-only system metrics (CPU %, RAM, GPU utilisation/VRAM) for the always-visible footer and the client
//! cards. UI-free. One long-lived helper process (`backend/system_metrics.py`, bundled runtime, no torch/CUDA)
//! prints a JSON line per sample; this module parses them strictly, stamps each with a LOCAL monotonic receipt
//! time (an age, never a foreign clock), and reports Starting / Live / Stale / Unavailable honestly: a counter
//! that is missing is `None` and shown as "—", never as 0. Field names and byte units follow the coordinator's
//! stats contract so remote reports parse through the same `Sample` (`Sample::from_api`). GPU rows are NVML/PCI
//! order plus uuid: they are for display only. Matching one to a verified worker GPU needs the uuid of the device
//! the worker actually opened (CUDA_VISIBLE_DEVICES aware); never forward these rows to the network as claims.
//! The helper is spawned without a Windows no-window flag (the worker launch has none either): console flashing on
//! Windows is an unverified native-gate item, not solved here.
use serde_json::Value;
use std::collections::BTreeMap;
use std::io::{BufRead, BufReader, Read};
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// A sample older than this is shown as stale, never as current (helper interval is ~1.5 s).
pub const STALE_AFTER: Duration = Duration::from_secs(5);
/// No sample at all this long after start: the helper is not working.
const NO_SAMPLE_AFTER: Duration = Duration::from_secs(10);
const MAX_LINE: usize = 16 * 1024;
const MAX_GPUS: usize = 64;
const MAX_STDERR: usize = 2048;
/// The helper is started at most this many times; after that the failure stays visible and quiet.
const MAX_STARTS: u32 = 3;

#[derive(Clone, Debug, PartialEq)]
pub struct GpuStat {
    pub uuid: String,
    /// NVML index (PCI bus order, the worker's `--gpu`); the identity is the uuid.
    pub index: Option<u32>,
    pub name: Option<String>,
    pub util: Option<f32>,
    pub vram_used: Option<u64>,
    pub vram_total: Option<u64>,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Sample {
    pub seq: u64,
    pub cpu_percent: Option<f32>,
    pub ram_used: Option<u64>,
    pub ram_total: Option<u64>,
    /// `None` = GPUs could not be read (reason in `errors["gpu"]`); `Some(empty)` = none present.
    pub gpus: Option<Vec<GpuStat>>,
    /// Why a field is missing (`cpu`, `ram`, `gpu`), bounded.
    pub errors: BTreeMap<String, String>,
}

fn pct(v: Option<&Value>) -> Option<f32> {
    let x = v?.as_f64()?;
    (x.is_finite() && (0.0..=100.0).contains(&x)).then_some(x as f32)
}
fn bytes(v: Option<&Value>) -> Option<u64> {
    v?.as_u64()
}
fn text(v: Option<&Value>, max: usize) -> Option<String> {
    let s = v?.as_str()?;
    (!s.is_empty() && s.len() <= max && !s.chars().any(char::is_control)).then(|| s.to_owned())
}

/// The fields shared by the local helper line and the coordinator's `stats` object. Each field is checked
/// on its own: a bad value becomes `None`, it does not discard the rest.
fn fields(v: &Value) -> Sample {
    let (mut used, mut total) = (bytes(v.get("ram_used_bytes")), bytes(v.get("ram_total_bytes")));
    if used.zip(total).is_none_or(|(u, t)| u > t) {
        (used, total) = (None, None);
    }
    let gpus = v.get("gpus").and_then(Value::as_array).map(|a| {
        a.iter()
            .take(MAX_GPUS)
            .filter_map(|g| {
                let uuid = text(g.get("uuid"), 96)?; // no identity, no row
                let (mut u, mut t) = (bytes(g.get("vram_used_bytes")), bytes(g.get("vram_total_bytes")));
                if u.zip(t).is_none_or(|(u, t)| u > t) {
                    (u, t) = (None, None);
                }
                Some(GpuStat {
                    uuid,
                    index: g.get("index").and_then(Value::as_u64).and_then(|i| u32::try_from(i).ok()),
                    name: text(g.get("name"), 120),
                    util: pct(g.get("utilization_percent")),
                    vram_used: u,
                    vram_total: t,
                })
            })
            .collect()
    });
    Sample { seq: v.get("seq").and_then(Value::as_u64).unwrap_or(0), cpu_percent: pct(v.get("cpu_percent")), ram_used: used, ram_total: total, gpus, errors: BTreeMap::new() }
}

/// One helper line (`{"v":1,"type":"system_metrics",...}`).
pub fn parse_line(line: &str) -> Result<Sample, String> {
    let v: Value = serde_json::from_str(line).map_err(|e| format!("bad metrics line: {e}"))?;
    if v.get("type").and_then(Value::as_str) != Some("system_metrics") || v.get("v").and_then(Value::as_u64) != Some(1) {
        return Err("not a system_metrics v1 line".into());
    }
    let mut s = fields(&v);
    if let Some(m) = v.get("errors").and_then(Value::as_object) {
        for (k, why) in m.iter().take(8) {
            if let Some(w) = why.as_str() {
                s.errors.insert(k.chars().take(16).collect(), w.chars().take(120).collect());
            }
        }
    }
    Ok(s)
}

impl Sample {
    /// A coordinator `stats` object (`/v1/installations`): returns the sample and the server-computed
    /// `measured_age_s` (how old the measurement was when the server answered). `Ok(None)` = `stats: null`.
    pub fn from_api(stats: &Value) -> Result<Option<(Sample, Option<f64>)>, String> {
        if stats.is_null() {
            return Ok(None);
        }
        if !stats.is_object() {
            return Err("stats must be an object or null".into());
        }
        let age = stats.get("measured_age_s").and_then(Value::as_f64).filter(|a| a.is_finite() && *a >= 0.0);
        Ok(Some((fields(stats), age)))
    }
}

/// Where the app gets local metrics from. `Off` (the default, and what tests use) starts nothing.
#[derive(Clone, Debug, Default, PartialEq)]
pub enum Source {
    #[default]
    Off,
    /// The bundled (or developer) Python helper, one process for the session.
    Local,
    /// Private evidence renders: the last valid line of a JSON-lines file, shown as a constant live reading.
    Fixture(std::path::PathBuf),
    /// Tests: any command that prints helper lines.
    Command(Vec<String>),
}

impl Source {
    pub fn supervisor(&self, repo: Option<&std::path::Path>, python: Option<std::path::PathBuf>) -> Supervisor {
        match self {
            Source::Off => Supervisor::off(),
            Source::Local => {
                let repo = repo.map(std::path::Path::to_path_buf);
                Supervisor::new(move || match &repo {
                    Some(r) => Ok(helper_command(r, python.as_deref())),
                    None => Err("no runtime folder was found for the metrics helper".into()),
                })
            }
            Source::Fixture(f) => match std::fs::read_to_string(f).ok().and_then(|t| t.lines().rev().find_map(|l| parse_line(l).ok())) {
                Some(s) => Supervisor::fixture(s),
                None => Supervisor::off(),
            },
            Source::Command(a) => {
                let a = a.clone();
                Supervisor::new(move || {
                    let (p, rest) = a.split_first().ok_or("empty metrics command")?;
                    let mut c = Command::new(p);
                    c.args(rest);
                    Ok(c)
                })
            }
        }
    }
}

/// What the UI may show right now.
#[derive(Clone, Debug, PartialEq)]
pub enum View {
    Starting,
    Live(Sample),
    Stale { sample: Sample, age: Duration },
    Unavailable(String),
}

/// A client card's telemetry. Remote values are real only when the network API supplied them.
#[derive(Clone, Debug, PartialEq)]
pub enum ClientTelemetry {
    /// This computer, from the local sampler.
    Local(View),
    NotConnected,
    /// Connected or configured, but nothing was reported (and why).
    Unreported(String),
    /// Reported by the client over the API. Ages are what the SERVER computed when it answered; `received` is the local
    /// instant we got that answer, so the shown age keeps growing if polling stops (build with `ClientTelemetry::reported`).
    Reported { sample: Sample, measured_age_s: Option<f64>, server_received_age_s: Option<f64>, received: Instant },
}

/// An age is a finite, non-negative number of seconds; NaN/inf/negative means "unknown" (never fresh, never a NaN label).
fn clean_age(a: Option<f64>) -> Option<f64> {
    a.filter(|a| a.is_finite() && *a >= 0.0)
}

impl ClientTelemetry {
    /// Both ages come from the server (`measured_age_s`, `received_age_s` of `/v1/installations`); `Sample::from_api`
    /// returns only the first, so the caller must read `received_age_s` itself. Invalid ages become unknown here.
    pub fn reported(sample: Sample, measured_age_s: Option<f64>, server_received_age_s: Option<f64>) -> Self {
        Self::reported_at(sample, measured_age_s, server_received_age_s, Instant::now())
    }
    /// For tests / replay: the receipt instant is given.
    pub fn reported_at(sample: Sample, measured_age_s: Option<f64>, server_received_age_s: Option<f64>, received: Instant) -> Self {
        Self::Reported { sample, measured_age_s: clean_age(measured_age_s), server_received_age_s: clean_age(server_received_age_s), received }
    }
}

/// Remote reports older than this are stale (the client reports about every few seconds).
pub const REMOTE_STALE_S: f64 = 15.0;

#[derive(Default)]
struct Shared {
    latest: Option<(Sample, Instant)>,
    ended: Option<String>,
    bad_lines: u32,
    /// Raw bounded bytes, decoded lossily only when read: a truncate can never split a character (and panic under the lock).
    stderr: Vec<u8>,
}

/// One running helper. Dropping it kills and reaps the process.
pub struct Monitor {
    shared: Arc<Mutex<Shared>>,
    child: Child,
    threads: Vec<std::thread::JoinHandle<()>>,
    started: Instant,
}

fn read_bounded_line(r: &mut impl BufRead, buf: &mut Vec<u8>) -> std::io::Result<Option<bool>> {
    buf.clear();
    loop {
        let avail = r.fill_buf()?;
        if avail.is_empty() {
            return Ok((!buf.is_empty()).then_some(true));
        }
        let (take, done) = match avail.iter().position(|b| *b == b'\n') {
            Some(i) => (i, true),
            None => (avail.len(), false),
        };
        let room = MAX_LINE.saturating_sub(buf.len());
        buf.extend_from_slice(&avail[..take.min(room)]);
        let over = take > room;
        r.consume(take + usize::from(done));
        if over {
            // Skip the rest of an oversized line; it is not a sample.
            if !done {
                loop {
                    let a = r.fill_buf()?;
                    if a.is_empty() {
                        break;
                    }
                    let (n, end) = a.iter().position(|b| *b == b'\n').map_or((a.len(), false), |i| (i + 1, true));
                    r.consume(n);
                    if end {
                        break;
                    }
                }
            }
            return Ok(Some(false));
        }
        if done {
            return Ok(Some(true));
        }
    }
}

impl Monitor {
    /// Spawn `cmd` (stdout = metrics lines). `wake` runs after each accepted sample and when the helper ends.
    pub fn start(mut cmd: Command, wake: Arc<dyn Fn() + Send + Sync>) -> Result<Self, String> {
        cmd.stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped());
        let mut child = cmd.spawn().map_err(|e| format!("cannot start the metrics helper: {e}"))?;
        let (out, err) = (child.stdout.take().expect("piped"), child.stderr.take().expect("piped"));
        let shared = Arc::new(Mutex::new(Shared::default()));
        let (s1, w1) = (shared.clone(), wake.clone());
        let reader = std::thread::Builder::new().name("metrics-reader".into()).spawn(move || {
            let mut r = BufReader::new(out);
            let mut buf = vec![];
            loop {
                match read_bounded_line(&mut r, &mut buf) {
                    Ok(Some(true)) => {
                        let parsed = std::str::from_utf8(&buf).map_err(|e| e.to_string()).and_then(parse_line);
                        let mut g = s1.lock().unwrap();
                        match parsed {
                            Ok(s) => g.latest = Some((s, Instant::now())),
                            Err(_) => g.bad_lines = g.bad_lines.saturating_add(1),
                        }
                        drop(g);
                        w1();
                    }
                    Ok(Some(false)) => {
                        let mut g = s1.lock().unwrap();
                        g.bad_lines = g.bad_lines.saturating_add(1);
                    }
                    Ok(None) | Err(_) => break,
                }
            }
            let mut g = s1.lock().unwrap();
            g.ended.get_or_insert_with(|| "the metrics helper stopped".into());
            drop(g);
            w1();
        });
        let s2 = shared.clone();
        let drain = std::thread::Builder::new().name("metrics-stderr".into()).spawn(move || {
            // Keep a bounded tail for the reason text, then keep draining so the helper never blocks on a full pipe.
            let mut r = err;
            let mut chunk = [0u8; 512];
            while let Ok(n) = r.read(&mut chunk) {
                if n == 0 {
                    break;
                }
                let mut g = s2.lock().unwrap();
                let room = MAX_STDERR.saturating_sub(g.stderr.len());
                g.stderr.extend_from_slice(&chunk[..n.min(room)]);
            }
        });
        let threads: Vec<_> = [reader, drain].into_iter().filter_map(Result::ok).collect();
        Ok(Self { shared, child, threads, started: Instant::now() })
    }

    pub fn pid(&self) -> u32 {
        self.child.id()
    }

    /// The helper's bounded stderr (at most 2 KiB).
    pub fn stderr_tail(&self) -> String {
        stderr_text(&self.shared.lock().unwrap().stderr)
    }

    pub fn ended(&self) -> Option<String> {
        let g = self.shared.lock().unwrap();
        g.ended.is_some().then(|| drop_lock_reason(&g))
    }

    pub fn view(&self, now: Instant) -> View {
        let g = self.shared.lock().unwrap();
        match &g.latest {
            Some((s, at)) => {
                let age = now.saturating_duration_since(*at);
                if age <= STALE_AFTER {
                    View::Live(s.clone())
                } else {
                    View::Stale { sample: s.clone(), age }
                }
            }
            None if g.ended.is_some() => View::Unavailable(drop_lock_reason(&g)),
            None if now.saturating_duration_since(self.started) > NO_SAMPLE_AFTER => {
                View::Unavailable(if g.bad_lines > 0 { "the metrics helper sends unreadable data".into() } else { "no sample from the metrics helper".into() })
            }
            None => View::Starting,
        }
    }

    /// Kill, reap and join. Safe to call twice.
    pub fn stop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        for t in self.threads.drain(..) {
            let _ = t.join();
        }
    }
}

/// Bounded stderr as text; a character cut by the byte limit is dropped, never decoded to U+FFFD past the limit.
fn stderr_text(bytes: &[u8]) -> String {
    let end = match std::str::from_utf8(bytes) {
        Err(e) if e.error_len().is_none() => e.valid_up_to(),
        _ => bytes.len(),
    };
    String::from_utf8_lossy(&bytes[..end]).into_owned()
}

fn drop_lock_reason(g: &Shared) -> String {
    let e = g.ended.clone().unwrap_or_default();
    match stderr_text(&g.stderr).trim().lines().last() {
        Some(l) => format!("{e}: {}", l.chars().take(160).collect::<String>()),
        None => e,
    }
}

impl Drop for Monitor {
    fn drop(&mut self) {
        self.stop();
    }
}

/// Owns the helper for the whole session: starts it on the first tick, restarts it a bounded number of times
/// with a growing delay, and keeps the last failure visible instead of retrying every frame.
pub struct Supervisor {
    make: Option<Box<dyn Fn() -> Result<Command, String> + Send>>,
    mon: Option<Monitor>,
    starts: u32,
    retry_at: Option<Instant>,
    failure: Option<String>,
    fixed: Option<Sample>,
}

impl Supervisor {
    /// Nothing is started (tests, or metrics turned off): the footer says so.
    pub fn off() -> Self {
        Self { make: None, mon: None, starts: 0, retry_at: None, failure: Some("not started".into()), fixed: None }
    }
    pub fn new(make: impl Fn() -> Result<Command, String> + Send + 'static) -> Self {
        Self { make: Some(Box::new(make)), mon: None, starts: 0, retry_at: None, failure: None, fixed: None }
    }
    /// A constant reading for private evidence renders: always "live", no process.
    pub fn fixture(s: Sample) -> Self {
        Self { fixed: Some(s), ..Self::off() }
    }

    pub fn tick(&mut self, now: Instant, wake: &Arc<dyn Fn() + Send + Sync>) {
        let Some(make) = &self.make else { return };
        if let Some(m) = &self.mon {
            if let Some(why) = m.ended() {
                self.failure = Some(why);
                self.mon = None; // drop = kill + reap
                self.retry_at = (self.starts < MAX_STARTS).then(|| now + Duration::from_secs(5 * u64::from(self.starts)));
            }
        }
        if self.mon.is_none() && self.retry_at.is_none_or(|t| now >= t) && self.starts < MAX_STARTS {
            self.starts += 1;
            self.retry_at = None;
            match make().and_then(|c| Monitor::start(c, wake.clone())) {
                Ok(m) => {
                    self.mon = Some(m);
                    self.failure = None;
                }
                Err(e) => {
                    self.failure = Some(e);
                    self.retry_at = (self.starts < MAX_STARTS).then(|| now + Duration::from_secs(5 * u64::from(self.starts)));
                }
            }
        }
    }

    pub fn view(&self, now: Instant) -> View {
        if let Some(s) = &self.fixed {
            return View::Live(s.clone());
        }
        match (&self.mon, &self.failure) {
            (Some(m), _) => m.view(now),
            (None, Some(f)) => View::Unavailable(f.clone()),
            (None, None) => View::Starting,
        }
    }

    /// Explicit shutdown (the process is also reaped on drop).
    pub fn stop(&mut self) {
        if let Some(m) = &mut self.mon {
            m.stop();
        }
        self.mon = None;
        self.make = None;
    }
}

// ---- presentation (pure) -----------------------------------------------------------------------------------

pub fn fmt_pct(v: Option<f32>) -> String {
    v.map_or("—".into(), |x| format!("{x:.0}%"))
}
fn gib(b: u64) -> String {
    format!("{:.1}", b as f64 / (1u64 << 30) as f64)
}
pub fn fmt_mem(used: Option<u64>, total: Option<u64>) -> String {
    match used.zip(total) {
        Some((u, t)) => format!("{} / {} GiB", gib(u), gib(t)),
        None => "—".into(),
    }
}
pub fn gpu_line(g: &GpuStat) -> String {
    format!(
        "GPU{} {} {} · VRAM {}",
        g.index.map_or("?".into(), |i| i.to_string()),
        g.name.as_deref().unwrap_or("unnamed"),
        fmt_pct(g.util),
        fmt_mem(g.vram_used, g.vram_total)
    )
}

/// `G1 88% 10.0/24.0G`: the short per-GPU form (utilisation and VRAM, no name).
fn gpu_short(g: &GpuStat) -> String {
    let vram = g.vram_used.zip(g.vram_total).map_or("—".into(), |(u, t)| format!("{}/{}G", gib(u), gib(t)));
    format!("G{} {} {}", g.index.map_or("?".into(), |i| i.to_string()), fmt_pct(g.util), vram)
}

/// Everything a chrome row or card prints, with no layout in it.
#[derive(Clone, Debug, PartialEq)]
pub struct Cells {
    pub cpu: String,
    pub ram: String,
    /// The GPU cell at full detail: EVERY GPU with utilisation and VRAM (rendering ones first, marked), or the only GPU.
    pub gpu: String,
    /// The same information from longest to shortest (`gpu` first). The footer shows the longest that fits its
    /// width, so VRAM is never clipped behind a long name; the last entry is a one-GPU-sized aggregate.
    pub gpu_tiers: Vec<String>,
    /// One line per GPU, plus anything that explains a missing value.
    pub detail: Vec<String>,
    /// `None` = current data. Otherwise why the values are missing/old.
    pub status: Option<String>,
    /// Values are not current (stale or absent): draw them dimmed.
    pub dim: bool,
}

/// GPU identity here is the NVML/PCI row and its uuid. Mapping a row to a verified worker GPU (and so claiming it for the
/// network) needs the uuid of the device the worker actually opened (CUDA_VISIBLE_DEVICES aware): never forward these rows as is.
fn gpu_tiers(g: &[GpuStat], active: &[u32]) -> Vec<String> {
    let is_act = |x: &GpuStat| x.index.is_some_and(|i| active.contains(&i));
    let mut rows: Vec<&GpuStat> = g.iter().filter(|x| is_act(x)).collect();
    rows.extend(g.iter().filter(|x| !is_act(x)));
    let any = rows.iter().any(|x| is_act(x));
    let join = |f: &dyn Fn(&GpuStat) -> String, sep: &str| rows.iter().map(|x| f(x)).collect::<Vec<_>>().join(sep);
    let full = join(&|x| gpu_line(x), "  |  ");
    let short = join(&|x| format!("{}{}", if is_act(x) { "▶" } else { "" }, gpu_short(x)), "  ");
    let max = g.iter().filter_map(|x| x.util).fold(None, |m: Option<f32>, u| Some(m.map_or(u, |m| m.max(u))));
    let all_vram = g.iter().all(|x| x.vram_used.is_some() && x.vram_total.is_some());
    let (u, t): (u64, u64) = (g.iter().filter_map(|x| x.vram_used).sum(), g.iter().filter_map(|x| x.vram_total).sum());
    let vram = if all_vram { format!("{}/{} GiB", gib(u), gib(t)) } else { "—".into() };
    let mut tiers = vec![if any { format!("Rendering {full}") } else { full }, short];
    if g.len() > 1 {
        tiers.push(format!("{} GPUs · max {} · VRAM {vram}", g.len(), fmt_pct(max)));
    }
    tiers.push(format!("GPU {} {}", fmt_pct(max), if all_vram { format!("{:.0}/{:.0}G", u as f64 / (1u64 << 30) as f64, t as f64 / (1u64 << 30) as f64) } else { "—".into() }));
    tiers.dedup();
    tiers
}

fn sample_cells(s: &Sample, active: &[u32]) -> Cells {
    let (tiers, mut detail) = match &s.gpus {
        None => (vec!["GPU n/a".to_string()], vec![format!("GPU: {}", s.errors.get("gpu").map_or("not reported", String::as_str))]),
        Some(g) if g.is_empty() => (vec!["GPU none detected".into()], vec!["No NVIDIA GPU was found".into()]),
        Some(g) => (gpu_tiers(g, active), g.iter().map(|x| format!("{} · {}", gpu_line(x), x.uuid)).collect()),
    };
    for k in ["cpu", "ram"] {
        if let Some(e) = s.errors.get(k) {
            detail.push(format!("{}: {e}", k.to_uppercase()));
        }
    }
    if s.gpus.is_some() {
        if let Some(e) = s.errors.get("gpu") {
            detail.push(format!("GPU: {e}"));
        }
    }
    Cells { cpu: fmt_pct(s.cpu_percent), ram: fmt_mem(s.ram_used, s.ram_total), gpu: tiers[0].clone(), gpu_tiers: tiers, detail, status: None, dim: false }
}

fn missing(status: String) -> Cells {
    Cells { cpu: "—".into(), ram: "—".into(), gpu: "GPU —".into(), gpu_tiers: vec!["GPU —".into()], detail: vec![], status: Some(status), dim: true }
}

pub fn view_cells(v: &View, active: &[u32]) -> Cells {
    match v {
        View::Starting => missing("starting".into()),
        View::Unavailable(why) => missing(format!("unavailable: {why}")),
        View::Live(s) => sample_cells(s, active),
        View::Stale { sample, age } => Cells { status: Some(format!("stale {} s", age.as_secs())), dim: true, ..sample_cells(sample, active) },
    }
}

pub fn client_cells(t: &ClientTelemetry, active: &[u32]) -> Cells {
    client_cells_at(t, active, Instant::now())
}

/// `client_cells` judged at `now`: the server's ages plus the time since we received them, so a client whose
/// reports stop becomes stale on its own (the card never keeps showing "2 s old" forever).
pub fn client_cells_at(t: &ClientTelemetry, active: &[u32], now: Instant) -> Cells {
    match t {
        ClientTelemetry::Local(v) => view_cells(v, active),
        ClientTelemetry::NotConnected => missing("not connected".into()),
        ClientTelemetry::Unreported(why) => missing(why.clone()),
        ClientTelemetry::Reported { sample, measured_age_s, server_received_age_s, received } => {
            let elapsed = now.saturating_duration_since(*received).as_secs_f64();
            // Old if either the measurement or the server's last receipt is old (a sample that stopped arriving is old too).
            let age = clean_age(*measured_age_s).map(|a| a + elapsed).into_iter().chain(clean_age(*server_received_age_s).map(|a| a + elapsed)).fold(None, |m: Option<f64>, a| Some(m.map_or(a, |m| m.max(a))));
            match age {
                Some(a) if a <= REMOTE_STALE_S => sample_cells(sample, active),
                Some(a) => Cells { status: Some(format!("stale {a:.0} s")), dim: true, ..sample_cells(sample, active) },
                None => Cells { status: Some("age unknown".into()), dim: true, ..sample_cells(sample, active) },
            }
        }
    }
}

/// The helper command: the bundled (or developer) Python on the script itself, isolated (`-I`), because
/// importing the `backend` package would load cv2/torch.
pub fn helper_command(repo: &std::path::Path, python: Option<&std::path::Path>) -> Command {
    let py = python.map(Into::into).unwrap_or_else(|| crate::import::python(repo));
    let mut c = Command::new(py);
    c.args(["-I"]).arg(repo.join("backend").join("system_metrics.py")).args(["--interval", "1.5"]).current_dir(repo).env("PYTHONUNBUFFERED", "1");
    crate::runtime::prepare_python_command(&mut c, crate::runtime::launch(None).install.as_ref());
    c
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn line(extra: Value) -> String {
        let mut v = json!({"v":1,"type":"system_metrics","seq":3,"cpu_percent":37.5,"ram_used_bytes":4u64<<30,"ram_total_bytes":16u64<<30,
            "gpus":[{"uuid":"GPU-a","index":0,"name":"RTX 4090","utilization_percent":63.0,"vram_used_bytes":10u64<<30,"vram_total_bytes":24u64<<30}],"errors":{}});
        for (k, x) in extra.as_object().cloned().unwrap_or_default() {
            v[k] = x;
        }
        v.to_string()
    }

    #[test]
    fn parses_a_full_line_and_keeps_units_and_identity() {
        let s = parse_line(&line(json!({}))).unwrap();
        assert_eq!((s.seq, s.cpu_percent, s.ram_used, s.ram_total), (3, Some(37.5), Some(4 << 30), Some(16 << 30)));
        let g = &s.gpus.unwrap()[0];
        assert_eq!((g.uuid.as_str(), g.index, g.util, g.vram_total), ("GPU-a", Some(0), Some(63.0), Some(24 << 30)));
    }

    #[test]
    fn bad_or_missing_fields_become_none_never_zero_and_do_not_drop_the_rest() {
        let s = parse_line(&line(json!({"cpu_percent": 250.0, "ram_used_bytes": 99u64<<30, "errors": {"cpu": "warming up"}}))).unwrap();
        assert_eq!(s.cpu_percent, None, "out of range");
        assert_eq!((s.ram_used, s.ram_total), (None, None), "used > total is unknown, not clamped");
        assert!(s.gpus.is_some());
        assert_eq!(s.errors["cpu"], "warming up");
        let s = parse_line(&line(json!({"gpus": [{"uuid": "", "name": "x"}, {"uuid": "GPU-b", "utilization_percent": null, "vram_used_bytes": 5, "vram_total_bytes": 2}]}))).unwrap();
        let g = s.gpus.unwrap();
        assert_eq!(g.len(), 1, "a row without identity is dropped");
        assert_eq!((g[0].util, g[0].vram_used, g[0].vram_total), (None, None, None));
        assert_eq!(parse_line(&line(json!({"gpus": null}))).unwrap().gpus, None);
        for bad in ["", "{", "[]", r#"{"v":2,"type":"system_metrics"}"#, r#"{"v":1,"type":"other"}"#] {
            assert!(parse_line(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn api_stats_use_the_same_fields_and_null_means_nothing_reported() {
        assert_eq!(Sample::from_api(&Value::Null).unwrap(), None);
        assert!(Sample::from_api(&json!(5)).is_err());
        let (s, age) = Sample::from_api(&json!({"seq": 9, "cpu_percent": 12.0, "ram_used_bytes": 1, "ram_total_bytes": 2, "measured_age_s": 1.5, "received_age_s": 0.2,
            "gpus": [{"uuid": "GPU-x", "index": 1, "name": "A", "vram_used_bytes": 1, "vram_total_bytes": 2}]})).unwrap().unwrap();
        assert_eq!((s.cpu_percent, age), (Some(12.0), Some(1.5)));
        assert_eq!(s.gpus.unwrap()[0].util, None, "absent = unavailable");
        let (s, age) = Sample::from_api(&json!({"seq": 1})).unwrap().unwrap();
        assert_eq!((s.cpu_percent, s.ram_used, s.gpus, age), (None, None, None, None));
    }

    #[test]
    fn view_age_uses_the_local_receipt_clock() {
        let m = Monitor { shared: Arc::new(Mutex::new(Shared::default())), child: Command::new("sleep").arg("30").spawn().unwrap(), threads: vec![], started: Instant::now() };
        let t0 = Instant::now();
        assert_eq!(m.view(t0), View::Starting);
        assert!(matches!(m.view(t0 + NO_SAMPLE_AFTER + Duration::from_secs(1)), View::Unavailable(_)));
        m.shared.lock().unwrap().latest = Some((Sample { seq: 1, ..Default::default() }, t0));
        assert!(matches!(m.view(t0 + Duration::from_secs(2)), View::Live(_)));
        match m.view(t0 + Duration::from_secs(9)) {
            View::Stale { age, .. } => assert_eq!(age, Duration::from_secs(9)),
            v => panic!("{v:?}"),
        }
        let mut m = m;
        m.stop();
        m.stop();
    }

    fn gpu(i: u32, util: f32) -> GpuStat {
        GpuStat { uuid: format!("GPU-{i}"), index: Some(i), name: Some("RTX".into()), util: Some(util), vram_used: Some(1 << 30), vram_total: Some(8 << 30) }
    }

    #[test]
    fn cells_show_numbers_dash_for_missing_and_the_rendering_gpu() {
        let s = Sample { cpu_percent: Some(37.0), ram_used: Some(12 << 30), ram_total: Some(32 << 30), gpus: Some(vec![gpu(0, 5.0), gpu(1, 88.0)]), ..Default::default() };
        let c = view_cells(&View::Live(s.clone()), &[]);
        assert_eq!((c.cpu.as_str(), c.ram.as_str()), ("37%", "12.0 / 32.0 GiB"));
        assert_eq!(c.gpu, "GPU0 RTX 5% · VRAM 1.0 / 8.0 GiB  |  GPU1 RTX 88% · VRAM 1.0 / 8.0 GiB", "every GPU, idle ones too, utilisation + VRAM");
        assert_eq!(c.gpu_tiers[1], "G0 5% 1.0/8.0G  G1 88% 1.0/8.0G");
        assert_eq!(c.gpu_tiers[2], "2 GPUs · max 88% · VRAM 2.0/16.0 GiB");
        assert_eq!(c.gpu_tiers[3], "GPU 88% 2/16G");
        assert_eq!(c.detail.len(), 2);
        assert!(c.status.is_none() && !c.dim);
        let c = view_cells(&View::Live(s.clone()), &[1]);
        assert_eq!(c.gpu, "Rendering GPU1 RTX 88% · VRAM 1.0 / 8.0 GiB  |  GPU0 RTX 5% · VRAM 1.0 / 8.0 GiB");
        assert!(c.gpu_tiers[1].starts_with("▶G1 88%"), "{:?}", c.gpu_tiers);
        let one = Sample { gpus: Some(vec![gpu(0, 5.0)]), ..s.clone() };
        assert_eq!(view_cells(&View::Live(one), &[]).gpu, "GPU0 RTX 5% · VRAM 1.0 / 8.0 GiB");
        let none = Sample { cpu_percent: None, ram_used: None, ram_total: None, gpus: None, errors: [("gpu".to_string(), "NVML unavailable".to_string())].into(), ..Default::default() };
        let c = view_cells(&View::Live(none), &[]);
        assert_eq!((c.cpu.as_str(), c.ram.as_str(), c.gpu.as_str()), ("—", "—", "GPU n/a"));
        assert!(c.detail[0].contains("NVML unavailable"));
        let c = view_cells(&View::Stale { sample: s, age: Duration::from_secs(12) }, &[]);
        assert_eq!((c.status.as_deref(), c.dim), (Some("stale 12 s"), true));
        let c = view_cells(&View::Unavailable("nope".into()), &[]);
        assert_eq!((c.cpu.as_str(), c.status.as_deref()), ("—", Some("unavailable: nope")));
    }

    #[test]
    fn remote_cards_never_fabricate_numbers() {
        assert_eq!(client_cells(&ClientTelemetry::NotConnected, &[]).status.as_deref(), Some("not connected"));
        let c = client_cells(&ClientTelemetry::Unreported("API not wired".into()), &[]);
        assert_eq!((c.cpu.as_str(), c.gpu.as_str(), c.dim), ("—", "GPU —", true));
        let s = Sample { cpu_percent: Some(10.0), ..Default::default() };
        let t0 = Instant::now();
        let live = client_cells_at(&ClientTelemetry::reported_at(s.clone(), Some(2.0), Some(0.5), t0), &[], t0);
        assert_eq!((live.cpu.as_str(), live.status.clone()), ("10%", None));
        let old = client_cells_at(&ClientTelemetry::reported_at(s.clone(), Some(60.0), None, t0), &[], t0);
        assert_eq!(old.status.as_deref(), Some("stale 60 s"));
        // The shown age keeps growing on the LOCAL clock: polling stopped => stale, never frozen at "2 s".
        let t = ClientTelemetry::reported_at(s.clone(), Some(2.0), Some(0.5), t0);
        assert_eq!(client_cells_at(&t, &[], t0 + Duration::from_secs(10)).status, None);
        assert_eq!(client_cells_at(&t, &[], t0 + Duration::from_secs(14)).status.as_deref(), Some("stale 16 s"));
        // Invalid ages are unknown, never fresh and never a NaN label (constructor and direct-literal paths alike).
        for bad in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY, -3.0] {
            let c = client_cells_at(&ClientTelemetry::reported_at(Sample::default(), Some(bad), Some(bad), t0), &[], t0);
            assert_eq!(c.status.as_deref(), Some("age unknown"), "{bad}");
            let lit = ClientTelemetry::Reported { sample: Sample::default(), measured_age_s: Some(bad), server_received_age_s: None, received: t0 };
            assert_eq!(client_cells_at(&lit, &[], t0).status.as_deref(), Some("age unknown"), "{bad}");
        }
        let one_bad = client_cells_at(&ClientTelemetry::reported_at(Sample::default(), Some(f64::NAN), Some(40.0), t0), &[], t0);
        assert_eq!(one_bad.status.as_deref(), Some("stale 40 s"), "a valid age still counts");
        // the server's receipt age alone also counts
        let t = ClientTelemetry::reported_at(s, Some(1.0), Some(40.0), t0);
        assert_eq!(client_cells_at(&t, &[], t0).status.as_deref(), Some("stale 40 s"));
    }

    #[test]
    fn oversized_lines_are_skipped_not_parsed() {
        let mut data = vec![b'x'; MAX_LINE + 10];
        data.push(b'\n');
        data.extend_from_slice(b"ok\n");
        let mut r = BufReader::new(&data[..]);
        let mut buf = vec![];
        assert_eq!(read_bounded_line(&mut r, &mut buf).unwrap(), Some(false));
        assert_eq!(read_bounded_line(&mut r, &mut buf).unwrap(), Some(true));
        assert_eq!(buf, b"ok");
        assert_eq!(read_bounded_line(&mut r, &mut buf).unwrap(), None);
    }
}
