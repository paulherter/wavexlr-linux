//! Talks to the Wave XLR: vendor control transfers over usbfs, levels through the ALSA mixer.

use crate::protocol::{self, Levels, Patch};
use alsa::ctl::{ElemIface, ElemId};
use alsa::HCtl;
use std::ffi::CString;
use std::fmt;
use std::fs;
use std::io;
use std::os::fd::{AsRawFd, OwnedFd};
use std::path::PathBuf;

const VENDOR_INTERFACE: u32 = 3;
const REQ_READ: u8 = 0xA1;
const REQ_WRITE: u8 = 0x21;
const B_READ: u8 = 0x85;
const B_WRITE: u8 = 0x05;
const W_INDEX: u16 = 0x3303;
const BLOCK_CONFIG: u16 = 0x0000;
const TIMEOUT_MS: u32 = 1000;
const DEVICE_NAME: &str = "Wave XLR";

const PERMISSION_HINT: &str =
    "No permission for the USB device. Run install.sh again (it installs the udev rule), then replug the cable.";

/// The device could not be read or written.
#[derive(Debug, Clone, PartialEq)]
pub struct DeviceError(pub String);

impl fmt::Display for DeviceError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for DeviceError {}

impl From<String> for DeviceError {
    fn from(message: String) -> Self {
        DeviceError(message)
    }
}

/// Read-only facts about the device, shown in the window.
#[derive(Debug, Clone, PartialEq)]
pub struct DeviceInfo {
    pub name: String,
    pub firmware: String,
    pub serial: String,
}

/// Everything the controller needs from the device. The real one is [`Hardware`].
pub trait Backend {
    fn present(&mut self) -> bool;
    fn mixer_ready(&mut self) -> bool;
    fn read(&mut self) -> Result<Levels, DeviceError>;
    fn set_gain(&mut self, gain_db: f64) -> Result<(), DeviceError>;
    fn set_headphone(&mut self, headphone_db: f64) -> Result<(), DeviceError>;
    fn set_muted(&mut self, muted: bool) -> Result<(), DeviceError>;
    fn set_phantom(&mut self, on: bool) -> Result<(), DeviceError>;
    fn set_low_z(&mut self, on: bool) -> Result<(), DeviceError>;
    /// Block writes only: these three have no ALSA mirror.
    fn set_clipguard(&mut self, on: bool) -> Result<(), DeviceError>;
    fn set_gain_lock(&mut self, on: bool) -> Result<(), DeviceError>;
    fn set_mic_mix(&mut self, percent: f64) -> Result<(), DeviceError>;
    /// Name, firmware and serial from sysfs. Empty strings where the device does not say.
    fn device_info(&mut self) -> DeviceInfo;
    /// After a change at the device itself: bring the ALSA mixer to the real values. Optional.
    fn sync_mirror(&mut self, _levels: &Levels) {}
}

// --- usbfs -------------------------------------------------------------------------------------

/// struct usbdevfs_ctrltransfer from <linux/usbdevice_fs.h>.
#[repr(C)]
struct CtrlTransfer {
    request_type: u8,
    request: u8,
    value: u16,
    index: u16,
    length: u16,
    timeout: u32,
    data: *mut libc::c_void,
}

const fn ioc(dir: u32, nr: u32, size: usize) -> u32 {
    (dir << 30) | ((size as u32) << 16) | (0x55 << 8) | nr
}

/// _IOWR('U', 0, struct usbdevfs_ctrltransfer)
const USBDEVFS_CONTROL: u32 = ioc(3, 0, std::mem::size_of::<CtrlTransfer>());
/// _IOR('U', 15, unsigned int)
const USBDEVFS_CLAIMINTERFACE: u32 = ioc(2, 15, std::mem::size_of::<u32>());

#[derive(Debug, Clone, PartialEq)]
pub struct UsbLocation {
    /// e.g. "1-9.4.2"
    pub sysname: String,
    /// e.g. "/dev/bus/usb/001/011"
    pub devnode: String,
    pub serial: String,
}

fn read_attr(path: &str) -> io::Result<String> {
    Ok(fs::read_to_string(path)?.trim().to_string())
}

