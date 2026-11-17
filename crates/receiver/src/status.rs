//! What the receiver shows: a live table of controllers when it runs in a
//! terminal, plain log lines otherwise (piped to a file, verbose, dry run).

use crate::engine::ms;
use std::collections::VecDeque;
use std::fmt::Write as _;
use std::io::Write as _;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

#[derive(Clone, PartialEq)]
pub enum State {
    Connected,
    /// Nothing heard for a moment; inputs are released.
    Quiet,
    /// Disconnected; its player number is kept for this many more seconds.
    Away { left_s: u64 },
    /// The driver refused to create a virtual pad for it.
    NoPad,
}

#[derive(Clone, PartialEq)]
pub struct Row {
    pub player: u8,
    pub name: String,
    /// "USB", "Bluetooth" or "-".
    pub bus: &'static str,
    /// Type of the virtual pad.
    pub pad: &'static str,
    /// Number of the sender it is on; 0 while away.
    pub sender: u8,
    pub battery: Option<u8>,
    pub rtt_us: Option<u32>,
    pub state: State,
}

impl Row {
    pub fn battery_text(&self) -> String {
        self.battery.map_or("-".into(), |b| format!("{b}%"))
    }

    pub fn state_text(&self) -> String {
        match self.state {
            State::Connected => "connected".into(),
            State::Quiet => "no signal".into(),
            State::Away { left_s } => format!("away (player {} kept {left_s} s)", self.player),
            State::NoPad => "no virtual pad".into(),
        }
    }
}

/// Events kept for the table view.
const KEPT: usize = 8;

struct Inner {
    table: bool,
    title: String,
    rows: Vec<Row>,
    senders: usize,
    events: VecDeque<(Instant, String)>,
    started: Instant,
    drawn: Instant,
}

/// Shared between the engine, which reports, and whatever displays.
#[derive(Clone)]
pub struct Status(Arc<Mutex<Inner>>);

impl Status {
    pub fn new(title: String, table: bool) -> Status {
        if table {
            // Clear once; after that each redraw overwrites in place, without flicker.
            print!("\x1b[2J");
        } else {
            println!("{title}");
        }
        let now = Instant::now();
        let inner = Inner { table, title, rows: Vec::new(), senders: 0, events: VecDeque::new(), started: now, drawn: now };
        inner.draw();
        Status(Arc::new(Mutex::new(inner)))
    }

    /// Something happened that is worth telling: a controller came or went, a warning.
    pub fn event(&self, msg: String) {
        let mut s = self.0.lock().unwrap();
        if s.table {
            if s.events.len() == KEPT {
                s.events.pop_front();
            }
            s.events.push_back((Instant::now(), msg.trim_start().to_string()));
            s.draw();
        } else {
            println!("[{:7.2}] {msg}", s.started.elapsed().as_secs_f32());
            let _ = std::io::stdout().flush();
        }
    }

    /// The current list of controllers, ordered by player, and the number of senders.
    pub fn publish(&self, rows: Vec<Row>, senders: usize) {
        let mut s = self.0.lock().unwrap();
        let changed = s.rows != rows || s.senders != senders;
        (s.rows, s.senders) = (rows, senders);
        // Once a second regardless, so the "ago" times keep moving.
        if changed || s.drawn.elapsed() >= Duration::from_secs(1) {
            s.drawn = Instant::now();
            s.draw();
        }
    }
}

fn ago(t: Instant) -> String {
    match t.elapsed().as_secs() {
        s @ 0..=59 => format!("{s} s ago"),
        s @ 60..=3599 => format!("{} min ago", s / 60),
        s => format!("{} h ago", s / 3600),
    }
}

impl Inner {
    fn draw(&self) {
        if !self.table {
            return;
        }
        // "\x1b[K" clears the rest of each line, so shorter text leaves no residue.
        let mut out = String::from("\x1b[H");
        let mut line = |s: &str| {
            let _ = write!(out, "{s}\x1b[K\r\n");
        };
        line(&self.title);
        line("");
        if self.rows.is_empty() {
            line(if self.senders == 0 {
                "  No sender found yet. Start padlink-sender on the Pi; it finds this PC by itself."
            } else {
                "  Sender connected. Waiting for a controller to be plugged in or switched on."
            });
        } else {
            let w = self.rows.iter().map(|r| r.name.chars().count()).max().unwrap_or(0).max(10);
            line(&format!(
                "  {:<6}  {:<w$}  {:<9}  {:<6}  {:<7}  {:<10}  {:<11}  {}",
                "Player", "Controller", "Link", "Sender", "Battery", "Round trip", "Appears as", "Status"
            ));
            for r in &self.rows {
                line(&format!(
                    "  {:<6}  {:<w$}  {:<9}  {:<6}  {:<7}  {:<10}  {:<11}  {}",
                    r.player,
                    r.name,
                    r.bus,
                    if r.sender == 0 { "-".into() } else { r.sender.to_string() },
                    r.battery_text(),
                    r.rtt_us.map_or("-".into(), ms),
                    r.pad,
                    r.state_text()
                ));
            }
        }
        line("");
        if !self.events.is_empty() {
            line("  Recent:");
            for (t, msg) in &self.events {
                line(&format!("  {:>10}  {msg}", ago(*t)));
            }
            line("");
        }
        line("  Ctrl+C to quit");
        // Whatever was below a previously longer table.
        out.push_str("\x1b[J");
        let mut stdout = std::io::stdout().lock();
        let _ = stdout.write_all(out.as_bytes());
        let _ = stdout.flush();
    }
}

/// Whether the terminal understands the escape sequences the table needs.
/// The classic Windows console has to be asked to.
pub fn ansi_terminal() -> bool {
    use std::io::IsTerminal;
    if !std::io::stdout().is_terminal() {
        return false;
    }
    #[cfg(windows)]
    {
        use windows_sys::Win32::System::Console::{
            GetConsoleMode, GetStdHandle, SetConsoleMode, ENABLE_VIRTUAL_TERMINAL_PROCESSING, STD_OUTPUT_HANDLE,
        };
        // SAFETY: plain calls on our own standard output handle.
        unsafe {
            let handle = GetStdHandle(STD_OUTPUT_HANDLE);
            let mut mode = 0;
            // No console mode at all means a Unix-style terminal (Git Bash), which is fine.
            GetConsoleMode(handle, &mut mode) == 0 || SetConsoleMode(handle, mode | ENABLE_VIRTUAL_TERMINAL_PROCESSING) != 0
        }
    }
    #[cfg(not(windows))]
    true
}
