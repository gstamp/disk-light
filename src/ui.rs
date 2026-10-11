use crate::activity::{DiskReading, MonitorSnapshot};
use crate::config::{OverlayPosition, Settings};
use crate::monitor::{MonitorService, WM_APP_MONITOR_UPDATE};
use std::collections::HashSet;
use std::mem::{size_of, zeroed};
use std::ptr::{null, null_mut};
use std::sync::mpsc::Receiver;
use windows_sys::Win32::Foundation::{
    CloseHandle, ERROR_ALREADY_EXISTS, GetLastError, HANDLE, HINSTANCE, HWND, LPARAM, LRESULT,
    POINT, RECT, SIZE, WPARAM,
};
use windows_sys::Win32::Graphics::Gdi::{
    AC_SRC_ALPHA, AC_SRC_OVER, BI_RGB, BITMAPINFO, BITMAPINFOHEADER, BLENDFUNCTION, BeginPaint,
    COLOR_WINDOW, CreateBitmap, CreateCompatibleDC, CreateDIBSection, CreateSolidBrush,
    DIB_RGB_COLORS, DT_END_ELLIPSIS, DT_WORDBREAK, DeleteDC, DeleteObject, DrawTextW,
    EndPaint, FillRect, GetDC, InvalidateRect, PAINTSTRUCT, ReleaseDC, SelectObject, SetBkMode,
    SetTextColor, TRANSPARENT,
};
use windows_sys::Win32::Security::SECURITY_ATTRIBUTES;
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::System::Threading::CreateMutexW;
use windows_sys::Win32::UI::Controls::{BST_CHECKED, BST_UNCHECKED};
use windows_sys::Win32::UI::Input::KeyboardAndMouse::{EnableWindow, GetKeyState, VK_LBUTTON};
use windows_sys::Win32::UI::Shell::{
    NIF_ICON, NIF_MESSAGE, NIF_TIP, NIM_ADD, NIM_DELETE, NIM_MODIFY, NOTIFYICONDATAW,
    Shell_NotifyIconW,
};
use windows_sys::Win32::UI::WindowsAndMessaging::*;
use windows_sys::core::BOOL;

const HOST_CLASS: &str = "DiskLight.Host";
const OVERLAY_CLASS: &str = "DiskLight.Overlay";
const TIP_CLASS: &str = "DiskLight.Tooltip";
const SETTINGS_CLASS: &str = "DiskLight.Settings";
const WM_TRAY: u32 = WM_APP + 2;
const TIMER_ANIMATION: usize = 1;
const TRAY_ID: u32 = 1;
const CMD_SETTINGS: u32 = 100;
const CMD_PREVIEW: u32 = 101;
const CMD_EXIT: u32 = 102;
const ID_MONITOR_ALL: i32 = 200;
const ID_DISK_LIST: i32 = 201;
const ID_TRIGGER: i32 = 202;
const ID_CLEAR: i32 = 203;
const ID_TRIGGER_SECONDS: i32 = 204;
const ID_CLEAR_SECONDS: i32 = 205;
const ID_POSITION: i32 = 206;
const ID_MARGIN: i32 = 207;
const ID_STARTUP: i32 = 208;
const ID_SAVE: i32 = 209;
const ID_CANCEL: i32 = 210;
const OVERLAY_SIZE: i32 = 84;
const TIP_WIDTH: i32 = 350;
const TIP_HEIGHT: i32 = 62;
const RED_THRESHOLD_PERCENT: f64 = 95.0;

