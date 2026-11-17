//! Controller discovery and reading through Linux evdev.
//!
//! One thread per controller blocks in `poll()` on the device node, so a state
//! leaves for the network the moment the kernel delivers it. Nothing is polled
//! on a timer and nothing is queued.

use crate::net::Link;
use crate::{Config, INFO_EVERY, KEEPALIVE};
use evdev::{AbsoluteAxisType, Device, EventType, FFEffect, FFEffectData, FFEffectKind, FFEffectType, FFReplay, Key};
use padlink_proto::{
    btn, fnv1a64, scale_stick, scale_trigger, Name, PadInfo, PadState, BATTERY_UNKNOWN, BUS_BLUETOOTH, BUS_UNKNOWN,
    BUS_USB, MAX_SLOTS,
};
use std::collections::HashSet;
use std::os::fd::AsRawFd;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

const RESCAN_EVERY: Duration = Duration::from_secs(1);
const BATTERY_EVERY: Duration = Duration::from_secs(30);

// Linux input event codes (input-event-codes.h).
const BTN_SOUTH: u16 = 0x130;
const BTN_EAST: u16 = 0x131;
const BTN_NORTH: u16 = 0x133;
const BTN_WEST: u16 = 0x134;
const BTN_TL: u16 = 0x136;
const BTN_TR: u16 = 0x137;
const BTN_TL2: u16 = 0x138;
const BTN_TR2: u16 = 0x139;
const BTN_SELECT: u16 = 0x13a;
const BTN_START: u16 = 0x13b;
const BTN_MODE: u16 = 0x13c;
const BTN_THUMBL: u16 = 0x13d;
const BTN_THUMBR: u16 = 0x13e;
const BTN_DPAD_UP: u16 = 0x220;
const BTN_DPAD_DOWN: u16 = 0x221;
const BTN_DPAD_LEFT: u16 = 0x222;
const BTN_DPAD_RIGHT: u16 = 0x223;

const ABS_X: u16 = 0x00;
const ABS_Y: u16 = 0x01;
const ABS_Z: u16 = 0x02;
const ABS_RX: u16 = 0x03;
const ABS_RY: u16 = 0x04;
const ABS_RZ: u16 = 0x05;
const ABS_GAS: u16 = 0x09;
const ABS_BRAKE: u16 = 0x0a;
const ABS_HAT0X: u16 = 0x10;
const ABS_HAT0Y: u16 = 0x11;
const ABS_COUNT: usize = 64;

#[derive(Default)]
struct Registry {
    slots: [bool; MAX_SLOTS],
    /// Device nodes already examined (streaming, or known not to be a gamepad).
    seen: HashSet<PathBuf>,
}

pub fn run(cfg: &Config, link: Arc<Link>) -> ! {
    let reg = Arc::new(Mutex::new(Registry::default()));
    println!("watching /dev/input for controllers...");
    loop {
        scan(cfg, &link, &reg);
        std::thread::sleep(RESCAN_EVERY);
    }
}

fn scan(cfg: &Config, link: &Arc<Link>, reg: &Arc<Mutex<Registry>>) {
    let present: HashSet<PathBuf> = match std::fs::read_dir("/dev/input") {
        Ok(dir) => dir
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.file_name().and_then(|n| n.to_str()).is_some_and(|n| n.starts_with("event")))
            .collect(),
        Err(e) => {
            eprintln!("cannot list /dev/input: {e}");
            return;
        }
    };

    let fresh: Vec<PathBuf> = {
        let mut r = reg.lock().unwrap();
        r.seen.retain(|p| present.contains(p));
        present.into_iter().filter(|p| !r.seen.contains(p)).collect()
    };

    for path in fresh {
        let dev = match Device::open(&path) {
            Ok(d) => d,
            Err(e) => {
                // Often a permissions race right after hot-plug; retried next scan.
                if cfg.verbose {
                    eprintln!("{}: {e}", path.display());
                }
                continue;
            }
        };
        let mut r = reg.lock().unwrap();
        r.seen.insert(path.clone());
        // Every real gamepad has a south face button; keyboards, mice and the
        // DualShock's separate touchpad/motion nodes do not.
        if !dev.supported_keys().is_some_and(|k| k.contains(Key::new(BTN_SOUTH))) {
            if cfg.verbose {
                println!("{}: {} - not a gamepad, ignored", path.display(), dev.name().unwrap_or("?"));
            }
            continue;
        }
        let Some(slot) = r.slots.iter().position(|used| !used) else {
            eprintln!("{}: all {MAX_SLOTS} slots are in use, controller ignored", path.display());
            continue;
        };
        r.slots[slot] = true;
        drop(r);

        let (link, reg, grab, swap) = (link.clone(), reg.clone(), cfg.grab, cfg.swap_xy);
        std::thread::spawn(move || {
            let name = dev.name().unwrap_or("controller").to_string();
            println!("[slot {slot}] + {name} ({})", path.display());
            let why = stream(dev, &path, slot as u8, &link, grab, swap);
            println!("[slot {slot}] - {name} ({why})");
            link.on_rumble(slot as u8, None);
            link.send_gone(slot as u8);
            let mut r = reg.lock().unwrap();
            r.slots[slot] = false;
            r.seen.remove(&path);
        });
    }
}

