# Contributing to Keyloom

Thanks for your interest in improving Keyloom! Whether you're a VFX artist, a pipeline TD, or a developer, contributions of all kinds are welcome — bug reports, feature ideas, documentation fixes, and code.

## Legal Agreement

Keyloom is a GUI built on top of [Niko Pueringer's CorridorKey](https://github.com/nikopueringer/CorridorKey). By contributing to this project, you agree that your contributions will be licensed under the same terms as the upstream project's **[CorridorKey Licence](https://github.com/nikopueringer/CorridorKey/blob/main/LICENSE)**.

## Getting Started

### Prerequisites

- Python 3.11
- A virtual environment (`python -m venv .venv`)
- NVIDIA GPU and driver for inference; CPU-only fixture tests do not need models
- Rust/Cargo and the native build prerequisites in [rust-ui/README.md](rust-ui/README.md)
- Linux or Windows x86_64; macOS/MLX is not supported by this fork

### Dev Setup

```bash
git clone https://github.com/NickPittas/Keyloom.git
cd Keyloom
python -m venv .venv
.venv/Scripts/activate      # Windows
source .venv/bin/activate    # Linux
pip install -e ".[dev]"
```

### Running the App

The desktop app is the native Rust program in `rust-ui/` (`cargo build --release`, binary `keyloom-ui`); the Python code
under `backend/` is its worker and network service. See [native development](rust-ui/README.md) for build/run details. Installer builds require approval and use only `python packaging/build_release.py` on the target OS (see [packaging](packaging/README.md)); only online installers are distributed. Never install or launch an installer as part of a build.

### Running Tests

Run all tests through the isolated wrappers described in [rust-ui/AGENTS.md](rust-ui/AGENTS.md), never on a live desktop. From the repository root on Linux, for example:

```bash
REPO="$PWD"
bash "$REPO/rust-ui/tools/isolated-test.sh" "$REPO/.venv/bin/python" -m pytest "$REPO/tests" -m "not gpu"
bash "$REPO/rust-ui/tools/isolated-test.sh" /usr/bin/cargo test --offline
```

Use absolute interpreter and test paths. GPU/installed-app acceptance is separate from fixture tests and requires explicit approval. Follow [command monitoring and timing](AGENTS.md#command-monitoring-and-timing-gates-persistent-user-requirements) when executing commands.

### Linting and Formatting

```bash
ruff check                   # check for lint errors
ruff format --check          # check formatting
ruff format                  # auto-format
```

## Making Changes

### Pull Requests

1. Fork the repo and create a branch from `main`
2. Make your changes
3. Run tests and lint checks
4. Open a pull request against `main`

In your PR description, focus on **why** you made the change, not just what changed. If you're fixing a bug, describe the symptoms. If you're adding a feature, explain the use case.

### What Makes a Good Contribution

- **Bug fixes** — especially platform-specific issues (Windows and Linux), EXR/linear workflows, or color space handling
- **Tests** — more coverage is always welcome, particularly for the inference pipeline and clip management
- **Documentation** — better explanations, usage examples, or clarifying comments
- **Performance** — reducing VRAM usage, speeding up frame processing, or optimizing I/O
- **UI/UX** — improvements to the native Rust interface, accessibility, or workflow ergonomics

### Code Style

- [Ruff](https://docs.astral.sh/ruff/) for linting and formatting
- Line length: 120 characters
- Third-party model code in `gvm_core/`, `VideoMaMaInferenceModule/`, and `CorridorKeyModule/` is excluded from lint enforcement — those are kept close to their upstream research repos

### Model Weights

Model checkpoints (CorridorKey, GVM, VideoMaMa, MatAnyone2) are **not** in the git repo. Most tests don't need them. If you're working on inference code, follow the download instructions in the [README](README.md).

## Reporting Bugs

Open a [Keyloom GitHub issue](https://github.com/NickPittas/Keyloom/issues) with:

- OS and GPU info
- Steps to reproduce
- Expected vs actual behavior
- Relevant log output (check `logs/` directory)

## Security Vulnerabilities

Please **do not** open public issues for security vulnerabilities. See [SECURITY.md](SECURITY.md) for responsible disclosure instructions.

## Questions?

Use this repository's [issues](https://github.com/NickPittas/Keyloom/issues) for Keyloom questions and proposals. Upstream community links are for their respective projects, not a promise of support for this fork.
