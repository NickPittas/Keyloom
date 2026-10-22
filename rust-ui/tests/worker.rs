mod common;
use ck::jobs::KeyJob;
use ck::worker::*;
use common::*;
use serde_json::json;
use std::time::{Duration, Instant};

fn recv_until(c: &mut WorkerConn, mut stop: impl FnMut(&Msg) -> bool) -> Vec<Msg> {
    let (mut got, end) = (vec![], Instant::now() + Duration::from_secs(20));
    loop {
        match c.try_recv() {
            Some(m) => {
                let s = stop(&m);
                got.push(m);
                if s {
                    return got;
                }
            }
            None => {
                assert!(Instant::now() < end, "timed out; got {got:?}");
                std::thread::sleep(Duration::from_millis(5));
            }
        }
    }
}
fn is_hello(m: &Msg) -> bool {
    matches!(m, Msg::Event(Event::Hello(_)))
}
fn terminal(m: &Msg) -> bool {
    matches!(
        m,
        Msg::Event(Event::Done(_))
            | Msg::Event(Event::Rejected { .. })
            | Msg::Protocol(_)
            | Msg::Exited { .. }
    )
}
fn started(name: &str, fake: &str, n: usize) -> (std::path::PathBuf, WorkerConn, KeyJob, Vec<Msg>) {
    let base = tmp(name);
    let repo = make_repo(&base, "repo", json!({}));
    let mut c = WorkerConn::spawn(&local("t", 0, &repo)).unwrap();
    let hello = recv_until(&mut c, is_hello);
    let j = job(&base, "j1", n, fake);
    c.send(&Cmd::Key(&j)).unwrap();
    (base, c, j, hello)
}