#[derive(Clone, Copy)]
enum Role {
    Lx,
    Ly,
    Rx,
    Ry,
    Lt,
    Rt,
    HatX,
    HatY,
}

#[derive(Clone, Copy)]
struct Axis {
    role: Role,
    min: i32,
    max: i32,
}

struct Reader {
    axes: [Option<Axis>; ABS_COUNT],
    analog_lt: bool,
    analog_rt: bool,
    swap_xy: bool,
    keys: u16,
    hat: u16,
    pad: PadState,
}

impl Reader {
    fn new(dev: &Device, path: &Path, swap_override: Option<bool>) -> std::io::Result<Reader> {
        let info = dev.get_abs_state()?;
        let has = |code: u16| dev.supported_absolute_axes().is_some_and(|a| a.contains(AbsoluteAxisType(code)));

        // Layouts seen in the wild:
        //  * xpad, hid-playstation, hid-sony, hid-nintendo: right stick on RX/RY,
        //    analogue triggers on Z/RZ.
        //  * generic HID pads: right stick on Z/RZ, triggers are plain buttons.
        //  * some Bluetooth pads: triggers on BRAKE/GAS.
        let mut roles: Vec<(u16, Role)> =
            vec![(ABS_X, Role::Lx), (ABS_Y, Role::Ly), (ABS_HAT0X, Role::HatX), (ABS_HAT0Y, Role::HatY)];
        if has(ABS_RX) && has(ABS_RY) {
            roles.extend([(ABS_RX, Role::Rx), (ABS_RY, Role::Ry), (ABS_Z, Role::Lt), (ABS_RZ, Role::Rt)]);
        } else {
            roles.extend([(ABS_Z, Role::Rx), (ABS_RZ, Role::Ry)]);
        }
        if has(ABS_BRAKE) && has(ABS_GAS) {
            roles.retain(|(_, r)| !matches!(r, Role::Lt | Role::Rt));
            roles.extend([(ABS_BRAKE, Role::Lt), (ABS_GAS, Role::Rt)]);
        }

        let mut r = Reader {
            axes: [None; ABS_COUNT],
            analog_lt: false,
            analog_rt: false,
            swap_xy: swap_override.unwrap_or_else(|| xy_swapped(dev, path)),
            keys: 0,
            hat: 0,
            pad: PadState::default(),
        };
        for (code, role) in roles {
            if !has(code) {
                continue;
            }
            let i = info[code as usize];
            r.axes[code as usize] = Some(Axis { role, min: i.minimum, max: i.maximum });
            match role {
                Role::Lt => r.analog_lt = true,
                Role::Rt => r.analog_rt = true,
                _ => {}
            }
            // Start from where the sticks and triggers are right now.
            r.abs(code, i.value);
        }
        if let Ok(down) = dev.get_key_state() {
            for key in down.iter() {
                r.key(key.code(), true);
            }
        }
        r.pad.buttons = r.keys | r.hat;
        Ok(r)
    }

    fn key(&mut self, code: u16, down: bool) {
        let bit = match code {
            BTN_SOUTH => btn::A,
            BTN_EAST => btn::B,
            BTN_NORTH => if self.swap_xy { btn::X } else { btn::Y },
            BTN_WEST => if self.swap_xy { btn::Y } else { btn::X },
            BTN_TL => btn::LB,
            BTN_TR => btn::RB,
            BTN_SELECT => btn::BACK,
            BTN_START => btn::START,
            BTN_MODE => btn::GUIDE,
            BTN_THUMBL => btn::LTHUMB,
            BTN_THUMBR => btn::RTHUMB,
            BTN_DPAD_UP => btn::DPAD_UP,
            BTN_DPAD_DOWN => btn::DPAD_DOWN,
            BTN_DPAD_LEFT => btn::DPAD_LEFT,
            BTN_DPAD_RIGHT => btn::DPAD_RIGHT,
            // Digital triggers only count when the pad has no analogue ones.
            BTN_TL2 => {
                if !self.analog_lt {
                    self.pad.lt = if down { 255 } else { 0 };
                }
                return;
            }
            BTN_TR2 => {
                if !self.analog_rt {
                    self.pad.rt = if down { 255 } else { 0 };
                }
                return;
            }
            _ => return,
        };
        if down {
            self.keys |= bit;
        } else {
            self.keys &= !bit;
        }
    }

