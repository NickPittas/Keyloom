//! PNG / EXR decoding. `read_exr_raw` returns untouched channel samples (for numeric verification);
//! `decode_display` maps them to straight-alpha RGBA8 for the viewer.
#![allow(clippy::chunks_exact_to_as_chunks)] // as_chunks needs array-typed slices; not worth the churn
use exr::prelude::*;
use std::fs::File;
use std::path::Path;
use std::sync::OnceLock;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Interp {
    /// Values are already display-encoded (video-extracted EXR input, PNG).
    Passthrough,
    /// Linear light → sRGB OETF.
    LinearToSrgb,
    /// As `LinearToSrgb`, but the RGBA is premultiplied linear (legacy Processed EXR): divide by alpha first.
    LinearToSrgbPremul,
    /// Scalar matte: clamp to [0,1], shown as honest grey (no gamma lift).
    Matte,
}

#[derive(Debug, Clone)]
pub struct RawChannel {
    pub name: String,
    pub sample_type: &'static str,
    pub data: Vec<f32>,
}

#[derive(Debug, Clone)]
pub struct RawExr {
    pub width: usize,
    pub height: usize,
    pub compression: String,
    pub channels: Vec<RawChannel>,
}

#[derive(Debug, Clone)]
pub struct DisplayFrame {
    pub width: usize,
    pub height: usize,
    /// Straight (unassociated) alpha, sRGB-encoded colour.
    pub rgba: Vec<u8>,
    pub has_alpha: bool,
    pub clipped_hdr: bool,
    pub info: String,
}

impl DisplayFrame {
    pub fn bytes(&self) -> usize {
        self.rgba.len()
    }
}

fn type_name(s: &FlatSamples) -> &'static str {
    match s {
        FlatSamples::F16(_) => "half",
        FlatSamples::F32(_) => "float",
        FlatSamples::U32(_) => "uint",
    }
}

fn sample(s: &FlatSamples, i: usize) -> f32 {
    match s {
        FlatSamples::F16(v) => v[i].to_f32(),
        FlatSamples::F32(v) => v[i],
        FlatSamples::U32(v) => v[i] as f32,
    }
}

type Loaded = Image<Layer<AnyChannels<FlatSamples>>>;

/// Every channel must have exactly w*h samples; subsampled/odd channels are rejected instead of indexed.
fn check_channels(img: &Loaded, path: &Path) -> std::result::Result<(), String> {
    let n = img.layer_data.size.area();
    for c in &img.layer_data.channel_data.list {
        let len = match &c.sample_data {
            FlatSamples::F16(v) => v.len(),
            FlatSamples::F32(v) => v.len(),
            FlatSamples::U32(v) => v.len(),
        };
        if len != n || c.sampling != Vec2(1, 1) {
            return Err(format!(
                "{}: channel {} is subsampled or malformed ({len} samples for {n} pixels)",
                path.display(),
                c.name
            ));
        }
    }
    Ok(())
}

fn load_exr(path: &Path) -> std::result::Result<Loaded, String> {
    let img = load_exr_unchecked(path)?;
    check_channels(&img, path)?;
    Ok(img)
}

fn load_exr_unchecked(path: &Path) -> std::result::Result<Loaded, String> {
    read()
        .no_deep_data()
        .largest_resolution_level()
        .all_channels()
        .first_valid_layer()
        .all_attributes()
        .from_file(path)
        .map_err(|e| format!("{}: {e}", path.display()))
}

pub fn read_exr_raw(path: &Path) -> std::result::Result<RawExr, String> {
    let img = load_exr(path)?;
    let l = &img.layer_data;
    Ok(RawExr {
        width: l.size.width(),
        height: l.size.height(),
        compression: format!("{:?}", l.encoding.compression),
        channels: l
            .channel_data
            .list
            .iter()
            .map(|c| RawChannel {
                name: c.name.to_string(),
                sample_type: type_name(&c.sample_data),
                data: (0..l.size.area())
                    .map(|i| sample(&c.sample_data, i))
                    .collect(),
            })
            .collect(),
    })
}

const LUT_N: usize = 16384;
fn srgb_lut() -> &'static [u8] {
    static LUT: OnceLock<Vec<u8>> = OnceLock::new();
    LUT.get_or_init(|| {
        (0..=LUT_N)
            .map(|i| {
                let l = i as f32 / LUT_N as f32;
                let s = if l <= 0.003_130_8 {
                    l * 12.92
                } else {
                    1.055 * l.powf(1.0 / 2.4) - 0.055
                };
                (s * 255.0 + 0.5) as u8
            })
            .collect()
    })
}

