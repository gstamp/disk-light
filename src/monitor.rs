use crate::activity::{ActivityState, DiskReading, MonitorSnapshot, ProcessUsage};
use crate::config::Settings;
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};
use windows_sys::Win32::Foundation::HWND;
use windows_sys::Win32::System::Performance::{
    PDH_COUNTER_INFO_W, PDH_FMT_COUNTERVALUE, PDH_FMT_DOUBLE, PDH_HCOUNTER, PDH_HQUERY,
    PDH_MORE_DATA, PdhAddCounterW, PdhAddEnglishCounterW, PdhCloseQuery, PdhCollectQueryData,
    PdhExpandWildCardPathW, PdhGetCounterInfoW, PdhGetFormattedCounterValue, PdhOpenQueryW,
    PdhRemoveCounter,
};
use windows_sys::Win32::UI::WindowsAndMessaging::PostMessageW;

pub const WM_APP_MONITOR_UPDATE: u32 = 0x8001;
const PROCESS_REFRESH_INTERVAL: Duration = Duration::from_secs(5);

pub enum MonitorCommand {
    Reconfigure(Settings),
    Stop,
}

pub struct MonitorService {
    command: Sender<MonitorCommand>,
    thread: Option<JoinHandle<()>>,
}

impl MonitorService {
    pub fn start(
        hwnd: HWND,
        settings: Settings,
    ) -> Result<(Self, Receiver<MonitorSnapshot>), String> {
        let (command, commands) = mpsc::channel();
        let (updates, receiver) = mpsc::channel();
        let window = hwnd as isize;
        let thread = thread::Builder::new()
            .name("disk-light-pdh".to_owned())
            .spawn(move || run_monitor(window, settings, commands, updates))
            .map_err(|error| format!("Could not start the disk monitor thread: {error}"))?;
        Ok((
            Self {
                command,
                thread: Some(thread),
            },
            receiver,
        ))
    }

    pub fn reconfigure(&self, settings: Settings) {
        let _ = self.command.send(MonitorCommand::Reconfigure(settings));
    }
}

