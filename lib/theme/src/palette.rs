//! The colour roles a theme defines.
//!
//! A [`Palette`] is a fixed set of *semantic* colour roles, not a free-form
//! map: every role is a named field, so a theme cannot omit one and a
//! consumer cannot ask for a role that does not exist (illegal states are
//! unrepresentable). The window manager, the taskbar,
//! and the default apps all read these same roles, which is what makes a
//! theme switch apply consistently everywhere from one definition.
//!
//! The built-in themes' own palettes are [`Palette::DARK`] and
//! [`Palette::LIGHT`], defined here once.

use tairix_abi::sysinfo::VolumeHealth;

use crate::syntax::{SyntaxPalette, SyntaxRole};
use tairix_colour::Rgba;

/// The semantic colours every theme provides.
///
/// Roles are intentionally generic ("surface", "on-surface") rather than
/// per-widget ("button", "menu") so that adding a widget needs no new role
/// and adding a theme needs no new code — a theme is data.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct Palette {
    /// The empty desktop behind every window (the compositor root).
    pub desktop: Rgba,
    /// The base fill of a window or app content area.
    pub surface: Rgba,
    /// A raised/alternate fill (the taskbar, menus, headers) that must
    /// read as distinct from [`surface`](Self::surface).
    pub surface_raised: Rgba,
    /// The ground a document's own content is drawn on: an editor's or a
    /// terminal's page, an editable field's plate.
    ///
    /// Its own role rather than [`surface`](Self::surface), because a window
    /// and the page inside it are two things: the window ground is chrome the
    /// eye reads past, while the page is what the user is looking at and is
    /// paper on a light appearance and the deepest layer on a dark one. A
    /// field that only *shows* a value stays on the plate ladder — this is
    /// the ground of something being written.
    pub document: Rgba,
    /// The ground every title band is drawn on: a window's furniture bar, and
    /// the band a menu or dialog plate is capped with.
    ///
    /// One role for both, because they are one control with and without a
    /// command set. It separates from [`surface`](Self::surface) in the
    /// direction the appearance calls for — deeper on a light theme so a
    /// window's furniture reads as chrome rather than as more page, lifted on
    /// a dark one.
    pub title_band: Rgba,
    /// How opaque a *floating desktop-chrome* surface is — the taskbar and
    /// the popups it puts on screen, laid over a backdrop blurred by
    /// [`Metrics::chrome_backdrop_blur`](crate::Metrics::chrome_backdrop_blur).
    ///
    /// The surface keeps whichever colour role it would wear solid and takes
    /// this alpha instead, so a frosted bar is recognisably the same grey as a
    /// solid one and the wallpaper behind it reads through as a wash. Anything
    /// that reads as *part* of that surface — a list row, a menu row — takes
    /// it too, which is what keeps a resting row exactly its ground.
    ///
    /// `255` draws chrome solid.
    pub chrome_alpha: u8,
    /// How opaque a control *plate* raised on floating chrome is — a button,
    /// a text field, a card.
    ///
    /// A step more solid than [`chrome_alpha`](Self::chrome_alpha), so a
    /// control reads as furniture standing on the glass rather than a hole cut
    /// in it, while the backdrop still shows through.
    pub chrome_plate_alpha: u8,
    /// Primary foreground (text, icons) drawn on
    /// [`surface`](Self::surface) and
    /// [`surface_raised`](Self::surface_raised).
    pub on_surface: Rgba,
    /// Secondary foreground for less prominent text on a surface.
    pub on_surface_muted: Rgba,
    /// The accent used for selection, focus, and the active task.
    pub accent: Rgba,
    /// Foreground drawn on top of [`accent`](Self::accent).
    pub on_accent: Rgba,
    /// The plate a selected item is filled with, laid crisply over a backdrop
    /// blurred by
    /// [`Metrics::selection_backdrop_blur`](crate::Metrics::selection_backdrop_blur).
    ///
    /// Translucent, so what lies behind a selected item — a window's
    /// surface, the desktop wallpaper — still reads through its selection
    /// rather than being replaced by a block of accent. It is authored per
    /// theme rather than derived from [`accent`](Self::accent) at the draw
    /// site so a theme can tune the fill's weight against its own surfaces.
    pub selection_fill: Rgba,
    /// What a selected item's picture is washed with, through the picture's
    /// own coverage: heavier than [`selection_fill`](Self::selection_fill),
    /// because it marks a picture rather than tinting a plate under a name.
    /// Authored per theme for the same reason.
    pub selection_tint: Rgba,
    /// Window and control borders / separators.
    pub border: Rgba,

    // --- Reactive Alloy control roles -----------------------------------
    /// The inner plate of a control while the pointer rests on it.
    ///
    /// A hovered control lightens its plate rather than only its edge, which
    /// is the only feedback available to a control that wears no perimeter of
    /// its own — an icon seated in the taskbar. It is therefore one clear step
    /// away from [`surface_raised`](Self::surface_raised), in whichever
    /// direction the appearance requires: brighter on a dark theme, deeper on
    /// a light one.
    pub surface_hover: Rgba,
    /// The inner plate of a control while it is pressed (darker than
    /// [`surface`](Self::surface)).
    pub surface_pressed: Rgba,
    /// The band filling the row a command surface will act on — a menu's
    /// current row, whichever the pointer or the keyboard put it on.
    ///
    /// A *selection*, not a hover, which is why it is not
    /// [`surface_hover`](Self::surface_hover): in a menu there is no separate
    /// selected state for a pointer to hover *towards*, so the current row is
    /// the choice itself and has to read like one. It is therefore a decisive
    /// band rather than a wash, and it is laid **solid** — a diluted mark on
    /// floating chrome leaves the row that will act no heavier than the rest.
    ///
    /// A shade of the surface, never a hue: the accent is spent on the roles
    /// that must shout across the whole desktop, and a menu highlighted in it
    /// competes with them. Only a warning or a danger row overrides this with
    /// its own colour.
    pub surface_selected: Rgba,
    /// The quiet perimeter (Signal Rim) of a resting control.
    pub rim: Rgba,
    /// The reactive perimeter of a hovered, focused, or active control.
    pub rim_active: Rgba,
    /// The danger role for destructive actions and refusals.
    pub danger: Rgba,

    // --- Semantic signal roles (spec §6) --------------------------------
    /// Compute saturation / CPU pressure.
    pub cpu_pressure: Rgba,
    /// Memory pressure.
    pub memory_pressure: Rgba,
    /// Storage throughput / disk pressure.
    pub disk_pressure: Rgba,
    /// Network transfer / remote I/O activity.
    pub network_activity: Rgba,
    /// Power / battery pressure.
    pub power_pressure: Rgba,
    /// Thermal pressure.
    pub thermal_pressure: Rgba,
    /// Graphics / display-path utilisation.
    pub gpu_pressure: Rgba,
    /// General-purpose accelerator utilisation.
    pub accelerator_pressure: Rgba,
    /// Hung, not-responding, repair, restart, or force-action state.
    pub recovery: Rgba,
    /// Completed, verified, recovered.
    pub success: Rgba,
    /// Elevated impact, caution, delayed risk.
    pub warning: Rgba,
    /// Missing authority or blocked action.
    pub denied: Rgba,
    /// What the machine is running: the task census, a process count.
    ///
    /// Deliberately its own role rather than a borrowed
    /// [`cpu_pressure`](Self::cpu_pressure): a count of tasks is not compute
    /// saturation, and drawing it in the compute hue makes a task trace read
    /// as a second CPU trace beside the real one.
    pub workload: Rgba,
    /// Data read from a storage device.
    pub disk_read: Rgba,
    /// Data written to a storage device.
    pub disk_write: Rgba,
    /// Data received on a network interface.
    pub net_receive: Rgba,
    /// Data sent on a network interface.
    pub net_send: Rgba,

    // --- Scroll and window-frame roles ----------------------------------
    /// The quiet Scroll Channel (track) behind a scrollbar thumb.
    pub scroll_track: Rgba,
    /// The scrollbar thumb.
    pub scroll_thumb: Rgba,
    /// The scrollbar thumb while hovered, pressed or dragged, and an end
    /// button while its press is live: a neutral step from
    /// [`scroll_thumb`](Self::scroll_thumb), never the accent, so a bar in use
    /// reads as handled rather than as selected.
    pub scroll_thumb_active: Rgba,
    /// The Frame Rim every window wears: one quiet neutral, a step lighter
    /// than [`surface`](Self::surface) on a dark appearance and a step deeper
    /// on a light one, so a window's edge separates it from the desktop
    /// without drawing the eye.
    ///
    /// The rim is deliberately *not* a focus signal. A frame that brightened
    /// when focused made the boundary — the one part of the chrome the eye
    /// tracks a window's shape by — the loudest thing on the desktop, and
    /// left every unfocused window looking switched off. Focus is carried by
    /// the title bar instead: its text sits at
    /// [`on_surface`](Self::on_surface) while active and
    /// [`on_surface_muted`](Self::on_surface_muted) while not, and under
    /// heavy contrast the active frame gains a second, inner rim line so the
    /// distinction is a difference in shape and not only in tone.
    pub frame: Rgba,
    /// The wash a window's furniture takes where its bevel faces the key
    /// light at the upper left: the top and left of the rim. Translucent, so
    /// it lifts whatever tone is beneath it rather than painting a colour of
    /// its own.
    pub bevel_light: Rgba,
    /// The wash the same bevel takes where it faces away from that light: the
    /// rim's bottom and right, and the foot of the title band.
    pub bevel_shade: Rgba,
    /// The shadow a floating surface casts, at its darkest: the colour, and in
    /// its alpha how dark the shadow is where it is deepest.
    pub drop_shadow: Rgba,
    /// The shadow a decorated window casts while it rests behind the family
    /// the user is working in: lighter than
    /// [`drop_shadow`](Self::drop_shadow), so the window in front reads above
    /// it.
    pub inactive_drop_shadow: Rgba,

    // --- Window-command highlight roles ---------------------------------
    //
    // A hue per title-bar command, so a lit one says *which* command it is
    // before its glyph is read. Kept separate from `danger` / `warning` /
    // `success`: retuning a signal hue for legibility must not repaint a
    // window button, and a command's hue is its identity, not a severity.
    /// The wash the close command highlights with, red.
    ///
    /// Authored translucent, like [`selection_fill`](Self::selection_fill), so
    /// the highlight tints the title bar rather than covering it.
    pub window_close: Rgba,
    /// The wash the minimize command highlights with, yellow.
    pub window_minimize: Rgba,
    /// The wash the size toggle highlights with, green — maximizing and
    /// restoring alike, because they are one command.
    pub window_maximize: Rgba,
    /// The wash the put-to-back command highlights with, blue.
    pub window_put_to_back: Rgba,

    /// How opaque the hue a title bar takes from its window's identity icon is
    /// where it is strongest, at the icon itself.
    ///
    /// An opacity rather than a colour, because the colour is the icon's: the
    /// bar carries its application's own hue, so the theme sets only how far
    /// through it reads. Low, and deliberately so — the wash has to stay a tint
    /// on the chrome rather than a second, blurrier copy of the icon beside the
    /// real one. `0` turns the wash off for a theme that wants plain chrome.
    pub title_hue_alpha: u8,

    /// The colours a document's syntax is drawn in ([`Palette::syntax`]).
    pub syntax: SyntaxPalette,
}

