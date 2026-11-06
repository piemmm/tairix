//! What the desktop's screen capture keeps between captures: where a capture
//! is put, how long a timed one waits and a recording runs, what a recording
//! hears, and what a capture shows (`plans/CAPTURE.md`).
//!
//! Each is one key of the desktop settings document, which the session writes
//! once the user has made the choice.

use tairix_abi::recorder_ipc::RecordSound;

/// Where a capture is put.
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
pub enum CaptureFolder {
    /// The desktop.
    #[default]
    Desktop,
    /// The user's own files: `Pictures` for a still, `Videos` for a recording.
    Home,
    /// The clipboard, for a still. A recording, which no clipboard holds, is
    /// put on the desktop.
    Clipboard,
}

impl CaptureFolder {
    /// Every choice, in the order a menu lists them.
    pub const ALL: [Self; 3] = [Self::Desktop, Self::Home, Self::Clipboard];

    /// The canonical spelling.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Desktop => "desktop",
            Self::Home => "home",
            Self::Clipboard => "clipboard",
        }
    }

    /// The choice `value` spells; `None` outside the closed set.
    #[must_use]
    pub fn from_value(value: &str) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|choice| choice.as_str() == value)
    }
}

/// How long a capture taken from the capture bar waits before it is taken.
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
pub enum CaptureTimer {
    /// Not at all.
    #[default]
    Off,
    /// Five seconds.
    Seconds5,
    /// Ten seconds.
    Seconds10,
}

impl CaptureTimer {
    /// Every choice, in the order a menu lists them.
    pub const ALL: [Self; 3] = [Self::Off, Self::Seconds5, Self::Seconds10];

    /// The wait in whole seconds.
    #[must_use]
    pub const fn seconds(self) -> u32 {
        match self {
            Self::Off => 0,
            Self::Seconds5 => 5,
            Self::Seconds10 => 10,
        }
    }

    /// The canonical spelling.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::Seconds5 => "5",
            Self::Seconds10 => "10",
        }
    }

    /// The choice `value` spells; `None` outside the closed set.
    #[must_use]
    pub fn from_value(value: &str) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|choice| choice.as_str() == value)
    }
}

/// How long a recording runs before it stops itself.
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
pub enum CaptureDuration {
    /// Until the user stops it.
    #[default]
    Unlimited,
    /// Ten seconds.
    Seconds10,
    /// Thirty seconds.
    Seconds30,
    /// A minute.
    Minute1,
    /// Five minutes.
    Minutes5,
    /// Ten minutes.
    Minutes10,
}

impl CaptureDuration {
    /// Every choice, in the order a menu lists them.
    pub const ALL: [Self; 6] = [
        Self::Unlimited,
        Self::Seconds10,
        Self::Seconds30,
        Self::Minute1,
        Self::Minutes5,
        Self::Minutes10,
    ];

    /// The bound in whole seconds, or `None` for a recording only the user
    /// stops.
    #[must_use]
    pub const fn seconds(self) -> Option<u32> {
        match self {
            Self::Unlimited => None,
            Self::Seconds10 => Some(10),
            Self::Seconds30 => Some(30),
            Self::Minute1 => Some(60),
            Self::Minutes5 => Some(300),
            Self::Minutes10 => Some(600),
        }
    }

    /// The canonical spelling: `unlimited`, or the bound in whole seconds.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Unlimited => "unlimited",
            Self::Seconds10 => "10",
            Self::Seconds30 => "30",
            Self::Minute1 => "60",
            Self::Minutes5 => "300",
            Self::Minutes10 => "600",
        }
    }

    /// The choice `value` spells; `None` outside the closed set.
    #[must_use]
    pub fn from_value(value: &str) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|choice| choice.as_str() == value)
    }
}

/// The canonical spelling of what a recording hears.
#[must_use]
pub const fn sound_value(sound: RecordSound) -> &'static str {
    match sound {
        RecordSound::None => "none",
        RecordSound::Microphone => "microphone",
        RecordSound::Desktop => "desktop",
        RecordSound::Both => "both",
    }
}

/// What a recording hears, as `value` spells it; `None` outside the closed
/// set.
#[must_use]
pub fn parse_sound(value: &str) -> Option<RecordSound> {
    RecordSound::ALL
        .into_iter()
        .find(|sound| sound_value(*sound) == value)
}

/// What screen capture keeps between captures.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct CaptureSettings {
    /// Where a capture is put.
    pub save_to: CaptureFolder,
    /// How long a capture taken from the capture bar waits.
    pub timer: CaptureTimer,
    /// How long a recording runs before it stops itself.
    pub duration: CaptureDuration,
    /// What a recording hears.
    pub sound: RecordSound,
    /// Whether a capture shows the pointer.
    pub pointer: bool,
    /// Whether a recording marks each click.
    pub clicks: bool,
    /// Whether a floating thumbnail shows each capture as it is taken.
    pub thumbnail: bool,
}

impl Default for CaptureSettings {
    fn default() -> Self {
        Self {
            save_to: CaptureFolder::Desktop,
            timer: CaptureTimer::Off,
            duration: CaptureDuration::Unlimited,
            sound: RecordSound::None,
            pointer: true,
            clicks: false,
            thumbnail: true,
        }
    }
}

#[cfg(test)]
mod tests {
    use tairix_abi::recorder_ipc::RecordSound;

    use super::{
        parse_sound, sound_value, CaptureDuration, CaptureFolder, CaptureSettings, CaptureTimer,
    };

    #[test]
    fn every_choice_has_one_spelling_and_reads_back() {
        for folder in CaptureFolder::ALL {
            assert_eq!(CaptureFolder::from_value(folder.as_str()), Some(folder));
        }
        for timer in CaptureTimer::ALL {
            assert_eq!(CaptureTimer::from_value(timer.as_str()), Some(timer));
        }
        for duration in CaptureDuration::ALL {
            assert_eq!(
                CaptureDuration::from_value(duration.as_str()),
                Some(duration)
            );
        }
        for sound in RecordSound::ALL {
            assert_eq!(parse_sound(sound_value(sound)), Some(sound));
        }
        assert_eq!(CaptureFolder::from_value("Desktop"), None);
        assert_eq!(CaptureTimer::from_value("0"), None);
        assert_eq!(CaptureDuration::from_value("0"), None);
        assert_eq!(parse_sound("speaker"), None);
    }

    #[test]
    fn the_waits_and_bounds_are_whole_seconds() {
        assert_eq!(CaptureTimer::ALL.map(CaptureTimer::seconds), [0, 5, 10]);
        assert_eq!(
            CaptureDuration::ALL.map(CaptureDuration::seconds),
            [None, Some(10), Some(30), Some(60), Some(300), Some(600)]
        );
    }

    #[test]
    fn by_default_a_capture_goes_to_the_desktop_at_once_with_the_pointer() {
        let settings = CaptureSettings::default();
        assert_eq!(settings.save_to, CaptureFolder::Desktop);
        assert_eq!(settings.timer.seconds(), 0);
        assert_eq!(settings.duration.seconds(), None);
        assert_eq!(settings.sound, RecordSound::None);
        assert!(settings.pointer && settings.thumbnail && !settings.clicks);
    }
}
