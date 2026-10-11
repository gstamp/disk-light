use crate::activity::{DiskReading, ProcessUsage};
use crate::win32::wide;
use std::time::{Duration, Instant};
use windows_sys::Win32::System::Performance::{
    PDH_COUNTER_INFO_W, PDH_FMT_COUNTERVALUE, PDH_FMT_DOUBLE, PDH_HCOUNTER, PDH_HQUERY,
    PDH_MORE_DATA, PdhAddCounterW, PdhAddEnglishCounterW, PdhCloseQuery, PdhCollectQueryData,
    PdhExpandWildCardPathW, PdhGetCounterInfoW, PdhGetFormattedCounterValue, PdhOpenQueryW,
    PdhRemoveCounter,
};

const PROCESS_REFRESH_INTERVAL: Duration = Duration::from_secs(5);

struct Counter {
    name: String,
    handle: PDH_HCOUNTER,
}

struct ProcessCounter {
    name: String,
    handle: PDH_HCOUNTER,
}

pub(super) struct MonitorSample {
    pub(super) disks: Vec<DiskReading>,
    pub(super) top_process: Option<ProcessUsage>,
}

pub(super) struct CounterSource {
    query: PDH_HQUERY,
    counters: Vec<Counter>,
    process_counters: Vec<ProcessCounter>,
    process_refreshed_at: Instant,
    opened_at: Instant,
}

impl CounterSource {
    pub(super) fn open() -> Result<Self, String> {
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

        let paths =
            match expand_counter_path(&localized_wildcard, "Could not enumerate physical disks") {
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

    pub(super) fn age(&self) -> Duration {
        self.opened_at.elapsed()
    }

    pub(super) fn sample(&mut self) -> Result<MonitorSample, String> {
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
            let is_top = top
                .as_ref()
                .is_none_or(|current| io_bytes_per_second > current.io_bytes_per_second);
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
        return Err(pdh_error(
            "Could not read the performance counter path",
            status,
        ));
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
        return Err(pdh_error(
            "Could not read the performance counter path",
            status,
        ));
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
    let status =
        unsafe { PdhAddEnglishCounterW(query, wildcard.as_ptr(), 0, &mut wildcard_counter) };
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
