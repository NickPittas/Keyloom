//! Range and output-location contracts (`range-output-contracts.md`). Pure data + resolvers: nothing here
//! touches disk or is wired to a job engine in this slice.
use serde::{Deserialize, Serialize};
use std::path::{Component, Path, PathBuf};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RangeIdentity {
    pub in_point: usize,
    pub out_point: usize,
    pub in_stem: String,
    pub out_stem: String,
}

#[derive(Debug, PartialEq, Eq)]
pub enum RangeError {
    Empty,
    OutOfBounds,
    /// "Source changed: re-set In/Out" — never shifted silently.
    SourceChanged,
}

impl RangeIdentity {
    /// `a..=b` zero-based inclusive indices into the source stem list.
    pub fn from_inclusive(stems: &[String], a: usize, b: usize) -> Result<Self, RangeError> {
        if stems.is_empty() {
            return Err(RangeError::Empty);
        }
        if a > b || b >= stems.len() {
            return Err(RangeError::OutOfBounds);
        }
        Ok(Self {
            in_point: a,
            out_point: b,
            in_stem: stems[a].clone(),
            out_stem: stems[b].clone(),
        })
    }
    pub fn frame_count(&self) -> usize {
        self.out_point - self.in_point + 1
    }
    pub fn validate(&self, stems: &[String]) -> Result<(), RangeError> {
        match (stems.get(self.in_point), stems.get(self.out_point)) {
            (Some(a), Some(b))
                if *a == self.in_stem && *b == self.out_stem && self.in_point <= self.out_point =>
            {
                Ok(())
            }
            _ => Err(RangeError::SourceChanged),
        }
    }
}

