//! What the pane on show draws in the content column.
//!
//! Four shapes, and the registry decides which from the pane's own row: a
//! stated absence, a form of settables, the mounted volumes, or a read-only
//! column of machine readings. The Wallpaper pane is the one that carries
//! two things at once — its rows fixed at the top of the column with the
//! shipped pictures scrolling beneath them — and it is its own variant
//! rather than a pair of options, so "a gallery with no form" is a state
//! the shell cannot be in.
//!
//! The shell asks this what to measure, what to draw, and how it scrolls;
//! it never asks which of two options happens to be set.

use alloc::boxed::Box;
use alloc::string::String;

use tairix_abi::BundleId;
use tairix_controls::{ScrollModel, ScrollRange};
use tairix_geometry::{Rect, Scale};
use tairix_icon::IconArtwork;
use tairix_raster::Surface;
use tairix_sysconfig::SystemConfig;
use tairix_theme::{CursorSetId, Theme};
use tairix_wallpaper::{CatalogItem, DesktopSettings};

use crate::accounts::{AccountFacts, AccountSetting};
use crate::facts::{Facts, MachineFacts};
use crate::form::{Documents, Form, FormPlace};
use crate::gallery::Gallery;
use crate::network::{IfaceSetting, NetworkFacts};
use crate::registry::{PaneContent, PaneRow};
use crate::statement;
use crate::volumes::{Readings, VolumeReading};

/// Everything a body is built from: what the shell has been answered so
/// far, and the document every settable row reads.
///
/// A pane opens on whatever has arrived — nothing at all, at first — and is
/// rebuilt when the rest lands, so none of these is awaited.
pub(crate) struct Answered<'a> {
    /// The desktop's own settings document.
    pub(crate) settings: &'a DesktopSettings,
    /// The cursor sets the desktop answered with.
    pub(crate) cursor_sets: &'a [CursorSetId],
    /// The shipped pictures the desktop answered with.
    pub(crate) catalog: &'a [CatalogItem],
    /// The mounted volumes the mount-table walk answered with.
    pub(crate) volumes: &'a [VolumeReading],
    /// The machine's boot-time configuration, or `None` while the read has
    /// not landed. Absent is not the same fact as a store of defaults, so
    /// a machine row with no reading says so rather than showing one.
    pub(crate) config: Option<&'a SystemConfig>,
    /// The machine readings the caller took for the panes that state them.
    pub(crate) machine: &'a MachineFacts,
    /// The network readings the caller took for the panes that state them.
    pub(crate) network: &'a NetworkFacts,
    /// The per-interface edits a returning networking pane carries, which
    /// is none for every other pane.
    pub(crate) staged: &'a [(IfaceSetting, String)],
    /// The account readings the caller took for the pane that states them.
    pub(crate) accounts: &'a AccountFacts,
    /// The per-account edits a returning Users pane carries, which is none
    /// for every other pane.
    pub(crate) staged_accounts: &'a [(AccountSetting, String)],
    /// The sources the desktop said have notified, or `None` while it has
    /// not said.
    pub(crate) notify_sources: Option<&'a [BundleId]>,
}

impl<'a> Answered<'a> {
    /// The stores a form's rows are built from.
    fn documents(&self) -> Documents<'a> {
        Documents {
            settings: self.settings,
            cursor_sets: self.cursor_sets,
            config: self.config,
            addressing: &self.network.addressing,
            staged: self.staged,
            resolvers: self.network.resolvers_slice(),
            accounts: self.accounts,
            staged_accounts: self.staged_accounts,
            notify_sources: self.notify_sources,
            sources_full: false,
            lock_refusal: None,
        }
    }
}

/// The content column's contents.
pub(crate) enum Body {
    /// The pane composes no controls and says why: what this system does
    /// not have, or where the setting is reached instead.
    Statement,
    /// A form of settables over the desktop's settings document.
    Form(Form),
    /// The Wallpaper pane: its form fixed at the top of the column, with
    /// the picture gallery scrolling beneath it.
    Pictures {
        /// The settable rows, which stay put.
        form: Form,
        /// The pictures, which are what scrolls.
        gallery: Box<Gallery>,
    },
    /// The mounted volumes: one read-only card each, discovered rather than
    /// declared.
    Volumes(Readings),
    /// A read-only column of machine readings: what this machine is, or
    /// what its clock says.
    Facts(Facts),
}