pub fn linear_to_srgb8(v: f32) -> u8 {
    srgb_lut()[(v.clamp(0.0, 1.0) * LUT_N as f32 + 0.5) as usize]
}

fn find<'a>(img: &'a Loaded, name: &str) -> Option<&'a FlatSamples> {
    img.layer_data
        .channel_data
        .list
        .iter()
        .find(|c| c.name.to_string().eq_ignore_ascii_case(name))
        .map(|c| &c.sample_data)
}

fn decode_exr_display(path: &Path, interp: Interp) -> std::result::Result<DisplayFrame, String> {
    let img = load_exr(path)?;
    let l = &img.layer_data;
    let (w, h) = (l.size.width(), l.size.height());
    let n = w * h;
    let comp = format!("{:?}", l.encoding.compression);
    let rgb = [find(&img, "R"), find(&img, "G"), find(&img, "B")];
    let alpha = find(&img, "A");
    let mut out = vec![255u8; n * 4];
    let mut clipped = false;
    let mut track = |v: f32| {
        if !(0.0..=1.0).contains(&v) && v.is_finite() {
            clipped = true;
        }
    };
    let (desc, has_alpha);
    if let [Some(r), Some(g), Some(b)] = rgb {
        desc = if alpha.is_some() { "RGBA" } else { "RGB" };
        has_alpha = alpha.is_some();
        for i in 0..n {
            let a = alpha.map_or(1.0, |a| sample(a, i));
            for (k, s) in [r, g, b].into_iter().enumerate() {
                let mut v = sample(s, i);
                if interp == Interp::LinearToSrgbPremul && alpha.is_some() {
                    v = if a > 0.0 { v / a } else { 0.0 };
                }
                track(v);
                out[i * 4 + k] = match interp {
                    Interp::LinearToSrgb | Interp::LinearToSrgbPremul => linear_to_srgb8(v),
                    _ => (v.clamp(0.0, 1.0) * 255.0 + 0.5) as u8,
                };
            }
            if let Some(a) = alpha {
                out[i * 4 + 3] = (sample(a, i).clamp(0.0, 1.0) * 255.0 + 0.5) as u8;
            }
        }
    } else {
        // Scalar image: Y, or the first channel as a fallback. Never reinterpreted as colour.
        let y = find(&img, "Y")
            .or_else(|| l.channel_data.list.first().map(|c| &c.sample_data))
            .ok_or("EXR has no channels")?;
        desc = "Y";
        has_alpha = false;
        for i in 0..n {
            let v = sample(y, i);
            track(v);
            let g = (v.clamp(0.0, 1.0) * 255.0 + 0.5) as u8;
            out[i * 4..i * 4 + 3].fill(g);
        }
    }
    let ty = l
        .channel_data
        .list
        .first()
        .map(|c| type_name(&c.sample_data))
        .unwrap_or("?");
    Ok(DisplayFrame {
        width: w,
        height: h,
        rgba: out,
        has_alpha,
        clipped_hdr: clipped,
        info: format!("EXR {desc} {ty} {comp}"),
    })
}

fn decode_png(path: &Path, interp: Interp) -> std::result::Result<DisplayFrame, String> {
    let err = |e: &dyn std::fmt::Display| format!("{}: {e}", path.display());
    let mut dec = png::Decoder::new(std::io::BufReader::new(
        File::open(path).map_err(|e| err(&e))?,
    ));
    dec.set_transformations(png::Transformations::EXPAND | png::Transformations::STRIP_16);
    let mut rd = dec.read_info().map_err(|e| err(&e))?;
    let mut buf = vec![0u8; rd.output_buffer_size().ok_or("PNG too large")?];
    let info = rd.next_frame(&mut buf).map_err(|e| err(&e))?;
    let (w, h) = (info.width as usize, info.height as usize);
    let src = &buf[..info.buffer_size()];
    let mut out = Vec::with_capacity(w * h * 4);
    let has_alpha = matches!(
        info.color_type,
        png::ColorType::Rgba | png::ColorType::GrayscaleAlpha
    );
    match info.color_type {
        png::ColorType::Grayscale => src
            .iter()
            .for_each(|&g| out.extend_from_slice(&[g, g, g, 255])),
        png::ColorType::GrayscaleAlpha => src
            .chunks_exact(2)
            .for_each(|p| out.extend_from_slice(&[p[0], p[0], p[0], p[1]])),
        png::ColorType::Rgb => src
            .chunks_exact(3)
            .for_each(|p| out.extend_from_slice(&[p[0], p[1], p[2], 255])),
        png::ColorType::Rgba => out.extend_from_slice(src),
        png::ColorType::Indexed => return Err(err(&"indexed PNG not expanded")),
    }
    if interp == Interp::Matte {
        // Hint/matte PNG: grey from the first channel, opaque.
        out.chunks_exact_mut(4).for_each(|p| {
            p[1] = p[0];
            p[2] = p[0];
            p[3] = 255;
        });
    }
    Ok(DisplayFrame {
        width: w,
        height: h,
        rgba: out,
        has_alpha: has_alpha && interp != Interp::Matte,
        clipped_hdr: false,
        info: format!(
            "PNG {:?} {:?}-bit(8 shown)",
            info.color_type, info.bit_depth
        ),
    })
}

