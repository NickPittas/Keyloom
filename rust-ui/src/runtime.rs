//! Installed-runtime descriptor: the ONE JSON contract shared with `backend/install_paths.py` (see
//! docs/design/rust-ui/install-descriptor.md). A packaged install is relocatable: every root in the
//! descriptor resolves against the descriptor's own folder at run time, so the install tree can be
//! moved whole. A developer checkout keeps working unchanged (`--repo`, `CK_REPO`, `CK_PYTHON`,
//! executable/CWD ancestor discovery); a descriptor, once present, wins over ancestor discovery, and
//! a broken descriptor disables the runtime actions with an actionable message instead of silently
//! falling back to some developer checkout. Nothing here writes inside the (possibly read-only)
//! app root.
use serde_json::Value;
use std::path::{Component, Path, PathBuf};

/// Descriptor file name, identical in both languages.
pub const DESCRIPTOR_NAME: &str = "keyloom-install.json";
/// The descriptor in `dir`.
pub fn descriptor_in(dir: &Path) -> PathBuf {
    dir.join(DESCRIPTOR_NAME)
}
/// Explicit pointer at a descriptor file or its folder; also exported to child processes.
pub const ENV_INSTALL: &str = "CK_INSTALL";
/// Explicit developer checkout (validated: must contain `backend/worker`).
pub const ENV_REPO: &str = "CK_REPO";
const SCHEMA: u64 = 1;
const TOP_KEYS: [&str; 4] = ["schema", "app_name", "app_version", "roots"];
const ROOT_KEYS: [&str; 6] = [
    "python",
    "helper",
    "models",
    "cache",
    "scratch",
    "user_data",
];

/// A validated, resolved install. Paths are absolute and lexically normalized (no symlink
/// resolution, so a relocated tree stays coherent); `None` roots fall back to platform defaults.
#[derive(Clone, Debug, PartialEq)]
pub struct Install {
    pub descriptor: PathBuf,
    /// The descriptor's folder: the anchor every relative root resolves against.
    pub app_root: PathBuf,
    pub python: PathBuf,
    /// The repo-like tree the worker and import helper run from (`backend/`, `rust-ui/tools/`).
    pub helper: PathBuf,
    pub models: Option<PathBuf>,
    pub cache: Option<PathBuf>,
    pub scratch: Option<PathBuf>,
    pub user_data: Option<PathBuf>,
}

/// Descriptor discovery outcome. `Broken` never falls back: the file was found, so this IS an
/// installed app whose descriptor must be fixed (or the app updated).
#[derive(Clone, Debug, PartialEq)]
pub enum Found {
    Install(Install),
    Missing,
    Broken { path: PathBuf, problem: String },
}

/// What import and AI launching use, after precedence. `python = None` keeps the developer
/// semantics (`CK_PYTHON`, then `<repo>/.venv`).
#[derive(Clone, Debug, Default)]
pub struct Launch {
    pub repo: Option<PathBuf>,
    pub python: Option<PathBuf>,
    pub install: Option<Install>,
    pub problem: Option<String>,
}

/// Lexical normalization only (`a/./b/../c` → `a/c`); mirrors Python `os.path.normpath`.
fn normalize(p: &Path) -> PathBuf {
    let mut out: Vec<Component> = vec![];
    for c in p.components() {
        match c {
            Component::CurDir => {}
            Component::ParentDir => {
                if matches!(out.last(), Some(Component::Normal(_))) {
                    out.pop();
                }
            }
            other => out.push(other),
        }
    }
    let mut pb = PathBuf::new();
    for c in out {
        pb.push(c.as_os_str());
    }
    if pb.as_os_str().is_empty() {
        pb.push(".");
    }
    pb
}

fn absolute(p: &Path) -> PathBuf {
    match std::path::absolute(p) {
        Ok(a) => normalize(&a),
        Err(_) => normalize(p),
    }
}

fn str_field(roots: &Value, key: &str, path: &Path) -> Result<Option<String>, String> {
    match roots.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(s)) if !s.trim().is_empty() => Ok(Some(s.clone())),
        Some(_) => Err(format!(
            "roots.{key} must be a non-empty path or null in {}",
            path.display()
        )),
    }
}

