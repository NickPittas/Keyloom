//! Per-installation Path Mapping: a shared location *name* maps to this PC's own mount/drive/UNC root.
//! Jobs and the future API carry `SharedRef` (name + validated relative components), never a host path;
//! each receiving installation resolves it under its own root. Local roots are checked with this
//! platform's path rules; a foreign path is never parsed here. Separate from the legacy SSH
//! `Target::path_map`, which is left untouched.
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use std::path::{Component, Path, PathBuf};

const NAME_MAX: usize = 64;
const SCHEME: &str = "shared:";

/// One named shared location and where this PC sees it.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct SharedRoot {
    pub name: String,
    pub local: String,
    /// Keys this build does not know: carried through saves untouched.
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

/// Cross-platform logical reference: a root name plus relative components. Always validated.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SharedRef {
    root: String,
    rel: Vec<String>,
}

fn check_name(n: &str) -> Result<(), String> {
    let ok = !n.is_empty()
        && n.len() <= NAME_MAX
        && n.chars().next().is_some_and(|c| c.is_ascii_alphanumeric())
        && n.chars().all(|c| c.is_ascii_alphanumeric() || "-_.".contains(c));
    ok.then_some(()).ok_or_else(|| {
        format!("location name {n:?} must be 1-{NAME_MAX} letters, digits, - _ . and start with a letter or digit")
    })
}

/// A portable path component: rejects traversal, separators, drive/stream colons, control characters
/// and names Windows would rewrite (trailing dot/space, device names).
fn check_component(c: &str) -> Result<(), String> {
    let stem = c.split('.').next().unwrap_or("").to_ascii_uppercase();
    let device = matches!(stem.as_str(), "CON" | "PRN" | "AUX" | "NUL")
        || (stem.len() == 4 && (stem.starts_with("COM") || stem.starts_with("LPT")) && stem.ends_with(|d: char| ('1'..='9').contains(&d)));
    let bad = c.is_empty()
        || c == "."
        || c == ".."
        || c.ends_with(['.', ' '])
        || device
        || c.chars().any(|ch| ch.is_control() || "/\\:*?\"<>|".contains(ch));
    if bad {
        return Err(format!("path component {c:?} is not allowed in a shared reference"));
    }
    Ok(())
}

impl SharedRef {
    pub fn new<S: AsRef<str>>(root: &str, rel: &[S]) -> Result<Self, String> {
        check_name(root)?;
        rel.iter().try_for_each(|c| check_component(c.as_ref()))?;
        Ok(Self { root: root.into(), rel: rel.iter().map(|c| c.as_ref().into()).collect() })
    }
    pub fn root(&self) -> &str {
        &self.root
    }
    pub fn components(&self) -> &[String] {
        &self.rel
    }
    /// `shared:<root>/<a>/<b>`. Components cannot contain `/`, so no escaping is needed.
    pub fn encode(&self) -> String {
        std::iter::once(self.root.as_str()).chain(self.rel.iter().map(String::as_str)).fold(SCHEME.to_string(), |mut s, p| {
            if s.len() > SCHEME.len() {
                s.push('/');
            }
            s.push_str(p);
            s
        })
    }
    /// Inverse of `encode`; anything non-canonical (empty segment, `..`, absolute path, backslash) is an error.
    pub fn decode(s: &str) -> Result<Self, String> {
        let body = s.strip_prefix(SCHEME).ok_or_else(|| format!("not a shared reference: {s:?}"))?;
        let mut parts = body.split('/');
        let root = parts.next().unwrap_or("");
        Self::new(root, &parts.collect::<Vec<_>>())
    }
}

/// Native local-root rules: absolute on this platform (so `C:foo`, `\x` and POSIX-vs-drive confusion fail), no `..`.
pub fn check_local(p: &str) -> Result<(), String> {
    if p.is_empty() || p.contains('\0') {
        return Err("local path is empty".into());
    }
    let path = Path::new(p);
    if !path.is_absolute() {
        return Err(format!("\"{p}\" is not an absolute path on this PC"));
    }
    if path.components().any(|c| c == Component::ParentDir) {
        return Err(format!("\"{p}\" must not contain '..'"));
    }
    Ok(())
}