    fn abs(&mut self, code: u16, v: i32) {
        let Some(a) = self.axes.get(code as usize).copied().flatten() else { return };
        match a.role {
            Role::Lx => self.pad.lx = scale_stick(v, a.min, a.max),
            Role::Rx => self.pad.rx = scale_stick(v, a.min, a.max),
            // evdev Y grows downwards, the wire format's grows upwards.
            Role::Ly => self.pad.ly = scale_stick(v, a.min, a.max).saturating_neg(),
            Role::Ry => self.pad.ry = scale_stick(v, a.min, a.max).saturating_neg(),
            Role::Lt => self.pad.lt = scale_trigger(v, a.min, a.max),
            Role::Rt => self.pad.rt = scale_trigger(v, a.min, a.max),
            Role::HatX => {
                self.hat &= !(btn::DPAD_LEFT | btn::DPAD_RIGHT);
                self.hat |= match v.signum() {
                    -1 => btn::DPAD_LEFT,
                    1 => btn::DPAD_RIGHT,
                    _ => 0,
                };
            }
            Role::HatY => {
                self.hat &= !(btn::DPAD_UP | btn::DPAD_DOWN);
                self.hat |= match v.signum() {
                    -1 => btn::DPAD_UP,
                    1 => btn::DPAD_DOWN,
                    _ => 0,
                };
            }
        }
    }
}

/// The kernel's xpad driver reports the physical X button as BTN_NORTH and Y as
/// BTN_WEST (a historical mislabel); Sony and Nintendo drivers use the codes
/// literally. Returns true when the two need swapping to come out right.
fn xy_swapped(dev: &Device, path: &Path) -> bool {
    let driver = path
        .file_name()
        .map(|n| Path::new("/sys/class/input").join(n).join("device/device/driver"))
        .and_then(|p| std::fs::read_link(p).ok())
        .and_then(|l| l.file_name().map(|n| n.to_string_lossy().into_owned()));
    match driver.as_deref() {
        Some("xpad") => true,
        Some("sony") | Some("playstation") | Some("nintendo") => false,
        _ => dev.input_id().vendor() == 0x045e, // Microsoft
    }
}

/// Battery percentage from sysfs, matched on the controller's MAC address
/// (how the Sony drivers name their power supply). 255 when unknown.
fn battery(uniq: &str) -> u8 {
    if uniq.is_empty() {
        return BATTERY_UNKNOWN;
    }
    let uniq = uniq.to_ascii_lowercase();
    let Ok(dir) = std::fs::read_dir("/sys/class/power_supply") else { return BATTERY_UNKNOWN };
    for entry in dir.flatten() {
        if entry.file_name().to_string_lossy().to_ascii_lowercase().contains(&uniq) {
            if let Ok(s) = std::fs::read_to_string(entry.path().join("capacity")) {
                return s.trim().parse::<u8>().map_or(BATTERY_UNKNOWN, |v| v.min(100));
            }
        }
    }
    BATTERY_UNKNOWN
}

/// Vibration for one controller. The effect lasts a second at most and the
/// receiver repeats its request while a game keeps the motors on, so they
/// can never be left running by a lost packet or a receiver that vanished.
struct Rumbler {
    effect: FFEffect,
    level: (u8, u8),
}

impl Rumbler {
    fn data(strong: u8, weak: u8) -> FFEffectData {
        FFEffectData {
            direction: 0,
            trigger: Default::default(),
            replay: FFReplay { length: 1000, delay: 0 },
            kind: FFEffectKind::Rumble { strong_magnitude: strong as u16 * 257, weak_magnitude: weak as u16 * 257 },
        }
    }

    /// `None` when the controller has no rumble motors (or no write access).
    fn new(dev: &mut Device) -> Option<Rumbler> {
        if !dev.supported_ff().is_some_and(|ff| ff.contains(FFEffectType::FF_RUMBLE)) {
            return None;
        }
        let effect = dev.upload_ff_effect(Self::data(0, 0)).ok()?;
        Some(Rumbler { effect, level: (0, 0) })
    }

