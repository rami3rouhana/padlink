//! The Windows notification-area icon: shows what is connected, pops up a
//! note when a controller comes or goes, and holds the few settings worth a
//! click. Runs on its own thread; the engine never waits for it.

use crate::status::{State, Status};
use std::path::PathBuf;
use std::ptr::{null, null_mut};
use std::sync::{Mutex, OnceLock};
use windows_sys::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, WPARAM};
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::System::Registry::{
    RegDeleteKeyValueW, RegGetValueW, RegSetKeyValueW, HKEY_CURRENT_USER, REG_SZ, RRF_RT_REG_SZ,
};
use windows_sys::Win32::UI::Shell::{
    ShellExecuteW, Shell_NotifyIconW, NIF_ICON, NIF_INFO, NIF_MESSAGE, NIF_TIP, NIIF_INFO, NIIF_WARNING, NIM_ADD,
    NIM_DELETE, NIM_MODIFY, NOTIFYICONDATAW,
};
use windows_sys::Win32::UI::WindowsAndMessaging::*;

const WM_TRAY: u32 = WM_APP + 1;
const ID_AUTOSTART: usize = 1;
const ID_SETTINGS: usize = 2;
const ID_QUIT: usize = 3;
const RUN_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";
const RUN_VALUE: &str = "padlink";
pub const DRIVER_URL: &str = "https://github.com/nefarius/ViGEmBus/releases/latest";

struct Tray {
    status: Status,
    settings: Option<PathBuf>,
    /// Icon handles, kept as numbers so this can live in a static.
    idle_icon: usize,
    active_icon: usize,
    /// Broadcast by Explorer when it (re)starts and has forgotten every icon.
    taskbar_created: u32,
}

static TRAY: OnceLock<Tray> = OnceLock::new();
/// The tooltip currently shown, to skip updates that change nothing.
static SHOWN: Mutex<String> = Mutex::new(String::new());

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(Some(0)).collect()
}

/// Copies as much of `s` as fits, always leaving a terminating zero.
fn fill(dst: &mut [u16], s: &str) {
    let mut n = 0;
    for unit in s.encode_utf16().take(dst.len() - 1) {
        dst[n] = unit;
        n += 1;
    }
    dst[n] = 0;
}

/// A gamepad silhouette, `n` by `n` BGRA pixels, top row first.
fn icon_pixels(n: usize, (r, g, b): (u8, u8, u8)) -> Vec<u8> {
    fn inside(u: f32, v: f32) -> bool {
        let circle = |cx: f32, cy: f32, rad: f32| (u - cx).powi(2) + (v - cy).powi(2) <= rad * rad;
        let body = {
            let dx = ((u - 0.5).abs() - (0.44 - 0.2)).max(0.0);
            let dy = ((v - 0.46).abs() - (0.2 - 0.2)).max(0.0);
            dx * dx + dy * dy <= 0.2 * 0.2
        };
        let grips = circle(0.24, 0.62, 0.2) || circle(0.76, 0.62, 0.2);
        let dpad = ((u - 0.29).abs() < 0.04 && (v - 0.45).abs() < 0.12) || ((u - 0.29).abs() < 0.12 && (v - 0.45).abs() < 0.04);
        let buttons = circle(0.64, 0.5, 0.06) || circle(0.77, 0.4, 0.06);
        (body || grips) && !dpad && !buttons
    }
    const SUB: usize = 4;
    let mut px = Vec::with_capacity(n * n * 4);
    for y in 0..n {
        for x in 0..n {
            // Coverage from a 4x4 grid of samples smooths the edges.
            let hits = (0..SUB * SUB)
                .filter(|i| {
                    let u = (x as f32 + (i % SUB) as f32 / SUB as f32 + 0.125) / n as f32;
                    let v = (y as f32 + (i / SUB) as f32 / SUB as f32 + 0.125) / n as f32;
                    inside(u, v)
                })
                .count();
            px.extend_from_slice(&[b, g, r, (hits * 255 / (SUB * SUB)) as u8]);
        }
    }
    px
}

unsafe fn make_icon(rgb: (u8, u8, u8)) -> usize {
    let n = GetSystemMetrics(SM_CXSMICON).max(16) as usize;
    let colour = icon_pixels(n, rgb);
    // All zero: the alpha channel above decides what is transparent. Rows are padded to 16 bits.
    let mask = vec![0u8; n.div_ceil(16) * 2 * n];
    CreateIcon(GetModuleHandleW(null()), n as i32, n as i32, 1, 32, mask.as_ptr(), colour.as_ptr()) as usize
}

fn notify_data(hwnd: HWND) -> NOTIFYICONDATAW {
    // SAFETY: the structure is plain data for which all-zero is valid.
    let mut d: NOTIFYICONDATAW = unsafe { std::mem::zeroed() };
    d.cbSize = std::mem::size_of::<NOTIFYICONDATAW>() as u32;
    d.hWnd = hwnd;
    d.uID = 1;
    d
}