pub fn run() -> Result<(), String> {
    unsafe {
        let mutex_name = wide("Local\\DiskLight.SingleInstance");
        let mutex = CreateMutexW(
            null::<SECURITY_ATTRIBUTES>() as *mut _,
            0,
            mutex_name.as_ptr(),
        );
        if mutex.is_null() {
            return Err(format!(
                "Could not create the single-instance guard: {}",
                std::io::Error::last_os_error()
            ));
        }
        if GetLastError() == ERROR_ALREADY_EXISTS {
            CloseHandle(mutex);
            return Ok(());
        }

        let instance = GetModuleHandleW(null());
        if instance.is_null() {
            CloseHandle(mutex);
            return Err("Could not locate the application module.".to_owned());
        }
        register_class(instance, HOST_CLASS, Some(host_proc), 0)?;
        register_class(instance, OVERLAY_CLASS, Some(overlay_proc), 0)?;
        register_class(instance, TIP_CLASS, Some(tip_proc), 0)?;
        register_class(
            instance,
            SETTINGS_CLASS,
            Some(settings_proc),
            COLOR_WINDOW as isize + 1,
        )?;

        let (mut settings, load_warning) = match Settings::load() {
            Ok(settings) => (settings, None),
            Err(error) => (Settings::default(), Some(error)),
        };
        settings.sanitize();
        let mut app = Box::new(AppState::new(instance, mutex, settings, load_warning));
        app.taskbar_created = RegisterWindowMessageW(wide("TaskbarCreated").as_ptr());
        let app_pointer = (&mut *app) as *mut AppState;
        let class = wide(HOST_CLASS);
        let hwnd = CreateWindowExW(
            0,
            class.as_ptr(),
            wide("Disk Light").as_ptr(),
            WS_POPUP,
            0,
            0,
            0,
            0,
            null_mut(),
            null_mut(),
            instance,
            app_pointer.cast(),
        );
        if hwnd.is_null() {
            CloseHandle(mutex);
            return Err(format!(
                "Could not create the notification host window: {}",
                std::io::Error::last_os_error()
            ));
        }
        app.hwnd = hwnd;
        app.overlay = CreateWindowExW(
            WS_EX_LAYERED | WS_EX_TRANSPARENT | WS_EX_NOACTIVATE | WS_EX_TOOLWINDOW,
            wide(OVERLAY_CLASS).as_ptr(),
            wide("Disk Light activity indicator").as_ptr(),
            WS_POPUP,
            0,
            0,
            OVERLAY_SIZE,
            OVERLAY_SIZE,
            hwnd,
            null_mut(),
            instance,
            app_pointer.cast(),
        );
        app.tip = CreateWindowExW(
            WS_EX_TOPMOST | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE | WS_EX_TRANSPARENT,
            wide(TIP_CLASS).as_ptr(),
            wide("").as_ptr(),
            WS_POPUP,
            0,
            0,
            TIP_WIDTH,
            TIP_HEIGHT,
            hwnd,
            null_mut(),
            instance,
            app_pointer.cast(),
        );
        if app.overlay.is_null() || app.tip.is_null() {
            DestroyWindow(hwnd);
            return Err("Could not create the activity overlay.".to_owned());
        }

        app.tray_icon = make_tray_icon();
        if app.tray_icon.is_null() {
            DestroyWindow(hwnd);
            return Err("Could not create the notification-area icon.".to_owned());
        }
        app.add_tray_icon();
        if !app.tray_added {
            DestroyWindow(hwnd);
            DestroyIcon(app.tray_icon);
            CloseHandle(app.mutex);
            return Err("Could not add Disk Light to the notification area.".to_owned());
        }
        let (monitor, receiver) = match MonitorService::start(hwnd, app.settings.clone()) {
            Ok(monitor) => monitor,
            Err(error) => {
                let mut data = app.notify_data();
                Shell_NotifyIconW(NIM_DELETE, &mut data);
                DestroyWindow(hwnd);
                DestroyIcon(app.tray_icon);
                CloseHandle(app.mutex);
                return Err(error);
            }
        };
        app.monitor = Some(monitor);
        app.updates = Some(receiver);
        SetTimer(hwnd, TIMER_ANIMATION, 80, None);
        app.refresh_tray();

        let mut message: MSG = zeroed();
        loop {
            let result = GetMessageW(&mut message, null_mut(), 0, 0);
            if result <= 0 {
                break;
            }
            TranslateMessage(&message);
            DispatchMessageW(&message);
        }

        // Explicitly stop the worker before releasing the state passed to Win32.
        app.monitor.take();
        if app.tray_added {
            let mut data = app.notify_data();
            Shell_NotifyIconW(NIM_DELETE, &mut data);
        }
        if !app.tray_icon.is_null() {
            DestroyIcon(app.tray_icon);
        }
        if !app.tip.is_null() {
            DestroyWindow(app.tip);
        }
        if !app.overlay.is_null() {
            DestroyWindow(app.overlay);
        }
        if !app.hwnd.is_null() {
            DestroyWindow(app.hwnd);
        }
        CloseHandle(app.mutex);
    }
    Ok(())
}

struct AppState {
    instance: HINSTANCE,
    mutex: HANDLE,
    hwnd: HWND,
    overlay: HWND,
    tip: HWND,
    tray_icon: HICON,
    tray_added: bool,
    settings: Settings,
    snapshot: MonitorSnapshot,
    monitor: Option<MonitorService>,
    updates: Option<Receiver<MonitorSnapshot>>,
    preview: bool,
    settings_window: HWND,
    settings_data: *mut SettingsDialog,
    notice: Option<String>,
    tooltip_text: String,
    animation_started: std::time::Instant,
    taskbar_created: u32,
}

impl AppState {
    fn new(instance: HINSTANCE, mutex: HANDLE, settings: Settings, notice: Option<String>) -> Self {
        Self {
            instance,
            mutex,
            hwnd: null_mut(),
            overlay: null_mut(),
            tip: null_mut(),
            tray_icon: null_mut(),
            tray_added: false,
            settings,
            snapshot: MonitorSnapshot::unavailable("Looking for physical disk counters…"),
            monitor: None,
            updates: None,
            preview: false,
            settings_window: null_mut(),
            settings_data: null_mut(),
            notice,
            tooltip_text: String::new(),
            animation_started: std::time::Instant::now(),
            taskbar_created: 0,
        }
    }

    fn effective_visible(&self) -> bool {
        self.preview || self.snapshot.light_active
    }

    fn current_disk(&self) -> Option<&DiskReading> {
        self.snapshot.active_disk.as_ref()
    }

    fn add_tray_icon(&mut self) {
        let mut data = self.notify_data();
        self.tray_added = unsafe { Shell_NotifyIconW(NIM_ADD, &mut data) } != 0;
    }

    fn notify_data(&self) -> NOTIFYICONDATAW {
        let mut data: NOTIFYICONDATAW = unsafe { zeroed() };
        data.cbSize = size_of::<NOTIFYICONDATAW>() as u32;
        data.hWnd = self.hwnd;
        data.uID = TRAY_ID;
        data.uFlags = NIF_MESSAGE | NIF_ICON | NIF_TIP;
        data.uCallbackMessage = WM_TRAY;
        data.hIcon = self.tray_icon;
        let text = self.tray_tooltip();
        copy_wide(&text, &mut data.szTip);
        data
    }

    fn tray_tooltip(&self) -> String {
        if let Some(warning) = &self.notice {
            return format!("Disk Light: settings warning: {}", truncate(warning, 45));
        }
        if self.preview {
            return "Disk Light: overlay preview".to_owned();
        }
        if self.snapshot.light_active {
            if let Some(disk) = self.current_disk() {
                return format!(
                    "Disk Light: {} {:.0}% busy",
                    truncate(&disk_label(&disk.name), 35),
                    disk.busy_percent
                );
            }
        }
        format!("Disk Light: {}", truncate(&self.snapshot.status, 55))
    }

