//! What the window does with the picture's layers: adding, removing and
//! ordering them, which one is painted on, how each shows, and laying them
//! together on a worker.

use alloc::vec::Vec;

use tairix_geometry::Region;

use super::{Compute, Lands, Outcome, View};
use crate::canvas::{Canvas, Kind, Sample};
use crate::dialog::Form;
use crate::document::{copy_name, new_layer_name, Document, Layer, LayerRefusal, Shown};
use crate::layout::Layout;

/// Why a change to the layers could not be had for want of memory.
const NO_ROOM: &str = "There is not enough memory to change the layers";

impl View {
    /// Add a clear layer over the one painted on, and paint on it.
    pub(super) fn new_layer(&mut self, layout: &Layout, damage: &mut Region) -> Outcome {
        if !self.editable(layout, damage) {
            return Outcome::none();
        }
        let Some(picture) = self.document.picture() else {
            return Outcome::none();
        };
        let (width, height) = picture.size();
        let made = Canvas::new(width, height, Kind::Rgba, Sample::Rgba([0; 4]))
            .ok()
            .zip(new_layer_name(picture.layers()).ok());
        let Some((canvas, name)) = made else {
            self.state(NO_ROOM, layout, damage);
            return Outcome::none();
        };
        let at = picture.active() + 1;
        self.change_layers(
            |document| document.insert_layer(at, Layer::new(canvas, name)),
            layout,
            damage,
        )
    }

    /// Add a copy of the layer painted on over it, and paint on the copy.
    pub(super) fn duplicate_layer(&mut self, layout: &Layout, damage: &mut Region) -> Outcome {
        if !self.editable(layout, damage) {
            return Outcome::none();
        }
        let Some(picture) = self.document.picture() else {
            return Outcome::none();
        };
        let active = picture.active();
        let copied = picture.layers().get(active).and_then(|layer| {
            let mut copy = layer.try_clone().ok()?;
            copy.name = copy_name(&layer.name).ok()?;
            Some(copy)
        });
        let Some(copy) = copied else {
            self.state(NO_ROOM, layout, damage);
            return Outcome::none();
        };
        self.change_layers(
            |document| document.insert_layer(active + 1, copy),
            layout,
            damage,
        )
    }

    /// Take the layer painted on away.
    pub(super) fn delete_layer(&mut self, layout: &Layout, damage: &mut Region) -> Outcome {
        if !self.editable(layout, damage) {
            return Outcome::none();
        }
        let active = self.active_layer();
        self.change_layers(|document| document.remove_layer(active), layout, damage)
    }

    /// Move the layer painted on one place up the stack, or down it.
    pub(super) fn move_layer(&mut self, up: bool, layout: &Layout, damage: &mut Region) -> Outcome {
        if !self.editable(layout, damage) {
            return Outcome::none();
        }
        let active = self.active_layer();
        let Some(to) = self.beside(up) else {
            return Outcome::none();
        };
        self.change_layers(|document| document.move_layer(active, to), layout, damage)
    }

    /// Paint on the layer above the one painted on, or the one below.
    pub(super) fn step_layer(&mut self, up: bool, layout: &Layout, damage: &mut Region) -> Outcome {
        if !self.idle(layout, damage) {
            return Outcome::none();
        }
        if let Some(to) = self.beside(up) {
            self.paint_on(to, layout, damage);
        }
        Outcome::none()
    }

    /// The layer next to the one painted on, up the stack or down it.
    fn beside(&self, up: bool) -> Option<usize> {
        let picture = self.document.picture()?;
        let active = picture.active();
        if up {
            Some(active + 1).filter(|&to| to < picture.layers().len())
        } else {
            active.checked_sub(1)
        }
    }

    /// Paint on the layer `text` names, or numbers from one at the bottom.
    pub(super) fn go_to_layer(
        &mut self,
        text: &str,
        layout: &Layout,
        damage: &mut Region,
    ) -> Outcome {
        if !self.idle(layout, damage) {
            return Outcome::none();
        }
        let text = text.trim();
        let found = self.document.picture().and_then(|picture| {
            let layers = picture.layers();
            layers
                .iter()
                .position(|layer| layer.name == text)
                .or_else(|| {
                    text.parse::<usize>()
                        .ok()
                        .and_then(|number| number.checked_sub(1))
                        .filter(|&index| index < layers.len())
                })
        });
        match found {
            Some(index) => self.paint_on(index, layout, damage),
            None => self.state("There is no layer of that name or number", layout, damage),
        }
        Outcome::none()
    }

