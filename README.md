![Wave XLR for Linux](docs/readme-header.png)

# Wave XLR for Linux

An unofficial control app and level memory for the Elgato Wave XLR on Linux. It sets your levels again whenever the
device comes back, and gives you its settings in a small window.

> Not an official Elgato product, and not affiliated with or endorsed by Elgato or Corsair. "Wave XLR" is a name of
> its owner and is used here only to say which device the program is for.

![The app, dark and light](docs/screenshots.png)

## Why
The Linux audio driver resets the Wave XLR to start values (gain 75 dB, headphones -23.5 dB) every time the device
shows up: after a boot, a replug, or a resume. Elgato's software does not exist for Linux, so you had to set your
levels again every time. This app saves the levels you set and restores them within about a second.

## What it does
- Restores gain, headphone volume and mute after boot, login, replug and resume. 48 V, low impedance, Clipguard, gain
  lock and the mic/PC mix are restored only if you set them in the app.
- Tray icon (StatusNotifierItem, built for KDE Plasma) with Mute and Open window.
- A small window with the device's own controls: mic gain, live input meter, mute, **48 V**, **Clipguard**, **gain lock**,
  headphone volume, **mic/PC mix**, low impedance. It follows the dial and the mute touch on the device and shows the
  firmware version.
- Start at login and start minimised, as two switches in the window.
- Plain device names in your sound settings: "Wave XLR Microphone" and "Wave XLR Headphones" (installs the known
  WirePlumber fix for the silent microphone, with proper names).
- Light: about 8 MB of RAM in the background. The window is a separate process that only runs while it is open.

It does not do routing, a virtual mixer, effects or a low cut filter (on this device Elgato's software does those in
software). For that, see OpenWave and OpenXLR below.

## Install
You need Rust, `alsa-lib` headers, `alsa-utils` and `libpulse` (for the meter).
```sh
git clone https://github.com/OWNER/wavexlr-linux
cd wavexlr-linux
./install.sh          # builds, installs to ~/.local, asks for sudo once for the USB permission rule
```
Arch/CachyOS: `sudo pacman -S rust alsa-lib alsa-utils libpulse`. Remove everything with `./install.sh --uninstall`
(add `--purge` to delete the saved levels, too).

The app starts at login. Click the tray icon to open the window.

## Notes
- The Wave XLR must be set up for PipeWire the usual way. If the microphone stays silent, see
  [wavexlr-on-linux-cfg](https://github.com/jmansar/wavexlr-on-linux-cfg).
- **48 V** is only switched by you, in the window, after a warning. The app never turns it on by itself and only
  restores it if you have set it yourself. 48 V can damage dynamic and ribbon microphones.
- Only the original Wave XLR (`0fd9:007d`) is supported for now.
- Only one program may use the device's control interface at a time.

## How it works
See [DEVELOPING.md](DEVELOPING.md). In short: the daemon watches the device, waits until the audio driver
is done, writes your saved levels into it and saves changes you make on the device.

## Thanks
The USB control protocol was worked out by the community. This project uses the facts documented by
[OpenWave](https://github.com/rikkichy/openwave) and [OpenXLR](https://github.com/emaspa/openxlr) (no code taken).
If you want a full mixer with routing and effects, use one of them.

## License
MIT, see [LICENSE](LICENSE).
