//! padlink-receiver: runs on the gaming PC. Answers discovery broadcasts and
//! turns each streamed controller into a virtual Xbox 360 or DualShock 4 pad.
//!
//! Controllers are told apart by name and keep their identity across reconnects.

mod engine;
mod vpad;

use engine::{Config, Engine};
use padlink_proto::DEFAULT_PORT;
use std::net::{Ipv4Addr, UdpSocket};
use vpad::{Backend, Kind};

const HELP: &str = "\
padlink-receiver - virtual controllers fed by padlink-sender over the LAN

USAGE: padlink-receiver [OPTIONS]

  --pad <x360|ds4>   type of virtual controller to create (default x360;
                     Windows supports at most 4 Xbox 360 pads at once)
  --port <N>         UDP port (default 9977)
  --deadzone <0-0.9> stick dead zone as a fraction of travel (default 0:
                     games already apply their own)
  --hold <seconds>   how long a disconnected controller keeps its virtual pad
                     and player number, waiting for it to return (default 120)
  --dry-run          do not create virtual controllers, just print what arrives
  -v, --verbose      print per-controller packet statistics every 5 s
  -h, --help         this text";

fn parse_args() -> Result<Config, String> {
    let mut cfg = Config { kind: Kind::X360, port: DEFAULT_PORT, deadzone: 0.0, hold: std::time::Duration::from_secs(120), dry_run: false, verbose: false };
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
            "--hold" => {
                cfg.hold = std::time::Duration::from_secs(value("--hold")?.parse().map_err(|_| "invalid --hold")?)
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
    println!(
        "listening on UDP {} - senders on the LAN will find this PC by themselves ({} pads{})",
        cfg.port,
        cfg.kind.label(),
        if cfg.dry_run { ", dry run" } else { "" }
    );
    Engine::new(cfg, backend, sock).run()
}
