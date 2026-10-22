# Implementation checkpoint — 2026-10-22

This checkpoint includes the native Rust workspace, import destination flow,
playback/settings work, Python AI worker, local/SSH scheduler, batch output
preview/versioning, recovery, and validation tooling.

Validation at commit time: full isolated Rust suite passed; 235 focused Python
tests passed (worker stdin/protocol, Key jobs, range inputs, FFmpeg helpers).
Local RTX 4090 and remote Linux RTX 5090 real-model smoke tests passed, including
NAS outputs and the Rust SSH transport. See remote-validation.md for provenance.

Windows remains in progress: the task-local stdin polling fix passed PNG GPU
jobs; the hardened shared implementation passes mocked regression tests but
still needs final Windows validation and independent review. EXR and Windows
NAS authentication are not yet established. Remote tests require explicit opt-in.

The proposed boot worker service, persistent coordinator and installer modes
are not implemented. The current scheduler is owned by the desktop app.
Hint generation, video-source inference and intra-clip distribution remain
deferred. No credentials, model weights, runtime projects or build outputs are
part of this checkpoint.