impl Palette {
    /// The built-in dark theme's colours.
    ///
    /// The tokens are the Reactive Alloy design boards (`plans/desktop1.png`,
    /// `plans/desktop2a.png`) measured rather than invented: near-black cool
    /// surfaces, one alloy-orange accent family (a burnt-orange plate fill
    /// under a bright signal edge), the semantic signal hues the boards' own
    /// legend fixes, and a hover plate one measured step above the bar fill —
    /// the boards draw a hovered icon on the taskbar as a bare wash roughly
    /// nine levels lighter than the bar behind it.
    pub const DARK: Self = {
        let accent = Rgba::rgb(0xd1, 0x55, 0x0f);
        Self {
            desktop: Rgba::rgb(0x0b, 0x0e, 0x10),
            surface: Rgba::rgb(0x0f, 0x13, 0x16),
            surface_raised: Rgba::rgb(0x15, 0x1b, 0x1f),
            document: Rgba::rgb(0x0a, 0x0d, 0x0f),
            title_band: Rgba::rgb(0x23, 0x2b, 0x31),
            chrome_alpha: CHROME_ALPHA,
            chrome_plate_alpha: CHROME_PLATE_ALPHA,
            on_surface: Rgba::rgb(0xe8, 0xeb, 0xed),
            on_surface_muted: Rgba::rgb(0x8e, 0x97, 0x9c),
            accent,
            on_accent: ON_ACCENT,
            selection_fill: accent.with_alpha(SELECTION_ALPHA),
            selection_tint: accent.with_alpha(SELECTION_TINT_ALPHA),
            border: Rgba::rgb(0x1c, 0x23, 0x27),
            surface_hover: Rgba::rgb(0x28, 0x31, 0x37),
            surface_pressed: Rgba::rgb(0x0b, 0x0f, 0x11),
            surface_selected: Rgba::rgb(0x3d, 0x47, 0x4e),
            rim: Rgba::rgb(0x23, 0x2b, 0x30),
            rim_active: Rgba::rgb(0xff, 0x60, 0x00),
            danger: Rgba::rgb(0xe4, 0x1d, 0x21),
            cpu_pressure: Rgba::rgb(0xf7, 0x62, 0x02),
            memory_pressure: Rgba::rgb(0x8b, 0x43, 0xd6),
            disk_pressure: Rgba::rgb(0xf8, 0xa3, 0x30),
            network_activity: Rgba::rgb(0x0b, 0x88, 0xd9),
            power_pressure: Rgba::rgb(0xa8, 0xd8, 0x4f),
            thermal_pressure: Rgba::rgb(0xff, 0x8a, 0x5c),
            gpu_pressure: Rgba::rgb(0x22, 0xb8, 0xa6),
            accelerator_pressure: Rgba::rgb(0xd9, 0x4f, 0x8c),
            recovery: Rgba::rgb(0xe8, 0x48, 0x4c),
            success: Rgba::rgb(0x6f, 0xb2, 0x3a),
            warning: Rgba::rgb(0xe8, 0xb1, 0x3a),
            denied: Rgba::rgb(0xb0, 0x3a, 0x3d),
            workload: Rgba::rgb(0x3f, 0xb9, 0x50),
            disk_read: Rgba::rgb(0x62, 0xcf, 0x7a),
            disk_write: Rgba::rgb(0xdb, 0x4a, 0x3a),
            net_receive: Rgba::rgb(0x2f, 0x9f, 0xe0),
            net_send: Rgba::rgb(0x7b, 0x6c, 0xe8),
            scroll_track: Rgba::rgb(0x13, 0x1a, 0x1d),
            scroll_thumb: Rgba::rgb(0x2f, 0x3a, 0x3f),
            scroll_thumb_active: Rgba::rgb(0x5a, 0x66, 0x6c),
            frame: Rgba::rgb(0x4b, 0x52, 0x57),
            bevel_light: Rgba::new(0xff, 0xff, 0xff, 0x40),
            bevel_shade: Rgba::new(0x00, 0x00, 0x00, 0x78),
            drop_shadow: Rgba::new(0x00, 0x00, 0x00, 0x8c),
            inactive_drop_shadow: Rgba::new(0x00, 0x00, 0x00, 0x54),
            window_close: Rgba::rgb(0xff, 0x3b, 0x30).with_alpha(COMMAND_ALPHA),
            window_minimize: Rgba::rgb(0xff, 0xc1, 0x0a).with_alpha(COMMAND_ALPHA),
            window_maximize: Rgba::rgb(0x34, 0xc7, 0x59).with_alpha(COMMAND_ALPHA),
            window_put_to_back: Rgba::rgb(0x0a, 0x93, 0xe6).with_alpha(COMMAND_ALPHA),
            title_hue_alpha: TITLE_HUE_ALPHA,
            syntax: SyntaxPalette::dark(),
        }
    };

