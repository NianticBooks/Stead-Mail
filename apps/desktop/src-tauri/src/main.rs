// Prevents additional console window on Windows in release, DO NOT REMOVE!!
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    linux_graphics_workarounds();
    desktop_lib::run()
}

/// WebKitGTK on Wayland + the proprietary Nvidia driver aborts with "Error 71 (Protocol error)
/// dispatching to Wayland display" unless explicit sync is disabled. Verified on this project's
/// reference machine (RTX 5060, driver 615, Hyprland) on 2026-10-27. Must run before any GTK
/// initialisation, hence `main` and not `lib::run`. Set `STEAD_NO_GFX_WORKAROUND=1` to opt out.
#[cfg(target_os = "linux")]
fn linux_graphics_workarounds() {
    if std::env::var_os("STEAD_NO_GFX_WORKAROUND").is_some() {
        return;
    }
    let on_wayland = std::env::var_os("WAYLAND_DISPLAY").is_some();
    let nvidia = std::path::Path::new("/sys/module/nvidia").exists()
        || std::path::Path::new("/proc/driver/nvidia/version").exists();
    if on_wayland && nvidia && std::env::var_os("__NV_DISABLE_EXPLICIT_SYNC").is_none() {
        // SAFETY: single-threaded at this point; no other thread can be reading the environment.
        unsafe { std::env::set_var("__NV_DISABLE_EXPLICIT_SYNC", "1") };
    }
}

#[cfg(not(target_os = "linux"))]
fn linux_graphics_workarounds() {}
