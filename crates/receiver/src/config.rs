//! The optional settings file: defaults for the command-line options, plus
//! per-controller sections for button remapping and overrides.

use crate::vpad::Kind;
use padlink_proto::{btn, PadInfo};
use std::path::PathBuf;

pub const TEMPLATE: &str = "\
# padlink settings. Everything here is optional; lines starting with # are ignored.
# Restart padlink after editing.

# pad = auto          # auto, x360 or ds4
# deadzone = 0        # 0 to 0.9, for worn sticks
# hold = 120          # seconds a disconnected controller keeps its player number
# port = 9977
# key = some passphrase   # must match --key on the sender

# Per-controller settings. The heading is part of the controller's name as shown
# by padlink, or its USB id as vendor:product.
#
# [8BitDo]
# a = b               # pressing A sends B
# b = a
# x = y
# y = x
#
# [054c:0ce6]
# pad = x360
# deadzone = 0.08
# guide = none        # disable a button
#
# Buttons: a b x y lb rb back start guide ls rs up down left right
";

const BUTTONS: [(&str, u16); 15] = [
    ("up", btn::DPAD_UP),
    ("down", btn::DPAD_DOWN),
    ("left", btn::DPAD_LEFT),
    ("right", btn::DPAD_RIGHT),
    ("start", btn::START),
    ("back", btn::BACK),
    ("ls", btn::LTHUMB),
    ("rs", btn::RTHUMB),
    ("lb", btn::LB),
    ("rb", btn::RB),
    ("guide", btn::GUIDE),
    ("a", btn::A),
    ("b", btn::B),
    ("x", btn::X),
    ("y", btn::Y),
];

/// For each of the 16 button bits, the bits it produces.
pub type Remap = [u16; 16];

pub fn identity() -> Remap {
    std::array::from_fn(|i| 1 << i)
}

pub fn remap(buttons: u16, table: &Remap) -> u16 {
    (0..16).filter(|i| buttons >> i & 1 == 1).fold(0, |out, i| out | table[i])
}

#[derive(Debug, PartialEq)]
enum Matcher {
    Ids(u16, u16),
    /// Lower-cased fragment of the controller's name.
    Name(String),
}

#[derive(Debug, PartialEq)]
pub struct Rule {
    matcher: Matcher,
    /// `Some(None)` means "auto".
    pub pad: Option<Option<Kind>>,
    pub deadzone: Option<f32>,
    pub remap: Option<Remap>,
}

impl Rule {
    pub fn matches(&self, info: &PadInfo) -> bool {
        match &self.matcher {
            Matcher::Ids(v, p) => info.vendor == *v && info.product == *p,
            Matcher::Name(part) => info.name.as_str().to_lowercase().contains(part),
        }
    }
}

#[derive(Debug, Default, PartialEq)]
pub struct File {
    pub pad: Option<Option<Kind>>,
    pub deadzone: Option<f32>,
    pub hold: Option<u64>,
    pub port: Option<u16>,
    pub key: Option<String>,
    pub rules: Vec<Rule>,
}

pub fn parse_pad(v: &str) -> Result<Option<Kind>, String> {
    match v {
        "auto" => Ok(None),
        "x360" | "xbox" => Ok(Some(Kind::X360)),
        "ds4" | "ps4" => Ok(Some(Kind::Ds4)),
        _ => Err("pad must be auto, x360 or ds4".into()),
    }
}

pub fn parse_deadzone(v: &str) -> Result<f32, String> {
    v.parse().ok().filter(|d| (0.0..=0.9).contains(d)).ok_or("deadzone must be a number between 0 and 0.9".into())
}

fn button(name: &str) -> Option<u16> {
    BUTTONS.iter().find(|(n, _)| *n == name).map(|(_, bit)| *bit)
}

