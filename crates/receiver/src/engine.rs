//! The receive loop: answers discovery, tracks which controller is which, and
//! feeds each one's state into its virtual pad.
//!
//! Single thread, no queue: a packet is decoded and handed to the driver in the
//! same loop iteration, and only the newest state per controller ever matters.

use crate::config::{remap, Remap, Rule};
use crate::status::{Row, State, Status};
use crate::vpad::{Backend, Kind, VirtualPad};
use padlink_proto::{deadzone, is_handshake, seq_newer, Auth, Packet, PadInfo, PadState, BATTERY_UNKNOWN, MAX_PACKET};
use std::collections::HashMap;
use std::net::{SocketAddr, UdpSocket};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// Silence after which a pad's inputs are released, so a dropped link can never
/// leave a character running into a wall. The sender repeats itself every 100 ms.
const NEUTRAL_AFTER: Duration = Duration::from_millis(350);
/// Silence after which the controller counts as disconnected.
const LOST_AFTER: Duration = Duration::from_secs(5);
/// A sender that restarts begins counting from zero again; after this much
/// silence its packets are accepted regardless of sequence number.
const SEQ_RESET_AFTER: Duration = Duration::from_millis(500);
pub const TICK: Duration = Duration::from_millis(50);

pub struct Config {
    /// Type of virtual pad to create; `None` picks one per controller.
    pub pad: Option<Kind>,
    pub port: u16,
    pub deadzone: f32,
    /// How long a disconnected controller keeps its virtual pad and player number.
    pub hold: Duration,
    /// Shared key; packets without a valid tag are ignored when set.
    pub auth: Auth,
    /// Per-controller overrides from the settings file; the first match applies.
    pub rules: Vec<Rule>,
    pub dry_run: bool,
    pub verbose: bool,
}

/// At or below this percentage a battery is reported as low.
pub const LOW_BATTERY: u8 = 15;

/// Where a controller's packets come from: the sender's address and the slot
/// number it uses there. Two Pis can both have a slot 0.
type Route = (SocketAddr, u8);

/// Vibration on its way back to a physical controller. Shared with the driver
/// thread that reports what the game asked for, which sends it on immediately.
struct Rumble {
    sock: UdpSocket,
    auth: Auth,
    nonce: u64,
    /// Where the controller currently is, and the last requested motor levels.
    state: Mutex<(Option<Route>, u8, u8)>,
}

impl Rumble {
    fn set(&self, strong: u8, weak: u8) {
        let mut s = self.state.lock().unwrap();
        // The driver also notifies about the player LED; only motor changes matter.
        if (s.1, s.2) == (strong, weak) {
            return;
        }
        (s.1, s.2) = (strong, weak);
        self.send(*s);
    }

    fn set_route(&self, route: Option<Route>) {
        self.state.lock().unwrap().0 = route;
    }

    /// The sender stops the motors on its own unless told again within a second.
    fn repeat(&self) {
        let s = *self.state.lock().unwrap();
        if (s.1, s.2) != (0, 0) {
            self.send(s);
        }
    }

    fn send(&self, (route, strong, weak): (Option<Route>, u8, u8)) {
        if let Some((addr, slot)) = route {
            let mut buf = [0u8; MAX_PACKET];
            let _ = self.sock.send_to(self.auth.seal(self.nonce, &Packet::Rumble { slot, strong, weak }, &mut buf), addr);
        }
    }
}

const RUMBLE_EVERY: Duration = Duration::from_millis(400);

/// One physical controller and the virtual pad that stands in for it.
struct Seat {
    info: PadInfo,
    /// 1-based, the lowest number that was free when the controller first appeared.
    player: u8,
    /// `None` while the controller is away and its place is being held.
    route: Option<Route>,
    away_since: Option<Instant>,
    kind: Kind,
    deadzone: f32,
    remap: Option<Remap>,
    rumble: Arc<Rumble>,
    /// `None` when the driver refused to create it (e.g. a fifth Xbox pad).
    device: Option<Box<dyn VirtualPad>>,
    last_seq: u16,
    last_rx: Instant,
    applied: PadState,
    /// The driver has not accepted `applied` yet; retry on the next packet.
    dirty: bool,
    failures: u32,
    neutral: bool,
    battery: u8,
    received: u32,
    lost: u32,
    stale: u32,
}

