//! The real icons of programs and agents, for the panel and the tray flyout: the icon of the
//! executable on Windows, the icon of the outermost `.app` bundle on the Mac (a helper inside
//! an app, like Blender's Python or the `claude` inside Claude.app, gets the icon of the app
//! the user recognizes). A program without an icon of its own gets none, and the page keeps
//! its symbol: better than the generic one.
//!
//! Each icon is read once per path and size, off the main thread, and reaches the page as a
//! PNG `data:` URL.

use base64::Engine;
use std::collections::HashMap;
use std::path::Path;
use std::sync::{LazyLock, Mutex};

/// The icons already read, by size and path; `None` for a program without one.
type Icons = HashMap<(u32, String), Option<String>>;
static CACHE: LazyLock<Mutex<Icons>> = LazyLock::new(Mutex::default);

/// The icon of the program at `path`, `size` pixels wide (real pixels: the page asks for
/// twice as many on a 200% screen).
#[tauri::command]
pub async fn app_icon(path: String, size: u32) -> Option<String> {
    let size = size.clamp(16, 256);
    let key = (size, path.clone());
    if let Some(found) = CACHE.lock().unwrap_or_else(|p| p.into_inner()).get(&key) {
        return found.clone();
    }
    let found = tauri::async_runtime::spawn_blocking(move || read(Path::new(&path), size)).await.ok().flatten();
    CACHE.lock().unwrap_or_else(|p| p.into_inner()).insert(key, found.clone());
    found
}

/// The same icon as PNG bytes, for the tray menu. Blocking: off the main thread.
pub fn png(path: &str, size: u32) -> Option<Vec<u8>> {
    let key = (size, path.to_owned());
    let cached = CACHE.lock().unwrap_or_else(|p| p.into_inner()).get(&key).cloned();
    let url = match cached {
        Some(found) => found,
        None => {
            let found = read(Path::new(path), size);
            CACHE.lock().unwrap_or_else(|p| p.into_inner()).insert(key, found.clone());
            found
        }
    }?;
    base64::engine::general_purpose::STANDARD.decode(url.strip_prefix("data:image/png;base64,")?).ok()
}

fn read(path: &Path, size: u32) -> Option<String> {
    if !path.is_absolute() || !path.exists() {
        return None;
    }
    let png = png_of(path, size)?;
    Some(format!("data:image/png;base64,{}", base64::engine::general_purpose::STANDARD.encode(png)))
}

#[cfg(windows)]
fn png_of(path: &Path, size: u32) -> Option<Vec<u8>> {
    let (rgba, width, height) = windows::pixels(path, size)?;
    let mut out = Vec::new();
    {
        let mut encoder = png::Encoder::new(&mut out, width, height);
        encoder.set_color(png::ColorType::Rgba);
        encoder.set_depth(png::BitDepth::Eight);
        let mut writer = encoder.write_header().ok()?;
        writer.write_image_data(&rgba).ok()?;
    }
    Some(out)
}

#[cfg(windows)]
mod windows {
    use std::os::windows::ffi::OsStrExt;
    use std::path::Path;
    use windows_sys::Win32::Graphics::Gdi::{
        BI_RGB, BITMAP, BITMAPINFO, BITMAPINFOHEADER, CreateCompatibleDC, DIB_RGB_COLORS, DeleteDC, DeleteObject, GetDIBits,
        GetObjectW, HBITMAP, HDC,
    };
    use windows_sys::Win32::UI::Shell::{ExtractIconExW, SHDefExtractIconW};
    use windows_sys::Win32::UI::WindowsAndMessaging::{DestroyIcon, GetIconInfo, HICON, ICONINFO};

    /// The first icon of the file, as the shell would draw it at `size`: RGBA pixels, width
    /// and height. An executable without icons of its own gives none.
    pub fn pixels(path: &Path, size: u32) -> Option<(Vec<u8>, u32, u32)> {
        let wide: Vec<u16> = path.as_os_str().encode_wide().chain(Some(0)).collect();
        // Without icons of its own the shell would hand out the generic one.
        // SAFETY: a NUL-terminated path; index -1 only counts the icons.
        if unsafe { ExtractIconExW(wide.as_ptr(), -1, std::ptr::null_mut(), std::ptr::null_mut(), 0) } == 0 {
            return None;
        }
        let mut icon: HICON = std::ptr::null_mut();
        // SAFETY: a NUL-terminated path, and a place for one icon; S_OK (0) only when there is one.
        let result = unsafe { SHDefExtractIconW(wide.as_ptr(), 0, 0, &mut icon, std::ptr::null_mut(), size) };
        if result != 0 || icon.is_null() {
            return None;
        }
        let pixels = unsafe { from_icon(icon) };
        // SAFETY: the icon is ours, from SHDefExtractIconW.
        unsafe { DestroyIcon(icon) };
        pixels
    }

