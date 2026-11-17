//! padlink-receiver: runs on the gaming PC. Answers discovery broadcasts and
//! turns each streamed controller into a virtual Xbox 360 or DualShock 4 pad.
//!
//! Controllers are told apart by name and keep their identity across reconnects.

mod config;
mod engine;
mod status;
mod vpad;

use engine::{Config, Engine};
use padlink_proto::{Auth, DEFAULT_PORT};
use std::net::{Ipv4Addr, UdpSocket};
use std::path::PathBuf;
use status::Status;
use std::time::Duration;
use vpad::{Backend, Kind};

const HELP: &str = "\
padlink-receiver - virtual controllers fed by padlink-sender over the LAN

USAGE: padlink-receiver [OPTIONS]

Every option can also be set in the settings file, which additionally holds
per-controller button remapping. Options given here win.

  --pad <auto|x360|ds4>  type of virtual controller to create. auto (default):
                     PlayStation controllers become DualShock 4 pads, all
                     others Xbox 360 pads (and DS4 beyond the 4 XInput allows)
  --port <N>         UDP port (default 9977)
  --deadzone <0-0.9> stick dead zone as a fraction of travel (default 0:
                     games already apply their own)
  --hold <seconds>   how long a disconnected controller keeps its virtual pad
                     and player number, waiting for it to return (default 120)
  --key <passphrase> only accept senders started with the same --key
  --config <file>    settings file to use instead of the default one
  --view <auto|table|log>  what to show. auto (default): a live table of the
                     controllers in a terminal, log lines when piped to a file
  --dry-run          do not create virtual controllers, just print what arrives
  -v, --verbose      print per-controller packet statistics every 5 s
  -h, --help         this text";

/// Settings file first, then the command line on top of it.
fn configure() -> Result<(Config, Option<bool>, Option<PathBuf>), String> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.iter().any(|a| a == "-h" || a == "--help") {
        println!("{HELP}");
        if let Some(p) = config::default_path() {
            println!("\nSettings file: {}", p.display());
        }
        std::process::exit(0);
    }
    let explicit = match args.iter().position(|a| a == "--config") {
        Some(i) => Some(PathBuf::from(args.get(i + 1).ok_or("--config needs a value")?)),
        None => None,
    };
    let (file, path) = config::load(explicit)?;

    let mut cfg = Config {
        pad: file.pad.unwrap_or(None),
        port: file.port.unwrap_or(DEFAULT_PORT),
        deadzone: file.deadzone.unwrap_or(0.0),
        hold: Duration::from_secs(file.hold.unwrap_or(120)),
        auth: file.key.as_deref().map_or(Auth::none(), Auth::from_passphrase),
        rules: file.rules,
        dry_run: false,
        verbose: false,
    };

    let mut table = None;
    let mut args = args.into_iter();
    while let Some(a) = args.next() {
        let mut value = |name: &str| args.next().ok_or(format!("{name} needs a value"));
        match a.as_str() {
            "--pad" => cfg.pad = config::parse_pad(&value("--pad")?).map_err(|e| format!("--{e}"))?,
            "--port" => cfg.port = value("--port")?.parse().map_err(|_| "invalid --port")?,
            "--deadzone" => cfg.deadzone = config::parse_deadzone(&value("--deadzone")?).map_err(|e| format!("--{e}"))?,
            "--hold" => cfg.hold = Duration::from_secs(value("--hold")?.parse().map_err(|_| "invalid --hold")?),
            "--key" => cfg.auth = Auth::from_passphrase(&value("--key")?),
            "--config" => drop(value("--config")?),
            "--view" => {
                table = match value("--view")?.as_str() {
                    "auto" => None,
                    "table" => Some(true),
                    "log" => Some(false),
                    _ => return Err("--view must be auto, table or log".into()),
                }
            }
            "--dry-run" => cfg.dry_run = true,
            "-v" | "--verbose" => cfg.verbose = true,
            other => return Err(format!("unknown option {other} (try --help)")),
        }
    }
    Ok((cfg, table, path))
}

fn main() {
    let (cfg, table, _settings_path) = match configure() {
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
    let title = format!(
        "padlink-receiver, listening on UDP {} - senders on the LAN find this PC by themselves ({} pads{}{})",
        cfg.port,
        cfg.pad.map_or("automatic", Kind::label),
        if cfg.auth.enabled() { ", key set" } else { "" },
        if cfg.dry_run { ", dry run" } else { "" }
    );
    // Statistics and dry-run output are streams of lines; a table would fight them.
    let table = table.unwrap_or_else(|| !cfg.verbose && !cfg.dry_run && status::ansi_terminal());
    let status = Status::new(title, table);
    Engine::new(cfg, backend, sock, status).run()
}
