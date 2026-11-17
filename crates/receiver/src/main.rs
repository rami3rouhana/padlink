//! padlink-receiver: runs on the gaming PC. Answers discovery broadcasts and
//! turns each streamed controller into a virtual Xbox 360 or DualShock 4 pad.
//!
//! Single thread, no queue: a packet is decoded and handed to the driver in the
//! same loop iteration, and only the newest state per controller ever matters.

mod vpad;

use padlink_proto::{deadzone, seq_newer, Packet, PadState, BATTERY_UNKNOWN, DEFAULT_PORT};
use std::collections::HashMap;
use std::net::{IpAddr, Ipv4Addr, UdpSocket};
use std::time::{Duration, Instant};
use vpad::{Backend, Kind, VirtualPad};

/// Silence after which a pad's inputs are released, so a dropped link can never
/// leave a character running into a wall. The sender repeats itself every 100 ms.
const NEUTRAL_AFTER: Duration = Duration::from_millis(350);
/// Silence after which the virtual pad is unplugged.
const UNPLUG_AFTER: Duration = Duration::from_secs(5);
/// A sender that restarts begins counting from zero again; after this much
/// silence its packets are accepted regardless of sequence number.
const SEQ_RESET_AFTER: Duration = Duration::from_millis(500);
const TICK: Duration = Duration::from_millis(50);

const HELP: &str = "\
padlink-receiver - virtual controllers fed by padlink-sender over the LAN

USAGE: padlink-receiver [OPTIONS]

  --pad <x360|ds4>   type of virtual controller to create (default x360;
                     Windows supports at most 4 Xbox 360 pads at once)
  --port <N>         UDP port (default 9977)
  --deadzone <0-0.9> stick dead zone as a fraction of travel (default 0:
                     games already apply their own)
  --dry-run          do not create virtual controllers, just print what arrives
  -v, --verbose      print per-controller packet statistics every 5 s
  -h, --help         this text";

struct Config {
    kind: Kind,
    port: u16,
    deadzone: f32,
    dry_run: bool,
    verbose: bool,
}

fn parse_args() -> Result<Config, String> {
    let mut cfg = Config { kind: Kind::X360, port: DEFAULT_PORT, deadzone: 0.0, dry_run: false, verbose: false };
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        let mut value = |name: &str| args.next().ok_or(format!("{name} needs a value"));
        match a.as_str() {
            "--pad" => {
                cfg.kind = match value("--pad")?.as_str() {
                    "x360" | "xbox" => Kind::X360,
                    "ds4" | "ps4" => Kind::Ds4,
                    _ => return Err("--pad takes x360 or ds4".into()),
                }
            }
            "--port" => cfg.port = value("--port")?.parse().map_err(|_| "invalid --port")?,
            "--deadzone" => {
                cfg.deadzone = value("--deadzone")?.parse().map_err(|_| "invalid --deadzone")?;
                if !(0.0..=0.9).contains(&cfg.deadzone) {
                    return Err("--deadzone must be between 0 and 0.9".into());
                }
            }
            "--dry-run" => cfg.dry_run = true,
            "-v" | "--verbose" => cfg.verbose = true,
            "-h" | "--help" => {
                println!("{HELP}");
                std::process::exit(0);
            }
            other => return Err(format!("unknown option {other} (try --help)")),
        }
    }
    Ok(cfg)
}

