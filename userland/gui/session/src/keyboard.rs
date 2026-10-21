//! Backing the desktop's [`InputSource`] with a live keyboard channel.
//!
//! [`DesktopShell`](crate::DesktopShell) drives the desktop by
//! [`pump`](crate::DesktopShell::pump)ing an injected [`InputSource`]. The
//! pointer's live backing is [`DeviceInputSource`](crate::DeviceInputSource);
//! this module is the keyboard's: [`KeyboardInputSource`] reads framed
//! [`KeyInput`] records from a kernel keyboard channel and decodes each into
//! the desktop's `lib/input` [`InputEvent`] vocabulary the window manager
//! delivers to the focused window.
//!
//! It is also the one place a held key repeats. A USB keyboard reports a held
//! key once and a PS/2 one repeats it itself; the source drops a device's own
//! repeats and repeats a held key under the user's policy, so every keyboard
//! behaves alike and no surface above it repeats anything. And it is the one
//! place a lone press of Ctrl or Meta is recognised ([`ModifierTap`]), since
//! every record passes it in the order it was typed.
//!
//! The raw bytes arrive through an injected [`KeyInputChannel`] seam — a
//! capability-checked kernel input channel on a running system, an in-memory
//! queue in tests — so this `userland/gui` crate holds no
//! input capability of its own and the decode runs above the device, not
//! inside it. Every record is validated by
//! [`KeyInput::from_bytes`] before it becomes an [`InputEvent`]; a malformed
//! record surfaces its [`Errno`] and the shell's
//! [`pump`](crate::DesktopShell::pump) stops without misinterpreting the bytes.
//!
//! [`InputSource`]: crate::InputSource
//! [`InputEvent`]: tairix_wm::InputEvent
//! [`DeviceInputSource`]: crate::DeviceInputSource

use tairix_abi::input::{KeyInput, KeyValue, Modifiers, NamedKeyCode};
use tairix_abi::time::Duration64;
use tairix_abi::Errno;
use tairix_keymap::modifiers_from_abi;
use tairix_wm::{InputEvent, Key, NamedKey, WindowId};

use crate::switchuser::park_within;

/// A source of framed [`KeyInput`] record bytes from the kernel.
///
/// On a running system this is a capability-checked kernel keyboard channel
/// that hands the desktop one [`KeyInput::WIRE_LEN`]-byte record at a time;
/// tests back it with an in-memory queue. It deals only in
/// raw bytes: decoding and validating them is [`KeyboardInputSource`]'s job,
/// so the channel itself need not understand the wire format.
pub trait KeyInputChannel {
    /// Take the next pending record's bytes, or `None` when the channel is
    /// momentarily drained.
    ///
    /// # Errors
    ///
    /// Returns the kernel boundary's [`Errno`] when the channel itself faults
    /// (for example it was closed). The bytes are not interpreted here; a
    /// short or corrupt record is the decoder's concern, not the channel's.
    fn next_record(&mut self) -> Result<Option<[u8; KeyInput::WIRE_LEN]>, Errno>;
}

/// How a held key repeats.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct KeyRepeat {
    /// How long a key is held before it first repeats.
    pub delay: Duration64,
    /// The span between repeats, or `None` when a held key does not repeat.
    pub interval: Option<Duration64>,
}

/// How long a modifier may be held and still be a tap: past it, one let go on
/// its own is a shortcut abandoned half-way, not a request.
const TAP_HOLD_MAX_NS: u64 = 1_000_000_000;

/// Which modifier a lone tap was of.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum TapKey {
    /// Ctrl, which shows where the pointer is.
    Ctrl,
    /// Meta — the key a PC keyboard marks with its maker's logo — which opens
    /// and closes the program library.
    Meta,
}

/// A lone press of Ctrl or Meta: down from no modifier at all and up again
/// with no key and no other modifier between.
///
/// Only what the keyboard reported can spoil one, so a pointer button pressed
/// or the wheel turned while it was down is the embedder's to check
/// ([`DeviceInputSource::quiet_since`](crate::DeviceInputSource::quiet_since)).
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct ModifierTap {
    /// Which modifier it was.
    pub key: TapKey,
    /// When it went down.
    pub pressed_ns: u64,
}

