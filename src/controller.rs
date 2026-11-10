//! Device state machine and the restore/save logic. No sleeping in here, time comes in through `tick()`.

use crate::protocol::{self, Levels};
use crate::settings::Saved;
use crate::transport::{Backend, DeviceError, DeviceInfo};

pub use crate::ipc::StateName as State;

/// Let snd-usb-audio finish its own start-up writes first.
pub const SETTLE_SECONDS: f64 = 1.0;
/// After a restore, undo late resets by the driver.
pub const GUARD_SECONDS: f64 = 3.0;
pub const RETRY_SECONDS: f64 = 5.0;
pub const MAX_RESTORE_TRIES: u32 = 3;
pub const MAX_READ_FAILURES: u32 = 3;
pub const TOLERANCE_DB: f64 = 0.5;
/// The mix is stored as Q8.8, so anything closer than this is the same value.
pub const MIX_TOLERANCE: f64 = 0.1;
/// A dial that stood still this long counts as finished.
pub const MIRROR_AFTER_SECONDS: f64 = 1.0;
/// While the gain lock holds, a gain further than this from the locked value is put back.
pub const LOCK_TOLERANCE_DB: f64 = 0.1;
/// While polling fast, the (comparatively costly) presence scan runs only this often.
const PRESENT_CHECK_SECONDS: f64 = 0.25;

/// One variant per state, each carrying exactly the data that is valid in it.
enum Phase {
    Absent,
    Initializing {
        /// When the current settle phase began.
        since: f64,
        tries: u32,
    },
    Ready {
        levels: Levels,
        since: f64,
        /// When the levels were last restored; the guard window runs from here.
        applied_at: f64,
        /// Last change at the device that ALSA has not seen yet.
        changed_at: Option<f64>,
        read_failures: u32,
    },
    Error {
        message: String,
        since: f64,
    },
}

fn same_levels(levels: &Levels, saved: &Saved) -> bool {
    (levels.gain_db - saved.gain_db).abs() <= TOLERANCE_DB
        && (levels.headphone_db - saved.headphone_db).abs() <= TOLERANCE_DB
        && levels.muted == saved.muted
        && saved.phantom.is_none_or(|p| levels.phantom == p)
        && saved.low_z.is_none_or(|z| levels.low_z == z)
        && saved.clipguard.is_none_or(|c| levels.clipguard == c)
        && saved.gain_lock.is_none_or(|g| levels.gain_lock == g)
        && saved.mic_mix.is_none_or(|m| (levels.mic_mix_percent - m).abs() <= MIX_TOLERANCE)
}

fn snapshot(levels: &Levels) -> Saved {
    Saved::new(levels.gain_db, levels.headphone_db, levels.muted)
}

/// A command from the window, applied only in the Ready state.
#[derive(Clone, Copy, Debug)]
enum Cmd {
    Gain(f64),
    Headphone(f64),
    Muted(bool),
    Phantom(bool),
    LowZ(bool),
    Clipguard(bool),
    GainLock(bool),
    MicMix(f64),
}

pub struct Controller<B: Backend> {
    backend: B,
    store: Box<dyn FnMut(&Saved)>,
    phase: Phase,
    saved: Option<Saved>,
    /// Read from sysfs each time the device becomes Ready.
    info: Option<DeviceInfo>,
    /// Counts every change a viewer could see (state, error text or levels).
    revision: u64,
    /// The firmware only stores the gain lock flag, so the lock is enforced here: while this is `Some` (and the
    /// state is Ready) every gain the dial changes is put back. Cleared on any state change.
    locked_gain: Option<f64>,
    /// When the presence of the device was last checked.
    present_at: f64,
}

impl<B: Backend> Controller<B> {
    pub fn new(backend: B, saved: Option<Saved>, store: Box<dyn FnMut(&Saved)>) -> Self {
        Controller {
            backend,
            store,
            phase: Phase::Absent,
            saved,
            info: None,
            revision: 0,
            locked_gain: None,
            present_at: f64::NEG_INFINITY,
        }
    }

    pub fn state(&self) -> State {
        match self.phase {
            Phase::Absent => State::Absent,
            Phase::Initializing { .. } => State::Initializing,
            Phase::Ready { .. } => State::Ready,
            Phase::Error { .. } => State::Error,
        }
    }

    /// Plain English, empty unless the state is Error.
    pub fn error(&self) -> &str {
        match &self.phase {
            Phase::Error { message, .. } => message,
            _ => "",
        }
    }

    /// `Some` only in the Ready state.
    pub fn levels(&self) -> Option<Levels> {
        match &self.phase {
            Phase::Ready { levels, .. } => Some(*levels),
            _ => None,
        }
    }

    /// `Some` only in the Ready state.
    pub fn device_info(&self) -> Option<DeviceInfo> {
        match &self.phase {
            Phase::Ready { .. } => self.info.clone(),
            _ => None,
        }
    }