/// Tooltip text and whether any controller is connected right now.
fn summary(tray: &Tray) -> (String, bool) {
    let (rows, senders) = tray.status.snapshot();
    let connected = rows.iter().filter(|r| matches!(r.state, State::Connected | State::Quiet)).count();
    let text = match (connected, senders) {
        (0, 0) => "padlink - no sender found yet".to_string(),
        (0, _) => "padlink - waiting for a controller".to_string(),
        (1, _) => "padlink - 1 controller".to_string(),
        (n, _) => format!("padlink - {n} controllers"),
    };
    (text, connected > 0)
}

unsafe fn add_icon(hwnd: HWND, tray: &Tray) {
    let (text, active) = summary(tray);
    let mut d = notify_data(hwnd);
    d.uFlags = NIF_MESSAGE | NIF_ICON | NIF_TIP;
    d.uCallbackMessage = WM_TRAY;
    d.hIcon = (if active { tray.active_icon } else { tray.idle_icon }) as _;
    fill(&mut d.szTip, &text);
    // Fails while Explorer is still starting; its "TaskbarCreated" message brings us back here.
    if Shell_NotifyIconW(NIM_ADD, &d) != 0 {
        *SHOWN.lock().unwrap() = text;
    }
}

/// Runs twice a second: keeps the icon and tooltip current, shows pending notes.
unsafe fn refresh(hwnd: HWND, tray: &Tray) {
    let (text, active) = summary(tray);
    let mut shown = SHOWN.lock().unwrap();
    if *shown != text {
        let mut d = notify_data(hwnd);
        d.uFlags = NIF_ICON | NIF_TIP;
        d.hIcon = (if active { tray.active_icon } else { tray.idle_icon }) as _;
        fill(&mut d.szTip, &text);
        if Shell_NotifyIconW(NIM_MODIFY, &d) != 0 {
            *shown = text;
        }
    }
    drop(shown);

    let notes = tray.status.take_notes();
    if !notes.is_empty() {
        let mut d = notify_data(hwnd);
        d.uFlags = NIF_INFO;
        d.dwInfoFlags = if notes.iter().any(|n| n.starts_with('!')) { NIIF_WARNING } else { NIIF_INFO };
        let lines: Vec<&str> = notes.iter().map(|n| n.trim_start_matches(['+', '-', '!', ' '])).collect();
        fill(&mut d.szInfoTitle, "padlink");
        fill(&mut d.szInfo, &lines.join("\n"));
        Shell_NotifyIconW(NIM_MODIFY, &d);
    }
}

fn run_command() -> Option<Vec<u16>> {
    let exe = std::env::current_exe().ok()?;
    Some(wide(&format!("\"{}\"", exe.display())))
}

unsafe fn autostart_enabled() -> bool {
    RegGetValueW(HKEY_CURRENT_USER, wide(RUN_KEY).as_ptr(), wide(RUN_VALUE).as_ptr(), RRF_RT_REG_SZ, null_mut(), null_mut(), null_mut())
        == 0
}

unsafe fn set_autostart(on: bool) {
    let (key, value) = (wide(RUN_KEY), wide(RUN_VALUE));
    match run_command() {
        Some(cmd) if on => {
            RegSetKeyValueW(HKEY_CURRENT_USER, key.as_ptr(), value.as_ptr(), REG_SZ, cmd.as_ptr().cast(), (cmd.len() * 2) as u32);
        }
        _ => {
            RegDeleteKeyValueW(HKEY_CURRENT_USER, key.as_ptr(), value.as_ptr());
        }
    }
}

unsafe fn open(target: &str) {
    ShellExecuteW(null_mut(), wide("open").as_ptr(), wide(target).as_ptr(), null(), null(), SW_SHOWNORMAL);
}