    fn refresh_tray(&mut self) {
        if self.tray_added {
            let mut data = self.notify_data();
            unsafe { Shell_NotifyIconW(NIM_MODIFY, &mut data) };
        }
    }

    fn refresh_overlay(&mut self) {
        if self.effective_visible() {
            self.position_overlay();
            self.render_overlay();
            unsafe { ShowWindow(self.overlay, SW_SHOWNOACTIVATE) };
        } else {
            unsafe {
                ShowWindow(self.overlay, SW_HIDE);
                ShowWindow(self.tip, SW_HIDE);
            }
        }
        self.refresh_tray();
    }

    fn position_overlay(&self) {
        let mut work: RECT = unsafe { zeroed() };
        let got = unsafe {
            SystemParametersInfoW(SPI_GETWORKAREA, 0, (&mut work as *mut RECT).cast(), 0)
        };
        if got == 0 {
            return;
        }
        let edge = i32::from(self.settings.margin_px);
        let right = matches!(
            self.settings.position,
            OverlayPosition::BottomRight | OverlayPosition::TopRight
        );
        let bottom = matches!(
            self.settings.position,
            OverlayPosition::BottomRight | OverlayPosition::BottomLeft
        );
        let x = if right {
            work.right - OVERLAY_SIZE - edge
        } else {
            work.left + edge
        };
        let y = if bottom {
            work.bottom - OVERLAY_SIZE - edge
        } else {
            work.top + edge
        };
        unsafe {
            SetWindowPos(
                self.overlay,
                HWND_TOPMOST,
                x,
                y,
                OVERLAY_SIZE,
                OVERLAY_SIZE,
                SWP_NOACTIVATE,
            );
        }
    }

    fn render_overlay(&mut self) {
        let phase = self.animation_started.elapsed().as_secs_f64() * std::f64::consts::TAU / 2.4;
        let pulse = 0.82 + 0.18 * ((phase.sin() + 1.0) * 0.5);
        let red = self
            .current_disk()
            .is_some_and(|disk| disk.busy_percent >= RED_THRESHOLD_PERCENT);
        unsafe { draw_layered_glow(self.overlay, pulse, red) };
    }

    fn update_hover_tip(&mut self) {
        if !self.effective_visible() {
            return;
        }
        let mut cursor: POINT = unsafe { zeroed() };
        let mut rect: RECT = unsafe { zeroed() };
        let over = unsafe {
            GetCursorPos(&mut cursor) != 0
                && GetWindowRect(self.overlay, &mut rect) != 0
                && cursor.x >= rect.left
                && cursor.x < rect.right
                && cursor.y >= rect.top
                && cursor.y < rect.bottom
        };
        if !over {
            unsafe { ShowWindow(self.tip, SW_HIDE) };
            self.tooltip_text.clear();
            return;
        }
        let text = if self.preview && !self.snapshot.light_active {
            "Disk Light preview".to_owned()
        } else if let Some(disk) = self.current_disk() {
            let process_line = self
                .snapshot
                .top_process
                .as_ref()
                .map(|process| {
                    format!(
                        "Top process I/O: {} · {}",
                        process_label(&process.name),
                        format_io_rate(process.io_bytes_per_second)
                    )
                })
                .unwrap_or_else(|| "Top process I/O: unavailable".to_owned());
            format!(
                "{}  ·  {:.0}% busy\n{}",
                disk_label(&disk.name),
                disk.busy_percent,
                process_line,
            )
        } else {
            return;
        };
        if text != self.tooltip_text {
            self.tooltip_text = text.clone();
            let wide_text = wide(&text);
            unsafe {
                SetWindowTextW(self.tip, wide_text.as_ptr());
                InvalidateRect(self.tip, null(), 1);
            }
        }
        let width = TIP_WIDTH;
        let right_side = matches!(
            self.settings.position,
            OverlayPosition::BottomRight | OverlayPosition::TopRight
        );
        let x = if right_side {
            rect.left - width - 8
        } else {
            rect.right + 8
        };
        let y = rect.top + OVERLAY_SIZE / 2 - TIP_HEIGHT / 2;
        unsafe {
            SetWindowPos(
                self.tip,
                HWND_TOPMOST,
                x,
                y,
                width,
                TIP_HEIGHT,
                SWP_NOACTIVATE | SWP_SHOWWINDOW,
            );
        }
    }

    fn apply_settings(&mut self, settings: Settings) {
        self.settings = settings;
        self.notice = None;
        if let Some(monitor) = &self.monitor {
            monitor.reconfigure(self.settings.clone());
        }
        self.refresh_overlay();
    }

    fn open_settings(&mut self) {
        if !self.settings_window.is_null() {
            unsafe {
                ShowWindow(self.settings_window, SW_SHOWNORMAL);
                SetForegroundWindow(self.settings_window);
            }
            return;
        }
        unsafe { create_settings_window(self) };
    }

