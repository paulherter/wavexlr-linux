//! Config block of the Elgato Wave XLR (0fd9:007d), see DEVELOPING.md.

pub const VENDOR_ID: u16 = 0x0FD9;
pub const PRODUCT_ID: u16 = 0x007D;
pub const USB_ID: &str = "0fd9:007d";

pub const CONFIG_LEN: usize = 34;

/// The ALSA mirror stops here; the vendor block allows 80 dB.
pub const GAIN_MAX_DB: f64 = 75.0;
pub const HEADPHONE_MIN_DB: f64 = -60.0;
pub const HEADPHONE_MAX_DB: f64 = 0.0;

const GAIN: usize = 0;
const MUTE: usize = 4;
const CLIPGUARD: usize = 5;
const PHANTOM: usize = 6;
const HEADPHONE: usize = 9;
const MIC_MIX: usize = 12;
const GAIN_LOCK: usize = 28;
const LOW_Z: usize = 33;

/// Share of the microphone in the headphone mix, in percent. 0 = only PC sound (the factory default).
pub const MIC_MIX_MIN: f64 = 0.0;
pub const MIC_MIX_MAX: f64 = 100.0;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Levels {
    pub gain_db: f64,
    pub headphone_db: f64,
    pub muted: bool,
    pub phantom: bool,
    pub low_z: bool,
    pub clipguard: bool,
    pub gain_lock: bool,
    /// 0..=100, stored in the block as Q8.8 (uint16 / 256).
    pub mic_mix_percent: f64,
}

/// Fields to change in a config block. `None` keeps the byte as it is.
#[derive(Clone, Copy, Debug, Default)]
pub struct Patch {
    pub gain_db: Option<f64>,
    pub headphone_db: Option<f64>,
    pub muted: Option<bool>,
    pub phantom: Option<bool>,
    pub low_z: Option<bool>,
    pub clipguard: Option<bool>,
    pub gain_lock: Option<bool>,
    pub mic_mix_percent: Option<f64>,
}

fn check_len(block: &[u8]) -> Result<(), String> {
    if block.len() == CONFIG_LEN {
        Ok(())
    } else {
        Err(format!("config block has {} bytes, expected {CONFIG_LEN}", block.len()))
    }
}

pub fn decode_config(block: &[u8]) -> Result<Levels, String> {
    check_len(block)?;
    let gain = u16::from_le_bytes([block[GAIN], block[GAIN + 1]]);
    let headphone = i16::from_le_bytes([block[HEADPHONE], block[HEADPHONE + 1]]);
    let mix = u16::from_le_bytes([block[MIC_MIX], block[MIC_MIX + 1]]);
    Ok(Levels {
        gain_db: f64::from(gain) / 256.0,
        headphone_db: f64::from(headphone) / 256.0,
        muted: block[MUTE] != 0,
        phantom: block[PHANTOM] != 0,
        low_z: block[LOW_Z] != 0,
        clipguard: block[CLIPGUARD] != 0,
        gain_lock: block[GAIN_LOCK] != 0,
        mic_mix_percent: clamp_mic_mix(f64::from(mix) / 256.0),
    })
}

