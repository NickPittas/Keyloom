# Network coordinator (protocol v1) — control-plane foundation

**User setup is in Keyloom → Clients:** install/start/stop the per-user host service, join by IP/port, or create/use an optional reusable invitation. See [the network guide](../../docs/network-rendering.md). The sections below document the developer CLI and protocol.

Python stdlib `ssl`/`http.server`/`sqlite3` plus `cryptography`; no new dependency.
The coordinator records who owns which attempt. It never runs a worker, never resolves a client path and never
reuses `Storage::Transfer`. The Rust `Scheduler` stays the only executor (integration later).

Run (opt-in, loopback by default, explicit paths only — see `python -m backend.coordinator --help`):
`serve --state-dir D [--host --port --advertise-host H --invite-out F --invite-admin]`,
`pair --invitation F --credentials F --name N` (the destination is reserved — new, private, verified — *before* the one-use code is redeemed),
and the operator commands below. One process owns a state dir (lock file). State dir must be
**local disk** (SQLite WAL), not a NAS. The UI creates reusable 7-day invitations without restarting the host; CLI one-use invitations retain their original behavior.

## Trust and credentials
Trust note: the certificate and fingerprint inside an invitation only prove each other are consistent; they do not authenticate the server. Trust comes from delivering the invitation file over a channel you trust, optionally comparing the fingerprint `serve` prints (and `pair` prints) with the one you expect. The certificate is valid 365 days; there is no renewal/rotation yet (regenerate by hand and re-pair).
Invitation (private file, 0600): URL, certificate PEM, SHA-256 fingerprint, one-time code (10 min, ~50 bit,
hash stored, 10 failures/min/IP and 30/5 min global throttle). The client verifies the fingerprint offline, then uses that
certificate as the sole anchor with hostname/SAN checking (SAN = explicit advertised hosts; wildcard bind refused).
Tokens: 256-bit, per installation, server stores SHA-256 only, constant-time compare, revocable (`Store.revoke`).
Credential/key files: created `O_EXCL` (never overwritten), 0600 verified on every read; Windows uses `icacls`
owner-only ACL read back (logic unit-tested with a fake runner only — **not exercised on real Windows**).
Tokens/codes never appear in argv, stdout or logs. 10 bad-auth attempts/min/IP → 429 (a NAT'd attacker can lock out
its own peers; accepted on a LAN).

## API (all JSON, ≤1 MiB, `Authorization: Bearer <installation_id>.<token>`, one request per connection)
`POST /v1/pair` · `/v1/session` · `/v1/jobs` (submit) · `/v1/claim` · `/v1/attempts/{id}/(reject|start|events)` ·
`/v1/jobs/{id}/cancel` · `/v1/stats` · `GET /v1/jobs/{id}` · `/v1/jobs/{id}/events?after=&limit=` · `/v1/installations`.
No long-poll: clients poll. Claim/start/reject/events/stats need the installation's current `session_id`.

- **Submit**: `submission_id` + frozen `spec` + `target` (installation id or null=automatic). Same id+same canonical
  payload → original response; different payload → 409. Spec is an exact schema (`schema.validate_spec`): kind `key` (Key only, the
  original payload, unchanged) or kind `workflow` (the same fields plus optional frozen `hint`/`hint_stages`/`export`:
  Hint → Key → Deliver on ONE receiver, which rebuilds the stages on its own clip); mode `new` only; Resume/Replace/other
  kinds → 400. A receiver advertises the kinds it supports in claim `capabilities.kinds`; a `workflow` job is never
  offered to a Key-only (older) build. `GET job` on a queued job lists `ineligible` receivers with the concrete reason. Logical `shared:` refs only; no `path_map`; output = one location
  (root+templates+values) the receiver expands; `params`/`output_config` mirror `InferenceParams`/`OutputConfig`.
  `job_id` is logical and coordinator-allocated; `attempt_id` is allocated at **claim**.
- **Claim**: capabilities (kinds/maps/models) prefilter only; receiver preflight is final. A same-`claim_id` retry returns
  the same offer. `resources` (`sampled_at`, free VRAM/RAM) is recorded with the attempt but never used for placement,
  and may be `null` (no report). Rejected (offered only) → requeued for other clients (excluded for the rejecter);
  targeted → job fails.