impl Seat {
    fn apply(&mut self, status: &Status) {
        let Some(dev) = self.device.as_mut() else { return };
        match dev.update(&self.applied) {
            Ok(()) => (self.dirty, self.failures) = (false, 0),
            Err(e) => {
                // The first few updates after plug-in can bounce while the OS is still
                // bringing the device up; the next packet (at most 100 ms away) retries.
                self.dirty = true;
                self.failures += 1;
                if self.failures == 20 {
                    status.event(format!("! {}: the driver keeps rejecting updates: {e}", self.info.name.as_str()));
                }
            }
        }
    }

    fn release(&mut self, status: &Status) {
        self.neutral = true;
        self.applied = PadState::default();
        self.apply(status);
    }
}

/// A machine running padlink-sender. Several can feed one receiver.
struct Sender {
    /// 1-based, in order of appearance; shown when more than one is around.
    number: u8,
    last_heard: Instant,
    /// Network round trip to this sender in microseconds, smoothed.
    rtt_us: Option<u32>,
}

const PING_EVERY: Duration = Duration::from_secs(1);

/// A sender that has been silent this long is forgotten.
const SENDER_GONE_AFTER: Duration = Duration::from_secs(10);

fn random_u64() -> u64 {
    use std::hash::{BuildHasher, Hasher};
    // Seeded by the OS for every `RandomState`.
    std::collections::hash_map::RandomState::new().build_hasher().finish()
}

/// Formats microseconds as milliseconds, e.g. "0.4 ms".
pub fn ms(us: u32) -> String {
    if us < 10_000 {
        format!("{:.1} ms", us as f32 / 1000.0)
    } else {
        format!("{} ms", us / 1000)
    }
}

pub struct Engine {
    cfg: Config,
    backend: Backend,
    sock: UdpSocket,
    status: Status,
    /// Keyed by the controller's `uid`.
    seats: HashMap<u64, Seat>,
    routes: HashMap<Route, u64>,
    senders: HashMap<SocketAddr, Sender>,
    /// Random per run; senders must mix it into their authentication tags.
    nonce: u64,
    /// Addresses already reported for using a wrong key.
    rejected: Vec<std::net::IpAddr>,
    last_tick: Instant,
    last_ping: Instant,
    last_rumble: Instant,
    started: Instant,
    last_stats: Instant,
}

impl Engine {
    pub fn new(cfg: Config, backend: Backend, sock: UdpSocket, status: Status) -> Engine {
        sock.set_read_timeout(Some(TICK)).expect("set_read_timeout");
        Engine {
            cfg,
            backend,
            sock,
            status,
            seats: HashMap::new(),
            routes: HashMap::new(),
            senders: HashMap::new(),
            nonce: random_u64(),
            rejected: Vec::new(),
            last_tick: Instant::now(),
            last_ping: Instant::now(),
            last_rumble: Instant::now(),
            started: Instant::now(),
            last_stats: Instant::now(),
        }
    }

    pub fn run(mut self) -> ! {
        let mut buf = [0u8; MAX_PACKET + 1];
        loop {
            // Errors here are read timeouts, or Windows reporting an ICMP
            // "port unreachable" from an earlier reply. Neither is fatal.
            if let Ok((n, from)) = self.sock.recv_from(&mut buf) {
                let d = &buf[..n];
                // The discovery exchange happens before the sender knows our nonce.
                let nonce = if is_handshake(d) { 0 } else { self.nonce };
                match self.cfg.auth.open(nonce, d) {
                    Some(p) => self.on_packet(p, from),
                    None => self.on_rejected(d, from),
                }
            }
            if self.last_tick.elapsed() >= TICK {
                self.last_tick = Instant::now();
                self.tick();
            }
        }
    }

    fn log(&self, msg: String) {
        self.status.event(msg);
    }

    fn send(&self, p: Packet, to: SocketAddr) {
        let mut buf = [0u8; MAX_PACKET];
        let nonce = if matches!(p, Packet::Offer { .. }) { 0 } else { self.nonce };
        let _ = self.sock.send_to(self.cfg.auth.seal(nonce, &p, &mut buf), to);
    }

