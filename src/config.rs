use serde::{Deserialize, Serialize};
use std::fs;
use std::path::PathBuf;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum OverlayPosition {
    BottomRight,
    BottomLeft,
    TopRight,
    TopLeft,
}

impl Default for OverlayPosition {
    fn default() -> Self {
        Self::BottomRight
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    /// When true, newly discovered physical disks are monitored automatically.
    pub monitor_all_disks: bool,
    /// PDH instance names selected when `monitor_all_disks` is false.
    pub monitored_disks: Vec<String>,
    pub trigger_percent: u8,
    pub clear_percent: u8,
    pub trigger_seconds: u8,
    pub clear_seconds: u8,
    pub position: OverlayPosition,
    pub margin_px: u16,
    pub start_on_login: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            monitor_all_disks: true,
            monitored_disks: Vec::new(),
            trigger_percent: 70,
            clear_percent: 60,
            trigger_seconds: 2,
            clear_seconds: 5,
            position: OverlayPosition::BottomRight,
            margin_px: 24,
            start_on_login: false,
        }
    }
}

impl Settings {
    pub fn load() -> Result<Self, String> {
        let path = settings_path()?;
        let mut settings = match fs::read(&path) {
            Ok(bytes) => serde_json::from_slice::<Self>(&bytes)
                .map_err(|error| format!("Settings file is invalid: {error}"))?,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Self::default(),
            Err(error) => return Err(format!("Could not read settings: {error}")),
        };
        settings.sanitize();
        Ok(settings)
    }

    pub fn save(&self) -> Result<(), String> {
        let path = settings_path()?;
        let parent = path
            .parent()
            .ok_or_else(|| "The settings folder path is invalid.".to_owned())?;
        fs::create_dir_all(parent)
            .map_err(|error| format!("Could not create the settings folder: {error}"))?;
        let bytes = serde_json::to_vec_pretty(self)
            .map_err(|error| format!("Could not encode settings: {error}"))?;
        let temporary = path.with_extension("json.tmp");
        fs::write(&temporary, bytes)
            .map_err(|error| format!("Could not write settings: {error}"))?;

        // MoveFileExW replaces an existing settings file atomically on Windows.
        use std::os::windows::ffi::OsStrExt;
        use windows_sys::Win32::Storage::FileSystem::{
            MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH, MoveFileExW,
        };
        let from: Vec<u16> = temporary.as_os_str().encode_wide().chain(Some(0)).collect();
        let to: Vec<u16> = path.as_os_str().encode_wide().chain(Some(0)).collect();
        let moved = unsafe {
            MoveFileExW(
                from.as_ptr(),
                to.as_ptr(),
                MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
            )
        };
        if moved == 0 {
            let _ = fs::remove_file(&temporary);
            return Err(format!(
                "Could not finish saving settings (Windows error {}).",
                std::io::Error::last_os_error()
            ));
        }
        Ok(())
    }

    pub fn sanitize(&mut self) {
        self.trigger_percent = self.trigger_percent.clamp(2, 100);
        self.clear_percent = self.clear_percent.clamp(1, self.trigger_percent - 1);
        self.trigger_seconds = self.trigger_seconds.clamp(1, 30);
        self.clear_seconds = self.clear_seconds.clamp(1, 60);
        self.margin_px = self.margin_px.clamp(0, 300);
        self.monitored_disks.sort();
        self.monitored_disks.dedup();
    }

    pub fn monitors(&self, name: &str) -> bool {
        self.monitor_all_disks || self.monitored_disks.iter().any(|disk| disk == name)
    }
}

fn settings_path() -> Result<PathBuf, String> {
    let local_app_data = std::env::var_os("LOCALAPPDATA")
        .ok_or_else(|| "Windows did not provide the LOCALAPPDATA folder.".to_owned())?;
    Ok(PathBuf::from(local_app_data)
        .join("DiskLight")
        .join("settings.json"))
}
