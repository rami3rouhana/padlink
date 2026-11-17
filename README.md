# padlink

Use controllers that are connected to a Raspberry Pi (Batocera) on a PC, over the home
network, with as little delay as the network allows.

```
 controllers ──USB/Bluetooth──▶ Raspberry Pi                      PC (Windows or Linux)
                                padlink-sender ──UDP, 20 bytes──▶ padlink ──▶ virtual Xbox 360 / DS4 pads
                                (reads evdev)    auto-discovered             (seen by every game)
                                                ◀──── rumble ────
```

* **No IP configuration.** The sender broadcasts on the LAN, the PC answers. If the PC
  reboots or gets a new address, the sender finds it again by itself.
* **Controllers keep their place.** Each controller is recognised by its own identity
  (Bluetooth address, or USB port). When one drops out — flat battery, Bluetooth hiccup, a
  Pi reboot — its virtual pad stays plugged in for two minutes and it comes back as the
  same player, without the game noticing.
* **You can see what is connected.** Every controller is listed by name with its player
  number, USB/Bluetooth, battery level and network round trip.
* **Many controllers, several Pis.** Up to 16 per Pi, any number of Pis, hot-plugged at
  any time. PlayStation controllers become DualShock 4 pads (right button glyphs in
  games), everything else an Xbox 360 pad; a fifth Xbox pad becomes a DS4 because Windows
  stops at four.
* **Rumble** from the game reaches the real controller (Xbox 360 virtual pads on Windows).
* **Event-driven end to end.** A state leaves the Pi when the kernel delivers it and is
  handed to the driver when it arrives. No polling timers, no queues, no sleeps.
* **Loss-tolerant.** Every packet carries the full state and is repeated every 100 ms,
  stale or reordered packets are dropped, and if the link goes quiet for 350 ms all inputs
  are released so nothing stays "stuck".

## Install

### Windows PC

1. Install the [ViGEmBus driver](https://github.com/nefarius/ViGEmBus/releases/latest) once.
   Windows cannot create virtual controllers without a driver, so this step cannot be
   removed — padlink tells you and opens the page if it is missing.
2. Download `padlink-tray.exe` from the [releases](../../releases/latest) and double-click it.
   Allow it through the firewall when Windows asks (once).

A gamepad icon appears next to the clock (possibly behind the `^` arrow); it turns green
while a controller is connected. Click it to see the controllers and to tick
**Start with Windows**. That is all the setup there is.

`padlink-receiver.exe` is the same program for a terminal: it shows a live table instead
of an icon and prints errors instead of dialog boxes.

```
  Player  Controller                Link       Sender  Battery  Round trip  Appears as   Status
  1       Xbox Wireless Controller  Bluetooth  1       80%      0.4 ms      Xbox 360     connected
  2       DualSense                 USB        1       -        0.4 ms      DualShock 4  away (player 2 kept 95 s)
```

### Raspberry Pi / Batocera

Over SSH (user `root`, password `linux` by default), one line:

```sh
curl -fsSL https://raw.githubusercontent.com/rami3rouhana/padlink/main/deploy/batocera/install.sh | sh
```

It downloads the sender for the Pi's processor, installs it as a service and starts it; it
starts on every boot from then on. The binary is static and needs nothing installed on
Batocera. Run the line again to update, or `sh install.sh --uninstall` to remove.

Sender options go in `/userdata/system/padlink/options` (then reboot the Pi), or at
install time:
`curl ... | PADLINK_OPTIONS="--grab" sh`.

| Option | |
|---|---|
| `--grab` | Batocera itself ignores the controllers while they are streamed |
| `--key <passphrase>` | only talk to a PC started with the same key |
| `--swap-xy on\|off` | if X/Y (square/triangle) come out swapped on an unusual pad |
| `--target <ip>` | skip discovery, for networks that block broadcast |
| `--port <N>` | UDP port (default 9977) |

### Linux PC

`padlink-receiver-linux-x86_64` from the releases creates the virtual pads through uinput.
It needs write access to `/dev/uinput`; if it has none it prints the two commands that
grant it. Rumble is not passed back on Linux yet.

### Try it without a Pi or a controller

```powershell
padlink-receiver.exe
padlink-sender.exe --test-pattern     # second terminal: left stick circles, A taps
```

## Settings

All optional. The PC side reads `%APPDATA%\padlink\config.txt` (Linux:
`~/.config/padlink/config.txt`); the file is created with every line commented out, and
the tray menu opens it. Restart padlink after editing.

```ini
pad = auto          # auto, x360 or ds4
deadzone = 0        # 0 to 0.9, for worn sticks
hold = 120          # seconds a disconnected controller keeps its player number
key = some passphrase   # must match --key on the sender

[8BitDo]            # part of a controller's name, or its USB id as vendor:product
a = b               # pressing A sends B
b = a
guide = none        # disable a button

[054c:0ce6]
pad = x360
deadzone = 0.08
```

The same settings exist as command-line options (`--pad`, `--deadzone`, `--hold`, `--key`,
`--port`, `--config <file>`), which win over the file. `padlink-receiver` also has
`--view table|log`, `-v` for packet statistics and `--dry-run` to print states without
creating controllers.

**The key** is for shared networks (flats, dorms): without it, anyone on the same LAN can
send input to the PC. With it, packets are authenticated and replays from an earlier
session are rejected. They are not encrypted — button presses are not secret — and the
port should never be exposed to the internet either way.

## Getting the lowest latency

The software path is well under a millisecond; what remains is physical. The round trip
shown for each controller is the network part, measured continuously.

| Hop | Typical | What to do |
|---|---|---|
| Controller → Pi over Bluetooth | 4–15 ms | Use USB or the pad's own 2.4 GHz dongle |
| Pi → PC over Wi-Fi | 1–10 ms, spiky | Use Ethernet on the Pi; otherwise 5 GHz. Packets are DSCP-marked for the Wi-Fi voice queue |
| Pi → PC over Ethernet | < 0.5 ms | — |
| Game input sampling | up to one frame | — |

## Build

```sh
cargo test --workspace
cargo build --release -p padlink-receiver                                      # on the PC: both programs
rustup target add aarch64-unknown-linux-musl
cargo build --release -p padlink-sender --target aarch64-unknown-linux-musl    # from any OS
sh deploy/batocera/install.sh ./padlink-sender                                 # on the Pi, without downloading
```

The Linux binaries cross-link with Rust's bundled LLD (see `.cargo/config.toml`); no cross
toolchain is required. CI builds everything and attaches the binaries to `v*` tags, which
is where the installer downloads from.

## Wire format

One UDP port (9977). Header `"PL"`, version, type; little-endian. `STATE` is 20 bytes:
slot, battery, sequence, buttons (XInput bit layout), LT, RT, LX, LY, RX, RY. Other
packets carry discovery, controller identity, rumble and ping. With a key, each packet
ends in an 8-byte SipHash-2-4 tag. See `crates/proto/src/lib.rs`.

## Not done yet

* Rumble for virtual DualShock 4 pads and on the Linux receiver.
* DualShock touchpad / gyro pass-through.
* A macOS receiver (macOS has no supported way to create a virtual gamepad).

## Legacy

`connect.py`, `connectx.py`, `newConnect.py`, `ps.py` and `xbox.py` are the original Python
receivers this replaces. They are kept for reference and speak a different (text) protocol.
