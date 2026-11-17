//! UDP link to the receiver, including zero-config discovery.

use crate::Config;
use padlink_proto::{is_handshake, Auth, Packet, PadInfo, PadState, MAX_PACKET};
use std::io;
use std::net::{Ipv4Addr, SocketAddr, UdpSocket};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

const DISCOVER_EVERY: Duration = Duration::from_secs(1);
/// A receiver that has not answered for this long is considered gone.
const RECEIVER_TTL: Duration = Duration::from_millis(3500);

#[derive(Clone, Copy)]
struct Receiver {
    addr: SocketAddr,
    last_heard: Instant,
    /// Chosen by the receiver each time it starts; part of every authentication tag.
    nonce: u64,
}

pub struct Link {
    sock: UdpSocket,
    auth: Auth,
    receiver: Mutex<Option<Receiver>>,
    /// Bumped whenever a receiver is (re)acquired or has restarted.
    epoch: AtomicU32,
    key_warned: AtomicBool,
}

impl Link {
    pub fn start(cfg: &Config) -> io::Result<Arc<Link>> {
        let sock = UdpSocket::bind((Ipv4Addr::UNSPECIFIED, 0))?;
        sock.set_broadcast(true)?;
        prioritise(&sock);

        let link = Arc::new(Link {
            sock,
            auth: cfg.auth,
            receiver: Mutex::new(None),
            epoch: AtomicU32::new(0),
            key_warned: AtomicBool::new(false),
        });

        let probe_to = match cfg.target {
            Some(addr) => {
                println!("looking for the padlink-receiver at {addr}...");
                addr
            }
            None => {
                println!("looking for a padlink-receiver on the LAN (UDP {})...", cfg.port);
                SocketAddr::from((Ipv4Addr::BROADCAST, cfg.port))
            }
        };

        // Announce ourselves once a second, forever. It doubles as the liveness
        // probe: a receiver that stops answering is dropped and re-discovered,
        // so the PC can change IP, reboot or be swapped without any restart here.
        let l = link.clone();
        std::thread::spawn(move || {
            let mut buf = [0u8; MAX_PACKET];
            let probe = l.auth.seal(0, &Packet::Discover, &mut buf).to_vec();
            loop {
                let _ = l.sock.send_to(&probe, probe_to);
                {
                    let mut r = l.receiver.lock().unwrap();
                    if r.is_some_and(|r| r.last_heard.elapsed() > RECEIVER_TTL) {
                        println!("receiver went quiet, searching again...");
                        *r = None;
                    }
                }
                std::thread::sleep(DISCOVER_EVERY);
            }
        });

        let l = link.clone();
        std::thread::spawn(move || {
            let mut buf = [0u8; MAX_PACKET + 1];
            loop {
                match l.sock.recv_from(&mut buf) {
                    Ok((n, from)) => l.on_datagram(&buf[..n], from),
                    // e.g. ICMP "port unreachable" surfaced by the OS; not fatal.
                    Err(_) => std::thread::sleep(Duration::from_millis(50)),
                }
            }
        });

        Ok(link)
    }

    fn on_datagram(&self, d: &[u8], from: SocketAddr) {
        if is_handshake(d) {
            let Some(Packet::Offer { nonce }) = self.auth.open(0, d) else {
                if Packet::decode(d).is_some() && !self.key_warned.swap(true, Ordering::Relaxed) {
                    eprintln!("a receiver at {} answered, but its --key does not match ours; ignoring it", from.ip());
                }
                return;
            };
            let mut r = self.receiver.lock().unwrap();
            match r.as_mut() {
                Some(cur) if cur.addr == from => {
                    cur.last_heard = Instant::now();
                    if cur.nonce != nonce {
                        // It restarted and has forgotten every controller.
                        cur.nonce = nonce;
                        self.epoch.fetch_add(1, Ordering::Relaxed);
                    }
                }
                // Stay with the current receiver while it is alive.
                Some(_) => {}
                None => {
                    println!("receiver found at {from}");
                    *r = Some(Receiver { addr: from, last_heard: Instant::now(), nonce });
                    self.epoch.fetch_add(1, Ordering::Relaxed);
                }
            }
            return;
        }

        let Some(cur) = *self.receiver.lock().unwrap() else { return };
        if cur.addr != from {
            return;
        }
        match self.auth.open(cur.nonce, d) {
            // Echoed straight back so the receiver can time the round trip.
            Some(Packet::Ping { t }) => self.send(Packet::Pong { t }),
            _ => {}
        }
    }

    /// Changes when a receiver has just been found. Controllers watch this to
    /// introduce themselves at once instead of at their next periodic repeat.
    pub fn epoch(&self) -> u32 {
        self.epoch.load(Ordering::Relaxed)
    }

    fn send(&self, p: Packet) {
        let receiver = *self.receiver.lock().unwrap();
        if let Some(r) = receiver {
            let mut buf = [0u8; MAX_PACKET];
            let _ = self.sock.send_to(self.auth.seal(r.nonce, &p, &mut buf), r.addr);
        }
    }

    pub fn send_state(&self, slot: u8, seq: u16, battery: u8, pad: PadState) {
        self.send(Packet::State { slot, seq, battery, pad });
    }

    /// Tells the receiver who the controller in `slot` is. Repeated every second,
    /// so a receiver that starts later (or restarts) learns it too.
    pub fn send_info(&self, slot: u8, info: PadInfo) {
        self.send(Packet::Info { slot, info });
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
