use super::OVERLAY_SIZE;
use std::mem::{size_of, zeroed};
use std::ptr::{null, null_mut};
use windows_sys::Win32::Foundation::{HWND, POINT, RECT, SIZE};
use windows_sys::Win32::Graphics::Gdi::{
    AC_SRC_ALPHA, AC_SRC_OVER, BI_RGB, BITMAPINFO, BITMAPINFOHEADER, BLENDFUNCTION, CreateBitmap,
    CreateCompatibleDC, CreateDIBSection, DIB_RGB_COLORS, DeleteDC, DeleteObject, GetDC, ReleaseDC,
    SelectObject,
};
use windows_sys::Win32::UI::WindowsAndMessaging::*;

pub(super) unsafe fn draw_layered_glow(hwnd: HWND, pulse: f64, red: bool) {
    let screen = unsafe { GetDC(null_mut()) };
    if screen.is_null() {
        return;
    }
    let memory = unsafe { CreateCompatibleDC(screen) };
    if memory.is_null() {
        unsafe { ReleaseDC(null_mut(), screen) };
        return;
    }
    let mut info: BITMAPINFO = unsafe { zeroed() };
    info.bmiHeader = BITMAPINFOHEADER {
        biSize: size_of::<BITMAPINFOHEADER>() as u32,
        biWidth: OVERLAY_SIZE,
        biHeight: -OVERLAY_SIZE,
        biPlanes: 1,
        biBitCount: 32,
        biCompression: BI_RGB,
        ..unsafe { zeroed() }
    };
    let mut pixels: *mut core::ffi::c_void = null_mut();
    let bitmap =
        unsafe { CreateDIBSection(screen, &info, DIB_RGB_COLORS, &mut pixels, null_mut(), 0) };
    if bitmap.is_null() || pixels.is_null() {
        unsafe {
            DeleteDC(memory);
            ReleaseDC(null_mut(), screen);
        }
        return;
    }
    let previous = unsafe { SelectObject(memory, bitmap) };
    let buffer = unsafe {
        std::slice::from_raw_parts_mut(pixels.cast::<u32>(), (OVERLAY_SIZE * OVERLAY_SIZE) as usize)
    };
    let center = (f64::from(OVERLAY_SIZE) - 1.0) / 2.0;
    let (green_start, green_falloff, blue_start, blue_falloff) = if red {
        (82.0, 60.0, 91.0, 59.0)
    } else {
        (205.0, 156.0, 93.0, 57.0)
    };
    let (glow_red, glow_green, glow_blue) = if red {
        (255.0, 41.0, 59.0)
    } else {
        (255.0, 79.0, 31.0)
    };
    for y in 0..OVERLAY_SIZE {
        for x in 0..OVERLAY_SIZE {
            let dx = f64::from(x) - center;
            let dy = f64::from(y) - center;
            let radius = (dx * dx + dy * dy).sqrt();
            let (red, green, blue, alpha) = if radius < 28.0 {
                let t = (radius / 28.0).clamp(0.0, 1.0);
                let red = 255.0 - t * 18.0;
                let green = green_start - t * green_falloff;
                let blue = blue_start - t * blue_falloff;
                let highlight =
                    ((dx + 8.0).powi(2) / 340.0 + (dy + 11.0).powi(2) / 60.0).clamp(0.0, 1.0);
                (
                    red + (255.0 - red) * (1.0 - highlight) * 0.62,
                    green + (246.0 - green) * (1.0 - highlight) * 0.62,
                    blue + (220.0 - blue) * (1.0 - highlight) * 0.62,
                    238.0 * pulse,
                )
            } else if radius < 34.0 {
                (43.0, 48.0, 56.0, 230.0)
            } else if radius < 41.0 {
                let falloff = 1.0 - (radius - 34.0) / 7.0;
                (
                    glow_red,
                    glow_green,
                    glow_blue,
                    38.0 * falloff * falloff * pulse,
                )
            } else {
                (0.0, 0.0, 0.0, 0.0)
            };
            let a = alpha.clamp(0.0, 255.0) as u32;
            let r = (red.clamp(0.0, 255.0) as u32 * a) / 255;
            let g = (green.clamp(0.0, 255.0) as u32 * a) / 255;
            let b = (blue.clamp(0.0, 255.0) as u32 * a) / 255;
            buffer[(y * OVERLAY_SIZE + x) as usize] = (a << 24) | (r << 16) | (g << 8) | b;
        }
    }
    let mut position: POINT = unsafe { zeroed() };
    let size = SIZE {
        cx: OVERLAY_SIZE,
        cy: OVERLAY_SIZE,
    };
    let source = POINT { x: 0, y: 0 };
    let blend = BLENDFUNCTION {
        BlendOp: AC_SRC_OVER as u8,
        BlendFlags: 0,
        SourceConstantAlpha: 255,
        AlphaFormat: AC_SRC_ALPHA as u8,
    };
    unsafe {
        let mut rect: RECT = zeroed();
        GetWindowRect(hwnd, &mut rect);
        position.x = rect.left;
        position.y = rect.top;
        UpdateLayeredWindow(
            hwnd, screen, &position, &size, memory, &source, 0, &blend, ULW_ALPHA,
        );
        SelectObject(memory, previous);
        DeleteObject(bitmap);
        DeleteDC(memory);
        ReleaseDC(null_mut(), screen);
    }
}

pub(super) fn make_tray_icon() -> HICON {
    unsafe {
        let screen = GetDC(null_mut());
        if screen.is_null() {
            return null_mut();
        }
        let mut info: BITMAPINFO = zeroed();
        info.bmiHeader = BITMAPINFOHEADER {
            biSize: size_of::<BITMAPINFOHEADER>() as u32,
            biWidth: 32,
            biHeight: -32,
            biPlanes: 1,
            biBitCount: 32,
            biCompression: BI_RGB,
            ..zeroed()
        };
        let mut bits: *mut core::ffi::c_void = null_mut();
        let color = CreateDIBSection(screen, &info, DIB_RGB_COLORS, &mut bits, null_mut(), 0);
        ReleaseDC(null_mut(), screen);
        if color.is_null() || bits.is_null() {
            if !color.is_null() {
                DeleteObject(color);
            }
            return null_mut();
        }
        let pixels = std::slice::from_raw_parts_mut(bits.cast::<u32>(), 32 * 32);
        for y in 0..32 {
            for x in 0..32 {
                let dx = x as f64 - 15.5;
                let dy = y as f64 - 15.5;
                let radius = (dx * dx + dy * dy).sqrt();
                let (r, g, b, a) = if radius < 11.0 {
                    (250u32, 105u32, 36u32, 255u32)
                } else if radius < 14.0 {
                    (35, 40, 48, 255)
                } else {
                    (0, 0, 0, 0)
                };
                pixels[y * 32 + x] =
                    (a << 24) | ((r * a / 255) << 16) | ((g * a / 255) << 8) | (b * a / 255);
            }
        }
        let mask = CreateBitmap(32, 32, 1, 1, null());
        if mask.is_null() {
            DeleteObject(color);
            return null_mut();
        }
        let icon_info = ICONINFO {
            fIcon: 1,
            xHotspot: 0,
            yHotspot: 0,
            hbmMask: mask,
            hbmColor: color,
        };
        let icon = CreateIconIndirect(&icon_info);
        DeleteObject(mask);
        DeleteObject(color);
        icon
    }
}
