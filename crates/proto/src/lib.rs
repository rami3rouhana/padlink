//! padlink wire format.
//!
//! Every datagram starts with a 4-byte header: `b"PL"`, protocol version, packet type.
//! All multi-byte integers are little-endian. A full controller state is 20 bytes,
//! so it always fits in a single frame and is never fragmented.

pub const DEFAULT_PORT: u16 = 9977;
pub const VERSION: u8 = 1;
pub const MAX_SLOTS: usize = 16;

const MAGIC: [u8; 2] = *b"PL";
const T_DISCOVER: u8 = 1;
const T_OFFER: u8 = 2;
const T_STATE: u8 = 3;
const T_GONE: u8 = 4;

pub const STATE_LEN: usize = 20;
pub const MAX_PACKET: usize = 32;

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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Packet {
    /// Sender -> broadcast: "is a receiver out there?"
    Discover,
    /// Receiver -> sender: "yes, send to the address this came from."
    Offer,
    /// Sender -> receiver: full state of one controller. Sent on every change
    /// and repeated as a keepalive, so a lost packet heals itself.
    State { slot: u8, seq: u16, battery: u8, pad: PadState },
    /// Sender -> receiver: the controller in `slot` was unplugged.
    Gone { slot: u8 },
}

pub const BATTERY_UNKNOWN: u8 = 255;

impl Packet {
    /// Serialises into `buf` and returns the used prefix.
    pub fn encode<'a>(&self, buf: &'a mut [u8; MAX_PACKET]) -> &'a [u8] {
        buf[0] = MAGIC[0];
        buf[1] = MAGIC[1];
        buf[2] = VERSION;
        let len = match *self {
            Packet::Discover => {
                buf[3] = T_DISCOVER;
                4
            }
            Packet::Offer => {
                buf[3] = T_OFFER;
                4
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
        };
        &buf[..len]
    }

    /// Parses a datagram. Anything malformed, foreign or from another protocol
    /// version yields `None` and is simply ignored by the caller.
    pub fn decode(d: &[u8]) -> Option<Packet> {
        if d.len() < 4 || d[0..2] != MAGIC || d[2] != VERSION {
            return None;
        }
        let i16_at = |i: usize| i16::from_le_bytes([d[i], d[i + 1]]);
        match d[3] {
            T_DISCOVER => Some(Packet::Discover),
            T_OFFER => Some(Packet::Offer),
            T_GONE if d.len() >= 5 && (d[4] as usize) < MAX_SLOTS => Some(Packet::Gone { slot: d[4] }),
            T_STATE if d.len() >= STATE_LEN && (d[4] as usize) < MAX_SLOTS => Some(Packet::State {
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
            _ => None,
        }
    }
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

    #[test]
    fn roundtrip_all_packet_types() {
        let mut buf = [0u8; MAX_PACKET];
        for p in [
            Packet::Discover,
            Packet::Offer,
            Packet::Gone { slot: 3 },
            Packet::State { slot: 15, seq: 65535, battery: 80, pad: sample() },
        ] {
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
        assert_eq!(Packet::decode(b""), None);
        assert_eq!(Packet::decode(b"PL"), None);
        assert_eq!(Packet::decode(b"XX\x01\x01"), None);
        assert_eq!(Packet::decode(b"PL\x09\x01"), None); // wrong version
        assert_eq!(Packet::decode(b"PL\x01\x03\x00\x00"), None); // truncated state
        assert_eq!(Packet::decode(b"PL\x01\x04\x10"), None); // slot out of range
        assert_eq!(Packet::decode(b"PL\x01\x7f"), None); // unknown type
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