    /// Bumps whenever the state, the error text or the levels changed.
    pub fn revision(&self) -> u64 {
        self.revision
    }

    /// True once the Ready state is old enough that the restore guard is over, so ticking can slow down.
    pub fn settled(&self, now: f64) -> bool {
        matches!(&self.phase, Phase::Ready { since, .. } if now - since > GUARD_SECONDS + MIRROR_AFTER_SECONDS)
    }

    /// True while the gain lock is being enforced: the daemon polls fast then, so a dial turn is undone quickly.
    pub fn wants_fast_poll(&self) -> bool {
        self.locked_gain.is_some() && matches!(self.phase, Phase::Ready { .. })
    }

    fn go(&mut self, phase: Phase) {
        self.locked_gain = None;
        self.phase = phase;
        self.revision += 1;
    }

    fn save(&mut self) {
        if let Some(saved) = &self.saved {
            (self.store)(saved);
        }
    }

    /// Called after resume from sleep (and by the Retry command): the driver may have touched the levels.
    pub fn restore_again(&mut self, now: f64) {
        if matches!(self.phase, Phase::Ready { .. } | Phase::Error { .. }) {
            self.go(Phase::Initializing { since: now - SETTLE_SECONDS / 2.0, tries: 0 });
        }
    }

    pub fn tick(&mut self, now: f64) {
        let present = if self.wants_fast_poll() && now - self.present_at < PRESENT_CHECK_SECONDS {
            true // a failing read tells us soon enough when the device is gone
        } else {
            self.present_at = now;
            self.backend.present()
        };
        match &self.phase {
            Phase::Absent => {
                if present {
                    self.go(Phase::Initializing { since: now, tries: 0 });
                }
            }
            _ if !present => self.go(Phase::Absent),
            Phase::Initializing { since, tries } => {
                let (since, tries) = (*since, *tries);
                self.initialize(now, since, tries);
            }
            Phase::Ready { .. } => self.watch(now),
            Phase::Error { since, .. } => {
                if now - since >= RETRY_SECONDS {
                    self.go(Phase::Initializing { since: now - SETTLE_SECONDS, tries: 0 });
                }
            }
        }
    }

    fn initialize(&mut self, now: f64, since: f64, tries: u32) {
        if now - since < SETTLE_SECONDS || !self.backend.mixer_ready() {
            return;
        }
        match self.restore() {
            Ok(levels) => {
                self.info = Some(self.backend.device_info());
                self.go(Phase::Ready { levels, since: now, applied_at: now, changed_at: Some(now), read_failures: 0 });
                // The saved gain is applied by now: lock at that gain.
                self.locked_gain = levels.gain_lock.then_some(levels.gain_db);
            }
            Err(error) => {
                let tries = tries + 1;
                if tries >= MAX_RESTORE_TRIES {
                    self.go(Phase::Error { message: error.to_string(), since: now });
                } else {
                    // Settle again, then retry. Nothing a viewer sees has changed.
                    self.phase = Phase::Initializing { since: now, tries };
                }
            }
        }
    }

    /// Reads the device and brings it to the saved levels. Returns what the device has afterwards.
    fn restore(&mut self) -> Result<Levels, DeviceError> {
        let mut levels = self.backend.read()?;
        match self.saved.clone() {
            None => {
                // First start: nothing to restore, remember what the device has now.
                self.saved = Some(snapshot(&levels));
                self.save();
            }
            Some(saved) if !same_levels(&levels, &saved) => {
                self.apply(&saved)?;
                levels = self.backend.read()?;
                if !same_levels(&levels, &saved) {
                    return Err(DeviceError("the device did not take the saved levels".into()));
                }
            }
            Some(_) => {}
        }
        Ok(levels)
    }

    fn apply(&mut self, saved: &Saved) -> Result<(), DeviceError> {
        self.backend.set_gain(saved.gain_db)?;
        self.backend.set_headphone(saved.headphone_db)?;
        self.backend.set_muted(saved.muted)?;
        if let Some(on) = saved.phantom {
            self.backend.set_phantom(on)?;
        }
        if let Some(on) = saved.low_z {
            self.backend.set_low_z(on)?;
        }
        if let Some(on) = saved.clipguard {
            self.backend.set_clipguard(on)?;
        }
        if let Some(on) = saved.gain_lock {
            self.backend.set_gain_lock(on)?;
        }
        if let Some(percent) = saved.mic_mix {
            self.backend.set_mic_mix(percent)?;
        }
        Ok(())
    }