/// A lone press of a modifier as far as it has got.
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
enum Tapping {
    /// No modifier is held.
    #[default]
    Idle,
    /// This modifier alone went down at this instant, and nothing has gone
    /// with it.
    Down(TapKey, u64),
    /// A modifier is held that cannot end in a tap: something went with it,
    /// or another modifier was already down when it went down.
    Spoiled,
}

/// The modifier `held` is when it is one tappable modifier alone.
fn alone(held: Modifiers) -> Option<TapKey> {
    let only = |modifiers: Modifiers| held == modifiers;
    if only(Modifiers {
        ctrl: true,
        ..Modifiers::default()
    }) {
        Some(TapKey::Ctrl)
    } else if only(Modifiers {
        meta: true,
        ..Modifiers::default()
    }) {
        Some(TapKey::Meta)
    } else {
        None
    }
}

/// The key held down, and when it next repeats.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct Held {
    key: KeyValue,
    event: InputEvent,
    record: KeyInput,
    /// Monotonic nanoseconds of the next repeat, or `None` when it will not.
    next_ns: Option<u64>,
    /// The window that held the keyboard when the key went down.
    focus: Option<WindowId>,
}

/// A source that decodes [`KeyInput`] records from a [`KeyInputChannel`] and
/// repeats the held key.
///
/// Wrap a channel with [`new`](Self::new) and drain it through
/// [`DesktopShell::poll_key`](crate::DesktopShell::poll_key), the one drain
/// that keeps the seat's modifiers current.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct KeyboardInputSource<C> {
    channel: C,
    repeat: KeyRepeat,
    held: Option<Held>,
    /// The modifiers the last record left held.
    modifiers: Modifiers,
    tapping: Tapping,
    /// A tap recognised and not yet taken.
    tapped: Option<ModifierTap>,
}

impl<C> KeyboardInputSource<C> {
    /// Build a keyboard input source over `channel`, repeating a held key
    /// under `repeat`.
    pub const fn new(channel: C, repeat: KeyRepeat) -> Self {
        Self {
            channel,
            repeat,
            held: None,
            modifiers: Modifiers {
                shift: false,
                ctrl: false,
                alt: false,
                meta: false,
            },
            tapping: Tapping::Idle,
            tapped: None,
        }
    }

    /// The lone press of a modifier recognised since this was last asked, if
    /// any.
    pub fn take_tap(&mut self) -> Option<ModifierTap> {
        self.tapped.take()
    }

    /// Follow the modifiers to `now` at `now_ns`: a lone Ctrl or Meta let go in
    /// time is a tap, and anything else going with it spoils one.
    fn follow_modifiers(&mut self, now: Modifiers, now_ns: u64) {
        let none = now == Modifiers::default();
        self.tapping = match self.tapping {
            Tapping::Down(key, pressed_ns) if none => {
                if now_ns.saturating_sub(pressed_ns) <= TAP_HOLD_MAX_NS {
                    self.tapped = Some(ModifierTap { key, pressed_ns });
                }
                Tapping::Idle
            }
            _ if none => Tapping::Idle,
            Tapping::Idle if self.modifiers == Modifiers::default() => match alone(now) {
                Some(key) => Tapping::Down(key, now_ns),
                None => Tapping::Spoiled,
            },
            _ => Tapping::Spoiled,
        };
        self.modifiers = now;
    }

    /// Repeat a held key under `repeat` from now on. A key held when repeat
    /// is turned off stops repeating; one held when it is turned on does not
    /// start mid-hold.
    pub fn set_repeat(&mut self, repeat: KeyRepeat) {
        self.repeat = repeat;
        if repeat.interval.is_none() {
            if let Some(held) = self.held.as_mut() {
                held.next_ns = None;
            }
        }
    }

    /// Stop repeating whatever key is held: the screen it was typed at has
    /// gone, and a key held into a lock or another session must not follow.
    pub fn cancel_repeat(&mut self) {
        self.held = None;
    }