/// Parse and resolve a descriptor. Every error names the file and the field, and says what to do.
pub fn load(path: &Path) -> Result<Install, String> {
    let path = absolute(path);
    let bytes = std::fs::read(&path).map_err(|e| format!("cannot read {}: {e}", path.display()))?;
    let raw: Value = serde_json::from_slice(&bytes)
        .map_err(|e| format!("invalid JSON in {}: {e}", path.display()))?;
    let where_ = || path.display().to_string();
    if let Some(obj) = raw.as_object() {
        let unknown: Vec<String> = obj
            .keys()
            .filter(|k| !TOP_KEYS.contains(&k.as_str()))
            .cloned()
            .collect();
        if !unknown.is_empty() {
            return Err(format!(
                "unknown field {:?} in {w}: this Keyloom is older than the install; update the app (or point {ENV_INSTALL} at a matching install).",
                unknown.join(", "),
                w = where_()
            ));
        }
    }
    match raw.get("schema").and_then(Value::as_u64) {
        Some(SCHEMA) => {}
        Some(v) => {
            return Err(format!(
                "schema version {v} in {w}, this build reads {SCHEMA}: the install was made by a newer Keyloom; update the app (or point {ENV_INSTALL} at a matching install).",
                w = where_()
            ))
        }
        None => {
            return Err(format!(
                "missing or non-integer schema in {w} (expected {SCHEMA}); reinstall or repair the installation.",
                w = where_()
            ))
        }
    }
    for k in ["app_name", "app_version"] {
        if let Some(v) = raw.get(k) {
            if !v.is_string() {
                return Err(format!("{k} must be text in {w}", w = where_()));
            }
        }
    }
    let roots = raw
        .get("roots")
        .and_then(Value::as_object)
        .ok_or_else(|| format!("missing roots object in {w}", w = where_()))?;
    let unknown_roots: Vec<&str> = roots
        .keys()
        .filter(|k| !ROOT_KEYS.contains(&k.as_str()))
        .map(String::as_str)
        .collect();
    if !unknown_roots.is_empty() {
        return Err(format!(
            "unknown roots field {:?} in {w}: this Keyloom is older than the install; update the app.",
            unknown_roots.join(", "),
            w = where_()
        ));
    }
    let app_root = path
        .parent()
        .map(Path::to_path_buf)
        .ok_or_else(|| format!("descriptor path has no folder: {w}", w = where_()))?;
    let resolve = |v: Option<String>| -> Option<PathBuf> {
        v.map(|s| {
            let p = PathBuf::from(s);
            if p.is_absolute() {
                normalize(&p)
            } else {
                normalize(&app_root.join(p))
            }
        })
    };
    let python = str_field(&raw["roots"], "python", &path)?;
    let helper = str_field(&raw["roots"], "helper", &path)?;
    let (python, helper) = match (python, helper) {
        (Some(p), Some(h)) => (p, h),
        _ => {
            return Err(format!(
                "roots.python and roots.helper are required in {w}; reinstall or repair the installation.",
                w = where_()
            ))
        }
    };
    Ok(Install {
        descriptor: path.clone(),
        app_root: app_root.clone(),
        python: resolve(Some(python)).unwrap(),
        helper: resolve(Some(helper)).unwrap(),
        models: resolve(str_field(&raw["roots"], "models", &path)?),
        cache: resolve(str_field(&raw["roots"], "cache", &path)?),
        scratch: resolve(str_field(&raw["roots"], "scratch", &path)?),
        user_data: resolve(str_field(&raw["roots"], "user_data", &path)?),
    })
}

fn first_descriptor(starts: &[PathBuf]) -> Option<PathBuf> {
    starts.iter().find_map(|s| {
        s.ancestors()
            .take(4)
            .map(descriptor_in)
            .find(|p| p.is_file())
    })
}

