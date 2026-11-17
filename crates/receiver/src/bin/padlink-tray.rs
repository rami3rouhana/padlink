//! The receiver without a console window: on Windows it lives as an icon in
//! the notification area. Elsewhere it behaves like `padlink-receiver`.
#![cfg_attr(windows, windows_subsystem = "windows")]

fn main() {
    padlink_receiver::run_tray()
}