pub fn decode_display(path: &Path, interp: Interp) -> std::result::Result<DisplayFrame, String> {
    match crate::project::ext_of(path).as_str() {
        "exr" => decode_exr_display(path, interp),
        "png" => decode_png(path, interp),
        e => Err(format!("{}: unsupported image type .{e}", path.display())),
    }
}

/// Cheap header read for the Properties/diagnostic panel (no pixel decode).
pub fn exr_header_summary(path: &Path) -> std::result::Result<String, String> {
    let m = exr::meta::MetaData::read_from_file(path, false).map_err(|e| e.to_string())?;
    let h = m.headers.first().ok_or("no header")?;
    let ch: Vec<String> = h.channels.list.iter().map(|c| c.name.to_string()).collect();
    Ok(format!(
        "{}×{} {:?} [{}]",
        h.layer_size.width(),
        h.layer_size.height(),
        h.compression,
        ch.join(",")
    ))
}

#[cfg(test)]
pub mod testutil {
    use super::*;
    /// Write a synthetic EXR with the given channels (`(name, values)`), half or float.
    pub fn write_exr(
        path: &Path,
        w: usize,
        h: usize,
        comp: Compression,
        half: bool,
        chans: &[(&str, Vec<f32>)],
    ) {
        let mut list = chans
            .iter()
            .map(|(n, v)| {
                let s = if half {
                    FlatSamples::F16(v.iter().map(|x| f16::from_f32(*x)).collect())
                } else {
                    FlatSamples::F32(v.clone())
                };
                AnyChannel::new(*n, s)
            })
            .collect::<Vec<_>>();
        list.sort_by(|a, b| a.name.cmp(&b.name));
        let layer = Layer::new(
            (w, h),
            LayerAttributes::default(),
            Encoding {
                compression: comp,
                ..Encoding::FAST_LOSSLESS
            },
            AnyChannels::sort(list.into_iter().collect()),
        );
        Image::from_layer(layer).write().to_file(path).unwrap();
    }
}

#[cfg(test)]
mod tests {
    use super::testutil::write_exr;
    use super::*;

    fn tmp(name: &str) -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!("ck-decode-{}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        d.join(name)
    }

    #[test]
    fn y_matte_all_lossless_compressions() {
        for (i, comp) in [
            Compression::Uncompressed,
            Compression::ZIP16,
            Compression::PIZ,
            Compression::ZIP1,
        ]
        .into_iter()
        .enumerate()
        {
            let p = tmp(&format!("y{i}.exr"));
            let vals: Vec<f32> = (0..16 * 8).map(|i| i as f32 / 127.0).collect();
            write_exr(&p, 16, 8, comp, false, &[("Y", vals.clone())]);
            let raw = read_exr_raw(&p).unwrap();
            assert_eq!(raw.channels.len(), 1);
            assert_eq!(raw.channels[0].name, "Y");
            assert_eq!(raw.channels[0].data, vals);
            let d = decode_display(&p, Interp::Matte).unwrap();
            assert_eq!((d.width, d.height, d.has_alpha), (16, 8, false));
            let px = &d.rgba[5 * 4..5 * 4 + 4];
            assert_eq!(px[0], px[1]);
            assert_eq!(px[0], ((5.0f32 / 127.0) * 255.0 + 0.5) as u8);
            assert!(d.info.contains("Y"));
        }
    }

