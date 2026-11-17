//! The receive loop: answers discovery, tracks which controller is which, and
//! feeds each one's state into its virtual pad.
//!
//! Single thread, no queue: a packet is decoded and handed to the driver in the
//! same loop iteration, and only the newest state per controller ever matters.

use crate::vpad::{Backend, Kind, VirtualPad};
use padlink_proto::{deadzone, seq_newer, Packet, PadInfo, PadState, BATTERY_UNKNOWN, MAX_PACKET};
use std::collections::HashMap;
use std::net::{SocketAddr, UdpSocket};
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
    pub kind: Kind,
    pub port: u16,
    pub deadzone: f32,
    /// How long a disconnected controller keeps its virtual pad and player number.
    pub hold: Duration,
    pub dry_run: bool,
    pub verbose: bool,
}

/// Where a controller's packets come from: the sender's address and the slot
/// number it uses there. Two Pis can both have a slot 0.
type Route = (SocketAddr, u8);

/// One physical controller and the virtual pad that stands in for it.
struct Seat {
    info: PadInfo,
    /// 1-based, the lowest number that was free when the controller first appeared.
    player: u8,
    /// `None` while the controller is away and its place is being held.
    route: Option<Route>,
    away_since: Option<Instant>,
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
    fn apply(&mut self) {
        let Some(dev) = self.device.as_mut() else { return };
        match dev.update(&self.applied) {
            Ok(()) => (self.dirty, self.failures) = (false, 0),
            Err(e) => {
                // The first few updates after plug-in can bounce while the OS is still
                // bringing the device up; the next packet (at most 100 ms away) retries.
                self.dirty = true;
                self.failures += 1;
                if self.failures == 20 {
                    eprintln!("{}: driver keeps rejecting updates: {e}", self.info.name.as_str());
                }
            }
        }
    }

    fn release(&mut self) {
        self.neutral = true;
        self.applied = PadState::default();
        self.apply();
    }
}

pub struct Engine {
    cfg: Config,
    backend: Backend,
    sock: UdpSocket,
    /// Keyed by the controller's `uid`.
    seats: HashMap<u64, Seat>,
    routes: HashMap<Route, u64>,
    last_tick: Instant,
    started: Instant,
    last_stats: Instant,
}

impl Engine {
    pub fn new(cfg: Config, backend: Backend, sock: UdpSocket) -> Engine {
        sock.set_read_timeout(Some(TICK)).expect("set_read_timeout");
        Engine {
            cfg,
            backend,
            sock,
            seats: HashMap::new(),
            routes: HashMap::new(),
            last_tick: Instant::now(),
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
                if let Some(p) = Packet::decode(&buf[..n]) {
                    self.on_packet(p, from);
                }
            }
            if self.last_tick.elapsed() >= TICK {
                self.last_tick = Instant::now();
                self.tick();
            }
        }
    }

    fn log(&self, msg: String) {
        println!("[{:7.2}] {msg}", self.started.elapsed().as_secs_f32());
    }

    fn send(&self, p: Packet, to: SocketAddr) {
        let mut buf = [0u8; MAX_PACKET];
        let _ = self.sock.send_to(p.encode(&mut buf), to);
    }

    fn on_packet(&mut self, p: Packet, from: SocketAddr) {
        match p {
            Packet::Discover => self.send(Packet::Offer { nonce: 0 }, from),
            Packet::Info { slot, info } => self.on_info((from, slot), info),
            Packet::State { slot, seq, battery, pad } => self.on_state((from, slot), seq, battery, pad),
            Packet::Gone { slot } => {
                if let Some(uid) = self.routes.remove(&(from, slot)) {
                    self.disconnect(uid, "disconnected");
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
        let kind = self.cfg.kind;
        let device = match self.backend.plug(kind) {
            Ok(d) => {
                self.log(format!(
                    "+ {} is player {player} ({}, from {}) -> virtual {} pad",
                    info.name.as_str(),
                    info.bus_label(),
                    route.0.ip(),
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
            seat.battery = battery;
            if battery != BATTERY_UNKNOWN {
                println!("  {}: battery {battery}%", seat.info.name.as_str());
            }
        }

        (pad.lx, pad.ly) = deadzone(pad.lx, pad.ly, self.cfg.deadzone);
        (pad.rx, pad.ry) = deadzone(pad.rx, pad.ry, self.cfg.deadzone);
        // Keepalives repeat the same state; skip the driver call for those.
        if pad != seat.applied || seat.dirty {
            seat.applied = pad;
            seat.apply();
        }
    }

    /// The controller left. Its virtual pad stays plugged in (idle) for the hold
    /// time, so that when it comes back - after a battery swap, a Bluetooth
    /// hiccup, a Pi reboot - it is the same player in the game as before.
    fn disconnect(&mut self, uid: u64, why: &str) {
        let Some(seat) = self.seats.get_mut(&uid) else { return };
        if let Some(route) = seat.route.take() {
            if self.routes.get(&route) == Some(&uid) {
                self.routes.remove(&route);
            }
        }
        if seat.away_since.is_some() {
            return;
        }
        seat.release();
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
                seat.release();
                println!("  {}: link went quiet, inputs released", seat.info.name.as_str());
            }
        }
        for uid in lost {
            self.disconnect(uid, "timed out");
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
                println!(
                    "  {}: {:.0} packets/s, {} lost, {} out of order",
                    seat.info.name.as_str(),
                    seat.received as f32 / secs,
                    seat.lost,
                    seat.stale
                );
                (seat.received, seat.lost, seat.stale) = (0, 0, 0);
            }
        }
    }
}
