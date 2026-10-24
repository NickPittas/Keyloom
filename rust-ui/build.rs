//! Windows only: embeds the approved Keyloom icon (`ui/theme/keyloom.ico`, converted from the selected master)
//! and the product name into `keyloom-ui.exe` as a compiled `.res` that MSVC `link.exe` accepts directly.
//! Written by hand so no resource-compiler crate or Windows SDK tool is needed.
use std::path::{Path, PathBuf};

fn u16le(v: &mut Vec<u8>, x: u16) {
    v.extend_from_slice(&x.to_le_bytes());
}
fn u32le(v: &mut Vec<u8>, x: u32) {
    v.extend_from_slice(&x.to_le_bytes());
}
fn pad4(v: &mut Vec<u8>) {
    while v.len() % 4 != 0 {
        v.push(0);
    }
}
fn wstr(s: &str) -> Vec<u8> {
    s.encode_utf16().chain([0]).flat_map(|c| c.to_le_bytes()).collect()
}

/// One `.res` record with numeric type and name ids.
fn record(out: &mut Vec<u8>, ty: u16, name: u16, data: &[u8]) {
    u32le(out, data.len() as u32);
    u32le(out, 32);
    for x in [0xFFFF, ty, 0xFFFF, name] {
        u16le(out, x);
    }
    u32le(out, 0); // data version
    u16le(out, 0x0030); // MOVEABLE | PURE
    u16le(out, 0); // language neutral
    u32le(out, 0);
    u32le(out, 0);
    out.extend_from_slice(data);
    pad4(out);
}

/// A VERSIONINFO node: length, value length, type, key, 4-aligned value, then children.
fn node(key: &str, value: &[u8], vlen: u16, text: bool, children: &[Vec<u8>]) -> Vec<u8> {
    let mut b = vec![0, 0];
    u16le(&mut b, vlen);
    u16le(&mut b, u16::from(text));
    b.extend(wstr(key));
    pad4(&mut b);
    b.extend_from_slice(value);
    pad4(&mut b);
    for c in children {
        b.extend_from_slice(c);
        pad4(&mut b);
    }
    let n = b.len() as u16;
    b[..2].copy_from_slice(&n.to_le_bytes());
    b
}

fn version_info(version: &str) -> Vec<u8> {
    let mut p = version.split('.').map(|x| x.parse::<u16>().unwrap_or(0));
    let (a, b, c) = (p.next().unwrap_or(0), p.next().unwrap_or(0), p.next().unwrap_or(0));
    let mut fixed = Vec::new();
    for x in [0xFEEF04BD, 0x0001_0000, u32::from(a) << 16 | u32::from(b), u32::from(c) << 16] {
        u32le(&mut fixed, x);
    }
    for x in [u32::from(a) << 16 | u32::from(b), u32::from(c) << 16, 0x3F, 0, 0x0004_0004, 1, 0, 0, 0] {
        u32le(&mut fixed, x);
    }
    let strings: Vec<Vec<u8>> = [
        ("CompanyName", "Keyloom"),
        ("FileDescription", "Keyloom"),
        ("FileVersion", version),
        ("InternalName", "keyloom-ui"),
        ("OriginalFilename", "keyloom-ui.exe"),
        ("ProductName", "Keyloom"),
        ("ProductVersion", version),
        ("LegalCopyright", "Keyloom, powered by CorridorKey"),
    ]
    .iter()
    .map(|(k, v)| node(k, &wstr(v), v.encode_utf16().count() as u16 + 1, true, &[]))
    .collect();
    let table = node("040904B0", &[], 0, true, &strings);
    let sfi = node("StringFileInfo", &[], 0, true, &[table]);
    let mut tr = Vec::new();
    u32le(&mut tr, 0x04B0_0409);
    let var = node("Translation", &tr, 4, false, &[]);
    let vfi = node("VarFileInfo", &[], 0, true, &[var]);
    node("VS_VERSION_INFO", &fixed, 52, false, &[sfi, vfi])
}

fn resource(ico: &Path, version: &str) -> Vec<u8> {
    let ico = std::fs::read(ico).unwrap_or_else(|e| panic!("{}: {e}", ico.display()));
    assert!(ico.len() > 6 && ico[2] == 1 && ico[3] == 0, "not an .ico file");
    let count = u16::from_le_bytes([ico[4], ico[5]]) as usize;
    let mut out = Vec::new();
    record(&mut out, 0, 0, &[]); // mandatory empty first record
    let mut group = Vec::new();
    for x in [0, 1, count as u16] {
        u16le(&mut group, x);
    }
    for i in 0..count {
        let e = &ico[6 + i * 16..22 + i * 16];
        let size = u32::from_le_bytes(e[8..12].try_into().unwrap()) as usize;
        let off = u32::from_le_bytes(e[12..16].try_into().unwrap()) as usize;
        let id = i as u16 + 1;
        record(&mut out, 3, id, &ico[off..off + size]);
        group.extend_from_slice(&e[..12]);
        u16le(&mut group, id);
    }
    record(&mut out, 14, 1, &group);
    record(&mut out, 16, 1, &version_info(version));
    out
}

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=../ui/theme/keyloom.ico");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") || std::env::var("CARGO_CFG_TARGET_ENV").as_deref() != Ok("msvc") {
        return;
    }
    let root = PathBuf::from(std::env::var_os("CARGO_MANIFEST_DIR").unwrap());
    let res = PathBuf::from(std::env::var_os("OUT_DIR").unwrap()).join("keyloom.res");
    std::fs::write(&res, resource(&root.join("../ui/theme/keyloom.ico"), &std::env::var("CARGO_PKG_VERSION").unwrap())).unwrap();
    println!("cargo:rustc-link-arg-bin=keyloom-ui={}", res.display());
}