    /// Paint on layer `index`: where painting lands changes, the picture does
    /// not.
    fn paint_on(&mut self, index: usize, layout: &Layout, damage: &mut Region) {
        if self.document.select_layer(index) {
            damage.add(layout.status());
        }
    }

    /// Show the layer painted on, or hide it.
    pub(super) fn toggle_layer(&mut self, layout: &Layout, damage: &mut Region) -> Outcome {
        if !self.editable(layout, damage) {
            return Outcome::none();
        }
        let active = self.active_layer();
        let shown = self
            .document
            .picture()
            .and_then(|picture| picture.layers().get(active))
            .map(Shown::of);
        match shown {
            Some(Ok(mut shown)) => {
                shown.visible = !shown.visible;
                self.change_layers(
                    |document| document.show_layer(active, shown),
                    layout,
                    damage,
                )
            }
            Some(Err(_)) => {
                self.state(NO_ROOM, layout, damage);
                Outcome::none()
            }
            None => Outcome::none(),
        }
    }

    /// Ask how the layer painted on is to show: its name, opacity and
    /// whether it shows.
    pub(super) fn ask_layer(&mut self, layout: &Layout, damage: &mut Region) -> Outcome {
        if !self.editable(layout, damage) {
            return Outcome::none();
        }
        let active = self.active_layer();
        let shown = self
            .document
            .picture()
            .and_then(|picture| picture.layers().get(active))
            .map(Shown::of);
        match shown {
            Some(Ok(shown)) => self.ask(Form::layer(&shown), layout, damage),
            Some(Err(_)) => self.state(NO_ROOM, layout, damage),
            None => {}
        }
        Outcome::none()
    }

    /// The layer form was answered: show the layer painted on as `shown`.
    pub(super) fn reshow_layer(
        &mut self,
        shown: Shown,
        layout: &Layout,
        damage: &mut Region,
    ) -> Outcome {
        if !self.editable(layout, damage) {
            return Outcome::none();
        }
        let active = self.active_layer();
        self.change_layers(
            |document| document.show_layer(active, shown),
            layout,
            damage,
        )
    }

    /// Lay the layer painted on over the one beneath it, the two becoming
    /// one, on a worker.
    pub(super) fn merge_down(&mut self, layout: &Layout, damage: &mut Region) -> Outcome {
        let active = self.active_layer();
        let Some(lowest) = active.checked_sub(1) else {
            self.state("There is no layer beneath this one", layout, damage);
            return Outcome::none();
        };
        self.compose(lowest..active + 1, "merge the layers", layout, damage)
    }

    /// Lay every layer together as one, on a worker.
    pub(super) fn flatten(&mut self, layout: &Layout, damage: &mut Region) -> Outcome {
        let Some(picture) = self.document.picture() else {
            return Outcome::none();
        };
        if picture.single() {
            self.state(
                "The picture is one layer, shown wholly, already",
                layout,
                damage,
            );
            return Outcome::none();
        }
        let count = picture.layers().len();
        self.compose(0..count, "flatten the picture", layout, damage)
    }

    /// Hand the layers in `range` to a worker to be laid together.
    fn compose(
        &mut self,
        range: core::ops::Range<usize>,
        what: &'static str,
        layout: &Layout,
        damage: &mut Region,
    ) -> Outcome {
        if !self.editable(layout, damage) {
            return Outcome::none();
        }
        let shared = self.document.picture().and_then(|picture| {
            let laid = picture.layers().get(range.clone())?;
            let mut layers = Vec::new();
            layers.try_reserve_exact(laid.len()).ok()?;
            for layer in laid {
                layers.push(layer.try_clone().ok()?);
            }
            Some(layers)
        });
        let Some(layers) = shared else {
            self.state(
                &alloc::format!("There is not enough memory to {what}"),
                layout,
                damage,
            );
            return Outcome::none();
        };
        self.begin_work(
            Compute::Compose { layers },
            Lands::Merged(range),
            what,
            layout,
            damage,
        )
    }

    /// Make `change` to the document's layers, showing what it changed or
    /// saying why it was refused.
    fn change_layers(
        &mut self,
        change: impl FnOnce(&mut Document) -> Result<(), LayerRefusal>,
        layout: &Layout,
        damage: &mut Region,
    ) -> Outcome {
        match change(&mut self.document) {
            Ok(()) => Self::layers_changed(layout, damage),
            Err(refusal) => {
                self.state(&alloc::format!("{refusal}"), layout, damage);
                Outcome::none()
            }
        }
    }
}