    /// The built-in light theme's colours.
    ///
    /// The light board (`plans/desktop1-light.png`) keeps the dark variant's
    /// alloy-orange accent family and semantic vocabulary and re-tunes every
    /// signal hue until it carries on a light ground, with the accent deepened
    /// to the burnt end of the family so orange text and rims stay legible.
    ///
    /// The neutrals are one descending ladder of *neutral* greys — no warm
    /// cast, so a signal hue is the only colour on screen. The window ground
    /// is a mid-light grey rather than paper: white window grounds would leave
    /// every app reading as a blank sheet, with nothing for a plate to be
    /// raised off and nowhere for a title band to sit. Paper is
    /// [`document`](Self::document)'s alone, so the surfaces a user *writes*
    /// on are the white ones. Raised chrome — plates, menus, the taskbar —
    /// catches the light *above* the window ground, and the interaction ladder
    /// deepens away from it.
    pub const LIGHT: Self = {
        let accent = Rgba::rgb(0xc8, 0x50, 0x0c);
        Self {
            desktop: Rgba::rgb(0x80, 0x80, 0x80),
            surface: Rgba::rgb(0xd9, 0xd9, 0xd9),
            surface_raised: Rgba::rgb(0xec, 0xec, 0xec),
            document: Rgba::rgb(0xff, 0xff, 0xff),
            title_band: Rgba::rgb(0xbf, 0xbf, 0xbf),
            chrome_alpha: CHROME_ALPHA,
            chrome_plate_alpha: CHROME_PLATE_ALPHA,
            on_surface: Rgba::rgb(0x1b, 0x1d, 0x20),
            on_surface_muted: Rgba::rgb(0x6a, 0x6f, 0x75),
            accent,
            on_accent: ON_ACCENT,
            selection_fill: accent.with_alpha(SELECTION_ALPHA),
            selection_tint: accent.with_alpha(SELECTION_TINT_ALPHA),
            border: Rgba::rgb(0xad, 0xad, 0xad),
            surface_hover: Rgba::rgb(0xcc, 0xcc, 0xcc),
            surface_pressed: Rgba::rgb(0xb3, 0xb3, 0xb3),
            surface_selected: Rgba::rgb(0xa6, 0xa6, 0xa6),
            rim: Rgba::rgb(0xc4, 0xc4, 0xc4),
            rim_active: Rgba::rgb(0xd2, 0x54, 0x0b),
            danger: Rgba::rgb(0xc4, 0x16, 0x1a),
            cpu_pressure: Rgba::rgb(0xd8, 0x54, 0x0a),
            memory_pressure: Rgba::rgb(0x74, 0x33, 0xbd),
            disk_pressure: Rgba::rgb(0xc4, 0x7a, 0x12),
            network_activity: Rgba::rgb(0x0b, 0x6f, 0xb0),
            power_pressure: Rgba::rgb(0x64, 0x8f, 0x1e),
            thermal_pressure: Rgba::rgb(0xc2, 0x50, 0x1a),
            gpu_pressure: Rgba::rgb(0x0f, 0x84, 0x78),
            accelerator_pressure: Rgba::rgb(0xa8, 0x2f, 0x66),
            recovery: Rgba::rgb(0xc9, 0x33, 0x37),
            success: Rgba::rgb(0x4b, 0x7d, 0x22),
            warning: Rgba::rgb(0xa9, 0x74, 0x1a),
            denied: Rgba::rgb(0x8f, 0x25, 0x28),
            workload: Rgba::rgb(0x2f, 0x7d, 0x33),
            disk_read: Rgba::rgb(0x1d, 0x7f, 0x47),
            disk_write: Rgba::rgb(0xb0, 0x36, 0x2a),
            net_receive: Rgba::rgb(0x11, 0x6f, 0xb0),
            net_send: Rgba::rgb(0x55, 0x46, 0xc4),
            scroll_track: Rgba::rgb(0xcf, 0xcf, 0xcf),
            scroll_thumb: Rgba::rgb(0x9e, 0x9e, 0x9e),
            scroll_thumb_active: Rgba::rgb(0x74, 0x74, 0x74),
            frame: Rgba::rgb(0x91, 0x91, 0x91),
            bevel_light: Rgba::new(0xff, 0xff, 0xff, 0x8c),
            bevel_shade: Rgba::new(0x00, 0x00, 0x00, 0x50),
            drop_shadow: Rgba::new(0x00, 0x00, 0x00, 0x5a),
            inactive_drop_shadow: Rgba::new(0x00, 0x00, 0x00, 0x36),
            window_close: Rgba::rgb(0xd7, 0x1f, 0x18).with_alpha(COMMAND_ALPHA),
            window_minimize: Rgba::rgb(0xc9, 0x8a, 0x00).with_alpha(COMMAND_ALPHA),
            window_maximize: Rgba::rgb(0x1d, 0x8c, 0x3c).with_alpha(COMMAND_ALPHA),
            window_put_to_back: Rgba::rgb(0x0b, 0x6f, 0xb0).with_alpha(COMMAND_ALPHA),
            title_hue_alpha: TITLE_HUE_ALPHA,
            syntax: SyntaxPalette::light(),
        }
    };

