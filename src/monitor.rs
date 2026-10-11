mod pdh;

use self::pdh::CounterSource;

use crate::activity::{ActivityState, MonitorSnapshot};
use crate::config::Settings;
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};
use windows_sys::Win32::Foundation::HWND;
use windows_sys::Win32::UI::WindowsAndMessaging::PostMessageW;

pub const WM_APP_MONITOR_UPDATE: u32 = 0x8001;

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
    let mut last_open_attempt = None;

    loop {
        match commands.recv_timeout(Duration::from_millis(500)) {
            Ok(MonitorCommand::Stop) => break,
            Ok(MonitorCommand::Reconfigure(next)) => {
                settings = next;
                state.reset();
                source = None;
                last_open_attempt = None;
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

        if source.is_none()
            && last_open_attempt
                .is_none_or(|attempt: Instant| attempt.elapsed() >= Duration::from_secs(5))
        {
            last_open_attempt = Some(Instant::now());
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