pub fn find_usb() -> Option<UsbLocation> {
    let wanted_vendor = format!("{:04x}", protocol::VENDOR_ID);
    let wanted_product = format!("{:04x}", protocol::PRODUCT_ID);
    for entry in fs::read_dir("/sys/bus/usb/devices").ok()?.flatten() {
        let base = entry.path().to_string_lossy().into_owned();
        let found = || -> Option<UsbLocation> {
            if read_attr(&format!("{base}/idVendor")).ok()? != wanted_vendor
                || read_attr(&format!("{base}/idProduct")).ok()? != wanted_product
            {
                return None;
            }
            let bus: u32 = read_attr(&format!("{base}/busnum")).ok()?.parse().ok()?;
            let dev: u32 = read_attr(&format!("{base}/devnum")).ok()?.parse().ok()?;
            Some(UsbLocation {
                sysname: entry.file_name().to_string_lossy().into_owned(),
                devnode: format!("/dev/bus/usb/{bus:03}/{dev:03}"),
                serial: read_attr(&format!("{base}/serial")).unwrap_or_default(),
            })
        };
        if let Some(location) = found() {
            return Some(location);
        }
    }
    None
}

/// bcdDevice "0135" is firmware 1.3.5 (major, minor, patch). Any other format is shown as it is.
pub fn format_firmware(bcd: &str) -> String {
    let nums: Vec<u32> = bcd.chars().filter_map(|c| c.to_digit(10)).collect();
    if bcd.chars().count() == 4 && nums.len() == 4 {
        format!("{}.{}.{}", nums[0] * 10 + nums[1], nums[2], nums[3])
    } else {
        bcd.to_string()
    }
}

/// Reads firmware and serial from the sysfs directory of the USB device.
fn read_device_info(base: &str) -> DeviceInfo {
    DeviceInfo {
        name: DEVICE_NAME.to_string(),
        firmware: read_attr(&format!("{base}/bcdDevice")).map(|bcd| format_firmware(&bcd)).unwrap_or_default(),
        serial: read_attr(&format!("{base}/serial")).unwrap_or_default(),
    }
}

pub fn audio_driver_bound(location: &UsbLocation) -> bool {
    PathBuf::from(format!("/sys/bus/usb/devices/{}:1.0/driver", location.sysname)).exists()
}

/// Number of the ALSA card whose usbid is 0fd9:007d.
pub fn find_alsa_card() -> Option<u32> {
    for entry in fs::read_dir("/proc/asound").ok()?.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        let Some(number) = name.strip_prefix("card").and_then(|n| n.parse::<u32>().ok()) else {
            continue;
        };
        if read_attr(&format!("/proc/asound/{name}/usbid")).is_ok_and(|id| id == protocol::USB_ID) {
            return Some(number);
        }
    }
    None
}

/// Open usbfs handle with interface 3 claimed. One per device incarnation.
struct VendorLink {
    fd: OwnedFd,
}

impl VendorLink {
    fn open(location: &UsbLocation) -> Result<Self, DeviceError> {
        let file = fs::OpenOptions::new().read(true).write(true).open(&location.devnode).map_err(|error| {
            if error.kind() == io::ErrorKind::PermissionDenied {
                DeviceError(PERMISSION_HINT.into())
            } else {
                DeviceError(format!("cannot open {}: {error}", location.devnode))
            }
        })?;
        let fd = OwnedFd::from(file);
        let mut interface: u32 = VENDOR_INTERFACE;
        // SAFETY: valid fd, and the argument is a pointer to a u32 as USBDEVFS_CLAIMINTERFACE expects.
        let result = unsafe { libc::ioctl(fd.as_raw_fd(), USBDEVFS_CLAIMINTERFACE as _, &mut interface as *mut u32) };
        if result < 0 {
            return Err(DeviceError(format!("cannot claim the control interface: {}", io::Error::last_os_error())));
        }
        Ok(VendorLink { fd })
    }

    fn transfer(&self, request_type: u8, request: u8, block: u16, data: &mut [u8]) -> Result<usize, DeviceError> {
        let mut control = CtrlTransfer {
            request_type,
            request,
            value: block,
            index: W_INDEX,
            length: data.len() as u16, // at most 34 bytes
            timeout: TIMEOUT_MS,
            data: data.as_mut_ptr().cast(),
        };
        // SAFETY: valid fd; `control` matches struct usbdevfs_ctrltransfer and `data` outlives the call.
        let got = unsafe { libc::ioctl(self.fd.as_raw_fd(), USBDEVFS_CONTROL as _, &mut control as *mut CtrlTransfer) };
        if got < 0 {
            return Err(DeviceError(format!("USB transfer failed: {}", io::Error::last_os_error())));
        }
        Ok(got as usize)
    }