/// Pure core of [`discover`]: `env` is the `CK_INSTALL` value, `starts` the folders (binary location,
/// …) whose ancestors may hold an install.
pub fn discover_with(env: Option<PathBuf>, starts: &[PathBuf]) -> Found {
    if let Some(v) = env {
        let p = absolute(&v);
        let cand = if p.is_file() {
            p.clone()
        } else {
            descriptor_in(&p)
        };
        if !cand.is_file() {
            return Found::Broken {
                path: p,
                problem: format!(
                    "{ENV_INSTALL} points at {}, but there is no {DESCRIPTOR_NAME} there; fix {ENV_INSTALL} or unset it.",
                    cand.parent().map(|d| d.display().to_string()).unwrap_or_default()
                ),
            };
        }
        return match load(&cand) {
            Ok(i) => Found::Install(i),
            Err(e) => Found::Broken {
                path: cand,
                problem: e,
            },
        };
    }
    match first_descriptor(starts) {
        Some(p) => match load(&p) {
            Ok(i) => Found::Install(i),
            Err(e) => Found::Broken {
                path: p,
                problem: e,
            },
        },
        None => Found::Missing,
    }
}

/// `CK_INSTALL`, then the executable's folder ancestors (never the CWD: a packaged release must not
/// depend on where it was launched from, and an unrelated folder must not be picked up).
pub fn discover() -> Found {
    discover_with(
        std::env::var_os(ENV_INSTALL).map(PathBuf::from),
        &exe_dirs(),
    )
}

fn exe_dirs() -> Vec<PathBuf> {
    std::env::current_exe().ok().into_iter().collect()
}

fn is_checkout(p: &Path) -> bool {
    p.join("backend/worker/__main__.py").is_file()
}

/// The pre-descriptor developer discovery, unchanged: ancestors of the executable, then of the CWD.
pub fn checkout_discovery(starts: &[PathBuf], cwd: Option<&Path>) -> Option<PathBuf> {
    starts
        .iter()
        .map(PathBuf::as_path)
        .chain(cwd)
        .find_map(|s| {
            s.ancestors()
                .take(6)
                .find(|a| is_checkout(a))
                .map(Path::to_path_buf)
        })
}

/// Pure core of [`launch`] (tests drive precedence without touching the process environment).
pub fn launch_with(
    cli: Option<&Path>,
    env_repo: Option<PathBuf>,
    found: Found,
    discovery_starts: &[PathBuf],
    cwd: Option<&Path>,
) -> Launch {
    let (install, problem) = match found {
        Found::Install(i) => (Some(i), None),
        Found::Broken { path, problem } => (
            None,
            Some(format!("{problem} (descriptor: {})", path.display())),
        ),
        Found::Missing => (None, None),
    };
    if let Some(r) = cli {
        return Launch {
            repo: Some(absolute(r)),
            python: None,
            install,
            problem,
        };
    }
    if let Some(r) = env_repo.filter(|p| is_checkout(p)) {
        return Launch {
            repo: Some(absolute(&r)),
            python: None,
            install,
            problem,
        };
    }
    // A found-but-broken descriptor is terminal: no descriptor → no checkout fallback either.
    if problem.is_some() {
        return Launch {
            repo: None,
            python: None,
            install,
            problem,
        };
    }
    if let Some(i) = &install {
        return Launch {
            repo: Some(i.helper.clone()),
            python: Some(i.python.clone()),
            install,
            problem,
        };
    }
    Launch {
        repo: checkout_discovery(discovery_starts, cwd),
        python: None,
        install,
        problem,
    }
}

/// Resolve what import and AI launching use. Precedence: explicit `--repo`/caller override, then a
/// valid `CK_REPO`, then the install descriptor, then developer checkout discovery. A broken
/// descriptor yields `repo = None` + `problem`: never a silent fallback.
pub fn launch(cli: Option<&Path>) -> Launch {
    launch_with(
        cli,
        std::env::var_os(ENV_REPO).map(PathBuf::from),
        discover(),
        &exe_dirs(),
        std::env::current_dir().ok().as_deref(),
    )
}

/// Launch with an explicit override (import helper's `repo` argument): the override wins, but a
/// valid install is still discovered so child processes get `CK_INSTALL`.
pub fn launch_explicit(over: &Path) -> Launch {
    launch_with(Some(over), None, discover(), &[], None)
}

/// Default location for new projects when nothing was chosen: the install's user-data root, else a
/// discovered checkout (Qt-like), else the platform user area. Never a build-time path.
pub fn default_data_dir(l: &Launch) -> Option<PathBuf> {
    match &l.install {
        // An installed app's repo is its read-only packaged helper root: never a place for projects.
        Some(i) => i.user_data.clone().or_else(platform_user_data),
        None => l.repo.clone().or_else(platform_user_data),
    }
}