    fn watch(&mut self, now: f64) {
        let Phase::Ready { levels: known, applied_at, changed_at, read_failures, since } = &self.phase else {
            return;
        };
        let (known, applied_at, changed_at, mut failures, since) = (*known, *applied_at, *changed_at, *read_failures, *since);
        let mut levels = match self.backend.read() {
            Ok(levels) => levels,
            Err(error) => {
                failures += 1;
                if failures >= MAX_READ_FAILURES {
                    self.go(Phase::Error { message: error.to_string(), since: now });
                } else {
                    self.phase = Phase::Ready { levels: known, since, applied_at, changed_at, read_failures: failures };
                }
                return;
            }
        };
        if now - applied_at < GUARD_SECONDS {
            if let Some(saved) = self.saved.clone() {
                if !same_levels(&levels, &saved) {
                    match self.apply(&saved).and_then(|()| self.backend.read()) {
                        Ok(again) => levels = again,
                        Err(_) => {
                            self.phase = Phase::Ready { levels: known, since, applied_at, changed_at, read_failures: 0 };
                            return;
                        }
                    }
                }
            }
        }
        self.hold_lock(&mut levels);
        if levels != known {
            self.remember(&levels);
            self.phase = Phase::Ready { levels, since, applied_at, changed_at: Some(now), read_failures: 0 };
            self.revision += 1;
        } else if changed_at.is_some_and(|at| now - at >= MIRROR_AFTER_SECONDS) {
            self.phase = Phase::Ready { levels, since, applied_at, changed_at: None, read_failures: 0 };
            self.backend.sync_mirror(&levels);
        } else {
            self.phase = Phase::Ready { levels, since, applied_at, changed_at, read_failures: 0 };
        }
    }

    /// Keeps `locked_gain` in step with the device and undoes a dial turn while the lock is on. The firmware does
    /// not do that itself. `levels` ends up showing the locked gain, so nothing downstream sees the dial's value.
    fn hold_lock(&mut self, levels: &mut Levels) {
        if !levels.gain_lock {
            self.locked_gain = None;
            return;
        }
        let Some(locked) = self.locked_gain else {
            self.locked_gain = Some(levels.gain_db);
            return;
        };
        if (levels.gain_db - locked).abs() > LOCK_TOLERANCE_DB {
            // Same call as an app command: ALSA mirror first, exact block write last. A failure is retried by the
            // next poll.
            if let Err(error) = self.backend.set_gain(locked) {
                eprintln!("wavexlr: cannot hold the gain lock: {error}");
            }
            levels.gain_db = locked;
        }
    }

    /// The device changed by itself (dial, mute touch, another program): keep it as the new saved state.
    fn remember(&mut self, levels: &Levels) {
        let mut saved = self.saved.take().unwrap_or_else(|| snapshot(levels));
        saved.gain_db = levels.gain_db;
        saved.headphone_db = levels.headphone_db;
        saved.muted = levels.muted;
        if saved.phantom.is_some() {
            saved.phantom = Some(levels.phantom);
        }
        if saved.low_z.is_some() {
            saved.low_z = Some(levels.low_z);
        }
        if saved.clipguard.is_some() {
            saved.clipguard = Some(levels.clipguard);
        }
        if saved.gain_lock.is_some() {
            saved.gain_lock = Some(levels.gain_lock);
        }
        if saved.mic_mix.is_some() {
            saved.mic_mix = Some(levels.mic_mix_percent);
        }
        self.saved = Some(saved);
        self.save();
    }

    // --- commands from the window --------------------------------------------------------------

    fn command(&mut self, cmd: Cmd) {
        let Phase::Ready { levels: known, .. } = &self.phase else {
            return;
        };
        let known = *known;
        let result = match cmd {
            Cmd::Gain(db) => self.backend.set_gain(db),
            Cmd::Headphone(db) => self.backend.set_headphone(db),
            Cmd::Muted(on) => self.backend.set_muted(on),
            Cmd::Phantom(on) => self.backend.set_phantom(on).map(|()| self.pin(&known, Cmd::Phantom(on))),
            Cmd::LowZ(on) => self.backend.set_low_z(on).map(|()| self.pin(&known, Cmd::LowZ(on))),
            Cmd::Clipguard(on) => self.backend.set_clipguard(on).map(|()| self.pin(&known, Cmd::Clipguard(on))),
            Cmd::GainLock(on) => self.backend.set_gain_lock(on).map(|()| self.pin(&known, Cmd::GainLock(on))),
            Cmd::MicMix(percent) => {
                let percent = protocol::clamp_mic_mix(percent);
                self.backend.set_mic_mix(percent).map(|()| self.pin(&known, Cmd::MicMix(percent)))
            }
        };
        let mut levels = match result.and_then(|()| self.backend.read()) {
            Ok(levels) => levels,
            Err(error) => {
                // The watch in tick() counts persistent failures; a single refused command changes nothing.
                eprintln!("wavexlr: command failed: {error}");
                return;
            }
        };
        if matches!(cmd, Cmd::Gain(_)) {
            // The lock stops the dial, not the app: the gain the app chose becomes the locked one.
            self.locked_gain = None;
        }
        self.hold_lock(&mut levels);
        self.remember(&levels);
        if let Phase::Ready { levels: current, .. } = &mut self.phase {
            *current = levels;
        }
        self.revision += 1;
    }

