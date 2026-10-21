//! Image decoding via the Windows Imaging Component (WIC).
//!
//! All results are 32bpp premultiplied BGRA, ready for `Canvas::bitmap`.
//! Callers must have COM initialised on the current thread.

use windows::Win32::Graphics::Imaging::*;
use windows::Win32::System::Com::{CoCreateInstance, CLSCTX_INPROC_SERVER};
use windows::Win32::UI::Shell::SHCreateMemStream;
use windows::Win32::UI::WindowsAndMessaging::HICON;

#[derive(Debug, Clone)]
pub struct Pixels {
    pub w: u32,
    pub h: u32,
    pub data: Vec<u8>,
}

fn factory() -> Result<IWICImagingFactory, String> {
    unsafe { CoCreateInstance(&CLSID_WICImagingFactory, None, CLSCTX_INPROC_SERVER) }.map_err(|e| format!("WIC: {e}"))
}

fn to_pixels(f: &IWICImagingFactory, src: &IWICBitmapSource) -> Result<Pixels, String> {
    unsafe {
        let conv = f.CreateFormatConverter().map_err(|e| e.to_string())?;
        conv.Initialize(src, &GUID_WICPixelFormat32bppPBGRA, WICBitmapDitherTypeNone, None, 0.0, WICBitmapPaletteTypeCustom)
            .map_err(|e| format!("format conversion: {e}"))?;
        let (mut w, mut h) = (0u32, 0u32);
        conv.GetSize(&mut w, &mut h).map_err(|e| e.to_string())?;
        let mut data = vec![0u8; (w * h * 4) as usize];
        conv.CopyPixels(std::ptr::null(), w * 4, &mut data).map_err(|e| format!("CopyPixels: {e}"))?;
        Ok(Pixels { w, h, data })
    }
}

fn frame(f: &IWICImagingFactory, bytes: &[u8]) -> Result<IWICBitmapSource, String> {
    unsafe {
        let stream = SHCreateMemStream(Some(bytes)).ok_or("SHCreateMemStream failed")?;
        let decoder = f
            .CreateDecoderFromStream(&stream, std::ptr::null(), WICDecodeMetadataCacheOnDemand)
            .map_err(|e| format!("unsupported image: {e}"))?;
        let frame = decoder.GetFrame(0).map_err(|e| e.to_string())?;
        windows::core::Interface::cast(&frame).map_err(|e: windows::core::Error| e.to_string())
    }
}

/// Size of an encoded image without decoding all pixels.
pub fn dimensions(bytes: &[u8]) -> Result<(u32, u32), String> {
    let f = factory()?;
    let src = frame(&f, bytes)?;
    let (mut w, mut h) = (0, 0);
    unsafe { src.GetSize(&mut w, &mut h).map_err(|e| e.to_string())? };
    Ok((w, h))
}

/// Centre-crops to the target aspect ratio, then scales to exactly `w`×`h`
/// — the same framing as the Windows "Fill" wallpaper position.
pub fn decode_fill(bytes: &[u8], w: u32, h: u32) -> Result<Pixels, String> {
    let f = factory()?;
    let src = frame(&f, bytes)?;
    unsafe {
        let (mut sw, mut sh) = (0u32, 0u32);
        src.GetSize(&mut sw, &mut sh).map_err(|e| e.to_string())?;
        let (crop_w, crop_h) = fill_crop(sw, sh, w, h);
        let clipper = f.CreateBitmapClipper().map_err(|e| e.to_string())?;
        let rect = WICRect {
            X: ((sw - crop_w) / 2) as i32,
            Y: ((sh - crop_h) / 2) as i32,
            Width: crop_w as i32,
            Height: crop_h as i32,
        };
        clipper.Initialize(&src, &rect).map_err(|e| format!("clip: {e}"))?;
        let scaler = f.CreateBitmapScaler().map_err(|e| e.to_string())?;
        scaler.Initialize(&clipper, w, h, WICBitmapInterpolationModeHighQualityCubic).map_err(|e| format!("scale: {e}"))?;
        to_pixels(&f, &windows::core::Interface::cast(&scaler).map_err(|e: windows::core::Error| e.to_string())?)
    }
}

/// Largest centred crop of a `sw`×`sh` image with the aspect ratio of `w`×`h`.
pub fn fill_crop(sw: u32, sh: u32, w: u32, h: u32) -> (u32, u32) {
    let (sw64, sh64, w64, h64) = (sw as u64, sh as u64, w.max(1) as u64, h.max(1) as u64);
    if sw64 * h64 > sh64 * w64 {
        (((sh64 * w64) / h64).max(1) as u32, sh)
    } else {
        (sw, ((sw64 * h64) / w64).max(1) as u32)
    }
}

/// Converts a shell-rendered HBITMAP (32bpp with alpha) to pixels.
pub fn from_hbitmap(hbm: windows::Win32::Graphics::Gdi::HBITMAP) -> Option<Pixels> {
    let f = factory().ok()?;
    unsafe {
        let bmp = f.CreateBitmapFromHBITMAP(hbm, None, WICBitmapUsePremultipliedAlpha).ok()?;
        to_pixels(&f, &windows::core::Interface::cast(&bmp).ok()?).ok()
    }
}

/// Renders an icon handle to pixels (for app icons in the bar).
pub fn icon(hicon: HICON) -> Option<Pixels> {
    let f = factory().ok()?;
    unsafe {
        let bmp = f.CreateBitmapFromHICON(hicon).ok()?;
        to_pixels(&f, &windows::core::Interface::cast(&bmp).ok()?).ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn crop_math() {
        assert_eq!(fill_crop(4000, 2000, 1920, 1080), (3555, 2000));
        assert_eq!(fill_crop(1000, 1000, 1920, 1080), (1000, 562));
        assert_eq!(fill_crop(1920, 1080, 1920, 1080), (1920, 1080));
    }

    #[test]
    fn decodes_png() {
        unsafe {
            let _ = windows::Win32::System::Com::CoInitializeEx(None, windows::Win32::System::Com::COINIT_MULTITHREADED);
        }
        // 2x1 PNG: red, green.
        let png: &[u8] = &[
            0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A, 0x00, 0x00, 0x00, 0x0D, 0x49, 0x48, 0x44, 0x52, 0x00, 0x00,
            0x00, 0x02, 0x00, 0x00, 0x00, 0x01, 0x08, 0x02, 0x00, 0x00, 0x00, 0x7B, 0x40, 0xE8, 0xDD, 0x00, 0x00, 0x00,
            0x0F, 0x49, 0x44, 0x41, 0x54, 0x78, 0x9C, 0x63, 0xF8, 0xCF, 0xC0, 0xC0, 0xF0, 0x9F, 0x01, 0x00, 0x07, 0xFF,
            0x01, 0xFF, 0x01, 0x7F, 0x89, 0xA7, 0x00, 0x00, 0x00, 0x00, 0x49, 0x45, 0x4E, 0x44, 0xAE, 0x42, 0x60, 0x82,
        ];
        assert_eq!(dimensions(png).unwrap(), (2, 1));
        let p = decode_fill(png, 4, 2).unwrap();
        assert_eq!((p.w, p.h, p.data.len()), (4, 2, 32));
    }
}