    fn set(&mut self, strong: u8, weak: u8) {
        if (strong, weak) == (0, 0) {
            let _ = self.effect.stop();
        } else {
            if (strong, weak) != self.level {
                let _ = self.effect.update(Self::data(strong, weak));
            }
            // Also restarts the one-second clock when the level is unchanged.
            let _ = self.effect.play(1);
        }
        self.level = (strong, weak);
    }
}

/// Name and identity as shown on the receiver. The `uid` must survive a
/// reconnect, so that the same controller gets the same virtual pad back:
/// wireless pads are identified by their MAC address, wired ones by model plus
/// the USB port they sit in (plus this machine, in case two Pis are in use).
fn identify(dev: &Device) -> PadInfo {
    let id = dev.input_id();
    let uniq = dev.unique_name().unwrap_or("").to_ascii_lowercase();
    let uid = if uniq.chars().any(|c| c.is_ascii_hexdigit() && c != '0') {
        fnv1a64(uniq.as_bytes())
    } else {
        let machine = std::fs::read_to_string("/etc/machine-id").unwrap_or_default();
        let port = dev.physical_path().unwrap_or("");
        fnv1a64(format!("{}|{:04x}:{:04x}|{port}", machine.trim(), id.vendor(), id.product()).as_bytes())
    };
    PadInfo {
        bus: match id.bus_type().0 {
            0x03 => BUS_USB,
            0x05 => BUS_BLUETOOTH,
            _ => BUS_UNKNOWN,
        },
        vendor: id.vendor(),
        product: id.product(),
        uid,
        name: Name::new(dev.name().unwrap_or("controller").trim()),
    }
}

/// Streams one controller until it disappears. Returns the reason it stopped.
fn stream(mut dev: Device, path: &Path, slot: u8, link: &Link, grab: bool, swap: Option<bool>) -> String {
    let mut reader = match Reader::new(&dev, path, swap) {
        Ok(r) => r,
        Err(e) => return format!("cannot read axis info: {e}"),
    };
    if grab {
        if let Err(e) = dev.grab() {
            eprintln!("[slot {slot}] --grab failed: {e}");
        }
    }
    match Rumbler::new(&mut dev) {
        Some(mut r) => link.on_rumble(slot, Some(Box::new(move |strong, weak| r.set(strong, weak)))),
        None => link.on_rumble(slot, None),
    }
    let uniq = dev.unique_name().unwrap_or("").to_string();
    let mut level = battery(&uniq);
    let mut level_read = Instant::now();

    let fd = dev.as_raw_fd();
    let mut seq = 0u16;
    let mut sent = reader.pad;
    let mut last_send = Instant::now();
    let info = identify(&dev);
    let mut info_sent = Instant::now();
    let mut epoch = link.epoch();
    link.send_info(slot, info);
    link.send_state(slot, seq, level, sent);

    loop {
        let wait = KEEPALIVE.saturating_sub(last_send.elapsed());
        let mut pfd = libc::pollfd { fd, events: libc::POLLIN, revents: 0 };
        let ready = unsafe { libc::poll(&mut pfd, 1, wait.as_millis() as libc::c_int) };
        if ready < 0 {
            let e = std::io::Error::last_os_error();
            if e.kind() == std::io::ErrorKind::Interrupted {
                continue;
            }
            return format!("poll: {e}");
        }

        if ready > 0 {
            let events = match dev.fetch_events() {
                Ok(ev) => ev,
                Err(e) if e.raw_os_error() == Some(libc::ENODEV) => return "disconnected".into(),
                Err(e) => return format!("read: {e}"),
            };
            for ev in events {
                let t = ev.event_type();
                if t == EventType::KEY {
                    reader.key(ev.code(), ev.value() != 0);
                } else if t == EventType::ABSOLUTE {
                    reader.abs(ev.code(), ev.value());
                } else if t == EventType::SYNCHRONIZATION {
                    // End of one hardware report: everything in it goes out as one packet.
                    reader.pad.buttons = reader.keys | reader.hat;
                    if reader.pad != sent {
                        sent = reader.pad;
                        seq = seq.wrapping_add(1);
                        link.send_state(slot, seq, level, sent);
                        last_send = Instant::now();
                    }
                }
            }
        }

        if info_sent.elapsed() >= INFO_EVERY || epoch != link.epoch() {
            epoch = link.epoch();
            link.send_info(slot, info);
            info_sent = Instant::now();
        }
        if last_send.elapsed() >= KEEPALIVE {
            if level_read.elapsed() >= BATTERY_EVERY {
                level = battery(&uniq);
                level_read = Instant::now();
            }
            seq = seq.wrapping_add(1);
            link.send_state(slot, seq, level, sent);
            last_send = Instant::now();
        }
    }
}