    /// Whether a repeat is due at `now_ns`.
    #[must_use]
    pub fn repeat_due(&self, now_ns: u64) -> bool {
        self.held
            .and_then(|held| held.next_ns)
            .is_some_and(|due| due <= now_ns)
    }

    /// `park_ns` shortened to the next repeat, or left as it is when no key
    /// is repeating: a keyboard at rest arms no timer.
    #[must_use]
    pub fn park_deadline_ns(&self, now_ns: u64, park_ns: u64) -> u64 {
        park_within(
            park_ns,
            self.held
                .and_then(|held| held.next_ns)
                .map(|due| due.saturating_sub(now_ns)),
        )
    }

    /// The underlying channel.
    pub const fn channel(&self) -> &C {
        &self.channel
    }

    /// The underlying channel, mutably.
    pub fn channel_mut(&mut self) -> &mut C {
        &mut self.channel
    }

    /// Consume the source, returning the channel it wrapped.
    pub fn into_channel(self) -> C {
        self.channel
    }
}

/// Map an ABI wire [`NamedKeyCode`] to the desktop's [`NamedKey`].
///
/// The wire ABI gives every function key its own discriminant; the routing
/// vocabulary folds them into one [`NamedKey::Function`] carrying the number,
/// so callers match on the family rather than twelve variants.
const fn named_key(code: NamedKeyCode) -> NamedKey {
    match code {
        NamedKeyCode::Enter => NamedKey::Enter,
        NamedKeyCode::Escape => NamedKey::Escape,
        NamedKeyCode::Backspace => NamedKey::Backspace,
        NamedKeyCode::Tab => NamedKey::Tab,
        NamedKeyCode::Delete => NamedKey::Delete,
        NamedKeyCode::Insert => NamedKey::Insert,
        NamedKeyCode::Home => NamedKey::Home,
        NamedKeyCode::End => NamedKey::End,
        NamedKeyCode::PageUp => NamedKey::PageUp,
        NamedKeyCode::PageDown => NamedKey::PageDown,
        NamedKeyCode::Left => NamedKey::Left,
        NamedKeyCode::Right => NamedKey::Right,
        NamedKeyCode::Up => NamedKey::Up,
        NamedKeyCode::Down => NamedKey::Down,
        NamedKeyCode::F1 => NamedKey::Function { number: 1 },
        NamedKeyCode::F2 => NamedKey::Function { number: 2 },
        NamedKeyCode::F3 => NamedKey::Function { number: 3 },
        NamedKeyCode::F4 => NamedKey::Function { number: 4 },
        NamedKeyCode::F5 => NamedKey::Function { number: 5 },
        NamedKeyCode::F6 => NamedKey::Function { number: 6 },
        NamedKeyCode::F7 => NamedKey::Function { number: 7 },
        NamedKeyCode::F8 => NamedKey::Function { number: 8 },
        NamedKeyCode::F9 => NamedKey::Function { number: 9 },
        NamedKeyCode::F10 => NamedKey::Function { number: 10 },
        NamedKeyCode::F11 => NamedKey::Function { number: 11 },
        NamedKeyCode::F12 => NamedKey::Function { number: 12 },
    }
}

/// Map a decoded ABI [`KeyValue`] to the desktop's [`Key`].
const fn key(value: KeyValue) -> Key {
    match value {
        KeyValue::Char(c) => Key::Char(c),
        KeyValue::Named(named) => Key::Named(named_key(named)),
    }
}

/// Whether `event` is the secure attention key: Ctrl+Alt+Delete pressed.
///
/// The session takes it before any holder of the seat routes a key, so no
/// application ever receives it.
#[must_use]
pub fn is_secure_attention(event: &InputEvent) -> bool {
    matches!(
        event,
        InputEvent::KeyPressed {
            key: Key::Named(NamedKey::Delete),
            modifiers,
        } if modifiers.ctrl && modifiers.alt
    )
}

