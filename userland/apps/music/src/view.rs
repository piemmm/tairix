//! The player's state: the playlist as the window shows it, the transport,
//! and what the listener does to them — input in, state and damage out.
//!
//! Every entry point updates state and reports the rectangles it changed;
//! none paints and none waits. What the playback thread must do is a
//! [`Command`], what the worker must fetch is a [`Request`], and a continuous
//! control acts durably once, where it settles: dragging the seek slider
//! moves the position shown and seeks on release, and dragging the volume
//! sets the stream's level as it moves and saves it on release.

use alloc::string::String;
use alloc::vec::Vec;

use tairix_abi::audio::{AudioDeviceDescriptor, AudioGain};
use tairix_abi::time::Duration64;
use tairix_abi::window_ipc::{
    AppMenu, AppMenuItem, AppMenuItemId, AppMenuLabel, AppMenuMark, AppMenuRow, PickPurpose,
};
use tairix_audio::target::AudioTarget;
use tairix_audio::volume::{level_at_permille, permille_of_level, DEFAULT_FLOOR_MILLIBEL};
use tairix_controls::{
    ButtonAction, ControlRole, IconButton, ScrollAction, ScrollBar, ScrollModel, ScrollOrientation,
    ScrollRange, SelectionState, Slider, SliderAction,
};
use tairix_geometry::{Point, Region, Scale};
use tairix_icon::IconKind;
use tairix_input::{ClickKind, DoubleClickTracker, InputEvent, Key, NamedKey, PointerButton};
use tairix_player::{Control, EntryId, Programme, Span, Status, Transport};
use tairix_rng::{NonCryptoRng, RandU64};
use tairix_sound::{CoverRange, Metadata, SoundInfo, TagKind};
use tairix_theme::Theme;
use tairix_window::menu::{MenuBuilder, Plate};

use crate::layout::Layout;
use crate::playlist::{Edit, Playlist, Repeat};

/// The slider's full travel: it reads its position in parts per thousand.
const TRAVEL: u16 = 1_000;

/// One level step from the keyboard, in hundredths of a decibel.
const LEVEL_STEP_MILLIBEL: i32 = 300;

/// The menu rows' ids. The icon bar reserves the first for its own Quit row.
mod row {
    pub const OPEN_FILE: u16 = 10;
    pub const OPEN_FOLDER: u16 = 11;
    pub const CLEAR: u16 = 12;
    pub const SHUFFLE: u16 = 20;
    pub const REPEAT_OFF: u16 = 21;
    pub const REPEAT_ALL: u16 = 22;
    pub const REPEAT_ONE: u16 = 23;
    pub const NORMALISE: u16 = 24;
    pub const PLAY: u16 = 30;
    pub const REMOVE: u16 = 31;
    pub const MOVE_UP: u16 = 32;
    pub const MOVE_DOWN: u16 = 33;
    pub const DEFAULT_DEVICE: u16 = 40;
    pub const FIRST_DEVICE: u16 = 41;
}

/// What a row's file was found to be.
#[derive(Clone, Debug, PartialEq)]
pub enum Track {
    /// Its tags are being read.
    Reading,
    /// Its tags have been read.
    Known(Known),
    /// It could not be read.
    Unreadable,
}

/// A track whose tags have been read.
#[derive(Clone, Debug, PartialEq)]
pub struct Known {
    /// What its stream is.
    pub info: SoundInfo,
    /// Its title, where it states one.
    pub title: Option<String>,
    /// Who performed it.
    pub artist: Option<String>,
    /// The album it belongs to.
    pub album: Option<String>,
    /// Where the file holds its cover picture.
    pub cover: Option<CoverRange>,
}

impl Known {
    /// What `info` and `metadata` say of a track.
    #[must_use]
    pub fn of(info: SoundInfo, metadata: &Metadata) -> Self {
        let tag = |wanted: TagKind| {
            metadata
                .tags
                .iter()
                .find(|tag| tag.kind == wanted)
                .map(|tag| tag.value.clone())
        };
        Self {
            info,
            title: tag(TagKind::Title),
            artist: tag(TagKind::Artist),
            album: tag(TagKind::Album),
            cover: metadata.cover,
        }
    }

    /// The track's length, where its stream states it.
    #[must_use]
    pub fn length(&self) -> Option<Span> {
        self.info
            .frames
            .map(|frames| Span::of_frames(frames, self.info.rate.hz()))
    }
}

/// One entry as the window shows it.
#[derive(Clone, Debug, PartialEq)]
pub struct Row {
    /// The file's own name, which stands for its title until one is read.
    pub name: String,
    /// What it was found to be.
    pub track: Track,
}

impl Row {
    /// What the row is called: its title, else its file's name.
    #[must_use]
    pub fn title(&self) -> &str {
        match &self.track {
            Track::Known(Known {
                title: Some(title), ..
            }) => title,
            _ => &self.name,
        }
    }

    /// Who performed it, where that is known.
    #[must_use]
    pub fn artist(&self) -> &str {
        match &self.track {
            Track::Known(Known {
                artist: Some(artist),
                ..
            }) => artist,
            _ => "",
        }
    }

    /// What was read of it.
    #[must_use]
    pub fn known(&self) -> Option<&Known> {
        match &self.track {
            Track::Known(known) => Some(known),
            Track::Reading | Track::Unreadable => None,
        }
    }
}