    /// SAFETY: `icon` is a valid icon handle.
    unsafe fn from_icon(icon: HICON) -> Option<(Vec<u8>, u32, u32)> {
        let mut info: ICONINFO = unsafe { std::mem::zeroed() };
        if unsafe { GetIconInfo(icon, &mut info) } == 0 {
            return None;
        }
        let dc = unsafe { CreateCompatibleDC(std::ptr::null_mut()) };
        let result = (|| {
            if info.hbmColor.is_null() {
                return None; // a monochrome icon: the symbol looks better
            }
            let (width, height) = unsafe { dimensions(info.hbmColor) }?;
            let mut color = unsafe { bits(dc, info.hbmColor, width, height) }?;
            // An icon without an alpha channel says what is transparent with its mask.
            if color.as_chunks::<4>().0.iter().all(|p| p[3] == 0) {
                let mask = unsafe { bits(dc, info.hbmMask, width, height) }?;
                for (pixel, m) in color.as_chunks_mut::<4>().0.iter_mut().zip(mask.as_chunks::<4>().0) {
                    pixel[3] = if m[0] == 0 { 255 } else { 0 };
                }
            }
            // BGRA → RGBA.
            for pixel in color.as_chunks_mut::<4>().0 {
                pixel.swap(0, 2);
            }
            Some((color, width as u32, height as u32))
        })();
        unsafe {
            DeleteDC(dc);
            if !info.hbmColor.is_null() {
                DeleteObject(info.hbmColor);
            }
            if !info.hbmMask.is_null() {
                DeleteObject(info.hbmMask);
            }
        }
        result
    }

    /// SAFETY: `bitmap` is a valid bitmap handle.
    unsafe fn dimensions(bitmap: HBITMAP) -> Option<(i32, i32)> {
        let mut info: BITMAP = unsafe { std::mem::zeroed() };
        let read = unsafe { GetObjectW(bitmap, size_of::<BITMAP>() as i32, (&mut info as *mut BITMAP).cast()) };
        (read != 0 && info.bmWidth > 0 && info.bmHeight > 0).then_some((info.bmWidth, info.bmHeight))
    }

    /// The bitmap as 32-bit BGRA rows, top to bottom.
    ///
    /// SAFETY: `dc` and `bitmap` are valid handles, and the bitmap is `width` × `height`.
    unsafe fn bits(dc: HDC, bitmap: HBITMAP, width: i32, height: i32) -> Option<Vec<u8>> {
        let mut info: BITMAPINFO = unsafe { std::mem::zeroed() };
        info.bmiHeader = BITMAPINFOHEADER {
            biSize: size_of::<BITMAPINFOHEADER>() as u32,
            biWidth: width,
            biHeight: -height,
            biPlanes: 1,
            biBitCount: 32,
            biCompression: BI_RGB,
            ..unsafe { std::mem::zeroed() }
        };
        let mut data = vec![0u8; (width * height * 4) as usize];
        let lines = unsafe { GetDIBits(dc, bitmap, 0, height as u32, data.as_mut_ptr().cast(), &mut info, DIB_RGB_COLORS) };
        (lines == height).then_some(data)
    }
}

/// On the Mac: the `.icns` of the outermost app bundle, turned into a PNG by `sips`.
/// The outermost `.app` the executable lives in: the app the user recognizes.
#[cfg(target_os = "macos")]
pub fn bundle_of(path: &Path) -> Option<std::path::PathBuf> {
    let mut bundle = std::path::PathBuf::new();
    for part in path.components() {
        bundle.push(part);
        if part.as_os_str().to_string_lossy().ends_with(".app") {
            return Some(bundle);
        }
    }
    None
}

#[cfg(target_os = "macos")]
fn png_of(path: &Path, size: u32) -> Option<Vec<u8>> {
    use std::process::Command;

    let bundle = bundle_of(path)?;
    let contents = bundle.join("Contents");
    let plist = contents.join("Info.plist");
    let key = |name: &str| -> Option<String> {
        let out = Command::new("/usr/bin/plutil").args(["-extract", name, "raw", "-o", "-"]).arg(&plist).output().ok()?;
        let value = String::from_utf8(out.stdout).ok()?.trim().to_owned();
        (out.status.success() && !value.is_empty()).then_some(value)
    };
    // An app without an icon of its own would get the generic one: better the symbol.
    let name = key("CFBundleIconFile").or_else(|| key("CFBundleIconName"))?;
    let file = if name.ends_with(".icns") { name } else { format!("{name}.icns") };
    let icns = contents.join("Resources").join(file);
    if !icns.is_file() {
        return None;
    }
    let out = std::env::temp_dir().join(format!("clak-icon-{}.png", clak_wire::json::random_hex(8)));
    let status = Command::new("/usr/bin/sips")
        .args(["-s", "format", "png", "-Z", &size.to_string()])
        .arg(&icns)
        .arg("--out")
        .arg(&out)
        .output()
        .ok()?;
    let png = status.status.success().then(|| std::fs::read(&out).ok()).flatten();
    let _ = std::fs::remove_file(&out);
    png
}

#[cfg(not(any(windows, target_os = "macos")))]
fn png_of(_path: &Path, _size: u32) -> Option<Vec<u8>> {
    None
}

#[cfg(all(test, windows))]
mod tests {
    use super::*;

    #[test]
    fn an_executable_gives_its_own_icon_and_one_without_gives_none() {
        let windir = std::env::var("WINDIR").unwrap_or_else(|_| r"C:\Windows".into());
        let notepad = Path::new(&windir).join("System32").join("notepad.exe");
        let png = png_of(&notepad, 48).expect("notepad has an icon");
        assert!(png.starts_with(b"\x89PNG"));
        assert!(read(&notepad, 48).unwrap().starts_with("data:image/png;base64,"));
        // Cargo has no icons of its own.
        assert_eq!(png_of(Path::new(env!("CARGO")), 32), None);
        assert_eq!(read(Path::new("relative.exe"), 32), None);
    }
}
