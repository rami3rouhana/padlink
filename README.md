# padlink

Use controllers that are connected to a Raspberry Pi (Batocera) on a Windows PC,
over the home network, with as little delay as the network allows.

```
 controllers ──USB/Bluetooth──▶ Raspberry Pi                      Windows PC
                                padlink-sender ──UDP, 20 bytes──▶ padlink-receiver ──▶ virtual Xbox 360 / DS4 pads
                                (reads evdev)    auto-discovered  (ViGEmBus)           (seen by every game)
```

* **No IP configuration.** The sender broadcasts on the LAN, the receiver answers. If the
  PC reboots or gets a new address, the sender finds it again by itself.
* **Many controllers.** Up to 16 per Pi, hot-plugged at any time, each becoming its own
  virtual pad (Windows allows 4 Xbox 360 pads; use `--pad ds4` beyond that).
* **Event-driven end to end.** A state leaves the Pi when the kernel delivers it and is handed
  to the driver when it arrives. No polling timers, no queues, no sleeps.
* **Loss-tolerant.** Every packet carries the full state and is repeated every 100 ms, stale or
  reordered packets are dropped by sequence number, and if the link goes quiet for 350 ms all
  inputs are released so nothing stays "stuck".

## Install

### Windows PC (receiver)

1. Install the [ViGEmBus driver](https://github.com/nefarius/ViGEmBus/releases) once.
2. Run `padlink-receiver.exe`. Allow it through the firewall when Windows asks, or once, as admin:
   ```powershell
   New-NetFirewallRule -DisplayName padlink -Direction Inbound -Protocol UDP -LocalPort 9977 -Action Allow -Profile Private
   ```

Options: `--pad ds4` for DualShock 4 pads, `--deadzone 0.1` for worn sticks, `-v` for packet
statistics, `--dry-run` to print states without creating controllers.

### Raspberry Pi / Batocera (sender)

Copy `padlink-sender` (arm64 for Pi 3/4/5, armv7 for 32-bit images) and `deploy/batocera/padlink`
to the Pi, then over SSH:

```sh
mkdir -p /userdata/system/padlink /userdata/system/services
cp padlink-sender /userdata/system/padlink/ && chmod +x /userdata/system/padlink/padlink-sender
cp padlink /userdata/system/services/padlink && chmod +x /userdata/system/services/padlink
batocera-services enable padlink && batocera-services start padlink
```

It now starts on every boot. The binary is static, so it needs nothing installed on Batocera.

Options: `--grab` so Batocera itself ignores the controllers while streaming,
`--swap-xy on|off` if X/Y (square/triangle) come out swapped on an unusual pad,
`--target <ip>` to bypass discovery on networks that block broadcast.

### Try it without a Pi or a controller

```powershell
padlink-receiver.exe -v
padlink-sender.exe --test-pattern     # second terminal: left stick circles, A taps
```

## Build

```sh
cargo test -p padlink-proto
cargo build --release -p padlink-receiver                                      # on Windows
rustup target add aarch64-unknown-linux-musl
cargo build --release -p padlink-sender --target aarch64-unknown-linux-musl    # from any OS
```

The Pi binary cross-links with Rust's bundled LLD (see `.cargo/config.toml`); no cross
toolchain is required. CI builds all three binaries and attaches them to `v*` tags.

## Getting the lowest latency

The software path here is well under a millisecond; what remains is physical:

| Hop | Typical | What to do |
|---|---|---|
| Controller → Pi over Bluetooth | 4–15 ms | Use USB or the pad's own 2.4 GHz dongle |
| Pi → PC over Wi-Fi | 1–10 ms, spiky | Use Ethernet on the Pi; otherwise 5 GHz. Packets are DSCP-marked for the Wi-Fi voice queue |
| Pi → PC over Ethernet | < 0.5 ms | — |
| Game input sampling | up to one frame | — |

## Wire format

One UDP port (9977). Header `"PL"`, version, type; little-endian. `STATE` is 20 bytes:
slot, battery, sequence, buttons (XInput bit layout), LT, RT, LX, LY, RX, RY.
See `crates/proto/src/lib.rs`.

## Not done yet

* Rumble back to the physical controller (the protocol has room for it).
* DualShock touchpad / gyro pass-through.
* Packets are neither authenticated nor encrypted: anyone on the same LAN can inject
  input. Fine at home; do not expose the port to the internet.

## Legacy

`connect.py`, `connectx.py`, `newConnect.py`, `ps.py` and `xbox.py` are the original Python
receivers this replaces. They are kept for reference and speak a different (text) protocol.
