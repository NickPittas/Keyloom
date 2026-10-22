//! Replace planning: only an app-owned result of the same output identity can be replaced; settings may differ.
use ck::jobs::JobMode;
use ck::project::{load_project, LoadOptions};
use ck::workflow::*;
use serde_json::json;

fn png(p: &std::path::Path) {
    let f = std::fs::File::create(p).unwrap();
    let mut e = png::Encoder::new(f, 4, 4);
    e.set_color(png::ColorType::Rgb);
    e.set_depth(png::BitDepth::Eight);
    e.write_header()
        .unwrap()
        .write_image_data(&[7; 48])
        .unwrap();
}

#[test]
fn replace_is_offered_for_an_owned_result_and_refused_when_the_output_identity_differs() {
    let base = std::env::temp_dir().join(format!("ck-replace-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    let clip = base.join("clips/c1");
    std::fs::create_dir_all(clip.join("Frames")).unwrap();
    for i in 0..4 {
        png(&clip.join(format!("Frames/f_{}.png", 1101 + i)));
    }
    std::fs::write(
        clip.join(".video_metadata.json"),
        r#"{"fps":24.0,"width":4,"height":4}"#,
    )
    .unwrap();
    std::fs::write(clip.join("clip.json"), r#"{"clip_id":"cid"}"#).unwrap();
    std::fs::write(base.join("project.json"), r#"{"version":2}"#).unwrap();
    let p = load_project(&base, &LoadOptions::default()).unwrap();
    let c = &p.clips[0];
    let params = Params::default();
    let out = OutputCfg {
        fg_enabled: false,
        comp_enabled: false,
        processed_enabled: false,
        matte_format: "png".into(),
        ..OutputCfg::default()
    };
    fn inp_<'a>(
        c: &'a ck::project::Clip,
        params: &'a Params,
        out: &'a OutputCfg,
        range: (usize, usize),
    ) -> Inputs<'a> {
        Inputs {
            clip: c,
            project_name: "P",
            location: None,
            params,
            out,
            range,
            digest: None,
            pending_hint: false,
        }
    }
    let first = plan(&inp_(c, &params, &out, (0, 3)), Intent::New).unwrap();
    // Fabricate the owned result the worker would have written: manifest v3 + the files.
    let job = &first.job;
    std::fs::create_dir_all(&job.manifest_dir).unwrap();
    let files: std::collections::BTreeMap<_, Vec<String>> = job
        .outputs
        .iter()
        .map(|(k, v)| {
            (
                k.clone(),
                v.iter()
                    .map(|p| {
                        p.strip_prefix(&job.manifest_dir)
                            .unwrap()
                            .to_string_lossy()
                            .into_owned()
                    })
                    .collect(),
            )
        })
        .collect();
    for f in job.outputs.values().flatten() {
        std::fs::create_dir_all(f.parent().unwrap()).unwrap();
        png(f);
    }
    let stems: Vec<String> = c.input_files.iter().map(|f| f.0.clone()).collect();
    std::fs::write(
        job.manifest_dir.join(".corridorkey_manifest.json"),
        json!({"version":3,"engine":"corridorkey-v2","clip_id":job.clip_id,"range":job.range,"params":job.params,
               "enabled_outputs":["matte"],"formats":{"fg":"exr","matte":"png","comp":"png","processed":"exr"},
               "exr_compression":"dwab","files":files,"source_stems":stems,"complete":true,"effective_profile":{}}).to_string(),
    )
    .unwrap();

    // Same identity, different SETTINGS: New is blocked as existing, Replace is available and builds mode=replace.
    let changed = Params {
        despill_strength: 0.1,
        hint_mode: "none".into(),
        ..params.clone()
    };
    let blocked = plan(&inp_(c, &changed, &out, (0, 3)), Intent::New).unwrap_err();
    let ex = blocked.existing.expect("existing result");
    assert!(ex.replace && ex.replace_why.is_none());
    assert_eq!(ex.replace_files, 4);
    let rep = plan(&inp_(c, &changed, &out, (0, 3)), Intent::Replace).unwrap();
    assert_eq!(rep.job.mode, JobMode::Replace);
    assert_eq!(rep.job.outputs, first.job.outputs, "exactly the same files");
    assert_eq!(serde_json::to_value(&rep.job).unwrap()["mode"], "replace");
    assert!(rep.summary.contains("replacing"));

    // A different output format changes the output identity: not replaceable, with the reason.
    let other = OutputCfg {
        matte_format: "exr".into(),
        ..out.clone()
    };
    let ex = plan(&inp_(c, &changed, &other, (0, 3)), Intent::New)
        .unwrap_err()
        .existing;
    // (a different extension resolves other files, so there may be no existing result at all)
    if let Some(ex) = ex {
        assert!(!ex.replace && ex.replace_why.is_some());
    }
    assert!(plan(&inp_(c, &changed, &other, (0, 3)), Intent::Replace).is_err());
    // A different range is a different identity.
    assert!(plan(&inp_(c, &changed, &out, (0, 2)), Intent::Replace).is_err());
    // Nothing to replace when no result exists.
    std::fs::remove_dir_all(&job.manifest_dir).unwrap();
    assert!(plan(&inp_(c, &changed, &out, (0, 3)), Intent::Replace)
        .unwrap_err()
        .reasons[0]
        .contains("Nothing to replace"));
    let _ = std::fs::remove_dir_all(base);
}

#[test]
fn temporal_awareness_48_24_reaches_the_job_persists_and_travels_through_presets_and_replace() {
    use ck::settings::{temporal_explain, temporal_label, Settings};
    let base = std::env::temp_dir().join(format!("ck-temporal-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    let clip = base.join("clips/c1");
    std::fs::create_dir_all(clip.join("Frames")).unwrap();
    for i in 0..4 {
        png(&clip.join(format!("Frames/f_{}.png", 1101 + i)));
    }
    std::fs::write(
        clip.join(".video_metadata.json"),
        r#"{"fps":24.0,"width":4,"height":4}"#,
    )
    .unwrap();
    std::fs::write(clip.join("clip.json"), "{}").unwrap();
    std::fs::write(base.join("project.json"), r#"{"version":2}"#).unwrap();
    let p = load_project(&base, &LoadOptions::default()).unwrap();
    let c = &p.clips[0];
    let params = Params {
        temporal_window: "48/24".into(),
        ..Params::default()
    };
    let out = OutputCfg {
        fg_enabled: false,
        comp_enabled: false,
        processed_enabled: false,
        matte_format: "png".into(),
        ..OutputCfg::default()
    };
    let i = Inputs {
        clip: c,
        project_name: "P",
        location: None,
        params: &params,
        out: &out,
        range: (0, 3),
        digest: None,
        pending_hint: false,
    };
    let pl = plan(&i, Intent::New).unwrap();
    assert_eq!(
        pl.job.params["temporal_window"], "48/24",
        "exact wire value"
    );
    commit(c, &pl, &params, &out).unwrap();
    // Reopen: the clip's stored workflow seeds the next draft with the same value.
    let p = load_project(&base, &LoadOptions::default()).unwrap();
    let (seeded, _, _) = seed(&p.clips[0], &p, &Settings::default());
    assert_eq!(seeded.temporal_window, "48/24");
    // A workflow preset carries it; applying onto another draft changes exactly that field.
    let data = ck::presets::workflow_data(&params, &out);
    let mut other = Params::default();
    let mut o2 = OutputCfg::default();
    ck::presets::apply_workflow(&data, &mut other, &mut o2).unwrap();
    assert_eq!(other.temporal_window, "48/24");
    // Wording stays tied to the exact presets.
    assert_eq!(temporal_label("48/24"), "48 frames (stride 24)");
    assert_eq!(temporal_label("1/1"), "Off: single frame (1/1)");
    assert!(temporal_explain("48/24", Some(4)).contains("capped at 4"));
    assert!(temporal_explain("1/1", Some(4)).contains("on its own"));
    // (Replace with a changed window is covered by the replace test: params are not part of the identity.)
    let _ = std::fs::remove_dir_all(base);
}