/// Component list used for overlap and prefix tests (case-insensitive where the platform is).
fn key(p: &Path) -> Vec<String> {
    p.components()
        .filter(|c| !matches!(c, Component::CurDir))
        .map(|c| {
            let s = c.as_os_str().to_string_lossy().into_owned();
            if cfg!(windows) { s.to_lowercase() } else { s }
        })
        .collect()
}

pub fn check_root(r: &SharedRoot) -> Result<(), String> {
    check_name(&r.name)?;
    check_local(&r.local).map_err(|e| format!("{}: {e}", r.name))
}

/// Every row valid, names unique (case-insensitively), no two local roots equal or nested
/// (a local path would otherwise have two logical names).
pub fn validate_roots(roots: &[SharedRoot]) -> Result<(), String> {
    for (i, r) in roots.iter().enumerate() {
        check_root(r)?;
        let k = key(Path::new(&r.local));
        for o in &roots[..i] {
            if o.name.eq_ignore_ascii_case(&r.name) {
                return Err(format!("duplicate location name {}", r.name));
            }
            let ok = key(Path::new(&o.local));
            if k.starts_with(&ok) || ok.starts_with(&k) {
                return Err(format!("locations {} and {} overlap ({} / {})", o.name, r.name, o.local, r.local));
            }
        }
    }
    Ok(())
}

/// This PC's path for a reference. The reference's components are already safe, so the result stays under the root.
pub fn resolve(roots: &[SharedRoot], r: &SharedRef) -> Result<PathBuf, String> {
    let root = roots
        .iter()
        .find(|x| x.name.eq_ignore_ascii_case(&r.root))
        .ok_or_else(|| format!("this PC has no path mapping for shared location {:?}", r.root))?;
    check_local(&root.local)?;
    Ok(r.rel.iter().fold(PathBuf::from(&root.local), |p, c| p.join(c)))
}

/// Logical reference for a path on this PC (must lie inside exactly one mapped root).
pub fn reference_for(roots: &[SharedRoot], path: &Path) -> Result<SharedRef, String> {
    if !path.is_absolute() || path.components().any(|c| c == Component::ParentDir) {
        return Err(format!("{} is not a clean absolute path", path.display()));
    }
    let k = key(path);
    for r in roots {
        let rk = key(Path::new(&r.local));
        if k.starts_with(&rk) {
            let rel: Vec<String> = path
                .components()
                .filter(|c| !matches!(c, Component::CurDir))
                .skip(rk.len())
                .map(|c| c.as_os_str().to_str().map(str::to_owned).ok_or("path is not valid UTF-8"))
                .collect::<Result<_, _>>()?;
            return SharedRef::new(&r.name, &rel);
        }
    }
    Err(format!("{} is not inside any mapped shared location", path.display()))
}

/// `path` (a resolved path under the mapped location `root_name`) must really live inside that location once
/// symlinks are followed. The root's own canonical form is the trust anchor (the root itself may be an alias).
/// A path that does not exist yet is judged by its nearest existing ancestor (canonical) plus the remainder, which
/// is built from validated components and so cannot climb. Checked at one moment: it cannot stop a symlink being
/// swapped in afterwards, and the worker has no roots to re-check with (pilot: trusted paired clients/storage only).
pub fn confined(roots: &[SharedRoot], root_name: &str, path: &Path) -> Result<(), String> {
    let root = roots
        .iter()
        .find(|r| r.name.eq_ignore_ascii_case(root_name))
        .ok_or_else(|| format!("this PC has no path mapping for shared location {root_name:?}"))?;
    let anchor = std::fs::canonicalize(&root.local).map_err(|e| format!("shared location {root_name:?} ({}) is not reachable: {e}", root.local))?;
    // symlink_metadata, not exists(): a dangling or looping link must be judged (and refused), not skipped as absent.
    let existing = path.ancestors().find(|a| std::fs::symlink_metadata(a).is_ok()).ok_or_else(|| format!("{} has no existing ancestor", path.display()))?;
    let real = std::fs::canonicalize(existing).map_err(|e| format!("{} cannot be resolved ({e}): a dangling or looping symlink?", existing.display()))?;
    let rest = path.strip_prefix(existing).unwrap_or_else(|_| Path::new(""));
    if rest.components().any(|c| !matches!(c, Component::Normal(_))) {
        return Err(format!("{} is not a clean path", path.display()));
    }
    if real.starts_with(&anchor) {
        Ok(())
    } else {
        Err(format!("{} leaves shared location {root_name:?}: it resolves to {} outside {} (a symlink?)", path.display(), real.display(), anchor.display()))
    }
}