    /// The colour text of `role` is drawn in on [`document`](Self::document).
    #[must_use]
    pub const fn syntax(&self, role: SyntaxRole) -> Rgba {
        let s = &self.syntax;
        match role {
            SyntaxRole::Plain => self.on_surface,
            SyntaxRole::Keyword => s.keyword,
            SyntaxRole::Type => s.type_name,
            SyntaxRole::Function => s.function,
            SyntaxRole::String => s.string,
            SyntaxRole::Escape => s.escape,
            SyntaxRole::Number => s.number,
            SyntaxRole::Comment => s.comment,
            SyntaxRole::Punctuation => s.punctuation,
            SyntaxRole::Tag => s.tag,
            SyntaxRole::Attribute => s.attribute,
            SyntaxRole::Key => s.key,
            SyntaxRole::Directive => s.directive,
            SyntaxRole::Heading => s.heading,
            SyntaxRole::Emphasis => s.emphasis,
            SyntaxRole::Link => s.link,
            SyntaxRole::Code => s.code,
            SyntaxRole::Error => s.error,
            SyntaxRole::Control => s.control,
            SyntaxRole::Invalid => s.invalid,
            SyntaxRole::Invisible => s.invisible,
        }
    }

    /// The semantic signal colour for a resource pressure.
    ///
    /// One place maps a pressure to its role so no consumer restates the
    /// mapping. The kind is taken as a small local enum
    /// ([`SignalRole`]) rather than a `lib/controls` type, keeping this crate
    /// at the bottom of the layering.
    #[must_use]
    pub const fn signal(&self, role: SignalRole) -> Rgba {
        match role {
            SignalRole::Cpu => self.cpu_pressure,
            SignalRole::Memory => self.memory_pressure,
            SignalRole::Disk => self.disk_pressure,
            SignalRole::Network => self.network_activity,
            SignalRole::Power => self.power_pressure,
            SignalRole::Thermal => self.thermal_pressure,
            SignalRole::Gpu => self.gpu_pressure,
            SignalRole::Accelerator => self.accelerator_pressure,
            SignalRole::Recovery => self.recovery,
            SignalRole::Success => self.success,
            SignalRole::Warning => self.warning,
            SignalRole::Denied => self.denied,
            SignalRole::Workload => self.workload,
            SignalRole::DiskRead => self.disk_read,
            SignalRole::DiskWrite => self.disk_write,
            SignalRole::NetReceive => self.net_receive,
            SignalRole::NetSend => self.net_send,
        }
    }
}

