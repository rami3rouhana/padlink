//! padlink wire format.
//!
//! Every datagram starts with a 4-byte header: `b"PL"`, protocol version, packet type.
//! All multi-byte integers are little-endian. A full controller state is 20 bytes,
//! so it always fits in a single frame and is never fragmented. When a shared key
//! is configured, every datagram ends with an 8-byte authentication tag.

pub const DEFAULT_PORT: u16 = 9977;
pub const VERSION: u8 = 2;
pub const MAX_SLOTS: usize = 16;

const MAGIC: [u8; 2] = *b"PL";
const T_DISCOVER: u8 = 1;
const T_OFFER: u8 = 2;
const T_STATE: u8 = 3;
const T_GONE: u8 = 4;
const T_INFO: u8 = 5;
const T_RUMBLE: u8 = 6;
const T_PING: u8 = 7;
const T_PONG: u8 = 8;

pub const STATE_LEN: usize = 20;
pub const NAME_MAX: usize = 40;
pub const TAG_LEN: usize = 8;
pub const MAX_PACKET: usize = 80;

/// Button bits. The layout is identical to XInput's `wButtons`, so the Xbox 360
/// path on the receiver is a straight copy.
pub mod btn {
    pub const DPAD_UP: u16 = 0x0001;
    pub const DPAD_DOWN: u16 = 0x0002;
    pub const DPAD_LEFT: u16 = 0x0004;
    pub const DPAD_RIGHT: u16 = 0x0008;
    pub const START: u16 = 0x0010;
    pub const BACK: u16 = 0x0020;
    pub const LTHUMB: u16 = 0x0040;
    pub const RTHUMB: u16 = 0x0080;
    pub const LB: u16 = 0x0100;
    pub const RB: u16 = 0x0200;
    pub const GUIDE: u16 = 0x0400;
    pub const A: u16 = 0x1000;
    pub const B: u16 = 0x2000;
    pub const X: u16 = 0x4000;
    pub const Y: u16 = 0x8000;
}

/// One controller's complete state. Sticks are full-range `i16` with +Y = up
/// (XInput convention); triggers are 0..=255.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PadState {
    pub buttons: u16,
    pub lt: u8,
    pub rt: u8,
    pub lx: i16,
    pub ly: i16,
    pub rx: i16,
    pub ry: i16,
}

/// A controller's display name, stored inline so packets stay `Copy`.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct Name {
    len: u8,
    bytes: [u8; NAME_MAX],
}

impl Name {
    /// Truncates to `NAME_MAX` bytes on a character boundary.
    pub fn new(s: &str) -> Name {
        let mut end = s.len().min(NAME_MAX);
        while !s.is_char_boundary(end) {
            end -= 1;
        }
        let mut bytes = [0u8; NAME_MAX];
        bytes[..end].copy_from_slice(&s.as_bytes()[..end]);
        Name { len: end as u8, bytes }
    }

    pub fn as_str(&self) -> &str {
        std::str::from_utf8(&self.bytes[..self.len as usize]).unwrap_or("controller")
    }
}

impl std::fmt::Debug for Name {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{:?}", self.as_str())
    }
}

pub const BUS_UNKNOWN: u8 = 0;
pub const BUS_USB: u8 = 1;
pub const BUS_BLUETOOTH: u8 = 2;

/// Who a controller is. `uid` identifies the physical controller across
/// reconnects: derived from its MAC address when it has one, otherwise from
/// its model and the USB port it is plugged into.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PadInfo {
    pub bus: u8,
    pub vendor: u16,
    pub product: u16,
    pub uid: u64,
    pub name: Name,
}

impl PadInfo {
    pub fn bus_label(&self) -> &'static str {
        match self.bus {
            BUS_USB => "USB",
            BUS_BLUETOOTH => "Bluetooth",
            _ => "-",
        }
    }
}

