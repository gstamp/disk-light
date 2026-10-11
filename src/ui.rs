use crate::activity::{DiskReading, MonitorSnapshot};
use crate::config::{OverlayPosition, Settings};
use crate::monitor::{MonitorService, WM_APP_MONITOR_UPDATE};

mod graphics;
mod settings;

use crate::win32::wide;
use std::mem::{size_of, zeroed};
use std::ptr::{null, null_mut};
use std::sync::mpsc::Receiver;
use windows_sys::Win32::Foundation::{
    CloseHandle, ERROR_ALREADY_EXISTS, GetLastError, HANDLE, HINSTANCE, HWND, LPARAM, LRESULT,
    POINT, RECT, WPARAM,
};
use windows_sys::Win32::Graphics::Gdi::{
    BeginPaint, COLOR_WINDOW, CreateSolidBrush, DT_END_ELLIPSIS, DT_WORDBREAK, DeleteObject,
    DrawTextW, EndPaint, FillRect, InvalidateRect, PAINTSTRUCT, SetBkMode, SetTextColor,
    TRANSPARENT,
};
use windows_sys::Win32::Security::SECURITY_ATTRIBUTES;
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::System::Threading::CreateMutexW;
use windows_sys::Win32::UI::Shell::{
    NIF_ICON, NIF_MESSAGE, NIF_TIP, NIM_ADD, NIM_DELETE, NIM_MODIFY, NOTIFYICONDATAW,
    Shell_NotifyIconW,
};
use windows_sys::Win32::UI::WindowsAndMessaging::*;

const HOST_CLASS: &str = "DiskLight.Host";
const OVERLAY_CLASS: &str = "DiskLight.Overlay";
const TIP_CLASS: &str = "DiskLight.Tooltip";
const WM_TRAY: u32 = WM_APP + 2;
const TIMER_ANIMATION: usize = 1;
const TRAY_ID: u32 = 1;
const CMD_SETTINGS: u32 = 100;
const CMD_PREVIEW: u32 = 101;
const CMD_EXIT: u32 = 102;
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
            settings::CLASS_NAME,
            Some(settings::settings_proc),
            COLOR_WINDOW as isize + 1,
        )?;

        let (settings, load_warning) = match Settings::load() {
            Ok(settings) => (settings, None),
            Err(error) => (Settings::default(), Some(error)),
        };
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

        app.tray_icon = graphics::make_tray_icon();
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
                let data = app.notify_data();
                Shell_NotifyIconW(NIM_DELETE, &data);
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
            let data = app.notify_data();
            Shell_NotifyIconW(NIM_DELETE, &data);
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
    settings_dialog: *mut core::ffi::c_void,
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
            settings_dialog: null_mut(),
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
        let data = self.notify_data();
        self.tray_added = unsafe { Shell_NotifyIconW(NIM_ADD, &data) } != 0;
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
        if self.snapshot.light_active
            && let Some(disk) = self.current_disk()
        {
            return format!(
                "Disk Light: {} {:.0}% busy",
                truncate(&disk_label(&disk.name), 35),
                disk.busy_percent
            );
        }
        format!("Disk Light: {}", truncate(&self.snapshot.status, 55))
    }

    fn refresh_tray(&mut self) {
        if self.tray_added {
            let data = self.notify_data();
            unsafe { Shell_NotifyIconW(NIM_MODIFY, &data) };
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
        let x = if self.settings.position.is_right() {
            work.right - OVERLAY_SIZE - edge
        } else {
            work.left + edge
        };
        let y = if self.settings.position.is_bottom() {
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
        unsafe { graphics::draw_layered_glow(self.overlay, pulse, red) };
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
        let x = if self.settings.position.is_right() {
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
        unsafe { settings::create_settings_window(self) };
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

fn copy_wide<const N: usize>(value: &str, target: &mut [u16; N]) {
    target.fill(0);
    let encoded: Vec<u16> = value.encode_utf16().collect();
    let count = encoded.len().min(N.saturating_sub(1));
    target[..count].copy_from_slice(&encoded[..count]);
}

pub(super) fn truncate(value: &str, max_chars: usize) -> String {
    let mut chars = value.chars();
    let result: String = chars.by_ref().take(max_chars).collect();
    if chars.next().is_some() {
        format!("{result}…")
    } else {
        result
    }
}

pub(super) fn disk_label(instance_name: &str) -> String {
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
    if let Some((name, instance)) = instance_name.rsplit_once('#')
        && instance.parse::<u32>().is_ok()
    {
        return format!("{name}.exe (instance {instance})");
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