/// The foreground both built-in themes draw on an accent fill.
///
/// A primary action is one treatment in the design boards — a warm white
/// label on the alloy-orange plate — and it reads identically on either
/// appearance, so the two variants share the token instead of restating it.
const ON_ACCENT: Rgba = Rgba::rgb(0xff, 0xf5, 0xee);

/// The opacity both built-in themes give their selection fill: a little under
/// a third.
///
/// A selection marks an item without hiding it, so the surface or wallpaper
/// under a selected tile still reads through the accent rather than being
/// replaced by it. It is this light because it is not doing the work alone:
/// the backdrop beneath a selected item is frosted first, and that separation
/// is what makes the item read as picked, leaving the accent to tint rather
/// than to cover. Each theme still authors its own fill colour and may tune
/// this.
pub(crate) const SELECTION_ALPHA: u8 = 77;

/// The opacity both built-in themes wash a selected item's picture with:
/// about nine twentieths, enough to read as lit over the picture's own colour
/// where the lighter fill beside it only tints a plate.
pub(crate) const SELECTION_TINT_ALPHA: u8 = 115;

/// The opacity both built-in themes give their window-command highlights:
/// half.
///
/// A command's hue says which of the four the pointer is on; it is not trying
/// to replace the title bar it sits in. Half opacity is enough for the colour
/// to be unmistakable while the bar still reads through it, so a lit command
/// looks like part of the window rather than a sticker on it. The two
/// appearances share the weight and author the hues themselves, deeper on
/// paper-white and brighter on near-black, so each carries on its own bar.
pub(crate) const COMMAND_ALPHA: u8 = 128;