    fn read_config(&self) -> Result<Vec<u8>, DeviceError> {
        let mut buffer = vec![0u8; protocol::CONFIG_LEN];
        let got = self.transfer(REQ_READ, B_READ, BLOCK_CONFIG, &mut buffer)?;
        buffer.truncate(got);
        Ok(buffer)
    }

    fn write_config(&self, block: &[u8]) -> Result<(), DeviceError> {
        let mut buffer = block.to_vec();
        self.transfer(REQ_WRITE, B_WRITE, BLOCK_CONFIG, &mut buffer).map(|_| ())
    }
}

// --- ALSA mixer --------------------------------------------------------------------------------

const GAIN: &str = "Mic Capture Volume";
const MUTE: &str = "Mic Capture Switch";
const HEADPHONE: &str = "PCM Playback Volume";

/// The kernel's mirror of the device controls. Writes through here keep PipeWire and KDE in step.
struct AlsaMixer {
    card: u32,
}

/// One open handle on the card's controls, for one operation.
struct Controls {
    hctl: HCtl,
}

fn alsa_error(what: &str, error: alsa::Error) -> DeviceError {
    DeviceError(format!("ALSA {what}: {error}"))
}

impl Controls {
    fn open(card: u32) -> Result<Self, DeviceError> {
        let hctl = HCtl::new(&format!("hw:{card}"), false).map_err(|e| alsa_error("open", e))?;
        hctl.load().map_err(|e| alsa_error("load", e))?;
        Ok(Controls { hctl })
    }

    fn id(name: &str) -> ElemId {
        let mut id = ElemId::new(ElemIface::Mixer);
        id.set_name(&CString::new(name).expect("control names have no NUL"));
        id
    }

    fn get(&self, name: &str) -> Result<alsa::ctl::ElemValue, DeviceError> {
        let elem = self.hctl.find_elem(&Self::id(name)).ok_or_else(|| DeviceError(format!("no ALSA control {name}")))?;
        elem.read().map_err(|e| alsa_error(name, e))
    }

    fn set<F: Fn(&mut alsa::ctl::ElemValue, u32)>(&self, name: &str, put: F) -> Result<(), DeviceError> {
        let elem = self.hctl.find_elem(&Self::id(name)).ok_or_else(|| DeviceError(format!("no ALSA control {name}")))?;
        let channels = elem.info().map_err(|e| alsa_error(name, e))?.get_count();
        let mut value = elem.read().map_err(|e| alsa_error(name, e))?;
        for channel in 0..channels {
            put(&mut value, channel);
        }
        elem.write(&value).map_err(|e| alsa_error(name, e))?;
        Ok(())
    }

    fn integer(&self, name: &str) -> Result<i64, DeviceError> {
        self.get(name)?.get_integer(0).map(i64::from).ok_or_else(|| DeviceError(format!("cannot read the ALSA control {name}")))
    }

    fn is_muted(&self) -> Result<bool, DeviceError> {
        self.get(MUTE)?.get_boolean(0).map(|on| !on).ok_or_else(|| DeviceError(format!("cannot read the ALSA control {MUTE}")))
    }

    fn gain_steps(&self) -> Result<i64, DeviceError> {
        self.integer(GAIN)
    }

    fn headphone_steps(&self) -> Result<i64, DeviceError> {
        self.integer(HEADPHONE)
    }

    fn set_gain(&self, gain_db: f64) -> Result<(), DeviceError> {
        let steps = protocol::alsa_gain_steps(gain_db) as i32;
        self.set(GAIN, |value, channel| {
            value.set_integer(channel, steps);
        })
    }

    fn set_headphone(&self, headphone_db: f64) -> Result<(), DeviceError> {
        let steps = protocol::alsa_headphone_steps(headphone_db) as i32;
        self.set(HEADPHONE, |value, channel| {
            value.set_integer(channel, steps);
        })
    }

