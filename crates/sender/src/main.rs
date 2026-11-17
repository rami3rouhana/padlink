//! padlink-sender: runs on the machine the controllers are connected to
//! (the Raspberry Pi running Batocera) and streams their state to the receiver.
//!
//! No address needs configuring: the sender broadcasts a discovery packet on the
//! LAN, the receiver answers, and from then on states go straight to it.

mod net;
#[cfg(target_os = "linux")]
mod pads;

use std::time::Duration;

/// How often an unchanged state is repeated. This heals lost packets and tells
/// the receiver the controller is still alive.
pub const KEEPALIVE: Duration = Duration::from_millis(100);
/// How often a controller's name and identity are repeated.
pub const INFO_EVERY: Duration = Duration::from_secs(1);

pub struct Config {
    pub port: u16,
    pub target: Option<std::net::SocketAddr>,
    pub auth: padlink_proto::Auth,
    pub grab: bool,
    pub swap_xy: Option<bool>,
    pub test_pattern: Option<u32>,
    pub verbose: bool,
}

const HELP: &str = "\
padlink-sender - stream game controllers to a padlink-receiver on the LAN

USAGE: padlink-sender [OPTIONS]

  --port <N>         UDP port (default 9977)
  --target <IP>      look for the receiver at this address instead of
                     broadcasting (for networks that block broadcast)
  --key <passphrase> shared key; must match the receiver's --key
  --grab             take exclusive control of the controllers, so Batocera
                     itself does not react to them while they are streamed
  --swap-xy <on|off> force the X/Y (square/triangle) swap instead of auto-detecting
  --test-pattern[=N] stream a synthetic controller that circles its left stick
                     and taps A (works on any OS, no controller needed).
                     Give each extra instance its own number N
  -v, --verbose      log every device that is looked at
  -h, --help         this text";

fn parse_args() -> Result<Config, String> {
    let mut cfg = Config {
        port: padlink_proto::DEFAULT_PORT,
        target: None,
        auth: padlink_proto::Auth::none(),
        grab: false,
        swap_xy: None,
        test_pattern: None,
        verbose: false,
    };
    let mut target_host: Option<String> = None;
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        let mut value = |name: &str| args.next().ok_or(format!("{name} needs a value"));
        match a.as_str() {
            "--port" => cfg.port = value("--port")?.parse().map_err(|_| "invalid --port")?,
            "--target" => target_host = Some(value("--target")?),
            "--key" => cfg.auth = padlink_proto::Auth::from_passphrase(&value("--key")?),
            "--grab" => cfg.grab = true,
            "--swap-xy" => {
                cfg.swap_xy = Some(match value("--swap-xy")?.as_str() {
                    "on" => true,
                    "off" => false,
                    _ => return Err("--swap-xy takes on or off".into()),
                })
            }
            "--test-pattern" => cfg.test_pattern = Some(1),
            t if t.starts_with("--test-pattern=") => {
                cfg.test_pattern = Some(t[15..].parse().map_err(|_| "--test-pattern=N needs a number")?)
            }
            "-v" | "--verbose" => cfg.verbose = true,
            "-h" | "--help" => {
                println!("{HELP}");
                std::process::exit(0);
            }
            other => return Err(format!("unknown option {other} (try --help)")),
        }
    }
    if let Some(host) = target_host {
        use std::net::ToSocketAddrs;
        let with_port = if host.parse::<std::net::IpAddr>().is_ok() || !host.contains(':') {
            format!("{host}:{}", cfg.port)
        } else {
            host
        };
        cfg.target = Some(
            with_port
                .to_socket_addrs()
                .ok()
                .and_then(|mut a| a.next())
                .ok_or(format!("cannot resolve --target {with_port}"))?,
        );
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
    let link = match net::Link::start(&cfg) {
        Ok(l) => l,
        Err(e) => {
            eprintln!("error: cannot open UDP socket: {e}");
            std::process::exit(1);
        }
    };

    if let Some(n) = cfg.test_pattern {
        println!("streaming synthetic controller {n} (Ctrl+C to stop)");
        test_pattern(&link, n);
    }

    #[cfg(target_os = "linux")]
    pads::run(&cfg, link);

    #[cfg(not(target_os = "linux"))]
    {
        let _ = link;
        eprintln!("error: reading real controllers needs Linux; on this OS only --test-pattern works");
        std::process::exit(1);
    }
}

/// Left stick draws a circle once per second, A is held for the first 200 ms of
/// every second. Handy for checking the whole chain without a controller.
fn test_pattern(link: &net::Link, n: u32) -> ! {
    use padlink_proto::{btn, fnv1a64, Name, PadInfo, PadState, BATTERY_UNKNOWN, BUS_UNKNOWN};
    // Instances 50 and up pose as a PlayStation controller.
    let (name, vendor, product) = if n >= 50 {
        (format!("Test pattern {n} (DualSense)"), 0x054c, 0x0ce6)
    } else {
        (format!("Test pattern {n}"), 0, 0)
    };
    let info = PadInfo { bus: BUS_UNKNOWN, vendor, product, uid: fnv1a64(name.as_bytes()), name: Name::new(&name) };
    let start = std::time::Instant::now();
    let mut seq = 0u16;
    let mut info_sent: Option<std::time::Instant> = None;
    let mut epoch = link.epoch();
    loop {
        if info_sent.map_or(true, |t| t.elapsed() >= INFO_EVERY) || epoch != link.epoch() {
            epoch = link.epoch();
            link.send_info(0, info);
            info_sent = Some(std::time::Instant::now());
        }
        let t = start.elapsed().as_secs_f32();
        let angle = t * std::f32::consts::TAU;
        let pad = PadState {
            buttons: if t.fract() < 0.2 { btn::A } else { 0 },
            lx: (angle.cos() * 32767.0) as i16,
            ly: (angle.sin() * 32767.0) as i16,
            ..Default::default()
        };
        seq = seq.wrapping_add(1);
        link.send_state(0, seq, BATTERY_UNKNOWN, pad);
        std::thread::sleep(Duration::from_millis(4));
    }
}
