//! Persistent child client for the network bridge (`python -m backend.netlink`, JSON lines over stdio).
//! UI-free transport only: it sends the bridge's fixed operations and hands back correlated replies. It never
//! retries, submits, claims, starts or cancels by itself, resolves no path and holds no credential (operations
//! carry file *paths*; the bridge reads the private files and redacts secrets). Nothing here blocks the UI
//! thread: a writer thread feeds stdin, a reader thread parses stdout, `poll` only swaps in what has arrived.
//!
//! Protocol v1 (see `backend/netlink.py`): one request per line `{"id","op","args"}` (<= 1 MiB incl. newline),
//! one reply per request `{"id","ok":true,"result"}` / `{"id","ok":false,"error":{"code","message"}}`, plus one
//! `{"event":"ready","protocol":1}` at start. Requests are served strictly in order by the bridge.
//! Platform requirement (not verified natively): the child must be the real interpreter, not a launcher that
//! spawns it. If a launcher (e.g. a Windows venv `python.exe` redirector) is killed, an orphaned grandchild can keep
//! the pipes open and `stop_process` would wait for the reader threads. Installed runs resolve the descriptor's
//! `runtime/python/...` executable (`launch(None).python`, used below when no python is passed); developer venvs on
//! Windows need a native test before the link is relied on there.
//! Error codes are passed through untouched (`busy`, `security`, `transport`, `paired_not_saved`, `pair_uncertain`,
//! coordinator codes...). This module adds only `link_ended` (the process ended before answering: the request
//! MAY have been processed) and `link_protocol` (unreadable output).
use serde_json::{json, Value};
use std::collections::{HashSet, VecDeque};
use std::io::{BufRead, BufReader, Read, Write};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::{channel, Sender};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

pub const PROTOCOL: u64 = 1;
/// The bridge refuses longer request lines.
pub const MAX_REQUEST_LINE: usize = 1 << 20;
/// Replies carry at most a 1 MiB coordinator body plus the envelope.
pub const MAX_REPLY_LINE: usize = (1 << 20) + 4096;
pub const MAX_PENDING: usize = 64;
const MAX_STDERR: usize = 2048;
const READY_TIMEOUT: Duration = Duration::from_secs(20);
const CLOSE_GRACE: Duration = Duration::from_secs(3);
/// Per-request network timeout handed to the bridge (`--timeout`).
pub const BRIDGE_TIMEOUT_S: u32 = 15;

/// The bridge's operations; `name()` is the wire name.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Op {
    Pair,
    Connect,
    Register,
    Submit,
    Claim,
    Start,
    Reject,
    Events,
    Stats,
    Cancel,
    Job,
    JobEvents,
    Installations,
    Estimate,
}