/// An output device the audio service lists.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Device {
    /// The service's name for it this boot.
    pub id: u32,
    /// The `audio:` reference it is remembered by: where it is, which outlives
    /// the boot its id belongs to.
    pub target: String,
    /// What it is called.
    pub name: String,
}

impl Device {
    /// The device `descriptor` lists.
    #[must_use]
    pub fn of(descriptor: &AudioDeviceDescriptor) -> Self {
        Self {
            id: descriptor.device_id,
            target: alloc::format!("{}", AudioTarget::at(descriptor)),
            name: String::from(descriptor.name.as_str()),
        }
    }
}

/// What the window asks of the playback thread.
#[derive(Clone, Debug, PartialEq)]
pub enum Command {
    /// A transport control.
    Control(Control),
    /// A change to the playlist.
    Edit(Edit),
}

/// What the window asks of its worker.
#[derive(Clone, Debug, PartialEq)]
pub enum Request {
    /// Read the tags and shape of this entry's file.
    Probe(EntryId),
    /// Decode this entry's cover, held at this range of its file, to a
    /// square of this side.
    Art {
        /// The entry.
        entry: EntryId,
        /// Where its file holds the picture.
        cover: CoverRange,
        /// The picture's side, in pixels.
        side: u32,
    },
    /// List the output devices.
    Devices,
    /// Save what the listener set.
    Save(Saved),
}

/// What the player keeps for the listener between runs.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Saved {
    /// The stream's level.
    pub gain: AudioGain,
    /// Whether the playlist plays shuffled.
    pub shuffle: bool,
    /// What plays again when it runs out.
    pub repeat: Repeat,
    /// Whether each track plays at its own stated loudness.
    pub normalise: bool,
    /// The `audio:` reference of the chosen output, or the default.
    pub device: Option<String>,
}

impl Default for Saved {
    fn default() -> Self {
        Self {
            gain: AudioGain::UNITY,
            shuffle: false,
            repeat: Repeat::Off,
            normalise: true,
            device: None,
        }
    }
}

/// What an input came to, beyond the state and damage it changed.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Outcome {
    /// For the playback thread, in order.
    pub commands: Vec<Command>,
    /// For the worker, in order.
    pub requests: Vec<Request>,
    /// The session's picker, asked to choose this.
    pub pick: Option<PickPurpose>,
    /// The context menu, asked to open here.
    pub menu: Option<Point>,
    /// The listener asked to quit.
    pub quit: bool,
}

/// The transport's controls.
struct Controls {
    previous: IconButton,
    play: IconButton,
    next: IconButton,
    shuffle: IconButton,
    repeat: IconButton,
    seek: Slider,
    volume: Slider,
    scroll: ScrollBar,
}

/// The player.
pub struct Player {
    playlist: Playlist<Row>,
    /// Every read row's length summed exactly, so taking a row out takes back
    /// what it added and stating the total walks nothing.
    listed_nanos: u128,
    next_entry: u64,
    selected: Option<EntryId>,
    status: Status,
    /// The seek slider's place while it is dragged, which the playback
    /// position does not overwrite.
    seeking: Option<u16>,
    saved: Saved,
    devices: Vec<Device>,
    notice: Option<String>,
    rng: NonCryptoRng,
    clicks: DoubleClickTracker,
    double_click: Duration64,
    /// The entry the context menu was opened on.
    menu_for: Option<EntryId>,
    /// Where the pointer last was, which a press acts at.
    pointer: Point,
    controls: Controls,
}

impl Player {
    /// A player with an empty playlist, set as `saved` says, drawing its
    /// shuffles from `seed`.
    #[must_use]
    pub fn new(saved: Saved, seed: u64, double_click: Duration64) -> Self {
        let mut rng = NonCryptoRng::seed_from_u64(seed);
        let mut playlist = Playlist::default();
        if saved.shuffle {
            playlist.apply(Edit::Shuffle(Some(rng.next_u64())));
        }
        playlist.apply(Edit::Repeat(saved.repeat));
        let mut controls = Controls {
            previous: IconButton::new(IconKind::SkipPrevious, ControlRole::Neutral),
            play: IconButton::new(IconKind::Resume, ControlRole::Primary),
            next: IconButton::new(IconKind::SkipNext, ControlRole::Neutral),
            shuffle: IconButton::new(IconKind::Shuffle, ControlRole::Neutral),
            repeat: IconButton::new(repeat_icon(saved.repeat), ControlRole::Neutral),
            seek: Slider::new(0),
            volume: Slider::new(permille_of_level(saved.gain)),
            scroll: ScrollBar::new(
                ScrollOrientation::Vertical,
                ScrollModel::new(ScrollRange::new(0, 0, 0), 1, 1),
            ),
        };
        mark(&mut controls.shuffle, saved.shuffle);
        mark(&mut controls.repeat, saved.repeat != Repeat::Off);
        Self {
            playlist,
            listed_nanos: 0,
            next_entry: 1,
            selected: None,
            status: Status::new(saved.gain),
            seeking: None,
            saved,
            devices: Vec::new(),
            notice: None,
            rng,
            clicks: DoubleClickTracker::new(),
            double_click,
            menu_for: None,
            pointer: Point::ORIGIN,
            controls,
        }
    }

