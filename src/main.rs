#![cfg_attr(windows, windows_subsystem = "windows")]

#[cfg(windows)]
mod activity;
#[cfg(windows)]
mod config;
#[cfg(windows)]
mod monitor;
#[cfg(windows)]
mod ui;
#[cfg(windows)]
mod win32;

#[cfg(windows)]
fn main() {
    if let Err(error) = ui::run() {
        let message = format!("Disk Light could not start.\n\n{error}");
        let wide: Vec<u16> = message.encode_utf16().chain(std::iter::once(0)).collect();
        unsafe {
            windows_sys::Win32::UI::WindowsAndMessaging::MessageBoxW(
                std::ptr::null_mut(),
                wide.as_ptr(),
                "Disk Light"
                    .encode_utf16()
                    .chain(std::iter::once(0))
                    .collect::<Vec<_>>()
                    .as_ptr(),
                windows_sys::Win32::UI::WindowsAndMessaging::MB_ICONERROR
                    | windows_sys::Win32::UI::WindowsAndMessaging::MB_OK,
            );
        }
    }
}

#[cfg(not(windows))]
fn main() {
    eprintln!("Disk Light is a native Windows utility and can only run on Windows.");
}