impl Op {
    pub fn name(self) -> &'static str {
        match self {
            Op::Pair => "pair",
            Op::Connect => "connect",
            Op::Register => "register",
            Op::Submit => "submit",
            Op::Claim => "claim",
            Op::Start => "start",
            Op::Reject => "reject",
            Op::Events => "events",
            Op::Stats => "stats",
            Op::Cancel => "cancel",
            Op::Job => "job",
            Op::JobEvents => "job_events",
            Op::Installations => "installations",
            Op::Estimate => "estimate",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LinkError {
    pub code: String,
    pub message: String,
}

impl LinkError {
    fn local(code: &str, message: &str) -> Self {
        Self { code: code.into(), message: message.into() }
    }
    /// Another bridge process already holds these credentials.
    pub fn is_busy(&self) -> bool {
        self.code == "busy"
    }
}

/// One answer. `id` is `None` only when the bridge could not read the request it answers.
#[derive(Clone, Debug, PartialEq)]
pub struct Reply {
    pub id: Option<String>,
    pub outcome: Result<Value, LinkError>,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Message {
    Ready { protocol: u64 },
    Reply(Reply),
}

fn valid_id(s: &str) -> bool {
    (1..=64).contains(&s.len()) && s.bytes().all(|b| b.is_ascii_alphanumeric() || b"_.-".contains(&b))
}

/// Strict parse of one bridge line; anything else is an error (never guessed at).
pub fn parse_line(line: &str) -> Result<Message, String> {
    let v: Value = serde_json::from_str(line).map_err(|e| format!("not JSON: {e}"))?;
    let o = v.as_object().ok_or("not an object")?;
    if let Some(ev) = o.get("event") {
        let protocol = o.get("protocol").and_then(Value::as_u64).ok_or("ready lacks protocol")?;
        return (ev == "ready" && o.len() == 2).then_some(Message::Ready { protocol }).ok_or_else(|| "unknown event".into());
    }
    let id = match o.get("id").ok_or("reply lacks id")? {
        Value::Null => None,
        Value::String(s) if valid_id(s) => Some(s.clone()),
        _ => return Err("invalid reply id".into()),
    };
    let outcome = match o.get("ok").and_then(Value::as_bool).ok_or("reply lacks ok")? {
        true if o.len() == 3 && o.get("result").is_some_and(Value::is_object) => Ok(o["result"].clone()),
        false if o.len() == 3 => {
            let e = o.get("error").and_then(Value::as_object).ok_or("error reply lacks error")?;
            let code = e.get("code").and_then(Value::as_str).filter(|c| (1..=64).contains(&c.len())).ok_or("error lacks code")?;
            let msg = e.get("message").and_then(Value::as_str).ok_or("error lacks message")?;
            Err(LinkError { code: code.into(), message: msg.chars().take(500).collect() })
        }
        _ => return Err("reply has the wrong shape".into()),
    };
    Ok(Message::Reply(Reply { id, outcome }))
}

enum Line {
    Text,
    TooLong,
    Eof,
}

fn read_bounded_line(r: &mut impl BufRead, buf: &mut Vec<u8>) -> std::io::Result<Line> {
    buf.clear();
    let mut over = false;
    loop {
        let avail = r.fill_buf()?;
        if avail.is_empty() {
            return Ok(if buf.is_empty() && !over { Line::Eof } else if over { Line::TooLong } else { Line::Text });
        }
        let (take, done) = avail.iter().position(|b| *b == b'\n').map_or((avail.len(), false), |i| (i, true));
        let room = MAX_REPLY_LINE.saturating_sub(buf.len());
        if take > room {
            over = true;
        } else {
            buf.extend_from_slice(&avail[..take]);
        }
        r.consume(take + usize::from(done));
        if done {
            return Ok(if over { Line::TooLong } else { Line::Text });
        }
    }
}

#[derive(Default)]
struct Shared {
    ready: Option<u64>,
    pending: HashSet<String>,
    replies: VecDeque<Reply>,
    ended: Option<String>,
    stray: u32,
    /// Raw bytes, bounded; decoded lossily only when read, so a multi-byte character can never be split by a truncate.
    stderr: Vec<u8>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum State {
    Starting,
    Ready,
    /// Terminal: the reason is shown as-is; a new `Link` is a deliberate user/app action.
    Ended(String),
}

pub struct Link {
    shared: Arc<Mutex<Shared>>,
    child: Child,
    tx: Option<Sender<Vec<u8>>>,
    threads: Vec<std::thread::JoinHandle<()>>,
    started: Instant,
    next: u64,
    reaped: bool,
}

impl std::fmt::Debug for Link {
    // Never prints request arguments.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Link(pid {}, {:?})", self.child.id(), self.state())
    }
}

fn end(shared: &Mutex<Shared>, why: String, wake: &dyn Fn()) {
    shared.lock().unwrap().ended.get_or_insert(why);
    wake();
}

impl Link {
    /// Spawn the bridge (or a test stand-in). `wake` runs after each reply and when the link ends.
    pub fn start(mut cmd: Command, wake: Arc<dyn Fn() + Send + Sync>) -> Result<Self, String> {
        cmd.stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped());
        let mut child = cmd.spawn().map_err(|e| format!("cannot start the network link: {e}"))?;
        let (mut stdin, out, err) = (child.stdin.take().expect("piped"), child.stdout.take().expect("piped"), child.stderr.take().expect("piped"));
        let shared = Arc::new(Mutex::new(Shared::default()));
        let (s1, w1) = (shared.clone(), wake.clone());
        let reader = std::thread::Builder::new().name("netlink-reader".into()).spawn(move || {
            let mut r = BufReader::new(out);
            let mut buf = vec![];
            loop {
                let why = match read_bounded_line(&mut r, &mut buf) {
                    Ok(Line::Text) => match std::str::from_utf8(&buf).map_err(|e| e.to_string()).and_then(parse_line) {
                        Ok(Message::Ready { protocol }) if protocol == PROTOCOL => {
                            s1.lock().unwrap().ready = Some(protocol);
                            w1();
                            continue;
                        }
                        Ok(Message::Ready { protocol }) => format!("the link speaks protocol {protocol}, this app needs {PROTOCOL}"),
                        Ok(Message::Reply(rep)) => {
                            let mut g = s1.lock().unwrap();
                            // Only a reply to a request we are still waiting for is kept; the rest is counted, not trusted.
                            match rep.id.as_ref().map(|i| g.pending.remove(i)) {
                                Some(true) => g.replies.push_back(rep),
                                _ => g.stray = g.stray.saturating_add(1),
                            }
                            drop(g);
                            w1();
                            continue;
                        }
                        Err(e) => format!("the link sent unreadable output ({e})"),
                    },
                    Ok(Line::TooLong) => "the link sent a line over the size limit".into(),
                    Ok(Line::Eof) | Err(_) => "the network link process ended".into(),
                };
                end(&s1, why, &*w1);
                break;
            }
        });
        let (tx, rx) = channel::<Vec<u8>>();
        let (s2, w2) = (shared.clone(), wake.clone());
        let writer = std::thread::Builder::new().name("netlink-writer".into()).spawn(move || {
            while let Ok(line) = rx.recv() {
                if stdin.write_all(&line).and_then(|_| stdin.flush()).is_err() {
                    end(&s2, "the network link process stopped accepting requests".into(), &*w2);
                    break;
                }
            }
            // Dropping `stdin` here is the EOF the bridge treats as a clean shutdown.
        });
        let s3 = shared.clone();
        let drain = std::thread::Builder::new().name("netlink-stderr".into()).spawn(move || {
            let mut r = err;
            let mut chunk = [0u8; 512];
            while let Ok(n) = r.read(&mut chunk) {
                if n == 0 {
                    break;
                }
                let mut g = s3.lock().unwrap();
                let room = MAX_STDERR.saturating_sub(g.stderr.len());
                g.stderr.extend_from_slice(&chunk[..n.min(room)]);
            }
        });
        let threads = [reader, writer, drain].into_iter().filter_map(Result::ok).collect();
        Ok(Self { shared, child, tx: Some(tx), threads, started: Instant::now(), next: 1, reaped: false })
    }

    pub fn pid(&self) -> u32 {
        self.child.id()
    }

    pub fn state(&self) -> State {
        self.state_at(Instant::now())
    }

    /// `state` at a given instant (the ready timeout is judged against it; tests pass a later `now`).
    pub fn state_at(&self, now: Instant) -> State {
        let g = self.shared.lock().unwrap();
        match (&g.ended, g.ready) {
            (Some(why), _) => State::Ended(why.clone()),
            (None, Some(_)) => State::Ready,
            (None, None) if now.saturating_duration_since(self.started) > READY_TIMEOUT => State::Ended("the network link did not start".into()),
            (None, None) => State::Starting,
        }
    }

    /// Bounded tail of the process's stderr (the bridge redacts secrets and prints at most a few lines).
    pub fn stderr_tail(&self) -> String {
        let g = self.shared.lock().unwrap();
        // A cut at the byte limit may leave half a character at the end: drop it so the text never exceeds the limit.
        let end = match std::str::from_utf8(&g.stderr) {
            Err(e) if e.error_len().is_none() => e.valid_up_to(),
            _ => g.stderr.len(),
        };
        String::from_utf8_lossy(&g.stderr[..end]).into_owned()
    }

    /// Replies with no matching pending request (never delivered to callers).
    pub fn stray_replies(&self) -> u32 {
        self.shared.lock().unwrap().stray
    }

    /// Queue one operation; returns its unique request id. `args` must be a JSON object. Nothing is sent twice.
    pub fn request(&mut self, op: Op, args: Value) -> Result<String, LinkError> {
        if !args.is_object() {
            return Err(LinkError::local("bad_request", "args must be a JSON object"));
        }
        let id = format!("r{}", self.next);
        let mut line = serde_json::to_vec(&json!({"id": id, "op": op.name(), "args": args})).expect("serialisable");
        line.push(b'\n');
        if line.len() > MAX_REQUEST_LINE {
            return Err(LinkError::local("too_large", "request exceeds 1 MiB"));
        }
        let mut g = self.shared.lock().unwrap();
        if let Some(why) = &g.ended {
            return Err(LinkError::local("link_ended", why));
        }
        if g.pending.len() >= MAX_PENDING {
            return Err(LinkError::local("busy_link", "too many requests are waiting for the link"));
        }
        g.pending.insert(id.clone());
        drop(g);
        self.next += 1;
        match self.tx.as_ref().map(|t| t.send(line)) {
            Some(Ok(())) => Ok(id),
            _ => {
                self.shared.lock().unwrap().pending.remove(&id);
                Err(LinkError::local("link_ended", "the network link is closed"))
            }
        }
    }

    pub fn pending(&self) -> usize {
        self.shared.lock().unwrap().pending.len()
    }

    /// Everything that arrived since the last call. If the link has ended, each request still waiting gets a
    /// `link_ended` reply exactly once (it MAY have been processed; nothing is retried or cancelled for the caller).
    pub fn poll(&mut self) -> Vec<Reply> {
        self.poll_at(Instant::now())
    }

    pub fn poll_at(&mut self, now: Instant) -> Vec<Reply> {
        let state = self.state_at(now);
        let mut g = self.shared.lock().unwrap();
        if let (State::Ended(why), None) = (&state, &g.ended) {
            g.ended = Some(why.clone());
        }
        let mut out: Vec<Reply> = g.replies.drain(..).collect();
        if let Some(why) = g.ended.clone() {
            let mut ids: Vec<String> = g.pending.drain().collect();
            ids.sort_by_key(|i| i[1..].parse::<u64>().unwrap_or(0));
            out.extend(ids.into_iter().map(|id| Reply {
                id: Some(id),
                outcome: Err(LinkError::local("link_ended", &format!("{why}; the request may have been processed"))),
            }));
            drop(g);
            self.stop_process(); // an ended link is not left half alive
        }
        out
    }

    fn stop_process(&mut self) {
        if self.reaped {
            return;
        }
        self.reaped = true;
        let _ = self.child.kill();
        let _ = self.child.wait();
        self.tx = None;
        for t in self.threads.drain(..) {
            let _ = t.join();
        }
    }

    /// Ask the bridge to close (releases its credential lock), wait up to 3 s, then kill if it did not exit. Reaps.
    /// **Blocks for up to `CLOSE_GRACE` (3 s): never call it on the UI thread**; hand the `Link` to a short-lived
    /// thread (plain `Drop` kills without waiting for a graceful exit).
    pub fn close(&mut self) {
        if let Some(tx) = self.tx.take() {
            let _ = tx.send(b"{\"id\":\"close\",\"op\":\"close\"}\n".to_vec());
        } // dropping the sender also ends the writer, closing stdin (EOF)
        let end = Instant::now() + CLOSE_GRACE;
        while Instant::now() < end && matches!(self.child.try_wait(), Ok(None)) {
            std::thread::sleep(Duration::from_millis(20));
        }
        self.stop_process();
    }
}

impl Drop for Link {
    fn drop(&mut self) {
        self.stop_process();
    }
}

/// Where the app gets its link from. `Off` (the default, and what tests use) starts nothing.
#[derive(Clone, Debug, Default, PartialEq)]
pub enum Source {
    #[default]
    Off,
    /// The bundled (or developer) Python running `backend.netlink`.
    Local,
    /// Tests: any command that speaks the bridge protocol.
    Command(Vec<String>),
}

impl Source {
    pub fn command(&self, repo: Option<&std::path::Path>, python: Option<&std::path::Path>) -> Result<Option<Command>, String> {
        match self {
            Source::Off => Ok(None),
            Source::Local => repo.map(|r| Some(bridge_command(r, python))).ok_or_else(|| "no runtime folder was found for the network link".into()),
            Source::Command(a) => {
                let (p, rest) = a.split_first().ok_or("empty link command")?;
                let mut c = Command::new(p);
                c.args(rest);
                Ok(Some(c))
            }
        }
    }
}

/// `python -m backend.netlink` from the runtime folder. No `-I`: isolated mode drops the cwd from `sys.path`
/// and `-m` needs it (same as the worker launch). Installed runs hand the descriptor over and strip foreign
/// Python variables via `prepare_python_command`; no checkout path is assumed beyond `repo`.
pub fn bridge_command(repo: &std::path::Path, python: Option<&std::path::Path>) -> Command {
    let launch = crate::runtime::launch(None);
    let py = python.map(Into::into).or_else(|| launch.python.clone()).unwrap_or_else(|| crate::import::python(repo));
    let mut c = Command::new(py);
    c.args(["-m", "backend.netlink", "--timeout", &BRIDGE_TIMEOUT_S.to_string()]).current_dir(repo).env("PYTHONUNBUFFERED", "1");
    crate::runtime::prepare_python_command(&mut c, launch.install.as_ref());
    c
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_replies_strictly() {
        assert_eq!(parse_line(r#"{"event":"ready","protocol":1}"#), Ok(Message::Ready { protocol: 1 }));
        let ok = parse_line(r#"{"id":"r1","ok":true,"result":{"a":1}}"#).unwrap();
        assert_eq!(ok, Message::Reply(Reply { id: Some("r1".into()), outcome: Ok(json!({"a":1})) }));
        let err = parse_line(r#"{"id":null,"ok":false,"error":{"code":"too_large","message":"m"}}"#).unwrap();
        assert!(matches!(err, Message::Reply(Reply { id: None, outcome: Err(e) }) if e.code == "too_large"));
        for bad in ["", "[]", "{", r#"{"event":"other","protocol":1}"#, r#"{"event":"ready"}"#, r#"{"id":"r1","ok":true}"#,
            r#"{"id":"r1","ok":true,"result":[1]}"#, r#"{"id":"../x","ok":true,"result":{}}"#, r#"{"id":"r1","ok":false,"error":{"code":5,"message":"m"}}"#,
            r#"{"id":"r1","ok":true,"result":{},"extra":1}"#, r#"{"id":5,"ok":true,"result":{}}"#, r#"{"ok":true,"result":{}}"#] {
            assert!(parse_line(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn op_names_match_the_bridge() {
        let names: Vec<_> = [Op::Pair, Op::Connect, Op::Register, Op::Submit, Op::Claim, Op::Start, Op::Reject, Op::Events, Op::Stats, Op::Cancel, Op::Job, Op::JobEvents, Op::Installations].map(Op::name).to_vec();
        assert_eq!(names, ["pair", "connect", "register", "submit", "claim", "start", "reject", "events", "stats", "cancel", "job", "job_events", "installations"]);
    }

    #[test]
    fn bounded_line_reader_skips_overlong_lines_and_resyncs() {
        let mut data = vec![b'x'; MAX_REPLY_LINE + 10];
        data.push(b'\n');
        data.extend_from_slice(b"ok\n");
        let mut r = BufReader::new(&data[..]);
        let mut buf = vec![];
        assert!(matches!(read_bounded_line(&mut r, &mut buf).unwrap(), Line::TooLong));
        assert!(matches!(read_bounded_line(&mut r, &mut buf).unwrap(), Line::Text));
        assert_eq!(buf, b"ok");
        assert!(matches!(read_bounded_line(&mut r, &mut buf).unwrap(), Line::Eof));
    }

    #[test]
    fn local_source_uses_module_launch_not_isolated_mode() {
        let c = bridge_command(std::path::Path::new("/opt/ck"), Some(std::path::Path::new("/opt/ck/py")));
        let args: Vec<_> = c.get_args().map(|a| a.to_string_lossy().into_owned()).collect();
        assert_eq!(&args[..3], ["-m", "backend.netlink", "--timeout"]);
        assert!(!args.iter().any(|a| a == "-I"));
        assert_eq!(c.get_current_dir(), Some(std::path::Path::new("/opt/ck")));
        assert!(Source::Off.command(None, None).unwrap().is_none());
        assert!(Source::Local.command(None, None).is_err());
    }
}