    /// Says so, once per address, when something that looks like padlink
    /// traffic fails authentication - almost always a mistyped key.
    fn on_rejected(&mut self, d: &[u8], from: SocketAddr) {
        // Right after a restart, senders still sign with the previous run's nonce
        // for up to a second; only the handshake tells us the key itself is wrong.
        if !self.cfg.auth.enabled() || !is_handshake(d) || Packet::decode(d).is_none() {
            return;
        }
        if !self.rejected.contains(&from.ip()) {
            self.rejected.push(from.ip());
            self.log(format!("! ignoring the sender at {}: it uses a different key, or none", from.ip()));
        }
    }

    fn on_packet(&mut self, p: Packet, from: SocketAddr) {
        match self.senders.get_mut(&from) {
            Some(s) => s.last_heard = Instant::now(),
            None => {
                let number = (1..=u8::MAX).find(|n| self.senders.values().all(|s| s.number != *n)).unwrap_or(u8::MAX);
                self.senders.insert(from, Sender { number, last_heard: Instant::now(), rtt_us: None });
                self.log(format!("sender {number} found at {}", from.ip()));
            }
        }
        match p {
            Packet::Discover => self.send(Packet::Offer { nonce: self.nonce }, from),
            Packet::Info { slot, info } => self.on_info((from, slot), info),
            Packet::State { slot, seq, battery, pad } => self.on_state((from, slot), seq, battery, pad),
            Packet::Gone { slot } => {
                if let Some(uid) = self.routes.remove(&(from, slot)) {
                    self.disconnect(uid, "disconnected");
                }
            }
            Packet::Pong { t } => {
                let now = self.started.elapsed().as_micros() as u64;
                // Anything older than a few seconds is a stray, not a measurement.
                let Some(rtt) = now.checked_sub(t).filter(|d| *d < 5_000_000) else { return };
                if let Some(s) = self.senders.get_mut(&from) {
                    let first = s.rtt_us.is_none();
                    // Light smoothing so the readout does not flicker.
                    s.rtt_us = Some(s.rtt_us.map_or(rtt as u32, |old| (old * 3 + rtt as u32) / 4));
                    if first {
                        let msg = format!("sender {}: network round trip {}", s.number, ms(rtt as u32));
                        self.log(msg);
                    }
                }
            }
            _ => {}
        }
    }

    fn on_info(&mut self, route: Route, info: PadInfo) {
        // The slot was reused by another controller before we heard the first one leave.
        if let Some(old) = self.routes.insert(route, info.uid) {
            if old != info.uid {
                self.disconnect(old, "disconnected");
            }
        }
        if let Some(seat) = self.seats.get_mut(&info.uid) {
            if seat.route != Some(route) {
                // Same controller, new path (re-plugged, or its sender restarted).
                seat.rumble.set_route(Some(route));
                if let Some(old) = seat.route.replace(route) {
                    if self.routes.get(&old) == Some(&info.uid) {
                        self.routes.remove(&old);
                    }
                }
            }
            if seat.away_since.take().is_some() {
                seat.last_rx = Instant::now() - SEQ_RESET_AFTER;
                let msg = format!("+ {} is back as player {}", info.name.as_str(), seat.player);
                self.log(msg);
            }
            return;
        }

        let player = (1..=u8::MAX).find(|n| self.seats.values().all(|s| s.player != *n)).unwrap_or(u8::MAX);
        let rule = self.cfg.rules.iter().find(|r| r.matches(&info));
        let deadzone = rule.and_then(|r| r.deadzone).unwrap_or(self.cfg.deadzone);
        let remap = rule.and_then(|r| r.remap);
        let kind = rule.and_then(|r| r.pad).unwrap_or(self.cfg.pad).unwrap_or_else(|| self.pick_kind(&info));
        let rumble = Arc::new(Rumble {
            sock: self.sock.try_clone().expect("duplicate socket handle"),
            auth: self.cfg.auth,
            nonce: self.nonce,
            state: Mutex::new((Some(route), 0, 0)),
        });
        let from_game = rumble.clone();
        let on_rumble = Box::new(move |strong, weak| from_game.set(strong, weak));
        let device = match self.backend.plug(kind, on_rumble) {
            Ok(d) => {
                self.log(format!(
                    "+ {} is player {player} ({}, on sender {}) -> virtual {} pad",
                    info.name.as_str(),
                    info.bus_label(),
                    self.senders.get(&route.0).map_or(0, |s| s.number),
                    kind.label()
                ));
                Some(d)
            }
            Err(e) => {
                self.log(format!("! {}: cannot create a virtual pad: {e}", info.name.as_str()));
                None
            }
        };
        self.seats.insert(
            info.uid,
            Seat {
                info,
                player,
                route: Some(route),
                away_since: None,
                kind,
                deadzone,
                remap,
                rumble,
                device,
                last_seq: 0,
                // Far enough back that the first state is accepted whatever its number.
                last_rx: Instant::now() - SEQ_RESET_AFTER,
                applied: PadState::default(),
                dirty: false,
                failures: 0,
                neutral: true,
                battery: BATTERY_UNKNOWN,
                received: 0,
                lost: 0,
                stale: 0,
            },
        );
    }