unsafe fn show_menu(hwnd: HWND, tray: &Tray) {
    let menu = CreatePopupMenu();
    // Informational lines: shown in normal text, but not clickable.
    let info = |text: &str| {
        AppendMenuW(menu, MF_STRING | MF_DISABLED, 0, wide(text).as_ptr());
    };
    let (rows, senders) = tray.status.snapshot();
    if rows.is_empty() {
        info(if senders == 0 { "No sender found yet - is the Pi on?" } else { "Sender connected - waiting for a controller" });
    }
    for r in &rows {
        let mut text = format!("Player {}:  {}", r.player, r.name);
        let mut details = Vec::new();
        if r.bus != "-" {
            details.push(r.bus.to_string());
        }
        if let Some(b) = r.battery {
            details.push(format!("battery {b}%"));
        }
        if let (Some(rtt), State::Connected) = (r.rtt_us, &r.state) {
            details.push(crate::engine::ms(rtt));
        }
        if r.state != State::Connected {
            details.push(r.state_text());
        }
        if !details.is_empty() {
            text.push_str(&format!("   ({})", details.join(", ")));
        }
        info(&text);
    }
    AppendMenuW(menu, MF_SEPARATOR, 0, null());
    let autostart = autostart_enabled();
    AppendMenuW(menu, MF_STRING | if autostart { MF_CHECKED } else { 0 }, ID_AUTOSTART, wide("Start with Windows").as_ptr());
    if tray.settings.is_some() {
        AppendMenuW(menu, MF_STRING, ID_SETTINGS, wide("Open settings file").as_ptr());
    }
    AppendMenuW(menu, MF_STRING, ID_QUIT, wide("Quit").as_ptr());

    let mut at = POINT { x: 0, y: 0 };
    GetCursorPos(&mut at);
    // Without this the menu does not close when clicking elsewhere.
    SetForegroundWindow(hwnd);
    let choice = TrackPopupMenu(menu, TPM_RETURNCMD | TPM_NONOTIFY | TPM_RIGHTBUTTON, at.x, at.y, 0, hwnd, null());
    DestroyMenu(menu);
    match choice as usize {
        ID_AUTOSTART => set_autostart(!autostart),
        ID_SETTINGS => {
            if let Some(path) = &tray.settings {
                open(&path.display().to_string());
            }
        }
        ID_QUIT => {
            Shell_NotifyIconW(NIM_DELETE, &notify_data(hwnd));
            // The virtual controllers unplug as the process ends.
            std::process::exit(0);
        }
        _ => {}
    }
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    let Some(tray) = TRAY.get() else { return DefWindowProcW(hwnd, msg, wparam, lparam) };
    match msg {
        WM_TRAY if matches!(lparam as u32, WM_LBUTTONUP | WM_RBUTTONUP) => show_menu(hwnd, tray),
        WM_TIMER => refresh(hwnd, tray),
        _ if msg == tray.taskbar_created => add_icon(hwnd, tray),
        _ => return DefWindowProcW(hwnd, msg, wparam, lparam),
    }
    0
}

/// Shows the icon and serves it until the process ends.
pub fn run(status: Status, settings: Option<PathBuf>) {
    // SAFETY: ordinary single-threaded Win32 window code; every pointer passed
    // outlives the call it is passed to.
    unsafe {
        let tray = TRAY.get_or_init(|| Tray {
            status,
            settings,
            idle_icon: make_icon((0x8b, 0x94, 0x9e)),
            active_icon: make_icon((0x2e, 0xa0, 0x43)),
            taskbar_created: RegisterWindowMessageW(wide("TaskbarCreated").as_ptr()),
        });
        // The program may have been moved since "Start with Windows" was ticked.
        if autostart_enabled() {
            set_autostart(true);
        }
        let class = wide("padlink-tray");
        let instance = GetModuleHandleW(null());
        let mut wc: WNDCLASSW = std::mem::zeroed();
        wc.lpfnWndProc = Some(wndproc);
        wc.hInstance = instance;
        wc.lpszClassName = class.as_ptr();
        RegisterClassW(&wc);
        // Never shown; it exists to receive the icon's messages.
        let hwnd = CreateWindowExW(0, class.as_ptr(), class.as_ptr(), 0, 0, 0, 0, 0, null_mut(), null_mut(), instance, null());
        add_icon(hwnd, tray);
        SetTimer(hwnd, 1, 500, None);
        let mut msg: MSG = std::mem::zeroed();
        while GetMessageW(&mut msg, null_mut(), 0, 0) > 0 {
            TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
}

/// An error box, for when there is no console to print to.
pub fn error_box(text: &str) {
    // SAFETY: both strings are zero-terminated and live through the call.
    unsafe {
        MessageBoxW(null_mut(), wide(text).as_ptr(), wide("padlink").as_ptr(), MB_OK | MB_ICONERROR);
    }
}

/// Explains the missing driver and offers to open its download page.
pub fn offer_driver() {
    let text = "padlink needs the ViGEmBus driver to create virtual controllers, and it is not installed.\n\n\
                It is a one-time install. Open the download page now?\n\
                (Run the installer, then start padlink again.)";
    // SAFETY: as above.
    unsafe {
        if MessageBoxW(null_mut(), wide(text).as_ptr(), wide("padlink").as_ptr(), MB_YESNO | MB_ICONWARNING) == IDYES {
            open(DRIVER_URL);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn icon_is_a_shape_with_holes_and_clear_corners() {
        let n = 32;
        let px = icon_pixels(n, (1, 2, 3));
        let alpha = |x: usize, y: usize| px[(y * n + x) * 4 + 3];
        assert_eq!(px.len(), n * n * 4);
        assert_eq!(&px[..3], &[3, 2, 1], "BGR order");
        assert_eq!((alpha(0, 0), alpha(n - 1, 0), alpha(n / 2, n - 1)), (0, 0, 0));
        assert_eq!(alpha(n / 2, 14), 255, "body");
        assert_eq!(alpha(9, 14), 0, "d-pad is cut out");
    }

    #[test]
    fn fill_truncates_and_terminates() {
        let mut buf = [7u16; 4];
        fill(&mut buf, "abcdef");
        assert_eq!(buf, [97, 98, 99, 0]);
    }
}