    /// The edits that bring a fresh playback thread's playlist to this one's
    /// shuffle and repeat.
    #[must_use]
    pub fn opening_edits(&self) -> [Edit; 2] {
        [
            Edit::Shuffle(self.playlist.shuffle()),
            Edit::Repeat(self.playlist.repeat()),
        ]
    }

    /// The playlist as the window shows it.
    #[must_use]
    pub fn playlist(&self) -> &Playlist<Row> {
        &self.playlist
    }

    /// How long the rows read so far play for, together.
    #[must_use]
    pub fn listed_length(&self) -> Span {
        Span::from_nanos(u64::try_from(self.listed_nanos).unwrap_or(u64::MAX))
    }

    /// The rows in sight in `layout`, each with its place in the arrangement.
    pub fn rows_in_sight(&self, layout: &Layout) -> impl Iterator<Item = (usize, EntryId)> + '_ {
        let visible = layout.visible_rows(self.scroll());
        self.playlist
            .arranged()
            .iter()
            .copied()
            .enumerate()
            .skip(visible.start)
            .take(visible.len())
    }

    /// The entry selected.
    #[must_use]
    pub const fn selected(&self) -> Option<EntryId> {
        self.selected
    }

    /// What the playback thread last said.
    #[must_use]
    pub const fn status(&self) -> &Status {
        &self.status
    }

    /// The entry being heard.
    #[must_use]
    pub fn heard(&self) -> Option<EntryId> {
        self.status.heard.map(|(entry, _)| entry)
    }

    /// The line the status bar shows instead of the playlist's summary.
    #[must_use]
    pub fn notice(&self) -> Option<&str> {
        self.notice.as_deref()
    }

    /// What the listener set.
    #[must_use]
    pub const fn saved(&self) -> &Saved {
        &self.saved
    }

    /// The output devices last listed.
    #[must_use]
    pub fn devices(&self) -> &[Device] {
        &self.devices
    }

    /// The place the seek slider shows: the dragged one, else the one heard.
    #[must_use]
    pub fn shown_position(&self) -> Option<(Span, Span)> {
        let (_, info) = self.status.heard?;
        let hz = info.rate.hz();
        let length = Span::of_frames(info.frames?, hz);
        let here = match self.seeking {
            Some(permille) => travel_span(length, permille),
            None => Span::of_frames(self.status.position, hz),
        };
        Some((here, length))
    }

    /// The previous-track button.
    #[must_use]
    pub const fn previous_button(&self) -> &IconButton {
        &self.controls.previous
    }

    /// The play and pause button.
    #[must_use]
    pub const fn play_button(&self) -> &IconButton {
        &self.controls.play
    }

    /// The next-track button.
    #[must_use]
    pub const fn next_button(&self) -> &IconButton {
        &self.controls.next
    }

    /// The shuffle toggle.
    #[must_use]
    pub const fn shuffle_button(&self) -> &IconButton {
        &self.controls.shuffle
    }

    /// The repeat control.
    #[must_use]
    pub const fn repeat_button(&self) -> &IconButton {
        &self.controls.repeat
    }

    /// The seek slider.
    #[must_use]
    pub const fn seek_slider(&self) -> &Slider {
        &self.controls.seek
    }

    /// The volume slider.
    #[must_use]
    pub const fn volume_slider(&self) -> &Slider {
        &self.controls.volume
    }

    /// The playlist's scroll bar.
    #[must_use]
    pub const fn scroll_bar(&self) -> &ScrollBar {
        &self.controls.scroll
    }

    /// How far the playlist is scrolled, in pixels.
    #[must_use]
    pub fn scroll(&self) -> u32 {
        u32::try_from(self.controls.scroll.model().range().offset()).unwrap_or(u32::MAX)
    }

    /// Fit the playlist's scrolling to `layout`, as after a resize.
    pub fn relayout(&mut self, layout: &Layout) {
        let model = self.controls.scroll.model();
        let content = u64::try_from(self.playlist.len())
            .unwrap_or(u64::MAX)
            .saturating_mul(u64::from(layout.row_pitch()));
        let range = model
            .range()
            .resize(content, u64::from(layout.rows().height));
        let pitch = u64::from(layout.row_pitch().max(1));
        let page = u64::from(layout.rows().height).max(pitch);
        self.controls
            .scroll
            .set_model(ScrollModel::new(range, pitch, page));
    }

    /// Append files named `names` to the playlist, answering the entries
    /// they became; a stopped player begins playing the first.
    pub fn add(
        &mut self,
        names: Vec<String>,
        layout: &Layout,
        damage: &mut Region,
    ) -> (Vec<EntryId>, Outcome) {
        let mut outcome = Outcome::default();
        let mut added = Vec::with_capacity(names.len());
        for name in names {
            let entry = EntryId::new(self.next_entry);
            self.next_entry = self.next_entry.saturating_add(1);
            added.push((
                entry,
                Row {
                    name,
                    track: Track::Reading,
                },
            ));
            outcome.requests.push(Request::Probe(entry));
        }
        let entries: Vec<EntryId> = added.iter().map(|(entry, _)| *entry).collect();
        self.playlist.add(added);
        self.relayout(layout);
        if self.selected.is_none() {
            self.selected = entries.first().copied();
        }
        if let (true, Some(&first)) = (self.status.transport == Transport::Stopped, entries.first())
        {
            outcome
                .commands
                .push(Command::Control(Control::Jump(first)));
        }
        damage.add(layout.rows());
        damage.add(layout.scrollbar());
        damage.add(layout.status());
        (entries, outcome)
    }

    /// Adopt what the playback thread published, reporting the parts it
    /// changed: the meters alone when only the peaks moved.
    pub fn adopt(&mut self, status: Status, layout: &Layout, damage: &mut Region) -> Outcome {
        let mut outcome = Outcome::default();
        let before = core::mem::replace(&mut self.status, status);
        let heard = self.heard();
        let was = before.heard.map(|(entry, _)| entry);
        if heard != was {
            for part in [
                layout.art(),
                layout.title(),
                layout.subtitle(),
                layout.format(),
                layout.total(),
            ] {
                damage.add(part);
            }
            for entry in [was, heard].into_iter().flatten() {
                self.damage_row(entry, layout, damage);
            }
            if let Some(entry) = heard {
                outcome.requests.extend(self.art_request(entry, layout));
            }
        }
        if self.seeking.is_none() {
            let travel = self.position_travel();
            if travel != self.controls.seek.value() {
                self.controls.seek.set_value(travel);
                damage.add(layout.seek());
            }
        }
        let clock = |status: &Status| {
            status
                .heard
                .map(|(_, info)| Span::of_frames(status.position, info.rate.hz()).seconds())
        };
        if clock(&before) != clock(&self.status) || heard != was {
            damage.add(layout.elapsed());
        }
        if before.transport != self.status.transport {
            self.controls.play = IconButton::new(
                if self.status.transport == Transport::Playing {
                    IconKind::Pause
                } else {
                    IconKind::Resume
                },
                ControlRole::Primary,
            );
            damage.add(layout.play());
            damage.add(layout.status());
        }
        if before.peaks != self.status.peaks {
            damage.add(layout.meters());
        }
        outcome
    }

    /// What was read of `entry`'s file: its stream and tags, or that it could
    /// not be read.
    pub fn probed(
        &mut self,
        entry: EntryId,
        read: Option<(SoundInfo, &Metadata)>,
        layout: &Layout,
        damage: &mut Region,
    ) -> Outcome {
        let mut outcome = Outcome::default();
        let Some(row) = self.playlist.get_mut(entry) else {
            return outcome;
        };
        let was = length_nanos(row);
        row.track = match read {
            Some((info, metadata)) => Track::Known(Known::of(info, metadata)),
            None => Track::Unreadable,
        };
        self.listed_nanos = self.listed_nanos - was + length_nanos(row);
        self.damage_row(entry, layout, damage);
        damage.add(layout.status());
        if self.heard() == Some(entry) {
            damage.add(layout.title());
            damage.add(layout.subtitle());
            outcome.requests.extend(self.art_request(entry, layout));
        }
        outcome
    }

    /// `entry`'s cover arrived, answering whether it is on show.
    pub fn art_landed(&self, entry: EntryId, layout: &Layout, damage: &mut Region) -> bool {
        let shown = self.heard() == Some(entry);
        if shown {
            damage.add(layout.art());
        }
        shown
    }

    /// The output devices the audio service now lists.
    pub fn devices_listed(&mut self, devices: Vec<Device>) {
        self.devices = devices;
    }

    /// Show `notice` on the status line until the next.
    pub fn notify(&mut self, notice: String, layout: &Layout, damage: &mut Region) {
        self.notice = Some(notice);
        damage.add(layout.status());
    }

    /// One input event, at `now_ns`.
    pub fn input(
        &mut self,
        event: &InputEvent,
        now_ns: u64,
        layout: &Layout,
        scale: Scale,
        theme: &Theme,
        damage: &mut Region,
    ) -> Outcome {
        match *event {
            InputEvent::KeyPressed { key, modifiers } => self.key(
                key,
                modifiers.ctrl,
                modifiers.shift,
                modifiers.alt,
                layout,
                damage,
            ),
            InputEvent::PointerScrolled { dy, dx } => {
                if let Some(ScrollAction::ScrollTo { offset }) =
                    self.controls
                        .scroll
                        .wheel(dx, dy, scale, layout.scrollbar(), damage)
                {
                    self.scroll_to(offset, layout, damage);
                }
                Outcome::default()
            }
            _ => self.pointer(event, now_ns, layout, scale, theme, damage),
        }
    }

    /// Route a pointer event to the control under it.
    fn pointer(
        &mut self,
        event: &InputEvent,
        now_ns: u64,
        layout: &Layout,
        scale: Scale,
        theme: &Theme,
        damage: &mut Region,
    ) -> Outcome {
        let mut outcome = Outcome::default();
        if let InputEvent::PointerMoved { to } = *event {
            self.pointer = to;
        }
        if let Some(action) =
            self.controls
                .seek
                .on_pointer(event, layout.seek(), scale, theme, damage)
        {
            self.seek_action(action, layout, damage, &mut outcome);
        }
        if let Some(action) =
            self.controls
                .volume
                .on_pointer(event, layout.volume(), scale, theme, damage)
        {
            self.volume_action(action, &mut outcome);
        }
        if let Some(ScrollAction::ScrollTo { offset }) =
            self.controls
                .scroll
                .on_pointer(event, layout.scrollbar(), scale, theme, damage)
        {
            self.scroll_to(offset, layout, damage);
        }
        // Every button sees every event, so a hover or a press elsewhere
        // clears; at most one is activated by it.
        let mut pressed = None;
        for (slot, (bounds, button)) in [
            (layout.previous(), &mut self.controls.previous),
            (layout.play(), &mut self.controls.play),
            (layout.next(), &mut self.controls.next),
            (layout.shuffle(), &mut self.controls.shuffle),
            (layout.repeat(), &mut self.controls.repeat),
        ]
        .into_iter()
        .enumerate()
        {
            if button.on_pointer(event, bounds, damage) == Some(ButtonAction::Activated) {
                pressed = Some(slot);
            }
        }
        match pressed {
            Some(0) => outcome.commands.push(Command::Control(Control::Previous)),
            Some(1) => self.play_or_pause(&mut outcome),
            Some(2) => outcome.commands.push(Command::Control(Control::Next)),
            Some(3) => self.toggle_shuffle(layout, damage, &mut outcome),
            Some(4) => self.cycle_repeat(layout, damage, &mut outcome),
            _ => {}
        }
        if let InputEvent::PointerPressed { button } = *event {
            self.press_rows(button, now_ns, layout, damage, &mut outcome);
        }
        outcome
    }

    /// A press over the playlist: a primary press selects a row and a
    /// double-press plays it; a secondary press asks for the context menu.
    fn press_rows(
        &mut self,
        button: PointerButton,
        now_ns: u64,
        layout: &Layout,
        damage: &mut Region,
        outcome: &mut Outcome,
    ) {
        let point = self.pointer;
        if !layout.rows().contains(point) {
            return;
        }
        let entry = layout
            .row_at(point.y, self.scroll())
            .and_then(|index| self.playlist.arranged().get(index).copied());
        if let Some(entry) = entry {
            self.select(Some(entry), layout, damage);
        }
        match button {
            PointerButton::Secondary => {
                self.menu_for = entry;
                outcome.menu = Some(point);
            }
            PointerButton::Primary => {
                let Some(entry) = entry else {
                    self.clicks.reset();
                    return;
                };
                if self
                    .clicks
                    .register(now_ns, entry.get(), button, self.double_click)
                    == ClickKind::Double
                {
                    outcome
                        .commands
                        .push(Command::Control(Control::Jump(entry)));
                }
            }
            PointerButton::Middle => {}
        }
    }

    fn key(
        &mut self,
        key: Key,
        ctrl: bool,
        shift: bool,
        alt: bool,
        layout: &Layout,
        damage: &mut Region,
    ) -> Outcome {
        let mut outcome = Outcome::default();
        match key {
            Key::Char(' ') => self.play_or_pause(&mut outcome),
            Key::Char('o' | 'O') if ctrl => {
                outcome.pick = Some(if shift {
                    PickPurpose::Folder
                } else {
                    PickPurpose::Open
                });
            }
            Key::Char('+' | '=') => {
                self.step_level(LEVEL_STEP_MILLIBEL, layout, damage, &mut outcome);
            }
            Key::Char('-' | '_') => {
                self.step_level(-LEVEL_STEP_MILLIBEL, layout, damage, &mut outcome);
            }
            Key::Char('s' | 'S') if !ctrl => self.toggle_shuffle(layout, damage, &mut outcome),
            Key::Char('r' | 'R') if !ctrl => self.cycle_repeat(layout, damage, &mut outcome),
            Key::Named(NamedKey::Enter) => {
                if let Some(entry) = self.selected {
                    outcome
                        .commands
                        .push(Command::Control(Control::Jump(entry)));
                }
            }
            Key::Named(NamedKey::Delete) => {
                if let Some(entry) = self.selected {
                    self.remove(entry, layout, damage, &mut outcome);
                }
            }
            Key::Named(NamedKey::Up) if alt => {
                self.shift_selected(-1, layout, damage, &mut outcome);
            }
            Key::Named(NamedKey::Down) if alt => {
                self.shift_selected(1, layout, damage, &mut outcome);
            }
            Key::Named(NamedKey::Up) => self.step_selection(-1, layout, damage),
            Key::Named(NamedKey::Down) => self.step_selection(1, layout, damage),
            Key::Named(NamedKey::PageUp) => self.step_selection(-page_rows(layout), layout, damage),
            Key::Named(NamedKey::PageDown) => {
                self.step_selection(page_rows(layout), layout, damage);
            }
            Key::Named(NamedKey::Home) => self.step_selection(isize::MIN, layout, damage),
            Key::Named(NamedKey::End) => self.step_selection(isize::MAX, layout, damage),
            Key::Named(NamedKey::Left) => outcome.commands.push(Command::Control(if ctrl {
                Control::Previous
            } else {
                Control::Back
            })),
            Key::Named(NamedKey::Right) => outcome.commands.push(Command::Control(if ctrl {
                Control::Next
            } else {
                Control::Forward
            })),
            _ => {}
        }
        outcome
    }

    /// The context menu for where it was last asked: a row's own commands
    /// over the player's.
    #[must_use]
    pub fn context_menu(&self) -> AppMenu {
        let mut menu = MenuBuilder::new();
        if let Some(entry) = self.menu_for {
            let position = self.playlist.position(entry).unwrap_or(0);
            menu.item(row::PLAY, "Play", "Enter", true, Plate::Root);
            menu.item(row::REMOVE, "Remove", "Delete", true, Plate::Root);
            menu.item(row::MOVE_UP, "Move up", "Alt+Up", position > 0, Plate::Root);
            menu.item(
                row::MOVE_DOWN,
                "Move down",
                "Alt+Down",
                position + 1 < self.playlist.len(),
                Plate::Root,
            );
            menu.separator(Plate::Root);
        }
        menu.item(row::OPEN_FILE, "Open file…", "Ctrl+O", true, Plate::Root);
        menu.item(
            row::OPEN_FOLDER,
            "Open folder…",
            "Ctrl+Shift+O",
            true,
            Plate::Root,
        );
        menu.item(
            row::CLEAR,
            "Clear the list",
            "",
            !self.playlist.is_empty(),
            Plate::Root,
        );
        menu.separator(Plate::Root);
        menu.mark(
            row::SHUFFLE,
            "Shuffle",
            "S",
            self.saved.shuffle,
            Plate::Root,
        );
        if let Some(plate) = menu.submenu("Repeat", Plate::Root) {
            for (id, label, mode) in REPEATS {
                menu.radio(id, label, "", self.saved.repeat == mode, plate);
            }
        }
        menu.mark(
            row::NORMALISE,
            "Level tracks by their own loudness",
            "",
            self.saved.normalise,
            Plate::Root,
        );
        if let Some(plate) = menu.submenu("Output", Plate::Root) {
            menu.radio(
                row::DEFAULT_DEVICE,
                "Default",
                "",
                self.saved.device.is_none(),
                plate,
            );
            for (id, device) in (row::FIRST_DEVICE..).zip(&self.devices) {
                let chosen = self.saved.device.as_deref() == Some(device.target.as_str());
                menu.radio(id, &device.name, "", chosen, plate);
            }
        }
        menu.finish()
    }

    /// The rows the icon bar's menu carries above its Quit.
    #[must_use]
    pub fn bar_rows(&self) -> Vec<AppMenuRow> {
        let row = |id: u16, label: &str, mark: AppMenuMark| {
            let id = AppMenuItemId::new(id).ok()?;
            let label = AppMenuLabel::new(label).ok()?;
            Some(AppMenuRow::Item(
                AppMenuItem::new(id, label).with_mark(mark),
            ))
        };
        let tick = |on: bool| {
            if on {
                AppMenuMark::Check
            } else {
                AppMenuMark::None
            }
        };
        let radio = |on: bool| {
            if on {
                AppMenuMark::Radio
            } else {
                AppMenuMark::None
            }
        };
        [
            row(row::OPEN_FILE, "Open file…", AppMenuMark::None),
            row(row::OPEN_FOLDER, "Open folder…", AppMenuMark::None),
            Some(AppMenuRow::Separator),
            row(row::SHUFFLE, "Shuffle", tick(self.saved.shuffle)),
            row(
                row::REPEAT_OFF,
                "Repeat off",
                radio(self.saved.repeat == Repeat::Off),
            ),
            row(
                row::REPEAT_ALL,
                "Repeat the list",
                radio(self.saved.repeat == Repeat::All),
            ),
            row(
                row::REPEAT_ONE,
                "Repeat the track",
                radio(self.saved.repeat == Repeat::One),
            ),
        ]
        .into_iter()
        .flatten()
        .collect()
    }

    /// The menu row `id` was chosen, from either menu.
    pub fn choose(&mut self, id: u16, layout: &Layout, damage: &mut Region) -> Outcome {
        let mut outcome = Outcome::default();
        match id {
            row::OPEN_FILE => outcome.pick = Some(PickPurpose::Open),
            row::OPEN_FOLDER => outcome.pick = Some(PickPurpose::Folder),
            row::CLEAR => self.clear(layout, damage, &mut outcome),
            row::SHUFFLE => self.toggle_shuffle(layout, damage, &mut outcome),
            row::NORMALISE => {
                self.saved.normalise = !self.saved.normalise;
                outcome
                    .commands
                    .push(Command::Control(Control::SetNormalise(
                        self.saved.normalise,
                    )));
                outcome.requests.push(Request::Save(self.saved.clone()));
            }
            row::PLAY | row::REMOVE | row::MOVE_UP | row::MOVE_DOWN => {
                let Some(entry) = self.menu_for else {
                    return outcome;
                };
                match id {
                    row::PLAY => outcome
                        .commands
                        .push(Command::Control(Control::Jump(entry))),
                    row::REMOVE => self.remove(entry, layout, damage, &mut outcome),
                    row::MOVE_UP => self.move_entry(entry, -1, layout, damage, &mut outcome),
                    _ => self.move_entry(entry, 1, layout, damage, &mut outcome),
                }
            }
            row::DEFAULT_DEVICE => self.choose_device(None, &mut outcome),
            _ => {
                if let Some(mode) = REPEATS
                    .iter()
                    .find(|(repeat_row, ..)| *repeat_row == id)
                    .map(|(.., mode)| *mode)
                {
                    self.set_repeat(mode, layout, damage, &mut outcome);
                } else if let Some(index) = id
                    .checked_sub(row::FIRST_DEVICE)
                    .map(usize::from)
                    .filter(|&index| index < self.devices.len())
                {
                    self.choose_device(Some(index), &mut outcome);
                }
            }
        }
        outcome
    }

    fn choose_device(&mut self, index: Option<usize>, outcome: &mut Outcome) {
        let device = index.and_then(|index| self.devices.get(index));
        self.saved.device = device.map(|device| device.target.clone());
        outcome.commands.push(Command::Control(Control::SetDevice(
            device.map_or(0, |device| device.id),
        )));
        outcome.requests.push(Request::Save(self.saved.clone()));
    }

    fn play_or_pause(&mut self, outcome: &mut Outcome) {
        if self.status.transport != Transport::Stopped {
            outcome
                .commands
                .push(Command::Control(Control::TogglePause));
            return;
        }
        let first = self
            .selected
            .or_else(|| self.playlist.arranged().first().copied());
        if let Some(entry) = first {
            outcome
                .commands
                .push(Command::Control(Control::Jump(entry)));
        }
    }

    fn seek_action(
        &mut self,
        action: SliderAction,
        layout: &Layout,
        damage: &mut Region,
        outcome: &mut Outcome,
    ) {
        damage.add(layout.elapsed());
        match action {
            SliderAction::SetValue { permille } => self.seeking = Some(permille),
            SliderAction::Settled { permille } => {
                self.seeking = None;
                if let Some((_, length)) = self.shown_position() {
                    outcome
                        .commands
                        .push(Command::Control(Control::SeekTo(travel_span(
                            length, permille,
                        ))));
                }
            }
        }
    }

    fn volume_action(&mut self, action: SliderAction, outcome: &mut Outcome) {
        let (SliderAction::SetValue { permille } | SliderAction::Settled { permille }) = action;
        let gain = level_at_permille(permille);
        if gain != self.saved.gain {
            self.saved.gain = gain;
            outcome
                .commands
                .push(Command::Control(Control::SetGain(gain)));
        }
        if matches!(action, SliderAction::Settled { .. }) {
            outcome.requests.push(Request::Save(self.saved.clone()));
        }
    }

    fn step_level(
        &mut self,
        step: i32,
        layout: &Layout,
        damage: &mut Region,
        outcome: &mut Outcome,
    ) {
        let level = self
            .saved
            .gain
            .millibel()
            .saturating_add(step)
            .clamp(DEFAULT_FLOOR_MILLIBEL, 0);
        let Ok(gain) = AudioGain::new(level) else {
            return;
        };
        if gain == self.saved.gain {
            return;
        }
        self.saved.gain = gain;
        self.controls.volume.set_value(permille_of_level(gain));
        damage.add(layout.volume());
        outcome
            .commands
            .push(Command::Control(Control::SetGain(gain)));
        outcome.requests.push(Request::Save(self.saved.clone()));
    }

    fn toggle_shuffle(&mut self, layout: &Layout, damage: &mut Region, outcome: &mut Outcome) {
        self.saved.shuffle = !self.saved.shuffle;
        let seed = self.saved.shuffle.then(|| self.rng.next_u64());
        self.edit(Edit::Shuffle(seed), outcome);
        mark(&mut self.controls.shuffle, self.saved.shuffle);
        damage.add(layout.shuffle());
        outcome.requests.push(Request::Save(self.saved.clone()));
    }

    fn cycle_repeat(&mut self, layout: &Layout, damage: &mut Region, outcome: &mut Outcome) {
        self.set_repeat(self.saved.repeat.next(), layout, damage, outcome);
    }

    fn set_repeat(
        &mut self,
        mode: Repeat,
        layout: &Layout,
        damage: &mut Region,
        outcome: &mut Outcome,
    ) {
        if mode == self.saved.repeat {
            return;
        }
        self.saved.repeat = mode;
        self.edit(Edit::Repeat(mode), outcome);
        self.controls.repeat = IconButton::new(repeat_icon(mode), ControlRole::Neutral);
        mark(&mut self.controls.repeat, mode != Repeat::Off);
        damage.add(layout.repeat());
        outcome.requests.push(Request::Save(self.saved.clone()));
    }

    fn remove(
        &mut self,
        entry: EntryId,
        layout: &Layout,
        damage: &mut Region,
        outcome: &mut Outcome,
    ) {
        if self.selected == Some(entry) {
            let at = self.playlist.position(entry).unwrap_or(0);
            let arranged = self.playlist.arranged();
            self.selected = arranged
                .get(at + 1)
                .or_else(|| at.checked_sub(1).and_then(|before| arranged.get(before)))
                .copied();
        }
        if let Some(row) = self.playlist.get(entry) {
            self.listed_nanos -= length_nanos(row);
        }
        self.edit(Edit::Remove(alloc::vec![entry]), outcome);
        self.playlist.settle();
        self.relayout(layout);
        damage.add(layout.rows());
        damage.add(layout.scrollbar());
        damage.add(layout.status());
    }

    fn clear(&mut self, layout: &Layout, damage: &mut Region, outcome: &mut Outcome) {
        self.selected = None;
        self.listed_nanos = 0;
        self.edit(Edit::Clear, outcome);
        self.playlist.settle();
        self.relayout(layout);
        damage.add(layout.rows());
        damage.add(layout.scrollbar());
        damage.add(layout.status());
    }

    fn shift_selected(
        &mut self,
        by: isize,
        layout: &Layout,
        damage: &mut Region,
        outcome: &mut Outcome,
    ) {
        if let Some(entry) = self.selected {
            self.move_entry(entry, by, layout, damage, outcome);
        }
    }

    fn move_entry(
        &mut self,
        entry: EntryId,
        by: isize,
        layout: &Layout,
        damage: &mut Region,
        outcome: &mut Outcome,
    ) {
        let Some(at) = self.playlist.position(entry) else {
            return;
        };
        let Some(to) = at
            .checked_add_signed(by)
            .filter(|&to| to < self.playlist.len())
        else {
            return;
        };
        self.edit(Edit::Move { entry, to }, outcome);
        damage.add(layout.row(at, self.scroll()));
        damage.add(layout.row(to, self.scroll()));
        self.reveal(to, layout, damage);
    }

    /// Make `edit` here and have the playback thread make it too.
    fn edit(&mut self, edit: Edit, outcome: &mut Outcome) {
        self.playlist.apply(edit.clone());
        outcome.commands.push(Command::Edit(edit));
    }

    fn step_selection(&mut self, by: isize, layout: &Layout, damage: &mut Region) {
        let last = self.playlist.len().checked_sub(1);
        let Some(last) = last else {
            return;
        };
        let at = self
            .selected
            .and_then(|entry| self.playlist.position(entry));
        let to = match at {
            None => 0,
            Some(at) => at.saturating_add_signed(by).min(last),
        };
        let entry = self.playlist.arranged().get(to).copied();
        self.select(entry, layout, damage);
        self.reveal(to, layout, damage);
    }

    fn select(&mut self, entry: Option<EntryId>, layout: &Layout, damage: &mut Region) {
        if entry == self.selected {
            return;
        }
        for marked in [self.selected, entry].into_iter().flatten() {
            self.damage_row(marked, layout, damage);
        }
        self.selected = entry;
    }

    /// Scroll the least that brings row `index` into sight.
    fn reveal(&mut self, index: usize, layout: &Layout, damage: &mut Region) {
        let pitch = u64::from(layout.row_pitch());
        let top = u64::try_from(index)
            .unwrap_or(u64::MAX)
            .saturating_mul(pitch);
        let viewport = u64::from(layout.rows().height);
        let offset = self.controls.scroll.model().range().offset();
        let wanted = if top < offset {
            top
        } else if top.saturating_add(pitch) > offset.saturating_add(viewport) {
            top.saturating_add(pitch).saturating_sub(viewport)
        } else {
            return;
        };
        self.scroll_to(wanted, layout, damage);
    }

    fn scroll_to(&mut self, offset: u64, layout: &Layout, damage: &mut Region) {
        let model = self.controls.scroll.model();
        let range = model.range().with_offset(offset);
        if range.offset() == model.range().offset() {
            return;
        }
        self.controls.scroll.set_model(ScrollModel::new(
            range,
            model.line_step(),
            model.page_step(),
        ));
        damage.add(layout.rows());
        damage.add(layout.scrollbar());
    }

    /// Repaint `entry`'s row where it is in sight: a row out of sight has
    /// nothing on screen, and finding it would cost a walk of the list.
    fn damage_row(&self, entry: EntryId, layout: &Layout, damage: &mut Region) {
        if let Some((index, _)) = self.rows_in_sight(layout).find(|&(_, held)| held == entry) {
            damage.add(layout.row(index, self.scroll()));
        }
    }

    /// The cover to fetch for `entry`, when it has one.
    fn art_request(&self, entry: EntryId, layout: &Layout) -> Option<Request> {
        let cover = self.playlist.get(entry)?.known()?.cover?;
        let side = layout.art().width;
        (side > 0).then_some(Request::Art { entry, cover, side })
    }

    /// The seek slider's place for the position heard.
    fn position_travel(&self) -> u16 {
        let Some((_, info)) = self.status.heard else {
            return 0;
        };
        let Some(frames) = info.frames.filter(|&frames| frames > 0) else {
            return 0;
        };
        let travel =
            u128::from(self.status.position.min(frames)) * u128::from(TRAVEL) / u128::from(frames);
        u16::try_from(travel).unwrap_or(TRAVEL)
    }
}