/// Translate a decoded ABI [`KeyInput`] into the desktop [`InputEvent`].
///
/// The one translation from the wire vocabulary to the routing one, so a
/// surface the serve loop hands a raw record (the trusted picker, which the
/// router reaches by window id rather than through this source) reads it
/// exactly as every other event was read.
pub(crate) fn to_input_event(record: KeyInput) -> InputEvent {
    let modifiers = modifiers_from_abi(record.modifiers());
    match record {
        KeyInput::Pressed { key: value, .. } => InputEvent::KeyPressed {
            key: key(value),
            modifiers,
        },
        KeyInput::Released { key: value, .. } => InputEvent::KeyReleased {
            key: key(value),
            modifiers,
        },
        KeyInput::ModifiersChanged { .. } => InputEvent::ModifiersChanged { modifiers },
    }
}

impl<C: KeyInputChannel> KeyboardInputSource<C> {
    /// Poll one keyboard record at monotonic `now_ns`, returning the decoded
    /// routing event **and** the validated wire [`KeyInput`] it came from, so
    /// the window server can forward the original record to a focused app
    /// without re-encoding it.
    ///
    /// Once the channel is empty a repeat of the held key that has come due
    /// is answered, one per drain: a loop that was late repeats once rather
    /// than catching up in a burst. A press of the key already held is the
    /// device's own repeat and is dropped. A key held while the keyboard
    /// moves to another window, `focus`, stops repeating: what it was pressed
    /// for was not that window, as a prompt opened under a held `Enter` is
    /// not.
    ///
    /// Crate-private: drains go through [`DesktopShell::poll_key`](crate::DesktopShell::poll_key).
    ///
    /// # Errors
    ///
    /// A channel fault, or the fail-closed refusal of a malformed record.
    pub(crate) fn poll_record(
        &mut self,
        now_ns: u64,
        focus: Option<WindowId>,
    ) -> Result<Option<(InputEvent, KeyInput)>, Errno> {
        while let Some(bytes) = self.channel.next_record()? {
            let record = KeyInput::from_bytes(&bytes)?;
            let event = to_input_event(record);
            if let KeyInput::ModifiersChanged { modifiers } = record {
                self.follow_modifiers(modifiers, now_ns);
            } else if self.tapping != Tapping::Idle {
                self.tapping = Tapping::Spoiled;
            }
            match record {
                KeyInput::Pressed { key, .. } => {
                    if self.held.is_some_and(|held| held.key == key) {
                        continue;
                    }
                    self.held = Some(Held {
                        key,
                        event,
                        record,
                        next_ns: self.repeat.interval.map(|_| {
                            now_ns.saturating_add(self.repeat.delay.saturating_total_nanos())
                        }),
                        focus,
                    });
                }
                KeyInput::Released { key, .. } => {
                    if self.held.is_some_and(|held| held.key == key) {
                        self.held = None;
                    }
                }
                // The same key under new modifiers is a different key, and the
                // record that would say which cannot be re-read.
                KeyInput::ModifiersChanged { .. } => self.held = None,
            }
            return Ok(Some((event, record)));
        }
        Ok(self.take_due(now_ns, focus))
    }

    /// The held key's repeat, if it is due at `now_ns`, scheduling the next.
    fn take_due(&mut self, now_ns: u64, focus: Option<WindowId>) -> Option<(InputEvent, KeyInput)> {
        let interval = self.repeat.interval?;
        let held = self.held.as_mut()?;
        if held.focus != focus {
            held.next_ns = None;
        }
        if held.next_ns.is_none_or(|due| due > now_ns) {
            return None;
        }
        held.next_ns = Some(now_ns.saturating_add(interval.saturating_total_nanos()));
        Some((held.event, held.record))
    }
}

#[cfg(test)]
mod tests {
    use super::{KeyInputChannel, KeyRepeat, KeyboardInputSource};
    use alloc::collections::VecDeque;
    use tairix_abi::input::{KeyInput, KeyValue, Modifiers as AbiModifiers, NamedKeyCode};
    use tairix_abi::time::{Duration64, NANOS_PER_MILLI as MS};
    use tairix_abi::Errno;
    use tairix_wm::{InputEvent, Key, Modifiers, NamedKey};

