//! Virtual controller backends: ViGEmBus on Windows, uinput on Linux, and a
//! printing dry run that works everywhere.

use padlink_proto::PadState;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    X360,
    Ds4,
}

impl Kind {
    pub fn label(self) -> &'static str {
        match self {
            Kind::X360 => "Xbox 360",
            Kind::Ds4 => "DualShock 4",
        }
    }
}

/// A plugged-in virtual controller. Dropping it unplugs it.
pub trait VirtualPad {
    fn update(&mut self, pad: &PadState) -> Result<(), String>;
}

/// Called from a driver thread when a game changes the vibration of a virtual
/// pad, with the strong and weak motor levels.
pub type RumbleFn = Box<dyn FnMut(u8, u8) + Send>;

pub enum Backend {
    DryRun,
    #[cfg(windows)]
    Vigem(std::sync::Arc<vigem_client::Client>),
    #[cfg(target_os = "linux")]
    Uinput,
}

impl Backend {
    pub fn open(dry_run: bool) -> Result<Backend, String> {
        if dry_run {
            return Ok(Backend::DryRun);
        }
        #[cfg(windows)]
        {
            let client = vigem_client::Client::connect().map_err(|e| {
                format!(
                    "cannot reach the ViGEmBus driver ({e:?}). Install it from \
                     https://github.com/nefarius/ViGEmBus/releases and try again"
                )
            })?;
            Ok(Backend::Vigem(std::sync::Arc::new(client)))
        }
        #[cfg(target_os = "linux")]
        {
            // Fail now, with advice, rather than when the first controller shows up.
            std::fs::OpenOptions::new().write(true).open("/dev/uinput").map_err(|e| {
                format!(
                    "cannot open /dev/uinput ({e}). Run as root once to check, then allow your user with:\n                       echo 'KERNEL==\"uinput\", MODE=\"0660\", GROUP=\"input\"' | sudo tee /etc/udev/rules.d/99-padlink.rules\n                       sudo usermod -aG input $USER    (then log out and in; `sudo modprobe uinput` if the file is missing)"
                )
            })?;
            Ok(Backend::Uinput)
        }
        #[cfg(not(any(windows, target_os = "linux")))]
        Err("virtual controllers need Windows or Linux; on this OS use --dry-run".into())
    }

    pub fn plug(&self, kind: Kind, rumble: RumbleFn) -> Result<Box<dyn VirtualPad>, String> {
        match self {
            Backend::DryRun => {
                drop(rumble);
                Ok(Box::new(Printer))
            }
            #[cfg(windows)]
            Backend::Vigem(client) => windows::plug(client.clone(), kind, rumble),
            #[cfg(target_os = "linux")]
            Backend::Uinput => {
                // Vibration is not passed back from Linux games yet.
                drop(rumble);
                linux::plug(kind)
            }
        }
    }
}

struct Printer;

impl VirtualPad for Printer {
    fn update(&mut self, p: &PadState) -> Result<(), String> {
        println!(
            "  buttons {:#06x}  L({:+6},{:+6})  R({:+6},{:+6})  LT {:3}  RT {:3}",
            p.buttons, p.lx, p.ly, p.rx, p.ry, p.lt, p.rt
        );
        Ok(())
    }
}

#[cfg(windows)]
mod windows {
    use super::{Kind, RumbleFn, VirtualPad};
    use padlink_proto::PadState;
    use std::sync::Arc;
    use vigem_client as vg;

    struct X360(vg::Xbox360Wired<Arc<vg::Client>>);
    struct Ds4(vg::DualShock4Wired<Arc<vg::Client>>);

    pub fn plug(client: Arc<vg::Client>, kind: Kind, mut rumble: RumbleFn) -> Result<Box<dyn VirtualPad>, String> {
        let err = |e: vg::Error| format!("{e:?}");
        match kind {
            Kind::X360 => {
                let mut t = vg::Xbox360Wired::new(client, vg::TargetId::XBOX360_WIRED);
                t.plugin().map_err(err)?;
                t.wait_ready().map_err(err)?;
                // The thread ends by itself when the pad is unplugged.
                if let Ok(notifications) = t.request_notification() {
                    notifications.spawn_thread(move |_, n| rumble(n.large_motor, n.small_motor));
                }
                Ok(Box::new(X360(t)))
            }
            Kind::Ds4 => {
                let mut t = vg::DualShock4Wired::new(client, vg::TargetId::DUALSHOCK4_WIRED);
                t.plugin().map_err(err)?;
                t.wait_ready().map_err(err)?;
                // The driver library does not report DS4 vibration yet.
                drop(rumble);
                Ok(Box::new(Ds4(t)))
            }
        }
    }

    impl VirtualPad for X360 {
        fn update(&mut self, p: &PadState) -> Result<(), String> {
            // The wire format uses XInput's button bits and axis conventions as-is.
            let report = vg::XGamepad {
                buttons: vg::XButtons { raw: p.buttons },
                left_trigger: p.lt,
                right_trigger: p.rt,
                thumb_lx: p.lx,
                thumb_ly: p.ly,
                thumb_rx: p.rx,
                thumb_ry: p.ry,
            };
            self.0.update(&report).map_err(|e| format!("{e:?}"))
        }
    }

