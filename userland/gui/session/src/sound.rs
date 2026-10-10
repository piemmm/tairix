//! The session's sound (`plans/SOUND.md` §Desktop integration): the device
//! controls it asks the audio service for, what the bar shows, and the
//! user's own controls, remembered while the session's room shows them and
//! put back when it claims the room again.
//!
//! Its round trips are a [`ControlQueue`]'s: one in flight, the latest
//! control of each kind for each device winning meanwhile.

use alloc::vec::Vec;

use tairix_abi::audio::{AudioDeviceDescriptor, ControlAccess};
use tairix_abi::driver::audio::StreamDirection;
use tairix_abi::Errno;
use tairix_audio::stream::{ControlQueue, DeviceControl};
use tairix_taskbar::{OutputState, SoundState};
use tairix_wallpaper::SoundControls;

/// One round trip to the audio service: the controls to apply, in order, and
/// then the devices to list.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct SoundJob {
    /// What to apply, each to its device.
    pub controls: Vec<(u32, DeviceControl)>,
}

/// What a round trip came back with.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SoundAnswer {
    /// The first of the controls the service refused, if it refused any.
    pub refused: Option<Errno>,
    /// The sinks and then the sources, or why they could not be listed.
    pub devices: Result<Vec<AudioDeviceDescriptor>, Errno>,
}

/// What a landed answer asks of the session.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Landed {
    /// What the bar shows now.
    pub shown: SoundState,
    /// The user's controls to remember, when this answer moved them.
    pub remember: Option<SoundControls>,
    /// A control the service refused, to be stated.
    pub refused: Option<Errno>,
}

/// The session's side of the audio service.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct SoundSession {
    queue: ControlQueue,
    /// Whether the last listing showed the room as this session's.
    holding: bool,
    /// A level drag in progress is remembered where it settles, not on the
    /// way there.
    dragging: bool,
    capturing: u32,
    output: Option<OutputState>,
}

impl SoundSession {
    /// A session that has asked for nothing yet.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            queue: ControlQueue::new(),
            holding: false,
            dragging: false,
            capturing: 0,
            output: None,
        }
    }

    /// Ask for `control` on the device `device_id`, replacing a control of
    /// the same kind still queued for it. A level not yet `settled` is part
    /// of a drag.
    pub fn ask(&mut self, device_id: u32, control: DeviceControl, settled: bool) {
        if let DeviceControl::Level(_) = control {
            self.dragging = !settled;
        }
        self.queue.ask(device_id, control);
    }

    /// Ask for the devices to be listed again.
    pub fn refresh(&mut self) {
        self.queue.refresh();
    }

    /// The round trip to start, if one is owed and none is in flight.
    pub fn next_job(&mut self) -> Option<SoundJob> {
        self.queue.next_trip().map(|controls| SoundJob { controls })
    }

    /// Adopt a landed answer. `remembered` is what the user's document holds.
    ///
    /// The listing that first shows the room as this session's puts the
    /// user's controls back rather than learning from it: a room just claimed
    /// shows the machine's baseline, or what another session left, and
    /// neither is the user's choice.
    pub fn landed(&mut self, answer: SoundAnswer, remembered: &SoundControls) -> Landed {
        self.queue.landed();
        let mut remember = None;
        if let Ok(devices) = answer.devices {
            self.output = devices
                .iter()
                .find(|device| {
                    device.direction == StreamDirection::Playback && device.default.is_default()
                })
                .map(|device| OutputState {
                    device_id: device.device_id,
                    name: device.name.as_str().into(),
                    level: device.level,
                    muted: device.muted,
                    may_change: device.access.may_change(),
                });
            let ours = devices
                .iter()
                .any(|device| device.access == ControlAccess::Own);
            if ours && !self.holding {
                self.holding = true;
                self.queue.ask_first(remembered.restore(&devices));
            } else if ours {
                if !self.dragging {
                    let mut kept = remembered.clone();
                    if kept.observe(&devices) {
                        remember = Some(kept);
                    }
                }
            } else {
                self.holding = false;
            }
        } else {
            self.output = None;
            self.holding = false;
        }
        Landed {
            shown: self.shown(),
            remember,
            refused: answer.refused,
        }
    }

    /// Adopt how many capture streams on the machine are moving frames,
    /// answering what the bar shows now.
    pub fn captures(&mut self, live: u32) -> SoundState {
        self.capturing = live;
        self.shown()
    }

    fn shown(&self) -> SoundState {
        SoundState {
            output: self.output.clone(),
            recording: self.capturing > 0,
        }
    }
}

#[cfg(test)]
#[path = "sound_tests.rs"]
mod tests;