/// The opacity both built-in themes give the hue a title bar takes from its
/// window's identity icon: a little under a third.
///
/// The bar is not trying to show the icon twice. At this weight the colour is
/// unmistakable as *that application's* while the chrome underneath still reads
/// as chrome, and the icon a few pixels away stays the saturated thing the eye
/// goes to. Both appearances share it because the wash is not a palette colour
/// of theirs at all — it is whatever the application's artwork happens to be.
/// Any less reads as almost nothing at the densities the desktop runs at, and
/// a hue that has to be looked for is not identifying anything.
pub(crate) const TITLE_HUE_ALPHA: u8 = 82;

/// The opacity both built-in themes give their floating chrome: four fifths.
///
/// The bar, its popups and every menu plate sit over the wallpaper and over
/// whatever windows are open, and the desktop reads better when they are part
/// of that picture rather than solid cards on top of it. One fifth through is
/// as far as it goes: the icons and text on top need a settled ground to read
/// against, and the backdrop they are laid over is blurred first, which is
/// what carries the separation the missing opacity would otherwise have to.
pub(crate) const CHROME_ALPHA: u8 = 204;

/// The opacity both built-in themes give a plate raised on floating chrome:
/// half of what is left between its ground and solid.
///
/// A button or a text field is furniture standing on the glass, so it reads
/// as a distinct object rather than a hole cut in the surface — while still
/// letting the blurred backdrop through, which is what keeps a popup one
/// piece of glass instead of a frosted sheet with solid patches on it. Derived
/// from the ground rather than authored beside it, so raising the chrome
/// opacity cannot quietly narrow the step to nothing.
pub(crate) const CHROME_PLATE_ALPHA: u8 = CHROME_ALPHA + (u8::MAX - CHROME_ALPHA) / 2;