impl Drop for MonitorService {
    fn drop(&mut self) {
        let _ = self.command.send(MonitorCommand::Stop);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

fn run_monitor(
    hwnd: isize,
    mut settings: Settings,
    commands: Receiver<MonitorCommand>,
    updates: Sender<MonitorSnapshot>,
) {
    let mut state = ActivityState::default();
    let mut source: Option<CounterSource> = None;
    let mut last_open_attempt = Instant::now() - Duration::from_secs(10);

    loop {
        match commands.recv_timeout(Duration::from_millis(500)) {
            Ok(MonitorCommand::Stop) => break,
            Ok(MonitorCommand::Reconfigure(next)) => {
                settings = next;
                state.reset();
                source = None;
                last_open_attempt = Instant::now() - Duration::from_secs(10);
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
            Err(mpsc::RecvTimeoutError::Timeout) => {}
        }

        // Physical disks can be attached or removed while the tray process is running.
        // Refresh the PDH instance list periodically without resetting the hysteresis state.
        if source
            .as_ref()
            .is_some_and(|counter_source| counter_source.age() >= Duration::from_secs(30))
        {
            source = None;
        }

        if source.is_none() && last_open_attempt.elapsed() >= Duration::from_secs(5) {
            last_open_attempt = Instant::now();
            source = match CounterSource::open() {
                Ok(source) => Some(source),
                Err(error) => {
                    state.reset();
                    publish(&updates, hwnd, MonitorSnapshot::unavailable(error));
                    None
                }
            };
        }

        let Some(counter_source) = source.as_mut() else {
            continue;
        };
        match counter_source.sample() {
            Ok(sample) => {
                let mut snapshot = state.update(sample.disks, &settings, Instant::now());
                snapshot.top_process = sample.top_process;
                publish(&updates, hwnd, snapshot);
            }
            Err(error) => {
                source = None;
                state.reset();
                publish(&updates, hwnd, MonitorSnapshot::unavailable(error));
            }
        }
    }
}

fn publish(updates: &Sender<MonitorSnapshot>, hwnd: isize, snapshot: MonitorSnapshot) {
    if updates.send(snapshot).is_ok() {
        unsafe {
            PostMessageW(hwnd as HWND, WM_APP_MONITOR_UPDATE, 0, 0);
        }
    }
}

struct Counter {
    name: String,
    handle: PDH_HCOUNTER,
}

struct ProcessCounter {
    name: String,
    handle: PDH_HCOUNTER,
}

struct MonitorSample {
    disks: Vec<DiskReading>,
    top_process: Option<ProcessUsage>,
}

struct CounterSource {
    query: PDH_HQUERY,
    counters: Vec<Counter>,
    process_counters: Vec<ProcessCounter>,
    process_refreshed_at: Instant,
    opened_at: Instant,
}

impl CounterSource {
    fn open() -> Result<Self, String> {
        let mut query = std::ptr::null_mut();
        let status = unsafe { PdhOpenQueryW(std::ptr::null(), 0, &mut query) };
        if status != 0 {
            return Err(pdh_error(
                "Could not open the Windows disk performance query",
                status,
            ));
        }

        // PDH accepts English counter paths through this API, then gives us the
        // localized path needed to expand the instance wildcard on any Windows locale.
        let wildcard = wide("\\PhysicalDisk(*)\\% Disk Time");
        let mut wildcard_counter = std::ptr::null_mut();
        let status =
            unsafe { PdhAddEnglishCounterW(query, wildcard.as_ptr(), 0, &mut wildcard_counter) };
        if status != 0 {
            unsafe { PdhCloseQuery(query) };
            return Err(pdh_error(
                "The PhysicalDisk % Disk Time counter is unavailable",
                status,
            ));
        }
        let localized_wildcard = match counter_path(wildcard_counter) {
            Ok(path) => path,
            Err(error) => {
                unsafe { PdhCloseQuery(query) };
                return Err(error);
            }
        };
        unsafe { PdhRemoveCounter(wildcard_counter) };

        let paths = match expand_counter_path(
            &localized_wildcard,
            "Could not enumerate physical disks",
        ) {
            Ok(paths) => paths,
            Err(error) => {
                unsafe { PdhCloseQuery(query) };
                return Err(error);
            }
        };
        let mut counters = Vec::new();
        for path in paths {
            let path = wide(&path);
            let mut handle = std::ptr::null_mut();
            let status = unsafe { PdhAddCounterW(query, path.as_ptr(), 0, &mut handle) };
            if status != 0 {
                continue;
            }
            let name = match counter_instance_name(handle) {
                Ok(name) => name,
                Err(_) => {
                    unsafe { PdhRemoveCounter(handle) };
                    continue;
                }
            };
            if name == "_Total" {
                unsafe { PdhRemoveCounter(handle) };
            } else {
                counters.push(Counter { name, handle });
            }
        }

        if counters.is_empty() {
            unsafe { PdhCloseQuery(query) };
            return Err(
                "Physical disk activity counters are unavailable. No counters were enabled or exposed; Disk Light did not change system settings.".to_owned(),
            );
        }

        let process_counters = add_process_counters(query);
        let status = unsafe { PdhCollectQueryData(query) };
        if status != 0 {
            unsafe { PdhCloseQuery(query) };
            return Err(pdh_error("Could not collect disk activity", status));
        }
        Ok(Self {
            query,
            counters,
            process_counters,
            process_refreshed_at: Instant::now(),
            opened_at: Instant::now(),
        })
    }

    fn age(&self) -> Duration {
        self.opened_at.elapsed()
    }

    fn sample(&mut self) -> Result<MonitorSample, String> {
        if self.process_refreshed_at.elapsed() >= PROCESS_REFRESH_INTERVAL {
            self.refresh_process_counters();
        }
        let status = unsafe { PdhCollectQueryData(self.query) };
        if status != 0 {
            return Err(pdh_error("Could not collect disk activity", status));
        }

        let mut readings = Vec::with_capacity(self.counters.len());
        for counter in &self.counters {
            let mut counter_type = 0;
            let mut value: PDH_FMT_COUNTERVALUE = unsafe { std::mem::zeroed() };
            let status = unsafe {
                PdhGetFormattedCounterValue(
                    counter.handle,
                    PDH_FMT_DOUBLE,
                    &mut counter_type,
                    &mut value,
                )
            };
            if status != 0 || (value.CStatus != 0 && value.CStatus != 1) {
                return Err(format!(
                    "Windows stopped returning valid data for disk '{}'.",
                    counter.name
                ));
            }
            let busy = unsafe { value.Anonymous.doubleValue };
            if !busy.is_finite() {
                return Err(format!(
                    "Windows returned an invalid activity value for disk '{}'.",
                    counter.name
                ));
            }
            readings.push(DiskReading {
                name: counter.name.clone(),
                busy_percent: busy.clamp(0.0, 100.0),
            });
        }
        Ok(MonitorSample {
            disks: readings,
            top_process: self.top_process(),
        })
    }

    fn refresh_process_counters(&mut self) {
        for counter in self.process_counters.drain(..) {
            unsafe { PdhRemoveCounter(counter.handle) };
        }
        self.process_counters = add_process_counters(self.query);
        self.process_refreshed_at = Instant::now();
    }

    fn top_process(&self) -> Option<ProcessUsage> {
        let mut top: Option<ProcessUsage> = None;
        for counter in &self.process_counters {
            let mut counter_type = 0;
            let mut value: PDH_FMT_COUNTERVALUE = unsafe { std::mem::zeroed() };
            let status = unsafe {
                PdhGetFormattedCounterValue(
                    counter.handle,
                    PDH_FMT_DOUBLE,
                    &mut counter_type,
                    &mut value,
                )
            };
            if status != 0 || (value.CStatus != 0 && value.CStatus != 1) {
                continue;
            }
            let io_bytes_per_second = unsafe { value.Anonymous.doubleValue };
            if !io_bytes_per_second.is_finite() || io_bytes_per_second <= 0.0 {
                continue;
            }
            let is_top = top.as_ref().is_none_or(|current| {
                io_bytes_per_second > current.io_bytes_per_second
            });
            if is_top {
                top = Some(ProcessUsage {
                    name: counter.name.clone(),
                    io_bytes_per_second,
                });
            }
        }
        top
    }
}

impl Drop for CounterSource {
    fn drop(&mut self) {
        if !self.query.is_null() {
            unsafe { PdhCloseQuery(self.query) };
        }
    }
}

fn counter_info(handle: PDH_HCOUNTER) -> Result<Vec<usize>, String> {
    let mut bytes = 0;
    let status = unsafe { PdhGetCounterInfoW(handle, false, &mut bytes, std::ptr::null_mut()) };
    if status != PDH_MORE_DATA || bytes < std::mem::size_of::<PDH_COUNTER_INFO_W>() as u32 {
        return Err(pdh_error("Could not read the performance counter path", status));
    }
    let words = (bytes as usize).div_ceil(std::mem::size_of::<usize>());
    let mut buffer = vec![0usize; words];
    let status = unsafe {
        PdhGetCounterInfoW(
            handle,
            false,
            &mut bytes,
            buffer.as_mut_ptr().cast::<PDH_COUNTER_INFO_W>(),
        )
    };
    if status != 0 {
        return Err(pdh_error("Could not read the performance counter path", status));
    }
    Ok(buffer)
}

fn counter_path(handle: PDH_HCOUNTER) -> Result<String, String> {
    let buffer = counter_info(handle)?;
    let info = buffer.as_ptr().cast::<PDH_COUNTER_INFO_W>();
    let path = unsafe { (*info).szFullPath };
    wide_pointer_to_string(path)
        .ok_or_else(|| "Windows returned an empty performance counter path.".to_owned())
}

fn counter_instance_name(handle: PDH_HCOUNTER) -> Result<String, String> {
    let buffer = counter_info(handle)?;
    let info = buffer.as_ptr().cast::<PDH_COUNTER_INFO_W>();
    let instance = unsafe { (*info).Anonymous.CounterPath.szInstanceName };
    wide_pointer_to_string(instance)
        .ok_or_else(|| "Windows returned an empty counter instance name.".to_owned())
}

fn add_process_counters(query: PDH_HQUERY) -> Vec<ProcessCounter> {
    let wildcard = wide("\\Process(*)\\IO Data Bytes/sec");
    let mut wildcard_counter = std::ptr::null_mut();
    let status = unsafe {
        PdhAddEnglishCounterW(query, wildcard.as_ptr(), 0, &mut wildcard_counter)
    };
    if status != 0 {
        return Vec::new();
    }

    let localized_wildcard = counter_path(wildcard_counter);
    unsafe { PdhRemoveCounter(wildcard_counter) };
    let Ok(localized_wildcard) = localized_wildcard else {
        return Vec::new();
    };
    let Ok(paths) = expand_counter_path(
        &localized_wildcard,
        "Could not enumerate process I/O counters",
    ) else {
        return Vec::new();
    };

    let mut counters = Vec::new();
    for path in paths {
        let path = wide(&path);
        let mut handle = std::ptr::null_mut();
        if unsafe { PdhAddCounterW(query, path.as_ptr(), 0, &mut handle) } != 0 {
            continue;
        }
        let Ok(name) = counter_instance_name(handle) else {
            unsafe { PdhRemoveCounter(handle) };
            continue;
        };
        if name == "_Total" || name.eq_ignore_ascii_case("Idle") {
            unsafe { PdhRemoveCounter(handle) };
        } else {
            counters.push(ProcessCounter { name, handle });
        }
    }
    counters
}

fn expand_counter_path(wildcard: &str, context: &str) -> Result<Vec<String>, String> {
    let wildcard = wide(wildcard);
    let mut chars = 0;
    let first = unsafe {
        PdhExpandWildCardPathW(
            std::ptr::null(),
            wildcard.as_ptr(),
            std::ptr::null_mut(),
            &mut chars,
            0,
        )
    };
    if first != PDH_MORE_DATA || chars == 0 {
        return Err(pdh_error(context, first));
    }
    let mut expanded = vec![0u16; chars as usize];
    let status = unsafe {
        PdhExpandWildCardPathW(
            std::ptr::null(),
            wildcard.as_ptr(),
            expanded.as_mut_ptr(),
            &mut chars,
            0,
        )
    };
    if status != 0 {
        return Err(pdh_error(context, status));
    }
    Ok(multi_string(&expanded))
}

fn wide(value: &str) -> Vec<u16> {
    value.encode_utf16().chain(Some(0)).collect()
}

fn wide_pointer_to_string(pointer: *const u16) -> Option<String> {
    if pointer.is_null() {
        return None;
    }
    let mut length = 0;
    unsafe {
        while *pointer.add(length) != 0 {
            length += 1;
        }
        Some(String::from_utf16_lossy(std::slice::from_raw_parts(
            pointer, length,
        )))
    }
}

fn multi_string(buffer: &[u16]) -> Vec<String> {
    let mut result = Vec::new();
    let mut start = 0;
    while start < buffer.len() && buffer[start] != 0 {
        let Some(offset) = buffer[start..].iter().position(|&unit| unit == 0) else {
            break;
        };
        result.push(String::from_utf16_lossy(&buffer[start..start + offset]));
        start += offset + 1;
    }
    result
}

fn pdh_error(context: &str, status: u32) -> String {
    format!("{context} (PDH status 0x{status:08X}).")
}