struct Pad {
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

fn main() {
    let cfg = match parse_args() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("error: {e}");
            std::process::exit(2);
        }
    };
    let backend = match Backend::open(cfg.dry_run) {
        Ok(b) => b,
        Err(e) => {
            eprintln!("error: {e}");
            std::process::exit(1);
        }
    };
    let sock = match UdpSocket::bind((Ipv4Addr::UNSPECIFIED, cfg.port)) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("error: cannot listen on UDP port {}: {e}", cfg.port);
            std::process::exit(1);
        }
    };
    sock.set_read_timeout(Some(TICK)).expect("set_read_timeout");
    println!(
        "listening on UDP {} - senders on the LAN will find this PC by themselves ({} pads{})",
        cfg.port,
        cfg.kind.label(),
        if cfg.dry_run { ", dry run" } else { "" }
    );

    let mut pads: HashMap<(IpAddr, u8), Pad> = HashMap::new();
    let mut buf = [0u8; 64];
    let mut offer_buf = [0u8; padlink_proto::MAX_PACKET];
    let offer = Packet::Offer.encode(&mut offer_buf).to_vec();
    let mut last_tick = Instant::now();
    let mut last_stats = Instant::now();

    loop {
        // Errors here are read timeouts, or Windows reporting an ICMP
        // "port unreachable" from an earlier reply. Neither is fatal.
        if let Ok((n, from)) = sock.recv_from(&mut buf) {
            match Packet::decode(&buf[..n]) {
                Some(Packet::Discover) => {
                    let _ = sock.send_to(&offer, from);
                }
                Some(Packet::State { slot, seq, battery, pad }) => {
                    let key = (from.ip(), slot);
                    let entry = pads.entry(key).or_insert_with(|| {
                        let device = match backend.plug(cfg.kind) {
                            Ok(d) => {
                                println!("[{} #{slot}] connected -> virtual {} pad", from.ip(), cfg.kind.label());
                                Some(d)
                            }
                            Err(e) => {
                                eprintln!("[{} #{slot}] cannot create virtual pad: {e}", from.ip());
                                None
                            }
                        };
                        Pad {
                            device,
                            last_seq: seq.wrapping_sub(1),
                            last_rx: Instant::now(),
                            applied: PadState::default(),
                            dirty: false,
                            failures: 0,
                            neutral: false,
                            battery: BATTERY_UNKNOWN,
                            received: 0,
                            lost: 0,
                            stale: 0,
                        }
                    });

                    if !seq_newer(seq, entry.last_seq) && entry.last_rx.elapsed() < SEQ_RESET_AFTER {
                        // Reordered or duplicated on the way; a newer state is already applied.
                        entry.stale += 1;
                        continue;
                    }
                    entry.received += 1;
                    entry.lost += seq.wrapping_sub(entry.last_seq).wrapping_sub(1).min(1000) as u32;
                    entry.last_seq = seq;
                    entry.last_rx = Instant::now();
                    entry.neutral = false;
                    if battery != entry.battery {
                        entry.battery = battery;
                        if battery != BATTERY_UNKNOWN {
                            println!("[{} #{slot}] battery {battery}%", from.ip());
                        }
                    }

                    let mut pad = pad;
                    (pad.lx, pad.ly) = deadzone(pad.lx, pad.ly, cfg.deadzone);
                    (pad.rx, pad.ry) = deadzone(pad.rx, pad.ry, cfg.deadzone);
                    // Keepalives repeat the same state; skip the driver call for those.
                    if pad != entry.applied || entry.dirty {
                        entry.applied = pad;
                        apply(entry, key);
                    }
                }
                Some(Packet::Gone { slot }) => {
                    if pads.remove(&(from.ip(), slot)).is_some() {
                        println!("[{} #{slot}] disconnected", from.ip());
                    }
                }
                Some(Packet::Offer) | None => {}
            }
        }

        if last_tick.elapsed() >= TICK {
            last_tick = Instant::now();
            pads.retain(|key, p| {
                let idle = p.last_rx.elapsed();
                if idle > UNPLUG_AFTER {
                    println!("[{} #{}] timed out, unplugged", key.0, key.1);
                    return false;
                }
                if idle > NEUTRAL_AFTER && !p.neutral {
                    p.neutral = true;
                    p.applied = PadState::default();
                    apply(p, *key);
                    println!("[{} #{}] link went quiet, inputs released", key.0, key.1);
                }
                true
            });
        }

        if cfg.verbose && last_stats.elapsed() >= Duration::from_secs(5) {
            let secs = last_stats.elapsed().as_secs_f32();
            last_stats = Instant::now();
            for (key, p) in pads.iter_mut() {
                println!(
                    "[{} #{}] {:.0} packets/s, {} lost, {} out of order",
                    key.0,
                    key.1,
                    p.received as f32 / secs,
                    p.lost,
                    p.stale
                );
                (p.received, p.lost, p.stale) = (0, 0, 0);
            }
        }
    }
}

fn apply(p: &mut Pad, key: (IpAddr, u8)) {
    let Some(dev) = p.device.as_mut() else { return };
    match dev.update(&p.applied) {
        Ok(()) => (p.dirty, p.failures) = (false, 0),
        Err(e) => {
            // The first few updates after plug-in can bounce while Windows is still
            // bringing the device up; the next packet (at most 100 ms away) retries.
            p.dirty = true;
            p.failures += 1;
            if p.failures == 20 {
                eprintln!("[{} #{}] driver keeps rejecting updates: {e}", key.0, key.1);
            }
        }
    }
}
