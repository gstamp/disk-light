use super::{AppState, OverlayPosition, disk_label, truncate};
use crate::win32::wide;
use std::collections::HashSet;
use std::mem::size_of;
use std::ptr::{null, null_mut};
use windows_sys::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::UI::Controls::{BST_CHECKED, BST_UNCHECKED};
use windows_sys::Win32::UI::Input::KeyboardAndMouse::{EnableWindow, GetKeyState, VK_LBUTTON};
use windows_sys::Win32::UI::WindowsAndMessaging::*;
use windows_sys::core::BOOL;

pub(super) const CLASS_NAME: &str = "DiskLight.Settings";
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
const OVERLAY_POSITIONS: [(OverlayPosition, &str); 4] = [
    (OverlayPosition::BottomRight, "Bottom right"),
    (OverlayPosition::BottomLeft, "Bottom left"),
    (OverlayPosition::TopRight, "Top right"),
    (OverlayPosition::TopLeft, "Top left"),
];

struct ControlBounds {
    x: i32,
    y: i32,
    width: i32,
    height: i32,
}

impl ControlBounds {
    const fn new(x: i32, y: i32, width: i32, height: i32) -> Self {
        Self {
            x,
            y,
            width,
            height,
        }
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

pub(super) unsafe fn create_settings_window(app: &mut AppState) {
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
    unsafe { (*app_pointer).settings_dialog = pointer.cast() };
    let hwnd = unsafe {
        CreateWindowExW(
            WS_EX_APPWINDOW,
            wide(CLASS_NAME).as_ptr(),
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
        if unsafe { (*app_pointer).settings_dialog } == pointer.cast() {
            unsafe {
                (*app_pointer).settings_dialog = null_mut();
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

pub(super) unsafe extern "system" fn settings_proc(
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
                    app.settings_dialog = null_mut();
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
    let app = unsafe { &*dialog.app };
    let status = if let Some(warning) = &app.notice {
        format!("Settings warning: {}", truncate(warning, 65))
    } else {
        format!("Status: {}", app.snapshot.status)
    };
    unsafe { build_heading(dialog, &status) };
    unsafe { build_disk_controls(dialog, app) };
    unsafe { build_threshold_controls(dialog, app) };
    unsafe { build_overlay_controls(dialog, app) };
    unsafe { build_startup_controls(dialog, app) };
}

unsafe fn build_heading(dialog: &SettingsDialog, status: &str) {
    unsafe {
        create_control(
            dialog.window,
            "STATIC",
            "Disk Light watches Windows physical disk activity.",
            WS_CHILD | WS_VISIBLE,
            ControlBounds::new(18, 14, 490, 22),
            0,
        );
        create_control(
            dialog.window,
            "STATIC",
            status,
            WS_CHILD | WS_VISIBLE,
            ControlBounds::new(18, 38, 490, 22),
            0,
        );
    }
}

unsafe fn build_disk_controls(dialog: &mut SettingsDialog, app: &AppState) {
    unsafe {
        dialog.monitor_all = create_control(
            dialog.window,
            "BUTTON",
            "Monitor all physical disks",
            WS_CHILD | WS_VISIBLE | BS_AUTOCHECKBOX as u32 | WS_TABSTOP,
            ControlBounds::new(18, 68, 240, 24),
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
            ControlBounds::new(18, 96, 350, 20),
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
            ControlBounds::new(18, 118, 490, 82),
            ID_DISK_LIST,
        );
        refresh_disk_list(dialog);
        if dialog.disk_names.is_empty() || app.settings.monitor_all_disks {
            EnableWindow(dialog.disk_list, 0);
        }
    }
}

unsafe fn build_threshold_controls(dialog: &mut SettingsDialog, app: &AppState) {
    unsafe {
        create_control(
            dialog.window,
            "STATIC",
            "Show light at",
            WS_CHILD | WS_VISIBLE,
            ControlBounds::new(18, 216, 100, 22),
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
            ControlBounds::new(180, 216, 115, 22),
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
            ControlBounds::new(358, 216, 70, 22),
            0,
        );

        create_control(
            dialog.window,
            "STATIC",
            "Clear below",
            WS_CHILD | WS_VISIBLE,
            ControlBounds::new(18, 252, 100, 22),
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
            ControlBounds::new(180, 252, 42, 22),
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
            ControlBounds::new(285, 252, 70, 22),
            0,
        );
        create_control(
            dialog.window,
            "STATIC",
            "Keep clear below the show threshold to reduce flicker.",
            WS_CHILD | WS_VISIBLE,
            ControlBounds::new(18, 278, 450, 20),
            0,
        );
    }
}

unsafe fn build_overlay_controls(dialog: &mut SettingsDialog, app: &AppState) {
    unsafe {
        create_control(
            dialog.window,
            "STATIC",
            "Overlay position",
            WS_CHILD | WS_VISIBLE,
            ControlBounds::new(18, 316, 120, 22),
            0,
        );
        dialog.position = create_control(
            dialog.window,
            "COMBOBOX",
            "",
            WS_CHILD | WS_VISIBLE | WS_TABSTOP | CBS_DROPDOWNLIST as u32 | WS_VSCROLL,
            ControlBounds::new(145, 312, 210, 120),
            ID_POSITION,
        );
        for (index, (position, label)) in OVERLAY_POSITIONS.iter().enumerate() {
            SendMessageW(
                dialog.position,
                CB_ADDSTRING,
                0,
                wide(label).as_ptr() as LPARAM,
            );
            if app.settings.position == *position {
                SendMessageW(dialog.position, CB_SETCURSEL, index, 0);
            }
        }
        create_control(
            dialog.window,
            "STATIC",
            "Margin",
            WS_CHILD | WS_VISIBLE,
            ControlBounds::new(370, 316, 55, 22),
            0,
        );
        dialog.margin = create_edit(dialog.window, app.settings.margin_px, 425, 312, ID_MARGIN);
        create_control(
            dialog.window,
            "STATIC",
            "pixels",
            WS_CHILD | WS_VISIBLE,
            ControlBounds::new(477, 316, 45, 22),
            0,
        );
    }
}

unsafe fn build_startup_controls(dialog: &mut SettingsDialog, app: &AppState) {
    unsafe {
        dialog.startup = create_control(
            dialog.window,
            "BUTTON",
            "Start Disk Light when I sign in",
            WS_CHILD | WS_VISIBLE | BS_AUTOCHECKBOX as u32 | WS_TABSTOP,
            ControlBounds::new(18, 360, 310, 24),
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
            ControlBounds::new(18, 399, 490, 22),
            0,
        );
        create_control(
            dialog.window,
            "BUTTON",
            "Save",
            WS_CHILD | WS_VISIBLE | BS_DEFPUSHBUTTON as u32 | WS_TABSTOP,
            ControlBounds::new(336, 448, 80, 30),
            ID_SAVE,
        );
        create_control(
            dialog.window,
            "BUTTON",
            "Cancel",
            WS_CHILD | WS_VISIBLE | WS_TABSTOP,
            ControlBounds::new(426, 448, 82, 30),
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
    let position_index = unsafe { SendMessageW(dialog.position, CB_GETCURSEL, 0, 0) };
    next.position = usize::try_from(position_index)
        .ok()
        .and_then(|index| OVERLAY_POSITIONS.get(index))
        .map(|(position, _)| *position)
        .unwrap_or_default();
    next.start_on_login = unsafe { is_checked(dialog.startup) };

    if next.start_on_login != current.start_on_login
        && let Err(error) = set_start_on_login(next.start_on_login)
    {
        unsafe { show_message(dialog.window, &error, MB_ICONERROR) };
        return;
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
    bounds: ControlBounds,
    id: i32,
) -> HWND {
    let ControlBounds {
        x,
        y,
        width,
        height,
    } = bounds;
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
            ControlBounds::new(x, y, 55, 25),
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
