//! The hello's optional GPU identity: old workers parse unchanged, a verified uuid is exposed, anything else is
//! an explained "unavailable" (never an index/name fallback). Pure parsing, no process.
use ck::worker::{Gpu, Hello};
use serde_json::{json, Value};

fn hello(gpu: Value) -> Hello {
    serde_json::from_value(json!({"protocol":1,"worker_rev":"r","engine":"e","host":"h","gpu":gpu,"models":{},"capabilities":{}})).unwrap()
}

#[test]
fn an_old_hello_without_identity_fields_parses_and_has_no_verified_uuid() {
    let g = hello(json!({"index":0,"name":"RTX","vram_gb":24.0})).gpu.unwrap();
    assert_eq!((g.uuid.clone(), g.uuid_status.clone(), g.uuid_reason.clone()), (None, None, None));
    let why = g.verified_uuid().unwrap_err();
    assert!(why.contains("predates"), "{why}");
}

#[test]
fn a_verified_identity_is_exposed_only_with_status_and_uuid_together() {
    let g: Gpu = hello(json!({"index":1,"name":"RTX","vram_gb":24.0,"uuid":"GPU-11111111-2222-3333-4444-555555555555","uuid_status":"verified","uuid_reason":null})).gpu.unwrap();
    assert_eq!(g.verified_uuid(), Ok("GPU-11111111-2222-3333-4444-555555555555"));
    // a uuid without the verified status is not an identity
    let g = hello(json!({"index":1,"name":"RTX","vram_gb":24.0,"uuid":"GPU-11111111-2222-3333-4444-555555555555","uuid_status":"unverified","uuid_reason":"uuid not in NVML"})).gpu.unwrap();
    assert_eq!(g.verified_uuid(), Err("uuid not in NVML".to_string()));
    // "verified" with no uuid is also refused
    let g = hello(json!({"index":1,"name":"RTX","vram_gb":24.0,"uuid":null,"uuid_status":"verified"})).gpu.unwrap();
    assert!(g.verified_uuid().is_err());
}

#[test]
fn unverified_gives_the_reason_and_local_fields_are_untouched() {
    let g = hello(json!({"index":2,"name":"RTX","vram_gb":12.5,"uuid":null,"uuid_status":"unverified","uuid_reason":"MIG devices are not supported as a network GPU identity"})).gpu.unwrap();
    assert_eq!((g.index, g.name.as_str(), g.vram_gb), (2, "RTX", 12.5));
    assert!(g.verified_uuid().unwrap_err().contains("MIG"));
    let g = hello(json!({"index":2,"name":"RTX","vram_gb":12.5,"uuid":null,"uuid_status":"unverified"})).gpu.unwrap();
    assert_eq!(g.verified_uuid(), Err("the GPU identity is unverified".to_string()));
}

#[test]
fn only_the_canonical_lowercase_form_counts_as_verified() {
    for bad in ["GPU-11111111-2222-3333-4444-55555555555", "gpu-11111111-2222-3333-4444-555555555555", "GPU-AAAAAAAA-2222-3333-4444-555555555555",
        "11111111-2222-3333-4444-555555555555", "MIG-11111111-2222-3333-4444-555555555555", "GPU-1111111-12222-3333-4444-555555555555", " GPU-11111111-2222-3333-4444-555555555555", ""] {
        let g = hello(json!({"index":0,"name":"x","vram_gb":1.0,"uuid":bad,"uuid_status":"verified"})).gpu.unwrap();
        assert!(g.verified_uuid().is_err(), "{bad:?}");
    }
    assert!(ck::worker::is_canonical_uuid("GPU-0123abcd-0123-abcd-0123-0123456789ab"));
}
