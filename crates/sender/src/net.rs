//! UDP link to the receiver, including zero-config discovery.

use crate::Config;
use padlink_proto::{Packet, PadState, MAX_PACKET};
use std::io;
use std::net::{Ipv4Addr, SocketAddr, UdpSocket};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

const DISCOVER_EVERY: Duration = Duration::from_secs(1);
/// A receiver that has not answered for this long is considered gone.
const RECEIVER_TTL: Duration = Duration::from_millis(3500);

pub struct Link {
    sock: UdpSocket,
    /// Where states go, and when that receiver was last heard from.
    target: Mutex<Option<(SocketAddr, Instant)>>,
}

impl Link {
    pub fn start(cfg: &Config) -> io::Result<Arc<Link>> {
        let sock = UdpSocket::bind((Ipv4Addr::UNSPECIFIED, 0))?;
        sock.set_broadcast(true)?;
        prioritise(&sock);

        let link = Arc::new(Link { sock, target: Mutex::new(None) });

        if let Some(addr) = cfg.target {
            println!("sending to {addr} (discovery disabled)");
            *link.target.lock().unwrap() = Some((addr, Instant::now()));
            return Ok(link);
        }

        println!("looking for a padlink-receiver on the LAN (UDP {})...", cfg.port);
        let port = cfg.port;

        // Announce ourselves once a second, forever. It doubles as the liveness
        // probe: a receiver that stops answering is dropped and re-discovered,
        // so the laptop can change IP, reboot or be swapped without any restart here.
        let l = link.clone();
        std::thread::spawn(move || {
            let mut buf = [0u8; MAX_PACKET];
            let probe = Packet::Discover.encode(&mut buf).to_vec();
            loop {
                let _ = l.sock.send_to(&probe, (Ipv4Addr::BROADCAST, port));
                {
                    let mut t = l.target.lock().unwrap();
                    if matches!(*t, Some((_, seen)) if seen.elapsed() > RECEIVER_TTL) {
                        println!("receiver went quiet, searching again...");
                        *t = None;
                    }
                }
                std::thread::sleep(DISCOVER_EVERY);
            }
        });

        let l = link.clone();
        std::thread::spawn(move || {
            let mut buf = [0u8; 64];
            loop {
                let Ok((n, from)) = l.sock.recv_from(&mut buf) else {
                    // e.g. ICMP "port unreachable" surfaced by the OS; not fatal.
                    std::thread::sleep(Duration::from_millis(50));
                    continue;
                };
                if Packet::decode(&buf[..n]) != Some(Packet::Offer) {
                    continue;
                }
                let mut t = l.target.lock().unwrap();
                match *t {
                    Some((addr, ref mut seen)) if addr == from => *seen = Instant::now(),
                    // Stay with the current receiver while it is alive.
                    Some(_) => {}
                    None => {
                        println!("receiver found at {from}");
                        *t = Some((from, Instant::now()));
                    }
                }
            }
        });

        Ok(link)
    }

    fn send(&self, p: Packet) {
        let target = self.target.lock().unwrap().map(|(addr, _)| addr);
        if let Some(addr) = target {
            let mut buf = [0u8; MAX_PACKET];
            let _ = self.sock.send_to(p.encode(&mut buf), addr);
        }
    }

    pub fn send_state(&self, slot: u8, seq: u16, battery: u8, pad: PadState) {
        self.send(Packet::State { slot, seq, battery, pad });
    }

    #[cfg_attr(not(target_os = "linux"), allow(dead_code))]
    pub fn send_gone(&self, slot: u8) {
        // No acknowledgement exists, so say it a few times.
        for _ in 0..3 {
            self.send(Packet::Gone { slot });
        }
    }
}

/// Marks our packets as "expedited forwarding". On Wi-Fi (WMM) that puts them in
/// the voice queue, which gets air time ahead of downloads and video streams.
#[cfg(target_os = "linux")]
fn prioritise(sock: &UdpSocket) {
    use std::os::fd::AsRawFd;
    let tos: libc::c_int = 0xb8; // DSCP EF
    unsafe {
        libc::setsockopt(
            sock.as_raw_fd(),
            libc::IPPROTO_IP,
            libc::IP_TOS,
            &tos as *const _ as *const libc::c_void,
            std::mem::size_of_val(&tos) as libc::socklen_t,
        );
    }
}

#[cfg(not(target_os = "linux"))]
fn prioritise(_: &UdpSocket) {}
