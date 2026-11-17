//! Virtual controller backends: ViGEmBus on Windows, and a printing dry run
//! that works everywhere.

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

pub enum Backend {
    DryRun,
    #[cfg(windows)]
    Vigem(std::sync::Arc<vigem_client::Client>),
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
        #[cfg(not(windows))]
        Err("virtual controllers need Windows with ViGEmBus; on this OS use --dry-run".into())
    }

    pub fn plug(&self, kind: Kind) -> Result<Box<dyn VirtualPad>, String> {
        match self {
            Backend::DryRun => Ok(Box::new(Printer)),
            #[cfg(windows)]
            Backend::Vigem(client) => windows::plug(client.clone(), kind),
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
    use super::{Kind, VirtualPad};
    use padlink_proto::PadState;
    use std::sync::Arc;
    use vigem_client as vg;

    struct X360(vg::Xbox360Wired<Arc<vg::Client>>);
    struct Ds4(vg::DualShock4Wired<Arc<vg::Client>>);

    pub fn plug(client: Arc<vg::Client>, kind: Kind) -> Result<Box<dyn VirtualPad>, String> {
        let err = |e: vg::Error| format!("{e:?}");
        match kind {
            Kind::X360 => {
                let mut t = vg::Xbox360Wired::new(client, vg::TargetId::XBOX360_WIRED);
                t.plugin().map_err(err)?;
                t.wait_ready().map_err(err)?;
                Ok(Box::new(X360(t)))
            }
            Kind::Ds4 => {
                let mut t = vg::DualShock4Wired::new(client, vg::TargetId::DUALSHOCK4_WIRED);
                t.plugin().map_err(err)?;
                t.wait_ready().map_err(err)?;
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