/// A semantic signal a control can display, used to resolve one palette
/// colour and (in the renderer) one shape fallback.
///
/// This is the theme-side vocabulary of the spec §6 semantic roles. The
/// resource-pressure subset lines up one-to-one with `lib/controls`'
/// `PressureKind`; a renderer maps its typed state to a `SignalRole` and asks
/// the palette for the colour, so the mapping lives in exactly one place.
///
/// The vocabulary is wider than that subset, because not every signal a
/// control shows is a resource pressure: an outcome (recovery, success), and a
/// *direction* of transfer — read against write, receive against send — are
/// signals a reader must be able to tell apart on one instrument, and neither
/// is a resource under load.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
pub enum SignalRole {
    /// Compute saturation.
    Cpu,
    /// Memory pressure.
    Memory,
    /// Storage throughput.
    Disk,
    /// Network transfer.
    Network,
    /// Power / battery pressure.
    Power,
    /// Thermal pressure.
    Thermal,
    /// Graphics / display-path utilisation.
    Gpu,
    /// General-purpose accelerator utilisation.
    Accelerator,
    /// Recovery / repair / restart.
    Recovery,
    /// Completed / verified.
    Success,
    /// Caution.
    Warning,
    /// Missing authority.
    Denied,
    /// What the machine is running: a task census.
    Workload,
    /// Data read from a storage device.
    DiskRead,
    /// Data written to a storage device.
    DiskWrite,
    /// Data received on a network interface.
    NetReceive,
    /// Data sent on a network interface.
    NetSend,
}

impl SignalRole {
    /// The role a volume in `health` is drawn in.
    ///
    /// One binding between a volume's banded state and the signal
    /// vocabulary, so a degraded disk cannot read amber on one surface and
    /// green on another. It lives here because this crate owns the roles;
    /// the band itself is
    /// [`MountAvailability::health`](tairix_abi::sysinfo::MountAvailability::health).
    #[must_use]
    pub const fn for_volume_health(health: VolumeHealth) -> Self {
        match health {
            VolumeHealth::Healthy => Self::Success,
            VolumeHealth::Degraded => Self::Warning,
            VolumeHealth::Failing => Self::Recovery,
        }
    }
}