    fn set_muted(&self, muted: bool) -> Result<(), DeviceError> {
        self.set(MUTE, |value, channel| {
            value.set_boolean(channel, !muted);
        })
    }
}

impl AlsaMixer {
    fn controls(&self) -> Result<Controls, DeviceError> {
        Controls::open(self.card)
    }

    fn ready(&self) -> bool {
        self.controls().is_ok_and(|c| c.gain_steps().is_ok() && c.headphone_steps().is_ok())
    }
}

// --- the real device ---------------------------------------------------------------------------

#[derive(Default)]
pub struct Hardware {
    link: Option<VendorLink>,
    link_node: Option<String>,
    mixer: Option<AlsaMixer>,
}

impl Hardware {
    pub fn new() -> Self {
        Self::default()
    }

    fn drop_all(&mut self) {
        self.link = None;
        self.link_node = None;
        self.mixer = None;
    }

    fn vendor(&mut self) -> Result<&VendorLink, DeviceError> {
        let Some(location) = find_usb() else {
            self.drop_all();
            return Err(DeviceError("device is gone".into()));
        };
        if self.link.is_none() || self.link_node.as_deref() != Some(location.devnode.as_str()) {
            self.link = Some(VendorLink::open(&location)?);
            self.link_node = Some(location.devnode);
        }
        Ok(self.link.as_ref().expect("link was just set"))
    }

    /// Runs `action` on the vendor link, once more on a fresh handle if it fails.
    fn vendor_call<R>(&mut self, action: impl Fn(&VendorLink) -> Result<R, DeviceError>) -> Result<R, DeviceError> {
        match self.vendor().and_then(&action) {
            Ok(result) => Ok(result),
            Err(_) => {
                self.link = None;
                self.vendor().and_then(&action)
            }
        }
    }

    // The device block is the truth. The kernel keeps its own copy of the ALSA values, does not notice dial
    // turns, and rounds to 0.5 dB. So tell the ALSA mixer first (KDE and PipeWire read it), then write the
    // exact value into the block last.

    fn mirror(&self, update: impl FnOnce(&Controls) -> Result<(), DeviceError>) {
        // A failing mirror is fine: the hardware gets the value from the block write anyway.
        if let Some(mixer) = &self.mixer {
            let _ = mixer.controls().and_then(|controls| update(&controls));
        }
    }

    fn write_patched(&mut self, patch: Patch) -> Result<(), DeviceError> {
        self.vendor_call(|link| {
            let block = protocol::patch_config(&link.read_config()?, patch)?;
            link.write_config(&block)
        })
    }
}

impl Backend for Hardware {
    fn present(&mut self) -> bool {
        let Some(location) = find_usb() else {
            self.drop_all();
            return false;
        };
        if !audio_driver_bound(&location) {
            self.drop_all();
            return false;
        }
        let Some(card) = find_alsa_card() else {
            return false;
        };
        if self.mixer.as_ref().map(|m| m.card) != Some(card) {
            self.mixer = Some(AlsaMixer { card });
        }
        true
    }

    fn mixer_ready(&mut self) -> bool {
        self.mixer.as_ref().is_some_and(AlsaMixer::ready)
    }

    fn read(&mut self) -> Result<Levels, DeviceError> {
        let block = self.vendor_call(VendorLink::read_config)?;
        Ok(protocol::decode_config(&block)?)
    }

    fn set_gain(&mut self, gain_db: f64) -> Result<(), DeviceError> {
        self.mirror(|c| c.set_gain(gain_db));
        self.write_patched(Patch { gain_db: Some(gain_db), ..Patch::default() })
    }

    fn set_headphone(&mut self, headphone_db: f64) -> Result<(), DeviceError> {
        self.mirror(|c| c.set_headphone(headphone_db));
        self.write_patched(Patch { headphone_db: Some(headphone_db), ..Patch::default() })
    }

    fn set_muted(&mut self, muted: bool) -> Result<(), DeviceError> {
        self.mirror(|c| c.set_muted(muted));
        self.write_patched(Patch { muted: Some(muted), ..Patch::default() })
    }

    fn set_phantom(&mut self, on: bool) -> Result<(), DeviceError> {
        self.write_patched(Patch { phantom: Some(on), ..Patch::default() })
    }