/// FNV-1a, used to turn a MAC address or port path into a stable `uid`.
pub fn fnv1a64(data: &[u8]) -> u64 {
    data.iter().fold(0xcbf29ce484222325, |h, &b| (h ^ b as u64).wrapping_mul(0x100000001b3))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Packet {
    /// Sender -> broadcast: "is a receiver out there?"
    Discover,
    /// Receiver -> sender: "yes, send to the address this came from."
    /// `nonce` is random per receiver run and is mixed into authentication tags.
    Offer { nonce: u64 },
    /// Sender -> receiver: full state of one controller. Sent on every change
    /// and repeated as a keepalive, so a lost packet heals itself.
    State { slot: u8, seq: u16, battery: u8, pad: PadState },
    /// Sender -> receiver: the controller in `slot` was unplugged.
    Gone { slot: u8 },
    /// Sender -> receiver: identity of the controller in `slot`. Sent before the
    /// first state and repeated every second; states for a slot the receiver
    /// has no identity for are ignored until this arrives.
    Info { slot: u8, info: PadInfo },
    /// Receiver -> sender: motor strengths for the controller in `slot`.
    /// Repeated while non-zero; the sender stops the motors if these stop coming.
    Rumble { slot: u8, strong: u8, weak: u8 },
    /// Receiver -> sender: echo this back so the round trip can be timed.
    Ping { t: u64 },
    Pong { t: u64 },
}

pub const BATTERY_UNKNOWN: u8 = 255;

impl Packet {
    /// Serialises into `buf` and returns the used prefix (no authentication tag).
    pub fn encode<'a>(&self, buf: &'a mut [u8; MAX_PACKET]) -> &'a [u8] {
        let len = self.write(buf);
        &buf[..len]
    }

    fn write(&self, buf: &mut [u8; MAX_PACKET]) -> usize {
        buf[0] = MAGIC[0];
        buf[1] = MAGIC[1];
        buf[2] = VERSION;
        match *self {
            Packet::Discover => {
                buf[3] = T_DISCOVER;
                4
            }
            Packet::Offer { nonce } => {
                buf[3] = T_OFFER;
                buf[4..12].copy_from_slice(&nonce.to_le_bytes());
                12
            }
            Packet::Gone { slot } => {
                buf[3] = T_GONE;
                buf[4] = slot;
                5
            }
            Packet::State { slot, seq, battery, pad } => {
                buf[3] = T_STATE;
                buf[4] = slot;
                buf[5] = battery;
                buf[6..8].copy_from_slice(&seq.to_le_bytes());
                buf[8..10].copy_from_slice(&pad.buttons.to_le_bytes());
                buf[10] = pad.lt;
                buf[11] = pad.rt;
                buf[12..14].copy_from_slice(&pad.lx.to_le_bytes());
                buf[14..16].copy_from_slice(&pad.ly.to_le_bytes());
                buf[16..18].copy_from_slice(&pad.rx.to_le_bytes());
                buf[18..20].copy_from_slice(&pad.ry.to_le_bytes());
                STATE_LEN
            }
            Packet::Info { slot, info } => {
                buf[3] = T_INFO;
                buf[4] = slot;
                buf[5] = info.bus;
                buf[6..8].copy_from_slice(&info.vendor.to_le_bytes());
                buf[8..10].copy_from_slice(&info.product.to_le_bytes());
                buf[10..18].copy_from_slice(&info.uid.to_le_bytes());
                let name = info.name.as_str().as_bytes();
                buf[18] = name.len() as u8;
                buf[19..19 + name.len()].copy_from_slice(name);
                19 + name.len()
            }
            Packet::Rumble { slot, strong, weak } => {
                buf[3] = T_RUMBLE;
                buf[4] = slot;
                buf[5] = strong;
                buf[6] = weak;
                7
            }
            Packet::Ping { t } | Packet::Pong { t } => {
                buf[3] = if matches!(self, Packet::Ping { .. }) { T_PING } else { T_PONG };
                buf[4..12].copy_from_slice(&t.to_le_bytes());
                12
            }
        }
    }

    /// Parses a datagram. Anything malformed, foreign or from another protocol
    /// version yields `None` and is simply ignored by the caller.
    pub fn decode(d: &[u8]) -> Option<Packet> {
        if d.len() < 4 || d[0..2] != MAGIC || d[2] != VERSION {
            return None;
        }
        let i16_at = |i: usize| i16::from_le_bytes([d[i], d[i + 1]]);
        let u64_at = |i: usize| d.get(i..i + 8).map(|b| u64::from_le_bytes(b.try_into().unwrap()));
        let slot_ok = d.len() >= 5 && (d[4] as usize) < MAX_SLOTS;
        match d[3] {
            T_DISCOVER => Some(Packet::Discover),
            T_OFFER => Some(Packet::Offer { nonce: u64_at(4)? }),
            T_PING => Some(Packet::Ping { t: u64_at(4)? }),
            T_PONG => Some(Packet::Pong { t: u64_at(4)? }),
            T_GONE if slot_ok => Some(Packet::Gone { slot: d[4] }),
            T_RUMBLE if slot_ok && d.len() >= 7 => Some(Packet::Rumble { slot: d[4], strong: d[5], weak: d[6] }),
            T_STATE if slot_ok && d.len() >= STATE_LEN => Some(Packet::State {
                slot: d[4],
                battery: d[5],
                seq: u16::from_le_bytes([d[6], d[7]]),
                pad: PadState {
                    buttons: u16::from_le_bytes([d[8], d[9]]),
                    lt: d[10],
                    rt: d[11],
                    lx: i16_at(12),
                    ly: i16_at(14),
                    rx: i16_at(16),
                    ry: i16_at(18),
                },
            }),
            T_INFO if slot_ok && d.len() >= 19 => {
                let name = d.get(19..19 + d[18] as usize)?;
                Some(Packet::Info {
                    slot: d[4],
                    info: PadInfo {
                        bus: d[5],
                        vendor: u16::from_le_bytes([d[6], d[7]]),
                        product: u16::from_le_bytes([d[8], d[9]]),
                        uid: u64_at(10)?,
                        name: Name::new(std::str::from_utf8(name).ok()?),
                    },
                })
            }
            _ => None,
        }
    }
}

