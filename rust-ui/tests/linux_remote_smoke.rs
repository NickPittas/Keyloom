//! Manual, `#[ignore]`d (writes real output on a remote machine; opt in by setting every variable below): the real Rust `WorkerConn` + `Target::Ssh` against the Linux smoke worker, through
//! `tools/ck-linux-smoke-ssh.sh` (the real `ssh` over an authenticated control socket). Needs the network, so
//! it is NOT part of the isolated suite. Run after the control socket exists:
//!   cargo test --offline --test linux_remote_smoke -- --ignored --nocapture
//!   CK_SSH_SOCKET    ssh ControlMaster socket (already authenticated)
//!   CK_SMOKE_HOST    remote host / ip            CK_SMOKE_USER   remote user
//!   CK_SMOKE_DEPLOY  remote deployment dir holding repo/ and bin/python
//!   CK_SMOKE_CLIP    shared clip root (clips/clip1 of tests/gpu_smoke_key_job.py's fixture: stems
//!                    shot_1001.., frames 3..=9 valid), as seen by the remote
//!   CK_SMOKE_OUT     shared, writable, task-owned output dir (must not exist yet; created by the test)
//!   CK_SMOKE_SHA     expected corridorkey_v2 SHA-256
use ck::jobs::KeyJob;
use ck::worker::*;
use serde_json::json;
use std::process::Command;
use std::time::{Duration, Instant};

fn env(k: &str) -> String {
    std::env::var(k).unwrap_or_else(|_| panic!("set {k} (see the header of this file)"))
}
const FIRST: usize = 3;
const LAST: usize = 9;

fn target() -> Target {
    Target {
        id: "linux-smoke".into(),
        kind: TargetKind::Ssh {
            os: RemoteOs::Linux,
            host: env("CK_SMOKE_HOST"),
            user: Some(env("CK_SMOKE_USER")),
            port: None,
            python: format!("{}/bin/python", env("CK_SMOKE_DEPLOY")),
            repo: format!("{}/repo", env("CK_SMOKE_DEPLOY")),
            ssh: Some(concat!(env!("CARGO_MANIFEST_DIR"), "/tools/ck-linux-smoke-ssh.sh").into()),
        },
        gpu: 0,
        enabled: true,
        storage: Storage::Shared,
        path_map: vec![],
    }
}

fn job(id: &str, out: &str, mode: &str) -> KeyJob {
    let stems: Vec<String> = (0..12).map(|i| format!("shot_{}", 1001 + i)).collect();
    let outs = |o: &str| -> Vec<String> {
        stems[FIRST..=LAST]
            .iter()
            .map(|s| format!("{out}/{o}/{s}.exr"))
            .collect()
    };
    serde_json::from_value(json!({
        "job_id": id, "clip_id": "smoke/clip1", "clip_root": env("CK_SMOKE_CLIP"),
        "range": {"in_point": FIRST, "out_point": LAST, "in_stem": stems[FIRST], "out_stem": stems[LAST]},
        "params": {"input_is_linear": false, "use_alpha_hint": false, "hint_quality": 0.95, "carry_hint": true,
                   "carry_hint_quality": 0.95, "vram_profile": "auto", "despill_strength": 0.5,
                   "cutout_linear": false, "hint_mode": "none", "temporal_window": "auto"},
        "output_config": {"fg_enabled": true, "fg_format": "exr", "matte_enabled": true, "matte_format": "exr",
                          "comp_enabled": false, "comp_format": "png", "processed_enabled": false,
                          "processed_format": "exr", "exr_compression": "piz"},
        "hint_keyframes_abs": null, "outputs": {"fg": outs("Fg"), "matte": outs("Matte")},
        "manifest_dir": out, "mode": mode, "publish": "final", "path_map": [],
        "require": {"protocol": 1, "models": {"corridorkey_v2": env("CK_SMOKE_SHA")}}
    }))
    .unwrap()
}

fn recv_until(
    c: &mut WorkerConn,
    secs: u64,
    mut stop: impl FnMut(&mut WorkerConn, &Msg) -> bool,
) -> Vec<Msg> {
    let (mut got, end) = (vec![], Instant::now() + Duration::from_secs(secs));
    loop {
        match c.try_recv() {
            Some(m) => {
                let s = stop(c, &m);
                got.push(m);
                if s {
                    return got;
                }
            }
            None => {
                assert!(Instant::now() < end, "timed out; got {got:?}");
                std::thread::sleep(Duration::from_millis(10));
            }
        }
    }
}
fn done_or_end(_: &mut WorkerConn, m: &Msg) -> bool {
    matches!(
        m,
        Msg::Event(Event::Done(_) | Event::Rejected { .. }) | Msg::Protocol(_) | Msg::Exited { .. }
    )
}
fn last_done(ev: &[Msg]) -> Done {
    match ev.last() {
        Some(Msg::Event(Event::Done(d))) => d.clone(),
        o => panic!("expected done, got {o:?}"),
    }
}
fn remote(cmd: &str) -> String {
    let o = Command::new(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tools/ck-linux-smoke-ssh.sh"
    ))
    .args([
        "-o",
        "BatchMode=yes",
        &format!("{}@{}", env("CK_SMOKE_USER"), env("CK_SMOKE_HOST")),
        cmd,
    ])
    .output()
    .unwrap();
    String::from_utf8_lossy(&o.stdout).trim().to_string()
}
fn count_exr(out: &str) -> usize {
    remote(&format!("find '{out}' -name '*.exr' | wc -l"))
        .parse()
        .unwrap()
}
fn exit_of(c: &mut WorkerConn) -> Option<i32> {
    let ev = recv_until(c, 60, |_, m| matches!(m, Msg::Exited { .. }));
    match ev.last() {
        Some(Msg::Exited { code, .. }) => *code,
        _ => unreachable!(),
    }
}