    /// Half a second's delay, then twenty repeats a second.
    const REPEAT: KeyRepeat = KeyRepeat {
        delay: Duration64::from_millis(500),
        interval: Some(Duration64::from_millis(50)),
    };

    /// The next event `source` answers at the start of time, before any
    /// repeat could be due.
    fn poll<C: KeyInputChannel>(
        source: &mut KeyboardInputSource<C>,
    ) -> Result<Option<InputEvent>, Errno> {
        Ok(source.poll_record(0, None)?.map(|(event, _)| event))
    }

    /// An in-memory channel that yields queued records, optionally faulting.
    struct QueueChannel {
        records: VecDeque<[u8; KeyInput::WIRE_LEN]>,
        fault: Option<Errno>,
    }

    impl QueueChannel {
        fn new(events: &[KeyInput]) -> Self {
            Self {
                records: events.iter().map(KeyInput::to_le_bytes).collect(),
                fault: None,
            }
        }

        fn push_raw(&mut self, bytes: [u8; KeyInput::WIRE_LEN]) {
            self.records.push_back(bytes);
        }

        fn fault_with(&mut self, errno: Errno) {
            self.fault = Some(errno);
        }
    }

    impl KeyInputChannel for QueueChannel {
        fn next_record(&mut self) -> Result<Option<[u8; KeyInput::WIRE_LEN]>, Errno> {
            if let Some(errno) = self.fault.take() {
                return Err(errno);
            }
            Ok(self.records.pop_front())
        }
    }

    /// The modifiers `ctrl` and `shift` spell, as a record.
    fn modifiers(ctrl: bool, shift: bool) -> KeyInput {
        KeyInput::ModifiersChanged {
            modifiers: AbiModifiers {
                ctrl,
                shift,
                ..AbiModifiers::default()
            },
        }
    }

    /// The record of Meta alone held, or of nothing held.
    fn meta(held: bool) -> KeyInput {
        KeyInput::ModifiersChanged {
            modifiers: AbiModifiers {
                meta: held,
                ..AbiModifiers::default()
            },
        }
    }

    /// Drain `records` into a fresh source, each a millisecond after the
    /// last, and answer the lone modifier press recognised, if any.
    fn tapped(records: &[KeyInput]) -> Option<super::ModifierTap> {
        let mut source = KeyboardInputSource::new(QueueChannel::new(&[]), REPEAT);
        for (index, record) in records.iter().enumerate() {
            source.channel_mut().records.push_back(record.to_le_bytes());
            let at = u64::try_from(index).expect("small") * 1_000_000;
            while source.poll_record(at, None).expect("well-formed").is_some() {}
        }
        source.take_tap()
    }

    #[test]
    fn a_lone_press_of_ctrl_is_a_tap_taken_once() {
        let mut source = KeyboardInputSource::new(
            QueueChannel::new(&[modifiers(true, false), modifiers(false, false)]),
            REPEAT,
        );
        while source.poll_record(5, None).expect("well-formed").is_some() {}
        assert_eq!(
            source.take_tap(),
            Some(super::ModifierTap {
                key: super::TapKey::Ctrl,
                pressed_ns: 5
            })
        );
        assert_eq!(source.take_tap(), None, "a tap is answered once");
    }

    #[test]
    fn a_lone_press_of_meta_is_a_tap_of_meta() {
        let tap = tapped(&[meta(true), meta(false)]).expect("a tap");
        assert_eq!(tap.key, super::TapKey::Meta);
        // Meta joined by Ctrl is a chord, whichever went down first.
        let both = KeyInput::ModifiersChanged {
            modifiers: AbiModifiers {
                meta: true,
                ctrl: true,
                ..AbiModifiers::default()
            },
        };
        assert_eq!(tapped(&[meta(true), both, meta(false)]), None);
        assert_eq!(
            tapped(&[modifiers(true, false), both, meta(true), meta(false)]),
            None
        );
        // A key typed under Meta is a shortcut, not a tap.
        let key = KeyInput::Pressed {
            key: KeyValue::Char('e'),
            modifiers: AbiModifiers {
                meta: true,
                ..AbiModifiers::default()
            },
        };
        assert_eq!(tapped(&[meta(true), key, meta(false)]), None);
    }