/// Returns a copy of the block with the given fields changed. Every other byte is kept as it is.
pub fn patch_config(block: &[u8], patch: Patch) -> Result<Vec<u8>, String> {
    check_len(block)?;
    let mut out = block.to_vec();
    if let Some(db) = patch.gain_db {
        // Clamped to 0..=75 dB first, so the cast cannot overflow.
        let raw = (clamp(db, 0.0, GAIN_MAX_DB) * 256.0).round_ties_even() as u16;
        out[GAIN..GAIN + 2].copy_from_slice(&raw.to_le_bytes());
    }
    if let Some(db) = patch.headphone_db {
        let raw = (clamp(db, HEADPHONE_MIN_DB, HEADPHONE_MAX_DB) * 256.0).round_ties_even() as i16;
        out[HEADPHONE..HEADPHONE + 2].copy_from_slice(&raw.to_le_bytes());
    }
    if let Some(on) = patch.muted {
        out[MUTE] = u8::from(on);
    }
    if let Some(on) = patch.phantom {
        out[PHANTOM] = u8::from(on);
    }
    if let Some(on) = patch.low_z {
        out[LOW_Z] = u8::from(on);
    }
    if let Some(on) = patch.clipguard {
        out[CLIPGUARD] = u8::from(on);
    }
    if let Some(on) = patch.gain_lock {
        out[GAIN_LOCK] = u8::from(on);
    }
    if let Some(percent) = patch.mic_mix_percent {
        // Clamped to 0..=100 first (at most 0x6400), so the cast cannot overflow.
        let raw = (clamp_mic_mix(percent) * 256.0).round_ties_even() as u16;
        out[MIC_MIX..MIC_MIX + 2].copy_from_slice(&raw.to_le_bytes());
    }
    Ok(out)
}

pub fn clamp(value: f64, low: f64, high: f64) -> f64 {
    value.max(low).min(high)
}

/// The mic/PC mix limited to its range, 0..=100 percent.
pub fn clamp_mic_mix(percent: f64) -> f64 {
    clamp(percent, MIC_MIX_MIN, MIC_MIX_MAX)
}

/// ALSA 'Mic Capture Volume' counts 0.5 dB steps from 0 to 150.
pub fn alsa_gain_steps(gain_db: f64) -> i64 {
    (clamp(gain_db, 0.0, GAIN_MAX_DB) * 2.0).round_ties_even() as i64
}