/// True for the two discovery packets, which are exchanged before either side
/// knows the session nonce and are therefore authenticated with nonce 0.
pub fn is_handshake(d: &[u8]) -> bool {
    d.len() >= 4 && matches!(d[3], T_DISCOVER | T_OFFER)
}

/// Optional shared-key authentication.
///
/// With a key, each datagram carries `SipHash-2-4(key, nonce || packet)`. The
/// nonce is chosen by the receiver at start-up and learned from its `Offer`, so
/// packets recorded in one session are useless in the next. This keeps other
/// devices on the LAN from injecting input; it is not encryption, and it does
/// not stop someone replaying packets captured within the same session.
#[derive(Clone, Copy)]
pub struct Auth(Option<(u64, u64)>);

impl Auth {
    pub fn none() -> Auth {
        Auth(None)
    }

    pub fn from_passphrase(pass: &str) -> Auth {
        let k0 = siphash24(0x70616c696e6b2d30, 0x6b65792d64657269, pass.as_bytes());
        let k1 = siphash24(k0, 0x70616c696e6b2d31, pass.as_bytes());
        Auth(Some((k0, k1)))
    }

    pub fn enabled(&self) -> bool {
        self.0.is_some()
    }

    fn tag(&self, key: (u64, u64), nonce: u64, body: &[u8]) -> [u8; TAG_LEN] {
        let mut m = [0u8; 8 + MAX_PACKET];
        m[..8].copy_from_slice(&nonce.to_le_bytes());
        m[8..8 + body.len()].copy_from_slice(body);
        siphash24(key.0, key.1, &m[..8 + body.len()]).to_le_bytes()
    }

    /// Serialises `p`, appending the tag when a key is set.
    pub fn seal<'a>(&self, nonce: u64, p: &Packet, buf: &'a mut [u8; MAX_PACKET]) -> &'a [u8] {
        let mut len = p.write(buf);
        if let Some(key) = self.0 {
            let tag = self.tag(key, nonce, &buf[..len]);
            buf[len..len + TAG_LEN].copy_from_slice(&tag);
            len += TAG_LEN;
        }
        &buf[..len]
    }

    /// Verifies (when a key is set) and parses a datagram.
    pub fn open(&self, nonce: u64, d: &[u8]) -> Option<Packet> {
        let Some(key) = self.0 else { return Packet::decode(d) };
        if d.len() < 4 + TAG_LEN || d.len() > MAX_PACKET {
            return None;
        }
        let (body, tag) = d.split_at(d.len() - TAG_LEN);
        if self.tag(key, nonce, body) != tag {
            return None;
        }
        Packet::decode(body)
    }
}

