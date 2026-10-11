# Disk Light implementation spec

**Status:** Draft, before implementation  
**Date:** 2026-10-10

## Purpose

Disk Light is a small native Windows utility that shows a temporary on-screen light when a disk stays under heavy load. The light is absent during normal activity. It is a visual warning, not a diagnosis of memory paging or a particular disk fault.

## First version

- Support Windows 10 and Windows 11 on x64.
- Build a single Rust desktop process with the stable toolchain and native Win32 APIs.
- Monitor local physical disk activity without administrator privileges.
- Show one compact, non-activating overlay near the lower-right edge of the primary display while any monitored disk meets the heavy-load rule.
- Keep a notification-area icon available for status, settings, and exit. The activity overlay remains hidden when disks return to normal.
- Start on login only when the user opts in.

## Activity rule

Use the Windows `PhysicalDisk` performance counters, sampling about twice per second. Ignore the `_Total` instance and evaluate each disk separately.

For the initial default, show an amber light when any monitored disk's `% Disk Time` remains at or above 70% for 2 seconds. Switch it to red when the busiest monitored disk reaches 95%. Hide it after all monitored disks stay below 60% for 5 seconds. The gap between show and hide thresholds prevents flicker around the boundary. Keep the show and clear thresholds and delays configurable.

If counters are missing or unavailable, leave the overlay hidden and report the monitor state through the notification-area menu. Do not enable system counters automatically or ask for elevation.

## Appearance and interaction

- Use a small circular amber light for moderate disk activity and switch it to red near saturation, with a restrained pulse while active. Do not show toast notifications or play sounds.
- Place the overlay inside the primary display's work area with a margin from the lower-right edge. Do not steal keyboard focus.
- On hover, identify the active disk, show its current busy percentage, and show the process with the highest reported process I/O rate when available. When several disks qualify, show the busiest one.
- The notification-area menu provides open settings, show/hide overlay preview, and exit.
- Settings initially cover monitored disks, trigger and clear thresholds, overlay position, and start-on-login.

## Implementation shape

Keep the app in one process. Separate disk sampling, the activity state machine, and the Win32 UI so each can be changed without coupling the others. Send sampled values to the UI thread at a fixed interval; do not busy-wait. Use native Win32 windows and notification-area APIs. Avoid WebView, Electron, a separate service, and background disk scans.

Persist user settings under `%LOCALAPPDATA%\DiskLight`. Do not inspect file contents. Process I/O is a best-effort indicator because the Windows process counter includes file, network, and device I/O rather than attributing bytes to a physical disk.

## Resource and platform targets

- No administrator rights required.
- Target an idle private working set below 20 MiB after startup settles. Treat this as a measured goal, not a compile-time guarantee.
- Keep idle CPU use negligible by sampling on a timer and doing no work while the UI state is unchanged.
- No network access or telemetry.

## Acceptance criteria

1. With no sustained disk pressure, the activity overlay is not visible.
2. Sustained high activity on any monitored disk makes the light appear within 3 seconds.
3. Once all monitored disks stay below the clear threshold, the light disappears within 6 seconds.
4. Brief spikes do not make the light flash repeatedly.
5. The light does not take keyboard focus or block normal use of other windows.
6. The menu can identify unavailable monitoring and exit the app.
7. The process runs without elevation and meets the idle memory target on a documented Windows test machine.

## References

- Microsoft documents the `PhysicalDisk` `% Disk Time` counter as the share of elapsed time a disk is busy servicing reads or writes: https://learn.microsoft.com/en-us/windows/win32/cimwin32prov/win32-perfrawdata-perfdisk-physicaldisk
- Microsoft lists physical-disk counters for diagnosing storage bottlenecks: https://learn.microsoft.com/en-us/troubleshoot/windows-server/performance/troubleshoot-performance-problems-in-windows