    fn set_low_z(&mut self, on: bool) -> Result<(), DeviceError> {
        self.write_patched(Patch { low_z: Some(on), ..Patch::default() })
    }

    fn set_clipguard(&mut self, on: bool) -> Result<(), DeviceError> {
        self.write_patched(Patch { clipguard: Some(on), ..Patch::default() })
    }

    fn set_gain_lock(&mut self, on: bool) -> Result<(), DeviceError> {
        self.write_patched(Patch { gain_lock: Some(on), ..Patch::default() })
    }

    fn set_mic_mix(&mut self, percent: f64) -> Result<(), DeviceError> {
        self.write_patched(Patch { mic_mix_percent: Some(percent), ..Patch::default() })
    }

    fn device_info(&mut self) -> DeviceInfo {
        match find_usb() {
            Some(location) => read_device_info(&format!("/sys/bus/usb/devices/{}", location.sysname)),
            None => DeviceInfo { name: DEVICE_NAME.to_string(), firmware: String::new(), serial: String::new() },
        }
    }

    /// After a change at the device itself: bring the ALSA mixer to the real values, only where it differs.
    fn sync_mirror(&mut self, levels: &Levels) {
        let Some(mixer) = &self.mixer else { return };
        let sync = || -> Result<bool, DeviceError> {
            let controls = mixer.controls()?;
            let stale = (
                controls.gain_steps()? != protocol::alsa_gain_steps(levels.gain_db),
                controls.headphone_steps()? != protocol::alsa_headphone_steps(levels.headphone_db),
                controls.is_muted()? != levels.muted,
            );
            if stale.0 {
                controls.set_gain(levels.gain_db)?;
            }
            if stale.1 {
                controls.set_headphone(levels.headphone_db)?;
            }
            if stale.2 {
                controls.set_muted(levels.muted)?;
            }
            Ok(stale.0 || stale.1 || stale.2)
        };
        // The ALSA write makes the kernel write its rounded copy into the device, so put the exact block back.
        if let Ok(true) = sync() {
            let patch = Patch {
                gain_db: Some(levels.gain_db),
                headphone_db: Some(levels.headphone_db),
                muted: Some(levels.muted),
                ..Patch::default()
            };
            if let Err(error) = self.write_patched(patch) {
                eprintln!("wavexlr: could not write the exact levels back: {error}");
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ioctl_numbers_match_the_kernel_headers() {
        assert_eq!(USBDEVFS_CONTROL, 0xC018_5500);
        assert_eq!(USBDEVFS_CLAIMINTERFACE, 0x8004_550F);
    }

    #[test]
    fn control_transfer_struct_has_the_kernel_size() {
        assert_eq!(std::mem::size_of::<CtrlTransfer>(), 24);
    }

    #[test]
    fn firmware_is_shown_as_major_minor_patch() {
        assert_eq!(format_firmware("0135"), "1.3.5");
        assert_eq!(format_firmware("0200"), "2.0.0");
        assert_eq!(format_firmware("1042"), "10.4.2");
        // Unexpected formats stay as they are.
        assert_eq!(format_firmware("1.35"), "1.35");
        assert_eq!(format_firmware("013"), "013");
        assert_eq!(format_firmware("01a5"), "01a5");
        assert_eq!(format_firmware(""), "");
    }

    #[test]
    fn device_info_comes_from_sysfs_files() {
        let dir = std::env::temp_dir().join(format!("wavexlr-test-{}-sysfs", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let base = dir.to_string_lossy().into_owned();
        // Nothing readable: empty strings, the name is fixed.
        let info = read_device_info(&base);
        assert_eq!((info.name.as_str(), info.firmware.as_str(), info.serial.as_str()), ("Wave XLR", "", ""));
        fs::write(dir.join("bcdDevice"), "0135\n").unwrap();
        fs::write(dir.join("serial"), "BCA123\n").unwrap();
        let info = read_device_info(&base);
        assert_eq!(info, DeviceInfo { name: "Wave XLR".into(), firmware: "1.3.5".into(), serial: "BCA123".into() });
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn permission_hint_names_the_fix() {
        assert!(PERMISSION_HINT.contains("install.sh") && PERMISSION_HINT.contains("replug"));
    }
}