    #[test]
    fn a_key_or_another_modifier_between_spoils_the_tap() {
        let key = KeyInput::Pressed {
            key: KeyValue::Char('c'),
            modifiers: AbiModifiers {
                ctrl: true,
                ..AbiModifiers::default()
            },
        };
        let spoiled: [&[KeyInput]; 3] = [
            &[modifiers(true, false), key, modifiers(false, false)],
            &[
                modifiers(true, false),
                modifiers(true, true),
                modifiers(false, false),
            ],
            // Ctrl joining a modifier already down is a chord, not a tap.
            &[
                modifiers(false, true),
                modifiers(true, true),
                modifiers(true, false),
                modifiers(false, false),
            ],
        ];
        for records in spoiled {
            assert_eq!(tapped(records), None, "{records:?}");
        }
    }

    #[test]
    fn a_tap_after_a_spoiled_one_still_counts() {
        let key = KeyInput::Pressed {
            key: KeyValue::Char('v'),
            modifiers: AbiModifiers {
                ctrl: true,
                ..AbiModifiers::default()
            },
        };
        assert!(tapped(&[
            modifiers(true, false),
            key,
            modifiers(false, false),
            modifiers(true, false),
            modifiers(false, false),
        ])
        .is_some());
    }

    #[test]
    fn ctrl_held_too_long_is_no_tap() {
        let mut source =
            KeyboardInputSource::new(QueueChannel::new(&[modifiers(true, false)]), REPEAT);
        while source.poll_record(0, None).expect("well-formed").is_some() {}
        source
            .channel_mut()
            .records
            .push_back(modifiers(false, false).to_le_bytes());
        while source
            .poll_record(super::TAP_HOLD_MAX_NS + 1, None)
            .expect("well-formed")
            .is_some()
        {}
        assert_eq!(source.take_tap(), None);
    }

    #[test]
    fn decodes_char_press_with_modifiers() {
        let mut source = KeyboardInputSource::new(
            QueueChannel::new(&[KeyInput::Pressed {
                key: KeyValue::Char('z'),
                modifiers: AbiModifiers {
                    ctrl: true,
                    ..AbiModifiers::default()
                },
            }]),
            REPEAT,
        );
        assert_eq!(
            poll(&mut source),
            Ok(Some(InputEvent::KeyPressed {
                key: Key::Char('z'),
                modifiers: Modifiers {
                    ctrl: true,
                    ..Modifiers::default()
                },
            }))
        );
        assert_eq!(poll(&mut source), Ok(None));
    }

    #[test]
    fn decodes_named_release_and_folds_function_keys() {
        let events = [
            KeyInput::Released {
                key: KeyValue::Named(NamedKeyCode::Escape),
                modifiers: AbiModifiers::default(),
            },
            KeyInput::Pressed {
                key: KeyValue::Named(NamedKeyCode::F5),
                modifiers: AbiModifiers::default(),
            },
        ];
        let mut source = KeyboardInputSource::new(QueueChannel::new(&events), REPEAT);
        assert_eq!(
            poll(&mut source),
            Ok(Some(InputEvent::KeyReleased {
                key: Key::Named(NamedKey::Escape),
                modifiers: Modifiers::default(),
            }))
        );
        assert_eq!(
            poll(&mut source),
            Ok(Some(InputEvent::KeyPressed {
                key: Key::Named(NamedKey::Function { number: 5 }),
                modifiers: Modifiers::default(),
            }))
        );
        assert_eq!(poll(&mut source), Ok(None));
    }

    #[test]
    fn malformed_record_surfaces_bad_magic() {
        let mut channel = QueueChannel::new(&[]);
        channel.push_raw([0u8; KeyInput::WIRE_LEN]);
        let mut source = KeyboardInputSource::new(channel, REPEAT);
        // An all-zero record has the wrong magic and must be refused, never
        // misinterpreted.
        assert_eq!(poll(&mut source), Err(Errno::BadMagic));
    }