impl Body {
    /// What `pane` draws, from what the shell has been answered.
    pub(crate) fn of(pane: &PaneRow, answered: &Answered<'_>) -> Self {
        match pane.content() {
            None => Self::Statement,
            Some(PaneContent::Form(composition)) => {
                Self::Form(Form::new(composition, answered.documents()))
            }
            Some(PaneContent::Pictures(composition)) => Self::Pictures {
                form: Form::new(composition, answered.documents()),
                gallery: Box::new(Gallery::new(answered.catalog, answered.settings)),
            },
            Some(PaneContent::Volumes) => Self::Volumes(Readings::new(answered.volumes)),
            Some(PaneContent::About) => Self::Facts(Facts::about(answered.machine)),
            Some(PaneContent::Clock) => Self::Facts(Facts::clock(answered.machine)),
        }
    }

    /// The form this body composes, if it composes one.
    pub(crate) const fn form(&self) -> Option<&Form> {
        match self {
            Self::Form(form) | Self::Pictures { form, .. } => Some(form),
            Self::Statement | Self::Volumes(_) | Self::Facts(_) => None,
        }
    }

    /// The form this body composes, to route an event into.
    pub(crate) const fn form_mut(&mut self) -> Option<&mut Form> {
        match self {
            Self::Form(form) | Self::Pictures { form, .. } => Some(form),
            Self::Statement | Self::Volumes(_) | Self::Facts(_) => None,
        }
    }

    /// The picture gallery this body draws, if it draws one.
    pub(crate) const fn gallery(&self) -> Option<&Gallery> {
        match self {
            Self::Pictures { gallery, .. } => Some(gallery),
            Self::Statement | Self::Form(_) | Self::Volumes(_) | Self::Facts(_) => None,
        }
    }

    /// The picture gallery this body draws, to route an event into.
    pub(crate) const fn gallery_mut(&mut self) -> Option<&mut Gallery> {
        match self {
            Self::Pictures { gallery, .. } => Some(gallery),
            Self::Statement | Self::Form(_) | Self::Volumes(_) | Self::Facts(_) => None,
        }
    }

    /// Whether this body composes controls a reader can act on.
    ///
    /// What decides whether the pane column is on the focus ring in its own
    /// right: a body with controls is reachable whether or not it is long
    /// enough to scroll, while one that only scrolls is reachable exactly
    /// when there is something to scroll.
    pub(crate) const fn composes_controls(&self) -> bool {
        self.form().is_some()
    }

    /// Whether the whole column scrolls, rather than a gallery beneath a
    /// form that stays put.
    pub(crate) const fn column_scrolls(&self) -> bool {
        !matches!(self, Self::Pictures { .. })
    }

    /// Whether this body stages a change to the machine's boot-time store,
    /// and so needs that store read before its rows can show anything.
    pub(crate) fn stages_machine_settings(&self) -> bool {
        self.composition()
            .is_some_and(crate::form::Composition::reads_machine)
    }

    /// Which settings this body composes, for a caller asking what it
    /// reads rather than what it draws.
    pub(crate) fn composition(&self) -> Option<crate::form::Composition> {
        self.form().map(Form::composition)
    }

    /// The per-interface edits the body is holding, so a rebuilt pane
    /// keeps a change the reader has staged but not yet applied.
    pub(crate) fn staged(&self) -> &[(IfaceSetting, String)] {
        self.form().map_or(&[], Form::staged)
    }

    /// The per-account edits the body is holding, on the same terms.
    pub(crate) fn staged_accounts(&self) -> &[(AccountSetting, String)] {
        self.form().map_or(&[], Form::staged_accounts)
    }

    /// Whether a choice list is open, which is modal: the list keeps the
    /// pointer even where it hangs outside the pane's own column.
    pub(crate) fn is_listing(&self) -> bool {
        self.form().is_some_and(Form::is_listing)
    }