    fn show_menu(&mut self) {
        unsafe {
            let menu = CreatePopupMenu();
            if menu.is_null() {
                return;
            }
            if let Some(notice) = &self.notice {
                let warning = wide(&format!("Settings warning: {}", truncate(notice, 78)));
                AppendMenuW(menu, MF_STRING | MF_GRAYED, 0, warning.as_ptr());
            }
            let status = if self.snapshot.status.starts_with("Monitoring ")
                || self.snapshot.status.starts_with("No ")
            {
                self.snapshot.status.clone()
            } else {
                format!(
                    "Monitoring unavailable: {}",
                    truncate(&self.snapshot.status, 78)
                )
            };
            let status = wide(&status);
            AppendMenuW(menu, MF_STRING | MF_GRAYED, 0, status.as_ptr());
            AppendMenuW(menu, MF_SEPARATOR, 0, null());
            AppendMenuW(
                menu,
                MF_STRING,
                CMD_SETTINGS as usize,
                wide("Open settings…").as_ptr(),
            );
            let preview_label = if self.preview {
                "Hide overlay preview"
            } else {
                "Show overlay preview"
            };
            AppendMenuW(
                menu,
                MF_STRING,
                CMD_PREVIEW as usize,
                wide(preview_label).as_ptr(),
            );
            AppendMenuW(menu, MF_STRING, CMD_EXIT as usize, wide("Exit").as_ptr());
            let mut cursor: POINT = zeroed();
            GetCursorPos(&mut cursor);
            SetForegroundWindow(self.hwnd);
            let selected = TrackPopupMenu(
                menu,
                TPM_RETURNCMD | TPM_RIGHTBUTTON,
                cursor.x,
                cursor.y,
                0,
                self.hwnd,
                null(),
            );
            PostMessageW(self.hwnd, WM_NULL, 0, 0);
            DestroyMenu(menu);
            match selected as u32 {
                CMD_SETTINGS => self.open_settings(),
                CMD_PREVIEW => {
                    self.preview = !self.preview;
                    self.refresh_overlay();
                }
                CMD_EXIT => {
                    PostMessageW(self.hwnd, WM_CLOSE, 0, 0);
                }
                _ => {}
            }
        }
    }

    fn accept_updates(&mut self) {
        let was_visible = self.effective_visible();
        let previous_status = self.snapshot.status.clone();
        if let Some(receiver) = &self.updates {
            while let Ok(snapshot) = receiver.try_recv() {
                self.snapshot = snapshot;
            }
        }
        if was_visible != self.effective_visible() {
            self.refresh_overlay();
        } else if self.snapshot.light_active && !self.preview {
            // Keep the tray hover text current while the warning is active.
            self.refresh_tray();
        } else if self.snapshot.status != previous_status {
            self.refresh_tray();
        }
    }
}

unsafe extern "system" fn host_proc(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    if message == WM_NCCREATE {
        let create = unsafe { &*(lparam as *const CREATESTRUCTW) };
        unsafe { SetWindowLongPtrW(hwnd, GWLP_USERDATA, create.lpCreateParams as isize) };
    }
    let pointer = unsafe { GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut AppState };
    if pointer.is_null() {
        return unsafe { DefWindowProcW(hwnd, message, wparam, lparam) };
    }
    let app = unsafe { &mut *pointer };
    if app.taskbar_created != 0 && message == app.taskbar_created {
        app.tray_added = false;
        app.add_tray_icon();
        app.refresh_tray();
        return 0;
    }
    match message {
        WM_TRAY => {
            let event = lparam as u32;
            if event == WM_RBUTTONUP || event == WM_CONTEXTMENU {
                app.show_menu();
            } else if event == WM_LBUTTONDBLCLK || event == WM_LBUTTONUP {
                app.open_settings();
            }
            0
        }
        WM_APP_MONITOR_UPDATE => {
            app.accept_updates();
            0
        }
        WM_TIMER if wparam == TIMER_ANIMATION => {
            if app.effective_visible() {
                app.render_overlay();
                app.update_hover_tip();
            }
            0
        }
        WM_SETTINGCHANGE | WM_DISPLAYCHANGE => {
            app.position_overlay();
            0
        }
        WM_CLOSE => {
            unsafe { PostQuitMessage(0) };
            0
        }
        WM_DESTROY => {
            unsafe {
                KillTimer(hwnd, TIMER_ANIMATION);
                PostQuitMessage(0);
            }
            0
        }
        _ => unsafe { DefWindowProcW(hwnd, message, wparam, lparam) },
    }
}

unsafe extern "system" fn overlay_proc(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    if message == WM_MOUSEACTIVATE {
        return MA_NOACTIVATE as LRESULT;
    }
    unsafe { DefWindowProcW(hwnd, message, wparam, lparam) }
}

unsafe extern "system" fn tip_proc(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match message {
        WM_NCHITTEST => HTTRANSPARENT as LRESULT,
        WM_MOUSEACTIVATE => MA_NOACTIVATE as LRESULT,
        WM_ERASEBKGND => 1,
        WM_PAINT => {
            let mut paint: PAINTSTRUCT = unsafe { zeroed() };
            let dc = unsafe { BeginPaint(hwnd, &mut paint) };
            let mut rect: RECT = unsafe { zeroed() };
            unsafe {
                GetClientRect(hwnd, &mut rect);
                let brush = CreateSolidBrush(0x00221E1B);
                FillRect(dc, &rect, brush);
                DeleteObject(brush);
                SetBkMode(dc, TRANSPARENT as i32);
                SetTextColor(dc, 0x00F5F2ED);
                let mut text = [0u16; 256];
                let length = GetWindowTextW(hwnd, text.as_mut_ptr(), text.len() as i32);
                rect.left += 12;
                rect.top += 7;
                rect.right -= 8;
                rect.bottom -= 7;
                DrawTextW(
                    dc,
                    text.as_ptr(),
                    length,
                    &mut rect,
                    DT_WORDBREAK | DT_END_ELLIPSIS,
                );
                EndPaint(hwnd, &paint);
            }
            0
        }
        _ => unsafe { DefWindowProcW(hwnd, message, wparam, lparam) },
    }
}

unsafe fn draw_layered_glow(hwnd: HWND, pulse: f64, red: bool) {
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
    let mut size = SIZE {
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
            hwnd,
            screen,
            &mut position,
            &mut size,
            memory,
            &source,
            0,
            &blend,
            ULW_ALPHA,
        );
        SelectObject(memory, previous);
        DeleteObject(bitmap);
        DeleteDC(memory);
        ReleaseDC(null_mut(), screen);
    }
}

