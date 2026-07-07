//! Convert Ryujinx EffectCapture frame dumps (frames/*.rgba.gz) to PNGs.
//!
//! Filenames encode geometry and host format:
//!   frame_%06d_%dx%d_%s.rgba.gz   e.g. frame_000312_1280x720_B8G8R8A8Unorm.rgba.gz
//! BGRA-family formats are swizzled to RGBA; everything else is assumed RGBA byte order.
//!
//! Usage: capture_frames_to_png <capture_dir_or_frames_dir> [out_dir] [--frames start-end]
//!
//! Default out_dir is <frames_dir>/png. Alpha is forced opaque (the swapchain image
//! carries whatever alpha the game left; for visual diffing we want the composited RGB).

use flate2::read::GzDecoder;
use std::io::Read;
use std::path::{Path, PathBuf};

struct FrameFile {
    path: PathBuf,
    frame: u64,
    width: u32,
    height: u32,
    bgra: bool,
}

fn parse_name(path: &Path) -> Option<FrameFile> {
    let name = path.file_name()?.to_str()?;
    let stem = name.strip_suffix(".rgba.gz")?;
    // frame_<frame>_<w>x<h>_<format>
    let rest = stem.strip_prefix("frame_")?;
    let mut parts = rest.splitn(3, '_');
    let frame = parts.next()?.parse().ok()?;
    let (w, h) = parts.next()?.split_once('x')?;
    let format = parts.next().unwrap_or("");
    Some(FrameFile {
        path: path.to_path_buf(),
        frame,
        width: w.parse().ok()?,
        height: h.parse().ok()?,
        bgra: format.starts_with("B8G8R8A8") || format.starts_with("Bgra"),
    })
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mut window: Option<(u64, u64)> = None;
    let mut positional = Vec::new();
    let mut i = 0;
    while i < args.len() {
        if args[i] == "--frames" {
            let spec = args.get(i + 1).expect("--frames start-end");
            let (s, e) = spec.split_once('-').expect("--frames start-end");
            window = Some((s.parse().unwrap(), e.parse().unwrap()));
            i += 2;
        } else {
            positional.push(args[i].clone());
            i += 1;
        }
    }
    let root = PathBuf::from(positional.first().expect(
        "usage: capture_frames_to_png <capture_dir_or_frames_dir> [out_dir] [--frames start-end]",
    ));
    let frames_dir = if root.join("frames").is_dir() { root.join("frames") } else { root };
    let out_dir = positional
        .get(1)
        .map(PathBuf::from)
        .unwrap_or_else(|| frames_dir.join("png"));
    std::fs::create_dir_all(&out_dir).expect("create out dir");

    let mut files: Vec<FrameFile> = std::fs::read_dir(&frames_dir)
        .expect("read frames dir")
        .filter_map(|e| parse_name(&e.ok()?.path()))
        .filter(|f| window.is_none_or(|(s, e)| f.frame >= s && f.frame <= e))
        .collect();
    files.sort_by_key(|f| f.frame);
    if files.is_empty() {
        eprintln!("no frame_*.rgba.gz files in {}", frames_dir.display());
        std::process::exit(1);
    }

    let mut converted = 0usize;
    for f in &files {
        let compressed = std::fs::read(&f.path).expect("read dump");
        let mut pixels = Vec::with_capacity((f.width * f.height * 4) as usize);
        GzDecoder::new(compressed.as_slice())
            .read_to_end(&mut pixels)
            .expect("gunzip");
        let expected = (f.width as usize) * (f.height as usize) * 4;
        if pixels.len() < expected {
            eprintln!(
                "frame {}: short read {} < {} bytes, skipping",
                f.frame,
                pixels.len(),
                expected
            );
            continue;
        }
        pixels.truncate(expected);
        for px in pixels.chunks_exact_mut(4) {
            if f.bgra {
                px.swap(0, 2);
            }
            px[3] = 0xFF;
        }
        let out = out_dir.join(format!("frame_{:06}.png", f.frame));
        image::save_buffer(&out, &pixels, f.width, f.height, image::ColorType::Rgba8)
            .expect("write png");
        converted += 1;
    }
    println!(
        "{} of {} frames -> {} (frames {}..{})",
        converted,
        files.len(),
        out_dir.display(),
        files.first().unwrap().frame,
        files.last().unwrap().frame
    );
}