    /// From now on the app restores this switch, because the user chose it.
    fn pin(&mut self, known: &Levels, cmd: Cmd) {
        let saved = self.saved.get_or_insert_with(|| snapshot(known));
        match cmd {
            Cmd::Phantom(on) => saved.phantom = Some(on),
            Cmd::LowZ(on) => saved.low_z = Some(on),
            Cmd::Clipguard(on) => saved.clipguard = Some(on),
            Cmd::GainLock(on) => saved.gain_lock = Some(on),
            Cmd::MicMix(percent) => saved.mic_mix = Some(percent),
            _ => {}
        }
    }

    pub fn set_gain(&mut self, gain_db: f64) {
        self.command(Cmd::Gain(gain_db));
    }

    pub fn set_headphone(&mut self, headphone_db: f64) {
        self.command(Cmd::Headphone(headphone_db));
    }

    pub fn set_muted(&mut self, muted: bool) {
        self.command(Cmd::Muted(muted));
    }

    pub fn set_phantom(&mut self, on: bool) {
        self.command(Cmd::Phantom(on));
    }

    pub fn set_low_z(&mut self, on: bool) {
        self.command(Cmd::LowZ(on));
    }

    pub fn set_clipguard(&mut self, on: bool) {
        self.command(Cmd::Clipguard(on));
    }

    pub fn set_gain_lock(&mut self, on: bool) {
        self.command(Cmd::GainLock(on));
    }