    /// PlayStation controllers become DualShock 4 pads, so games show the right
    /// button glyphs; everything else becomes an Xbox 360 pad, which every game
    /// understands. XInput stops at four Xbox pads, so a fifth becomes a DS4 too.
    fn pick_kind(&self, info: &PadInfo) -> Kind {
        const SONY: u16 = 0x054c;
        let xbox_pads = self.seats.values().filter(|s| s.kind == Kind::X360 && s.device.is_some()).count();
        if info.vendor == SONY || xbox_pads >= 4 {
            Kind::Ds4
        } else {
            Kind::X360
        }
    }

    fn on_state(&mut self, route: Route, seq: u16, battery: u8, mut pad: PadState) {
        // States for a slot we have no identity for yet are dropped; the sender
        // repeats the identity every second.
        let Some(seat) = self.routes.get(&route).and_then(|uid| self.seats.get_mut(uid)) else { return };
        let quiet = seat.last_rx.elapsed() >= SEQ_RESET_AFTER;
        if !quiet && !seq_newer(seq, seat.last_seq) {
            // Reordered or duplicated on the way; a newer state is already applied.
            seat.stale += 1;
            return;
        }
        seat.received += 1;
        if !quiet {
            seat.lost += seq.wrapping_sub(seat.last_seq).wrapping_sub(1) as u32;
        }
        seat.last_seq = seq;
        seat.last_rx = Instant::now();
        seat.neutral = false;
        if battery != seat.battery {
            let before = std::mem::replace(&mut seat.battery, battery);
            let low = battery <= LOW_BATTERY;
            // Say it when first known and when it runs low; the table shows the rest.
            if battery != BATTERY_UNKNOWN && (before == BATTERY_UNKNOWN || (low && before > LOW_BATTERY)) {
                let name = seat.info.name.as_str();
                self.status.event(if low {
                    format!("! {name}: battery {battery}% - charge it soon")
                } else {
                    format!("  {name}: battery {battery}%")
                });
            }
        }

        (pad.lx, pad.ly) = deadzone(pad.lx, pad.ly, seat.deadzone);
        (pad.rx, pad.ry) = deadzone(pad.rx, pad.ry, seat.deadzone);
        if let Some(table) = &seat.remap {
            pad.buttons = remap(pad.buttons, table);
        }
        // Keepalives repeat the same state; skip the driver call for those.
        if pad != seat.applied || seat.dirty {
            seat.applied = pad;
            seat.apply(&self.status);
        }
    }

    /// The controller left. Its virtual pad stays plugged in (idle) for the hold
    /// time, so that when it comes back - after a battery swap, a Bluetooth
    /// hiccup, a Pi reboot - it is the same player in the game as before.
    fn disconnect(&mut self, uid: u64, why: &str) {
        let Some(seat) = self.seats.get_mut(&uid) else { return };
        seat.rumble.set_route(None);
        if let Some(route) = seat.route.take() {
            if self.routes.get(&route) == Some(&uid) {
                self.routes.remove(&route);
            }
        }
        if seat.away_since.is_some() {
            return;
        }
        seat.release(&self.status);
        seat.away_since = Some(Instant::now());
        let (name, player) = (seat.info.name, seat.player);
        if self.cfg.hold.is_zero() {
            self.seats.remove(&uid);
            self.log(format!("- {} {why}", name.as_str()));
        } else {
            self.log(format!(
                "- {} {why} (player {player} is kept free for it for {} s)",
                name.as_str(),
                self.cfg.hold.as_secs()
            ));
        }
    }