#[test]
#[ignore]
fn linux_ssh_target_hello_job_cancel_eof_and_shutdown() {
    let root = env("CK_SMOKE_OUT");
    assert_eq!(
        remote(&format!("test -e '{root}' && echo exists")),
        "",
        "CK_SMOKE_OUT must not exist yet"
    );
    remote(&format!("mkdir -m 700 '{root}'"));
    let t = target();
    println!("ssh argv: {:?}", t.ssh_args().unwrap());
    let mut c = WorkerConn::spawn(&t).unwrap();
    let hello = recv_until(&mut c, 120, |_, m| matches!(m, Msg::Event(Event::Hello(_))));
    let Some(Msg::Event(Event::Hello(h))) = hello.last() else {
        unreachable!()
    };
    println!("hello: {h:?}");
    assert_eq!(h.gpu.as_ref().unwrap().index, 0);
    assert!(h.gpu.as_ref().unwrap().name.contains("5090"));

    // 1. job with the model hash required by the app: complete
    let out1 = format!("{}/v001", env("CK_SMOKE_OUT"));
    let j1 = job("rust-1", &out1, "new");
    c.send(&Cmd::Key(&j1)).unwrap();
    let d = last_done(&recv_until(&mut c, 180, done_or_end));
    println!("job1: {d:?}");
    assert_eq!(
        (d.outcome, d.published, d.expected),
        (Outcome::Complete, 7, 7)
    );
    assert_eq!(count_exr(&out1), 14);

    // 2. cancel after the first published frame, then resume to completion on the same connection
    let out2 = format!("{}/v002", env("CK_SMOKE_OUT"));
    c.send(&Cmd::Key(&job("rust-2", &out2, "new"))).unwrap();
    let ev = recv_until(&mut c, 180, |c, m| {
        if matches!(m, Msg::Event(Event::Progress { phase, done, .. }) if phase == "publish" && *done >= 2)
        {
            c.cancel("rust-2").unwrap();
        }
        done_or_end(c, m)
    });
    let d = last_done(&ev);
    println!("job2 cancel: {d:?}");
    assert_eq!(d.outcome, Outcome::Cancelled);
    assert!(d.published < 7);
    c.send(&Cmd::Key(&job("rust-3", &out2, "resume"))).unwrap();
    let d = last_done(&recv_until(&mut c, 180, done_or_end));
    println!("job3 resume: {d:?}");
    assert_eq!((d.outcome, d.published + d.skipped), (Outcome::Complete, 7));
    assert_eq!(count_exr(&out2), 14);

    // 3. EOF mid-job: request_stop() must cancel, release the lock and let the process exit
    let out3 = format!("{}/v003", env("CK_SMOKE_OUT"));
    c.send(&Cmd::Key(&job("rust-4", &out3, "new"))).unwrap();
    let ev = recv_until(&mut c, 180, |c, m| {
        if matches!(m, Msg::Event(Event::Progress { phase, done, .. }) if phase == "publish" && *done >= 1)
        {
            c.request_stop();
        }
        matches!(m, Msg::Exited { .. } | Msg::Protocol(_))
    });
    let Some(Msg::Exited { code, stderr }) = ev.last() else {
        panic!("{ev:?}")
    };
    println!(
        "EOF mid-job: exit={code:?}; stderr tail: {}",
        stderr.lines().last().unwrap_or("")
    );
    assert_eq!(*code, Some(0));
    let done = ev.iter().find_map(|m| {
        if let Msg::Event(Event::Done(d)) = m {
            Some(d.clone())
        } else {
            None
        }
    });
    println!("done before exit: {done:?}");
    assert_eq!(done.map(|d| d.outcome), Some(Outcome::Cancelled));
    assert_eq!(
        remote(&format!("ls -A '{out3}' | grep -c job.lock")),
        "0",
        "lock still present"
    );
    assert_eq!(
        remote("pgrep -fc '[b]ackend.worker --gpu 0'"),
        "0",
        "worker still running"
    );

    // 4. a fresh connection: explicit shutdown command
    let mut c = WorkerConn::spawn(&t).unwrap();
    recv_until(&mut c, 120, |_, m| matches!(m, Msg::Event(Event::Hello(_))));
    c.send(&Cmd::Shutdown).unwrap();
    assert_eq!(exit_of(&mut c), Some(0));
    assert_eq!(remote("pgrep -fc '[b]ackend.worker --gpu 0'"), "0");
}