pub fn parse(text: &str) -> Result<File, String> {
    let mut file = File::default();
    for (n, raw) in text.lines().enumerate() {
        let line = raw.split('#').next().unwrap_or("").trim();
        if line.is_empty() {
            continue;
        }
        let fail = |msg: String| format!("line {}: {msg}", n + 1);

        if let Some(head) = line.strip_prefix('[') {
            let head = head.strip_suffix(']').ok_or(fail("missing ]".into()))?.trim();
            if head.is_empty() {
                return Err(fail("empty [] heading".into()));
            }
            let ids = head.split_once(':').and_then(|(v, p)| {
                Some((u16::from_str_radix(v.trim(), 16).ok()?, u16::from_str_radix(p.trim(), 16).ok()?))
            });
            let matcher = match ids {
                Some((v, p)) => Matcher::Ids(v, p),
                None => Matcher::Name(head.to_lowercase()),
            };
            file.rules.push(Rule { matcher, pad: None, deadzone: None, remap: None });
            continue;
        }

        let (key, value) = line.split_once('=').ok_or(fail(format!("expected name = value, got \"{line}\"")))?;
        let (key, value) = (key.trim().to_lowercase(), value.trim());
        let lower = value.to_lowercase();
        match (file.rules.last_mut(), key.as_str()) {
            (None, "pad") => file.pad = Some(parse_pad(&lower).map_err(fail)?),
            (Some(r), "pad") => r.pad = Some(parse_pad(&lower).map_err(fail)?),
            (None, "deadzone") => file.deadzone = Some(parse_deadzone(value).map_err(fail)?),
            (Some(r), "deadzone") => r.deadzone = Some(parse_deadzone(value).map_err(fail)?),
            (None, "hold") => file.hold = Some(value.parse().map_err(|_| fail("hold must be a number of seconds".into()))?),
            (None, "port") => file.port = Some(value.parse().map_err(|_| fail("port must be 1-65535".into()))?),
            (None, "key") => file.key = Some(value.to_string()),
            (Some(r), from) => {
                let from = button(from).ok_or(fail(format!("unknown button or setting \"{key}\"")))?;
                let to = if lower == "none" {
                    0
                } else {
                    button(&lower).ok_or(fail(format!("unknown button \"{value}\"")))?
                };
                r.remap.get_or_insert_with(identity)[from.trailing_zeros() as usize] = to;
            }
            (None, _) => return Err(fail(format!("unknown setting \"{key}\""))),
        }
    }
    Ok(file)
}

pub fn default_path() -> Option<PathBuf> {
    #[cfg(windows)]
    let dir = std::env::var_os("APPDATA").map(PathBuf::from);
    #[cfg(not(windows))]
    let dir = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")));
    Some(dir?.join("padlink").join("config.txt"))
}

/// Reads the settings file. The default file is created (all commented out)
/// when missing, so there is something to open and edit; an explicitly named
/// file must exist.
pub fn load(explicit: Option<PathBuf>) -> Result<(File, Option<PathBuf>), String> {
    let path = match explicit {
        Some(p) => p,
        None => {
            let Some(p) = default_path() else { return Ok((File::default(), None)) };
            if !p.exists() {
                if let Some(dir) = p.parent() {
                    let _ = std::fs::create_dir_all(dir);
                }
                let _ = std::fs::write(&p, TEMPLATE);
                return Ok((File::default(), Some(p)));
            }
            p
        }
    };
    let text = std::fs::read_to_string(&path).map_err(|e| format!("cannot read {}: {e}", path.display()))?;
    let file = parse(text.trim_start_matches('\u{feff}')).map_err(|e| format!("{}, {e}", path.display()))?;
    Ok((file, Some(path)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use padlink_proto::Name;

    fn pad(name: &str, vendor: u16, product: u16) -> PadInfo {
        PadInfo { bus: 0, vendor, product, uid: 1, name: Name::new(name) }
    }

    #[test]
    fn template_is_valid_and_empty() {
        assert_eq!(parse(TEMPLATE), Ok(File::default()));
    }

    #[test]
    fn globals_and_sections() {
        let f = parse(
            "pad = ds4\ndeadzone=0.1 # worn\nhold = 30\nkey = Living Room\n\n[8BitDo]\nA = B\nb = a\nguide = none\n\n[054C:0ce6]\npad = auto\ndeadzone = 0.2\n",
        )
        .unwrap();
        assert_eq!((f.pad, f.deadzone, f.hold, f.port), (Some(Some(Kind::Ds4)), Some(0.1), Some(30), None));
        assert_eq!(f.key.as_deref(), Some("Living Room"));

        let (bitdo, sony) = (&f.rules[0], &f.rules[1]);
        assert!(bitdo.matches(&pad("8BitDo Pro 2", 0x2dc8, 0x6003)));
        assert!(!bitdo.matches(&pad("DualSense", 0x054c, 0x0ce6)));
        assert!(sony.matches(&pad("DualSense", 0x054c, 0x0ce6)));
        assert_eq!((sony.pad, sony.deadzone, sony.remap), (Some(None), Some(0.2), None));

        let table = bitdo.remap.unwrap();
        assert_eq!(remap(btn::A, &table), btn::B);
        assert_eq!(remap(btn::B | btn::X | btn::DPAD_UP, &table), btn::A | btn::X | btn::DPAD_UP);
        assert_eq!(remap(btn::GUIDE | btn::Y, &table), btn::Y);
        assert_eq!(remap(0xffff, &identity()), 0xffff);
    }

    #[test]
    fn errors_name_the_line() {
        assert!(parse("pad = n64").unwrap_err().starts_with("line 1:"));
        assert!(parse("\n\nbogus = 1").unwrap_err().starts_with("line 3:"));
        assert!(parse("[x]\na = q").unwrap_err().contains("unknown button \"q\""));
        assert!(parse("[x]\nturbo = a").unwrap_err().contains("turbo"));
        assert!(parse("[x").unwrap_err().contains("missing ]"));
        assert!(parse("deadzone = 2").is_err());
        assert!(parse("a = b").is_err(), "remaps need a section");
        assert!(parse("just words").is_err());
    }
}
