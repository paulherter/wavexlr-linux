# Developing Wave XLR for Linux

A small Rust program: a resident daemon with a tray icon, and a window process that only runs while the window is open.

## Layout
- `src/protocol.rs`: the 34-byte config block of the Wave XLR and the ALSA step maths.
- `src/transport.rs`: finding the device in sysfs, USB control transfers (usbfs ioctl), the ALSA mixer.
- `src/controller.rs`: the state machine (absent / initializing / ready / error), the restore/save logic and the gain lock.
- `src/settings.rs`, `src/autostart.rs`: saved levels in `~/.config/wavexlr/settings.json`, the two autostart switches.
- `src/daemon.rs`, `src/tray.rs`, `src/ipc.rs`: the resident part, the tray icon, the socket to the window.
- `src/window/`: the egui window (`wavexlr window --demo` shows it without a device; keys 1 to 8 switch the state).
- `packaging/`: udev rule, desktop entries, WirePlumber files, Arch `PKGBUILD`.

## Build and test
```sh
cargo build --release
cargo test
./target/release/wavexlr window --demo
```
Needs Rust and the `alsa-lib` headers. Install for your user with `./install.sh` (one sudo for the USB rule), remove with
`./install.sh --uninstall`.

## How the device is controlled
The Wave XLR (`0fd9:007d`) is a USB audio device plus a vendor interface (3) for its settings. The kernel's
`snd-usb-audio` writes fresh start values into the device every time it binds (gain 75 dB, headphones -23.5 dB), which is
why the levels are lost after boot or replug. The daemon waits until the card is ready and writes the saved levels back.

Settings live in one 34-byte config block, read and written with class control transfers on endpoint 0
(`bmRequestType` 0xA1 / 0x21, `bRequest` 0x85 / 0x05, `wValue` 0, `wIndex` 0x3303). Writes are read-modify-write of the
whole block. All values are little endian.

| Byte | Field |
|---|---|
| 0-1 | gain, uint16 / 256 dB (0..80) |
| 4 | mute |
| 5 | Clipguard |
| 6 | 48 V phantom power |
| 9-10 | headphone level, int16 / 256 dB |
| 12-13 | mic/PC mix, uint16 / 256 percent of microphone in the headphones |
| 14 | dial mode |
| 28 | gain lock flag (the firmware only stores it; the daemon enforces it) |
| 33 | low impedance |

Block `0x0001` (10 bytes) holds raw meter levels; the window meters the PCM capture instead. Gain, headphone volume and
mute are also mirrored through the ALSA mixer (`Mic Capture Volume`, `PCM Playback Volume`, `Mic Capture Switch`), so
the sound settings of your desktop stay in step. The protocol was worked out by the community; see the thanks in the README.

## Good to know
- Only one program may talk to the control interface at a time. Quit `wavexlr` before using other tools on the device.
- Never write 48 V while testing with a microphone connected.