- **Ownership**: unique active attempt per GPU slot and per job. Only the owning installation's current session may
  start/report; foreign/unknown attempt → 404. Offered attempts can be cancelled/revoked; a late start → 409.
  Started attempts stay fenced forever — no timer or restart reassigns them; only a receiver `terminal` event ends one.
  Cancel of a started job only sets `cancel_requested` (returned on start/events responses).
- **Events**: `seq` contiguous from 1 per attempt; replay with identical content is a no-op, different content 409,
  gap 409; batch ≤256, event ≤16 KiB, atomic; `terminal` must be last. Submitter reads with a persisted cursor.
- **Policy**: submitter or admin installation (invited with `--invite-admin`) may read/cancel a job; others get 404.
  Any paired installation may list `/v1/installations`.
- **GPU slot**: `installation_id:gpu_uuid`; one GPU per installation; a GPU UUID registered by another live installation
  → 409. The client must supply the verified device UUID (respect `CUDA_VISIBLE_DEVICES`; not an NVML index).
- **Stats** (`POST /v1/stats`, in-memory latest only, no history, works while idle, counts as heartbeat): `seq`,
  `measured_age_ms` (≤60 s; client sends age, never a timestamp), optional `cpu_percent`, `ram_used_bytes`,
  `ram_total_bytes`, `gpus[{uuid,index,name,utilization_percent?,vram_used_bytes?,vram_total_bytes?}]`. Absent/null =
  unavailable (never 0). `/v1/installations` returns `connected`, `last_seen_age_s` and `stats` with server-computed
  `measured_age_s`/`received_age_s`; `stats` is `null` when nothing was reported this session.

## Operator commands (server must be STOPPED; they take the same state-dir lock and refuse while `serve` runs)
- `revoke --state-dir D <installation_id>`: revokes credentials/session, requeues its *unstarted* offers, frees its GPU
  identity for a reinstall. STARTED attempts stay fenced and are listed; a new installation cannot register that GPU
  while the old one has an offered/started attempt.
- `abandon --state-dir D --reason TEXT --i-understand-writer-not-stopped <attempt_id>`: marks a lost STARTED attempt and
  its job `failed` (reason recorded). It never requeues, stops no process, releases no storage lock and proves nothing
  about the writer; inspect the machine/NAS first. The receiver's later events then get 409 `attempt_terminal`.

## Offers that are never started
Every transaction first revokes (compare-and-set on `offered` only) offers whose owner session has been silent more than
`SESSION_TTL` (120 s) and requeues the job (targeted jobs wait for their target). The late owner's `start` gets 409
`attempt_not_offered`; replaying its old `claim_id` returns that attempt with `state: "revoked"`. Started attempts are never expired.

## Receiver contract (for the future link integration)
- Execute **only** after `start` returns `state == "started"`; any other state (including a claim retry showing `revoked`) means do not run.
- Run the frozen **offer spec** exactly. `effective_plan` is display/audit only: the server neither validates nor returns a modified plan.
- Keep durable local state for every attempt you started; never send events for attempts you have no record of.
- One receiving process per installation (hold a local lock). A new session id takes over only after 120 s of silence (earlier → 409
  `session_conflict`), and takeover is **not** proof the old process stopped.
- Receiver preflight is mandatory and final: map every `shared:` root locally, validate the whole mapping list, check containment for
  writes, sanitize mapped source stems/output names, refuse to overwrite existing output, and refuse external-source clips (jobs are
  self-contained Frames/Input/AlphaHint under the mapped clip root). `KeyJob.path_map` stays empty.
- Heartbeat (stats or any call) at least every ~60 s while holding an offer.
- Credential/invitation/key files are re-checked on EVERY read: POSIX mode 0600; Windows owner-only ACL via `icacls` (fails closed if it cannot be read). The Windows path is exercised only with a mocked `icacls` — **not verified on real Windows, so Windows pilot readiness is unproven**. No credential rotation yet.
- Pairing failures are distinct: `paired_not_saved` (code spent, token lost: revoke + new invitation) and `pair_uncertain` (transport failure; the code MAY be spent; never auto-retried).

Limits: ≤32 concurrent handlers (extra connections dropped), 15 s per-read I/O timeout, and a hard 30 s deadline per connection (TLS handshake + headers + body + reply; the socket is shut down by a timer), ≤2000 active jobs.
Receiver-reported `effective_plan` (≤8 KiB) is stored but **not validated** by the server.