    pub fn set_mic_mix(&mut self, percent: f64) {
        self.command(Cmd::MicMix(percent));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    use std::rc::Rc;

    const DRIVER_START: Levels = Levels {
        gain_db: 75.0,
        headphone_db: -23.5,
        muted: false,
        phantom: true,
        low_z: false,
        clipguard: true,
        gain_lock: false,
        mic_mix_percent: 0.0,
    };

    /// Behaves like the real one: every (re)bind of the driver resets gain and headphones.
    struct Device {
        plugged: bool,
        mixer: bool,
        levels: Levels,
        fail_reads: bool,
        synced: Vec<Levels>,
    }

    type Shared = Rc<RefCell<Device>>;

    struct Fake(Shared);

    impl Device {
        fn plug(&mut self) {
            self.plugged = true;
            self.mixer = true;
            self.levels = Levels { gain_db: 75.0, headphone_db: -23.5, muted: false, ..self.levels };
        }

        fn unplug(&mut self) {
            self.plugged = false;
            self.mixer = false;
        }
    }

    impl Backend for Fake {
        fn present(&mut self) -> bool {
            self.0.borrow().plugged
        }
        fn mixer_ready(&mut self) -> bool {
            self.0.borrow().mixer
        }
        fn read(&mut self) -> Result<Levels, DeviceError> {
            let device = self.0.borrow();
            if device.fail_reads {
                Err(DeviceError("boom".into()))
            } else {
                Ok(device.levels)
            }
        }
        fn set_gain(&mut self, v: f64) -> Result<(), DeviceError> {
            self.0.borrow_mut().levels.gain_db = v;
            Ok(())
        }
        fn set_headphone(&mut self, v: f64) -> Result<(), DeviceError> {
            self.0.borrow_mut().levels.headphone_db = v;
            Ok(())
        }
        fn set_muted(&mut self, v: bool) -> Result<(), DeviceError> {
            self.0.borrow_mut().levels.muted = v;
            Ok(())
        }
        fn set_phantom(&mut self, v: bool) -> Result<(), DeviceError> {
            self.0.borrow_mut().levels.phantom = v;
            Ok(())
        }
        fn set_low_z(&mut self, v: bool) -> Result<(), DeviceError> {
            self.0.borrow_mut().levels.low_z = v;
            Ok(())
        }
        fn set_clipguard(&mut self, v: bool) -> Result<(), DeviceError> {
            self.0.borrow_mut().levels.clipguard = v;
            Ok(())
        }
        fn set_gain_lock(&mut self, v: bool) -> Result<(), DeviceError> {
            self.0.borrow_mut().levels.gain_lock = v;
            Ok(())
        }
        fn set_mic_mix(&mut self, v: f64) -> Result<(), DeviceError> {
            self.0.borrow_mut().levels.mic_mix_percent = v;
            Ok(())
        }
        fn device_info(&mut self) -> DeviceInfo {
            DeviceInfo { name: "Wave XLR".into(), firmware: "1.3.5".into(), serial: "TEST123".into() }
        }
        fn sync_mirror(&mut self, levels: &Levels) {
            self.0.borrow_mut().synced.push(*levels);
        }
    }

    struct Rig {
        device: Shared,
        ctl: Controller<Fake>,
        saved: Rc<RefCell<Option<Saved>>>,
    }

    fn make(saved: Option<Saved>) -> Rig {
        let device = Rc::new(RefCell::new(Device {
            plugged: false,
            mixer: false,
            levels: DRIVER_START,
            fail_reads: false,
            synced: Vec::new(),
        }));
        let box_ = Rc::new(RefCell::new(saved.clone()));
        let sink = box_.clone();
        let ctl = Controller::new(Fake(device.clone()), saved, Box::new(move |s| *sink.borrow_mut() = Some(s.clone())));
        Rig { device, ctl, saved: box_ }
    }

    fn standard() -> Rig {
        make(Some(Saved::new(30.0, -35.0, false)))
    }

    fn run(ctl: &mut Controller<Fake>, start: f64, seconds: f64) -> f64 {
        let mut t = start;
        while t < start + seconds {
            ctl.tick(t);
            t += 0.25;
        }
        t
    }

    fn levels(rig: &Rig) -> Levels {
        rig.device.borrow().levels
    }

    /// Ready, lock on at 30 dB, long past the restore guard. Returns the rig and the current time.
    fn locked_rig() -> (Rig, f64) {
        let mut rig = standard();
        rig.device.borrow_mut().plug();
        let t = run(&mut rig.ctl, 0.0, 6.0);
        rig.ctl.set_gain_lock(true);
        (rig, t)
    }

    #[test]
    fn dial_turn_while_locked_is_undone_and_not_saved() {
        let (mut rig, t) = locked_rig();
        assert!(rig.ctl.wants_fast_poll());
        let revision = rig.ctl.revision();
        rig.device.borrow_mut().levels.gain_db = 45.0; // the dial
        rig.ctl.tick(t);
        assert_eq!(levels(&rig).gain_db, 30.0);
        assert_eq!(rig.ctl.levels().unwrap().gain_db, 30.0);
        assert_eq!(rig.saved.borrow().as_ref().unwrap().gain_db, 30.0);
        assert_eq!(rig.ctl.revision(), revision);
        // A small wobble inside the tolerance is left alone.
        rig.device.borrow_mut().levels.gain_db = 30.05;
        rig.ctl.tick(t + 0.05);
        assert_eq!(levels(&rig).gain_db, 30.05);
    }

    #[test]
    fn other_levels_still_follow_the_device_while_locked() {
        let (mut rig, t) = locked_rig();
        rig.device.borrow_mut().levels.gain_db = 31.0;
        rig.device.borrow_mut().levels.headphone_db = -20.0;
        rig.ctl.tick(t);
        assert_eq!(levels(&rig).gain_db, 30.0);
        assert_eq!(rig.saved.borrow().as_ref().unwrap().headphone_db, -20.0);
        assert_eq!(rig.saved.borrow().as_ref().unwrap().gain_db, 30.0);
    }

    #[test]
    fn app_command_while_locked_moves_the_lock() {
        let (mut rig, t) = locked_rig();
        rig.ctl.set_gain(50.0);
        assert_eq!(levels(&rig).gain_db, 50.0);
        assert!(rig.ctl.wants_fast_poll());
        rig.device.borrow_mut().levels.gain_db = 60.0;
        rig.ctl.tick(t);
        assert_eq!(levels(&rig).gain_db, 50.0);
        assert_eq!(rig.saved.borrow().as_ref().unwrap().gain_db, 50.0);
    }

    #[test]
    fn dial_goes_through_when_the_lock_is_off() {
        let (mut rig, t) = locked_rig();
        rig.ctl.set_gain_lock(false);
        assert!(!rig.ctl.wants_fast_poll());
        rig.device.borrow_mut().levels.gain_db = 40.0;
        rig.ctl.tick(t);
        assert_eq!(levels(&rig).gain_db, 40.0);
        assert_eq!(rig.ctl.levels().unwrap().gain_db, 40.0);
        assert_eq!(rig.saved.borrow().as_ref().unwrap().gain_db, 40.0);
    }

    #[test]
    fn restore_applies_the_saved_gain_then_locks_there() {
        let mut saved = Saved::new(30.0, -35.0, false);
        saved.gain_lock = Some(true);
        let mut rig = make(Some(saved));
        rig.device.borrow_mut().plug();
        let t = run(&mut rig.ctl, 0.0, 6.0);
        assert_eq!(rig.ctl.state(), State::Ready);
        assert_eq!(levels(&rig).gain_db, 30.0);
        assert!(rig.ctl.wants_fast_poll());
        rig.device.borrow_mut().levels.gain_db = 45.0;
        rig.ctl.tick(t);
        assert_eq!(levels(&rig).gain_db, 30.0);
        assert_eq!(rig.saved.borrow().as_ref().unwrap().gain_db, 30.0);
    }

    #[test]
    fn leaving_ready_clears_the_lock() {
        let (mut rig, t) = locked_rig();
        rig.device.borrow_mut().unplug();
        rig.ctl.tick(t + 0.25);
        assert_eq!(rig.ctl.state(), State::Absent);
        assert!(!rig.ctl.wants_fast_poll());
        // Back again with the lock restored: it locks at the restored gain.
        rig.device.borrow_mut().plug();
        let t = run(&mut rig.ctl, t + 0.5, 6.0);
        assert!(rig.ctl.wants_fast_poll());
        rig.device.borrow_mut().levels.gain_db = 10.0;
        rig.ctl.tick(t);
        assert_eq!(levels(&rig).gain_db, 30.0);
    }

    #[test]
    fn fast_poll_only_while_locked_and_ready() {
        let mut rig = standard();
        assert!(!rig.ctl.wants_fast_poll());
        rig.device.borrow_mut().plug();
        let t = run(&mut rig.ctl, 0.0, 6.0);
        assert!(!rig.ctl.wants_fast_poll()); // Ready, lock off
        rig.ctl.set_gain_lock(true);
        assert!(rig.ctl.wants_fast_poll());
        rig.device.borrow_mut().fail_reads = true;
        run(&mut rig.ctl, t, 1.0);
        assert_eq!(rig.ctl.state(), State::Error);
        assert!(!rig.ctl.wants_fast_poll());
    }

    #[test]
    fn restores_saved_levels_after_plug() {
        let mut rig = standard();
        rig.device.borrow_mut().plug();
        run(&mut rig.ctl, 0.0, 3.0);
        assert_eq!(rig.ctl.state(), State::Ready);
        assert_eq!((levels(&rig).gain_db, levels(&rig).headphone_db), (30.0, -35.0));
    }

    #[test]
    fn nothing_is_written_before_the_driver_settled() {
        let mut rig = standard();
        rig.device.borrow_mut().plug();
        rig.ctl.tick(0.0);
        rig.ctl.tick(SETTLE_SECONDS - 0.1);
        assert_eq!(levels(&rig).gain_db, 75.0);
        assert_eq!(rig.ctl.state(), State::Initializing);
    }

    #[test]
    fn driver_start_values_are_never_saved() {
        let mut rig = standard();
        rig.device.borrow_mut().plug();
        run(&mut rig.ctl, 0.0, 3.0);
        rig.device.borrow_mut().unplug();
        let t = run(&mut rig.ctl, 3.0, 1.0);
        rig.device.borrow_mut().plug();
        run(&mut rig.ctl, t, 3.0);
        let saved = rig.saved.borrow().clone().unwrap();
        assert!(saved.gain_db == 30.0 && saved.headphone_db == -35.0);
        assert_eq!(levels(&rig).gain_db, 30.0);
    }

    #[test]
    fn late_driver_reset_is_undone_in_the_guard_window() {
        let mut rig = standard();
        rig.device.borrow_mut().plug();
        let t = run(&mut rig.ctl, 0.0, 2.0);
        rig.device.borrow_mut().levels = DRIVER_START; // driver resets again right after the restore
        run(&mut rig.ctl, t, 1.0);
        assert_eq!(levels(&rig).gain_db, 30.0);
    }

    #[test]
    fn hardware_change_after_the_guard_is_saved() {
        let mut rig = standard();
        rig.device.borrow_mut().plug();
        let t = run(&mut rig.ctl, 0.0, 6.0);
        rig.device.borrow_mut().levels.headphone_db = -20.0;
        run(&mut rig.ctl, t, 1.0);
        assert_eq!(rig.saved.borrow().as_ref().unwrap().headphone_db, -20.0);
    }

    #[test]
    fn first_start_remembers_the_device_without_writing() {
        let mut rig = make(None);
        rig.device.borrow_mut().plug();
        run(&mut rig.ctl, 0.0, 3.0);
        assert_eq!(rig.ctl.state(), State::Ready);
        let saved = rig.saved.borrow().clone().unwrap();
        assert!(saved.gain_db == 75.0 && saved.phantom.is_none());
    }

    #[test]
    fn phantom_is_left_alone_until_the_user_sets_it() {
        let mut rig = standard();
        rig.device.borrow_mut().levels = DRIVER_START;
        rig.device.borrow_mut().plug();
        run(&mut rig.ctl, 0.0, 3.0);
        assert!(levels(&rig).phantom); // untouched
        rig.ctl.set_phantom(false);
        assert!(!levels(&rig).phantom);
        rig.device.borrow_mut().unplug();
        let t = run(&mut rig.ctl, 3.0, 1.0);
        rig.device.borrow_mut().levels = DRIVER_START;
        rig.device.borrow_mut().plug();
        run(&mut rig.ctl, t, 3.0);
        assert!(!levels(&rig).phantom); // restored because the user chose it
    }

    #[test]
    fn unplug_goes_to_absent_and_back() {
        let mut rig = standard();
        rig.device.borrow_mut().plug();
        let t = run(&mut rig.ctl, 0.0, 3.0);
        rig.device.borrow_mut().unplug();
        let t = run(&mut rig.ctl, t, 1.0);
        assert_eq!(rig.ctl.state(), State::Absent);
        assert!(rig.ctl.levels().is_none());
        rig.device.borrow_mut().plug();
        run(&mut rig.ctl, t, 3.0);
        assert_eq!(rig.ctl.state(), State::Ready);
    }

    #[test]
    fn restore_failure_ends_in_error_and_retries() {
        let mut rig = standard();
        rig.device.borrow_mut().plug();
        rig.device.borrow_mut().fail_reads = true;
        let t = run(&mut rig.ctl, 0.0, 8.0);
        assert_eq!(rig.ctl.state(), State::Error);
        assert_eq!(rig.ctl.error(), "boom");
        rig.device.borrow_mut().fail_reads = false;
        run(&mut rig.ctl, t, 8.0);
        assert_eq!(rig.ctl.state(), State::Ready);
        assert_eq!(rig.ctl.error(), "");
    }

    #[test]
    fn commands_are_ignored_unless_ready() {
        let mut rig = standard();
        rig.ctl.set_gain(10.0);
        assert_eq!(levels(&rig).gain_db, 75.0);
    }

    #[test]
    fn resume_restores_again() {
        let mut rig = standard();
        rig.device.borrow_mut().plug();
        let t = run(&mut rig.ctl, 0.0, 6.0);
        rig.device.borrow_mut().levels = DRIVER_START;
        rig.ctl.restore_again(t);
        run(&mut rig.ctl, t, 3.0);
        assert_eq!(levels(&rig).gain_db, 30.0);
    }

    #[test]
    fn dial_turn_is_mirrored_into_alsa_after_it_stood_still() {
        let mut rig = standard();
        rig.device.borrow_mut().plug();
        let t = run(&mut rig.ctl, 0.0, 6.0);
        rig.device.borrow_mut().synced.clear(); // the start-up sync is not the point here
        rig.device.borrow_mut().levels.gain_db = 33.0; // someone turns the dial
        let t = run(&mut rig.ctl, t, MIRROR_AFTER_SECONDS - 0.5);
        assert!(rig.device.borrow().synced.is_empty()); // still turning
        run(&mut rig.ctl, t, 2.0);
        let synced = rig.device.borrow().synced.clone();
        assert!(synced.len() == 1 && synced[0].gain_db == 33.0);
    }

    // --- more than the Python tests cover ---

    #[test]
    fn revision_moves_only_when_a_viewer_could_see_a_change() {
        let mut rig = standard();
        rig.device.borrow_mut().plug();
        let t = run(&mut rig.ctl, 0.0, 6.0);
        let revision = rig.ctl.revision();
        run(&mut rig.ctl, t, 3.0);
        assert_eq!(rig.ctl.revision(), revision);
        rig.ctl.set_gain(33.0);
        assert_eq!(rig.ctl.revision(), revision + 1);
        assert_eq!(rig.ctl.levels().unwrap().gain_db, 33.0);
    }

    #[test]
    fn a_command_is_saved() {
        let mut rig = standard();
        rig.device.borrow_mut().plug();
        run(&mut rig.ctl, 0.0, 6.0);
        rig.ctl.set_muted(true);
        assert!(rig.saved.borrow().as_ref().unwrap().muted);
    }

    #[test]
    fn three_read_failures_in_ready_end_in_error() {
        let mut rig = standard();
        rig.device.borrow_mut().plug();
        let t = run(&mut rig.ctl, 0.0, 6.0);
        rig.device.borrow_mut().fail_reads = true;
        rig.ctl.tick(t);
        rig.ctl.tick(t + 0.25);
        assert_eq!(rig.ctl.state(), State::Ready);
        rig.ctl.tick(t + 0.5);
        assert_eq!(rig.ctl.state(), State::Error);
    }

    #[test]
    fn ticking_slows_down_only_after_the_guard_window() {
        let mut rig = standard();
        rig.device.borrow_mut().plug();
        run(&mut rig.ctl, 0.0, 1.5);
        assert_eq!(rig.ctl.state(), State::Ready);
        assert!(!rig.ctl.settled(2.0));
        assert!(rig.ctl.settled(10.0));
    }

    // --- clipguard, gain lock, mic/PC mix, device info ---

    /// What the device has after a driver rebind or a reset to the factory state.
    fn factory_reset(rig: &Rig) {
        rig.device.borrow_mut().levels = DRIVER_START;
    }

    #[test]
    fn new_switches_are_left_alone_until_the_user_sets_them() {
        let mut rig = standard();
        rig.device.borrow_mut().levels.clipguard = false;
        rig.device.borrow_mut().levels.gain_lock = true;
        rig.device.borrow_mut().levels.mic_mix_percent = 70.0;
        rig.device.borrow_mut().plug();
        run(&mut rig.ctl, 0.0, 6.0);
        let l = levels(&rig);
        assert!(!l.clipguard && l.gain_lock && l.mic_mix_percent == 70.0); // untouched
        let saved = rig.saved.borrow().clone().unwrap();
        assert_eq!((saved.clipguard, saved.gain_lock, saved.mic_mix), (None, None, None));
    }

    #[test]
    fn pinned_switches_are_restored_after_a_replug() {
        let mut rig = standard();
        rig.device.borrow_mut().plug();
        let t = run(&mut rig.ctl, 0.0, 3.0);
        rig.ctl.set_clipguard(false);
        rig.ctl.set_gain_lock(true);
        rig.ctl.set_mic_mix(35.0);
        let saved = rig.saved.borrow().clone().unwrap();
        assert_eq!((saved.clipguard, saved.gain_lock, saved.mic_mix), (Some(false), Some(true), Some(35.0)));
        rig.device.borrow_mut().unplug();
        let t = run(&mut rig.ctl, t, 1.0);
        factory_reset(&rig);
        rig.device.borrow_mut().plug();
        run(&mut rig.ctl, t, 3.0);
        assert_eq!(rig.ctl.state(), State::Ready);
        let l = levels(&rig);
        assert!(!l.clipguard && l.gain_lock && l.mic_mix_percent == 35.0);
    }

    #[test]
    fn only_the_pinned_one_is_restored() {
        let mut rig = standard();
        rig.device.borrow_mut().plug();
        let t = run(&mut rig.ctl, 0.0, 3.0);
        rig.ctl.set_gain_lock(true);
        rig.device.borrow_mut().unplug();
        let t = run(&mut rig.ctl, t, 1.0);
        factory_reset(&rig);
        rig.device.borrow_mut().levels.mic_mix_percent = 60.0; // set elsewhere, never by the app
        rig.device.borrow_mut().plug();
        run(&mut rig.ctl, t, 3.0);
        let l = levels(&rig);
        assert!(l.gain_lock);
        assert_eq!(l.mic_mix_percent, 60.0);
    }

    #[test]
    fn a_change_at_the_device_updates_a_pinned_value_only() {
        let mut rig = standard();
        rig.device.borrow_mut().plug();
        let t = run(&mut rig.ctl, 0.0, 6.0);
        rig.ctl.set_mic_mix(20.0);
        rig.ctl.set_gain_lock(false);
        rig.device.borrow_mut().levels.mic_mix_percent = 55.0; // another program
        rig.device.borrow_mut().levels.gain_lock = true;
        rig.device.borrow_mut().levels.clipguard = false; // never pinned
        run(&mut rig.ctl, t, 1.0);
        let saved = rig.saved.borrow().clone().unwrap();
        assert_eq!((saved.mic_mix, saved.gain_lock, saved.clipguard), (Some(55.0), Some(true), None));
        assert_eq!(rig.ctl.levels().unwrap().mic_mix_percent, 55.0);
    }

    #[test]
    fn mic_mix_command_is_clamped() {
        let mut rig = standard();
        rig.device.borrow_mut().plug();
        run(&mut rig.ctl, 0.0, 6.0);
        rig.ctl.set_mic_mix(150.0);
        assert_eq!(levels(&rig).mic_mix_percent, 100.0);
        assert_eq!(rig.saved.borrow().as_ref().unwrap().mic_mix, Some(100.0));
        rig.ctl.set_mic_mix(-3.0);
        assert_eq!(levels(&rig).mic_mix_percent, 0.0);
    }

    #[test]
    fn new_commands_are_ignored_unless_ready() {
        let mut rig = standard();
        rig.ctl.set_clipguard(false);
        rig.ctl.set_gain_lock(true);
        rig.ctl.set_mic_mix(50.0);
        let l = levels(&rig);
        assert!(l.clipguard && !l.gain_lock && l.mic_mix_percent == 0.0);
        rig.device.borrow_mut().plug();
        rig.ctl.tick(0.0); // Initializing, still not Ready
        rig.ctl.set_gain_lock(true);
        assert!(!levels(&rig).gain_lock);
        assert_eq!(rig.saved.borrow().as_ref().unwrap().gain_lock, None);
    }

    #[test]
    fn device_info_is_there_only_when_ready() {
        let mut rig = standard();
        assert!(rig.ctl.device_info().is_none());
        rig.device.borrow_mut().plug();
        let t = run(&mut rig.ctl, 0.0, 3.0);
        let info = rig.ctl.device_info().unwrap();
        assert_eq!((info.name.as_str(), info.firmware.as_str(), info.serial.as_str()), ("Wave XLR", "1.3.5", "TEST123"));
        rig.device.borrow_mut().unplug();
        run(&mut rig.ctl, t, 1.0);
        assert!(rig.ctl.device_info().is_none());
    }
}