/// `row`'s length in nanoseconds, where it has been read.
fn length_nanos(row: &Row) -> u128 {
    row.known()
        .and_then(Known::length)
        .map_or(0, |length| u128::from(length.nanos()))
}

/// How many rows a page of the playlist holds.
fn page_rows(layout: &Layout) -> isize {
    let rows = layout.rows().height / layout.row_pitch().max(1);
    isize::try_from(rows.max(1)).unwrap_or(1)
}

/// The repeat rows of a menu, by mode.
const REPEATS: [(u16, &str, Repeat); 3] = [
    (row::REPEAT_OFF, "Off", Repeat::Off),
    (row::REPEAT_ALL, "The whole list", Repeat::All),
    (row::REPEAT_ONE, "This track", Repeat::One),
];

/// The repeat control's glyph for `mode`.
const fn repeat_icon(mode: Repeat) -> IconKind {
    match mode {
        Repeat::One => IconKind::RepeatOne,
        Repeat::Off | Repeat::All => IconKind::Repeat,
    }
}

/// Show `button` latched on or off.
fn mark(button: &mut IconButton, on: bool) {
    let mut state = button.state();
    state.selection = if on {
        SelectionState::Selected
    } else {
        SelectionState::Unselected
    };
    button.set_state(state);
}

/// The place `permille` of the way through a track of `length` is.
fn travel_span(length: Span, permille: u16) -> Span {
    let nanos = u128::from(length.nanos()) * u128::from(permille.min(TRAVEL)) / u128::from(TRAVEL);
    Span::from_nanos(u64::try_from(nanos).unwrap_or(u64::MAX))
}

#[cfg(test)]
#[path = "view_tests.rs"]
mod tests;