/// ALSA 'PCM Playback Volume' counts 0.5 dB steps from 0 (-60 dB) to 120 (0 dB).
pub fn alsa_headphone_steps(headphone_db: f64) -> i64 {
    ((clamp(headphone_db, HEADPHONE_MIN_DB, HEADPHONE_MAX_DB) - HEADPHONE_MIN_DB) * 2.0).round_ties_even() as i64
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Power-on state seen on the real device (DEVELOPING.md).
    fn sample() -> Vec<u8> {
        "00 4b 00 ec 00 01 01 00 00 80 e8 00 00 00 01 ff 00 00 ff ff ff ff ff ff ff ff ff 01 00 ff 37 00 01 00"
            .split_whitespace()
            .map(|byte| u8::from_str_radix(byte, 16).unwrap())
            .collect()
    }

    fn changed(a: &[u8], b: &[u8]) -> Vec<usize> {
        (0..a.len()).filter(|&i| a[i] != b[i]).collect()
    }

    #[test]
    fn decode_sample_from_the_real_device() {
        let levels = decode_config(&sample()).unwrap();
        assert_eq!(levels.gain_db, 75.0);
        assert_eq!(levels.headphone_db, -23.5);
        assert!(levels.phantom && !levels.muted && !levels.low_z);
        // Power-on state: Clipguard on, gain not locked, mix all PC sound.
        assert!(levels.clipguard && !levels.gain_lock);
        assert_eq!(levels.mic_mix_percent, 0.0);
    }

    #[test]
    fn decode_clipguard_gain_lock_and_mix() {
        let mut block = sample();
        block[5] = 0;
        block[28] = 1;
        block[12..14].copy_from_slice(&[0x00, 0x50]); // captured: 80 %
        let levels = decode_config(&block).unwrap();
        assert!(!levels.clipguard && levels.gain_lock);
        assert_eq!(levels.mic_mix_percent, 80.0);
        block[12..14].copy_from_slice(&[0x00, 0x64]);
        assert_eq!(decode_config(&block).unwrap().mic_mix_percent, 100.0);
        block[12..14].copy_from_slice(&[0x80, 0x32]); // 50.5 %
        assert_eq!(decode_config(&block).unwrap().mic_mix_percent, 50.5);
        block[12..14].copy_from_slice(&[0xff, 0xff]); // out of range reads as 100 %
        assert_eq!(decode_config(&block).unwrap().mic_mix_percent, 100.0);
    }

    #[test]
    fn patch_new_fields_changes_only_their_bytes() {
        let s = sample();
        let out = patch_config(&s, Patch { clipguard: Some(false), ..Patch::default() }).unwrap();
        assert_eq!(changed(&s, &out), vec![5]);
        let out = patch_config(&s, Patch { gain_lock: Some(true), ..Patch::default() }).unwrap();
        assert_eq!(changed(&s, &out), vec![28]);
        let out = patch_config(&s, Patch { mic_mix_percent: Some(80.0), ..Patch::default() }).unwrap();
        assert_eq!(changed(&s, &out), vec![13]);
        assert_eq!(&out[12..14], &[0x00, 0x50]);
        let out = patch_config(&s, Patch { mic_mix_percent: Some(33.0), ..Patch::default() }).unwrap();
        assert_eq!(&out[12..14], &[0x00, 0x21]);
        assert_eq!(decode_config(&out).unwrap().mic_mix_percent, 33.0);
        let all = Patch { clipguard: Some(false), gain_lock: Some(true), mic_mix_percent: Some(100.0), ..Patch::default() };
        let out = patch_config(&s, all).unwrap();
        assert_eq!(changed(&s, &out), vec![5, 13, 28]);
        assert_eq!(&out[12..14], &[0x00, 0x64]);
    }

    #[test]
    fn mic_mix_is_clamped_and_keeps_fractions_exactly() {
        let s = sample();
        let low = patch_config(&s, Patch { mic_mix_percent: Some(-5.0), ..Patch::default() }).unwrap();
        assert_eq!(&low[12..14], &[0, 0]);
        let high = patch_config(&s, Patch { mic_mix_percent: Some(250.0), ..Patch::default() }).unwrap();
        assert_eq!(&high[12..14], &[0x00, 0x64]);
        let half = patch_config(&s, Patch { mic_mix_percent: Some(12.5), ..Patch::default() }).unwrap();
        assert_eq!(&half[12..14], &[0x80, 0x0c]);
        assert_eq!(decode_config(&half).unwrap().mic_mix_percent, 12.5);
        assert_eq!(clamp_mic_mix(101.0), 100.0);
        assert_eq!(clamp_mic_mix(-1.0), 0.0);
    }

    #[test]
    fn patch_changes_only_the_asked_byte() {
        let s = sample();
        let out = patch_config(&s, Patch { phantom: Some(false), ..Patch::default() }).unwrap();
        assert_eq!(changed(&s, &out), vec![6]);
        let out = patch_config(&s, Patch { low_z: Some(true), ..Patch::default() }).unwrap();
        assert_eq!(out[33], 1);
    }

    #[test]
    fn alsa_steps() {
        assert_eq!(alsa_gain_steps(40.0), 80);
        assert_eq!(alsa_gain_steps(99.0), 150);
        assert_eq!(alsa_headphone_steps(-23.5), 73);
        assert_eq!(alsa_headphone_steps(0.0), 120);
    }

    #[test]
    fn patch_gain_headphone_mute() {
        let s = sample();
        let patch = Patch { gain_db: Some(40.0), headphone_db: Some(-30.0), muted: Some(true), ..Patch::default() };
        let out = patch_config(&s, patch).unwrap();
        let levels = decode_config(&out).unwrap();
        assert_eq!((levels.gain_db, levels.headphone_db, levels.muted), (40.0, -30.0, true));
        assert_eq!(changed(&s, &out), vec![1, 4, 9, 10]);
    }

    #[test]
    fn wrong_length_is_refused() {
        assert!(decode_config(&[0; 10]).is_err());
        assert!(patch_config(&[0; 35], Patch::default()).is_err());
    }

    #[test]
    fn steps_round_half_to_even_like_the_python_version() {
        assert_eq!(alsa_gain_steps(36.25), 72); // 72.5 -> 72
        assert_eq!(alsa_gain_steps(36.75), 74); // 73.5 -> 74
    }
}