struct SettingsDialog {
    app: *mut AppState,
    window: HWND,
    disk_list: HWND,
    trigger: HWND,
    clear: HWND,
    trigger_seconds: HWND,
    clear_seconds: HWND,
    position: HWND,
    margin: HWND,
    monitor_all: HWND,
    startup: HWND,
    disk_names: Vec<String>,
    selected: HashSet<String>,
}

unsafe fn create_settings_window(app: &mut AppState) {
    let mut selected: HashSet<String> = app.settings.monitored_disks.iter().cloned().collect();
    if app.settings.monitor_all_disks {
        selected.extend(app.snapshot.disks.iter().map(|disk| disk.name.clone()));
    }
    let mut dialog = Box::new(SettingsDialog {
        app,
        window: null_mut(),
        disk_list: null_mut(),
        trigger: null_mut(),
        clear: null_mut(),
        trigger_seconds: null_mut(),
        clear_seconds: null_mut(),
        position: null_mut(),
        margin: null_mut(),
        monitor_all: null_mut(),
        startup: null_mut(),
        disk_names: Vec::new(),
        selected,
    });
    let app_state = unsafe { &*dialog.app };
    let mut disk_names: Vec<String> = app_state
        .snapshot
        .disks
        .iter()
        .map(|disk| disk.name.clone())
        .collect();
    disk_names.extend(app_state.settings.monitored_disks.iter().cloned());
    disk_names.sort();
    disk_names.dedup();
    dialog.disk_names = disk_names;
    let pointer = Box::into_raw(dialog);
    let app_pointer = unsafe { (*pointer).app };
    unsafe { (*app_pointer).settings_data = pointer };
    let hwnd = unsafe {
        CreateWindowExW(
            WS_EX_APPWINDOW,
            wide(SETTINGS_CLASS).as_ptr(),
            wide("Disk Light settings").as_ptr(),
            WS_OVERLAPPED | WS_CAPTION | WS_SYSMENU | WS_MINIMIZEBOX,
            CW_USEDEFAULT,
            CW_USEDEFAULT,
            540,
            535,
            null_mut(),
            null_mut(),
            (*(*pointer).app).instance,
            pointer.cast(),
        )
    };
    if hwnd.is_null() {
        if unsafe { (*app_pointer).settings_data } == pointer {
            unsafe {
                (*app_pointer).settings_data = null_mut();
                drop(Box::from_raw(pointer));
            }
        }
        return;
    }
    unsafe {
        (*pointer).window = hwnd;
        (*(*pointer).app).settings_window = hwnd;
        ShowWindow(hwnd, SW_SHOWNORMAL);
        SetForegroundWindow(hwnd);
    }
}

unsafe extern "system" fn settings_proc(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    if message == WM_NCCREATE {
        let create = unsafe { &*(lparam as *const CREATESTRUCTW) };
        unsafe { SetWindowLongPtrW(hwnd, GWLP_USERDATA, create.lpCreateParams as isize) };
    }
    let pointer = unsafe { GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut SettingsDialog };
    if pointer.is_null() {
        return unsafe { DefWindowProcW(hwnd, message, wparam, lparam) };
    }
    let dialog = unsafe { &mut *pointer };
    match message {
        WM_CREATE => {
            unsafe { build_settings_controls(dialog) };
            0
        }
        WM_COMMAND => {
            let id = (wparam & 0xffff) as i32;
            let notification = ((wparam >> 16) & 0xffff) as u16;
            match id {
                ID_MONITOR_ALL => unsafe {
                    let all = is_checked(dialog.monitor_all);
                    if !all {
                        dialog.selected.extend(dialog.disk_names.iter().cloned());
                    }
                    refresh_disk_list(dialog);
                    EnableWindow(dialog.disk_list, (!all) as BOOL);
                },
                ID_DISK_LIST
                    if notification == LBN_SELCHANGE as u16
                        && unsafe { GetKeyState(VK_LBUTTON as i32) < 0 } =>
                {
                    unsafe { toggle_disk_selection(dialog) };
                }
                ID_SAVE => unsafe { save_settings_dialog(dialog) },
                ID_CANCEL => unsafe {
                    DestroyWindow(hwnd);
                },
                _ => {}
            }
            0
        }
        WM_CLOSE => {
            unsafe { DestroyWindow(hwnd) };
            0
        }
        WM_NCDESTROY => {
            unsafe {
                if let Some(app) = dialog.app.as_mut() {
                    app.settings_window = null_mut();
                    app.settings_data = null_mut();
                }
                SetWindowLongPtrW(hwnd, GWLP_USERDATA, 0);
                drop(Box::from_raw(pointer));
            }
            0
        }
        _ => unsafe { DefWindowProcW(hwnd, message, wparam, lparam) },
    }
}