#[test]
fn parse_line_checks_version_and_shape() {
    let ok = r#"{"v":1,"event":"progress","job_id":"a","phase":"refine","done":3,"total":9}"#;
    assert_eq!(
        parse_line(ok).unwrap(),
        Event::Progress {
            job_id: "a".into(),
            phase: "refine".into(),
            done: 3,
            total: 9
        }
    );
    assert!(parse_line(&ok.replace("\"v\":1", "\"v\":2"))
        .unwrap_err()
        .contains("version"));
    assert!(parse_line(r#"{"event":"progress"}"#)
        .unwrap_err()
        .contains("version"));
    assert!(parse_line(r#"{"v":1,"event":"teleport","job_id":"a"}"#).is_err());
    assert!(parse_line(r#"{"v":1,"event":"progress","job_id":"a"}"#).is_err());
    assert!(parse_line("garbage").is_err());
    let d = parse_line(r#"{"v":1,"event":"done","job_id":"a","outcome":"cancelled","message":null,"error_kind":null,"manifest":"/m","published":2,"skipped":0,"expected":5,"manifest_complete":false}"#).unwrap();
    assert!(matches!(
        d,
        Event::Done(Done {
            outcome: Outcome::Cancelled,
            published: 2,
            ..
        })
    ));
    assert_eq!(
        Cmd::Cancel("x").line(),
        r#"{"cmd":"cancel","job_id":"x","v":1}"#
    );
}

#[test]
fn identity_never_trusts_name_and_size() {
    let mk = |sha: Option<&str>| {
        match parse_line(&format!(
        r#"{{"v":1,"event":"hello","protocol":1,"worker_rev":"r","engine":"e","host":"h","gpu":null,"models":{{"corridorkey_v2":{{"path":"/m/ck.safetensors","bytes":7,"sha256":{}}}}}}}"#,
        sha.map(|s| format!("\"{s}\"")).unwrap_or("null".into())
    ))
    .unwrap()
    {
        Event::Hello(h) => h,
        _ => unreachable!(),
    }
    };
    let req = |sha: Option<&str>| ck::jobs::Require {
        protocol: 1,
        models: [("corridorkey_v2".to_string(), sha.map(String::from))].into(),
    };
    assert_eq!(
        mk(Some("AB")).check_identity(&req(Some("ab")), false),
        Ok(false)
    ); // digest equal
    assert!(mk(Some("cd"))
        .check_identity(&req(Some("ab")), true)
        .unwrap_err()
        .contains("differs"));
    assert!(mk(None)
        .check_identity(&req(Some("ab")), true)
        .unwrap_err()
        .contains("no digest"));
    assert!(mk(Some("ab"))
        .check_identity(&req(None), false)
        .unwrap_err()
        .contains("unverifiable"));
    assert_eq!(mk(Some("ab")).check_identity(&req(None), true), Ok(true)); // explicit opt-in, flagged
    let mut r = req(Some("ab"));
    r.protocol = 2;
    assert!(mk(Some("ab")).check_identity(&r, true).is_err());
}

fn ssh(host: &str, user: Option<&str>, repo: &str, py: &str) -> Target {
    Target {
        id: "r".into(),
        kind: TargetKind::Ssh {
            os: RemoteOs::Linux,
            host: host.into(),
            user: user.map(Into::into),
            port: Some(2222),
            python: py.into(),
            repo: repo.into(),
            ssh: None,
        },
        gpu: 1,
        enabled: true,
        storage: Storage::Shared,
        path_map: vec![],
    }
}

#[test]
fn ssh_argv_is_hardened_and_every_dynamic_part_quoted() {
    let a = ssh("render2", Some("me"), "/opt/it's mine", "/opt/py")
        .ssh_args()
        .unwrap();
    assert_eq!(
        a,
        [
            "-T",
            "-o",
            "BatchMode=yes",
            "-o",
            "StrictHostKeyChecking=yes",
            "-o",
            "ClearAllForwardings=yes",
            "-o",
            "ServerAliveInterval=10",
            "-o",
            "ServerAliveCountMax=3",
            "-p",
            "2222",
            "me@render2",
            "--",
            "cd -- '/opt/it'\\''s mine' && exec '/opt/py' -m backend.worker --gpu 1"
        ]
    );
    assert_eq!(sh_quote("a$(b);`c`\n"), "'a$(b);`c`\n'");
    // Option/shell injection through host or user is refused before anything is spawned.
    for (h, u) in [
        ("-oProxyCommand=x", None),
        ("h;rm", None),
        ("a b", None),
        ("", None),
        ("h", Some("-l")),
        ("h", Some("u@x")),
        ("h", Some("a b")),
    ] {
        assert!(ssh(h, u, "/r", "/p").ssh_args().is_err(), "{h:?} {u:?}");
    }
    assert!(ssh("h", None, "/r", "-evil").ssh_args().is_err());
    let mut t = ssh("h", None, "/r", "/p");
    t.storage = Storage::Transfer;
    assert!(t.validate().is_err());
    // settings.json shape from the brief deserialises (unknown `storage` value handled, flat `kind`).
    let t: Target = serde_json::from_value(json!({"id":"render2-gpu0","kind":"ssh","host":"render2","user":null,"port":null,
        "python":"/opt/p","repo":"/opt/r","gpu":0,"storage":"shared","path_map":[["/mnt/NAS","/mnt/NAS"]],"enabled":true})).unwrap();
    assert_eq!(t.path_map[0].1, "/mnt/NAS");
}

#[test]
fn local_worker_hello_progress_publish_done() {
    let (base, mut c, j, hello) = started("ok", "ok", 3);
    let Msg::Event(Event::Hello(h)) = &hello[0] else {
        panic!()
    };
    assert_eq!(
        (h.protocol, h.gpu.as_ref().unwrap().index, h.host.as_str()),
        (1, 0, "fakehost")
    );
    let ev = recv_until(&mut c, terminal);
    let ids: Vec<_> = ev
        .iter()
        .filter_map(|m| {
            if let Msg::Event(e) = m {
                e.job_id()
            } else {
                None
            }
        })
        .collect();
    assert!(ids.iter().all(|i| *i == "j1"));
    assert!(matches!(ev[0], Msg::Event(Event::Accepted { .. })));
    assert_eq!(
        ev.iter()
            .filter(|m| matches!(m, Msg::Event(Event::Published { .. })))
            .count(),
        3
    );
    let Some(Msg::Event(Event::Done(d))) = ev.last() else {
        panic!("{ev:?}")
    };
    assert_eq!(
        (d.outcome, d.published, d.expected),
        (Outcome::Complete, 3, 3)
    );
    for p in &j.outputs["matte"] {
        assert!(std::fs::read(p).unwrap().starts_with(b"\x89PNG"));
    }
    // The same worker takes a second job (persistent process): a repeat is skipped, not clobbered.
    c.send(&Cmd::Key(&j)).unwrap();
    let ev = recv_until(&mut c, terminal);
    let Some(Msg::Event(Event::Done(d))) = ev.last() else {
        panic!()
    };
    assert_eq!((d.published, d.skipped), (0, 3));
    std::fs::remove_dir_all(base).unwrap();
}

#[test]
fn cancel_returns_cancelled_done() {
    let (base, mut c, _j, _) = started("cancel", "hold", 4);
    recv_until(&mut c, |m| matches!(m, Msg::Event(Event::Progress { .. })));
    c.cancel("j1").unwrap();
    let ev = recv_until(&mut c, terminal);
    assert!(matches!(
        ev.last(),
        Some(Msg::Event(Event::Done(Done {
            outcome: Outcome::Cancelled,
            ..
        })))
    ));
    std::fs::remove_dir_all(base).unwrap();
}

#[test]
fn crash_malformed_oversized_and_bad_version_are_reported() {
    let (b, mut c, ..) = started("crash", "crash", 2);
    let ev = recv_until(&mut c, terminal);
    let Some(Msg::Exited { code, .. }) = ev.last() else {
        panic!("{ev:?}")
    };
    assert_eq!(*code, Some(3));
    std::fs::remove_dir_all(b).unwrap();
    for (mode, needle) in [
        ("malformed", "not JSON"),
        ("oversized", "longer than"),
        ("badversion", "version"),
    ] {
        let (b, mut c, ..) = started(mode, mode, 2);
        let ev = recv_until(&mut c, terminal);
        let Some(Msg::Protocol(e)) = ev.last() else {
            panic!("{mode}: {ev:?}")
        };
        assert!(e.contains(needle), "{mode}: {e}");
        c.kill(); // worker is sleeping; kill reaps it
        std::fs::remove_dir_all(b).unwrap();
    }
}

#[test]
fn dropping_the_connection_closes_stdin_and_the_worker_exits() {
    let base = tmp("drop");
    let repo = make_repo(&base, "repo", json!({}));
    let mut c = WorkerConn::spawn(&local("t", 0, &repo)).unwrap();
    recv_until(&mut c, is_hello);
    drop(c);
    wait_until("stdin_eof logged", || {
        log_lines(&base, "repo")
            .iter()
            .any(|l| l["event"] == "stdin_eof")
    });
    std::fs::remove_dir_all(base).unwrap();
}

#[cfg(unix)]
#[test]
fn ssh_transport_runs_hostile_repo_path_without_shell_injection() {
    let base = tmp("ssh");
    let name = "re po $(touch PWNED);'q\" `touch PWNED2`";
    let repo = make_repo(&base, name, json!({}));
    let mut t = ssh("render2", Some("me"), repo.to_str().unwrap(), PY);
    if let TargetKind::Ssh { ssh, .. } = &mut t.kind {
        *ssh = Some(FAKE_SSH.into());
    }
    let mut c = WorkerConn::spawn(&t).unwrap();
    recv_until(&mut c, is_hello);
    let l = &log_lines(&base, name)[0];
    assert_eq!(l["cwd"], repo.to_str().unwrap());
    assert_eq!(
        l["argv"].as_array().unwrap()[1..],
        [json!("--gpu"), json!("1")]
    );
    let argv = l["ssh_argv"].as_str().unwrap().to_owned() + "\n";
    assert!(argv.starts_with("-T\n-o\nBatchMode=yes\n-o\nStrictHostKeyChecking=yes"));
    assert!(argv.contains("\nme@render2\n--\n"));
    for f in ["PWNED", "PWNED2"] {
        assert!(
            !repo.join(f).exists() && !base.join(f).exists() && !std::path::Path::new(f).exists()
        );
    }
    c.kill(); // reaped before cleanup: no worker still writing into the temp dir
    std::fs::remove_dir_all(base).unwrap();
}

const FIXTURES: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../tests/fixtures/worker_protocol"
);
fn fixture(name: &str) -> serde_json::Value {
    serde_json::from_str(&std::fs::read_to_string(format!("{FIXTURES}/{name}.json")).unwrap())
        .unwrap()
}

#[test]
fn backend_golden_jobs_round_trip_as_values() {
    for name in ["key_new", "key_resume", "legacy_layout", "rejected_paths"] {
        let fx = fixture(name);
        let msg = &fx["message"];
        let parsed = serde_json::from_value::<KeyJob>(msg["job"].clone());
        if fx["expect"]["parse"] == "rejected" {
            // The Python side rejects with code "paths"; Rust must refuse it too (serde or validate).
            assert!(
                parsed
                    .and_then(|j| j.validate().map_err(serde::de::Error::custom))
                    .is_err(),
                "{name}"
            );
            continue;
        }
        let job = parsed.unwrap_or_else(|e| panic!("{name}: {e}"));
        job.validate().unwrap_or_else(|e| panic!("{name}: {e}"));
        assert_eq!(&serde_json::to_value(&job).unwrap(), &msg["job"], "{name}"); // nothing added/dropped/renamed
        assert_eq!(
            &serde_json::from_str::<serde_json::Value>(&Cmd::Key(&job).line()).unwrap(),
            msg,
            "{name}"
        );
    }
}

#[test]
fn backend_golden_events_and_commands_parse() {
    let fx = fixture("events");
    let evs = fx["events"].as_array().unwrap();
    let parsed: Vec<Event> = evs
        .iter()
        .map(|e| parse_line(&e.to_string()).unwrap_or_else(|m| panic!("{e}: {m}")))
        .collect();
    assert_eq!(parsed.len(), 11);
    assert!(
        matches!(&parsed[0], Event::Hello(h) if h.host == "render2" && h.models["corridorkey_v2"].sha256.as_deref().is_some_and(|d| d.len() == 64) && h.capabilities.key)
    );
    assert!(matches!(&parsed[3], Event::Rejected { job_id: None, code, .. } if code == "protocol"));
    assert!(
        matches!(&parsed[9], Event::Done(d) if d.outcome == Outcome::Complete && d.manifest_complete && d.published == 3)
    );
    assert!(
        matches!(&parsed[10], Event::Done(d) if d.outcome == Outcome::Cancelled && d.manifest.is_none())
    );
    let cmds = fx["commands"].as_array().unwrap();
    for (c, line) in cmds
        .iter()
        .zip([Cmd::Cancel("job-key-new").line(), Cmd::Shutdown.line()])
    {
        assert_eq!(
            c,
            &serde_json::from_str::<serde_json::Value>(&line).unwrap()
        );
    }
}

fn win(repo: &str, py: &str) -> Target {
    let mut t = ssh("winbox", None, repo, py);
    if let TargetKind::Ssh { os, .. } = &mut t.kind {
        *os = RemoteOs::Windows;
    }
    t
}
fn b64_decode_utf16(s: &str) -> String {
    const T: &str = "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let (mut bits, mut n, mut bytes) = (0u32, 0, vec![]);
    for c in s.trim_end_matches('=').chars() {
        bits = bits << 6 | T.find(c).expect("base64 alphabet") as u32;
        n += 6;
        if n >= 8 {
            n -= 8;
            bytes.push((bits >> n) as u8);
            bits &= (1 << n) - 1;
        }
    }
    String::from_utf16(
        &bytes
            .chunks(2)
            .map(|b| u16::from_le_bytes([b[0], b[1]]))
            .collect::<Vec<_>>(),
    )
    .unwrap()
}

#[test]
fn windows_target_uses_one_encoded_powershell_command() {
    assert_eq!(ps_encode("a"), "YQA=");
    assert_eq!(ps_encode("ab"), "YQBiAA==");
    let a = win(
        r"C:\Corridor Key\repo",
        r"C:\Corridor Key\.venv\Scripts\python.exe",
    )
    .ssh_args()
    .unwrap();
    assert_eq!(&a[a.len() - 3..a.len() - 1], ["winbox", "--"]);
    let cmd = a.last().unwrap();
    let b64 = cmd
        .strip_prefix("powershell.exe -NoLogo -NoProfile -NonInteractive -EncodedCommand ")
        .unwrap();
    assert!(b64
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || "+/=".contains(c))); // nothing a shell could interpret
    assert_eq!(
        b64_decode_utf16(b64),
        r"Set-Location -LiteralPath 'C:\Corridor Key\repo' -ErrorAction Stop; & 'C:\Corridor Key\.venv\Scripts\python.exe' -m backend.worker --gpu 1; exit $LASTEXITCODE"
    );
    // The Linux path is unchanged.
    assert!(ssh("h", None, "/r", "/p")
        .ssh_args()
        .unwrap()
        .last()
        .unwrap()
        .starts_with("cd -- '/r' && exec '/p'"));
}

#[test]
fn windows_command_generation_resists_injection() {
    // Quotes are doubled inside a single-quoted literal; `$()`, backticks, `;`, `&`, `%` stay inert text.
    let evil = r"C:\a'; calc.exe; '$(Remove-Item x)`n%PATH%&whoami";
    let script = b64_decode_utf16(
        remote_command(RemoteOs::Windows, evil, r"C:\py.exe", 0)
            .rsplit(' ')
            .next()
            .unwrap(),
    );
    let lit = ps_quote(evil);
    assert_eq!(
        lit,
        r"'C:\a''; calc.exe; ''$(Remove-Item x)`n%PATH%&whoami'"
    );
    assert!(script.contains(&format!("-LiteralPath {lit} ")), "{script}");
    // Outside the literal there is nothing but the fixed script.
    assert_eq!(
        script.replace(&lit, "<REPO>"),
        r"Set-Location -LiteralPath <REPO> -ErrorAction Stop; & 'C:\py.exe' -m backend.worker --gpu 0; exit $LASTEXITCODE"
    );
    // Targets that could break out of the literal, or are not absolute Windows paths, are refused outright.
    for (repo, py) in [
        ("C:\\a\u{2019}; calc", r"C:\py.exe"), // typographic quote ends a PowerShell literal
        ("C:\\a\nb", r"C:\py.exe"),
        ("repo", r"C:\py.exe"),
        ("C:/repo", r"C:\py.exe"),
        (r"C:\repo", "python"),
        (r"C:\repo", "-evil"),
        ("/home/me/repo", r"C:\py.exe"),
    ] {
        assert!(win(repo, py).validate().is_err(), "{repo:?} {py:?}");
    }
    assert!(win(r"\\nas\tools\repo", r"C:\py.exe").validate().is_ok()); // UNC repo is fine
}

#[test]
fn path_maps_are_validated_per_target_os() {
    let mut w = win(r"C:\repo", r"C:\py.exe");
    w.path_map = vec![
        ("/mnt/NAS".into(), r"Z:\Shared".into()),
        ("/mnt/Other".into(), r"\\nas\other".into()),
    ];
    assert!(w.validate().is_ok());
    for bad in [
        ("/mnt/NAS", "Z:/Shared"),
        ("/mnt/NAS", "/srv/nas"),
        ("relative", r"Z:\x"),
    ] {
        w.path_map = vec![(bad.0.into(), bad.1.into())];
        assert!(w.validate().is_err(), "{bad:?}");
    }
    let mut l = ssh("h", None, "/r", "/p");
    l.path_map = vec![("/mnt/NAS".into(), r"Z:\Shared".into())];
    assert!(l.validate().is_err()); // Windows-style root on a Linux target
    l.path_map = vec![(r"Z:\Shared".into(), "/mnt/NAS".into())]; // Windows UI, Linux worker
    assert!(l.validate().is_ok());
    // `os` defaults to linux for settings written before the field existed; windows round-trips.
    let t: Target = serde_json::from_value(json!({"id":"w","kind":"ssh","os":"windows","host":"w","python":r"C:\p.exe","repo":r"C:\r","gpu":0})).unwrap();
    assert!(matches!(
        t.kind,
        TargetKind::Ssh {
            os: RemoteOs::Windows,
            ..
        }
    ));
}

#[test]
fn command_lines_are_ascii_only_so_remote_code_pages_cannot_alter_them() {
    let mut j = job(std::path::Path::new("/mnt/NAS"), "a", 1, "ok");
    j.clip_id = "proj/clip é 日本 😀".into();
    let line = Cmd::Key(&j).line();
    assert!(line.is_ascii() && line.contains(r"\u00e9") && line.contains(r"\ud83d\ude00"));
    // Same JSON value after decoding.
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&line).unwrap()["job"]["clip_id"],
        "proj/clip é 日本 😀"
    );
}