    fn tick(&mut self) {
        if self.last_rumble.elapsed() >= RUMBLE_EVERY {
            self.last_rumble = Instant::now();
            self.seats.values().for_each(|s| s.rumble.repeat());
        }
        if self.last_ping.elapsed() >= PING_EVERY {
            self.last_ping = Instant::now();
            let t = self.started.elapsed().as_micros() as u64;
            for addr in self.senders.keys() {
                self.send(Packet::Ping { t }, *addr);
            }
        }
        let mut lost = Vec::new();
        let mut expired = Vec::new();
        for (uid, seat) in self.seats.iter_mut() {
            if let Some(since) = seat.away_since {
                if since.elapsed() > self.cfg.hold {
                    expired.push(*uid);
                }
                continue;
            }
            let idle = seat.last_rx.elapsed();
            if idle > LOST_AFTER {
                lost.push(*uid);
            } else if idle > NEUTRAL_AFTER && !seat.neutral {
                seat.release(&self.status);
                self.status.event(format!("  {}: link went quiet, inputs released", seat.info.name.as_str()));
            }
        }
        for uid in lost {
            self.disconnect(uid, "timed out");
        }
        let gone: Vec<SocketAddr> =
            self.senders.iter().filter(|(_, s)| s.last_heard.elapsed() > SENDER_GONE_AFTER).map(|(a, _)| *a).collect();
        for addr in gone {
            if let Some(s) = self.senders.remove(&addr) {
                self.log(format!("sender {} at {} went away", s.number, addr.ip()));
            }
        }
        for uid in expired {
            if let Some(seat) = self.seats.remove(&uid) {
                self.log(format!("  {} did not come back, player {} is free again", seat.info.name.as_str(), seat.player));
            }
        }

        if self.cfg.verbose && self.last_stats.elapsed() >= Duration::from_secs(5) {
            let secs = self.last_stats.elapsed().as_secs_f32();
            self.last_stats = Instant::now();
            for seat in self.seats.values_mut().filter(|s| s.away_since.is_none()) {
                let rtt = seat.route.and_then(|r| self.senders.get(&r.0)).and_then(|s| s.rtt_us);
                self.status.event(format!(
                    "  {}: {:.0} packets/s, {} lost, {} out of order, round trip {}",
                    seat.info.name.as_str(),
                    seat.received as f32 / secs,
                    seat.lost,
                    seat.stale,
                    rtt.map_or("?".into(), ms)
                ));
                (seat.received, seat.lost, seat.stale) = (0, 0, 0);
            }
        }
        self.publish();
    }

    /// Hands the current picture to whatever displays it.
    fn publish(&self) {
        let mut rows: Vec<Row> = self
            .seats
            .values()
            .map(|seat| {
                let sender = seat.route.and_then(|r| self.senders.get(&r.0));
                Row {
                    player: seat.player,
                    name: seat.info.name.as_str().to_string(),
                    bus: seat.info.bus_label(),
                    pad: seat.kind.label(),
                    sender: sender.map_or(0, |s| s.number),
                    battery: Some(seat.battery).filter(|b| *b != BATTERY_UNKNOWN),
                    rtt_us: sender.and_then(|s| s.rtt_us),
                    state: match seat.away_since {
                        Some(since) => State::Away { left_s: self.cfg.hold.saturating_sub(since.elapsed()).as_secs() },
                        None if seat.device.is_none() => State::NoPad,
                        None if seat.last_rx.elapsed() > NEUTRAL_AFTER => State::Quiet,
                        None => State::Connected,
                    },
                }
            })
            .collect();
        rows.sort_by_key(|r| r.player);
        self.status.publish(rows, self.senders.len());
    }
}