    #[test]
    fn channel_fault_propagates() {
        let mut channel = QueueChannel::new(&[KeyInput::Pressed {
            key: KeyValue::Char('a'),
            modifiers: AbiModifiers::default(),
        }]);
        channel.fault_with(Errno::NotFound);
        let mut source = KeyboardInputSource::new(channel, REPEAT);
        assert_eq!(poll(&mut source), Err(Errno::NotFound));
        // After the one-shot fault clears, the queued record still decodes.
        assert_eq!(
            poll(&mut source),
            Ok(Some(InputEvent::KeyPressed {
                key: Key::Char('a'),
                modifiers: Modifiers::default(),
            }))
        );
    }

    #[test]
    fn into_channel_returns_the_wrapped_channel() {
        let source = KeyboardInputSource::new(
            QueueChannel::new(&[KeyInput::Pressed {
                key: KeyValue::Char('a'),
                modifiers: AbiModifiers::default(),
            }]),
            REPEAT,
        );
        let channel = source.into_channel();
        assert_eq!(channel.records.len(), 1);
    }

    fn press(c: char) -> KeyInput {
        KeyInput::Pressed {
            key: KeyValue::Char(c),
            modifiers: AbiModifiers::default(),
        }
    }

    fn release(c: char) -> KeyInput {
        KeyInput::Released {
            key: KeyValue::Char(c),
            modifiers: AbiModifiers::default(),
        }
    }

    fn typed<C: KeyInputChannel>(source: &mut KeyboardInputSource<C>, now_ns: u64) -> Option<char> {
        match source.poll_record(now_ns, None) {
            Ok(Some((
                InputEvent::KeyPressed {
                    key: Key::Char(c), ..
                },
                _,
            ))) => Some(c),
            _ => None,
        }
    }

    #[test]
    fn a_held_key_repeats_after_the_delay_then_at_the_rate() {
        let mut source = KeyboardInputSource::new(QueueChannel::new(&[press('a')]), REPEAT);
        assert_eq!(typed(&mut source, 0), Some('a'));
        assert_eq!(typed(&mut source, 499 * MS), None, "not before the delay");
        assert_eq!(typed(&mut source, 500 * MS), Some('a'));
        assert_eq!(typed(&mut source, 500 * MS), None, "one repeat per instant");
        assert_eq!(typed(&mut source, 549 * MS), None);
        assert_eq!(typed(&mut source, 550 * MS), Some('a'));
    }

    /// A prompt opened under a held `Enter` must not be answered by its
    /// repeats: the key was pressed for the window before it.
    #[test]
    fn a_key_held_while_the_keyboard_moves_to_another_window_stops_repeating() {
        let mut comp = crate::tests::compositor();
        let before = comp.add_window(
            tairix_wm::Point::new(0, 0),
            tairix_wm::Surface::new(8, 8).expect("a surface"),
        );
        let prompt = comp.add_window(
            tairix_wm::Point::new(10, 10),
            tairix_wm::Surface::new(8, 8).expect("a surface"),
        );
        let mut source = KeyboardInputSource::new(QueueChannel::new(&[press('a')]), REPEAT);
        assert!(matches!(source.poll_record(0, Some(before)), Ok(Some(_))));
        assert!(matches!(
            source.poll_record(500 * MS, Some(before)),
            Ok(Some(_))
        ));
        assert_eq!(
            source.poll_record(550 * MS, Some(prompt)),
            Ok(None),
            "no repeat reaches the window that gained the keyboard"
        );
        assert_eq!(source.poll_record(10_000 * MS, Some(before)), Ok(None));
        assert_eq!(
            source.park_deadline_ns(0, u64::MAX),
            u64::MAX,
            "and no wake is armed"
        );
    }

    /// A loop that was late repeats once, not once for every interval it
    /// missed: a stalled desktop must not dump a burst into a window.
    #[test]
    fn a_late_drain_repeats_once_rather_than_catching_up() {
        let mut source = KeyboardInputSource::new(QueueChannel::new(&[press('a')]), REPEAT);
        assert_eq!(typed(&mut source, 0), Some('a'));
        assert_eq!(typed(&mut source, 5_000 * MS), Some('a'));
        assert_eq!(typed(&mut source, 5_000 * MS), None);
        assert!(!source.repeat_due(5_049 * MS));
        assert!(source.repeat_due(5_050 * MS));
    }

