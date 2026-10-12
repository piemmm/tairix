//! The sounds a theme supplies, and which one each event plays.

use crate::SoundEvent;

/// One sound a theme supplies, stored as `<name>.flac`.
///
/// Events may share one: a terminal's bell is the soft notification, so two
/// events cost one file and one cache entry. The mapping is this table, not
/// logic in the authority.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
pub enum SoundAsset {
    /// The machine is ready.
    Startup,
    /// The machine is powering off.
    Shutdown,
    /// A user signed in.
    Login,
    /// A user signed out.
    Logout,
    /// A device arrived.
    UsbConnected,
    /// A device left.
    UsbDisconnected,
    /// A notification, or a terminal's bell.
    NotificationSoft,
    /// A message.
    NotificationMessage,
    /// A reminder.
    NotificationReminder,
    /// Something failed.
    Error,
    /// Something needs attention.
    Warning,
    /// Something worth knowing.
    Info,
}

/// The extension every asset is stored under.
pub const SOUND_FILE_EXTENSION: &str = ".flac";

impl SoundAsset {
    /// Every asset.
    pub const ALL: [Self; 12] = [
        Self::Startup,
        Self::Shutdown,
        Self::Login,
        Self::Logout,
        Self::UsbConnected,
        Self::UsbDisconnected,
        Self::NotificationSoft,
        Self::NotificationMessage,
        Self::NotificationReminder,
        Self::Error,
        Self::Warning,
        Self::Info,
    ];

    /// The asset `event` plays.
    #[must_use]
    pub const fn of(event: SoundEvent) -> Self {
        match event {
            SoundEvent::Startup => Self::Startup,
            SoundEvent::Shutdown => Self::Shutdown,
            SoundEvent::Login => Self::Login,
            SoundEvent::Logout => Self::Logout,
            SoundEvent::DeviceAttached => Self::UsbConnected,
            SoundEvent::DeviceDetached => Self::UsbDisconnected,
            SoundEvent::Notification | SoundEvent::Bell => Self::NotificationSoft,
            SoundEvent::NotificationMessage => Self::NotificationMessage,
            SoundEvent::NotificationReminder => Self::NotificationReminder,
            SoundEvent::Error => Self::Error,
            SoundEvent::Warning => Self::Warning,
            SoundEvent::Info => Self::Info,
        }
    }

    /// The asset's stable name: its file is this plus
    /// [`SOUND_FILE_EXTENSION`].
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Startup => "startup",
            Self::Shutdown => "shutdown",
            Self::Login => "login",
            Self::Logout => "logout",
            Self::UsbConnected => "usb_connected",
            Self::UsbDisconnected => "usb_disconnected",
            Self::NotificationSoft => "notification_soft",
            Self::NotificationMessage => "notification_message",
            Self::NotificationReminder => "notification_reminder",
            Self::Error => "error",
            Self::Warning => "warning",
            Self::Info => "info",
        }
    }

    /// The asset a file named `file` holds, or `None` for any other name: a
    /// theme file no event plays is an orphan.
    #[must_use]
    pub fn from_file_name(file: &str) -> Option<Self> {
        let name = file.strip_suffix(SOUND_FILE_EXTENSION)?;
        Self::ALL.into_iter().find(|asset| asset.name() == name)
    }
}

#[cfg(test)]
#[path = "event_tests.rs"]
mod tests;
