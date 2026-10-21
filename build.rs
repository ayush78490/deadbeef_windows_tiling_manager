//! Embeds the PerMonitorV2 manifest and the application icon (resource id 1)
//! into the executables. The icon is rasterised here so the repo carries no
//! binary assets.

use std::{env, fs, path::PathBuf};

fn main() {
    println!("cargo:rerun-if-changed=deadbeef.manifest");
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=templates");

    if env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }

    let out = PathBuf::from(env::var("OUT_DIR").unwrap());
    let ico = out.join("deadbeef.ico");
    fs::write(&ico, build_ico(&[16, 20, 24, 32, 48, 64, 256])).expect("write icon");

    let mut res = winres::WindowsResource::new();
    res.set_icon(ico.to_str().unwrap())
        .set_manifest_file("deadbeef.manifest")
        .set("FileDescription", "deadbeef tiling window manager")
        .set("ProductName", "deadbeef")
        .set("LegalCopyright", "MIT");
    res.compile().expect("failed to compile Windows resources (is the Windows SDK / rc.exe installed?)");
}

/// Nord palette.
const BG: [u8; 3] = [0x2e, 0x34, 0x40];
const TILE: [u8; 3] = [0x3b, 0x42, 0x52];
const C0: [u8; 3] = [0x88, 0xc0, 0xd0];
const C1: [u8; 3] = [0x81, 0xa1, 0xc1];

/// Draws a rounded dark square with a cyan gradient border and a dwindle
/// tile pattern: one tall tile on the left, two stacked tiles on the right.
fn render(size: u32) -> Vec<[u8; 4]> {
    let s = size as f32;
    let radius = s * 0.22;
    let border = (s / 16.0).max(1.0);
    let mut px = vec![[0u8; 4]; (size * size) as usize];

    // Signed distance to a rounded rect centred in the icon.
    let sdf = |x: f32, y: f32, inset: f32| -> f32 {
        let half = s / 2.0 - inset;
        let r = (radius - inset).max(0.0);
        let qx = (x - s / 2.0).abs() - (half - r);
        let qy = (y - s / 2.0).abs() - (half - r);
        let ox = qx.max(0.0);
        let oy = qy.max(0.0);
        (ox * ox + oy * oy).sqrt() + qx.max(qy).min(0.0) - r
    };

    let gap = (s / 16.0).max(1.0);
    let inner = border * 2.0 + gap;
    let tiles = [
        (inner, inner, s / 2.0 - gap / 2.0, s - inner),
        (s / 2.0 + gap / 2.0, inner, s - inner, s / 2.0 - gap / 2.0),
        (s / 2.0 + gap / 2.0, s / 2.0 + gap / 2.0, s - inner, s - inner),
    ];

    for y in 0..size {
        for x in 0..size {
            let (fx, fy) = (x as f32 + 0.5, y as f32 + 0.5);
            let d = sdf(fx, fy, 0.5);
            let coverage = (0.5 - d).clamp(0.0, 1.0);
            if coverage <= 0.0 {
                continue;
            }
            let t = ((fx + fy) / (2.0 * s)).clamp(0.0, 1.0);
            let grad = lerp(C0, C1, t);
            let rgb = if d > -border {
                grad
            } else if tiles
                .iter()
                .any(|&(l, tp, r, b)| fx >= l && fx < r && fy >= tp && fy < b)
            {
                if fx < s / 2.0 { lerp(TILE, C0, 0.35) } else { TILE }
            } else {
                BG
            };
            let a = (coverage * 255.0) as u8;
            px[(y * size + x) as usize] = [rgb[2], rgb[1], rgb[0], a]; // BGRA
        }
    }
    px
}

fn lerp(a: [u8; 3], b: [u8; 3], t: f32) -> [u8; 3] {
    let f = |i: usize| (a[i] as f32 + (b[i] as f32 - a[i] as f32) * t) as u8;
    [f(0), f(1), f(2)]
}

/// Builds a multi-resolution .ico with 32bpp BMP-encoded images.
fn build_ico(sizes: &[u32]) -> Vec<u8> {
    let images: Vec<Vec<u8>> = sizes
        .iter()
        .map(|&sz| {
            let px = render(sz);
            let mut bmp = Vec::new();
            // BITMAPINFOHEADER (height doubled: XOR + AND masks).
            bmp.extend_from_slice(&40u32.to_le_bytes());
            bmp.extend_from_slice(&(sz as i32).to_le_bytes());
            bmp.extend_from_slice(&((sz * 2) as i32).to_le_bytes());
            bmp.extend_from_slice(&1u16.to_le_bytes());
            bmp.extend_from_slice(&32u16.to_le_bytes());
            bmp.extend_from_slice(&[0u8; 24]);
            for y in (0..sz).rev() {
                for x in 0..sz {
                    bmp.extend_from_slice(&px[(y * sz + x) as usize]);
                }
            }
            let mask_row = (sz.div_ceil(32) * 4) as usize;
            bmp.extend(std::iter::repeat_n(0u8, mask_row * sz as usize));
            bmp
        })
        .collect();

    let mut ico = Vec::new();
    ico.extend_from_slice(&[0, 0, 1, 0]);
    ico.extend_from_slice(&(sizes.len() as u16).to_le_bytes());
    let mut offset = 6 + 16 * sizes.len() as u32;
    for (sz, img) in sizes.iter().zip(&images) {
        let dim = if *sz >= 256 { 0 } else { *sz as u8 };
        ico.extend_from_slice(&[dim, dim, 0, 0]);
        ico.extend_from_slice(&1u16.to_le_bytes());
        ico.extend_from_slice(&32u16.to_le_bytes());
        ico.extend_from_slice(&(img.len() as u32).to_le_bytes());
        ico.extend_from_slice(&offset.to_le_bytes());
        offset += img.len() as u32;
    }
    for img in images {
        ico.extend_from_slice(&img);
    }
    ico
}