unsafe fn build_settings_controls(dialog: &mut SettingsDialog) {
    let app = unsafe { &mut *dialog.app };
    let status = if let Some(warning) = &app.notice {
        format!("Settings warning: {}", truncate(warning, 65))
    } else {
        format!("Status: {}", app.snapshot.status)
    };
    unsafe {
        create_control(
            dialog.window,
            "STATIC",
            "Disk Light watches Windows physical disk activity.",
            WS_CHILD | WS_VISIBLE,
            18,
            14,
            490,
            22,
            0,
        );
        create_control(
            dialog.window,
            "STATIC",
            &status,
            WS_CHILD | WS_VISIBLE,
            18,
            38,
            490,
            22,
            0,
        );
        dialog.monitor_all = create_control(
            dialog.window,
            "BUTTON",
            "Monitor all physical disks",
            WS_CHILD | WS_VISIBLE | BS_AUTOCHECKBOX as u32 | WS_TABSTOP,
            18,
            68,
            240,
            24,
            ID_MONITOR_ALL,
        );
        SendMessageW(
            dialog.monitor_all,
            BM_SETCHECK,
            if app.settings.monitor_all_disks {
                BST_CHECKED as usize
            } else {
                BST_UNCHECKED as usize
            },
            0,
        );
        create_control(
            dialog.window,
            "STATIC",
            "Selected disks (click an entry to toggle):",
            WS_CHILD | WS_VISIBLE,
            18,
            96,
            350,
            20,
            0,
        );
        dialog.disk_list = create_control(
            dialog.window,
            "LISTBOX",
            "",
            WS_CHILD
                | WS_VISIBLE
                | WS_BORDER
                | WS_VSCROLL
                | LBS_NOTIFY as u32
                | LBS_NOINTEGRALHEIGHT as u32
                | WS_TABSTOP,
            18,
            118,
            490,
            82,
            ID_DISK_LIST,
        );
        refresh_disk_list(dialog);
        if dialog.disk_names.is_empty() || app.settings.monitor_all_disks {
            EnableWindow(dialog.disk_list, 0);
        }

        create_control(
            dialog.window,
            "STATIC",
            "Show light at",
            WS_CHILD | WS_VISIBLE,
            18,
            216,
            100,
            22,
            0,
        );
        dialog.trigger = create_edit(
            dialog.window,
            app.settings.trigger_percent,
            118,
            212,
            ID_TRIGGER,
        );
        create_control(
            dialog.window,
            "STATIC",
            "% disk busy for",
            WS_CHILD | WS_VISIBLE,
            180,
            216,
            115,
            22,
            0,
        );
        dialog.trigger_seconds = create_edit(
            dialog.window,
            app.settings.trigger_seconds,
            298,
            212,
            ID_TRIGGER_SECONDS,
        );
        create_control(
            dialog.window,
            "STATIC",
            "seconds",
            WS_CHILD | WS_VISIBLE,
            358,
            216,
            70,
            22,
            0,
        );

        create_control(
            dialog.window,
            "STATIC",
            "Clear below",
            WS_CHILD | WS_VISIBLE,
            18,
            252,
            100,
            22,
            0,
        );
        dialog.clear = create_edit(
            dialog.window,
            app.settings.clear_percent,
            118,
            248,
            ID_CLEAR,
        );
        create_control(
            dialog.window,
            "STATIC",
            "% for",
            WS_CHILD | WS_VISIBLE,
            180,
            252,
            42,
            22,
            0,
        );
        dialog.clear_seconds = create_edit(
            dialog.window,
            app.settings.clear_seconds,
            225,
            248,
            ID_CLEAR_SECONDS,
        );
        create_control(
            dialog.window,
            "STATIC",
            "seconds",
            WS_CHILD | WS_VISIBLE,
            285,
            252,
            70,
            22,
            0,
        );
        create_control(
            dialog.window,
            "STATIC",
            "Keep clear below the show threshold to reduce flicker.",
            WS_CHILD | WS_VISIBLE,
            18,
            278,
            450,
            20,
            0,
        );

        create_control(
            dialog.window,
            "STATIC",
            "Overlay position",
            WS_CHILD | WS_VISIBLE,
            18,
            316,
            120,
            22,
            0,
        );
        dialog.position = create_control(
            dialog.window,
            "COMBOBOX",
            "",
            WS_CHILD | WS_VISIBLE | WS_TABSTOP | CBS_DROPDOWNLIST as u32 | WS_VSCROLL,
            145,
            312,
            210,
            120,
            ID_POSITION,
        );
        let positions = ["Bottom right", "Bottom left", "Top right", "Top left"];
        for (index, label) in positions.iter().enumerate() {
            SendMessageW(
                dialog.position,
                CB_ADDSTRING,
                0,
                wide(label).as_ptr() as LPARAM,
            );
            if position_index(app.settings.position) == index {
                SendMessageW(dialog.position, CB_SETCURSEL, index, 0);
            }
        }
        create_control(
            dialog.window,
            "STATIC",
            "Margin",
            WS_CHILD | WS_VISIBLE,
            370,
            316,
            55,
            22,
            0,
        );
        dialog.margin = create_edit(dialog.window, app.settings.margin_px, 425, 312, ID_MARGIN);
        create_control(
            dialog.window,
            "STATIC",
            "pixels",
            WS_CHILD | WS_VISIBLE,
            477,
            316,
            45,
            22,
            0,
        );

        dialog.startup = create_control(
            dialog.window,
            "BUTTON",
            "Start Disk Light when I sign in",
            WS_CHILD | WS_VISIBLE | BS_AUTOCHECKBOX as u32 | WS_TABSTOP,
            18,
            360,
            310,
            24,
            ID_STARTUP,
        );
        SendMessageW(
            dialog.startup,
            BM_SETCHECK,
            if app.settings.start_on_login {
                BST_CHECKED as usize
            } else {
                BST_UNCHECKED as usize
            },
            0,
        );
        create_control(
            dialog.window,
            "STATIC",
            "The overlay has no toast or sound and passes mouse clicks through.",
            WS_CHILD | WS_VISIBLE,
            18,
            399,
            490,
            22,
            0,
        );
        create_control(
            dialog.window,
            "BUTTON",
            "Save",
            WS_CHILD | WS_VISIBLE | BS_DEFPUSHBUTTON as u32 | WS_TABSTOP,
            336,
            448,
            80,
            30,
            ID_SAVE,
        );
        create_control(
            dialog.window,
            "BUTTON",
            "Cancel",
            WS_CHILD | WS_VISIBLE | WS_TABSTOP,
            426,
            448,
            82,
            30,
            ID_CANCEL,
        );
    }
}

