# Network coordinator (protocol v1) — control-plane foundation

**User setup is in Keyloom → Clients:** every installation hosts automatically; press Rescan to list PCs (name + addresses) and tick the ones to connect, Start/Stop/Remove/Refresh the service. Joining a network is done from that page. See [the network guide](../../docs/network-rendering.md). The sections below document the developer CLI and protocol.

Python stdlib `ssl`/`http.server`/`sqlite3` plus `cryptography`; no new dependency.
The coordinator records who owns which attempt. It never runs a worker, never resolves a client path and never
reuses `Storage::Transfer`. The Rust `Scheduler` stays the only executor (integration later).

Run (opt-in, loopback by default, explicit paths only — see `python -m backend.coordinator --help`):
the operator commands below. Joining is `/v1/join`, called from the Clients page. One process owns a state dir (lock file).
State dir must be **local disk** (SQLite WAL), not a NAS.

## Trust and credentials
Trust note: the certificate and fingerprint a joining PC receives only prove each other are consistent; they do not authenticate the server. Trust comes from comparing the fingerprint the server shows with the one you expect. The certificate is valid 365 days; there is no renewal/rotation yet (regenerate by hand and re-join).
Join credentials (private file, 0600): URL, certificate PEM, SHA-256 fingerprint. The client verifies the fingerprint offline, then uses that
certificate as the sole anchor (CERT_REQUIRED, no host-name check) and compares the leaf SHA-256 right after every handshake,
before any request byte. The identity is the pinned certificate, not an address, so the same identity works over LAN, Tailscale,
ZeroTier and loopback and survives address changes with no re-keying; SANs are cosmetic (current interfaces + loopback at creation)
and an existing certificate/key/token is never regenerated.
Tokens: 256-bit, per installation, server stores SHA-256 only, constant-time compare, revocable (`Store.revoke`).
Credential/key files: created `O_EXCL` (never overwritten), 0600 verified on every read; Windows uses `icacls`
owner-only ACL read back (logic unit-tested with a fake runner only — **not exercised on real Windows**).
Tokens/codes never appear in argv, stdout or logs. 10 bad-auth attempts/min/IP → 429 (a NAT'd attacker can lock out
its own peers; accepted on a LAN).

## API (all JSON, ≤1 MiB, `Authorization: Bearer <installation_id>.<token>`, one request per connection)
`POST /v1/join` · `/v1/session` · `/v1/jobs` (submit) · `/v1/claim` · `/v1/attempts/{id}/(reject|start|events)` ·
`/v1/jobs/{id}/cancel` · `/v1/stats` · `GET /v1/jobs/{id}` · `/v1/jobs/{id}/events?after=&limit=` · `/v1/installations`.
No long-poll: clients poll. Claim/start/reject/events/stats need the installation's current `session_id`.
`/v1/session` accepts an optional `name` (1-64 characters, no control characters) that renames the same installation row; it never creates an identity or token, and callers that omit it keep their stored name. A PC joined to several networks sends its one name to each host.
The service status (`netlink service_status`) lists `certificate_addresses`; an existing certificate is never regenerated (joined PCs pin it), so a `cert_mismatch` is fixed by hosting on a covered address.

## Addressing, hello and discovery
- `GET /v1/hello` (unauthenticated, read-only, no secrets, rate-capped): `{protocol, fingerprint_sha256, network_id, name, host_installation_id, receiver_online, join, endpoints[{url,kind}] (<=16)}`.
  `receiver_online` is the host PC's own app session being online (service coordinator only; receiving needs the app/background mode, not the service alone).
- `POST /v1/local-identity` (loopback + private token): tells the host which existing installation is this PC (written to `local-installation.json`); `/v1/local-join` records it automatically.
- The server listens dual-stack (IPv6 wildcard with IPv4 fallback) on one of 8443..8452 (existing port kept) on all interfaces. Interfaces are re-read live every 30 s.
- Clients hold an ordered endpoint list (last working first). Fail-over happens only on connect / TLS / pin failure before any request byte; a sent request is never retried. Per-address connect is <= 3 s and one call (incl. body) <= its timeout (15 s).
- UDP discovery listens on the FIXED port 44554 (independent of the API port) and answers a probe with a unicast datagram <= 1200 bytes, rate-limited. Datagrams are hints: every candidate is verified by a handshake fingerprint match plus a pinned hello. The scan (`netlink discover`) also reads Tailscale peers, the OS neighbour table and known URLs, probes 8443..8452 only on those candidates (<= 512, <= 32 workers, hard 20 s), and reports `sources` / `partial` instead of claiming full coverage. Two PCs may share a name: rows are keyed by full fingerprint, never by name.
- Service config `service.json` keeps the strict v1 shape (`address` is cosmetic); PC name lives in `service-meta.json`; `service_configure` changes policy/name live. Remove writes `host-removed.marker`; only Start clears it.

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
- **Policy**: submitter or admin installation may read/cancel a job; others get 404.
  Any joined installation may list `/v1/installations`.
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
- Join failures are distinct: `paired_not_saved` (login issued but not saved: Join again from the Clients page) and `pair_uncertain` (transport failure; a login MAY have been issued; never auto-retried).

Limits: ≤32 concurrent handlers (extra connections dropped), 15 s per-read I/O timeout, and a hard 30 s deadline per connection (TLS handshake + headers + body + reply; the socket is shut down by a timer), ≤2000 active jobs.
Receiver-reported `effective_plan` (≤8 KiB) is stored but **not validated** by the server.