    impl VirtualPad for Ds4 {
        fn update(&mut self, p: &PadState) -> Result<(), String> {
            let d = p.to_ds4();
            let report = vg::DS4Report {
                thumb_lx: d.lx,
                thumb_ly: d.ly,
                thumb_rx: d.rx,
                thumb_ry: d.ry,
                buttons: d.buttons,
                special: d.special,
                trigger_l: d.l2,
                trigger_r: d.r2,
            };
            self.0.update(&report).map_err(|e| format!("{e:?}"))
        }
    }
}

#[cfg(target_os = "linux")]
mod linux {
    use super::{Kind, VirtualPad};
    use evdev::uinput::{VirtualDevice, VirtualDeviceBuilder};
    use evdev::{AbsInfo, AbsoluteAxisType as Abs, AttributeSet, BusType, EventType, InputEvent, InputId, Key, UinputAbsSetup};
    use padlink_proto::{btn, PadState};

    /// Buttons that sit on the same key for both pad types.
    const KEYS: [(u16, Key); 9] = [
        (btn::A, Key::BTN_SOUTH),
        (btn::B, Key::BTN_EAST),
        (btn::LB, Key::BTN_TL),
        (btn::RB, Key::BTN_TR),
        (btn::BACK, Key::BTN_SELECT),
        (btn::START, Key::BTN_START),
        (btn::GUIDE, Key::BTN_MODE),
        (btn::LTHUMB, Key::BTN_THUMBL),
        (btn::RTHUMB, Key::BTN_THUMBR),
    ];

    /// Presents itself exactly as the kernel's own driver presents the real
    /// pad - same ids, same key layout - so games and SDL recognise it.
    struct Pad {
        dev: VirtualDevice,
        /// Keys for X and Y. The Xbox driver has them the other way round.
        x: Key,
        y: Key,
        ds4: bool,
    }

    pub fn plug(kind: Kind) -> Result<Box<dyn VirtualPad>, String> {
        let ds4 = kind == Kind::Ds4;
        let (name, product, x, y) = match kind {
            Kind::X360 => ("Microsoft X-Box 360 pad", (0x045e, 0x028e, 0x0114), Key::BTN_NORTH, Key::BTN_WEST),
            Kind::Ds4 => ("Sony Interactive Entertainment Wireless Controller", (0x054c, 0x09cc, 0x8111), Key::BTN_WEST, Key::BTN_NORTH),
        };
        let mut keys = AttributeSet::<Key>::new();
        KEYS.iter().for_each(|(_, key)| keys.insert(*key));
        [x, y].into_iter().for_each(|key| keys.insert(key));
        if ds4 {
            // The real one reports its triggers as buttons too.
            keys.insert(Key::BTN_TL2);
            keys.insert(Key::BTN_TR2);
        }
        let build = || -> std::io::Result<VirtualDevice> {
            let mut b = VirtualDeviceBuilder::new()?
                .name(name)
                .input_id(InputId::new(BusType::BUS_USB, product.0, product.1, product.2))
                .with_keys(&keys)?;
            for axis in [Abs::ABS_X, Abs::ABS_Y, Abs::ABS_RX, Abs::ABS_RY] {
                b = b.with_absolute_axis(&UinputAbsSetup::new(axis, AbsInfo::new(0, -32768, 32767, 16, 128, 0)))?;
            }
            for axis in [Abs::ABS_Z, Abs::ABS_RZ] {
                b = b.with_absolute_axis(&UinputAbsSetup::new(axis, AbsInfo::new(0, 0, 255, 0, 0, 0)))?;
            }
            for axis in [Abs::ABS_HAT0X, Abs::ABS_HAT0Y] {
                b = b.with_absolute_axis(&UinputAbsSetup::new(axis, AbsInfo::new(0, -1, 1, 0, 0, 0)))?;
            }
            b.build()
        };
        let dev = build().map_err(|e| format!("uinput: {e}"))?;
        Ok(Box::new(Pad { dev, x, y, ds4 }))
    }

    impl VirtualPad for Pad {
        fn update(&mut self, p: &PadState) -> Result<(), String> {
            let down = |bit: u16| (p.buttons & bit != 0) as i32;
            let key = |key: Key, value: i32| InputEvent::new(EventType::KEY, key.code(), value);
            let abs = |axis: Abs, value: i32| InputEvent::new(EventType::ABSOLUTE, axis.0, value);
            // The wire format has up as positive; Linux has it negative.
            let flip = |v: i16| (-(v as i32)).min(32767);

            let mut events = Vec::with_capacity(24);
            events.extend(KEYS.iter().map(|(bit, k)| key(*k, down(*bit))));
            events.push(key(self.x, down(btn::X)));
            events.push(key(self.y, down(btn::Y)));
            if self.ds4 {
                events.push(key(Key::BTN_TL2, (p.lt > 0) as i32));
                events.push(key(Key::BTN_TR2, (p.rt > 0) as i32));
            }
            events.extend([
                abs(Abs::ABS_X, p.lx as i32),
                abs(Abs::ABS_Y, flip(p.ly)),
                abs(Abs::ABS_RX, p.rx as i32),
                abs(Abs::ABS_RY, flip(p.ry)),
                abs(Abs::ABS_Z, p.lt as i32),
                abs(Abs::ABS_RZ, p.rt as i32),
                abs(Abs::ABS_HAT0X, down(btn::DPAD_RIGHT) - down(btn::DPAD_LEFT)),
                abs(Abs::ABS_HAT0Y, down(btn::DPAD_DOWN) - down(btn::DPAD_UP)),
            ]);
            // The kernel drops the ones that did not change and adds the sync marker.
            self.dev.emit(&events).map_err(|e| e.to_string())
        }
    }
}