/// `CK_INSTALL` to export to a child process (worker, import helper, setup helper) so the Python
/// side resolves the same roots; `None` when there is no valid install or the caller already set it.
pub fn child_env(install: Option<&Install>) -> Option<(&'static str, PathBuf)> {
    let i = install?;
    if std::env::var_os(ENV_INSTALL).is_some() {
        return None;
    }
    Some((ENV_INSTALL, i.descriptor.clone()))
}

/// Python variables a host application (Nuke, Resolve, a DCC's launcher...) injects and that would make an
/// installed interpreter import foreign modules.
pub const POLLUTING_ENV: [&str; 3] = ["PYTHONPATH", "PYTHONHOME", "PYTHONSTARTUP"];

/// Environment for every Python child (worker, import helper, model helper). With an installation descriptor:
/// `CK_INSTALL` is exported, inherited `PYTHONPATH`/`PYTHONHOME`/`PYTHONSTARTUP` are removed and the user site
/// is disabled, so the bundled modules always win. Without one (developer checkout, `CK_REPO`, explicit
/// repo) the caller's environment is left alone on purpose. `-I` is deliberately NOT used: `-m backend...`
/// needs the app folder on `sys.path`, which isolated mode would drop.
pub fn prepare_python_command(c: &mut std::process::Command, install: Option<&Install>) {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        c.creation_flags(0x0800_0000); // GUI helpers must not create console windows.
    }
    if let Some((k, v)) = child_env(install) {
        c.env(k, v);
    }
    if install.is_some() {
        for k in POLLUTING_ENV {
            c.env_remove(k);
        }
        c.env("PYTHONNOUSERSITE", "1");
    }
}

fn nonempty(v: Option<std::ffi::OsString>) -> Option<PathBuf> {
    v.filter(|s| !s.is_empty()).map(PathBuf::from)
}

/// `base/Keyloom`.
fn user_area(base: PathBuf) -> PathBuf {
    base.join("Keyloom")
}

/// Unix user area for app data (not settings): `$XDG_DATA_HOME` or `~/.local/share`, then `Keyloom`.
pub fn unix_user_data(
    xdg: Option<std::ffi::OsString>,
    home: Option<std::ffi::OsString>,
) -> Option<PathBuf> {
    nonempty(xdg)
        .map(user_area)
        .or_else(|| nonempty(home).map(|h| user_area(h.join(".local/share"))))
}

/// Windows user area, no `HOME` involved: `%APPDATA%` (roaming), else `%LOCALAPPDATA%`, then `Keyloom`.
pub fn windows_user_data(
    appdata: Option<std::ffi::OsString>,
    local: Option<std::ffi::OsString>,
) -> Option<PathBuf> {
    nonempty(appdata)
        .or_else(|| nonempty(local))
        .map(user_area)
}