    /// A device that repeats a held key itself is not repeated twice: its
    /// own repeats are dropped and the source's policy is the only one.
    #[test]
    fn a_devices_own_repeat_of_the_held_key_is_dropped() {
        let mut source = KeyboardInputSource::new(
            QueueChannel::new(&[press('a'), press('a'), press('a'), release('a'), press('a')]),
            REPEAT,
        );
        assert_eq!(typed(&mut source, 0), Some('a'));
        // The two device repeats are consumed without surfacing; the release
        // is the next record the source answers.
        assert!(matches!(
            source.poll_record(MS, None),
            Ok(Some((InputEvent::KeyReleased { .. }, _)))
        ));
        assert_eq!(
            typed(&mut source, 2 * MS),
            Some('a'),
            "a new press after a release"
        );
    }

    #[test]
    fn releasing_the_held_key_or_changing_modifiers_stops_the_repeat() {
        let mut source = KeyboardInputSource::new(
            QueueChannel::new(&[press('a'), release('b'), release('a')]),
            REPEAT,
        );
        assert_eq!(typed(&mut source, 0), Some('a'));
        let _ = source.poll_record(MS, None);
        assert!(
            source.repeat_due(600 * MS),
            "releasing another key leaves the held one repeating"
        );
        let _ = source.poll_record(MS, None);
        assert!(!source.repeat_due(600 * MS), "its own release stops it");

        let mut shifted = KeyboardInputSource::new(
            QueueChannel::new(&[
                press('a'),
                KeyInput::ModifiersChanged {
                    modifiers: AbiModifiers {
                        shift: true,
                        ..AbiModifiers::default()
                    },
                },
            ]),
            REPEAT,
        );
        assert_eq!(typed(&mut shifted, 0), Some('a'));
        let _ = shifted.poll_record(MS, None);
        assert!(!shifted.repeat_due(600 * MS));
    }

    #[test]
    fn repeat_off_repeats_nothing_and_still_drops_a_devices_repeats() {
        let off = KeyRepeat {
            interval: None,
            ..REPEAT
        };
        let mut source =
            KeyboardInputSource::new(QueueChannel::new(&[press('a'), press('a')]), off);
        assert_eq!(typed(&mut source, 0), Some('a'));
        assert_eq!(source.poll_record(10_000 * MS, None), Ok(None));
        assert_eq!(source.park_deadline_ns(0, u64::MAX), u64::MAX);
    }

    #[test]
    fn turning_repeat_off_or_cancelling_stops_a_key_already_held() {
        let mut source = KeyboardInputSource::new(QueueChannel::new(&[press('a')]), REPEAT);
        assert_eq!(typed(&mut source, 0), Some('a'));
        source.set_repeat(KeyRepeat {
            interval: None,
            ..REPEAT
        });
        assert!(!source.repeat_due(u64::MAX));

        let mut cancelled = KeyboardInputSource::new(QueueChannel::new(&[press('a')]), REPEAT);
        assert_eq!(typed(&mut cancelled, 0), Some('a'));
        cancelled.cancel_repeat();
        assert!(!cancelled.repeat_due(u64::MAX));
    }

    #[test]
    fn the_park_is_shortened_to_the_next_repeat_only_while_one_is_pending() {
        let mut source = KeyboardInputSource::new(QueueChannel::new(&[press('a')]), REPEAT);
        assert_eq!(
            source.park_deadline_ns(0, u64::MAX),
            u64::MAX,
            "nothing held"
        );
        assert_eq!(typed(&mut source, 0), Some('a'));
        assert_eq!(source.park_deadline_ns(100 * MS, u64::MAX), 400 * MS);
        assert_eq!(source.park_deadline_ns(100 * MS, 10 * MS), 10 * MS);
        assert_eq!(
            source.park_deadline_ns(900 * MS, u64::MAX),
            0,
            "overdue is now"
        );
    }
}