    /// The height the body itself needs in a column `width` pixels wide.
    ///
    /// A gallery's own extent is not counted: it is the band left beneath a
    /// form that stays put, measured where that band is resolved.
    pub(crate) fn measured_height(
        &self,
        pane: &PaneRow,
        width: u32,
        scale: Scale,
        theme: &Theme,
    ) -> u32 {
        match self {
            Self::Statement => statement::measured_height(pane, width, scale, theme),
            Self::Form(form) | Self::Pictures { form, .. } => {
                form.measured_height(width, scale, theme)
            }
            Self::Volumes(readings) => readings.measured_height(width, scale, theme),
            Self::Facts(facts) => facts.measured_height(width, scale, theme),
        }
    }

    /// The scroll model the pane moves through, in physical pixels: the
    /// gallery's own where the pictures are what scrolls, else the whole
    /// column's, a [`line_step`] a line.
    ///
    /// `column` is the pane's column and `band` what it has left beneath a
    /// fixed form, both resolved by the shell because only it knows the frame.
    pub(crate) fn scroll_model(
        &self,
        pane: &PaneRow,
        column: Rect,
        band: Rect,
        (scale, theme, offset): (Scale, &Theme, u64),
    ) -> ScrollModel {
        match self {
            Self::Pictures { gallery, .. } => gallery.scroll_model(band, scale, theme, offset),
            Self::Statement | Self::Form(_) | Self::Volumes(_) | Self::Facts(_) => {
                let range = ScrollRange::new(
                    u64::from(self.measured_height(pane, column.width, scale, theme)),
                    u64::from(column.height),
                    offset,
                );
                ScrollModel::in_pixels(range, line_step(scale, theme))
            }
        }
    }

    /// Adopt the desktop settings the session now holds.
    pub(crate) fn adopt(&mut self, settings: &DesktopSettings) {
        match self {
            Self::Form(form) => form.adopt(settings),
            Self::Pictures { form, gallery } => {
                form.adopt(settings);
                gallery.adopt(settings);
            }
            // None reads the desktop's document: one states the registry's
            // own words, the others the machine's volumes and readings.
            Self::Statement | Self::Volumes(_) | Self::Facts(_) => {}
        }
    }

    /// Draw the body into `surface`, laid out unscrolled: a scrolling column
    /// is shown through the shell's view.
    pub(crate) fn render(
        &self,
        surface: &mut Surface,
        pane: &PaneRow,
        drawn: Drawn<'_>,
        artwork: &mut dyn IconArtwork,
    ) {
        let Drawn {
            place,
            band,
            offset,
        } = drawn;
        match self {
            Self::Statement => {
                statement::render(surface, pane, place.bounds, place.scale, place.theme);
            }
            Self::Form(form) => form.render(surface, place),
            Self::Pictures { form, gallery } => {
                form.render(surface, place);
                gallery.render(surface, band, offset, place.scale, place.theme);
            }
            Self::Volumes(readings) => {
                readings.render(surface, place.bounds, place.scale, place.theme, artwork);
            }
            Self::Facts(facts) => facts.render(surface, place.bounds, place.scale, place.theme),
        }
    }

    /// Draw the choice list a composed pane has open, which the shell paints
    /// above everything the pane shares the window with.
    pub(crate) fn render_popup(&self, surface: &mut Surface, place: FormPlace<'_>) {
        if let Some(form) = self.form() {
            form.render_popup(surface, place);
        }
    }
}

/// How far a line step moves a column of rows or plates: a control's height at
/// the desktop's density, so an arrow or an end button moves one row.
pub(crate) fn line_step(scale: Scale, theme: &Theme) -> u64 {
    u64::from(scale.scale_length(theme.metrics().control_height).max(1))
}

/// Where a body is drawn, gathered for the one call that draws it.
#[derive(Copy, Clone)]
pub(crate) struct Drawn<'a> {
    /// The pane column, laid out unscrolled, and the surface every length
    /// is resolved against.
    pub(crate) place: FormPlace<'a>,
    /// The band left beneath a fixed form, where a gallery is drawn.
    pub(crate) band: Rect,
    /// How far that band is scrolled, in pixels.
    pub(crate) offset: u64,
}
