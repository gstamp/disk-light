use crate::config::Settings;
use std::time::{Duration, Instant};

#[derive(Debug, Clone, PartialEq)]
pub struct DiskReading {
    pub name: String,
    pub busy_percent: f64,
}

#[derive(Debug, Clone)]
pub struct ProcessUsage {
    pub name: String,
    pub io_bytes_per_second: f64,
}

#[derive(Debug, Clone)]
pub struct MonitorSnapshot {
    pub status: String,
    pub disks: Vec<DiskReading>,
    pub active_disk: Option<DiskReading>,
    pub top_process: Option<ProcessUsage>,
    pub light_active: bool,
}

impl MonitorSnapshot {
    pub fn unavailable(message: impl Into<String>) -> Self {
        Self {
            status: message.into(),
            disks: Vec::new(),
            active_disk: None,
            top_process: None,
            light_active: false,
        }
    }
}

#[derive(Default)]
pub struct ActivityState {
    active: bool,
    above_since: Option<Instant>,
    below_since: Option<Instant>,
}

impl ActivityState {
    pub fn update(
        &mut self,
        all_disks: Vec<DiskReading>,
        settings: &Settings,
        now: Instant,
    ) -> MonitorSnapshot {
        let mut disks: Vec<_> = all_disks
            .into_iter()
            .filter(|disk| settings.monitors(&disk.name))
            .collect();
        disks.sort_by(|a, b| {
            b.busy_percent
                .partial_cmp(&a.busy_percent)
                .unwrap_or(std::cmp::Ordering::Equal)
        });

        if disks.is_empty() {
            self.reset();
            return MonitorSnapshot {
                status: if settings.monitor_all_disks {
                    "No physical disks were found".to_owned()
                } else {
                    "No selected disks are available".to_owned()
                },
                disks,
                active_disk: None,
                top_process: None,
                light_active: false,
            };
        }

        let busiest = disks.first().cloned();
        if self.active {
            let all_clear = disks
                .iter()
                .all(|disk| disk.busy_percent < f64::from(settings.clear_percent));
            if all_clear {
                let since = *self.below_since.get_or_insert(now);
                if now.duration_since(since)
                    >= Duration::from_secs(u64::from(settings.clear_seconds))
                {
                    self.active = false;
                    self.below_since = None;
                    self.above_since = None;
                }
            } else {
                self.below_since = None;
            }
        } else {
            let any_triggered = disks
                .iter()
                .any(|disk| disk.busy_percent >= f64::from(settings.trigger_percent));
            if any_triggered {
                let since = *self.above_since.get_or_insert(now);
                if now.duration_since(since)
                    >= Duration::from_secs(u64::from(settings.trigger_seconds))
                {
                    self.active = true;
                    self.above_since = None;
                    self.below_since = None;
                }
            } else {
                self.above_since = None;
            }
        }

        MonitorSnapshot {
            status: format!(
                "Monitoring {} disk{}",
                disks.len(),
                if disks.len() == 1 { "" } else { "s" }
            ),
            active_disk: self.active.then_some(busiest).flatten(),
            top_process: None,
            disks,
            light_active: self.active,
        }
    }

    pub fn reset(&mut self) {
        self.active = false;
        self.above_since = None;
        self.below_since = None;
    }
}