/// SipHash-2-4 with a 128-bit key.
pub fn siphash24(k0: u64, k1: u64, data: &[u8]) -> u64 {
    let mut v = [
        k0 ^ 0x736f6d6570736575,
        k1 ^ 0x646f72616e646f6d,
        k0 ^ 0x6c7967656e657261,
        k1 ^ 0x7465646279746573,
    ];
    fn round(v: &mut [u64; 4]) {
        v[0] = v[0].wrapping_add(v[1]);
        v[1] = v[1].rotate_left(13) ^ v[0];
        v[0] = v[0].rotate_left(32);
        v[2] = v[2].wrapping_add(v[3]);
        v[3] = v[3].rotate_left(16) ^ v[2];
        v[0] = v[0].wrapping_add(v[3]);
        v[3] = v[3].rotate_left(21) ^ v[0];
        v[2] = v[2].wrapping_add(v[1]);
        v[1] = v[1].rotate_left(17) ^ v[2];
        v[2] = v[2].rotate_left(32);
    }
    let mut absorb = |m: u64| {
        v[3] ^= m;
        round(&mut v);
        round(&mut v);
        v[0] ^= m;
    };
    let mut chunks = data.chunks_exact(8);
    for c in &mut chunks {
        absorb(u64::from_le_bytes(c.try_into().unwrap()));
    }
    let mut last = [0u8; 8];
    let rest = chunks.remainder();
    last[..rest.len()].copy_from_slice(rest);
    last[7] = data.len() as u8;
    absorb(u64::from_le_bytes(last));
    v[2] ^= 0xff;
    for _ in 0..4 {
        round(&mut v);
    }
    v[0] ^ v[1] ^ v[2] ^ v[3]
}

/// True if `seq` is newer than `last`, treating the counter as a wrapping ring.
pub fn seq_newer(seq: u16, last: u16) -> bool {
    (seq.wrapping_sub(last) as i16) > 0
}

/// Maps a raw axis reading in `min..=max` onto the full `i16` range.
pub fn scale_stick(v: i32, min: i32, max: i32) -> i16 {
    if max <= min {
        return 0;
    }
    let v = v.clamp(min, max) as i64;
    let (min, max) = (min as i64, max as i64);
    ((v - min) * 65535 / (max - min) - 32768) as i16
}

/// Maps a raw trigger reading in `min..=max` onto `0..=255`.
pub fn scale_trigger(v: i32, min: i32, max: i32) -> u8 {
    if max <= min {
        return 0;
    }
    let v = v.clamp(min, max) as i64;
    let (min, max) = (min as i64, max as i64);
    ((v - min) * 255 / (max - min)) as u8
}

/// Radial dead zone that rescales the remaining travel, so there is no jump at
/// the edge of the zone and diagonals still reach full deflection.
/// `zone` is a fraction of full travel (0.0 disables it).
pub fn deadzone(x: i16, y: i16, zone: f32) -> (i16, i16) {
    if zone <= 0.0 {
        return (x, y);
    }
    let (fx, fy) = (x as f32 / 32767.0, y as f32 / 32767.0);
    let mag = (fx * fx + fy * fy).sqrt();
    if mag <= zone {
        return (0, 0);
    }
    let k = (mag - zone) / (1.0 - zone.min(0.95)) / mag;
    let to_i16 = |f: f32| (f * k * 32767.0).clamp(-32768.0, 32767.0) as i16;
    (to_i16(fx), to_i16(fy))
}

/// DualShock 4 report fields, in the layout ViGEm expects.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Ds4 {
    pub lx: u8,
    pub ly: u8,
    pub rx: u8,
    pub ry: u8,
    pub buttons: u16,
    pub special: u8,
    pub l2: u8,
    pub r2: u8,
}