pub fn platform_user_data() -> Option<PathBuf> {
    if cfg!(windows) {
        windows_user_data(
            std::env::var_os("APPDATA"),
            std::env::var_os("LOCALAPPDATA"),
        )
    } else {
        unix_user_data(std::env::var_os("XDG_DATA_HOME"), std::env::var_os("HOME"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The shared schema fixture (parsed by Rust AND Python tests: no contract drift).
    const FIXTURE: &str = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/install/",
        "keyloom-install.json"
    );

    fn tmp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("ck-runtime-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn descriptor_and_user_area_use_keyloom_names() {
        let d = tmp("keyloom-names");
        std::fs::write(d.join(DESCRIPTOR_NAME), "{}").unwrap();
        assert_eq!(descriptor_in(&d), d.join(DESCRIPTOR_NAME));
        let base = tmp("keyloom-area");
        assert_eq!(user_area(base.clone()), base.join("Keyloom"));
        std::fs::create_dir_all(base.join("CorridorKey")).unwrap();
        assert_eq!(user_area(base.clone()), base.join("Keyloom"));
    }

    /// An install layout built from the shared fixture; `(root, install_dir)` where `install_dir` is
    /// where the descriptor lives. `relocate_to` moves the whole tree first.
    fn layout(name: &str, relocate_to: Option<&str>) -> (PathBuf, PathBuf) {
        let root = tmp(name);
        let install = root.join(relocate_to.unwrap_or("install"));
        std::fs::create_dir_all(&install).unwrap();
        std::fs::copy(FIXTURE, install.join(DESCRIPTOR_NAME)).unwrap();
        // Marker files prove resolution only reads the descriptor; nothing needs to exist.
        std::fs::create_dir_all(install.join("app/backend/worker")).unwrap();
        std::fs::write(install.join("app/backend/worker/__main__.py"), "").unwrap();
        std::fs::create_dir_all(root.join("var-cache")).unwrap();
        (root, install)
    }

    #[test]
    fn shared_fixture_resolves_and_relocates() {
        let (root, install) = layout("fixture", None);
        let i = load(&install.join(DESCRIPTOR_NAME)).unwrap();
        assert_eq!(i.app_root, install);
        assert_eq!(i.helper, install.join("app"));
        assert_eq!(i.python, install.join("runtime/python/bin/python3"));
        assert_eq!(i.models, Some(install.join("models")));
        assert_eq!(i.cache, Some(root.join("var-cache"))); // "../var-cache" escapes app root lexically
        assert_eq!(i.scratch, None);
        assert_eq!(i.user_data, None);
        // Relocate the whole tree and resolve again from an unrelated place: roots track the move.
        let moved = root
            .parent()
            .unwrap()
            .join(format!("ck-runtime-moved-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&moved);
        std::fs::rename(&root, &moved).unwrap();
        let m = moved.join("install");
        let i = load(&m.join(DESCRIPTOR_NAME)).unwrap();
        assert_eq!(i.helper, m.join("app"));
        assert_eq!(i.cache, Some(moved.join("var-cache")));
        // Discovery from a nested binary folder (like <install>/bin/app.exe) finds it via ancestors.
        let bin = m.join("bin");
        std::fs::create_dir_all(&bin).unwrap();
        match discover_with(None, &[bin]) {
            Found::Install(d) => assert_eq!(d.descriptor, m.join(DESCRIPTOR_NAME)),
            f => panic!("{f:?}"),
        }
        let _ = std::fs::remove_dir_all(&moved);
    }

    #[test]
    fn descriptor_errors_name_the_field_and_the_fix() {
        let root = tmp("errors");
        let w = |body: &str| {
            let p = root.join(DESCRIPTOR_NAME);
            std::fs::write(&p, body).unwrap();
            p
        };
        for (body, want) in [
            (
                r#"{"schema":2,"roots":{"python":"p","helper":"h"}}"#,
                "schema version 2",
            ),
            (
                r#"{"roots":{"python":"p","helper":"h"}}"#,
                "missing or non-integer schema",
            ),
            (
                r#"{"schema":1,"future":1,"roots":{"python":"p","helper":"h"}}"#,
                "unknown field \"future\"",
            ),
            (
                r#"{"schema":1,"roots":{"python":"p","helper":"h","wat":"x"}}"#,
                "unknown roots field \"wat\"",
            ),
            (
                r#"{"schema":1,"roots":{"helper":"h"}}"#,
                "roots.python and roots.helper are required",
            ),
            (
                r#"{"schema":1,"roots":{"python":"p","helper":"h","cache":""}}"#,
                "roots.cache must be a non-empty path or null",
            ),
            ("{nope", "invalid JSON"),
            (
                r#"{"schema":1,"roots":{"python":null,"helper":"h"}}"#,
                "roots.python and roots.helper are required",
            ),
        ] {
            let err = load(&w(body)).unwrap_err();
            assert!(err.contains(want), "{body} -> {err}");
            assert!(err.contains("keyloom-install.json"), "{body} -> {err}");
        }
    }

    #[test]
    fn precedence_explicit_then_env_then_descriptor_then_discovery() {
        let (root, install) = layout("prec", None);
        let dev = root.join("dev");
        std::fs::create_dir_all(dev.join("backend/worker")).unwrap();
        std::fs::write(dev.join("backend/worker/__main__.py"), "").unwrap();
        let found = discover_with(None, &[install.clone()]);
        let Found::Install(i) = &found else { panic!() };
        // (a) explicit caller override beats everything
        let l = launch_with(
            Some(&dev),
            Some(dev.clone()),
            found.clone(),
            &[dev.clone()],
            None,
        );
        assert_eq!(l.repo.as_deref(), Some(dev.as_path()));
        assert_eq!(l.python, None);
        // (b) CK_REPO (valid checkout) beats the descriptor
        let l = launch_with(None, Some(dev.clone()), found.clone(), &[dev.clone()], None);
        assert_eq!(l.repo.as_deref(), Some(dev.as_path()));
        assert_eq!(l.python, None);
        // (c) descriptor beats ancestor discovery, and supplies python
        let l = launch_with(None, None, found.clone(), &[dev.clone()], Some(&dev));
        assert_eq!(l.repo.as_deref(), Some(i.helper.as_path()));
        assert_eq!(l.python.as_deref(), Some(i.python.as_path()));
        // (d) a checkout-ONLY tree (no descriptor) still discovers (developer flow, unchanged)
        let l = launch_with(
            None,
            None,
            Found::Missing,
            &[dev.join("rust-ui/target/debug")],
            None,
        );
        assert_eq!(l.repo.as_deref(), Some(dev.as_path()));
        assert_eq!(l.problem, None);
    }

    #[test]
    fn broken_descriptor_never_falls_back_to_a_checkout() {
        let root = tmp("broken");
        let install = root.join("install");
        std::fs::create_dir_all(&install).unwrap();
        std::fs::write(install.join(DESCRIPTOR_NAME), r#"{"schema":9}"#).unwrap();
        let dev = root.join("dev");
        std::fs::create_dir_all(dev.join("backend/worker")).unwrap();
        std::fs::write(dev.join("backend/worker/__main__.py"), "").unwrap();
        let found = discover_with(None, &[install]);
        let l = launch_with(None, None, found, &[dev.clone()], Some(&dev));
        assert_eq!(l.repo, None, "no silent developer-checkout fallback");
        assert_eq!(l.install, None);
        let p = l.problem.unwrap();
        assert!(p.contains("schema version 9"), "{p}");
        // An unusable CK_INSTALL is equally terminal.
        match discover_with(Some(root.join("nowhere")), &[]) {
            Found::Broken { problem, .. } => assert!(problem.contains(ENV_INSTALL), "{problem}"),
            f => panic!("{f:?}"),
        }
    }

    #[test]
    fn ck_install_env_file_or_folder() {
        let (root, install) = layout("env", None);
        let desc = install.join(DESCRIPTOR_NAME);
        match discover_with(Some(desc.clone()), &[]) {
            Found::Install(i) => assert_eq!(i.descriptor, desc),
            f => panic!("{f:?}"),
        }
        match discover_with(Some(install.clone()), &[]) {
            Found::Install(i) => assert_eq!(i.descriptor, desc),
            f => panic!("{f:?}"),
        }
        let _ = root;
    }

    #[test]
    fn read_only_app_root_resolves() {
        let (root, install) = layout("ro", None);
        let perms = std::fs::metadata(&install).unwrap().permissions();
        let mut p = perms.clone();
        use std::os::unix::fs::PermissionsExt;
        p.set_mode(0o555);
        std::fs::set_permissions(&install, p).unwrap();
        assert!(load(&install.join(DESCRIPTOR_NAME)).is_ok());
        std::fs::set_permissions(&install, perms).unwrap(); // cleanup
        let _ = root;
    }

    #[test]
    fn child_env_is_additive() {
        let (root, install) = layout("childenv", None);
        let i = load(&install.join(DESCRIPTOR_NAME)).unwrap();
        let (k, v) = child_env(Some(&i)).expect("exports CK_INSTALL");
        assert_eq!(k, ENV_INSTALL);
        assert_eq!(v, i.descriptor);
        assert!(child_env(None).is_none());
        let _ = root;
    }

    #[test]
    fn installed_python_children_drop_foreign_python_env_and_developer_runs_keep_it() {
        let (root, install) = layout("scrub", None);
        let i = load(&install.join(DESCRIPTOR_NAME)).unwrap();
        let envs =
            |c: &std::process::Command| -> std::collections::BTreeMap<String, Option<String>> {
                c.get_envs()
                    .map(|(k, v)| {
                        (
                            k.to_string_lossy().into(),
                            v.map(|v| v.to_string_lossy().into()),
                        )
                    })
                    .collect()
            };
        let mut c = std::process::Command::new("python3");
        prepare_python_command(&mut c, Some(&i));
        let e = envs(&c);
        for k in POLLUTING_ENV {
            assert_eq!(
                e.get(k),
                Some(&None),
                "{k} must be removed from the child, not merely unset here"
            );
        }
        assert_eq!(e.get("PYTHONNOUSERSITE"), Some(&Some("1".into())));
        // End to end: a host-polluted environment must not let a foreign module shadow the bundled one.
        let evil = root.join("evil");
        std::fs::create_dir_all(&evil).unwrap();
        std::fs::write(evil.join("foreign_mod.py"), "NAME = 'foreign'").unwrap();
        let probe = "import sys\ntry:\n import foreign_mod\n print('LEAK')\nexcept ImportError:\n print('clean', sys.flags.no_user_site)";
        let run = |install: Option<&Install>| {
            let mut c = std::process::Command::new("/usr/bin/python3");
            c.env("PYTHONPATH", &evil)
                .env("PYTHONHOME", "/nonexistent-home")
                .args(["-c", probe]);
            prepare_python_command(&mut c, install);
            let o = c.output().unwrap();
            String::from_utf8_lossy(&o.stdout).trim().to_string()
                + &String::from_utf8_lossy(&o.stderr)
        };
        assert_eq!(run(Some(&i)), "clean 1");
        let mut dev = std::process::Command::new("python3");
        prepare_python_command(&mut dev, None);
        assert!(
            envs(&dev).is_empty(),
            "developer checkout keeps the caller's environment"
        );
        let _ = root;
    }

    #[test]
    fn platform_user_areas() {
        let a = std::ffi::OsString::from("/xdg");
        let h = std::ffi::OsString::from("/home/u");
        assert_eq!(
            unix_user_data(Some(a.clone()), Some(h.clone())),
            Some(PathBuf::from("/xdg/Keyloom"))
        );
        assert_eq!(
            unix_user_data(None, Some(h.clone())),
            Some(PathBuf::from("/home/u/.local/share/Keyloom"))
        );
        assert_eq!(unix_user_data(None, None), None);
        let ad = std::ffi::OsString::from(r"C:\Users\u\AppData\Roaming");
        let la = std::ffi::OsString::from(r"C:\Users\u\AppData\Local");
        assert_eq!(
            windows_user_data(Some(ad.clone()), Some(la.clone())),
            Some(PathBuf::from(r"C:\Users\u\AppData\Roaming").join("Keyloom"))
        );
        // Windows without APPDATA and without HOME still resolves via LOCALAPPDATA.
        assert_eq!(
            windows_user_data(None, Some(la.clone())),
            Some(PathBuf::from(r"C:\Users\u\AppData\Local").join("Keyloom"))
        );
        assert_eq!(windows_user_data(None, None), None);
    }

    #[test]
    fn default_data_dir_install_roots_never_fall_back_to_the_packaged_repo() {
        let (root, install) = layout("datadir", None);
        let i = load(&install.join(DESCRIPTOR_NAME)).unwrap();
        let l = Launch {
            repo: Some(root.join("dev")),
            python: None,
            install: Some(i.clone()),
            problem: None,
        };
        // user_data: null on a valid install -> platform user area, never the packaged helper/app root.
        assert_eq!(default_data_dir(&l), platform_user_data());
        assert_ne!(default_data_dir(&l), l.repo);
        let mut i2 = i.clone();
        i2.user_data = Some(root.join("ud"));
        let l2 = Launch {
            repo: Some(root.join("dev")),
            python: None,
            install: Some(i2),
            problem: None,
        };
        assert_eq!(default_data_dir(&l2), Some(root.join("ud")));
        let l3 = Launch {
            repo: None,
            python: None,
            install: None,
            problem: None,
        };
        assert_eq!(default_data_dir(&l3), platform_user_data());
        // Developer checkout (no descriptor) keeps the repo default.
        let l4 = Launch {
            repo: Some(root.join("dev")),
            python: None,
            install: None,
            problem: None,
        };
        assert_eq!(default_data_dir(&l4), l4.repo);
    }
}