unsafe fn save_settings_dialog(dialog: &mut SettingsDialog) {
    let current = unsafe { &(*dialog.app).settings };
    let mut next = current.clone();
    let all = unsafe { is_checked(dialog.monitor_all) };
    let trigger = unsafe { read_number(dialog.trigger, "Show threshold", 2, 100) };
    let clear = unsafe { read_number(dialog.clear, "Clear threshold", 1, 99) };
    let trigger_seconds = unsafe { read_number(dialog.trigger_seconds, "Show delay", 1, 30) };
    let clear_seconds = unsafe { read_number(dialog.clear_seconds, "Clear delay", 1, 60) };
    let margin = unsafe { read_number(dialog.margin, "Overlay margin", 0, 300) };
    let (Ok(trigger), Ok(clear), Ok(trigger_seconds), Ok(clear_seconds), Ok(margin)) =
        (trigger, clear, trigger_seconds, clear_seconds, margin)
    else {
        return;
    };
    if clear >= trigger {
        unsafe {
            show_message(
                dialog.window,
                "The clear threshold must be lower than the show threshold.",
                MB_ICONWARNING,
            )
        };
        return;
    }
    next.monitor_all_disks = all;
    next.monitored_disks = if all {
        Vec::new()
    } else {
        dialog.selected.iter().cloned().collect()
    };
    next.trigger_percent = trigger as u8;
    next.clear_percent = clear as u8;
    next.trigger_seconds = trigger_seconds as u8;
    next.clear_seconds = clear_seconds as u8;
    next.margin_px = margin as u16;
    next.position = match unsafe { SendMessageW(dialog.position, CB_GETCURSEL, 0, 0) } {
        1 => OverlayPosition::BottomLeft,
        2 => OverlayPosition::TopRight,
        3 => OverlayPosition::TopLeft,
        _ => OverlayPosition::BottomRight,
    };
    next.start_on_login = unsafe { is_checked(dialog.startup) };

    if next.start_on_login != current.start_on_login {
        if let Err(error) = set_start_on_login(next.start_on_login) {
            unsafe { show_message(dialog.window, &error, MB_ICONERROR) };
            return;
        }
    }
    if let Err(error) = next.save() {
        if next.start_on_login != current.start_on_login {
            let _ = set_start_on_login(current.start_on_login);
        }
        unsafe { show_message(dialog.window, &error, MB_ICONERROR) };
        return;
    }
    let app = unsafe { &mut *dialog.app };
    app.apply_settings(next);
    unsafe { DestroyWindow(dialog.window) };
}

unsafe fn toggle_disk_selection(dialog: &mut SettingsDialog) {
    let row = unsafe { SendMessageW(dialog.disk_list, LB_GETCURSEL, 0, 0) };
    if row < 0 || row as usize >= dialog.disk_names.len() {
        return;
    }
    let name = dialog.disk_names[row as usize].clone();
    if !dialog.selected.remove(&name) {
        dialog.selected.insert(name.clone());
    }
    let selected = dialog.selected.contains(&name);
    let display = format!(
        "{}  {}",
        if selected { "✓" } else { "○" },
        disk_label(&name)
    );
    unsafe {
        SendMessageW(dialog.disk_list, LB_DELETESTRING, row as usize, 0);
        SendMessageW(
            dialog.disk_list,
            LB_INSERTSTRING,
            row as usize,
            wide(&display).as_ptr() as LPARAM,
        );
        SendMessageW(dialog.disk_list, LB_SETCURSEL, row as usize, 0);
    }
}

unsafe fn refresh_disk_list(dialog: &mut SettingsDialog) {
    unsafe { SendMessageW(dialog.disk_list, LB_RESETCONTENT, 0, 0) };
    if dialog.disk_names.is_empty() {
        unsafe {
            SendMessageW(
                dialog.disk_list,
                LB_ADDSTRING,
                0,
                wide("Disk list appears when counters are available").as_ptr() as LPARAM,
            );
            EnableWindow(dialog.disk_list, 0);
        }
        return;
    }
    for name in &dialog.disk_names {
        let selected = dialog.selected.contains(name);
        let row = format!("{}  {}", if selected { "✓" } else { "○" }, disk_label(name));
        unsafe {
            SendMessageW(
                dialog.disk_list,
                LB_ADDSTRING,
                0,
                wide(&row).as_ptr() as LPARAM,
            )
        };
    }
}

unsafe fn create_control(
    parent: HWND,
    class: &str,
    text: &str,
    style: u32,
    x: i32,
    y: i32,
    width: i32,
    height: i32,
    id: i32,
) -> HWND {
    unsafe {
        CreateWindowExW(
            0,
            wide(class).as_ptr(),
            wide(text).as_ptr(),
            style,
            x,
            y,
            width,
            height,
            parent,
            id as usize as *mut _,
            GetModuleHandleW(null()),
            null_mut(),
        )
    }
}

unsafe fn create_edit<T: std::fmt::Display>(
    parent: HWND,
    value: T,
    x: i32,
    y: i32,
    id: i32,
) -> HWND {
    unsafe {
        create_control(
            parent,
            "EDIT",
            &value.to_string(),
            WS_CHILD | WS_VISIBLE | WS_BORDER | ES_NUMBER as u32 | ES_CENTER as u32 | WS_TABSTOP,
            x,
            y,
            55,
            25,
            id,
        )
    }
}

unsafe fn read_number(hwnd: HWND, label: &str, minimum: u32, maximum: u32) -> Result<u32, ()> {
    let mut text = [0u16; 64];
    let length = unsafe { GetWindowTextW(hwnd, text.as_mut_ptr(), text.len() as i32) };
    let parsed = String::from_utf16_lossy(&text[..length.max(0) as usize]).parse::<u32>();
    match parsed {
        Ok(value) if (minimum..=maximum).contains(&value) => Ok(value),
        _ => {
            unsafe {
                show_message(
                    hwnd,
                    &format!("Enter a {label} from {minimum} to {maximum}.",),
                    MB_ICONWARNING,
                )
            };
            Err(())
        }
    }
}