    #[test]
    fn rgba_linear_to_srgb_and_straight_alpha() {
        let p = tmp("rgba.exr");
        let n = 4 * 4;
        write_exr(
            &p,
            4,
            4,
            Compression::PIZ,
            false,
            &[
                ("R", vec![0.18; n]),
                ("G", vec![0.0; n]),
                ("B", vec![1.0; n]),
                ("A", vec![0.5; n]),
            ],
        );
        let d = decode_display(&p, Interp::LinearToSrgb).unwrap();
        assert!(d.has_alpha && !d.clipped_hdr);
        let px = &d.rgba[0..4];
        assert!(
            (px[0] as i32 - 118).abs() <= 1,
            "0.18 linear → {} (expected ~118)",
            px[0]
        );
        assert_eq!((px[1], px[2], px[3]), (0, 255, 128));
        // Passthrough does not gamma-encode.
        let d = decode_display(&p, Interp::Passthrough).unwrap();
        assert_eq!(d.rgba[0], (0.18f32 * 255.0 + 0.5) as u8);
    }

    #[test]
    fn premultiplied_processed_is_unpremultiplied_before_encode() {
        let p = tmp("premul.exr");
        // premultiplied linear: colour 0.09 at alpha 0.5 is straight 0.18
        write_exr(
            &p,
            2,
            1,
            Compression::Uncompressed,
            false,
            &[
                ("R", vec![0.09, 0.5]),
                ("G", vec![0.0; 2]),
                ("B", vec![0.0; 2]),
                ("A", vec![0.5, 0.0]),
            ],
        );
        let d = decode_display(&p, Interp::LinearToSrgbPremul).unwrap();
        assert!((d.rgba[0] as i32 - 118).abs() <= 1, "{}", d.rgba[0]);
        assert_eq!(d.rgba[3], 128);
        assert_eq!(d.rgba[4], 0, "alpha 0 must not divide by zero");
        let straight = decode_display(&p, Interp::LinearToSrgb).unwrap();
        assert!(
            straight.rgba[0] < 100,
            "straight reading of premultiplied data is darker"
        );
    }

    #[test]
    fn subsampled_or_short_channel_is_an_error_not_an_index_panic() {
        // Built in memory: the writer refuses subsampling, but a hostile file can carry it.
        let img = |len: usize, sampling: Vec2<usize>| {
            let mut c = AnyChannel::new("C", FlatSamples::F32(vec![0.5; len]));
            c.sampling = sampling;
            Image::from_layer(Layer::new(
                (4, 4),
                LayerAttributes::default(),
                Encoding::FAST_LOSSLESS,
                AnyChannels::sort([c].into_iter().collect()),
            ))
        };
        let p = Path::new("x.exr");
        assert!(check_channels(&img(16, Vec2(1, 1)), p).is_ok());
        assert!(check_channels(&img(4, Vec2(2, 2)), p)
            .unwrap_err()
            .contains("subsampled"));
        assert!(check_channels(&img(15, Vec2(1, 1)), p).is_err());
    }

    #[test]
    fn hdr_and_negative_flag() {
        let p = tmp("hdr.exr");
        write_exr(
            &p,
            2,
            1,
            Compression::Uncompressed,
            true,
            &[
                ("R", vec![4.0, -1.0]),
                ("G", vec![0.0; 2]),
                ("B", vec![0.0; 2]),
            ],
        );
        let d = decode_display(&p, Interp::LinearToSrgb).unwrap();
        assert!(d.clipped_hdr);
        assert_eq!((d.rgba[0], d.rgba[4]), (255, 0));
        assert!(d.info.contains("half"));
    }

    #[test]
    fn png_gray_rgb_rgba_matte() {
        let enc = |p: &Path, ct, data: &[u8]| {
            let mut e = png::Encoder::new(std::io::BufWriter::new(File::create(p).unwrap()), 2, 1);
            e.set_color(ct);
            e.set_depth(png::BitDepth::Eight);
            e.write_header().unwrap().write_image_data(data).unwrap();
        };
        let p = tmp("g.png");
        enc(&p, png::ColorType::Grayscale, &[10, 200]);
        let d = decode_display(&p, Interp::Matte).unwrap();
        assert_eq!(&d.rgba, &[10, 10, 10, 255, 200, 200, 200, 255]);
        let p = tmp("c.png");
        enc(&p, png::ColorType::Rgba, &[1, 2, 3, 4, 5, 6, 7, 8]);
        let d = decode_display(&p, Interp::Passthrough).unwrap();
        assert_eq!(&d.rgba, &[1, 2, 3, 4, 5, 6, 7, 8]);
        assert!(d.has_alpha);
    }
}