/// Result of the explicit Test action.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Access {
    pub read: Result<(), String>,
    pub write: Result<(), String>,
}

/// Read: the root lists. Write: create one uniquely named file with `create_new` (never overwrites),
/// write and read it back, then remove only that file. No recursion, nothing else is touched.
pub fn test_access(root: &Path) -> Access {
    let read = std::fs::read_dir(root).map(|_| ()).map_err(|e| format!("cannot read {}: {e}", root.display()));
    let write = (|| {
        use std::io::{Read, Write};
        let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_nanos());
        let (path, mut f) = (0..8u32)
            .find_map(|n| {
                let p = root.join(format!(".keyloom-access-{}-{nanos}-{n}.tmp", std::process::id()));
                match std::fs::OpenOptions::new().write(true).create_new(true).open(&p) {
                    Ok(f) => Some(Ok((p, f))),
                    Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => None,
                    Err(e) => Some(Err(e)),
                }
            })
            .ok_or_else(|| "could not pick an unused probe name".to_string())?
            .map_err(|e| format!("cannot write in {}: {e}", root.display()))?;
        let body = b"corridorkey access probe";
        let io = f.write_all(body).and_then(|_| f.sync_all()).and_then(|_| {
            let mut back = Vec::new();
            std::fs::File::open(&path)?.read_to_end(&mut back)?;
            (back == body).then_some(()).ok_or_else(|| std::io::Error::other("read-back differed"))
        });
        drop(f);
        let rm = std::fs::remove_file(&path);
        io.map_err(|e| format!("write test failed in {}: {e}", root.display()))?;
        rm.map_err(|e| format!("wrote the probe but could not remove {}: {e}", path.display()))
    })();
    Access { read, write }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn root_of(n: &str, l: &str) -> SharedRoot {
        root(n, l)
    }

    fn root(n: &str, l: &str) -> SharedRoot {
        SharedRoot { name: n.into(), local: l.into(), ..Default::default() }
    }

    #[test]
    fn reference_round_trips_with_spaces_and_unicode() {
        let r = SharedRef::new("NAS-1", &["Show é", "shot 01", "a b.mov"]).unwrap();
        let s = r.encode();
        assert_eq!(s, "shared:NAS-1/Show é/shot 01/a b.mov");
        assert_eq!(SharedRef::decode(&s).unwrap(), r);
        assert_eq!(SharedRef::decode("shared:NAS").unwrap().encode(), "shared:NAS");
    }

    #[test]
    fn hostile_references_are_rejected() {
        for bad in [
            "NAS/x", "shared:", "shared:NAS/", "shared:NAS//x", "shared:NAS/../x", "shared:NAS/./x",
            "shared:NAS/C:/x", "shared:NAS/a\\b", "shared:/etc/passwd", "shared:NAS/x\0", "shared:NAS/nul",
            "shared:NAS/COM1.txt", "shared:NAS/a.", "shared:NAS/a ", "shared:bad name/x", "shared:..",
        ] {
            assert!(SharedRef::decode(bad).is_err(), "{bad:?} must be rejected");
        }
        assert!(SharedRef::new("NAS", &["/abs"]).is_err() && SharedRef::new("NAS", &[".."]).is_err());
    }

    #[test]
    fn roots_reject_duplicates_overlap_and_non_native_paths() {
        assert!(validate_roots(&[root("A", "/mnt/a"), root("B", "/mnt/b")]).is_ok());
        assert!(validate_roots(&[root("A", "/mnt/a"), root("a", "/mnt/b")]).is_err());
        assert!(validate_roots(&[root("A", "/mnt/a"), root("B", "/mnt/a/")]).is_err());
        assert!(validate_roots(&[root("A", "/mnt/a"), root("B", "/mnt/a/sub")]).is_err());
        assert!(validate_roots(&[root("A", "/mnt/a"), root("B", "/mnt/ab")]).is_ok(), "whole components only");
        for bad in ["", "rel/dir", "/mnt/../x", "~/x"] {
            assert!(check_root(&root("A", bad)).is_err(), "{bad:?}");
        }
        assert!(check_root(&root("", "/mnt/a")).is_err() && check_root(&root("-x", "/mnt/a")).is_err());
        if cfg!(unix) {
            for foreign in [r"Z:\Shared", r"\\nas\share", r"C:foo"] {
                assert!(check_local(foreign).is_err(), "{foreign} is not a Linux path");
            }
        } else {
            for bad in [r"\foo", r"C:foo", "/mnt/a"] {
                assert!(check_local(bad).is_err(), "{bad}");
            }
            assert!(check_local(r"Z:\Shared").is_ok() && check_local(r"\\nas\share\x").is_ok());
        }
    }

    #[test]
    fn resolve_and_reverse_are_consistent() {
        let roots = [root("NAS", "/mnt/nas"), root("Other", "/mnt/nas2")];
        let r = SharedRef::decode("shared:nas/proj x/clip.mov").unwrap();
        let p = resolve(&roots, &r).unwrap();
        assert_eq!(p, Path::new("/mnt/nas/proj x/clip.mov"));
        let back = reference_for(&roots, &p).unwrap();
        assert_eq!(back.encode(), "shared:NAS/proj x/clip.mov");
        assert_eq!(reference_for(&roots, Path::new("/mnt/nas2/q")).unwrap().root(), "Other");
        assert!(reference_for(&roots, Path::new("/mnt/nas3/q")).is_err(), "prefix must be whole components");
        assert!(reference_for(&roots, Path::new("/mnt/nas/../etc")).is_err());
        assert!(reference_for(&roots, Path::new("rel")).is_err());
        assert!(resolve(&roots, &SharedRef::decode("shared:Missing/x").unwrap()).unwrap_err().contains("no path mapping"));
    }

    #[test]
    fn confined_allows_missing_nested_paths_and_refuses_dangling_and_looping_links() {
        use std::os::unix::fs::symlink;
        let d = std::env::temp_dir().join(format!("ck-confined-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        let (root, other) = (d.join("share"), d.join("other"));
        std::fs::create_dir_all(&root).unwrap();
        std::fs::create_dir_all(&other).unwrap();
        let roots = [root_of("S", root.to_str().unwrap())];
        assert!(confined(&roots, "S", &root.join("a/b/c/new.png")).is_ok(), "missing nested path under an existing folder");
        symlink(root.join("gone"), root.join("dangling")).unwrap();
        let e = confined(&roots, "S", &root.join("dangling/x/y.png")).unwrap_err();
        assert!(e.contains("dangling or looping"), "{e}");
        symlink(root.join("loop"), root.join("loop")).unwrap();
        assert!(confined(&roots, "S", &root.join("loop/x")).unwrap_err().contains("cannot be resolved"));
        symlink(&other, root.join("escape")).unwrap();
        assert!(confined(&roots, "S", &root.join("escape/new")).unwrap_err().contains("leaves shared location"));
        assert!(confined(&roots, "Nope", &root).unwrap_err().contains("no path mapping"));
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn access_test_uses_an_owned_probe_and_leaves_user_files() {
        let d = std::env::temp_dir().join(format!("ck-shared-access-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        std::fs::write(d.join("keep.txt"), b"mine").unwrap();
        let a = test_access(&d);
        assert_eq!(a, Access { read: Ok(()), write: Ok(()) });
        let names: Vec<_> = std::fs::read_dir(&d).unwrap().map(|e| e.unwrap().file_name()).collect();
        assert_eq!(names, vec![std::ffi::OsString::from("keep.txt")]);
        assert_eq!(std::fs::read(d.join("keep.txt")).unwrap(), b"mine");
        let missing = test_access(&d.join("nope"));
        assert!(missing.read.is_err() && missing.write.is_err());
        let _ = std::fs::remove_dir_all(&d);
    }
}