impl PadState {
    pub fn to_ds4(&self) -> Ds4 {
        let b = self.buttons;
        let has = |m: u16| b & m != 0;
        let (u, d, l, r) = (has(btn::DPAD_UP), has(btn::DPAD_DOWN), has(btn::DPAD_LEFT), has(btn::DPAD_RIGHT));
        // Hat value in the low nibble: 0 = north, clockwise to 7 = north-west, 8 = released.
        let x = if r { 1 } else if l { -1 } else { 0 };
        let y = if u { 1 } else if d { -1 } else { 0 };
        let hat: u16 = match (x, y) {
            (0, 1) => 0,
            (1, 1) => 1,
            (1, 0) => 2,
            (1, -1) => 3,
            (0, -1) => 4,
            (-1, -1) => 5,
            (-1, 0) => 6,
            (-1, 1) => 7,
            _ => 8,
        };
        let mut out = hat;
        let mut map = |from: u16, to: u16| {
            if has(from) {
                out |= to;
            }
        };
        map(btn::X, 1 << 4); // square
        map(btn::A, 1 << 5); // cross
        map(btn::B, 1 << 6); // circle
        map(btn::Y, 1 << 7); // triangle
        map(btn::LB, 1 << 8);
        map(btn::RB, 1 << 9);
        map(btn::BACK, 1 << 12); // share
        map(btn::START, 1 << 13); // options
        map(btn::LTHUMB, 1 << 14);
        map(btn::RTHUMB, 1 << 15);
        if self.lt > 0 {
            out |= 1 << 10;
        }
        if self.rt > 0 {
            out |= 1 << 11;
        }
        let x8 = |v: i16| ((v as i32 + 32768) >> 8) as u8;
        // DS4 Y grows downwards, ours grows upwards.
        let y8 = |v: i16| 255 - x8(v);
        Ds4 {
            lx: x8(self.lx),
            ly: y8(self.ly),
            rx: x8(self.rx),
            ry: y8(self.ry),
            buttons: out,
            special: has(btn::GUIDE) as u8,
            l2: self.lt,
            r2: self.rt,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> PadState {
        PadState { buttons: btn::A | btn::DPAD_UP | btn::RB, lt: 7, rt: 255, lx: -32768, ly: 32767, rx: 1234, ry: -1 }
    }

    fn info(name: &str) -> PadInfo {
        PadInfo { bus: BUS_BLUETOOTH, vendor: 0x054c, product: 0x0ce6, uid: 0x1122334455667788, name: Name::new(name) }
    }

    fn all_packets() -> Vec<Packet> {
        vec![
            Packet::Discover,
            Packet::Offer { nonce: u64::MAX - 5 },
            Packet::Gone { slot: 3 },
            Packet::State { slot: 15, seq: 65535, battery: 80, pad: sample() },
            Packet::Info { slot: 2, info: info("DualSense Wireless Controller") },
            Packet::Info { slot: 0, info: info(&"n".repeat(NAME_MAX)) },
            Packet::Rumble { slot: 1, strong: 255, weak: 3 },
            Packet::Ping { t: 123456789 },
            Packet::Pong { t: 987654321 },
        ]
    }

    #[test]
    fn roundtrip_all_packet_types() {
        let mut buf = [0u8; MAX_PACKET];
        for p in all_packets() {
            let bytes = p.encode(&mut buf).to_vec();
            assert_eq!(Packet::decode(&bytes), Some(p));
        }
    }

    #[test]
    fn state_is_twenty_bytes() {
        let mut buf = [0u8; MAX_PACKET];
        let p = Packet::State { slot: 0, seq: 0, battery: 0, pad: PadState::default() };
        assert_eq!(p.encode(&mut buf).len(), 20);
    }

    #[test]
    fn rejects_garbage() {
        let v = VERSION;
        assert_eq!(Packet::decode(b""), None);
        assert_eq!(Packet::decode(b"PL"), None);
        assert_eq!(Packet::decode(&[b'X', b'X', v, 1]), None);
        assert_eq!(Packet::decode(&[b'P', b'L', v + 1, 1]), None); // wrong version
        assert_eq!(Packet::decode(&[b'P', b'L', v, 3, 0, 0]), None); // truncated state
        assert_eq!(Packet::decode(&[b'P', b'L', v, 4, 16]), None); // slot out of range
        assert_eq!(Packet::decode(&[b'P', b'L', v, 2, 1, 2]), None); // truncated offer
        assert_eq!(Packet::decode(&[b'P', b'L', v, 0x7f]), None); // unknown type
    }

    #[test]
    fn names_truncate_on_a_character_boundary() {
        assert_eq!(Name::new("Pad").as_str(), "Pad");
        assert_eq!(Name::new(&"é".repeat(30)).as_str().len(), 40);
        assert_eq!(Name::new(&format!("{}é", "x".repeat(39))).as_str().len(), 39);
        // A name that claims more bytes than the packet holds is rejected.
        let mut buf = [0u8; MAX_PACKET];
        let mut bytes = Packet::Info { slot: 0, info: info("abc") }.encode(&mut buf).to_vec();
        bytes[18] = 200;
        assert_eq!(Packet::decode(&bytes), None);
    }

    #[test]
    fn siphash_reference_vector() {
        // From the SipHash paper: key 00..0f, message 00..0e.
        let msg: Vec<u8> = (0..15).collect();
        assert_eq!(siphash24(0x0706050403020100, 0x0f0e0d0c0b0a0908, &msg), 0xa129ca6149be45e5);
    }

    #[test]
    fn authentication() {
        let mut buf = [0u8; MAX_PACKET];
        let (a, other, off) = (Auth::from_passphrase("living room"), Auth::from_passphrase("guess"), Auth::none());
        for p in all_packets() {
            let sealed = a.seal(42, &p, &mut buf).to_vec();
            assert_eq!(a.open(42, &sealed), Some(p));
            assert_eq!(a.open(43, &sealed), None, "wrong session nonce");
            assert_eq!(other.open(42, &sealed), None, "wrong key");
            let mut tampered = sealed.clone();
            tampered[4] ^= 1;
            assert_eq!(a.open(42, &tampered), None, "modified in transit");
            // Without a key nothing is appended and anything well-formed is accepted.
            let plain = off.seal(0, &p, &mut buf).to_vec();
            assert_eq!(plain.len() + TAG_LEN, sealed.len());
            assert_eq!(off.open(7, &plain), Some(p));
            assert_eq!(a.open(42, &plain), None, "unsigned packet with a key set");
        }
        assert_eq!(a.open(0, b"PL"), None);
    }

    #[test]
    fn sequence_wraps() {
        assert!(seq_newer(1, 0));
        assert!(seq_newer(0, 65535));
        assert!(!seq_newer(5, 5));
        assert!(!seq_newer(4, 5));
        assert!(!seq_newer(65535, 0));
    }

    #[test]
    fn scaling_hits_both_ends() {
        assert_eq!(scale_stick(0, 0, 255), -32768);
        assert_eq!(scale_stick(255, 0, 255), 32767);
        assert_eq!(scale_stick(-32768, -32768, 32767), -32768);
        assert_eq!(scale_stick(32767, -32768, 32767), 32767);
        assert_eq!(scale_stick(0, -32768, 32767), 0);
        assert_eq!(scale_stick(999, 0, 255), 32767); // out-of-range input is clamped
        assert_eq!(scale_stick(5, 5, 5), 0); // degenerate axis
        assert_eq!(scale_trigger(0, 0, 1023), 0);
        assert_eq!(scale_trigger(1023, 0, 1023), 255);
    }

    #[test]
    fn deadzone_behaviour() {
        assert_eq!(deadzone(1234, -4321, 0.0), (1234, -4321));
        assert_eq!(deadzone(1000, -1000, 0.1), (0, 0));
        assert_eq!(deadzone(32767, 0, 0.1), (32767, 0));
        let (x, _) = deadzone(16384, 0, 0.1);
        assert!(x > 0 && x < 16384, "travel is rescaled, got {x}");
        // Full diagonal must not overflow.
        assert_eq!(deadzone(32767, -32768, 0.2), (32767, -32768));
    }

    #[test]
    fn ds4_mapping() {
        let d = sample().to_ds4();
        assert_eq!(d.buttons & 0xf, 0); // north
        assert_ne!(d.buttons & (1 << 5), 0); // cross
        assert_ne!(d.buttons & (1 << 9), 0); // R1
        assert_ne!(d.buttons & (1 << 11), 0); // R2 digital bit follows the trigger
        assert_eq!((d.lx, d.ly), (0, 0)); // full left, full up
        assert_eq!(d.r2, 255);

        let idle = PadState::default().to_ds4();
        assert_eq!(idle.buttons, 8);
        assert_eq!((idle.lx, idle.ly, idle.rx, idle.ry), (128, 127, 128, 127));

        let hat = |b: u16| PadState { buttons: b, ..Default::default() }.to_ds4().buttons & 0xf;
        assert_eq!(hat(btn::DPAD_UP | btn::DPAD_RIGHT), 1);
        assert_eq!(hat(btn::DPAD_RIGHT), 2);
        assert_eq!(hat(btn::DPAD_DOWN | btn::DPAD_RIGHT), 3);
        assert_eq!(hat(btn::DPAD_DOWN), 4);
        assert_eq!(hat(btn::DPAD_DOWN | btn::DPAD_LEFT), 5);
        assert_eq!(hat(btn::DPAD_LEFT), 6);
        assert_eq!(hat(btn::DPAD_UP | btn::DPAD_LEFT), 7);
    }
}