/// Default `{resolution}` label from the source width. Label only: nothing is resized.
pub fn resolution_label(w: u32, h: u32) -> String {
    match w {
        0..=1919 => format!("{h}p"),
        1920..=3199 => "2K".into(),
        3200..=4999 => "4K".into(),
        _ => format!("{}K", (w as f64 / 1000.0).round() as u32),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OutputKind {
    Fg,
    Matte,
    Comp,
    Processed,
}
impl OutputKind {
    pub fn token(self) -> &'static str {
        match self {
            OutputKind::Fg => "FG",
            OutputKind::Matte => "Matte",
            OutputKind::Comp => "Comp",
            OutputKind::Processed => "Processed",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OutputLocation {
    pub root: PathBuf,
    pub folder_template: String,
    pub file_template: String,
}

#[derive(Debug, Clone)]
pub struct ClipOutputValues {
    pub project: String,
    pub name: String,
    pub resolution: String,
    pub date: String,
    pub version: u32,
}

#[derive(Debug, PartialEq, Eq)]
pub enum LocationError {
    RootNotAbsolute,
    AbsoluteTemplate,
    BadSegment(String),
    UnknownToken(String),
    FileTemplateHashes,
    FileTemplateSlash,
    EmptyValue(&'static str),
    ReservedName(String),
    SegmentTooLong,
    EscapesRoot,
    /// Several outputs enabled but no `{output}` token: they would write the same files.
    OutputCollision,
}

fn sanitize(v: &str) -> String {
    let s: String = v
        .chars()
        .map(|c| {
            if "/\\:*?\"<>|".contains(c) || c.is_control() {
                '_'
            } else {
                c
            }
        })
        .collect();
    s.trim_matches(|c| c == '.' || c == ' ').to_string()
}

fn reserved(s: &str) -> bool {
    let base = s.split('.').next().unwrap_or("").to_ascii_uppercase();
    matches!(base.as_str(), "CON" | "PRN" | "AUX" | "NUL")
        || ((base.starts_with("COM") || base.starts_with("LPT"))
            && base.len() == 4
            && base.as_bytes()[3].is_ascii_digit()
            && base != "COM0"
            && base != "LPT0")
}

fn expand(tpl: &str, v: &ClipOutputValues, out: OutputKind) -> Result<String, LocationError> {
    let mut res = String::new();
    let mut rest = tpl;
    while let Some(i) = rest.find('{') {
        res.push_str(&rest[..i]);
        let j = rest[i..]
            .find('}')
            .ok_or_else(|| LocationError::UnknownToken(rest[i..].into()))?
            + i;
        let tok = &rest[i + 1..j];
        let (val, label): (String, &'static str) = match tok {
            "project" => (v.project.clone(), "project"),
            "name" => (v.name.clone(), "name"),
            "output" => (out.token().into(), "output"),
            "resolution" => (v.resolution.clone(), "resolution"),
            "date" => (v.date.clone(), "date"),
            "version" => (format!("v{:03}", v.version), "version"),
            _ => return Err(LocationError::UnknownToken(tok.into())),
        };
        let val = sanitize(&val);
        if val.is_empty() || val == "." || val == ".." {
            return Err(LocationError::EmptyValue(label));
        }
        res.push_str(&val);
        rest = &rest[j + 1..];
    }
    res.push_str(rest);
    Ok(res)
}

/// Check templates only (edit-time validation; also used before resolving).
pub fn validate_templates(
    loc: &OutputLocation,
    enabled_outputs: usize,
) -> Result<(), LocationError> {
    if !loc.root.is_absolute() {
        return Err(LocationError::RootNotAbsolute);
    }
    if loc.folder_template.starts_with('/') || loc.folder_template.contains(':') {
        return Err(LocationError::AbsoluteTemplate);
    }
    for seg in loc.folder_template.split('/') {
        if seg.is_empty() || seg == "." || seg == ".." {
            return Err(LocationError::BadSegment(seg.into()));
        }
    }
    if loc.file_template.contains('/') || loc.file_template.contains('\\') {
        return Err(LocationError::FileTemplateSlash);
    }
    let hashes: Vec<usize> = loc
        .file_template
        .match_indices('#')
        .map(|(i, _)| i)
        .collect();
    let contiguous = hashes.windows(2).all(|w| w[1] == w[0] + 1);
    if hashes.is_empty() || !contiguous {
        return Err(LocationError::FileTemplateHashes);
    }
    if enabled_outputs > 1
        && !loc.folder_template.contains("{output}")
        && !loc.file_template.contains("{output}")
    {
        return Err(LocationError::OutputCollision);
    }
    Ok(())
}

/// Resolve one output file path for source frame number `frame` (the stem's own number, not renumbered).
pub fn resolve_path(
    loc: &OutputLocation,
    v: &ClipOutputValues,
    out: OutputKind,
    enabled_outputs: usize,
    frame: u64,
    ext: &str,
) -> Result<PathBuf, LocationError> {
    validate_templates(loc, enabled_outputs)?;
    let folder = expand(&loc.folder_template, v, out)?;
    let mut path = loc.root.clone();
    for seg in folder.split('/') {
        if seg.is_empty() || seg == "." || seg == ".." {
            return Err(LocationError::BadSegment(seg.into()));
        }
        if reserved(seg) {
            return Err(LocationError::ReservedName(seg.into()));
        }
        if seg.len() > 255 {
            return Err(LocationError::SegmentTooLong);
        }
        path.push(seg);
    }
    // Locate the `####` run in the template *before* substituting tokens, so `#` inside a value can't confuse it.
    let start = loc.file_template.find('#').unwrap();
    let n = loc.file_template.matches('#').count();
    let (pre, post) = (&loc.file_template[..start], &loc.file_template[start + n..]);
    let file = format!(
        "{}{:0n$}{}.{}",
        expand(pre, v, out)?,
        frame,
        expand(post, v, out)?,
        ext.trim_start_matches('.'),
        n = n
    );
    if reserved(&file) {
        return Err(LocationError::ReservedName(file));
    }
    if file.len() > 255 {
        return Err(LocationError::SegmentTooLong);
    }
    path.push(file);
    // Final-string check: still under the root, no `..`.
    if !path.starts_with(&loc.root) || path.components().any(|c| matches!(c, Component::ParentDir))
    {
        return Err(LocationError::EscapesRoot);
    }
    Ok(path)
}

pub fn display(p: &Path) -> String {
    p.display().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    fn vals() -> ClipOutputValues {
        ClipOutputValues {
            project: "P".into(),
            name: "clip_name".into(),
            resolution: "2K".into(),
            date: "18-10-2027".into(),
            version: 1,
        }
    }
    fn loc(folder: &str, file: &str) -> OutputLocation {
        OutputLocation {
            root: "/mnt/NAS/Project/Masks".into(),
            folder_template: folder.into(),
            file_template: file.into(),
        }
    }

    #[test]
    fn user_example_resolves_exactly() {
        let l = loc("{name}_{resolution}_{date}_{version}", "{name}_####");
        let p = resolve_path(&l, &vals(), OutputKind::Matte, 1, 1101, "exr").unwrap();
        assert_eq!(
            p,
            Path::new("/mnt/NAS/Project/Masks/clip_name_2K_18-10-2027_v001/clip_name_1101.exr")
        );
        // Wider than padding grows, never truncates.
        let l = loc("{name}", "{name}_####");
        assert!(
            resolve_path(&l, &vals(), OutputKind::Matte, 1, 12345, "exr")
                .unwrap()
                .ends_with("clip_name_12345.exr")
        );
    }

    #[test]
    fn multi_output_needs_output_token() {
        let l = loc("{name}_{version}", "{name}_####");
        assert_eq!(
            resolve_path(&l, &vals(), OutputKind::Fg, 2, 1, "exr"),
            Err(LocationError::OutputCollision)
        );
        let l = loc("{name}_{version}/{output}", "{name}_####");
        assert!(resolve_path(&l, &vals(), OutputKind::Fg, 2, 1, "exr")
            .unwrap()
            .ends_with("clip_name_v001/FG/clip_name_0001.exr"));
    }

    #[test]
    fn rejects_traversal_absolute_unknown_and_bad_hashes() {
        assert!(matches!(
            validate_templates(&loc("../x", "a_####"), 1),
            Err(LocationError::BadSegment(_))
        ));
        assert_eq!(
            validate_templates(&loc("/abs", "a_####"), 1),
            Err(LocationError::AbsoluteTemplate)
        );
        assert!(matches!(
            validate_templates(&loc("a//b", "a_####"), 1),
            Err(LocationError::BadSegment(_))
        ));
        assert_eq!(
            validate_templates(&loc("a", "a"), 1),
            Err(LocationError::FileTemplateHashes)
        );
        assert_eq!(
            validate_templates(&loc("a", "a_##_##"), 1),
            Err(LocationError::FileTemplateHashes)
        );
        assert_eq!(
            validate_templates(&loc("a", "x/a_####"), 1),
            Err(LocationError::FileTemplateSlash)
        );
        assert!(matches!(
            resolve_path(
                &loc("{nope}", "a_####"),
                &vals(),
                OutputKind::Fg,
                1,
                1,
                "exr"
            ),
            Err(LocationError::UnknownToken(_))
        ));
    }

    #[test]
    fn sanitizes_values_and_rejects_empty_and_reserved() {
        let mut v = vals();
        v.name = "a/b:c".into();
        assert!(
            resolve_path(&loc("{name}", "n_####"), &v, OutputKind::Fg, 1, 1, "exr")
                .unwrap()
                .to_string_lossy()
                .contains("/a_b_c/")
        );
        v.name = "..".into();
        assert_eq!(
            resolve_path(&loc("{name}", "n_####"), &v, OutputKind::Fg, 1, 1, "exr"),
            Err(LocationError::EmptyValue("name"))
        );
        v.name = "CON".into();
        assert!(matches!(
            resolve_path(&loc("{name}", "n_####"), &v, OutputKind::Fg, 1, 1, "exr"),
            Err(LocationError::ReservedName(_))
        ));
    }

    #[test]
    fn hash_in_value_does_not_confuse_frame_number() {
        let mut v = vals();
        v.name = "A#B".into();
        let p = resolve_path(
            &loc("{name}", "{name}_####"),
            &v,
            OutputKind::Matte,
            1,
            7,
            "exr",
        )
        .unwrap();
        assert!(p.ends_with("A#B/A#B_0007.exr"), "{p:?}");
    }

    #[test]
    fn resolution_label_boundaries() {
        for (w, e) in [
            (1919, "1080p"),
            (1920, "2K"),
            (3199, "2K"),
            (3200, "4K"),
            (4999, "4K"),
            (5000, "5K"),
        ] {
            assert_eq!(resolution_label(w, 1080), e, "{w}");
        }
    }

    #[test]
    fn inclusive_range_and_source_change() {
        let stems: Vec<String> = (0..10).map(|i| format!("f{i:03}")).collect();
        let r = RangeIdentity::from_inclusive(&stems, 2, 5).unwrap();
        assert_eq!(r.frame_count(), 4);
        assert_eq!((r.in_stem.as_str(), r.out_stem.as_str()), ("f002", "f005"));
        assert!(r.validate(&stems).is_ok());
        let mut shifted = stems.clone();
        shifted.insert(0, "new".into());
        assert_eq!(r.validate(&shifted), Err(RangeError::SourceChanged));
        assert_eq!(
            RangeIdentity::from_inclusive(&stems, 5, 2),
            Err(RangeError::OutOfBounds)
        );
        assert_eq!(
            RangeIdentity::from_inclusive(&stems, 0, 10),
            Err(RangeError::OutOfBounds)
        );
    }
}
