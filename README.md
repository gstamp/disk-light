# Disk Light

Disk Light is a small Windows tray utility that shows a click-through light while a physical disk stays busy. It uses the Windows `PhysicalDisk` `% Disk Time` performance counter and does not scan files.

## Build and run

Build for 64-bit Windows 10 or 11 with the stable MSVC Rust toolchain:

```powershell
cargo build --release --target x86_64-pc-windows-msvc
```

Run `target\x86_64-pc-windows-msvc\release\disk-light.exe`. The process starts in the notification area without opening a console window. Right-click its icon to open settings, preview the overlay, or exit.

## Installer

The installer supports Windows 10 and 11 x64. It installs for the current user under
`%LOCALAPPDATA%\Programs\Disk Light`, adds a Start menu shortcut, and provides an
uninstaller in Windows Settings. It does not require administrator rights.
Sign-in startup remains opt-in through the app's settings. Uninstall removes the
startup entry and keeps `%LOCALAPPDATA%\DiskLight\settings.json` for a later reinstall.
Exit Disk Light before upgrading or uninstalling.

To build the MSI, install the .NET 8 SDK or newer alongside the Rust MSVC
toolchain, then run:

```powershell
.\scripts\build-installer.ps1
```

The script restores the pinned WiX tool, builds the release binary, and reads the
version from `Cargo.toml`. The output is `dist\DiskLight-<version>-windows-x64.msi`.
The .NET SDK and WiX are only required on the build machine.
Double-click the MSI to install, then launch Disk Light from the Start menu.
For a silent installation:

```powershell
msiexec /i dist\DiskLight-0.1.1-windows-x64.msi /qn /norestart
```

Higher package versions upgrade an existing MSI installation. Settings remain in
place. If you installed the earlier EXE installer, uninstall it before installing
the MSI. The MSI is unsigned; Windows may show a publisher warning.

The executable, Start menu shortcut, and Installed Apps entry use
`assets/disk-light.ico`, derived from the amber `assets/disk-light-busy.svg` artwork.
The ICO includes sizes from 16 to 256 pixels and is embedded by `build.rs`.

## Behavior

- Samples each physical disk about twice per second and ignores `_Total`.
- Shows an amber light after a disk reaches 70% busy for 2 seconds; clears it after every monitored disk stays below 60% for 5 seconds.
- Switches to red when the busiest monitored disk reaches 95% busy.
- On hover, shows the process with the highest reported process I/O rate when Windows exposes it. This rate includes file, network, and device I/O, so it is an indicator rather than exact attribution to a specific physical disk.
- Keeps the overlay hidden if the counters are unavailable. Disk Light does not enable or repair performance counters and does not request elevation.
- Stores settings in `%LOCALAPPDATA%\DiskLight\settings.json`.
- Adds a per-user sign-in entry only when **Start Disk Light when I sign in** is selected.
- Uses no network access, toast notifications, or sounds.

The release binary is optimized for a small resident footprint. The under-20-MiB idle working-set figure is a target; it still needs a recorded measurement on a named Windows 10 or 11 x64 machine before it can be treated as verified.