unsafe fn is_checked(hwnd: HWND) -> bool {
    unsafe { SendMessageW(hwnd, BM_GETCHECK, 0, 0) == BST_CHECKED as LRESULT }
}

unsafe fn show_message(parent: HWND, text: &str, icon: u32) {
    unsafe {
        MessageBoxW(
            parent,
            wide(text).as_ptr(),
            wide("Disk Light").as_ptr(),
            MB_OK | icon,
        )
    };
}

fn position_index(position: OverlayPosition) -> usize {
    match position {
        OverlayPosition::BottomRight => 0,
        OverlayPosition::BottomLeft => 1,
        OverlayPosition::TopRight => 2,
        OverlayPosition::TopLeft => 3,
    }
}

fn set_start_on_login(enabled: bool) -> Result<(), String> {
    use windows_sys::Win32::System::Registry::{
        HKEY_CURRENT_USER, KEY_SET_VALUE, REG_OPTION_NON_VOLATILE, REG_SZ, RegCloseKey,
        RegCreateKeyExW, RegDeleteValueW, RegSetValueExW,
    };
    let key_path = wide("Software\\Microsoft\\Windows\\CurrentVersion\\Run");
    let value_name = wide("DiskLight");
    let mut key = null_mut();
    let status = unsafe {
        RegCreateKeyExW(
            HKEY_CURRENT_USER,
            key_path.as_ptr(),
            0,
            null(),
            REG_OPTION_NON_VOLATILE,
            KEY_SET_VALUE,
            null(),
            &mut key,
            null_mut(),
        )
    };
    if status != 0 {
        return Err(format!(
            "Could not update the Windows sign-in setting (registry error {status})."
        ));
    }
    let result = if enabled {
        match std::env::current_exe() {
            Ok(executable) => {
                let command = format!("\"{}\"", executable.to_string_lossy());
                let data: Vec<u16> = command.encode_utf16().chain(Some(0)).collect();
                unsafe {
                    RegSetValueExW(
                        key,
                        value_name.as_ptr(),
                        0,
                        REG_SZ,
                        data.as_ptr().cast(),
                        (data.len() * size_of::<u16>()) as u32,
                    )
                }
            }
            Err(error) => {
                unsafe { RegCloseKey(key) };
                return Err(format!("Could not locate Disk Light.exe: {error}"));
            }
        }
    } else {
        let status = unsafe { RegDeleteValueW(key, value_name.as_ptr()) };
        if status == 2 { 0 } else { status }
    };
    unsafe { RegCloseKey(key) };
    if result == 0 {
        Ok(())
    } else {
        Err(format!(
            "Could not update the Windows sign-in setting (registry error {result})."
        ))
    }
}

unsafe fn register_class(
    instance: HINSTANCE,
    name: &str,
    proc: WNDPROC,
    background: isize,
) -> Result<(), String> {
    let class_name = wide(name);
    let class = WNDCLASSW {
        style: CS_HREDRAW | CS_VREDRAW,
        lpfnWndProc: proc,
        hInstance: instance,
        hIcon: null_mut(),
        hCursor: unsafe { LoadCursorW(null_mut(), IDC_ARROW) },
        hbrBackground: background as _,
        lpszMenuName: null(),
        lpszClassName: class_name.as_ptr(),
        cbClsExtra: 0,
        cbWndExtra: 0,
    };
    let registered = unsafe { RegisterClassW(&class) };
    if registered == 0 {
        let error = unsafe { GetLastError() };
        if error != 1410 {
            return Err(format!(
                "Could not register window class {name} (Windows error {error})."
            ));
        }
    }
    Ok(())
}

fn make_tray_icon() -> HICON {
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
                    (a << 24) | (r * a / 255 << 16) | (g * a / 255 << 8) | (b * a / 255);
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

fn wide(value: &str) -> Vec<u16> {
    value.encode_utf16().chain(Some(0)).collect()
}

fn copy_wide<const N: usize>(value: &str, target: &mut [u16; N]) {
    target.fill(0);
    let encoded: Vec<u16> = value.encode_utf16().collect();
    let count = encoded.len().min(N.saturating_sub(1));
    target[..count].copy_from_slice(&encoded[..count]);
}

fn truncate(value: &str, max_chars: usize) -> String {
    let mut chars = value.chars();
    let result: String = chars.by_ref().take(max_chars).collect();
    if chars.next().is_some() {
        format!("{result}…")
    } else {
        result
    }
}

fn disk_label(instance_name: &str) -> String {
    instance_name
        .split_whitespace()
        .next()
        .and_then(|index| index.parse::<u32>().ok())
        .map(|index| format!("Physical disk {index}"))
        .unwrap_or_else(|| instance_name.to_owned())
}

fn process_label(instance_name: &str) -> String {
    if instance_name.eq_ignore_ascii_case("System") {
        return "System".to_owned();
    }
    if let Some((name, instance)) = instance_name.rsplit_once('#') {
        if instance.parse::<u32>().is_ok() {
            return format!("{name}.exe (instance {instance})");
        }
    }
    format!("{instance_name}.exe")
}

fn format_io_rate(bytes_per_second: f64) -> String {
    const KIB: f64 = 1024.0;
    const MIB: f64 = KIB * 1024.0;
    if bytes_per_second >= MIB {
        format!("{:.1} MiB/s", bytes_per_second / MIB)
    } else if bytes_per_second >= KIB {
        format!("{:.0} KiB/s", bytes_per_second / KIB)
    } else {
        format!("{:.0} B/s", bytes_per_second)
    }
}
