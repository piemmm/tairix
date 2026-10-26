//! Each picture a window draws: where it shows and what it asks the artwork
//! lookup for.
//!
//! A decode changes pixels only inside the slots drawing through it, so a
//! window told which decodes came back ([`tairix_icon::Landed`]) repaints those
//! slots and nothing else, and asks after the rest without repainting them.
//! Each walk derives a slot's request, side and rectangle through the helper
//! its paint uses, so a landing repaints exactly where the paint drew.

use alloc::string::String;

use tairix_geometry::{Rect, Scale};
use tairix_icon::IconRequest;
use tairix_theme::Theme;

use super::{
    awaiting_listing, build_toolbar, candidate_art, chooser_identity, content_area,
    content_viewport, grid_view, identity_slot, list_view, network_row_art, network_tile_art,
    open_with_identity_rect, open_with_rows, place_art, row_art, sidebar_view, tile_art,
    toolbar_bounds, Identity, ManagerChrome, PropertiesLayout, Tiles,
};
use crate::browser::{Browser, Item};
use crate::chrome::{ToolbarBand, ToolbarModel};
use crate::open_with::OpenWithChooser;
use crate::source::DirectorySource;

/// One picture a window shows.
#[derive(Copy, Clone, Debug)]
pub struct ShownArt<'a> {
    /// What the paint asks the artwork lookup for.
    pub request: IconRequest<'a>,
    /// The pixel side it asks at.
    pub side: u32,
    /// The window-local part of its slot that shows.
    pub at: Rect,
}

/// Visit each picture [`render_into`](super::render_into) draws for `browser`
/// in `viewport` with `chrome`: the rail's rows, the toolbar's tools, and the
/// listing's tiles or rows.
pub fn manager_art<S: DirectorySource>(
    browser: &Browser<S>,
    (scale, theme): (Scale, &Theme),
    viewport: Rect,
    chrome: &ManagerChrome<'_>,
    visit: &mut dyn FnMut(ShownArt<'_>),
) {
    if let (Some(places), Some(view)) = (
        chrome.sidebar,
        sidebar_view(viewport, scale, theme, chrome.sidebar, chrome.toolbar),
    ) {
        for index in view.visible_range() {
            let (Some(place), Some(bounds), Some(at)) = (
                places.rows().get(index),
                view.row_rect(index),
                view.shown_row_rect(index),
            ) else {
                continue;
            };
            let (request, side) = place_art(place, bounds, scale, theme);
            visit(ShownArt { request, side, at });
        }
    }
    if chrome.toolbar.is_shown() {
        if let Some(bounds) = toolbar_bounds(scale, theme, viewport, ToolbarBand::Shown) {
            build_toolbar(
                ToolbarModel::for_browser(browser),
                chrome.tools,
                chrome.tool_model,
            )
            .visit_icons(bounds, scale, theme, |request, side, at| {
                visit(ShownArt { request, side, at });
            });
        }
    }
    if awaiting_listing(browser) {
        return;
    }
    let area = content_area(viewport, scale, theme, chrome.sidebar, chrome.toolbar);
    let content = content_viewport(area, scale, theme);
    let offset = browser.scroll_offset();
    let pictures = browser.presentation().pictures;
    // Spelled once for the walk, as the paint spells it once for the frame.
    let folder = browser
        .folder()
        .map(|parent| (crate::vfs::spell_absolute_path(parent), parent));
    let mut scratch = String::new();
    match Tiles::of(browser.view_mode()) {
        None => {
            let view = list_view(browser, scale, theme, content, chrome.toolbar);
            for index in view.visible_range(offset) {
                let (Some(item), Some(bounds)) = (browser.item(index), view.row_rect(index)) else {
                    break;
                };
                let Some(at) = view.shown_rect(offset, index) else {
                    continue;
                };
                let (_, request, side) = match (item, &folder) {
                    (Item::Entry(entry), Some((dir, parent))) => row_art(
                        (dir, parent),
                        entry,
                        bounds,
                        (scale, theme, pictures),
                        &mut scratch,
                    ),
                    (Item::Network(item), _) => network_row_art(item, bounds, scale, theme),
                    (Item::Entry(_), None) => break,
                };
                visit(ShownArt { request, side, at });
            }
        }
        Some(tiles) => {
            let view = grid_view(browser, tiles, scale, theme, content, chrome.toolbar);
            for index in view.visible_range(offset) {
                let (Some(item), Some(bounds)) = (browser.item(index), view.cell_rect(index))
                else {
                    break;
                };
                let Some(at) = view.shown_rect(offset, index) else {
                    continue;
                };
                let (_, request, side) = match (item, &folder) {
                    (Item::Entry(entry), Some((dir, parent))) => tile_art(
                        tiles,
                        (dir, parent),
                        entry,
                        bounds,
                        (scale, theme, pictures),
                        &mut scratch,
                    ),
                    (Item::Network(item), _) => network_tile_art(tiles, item, bounds, scale, theme),
                    (Item::Entry(_), None) => break,
                };
                visit(ShownArt { request, side, at });
            }
        }
    }
}

/// Visit the picture
/// [`draw_properties_window`](super::draw_properties_window) draws for
/// `identity` in `window`: its identity band's. Its tab strip draws none.
pub fn properties_art(
    identity: &Identity<'_>,
    (scale, theme): (Scale, &Theme),
    window: Rect,
    visit: &mut dyn FnMut(ShownArt<'_>),
) {
    let slot = identity_slot(
        PropertiesLayout::resolve(window, scale, theme).identity,
        scale,
    );
    visit(ShownArt {
        request: identity.art,
        side: slot.width,
        at: slot,
    });
}

/// Visit each picture
/// [`draw_open_with_chooser`](super::draw_open_with_chooser) draws for
/// `chooser` in `viewport`: the file's type in its identity band, and each
/// candidate row's application.
pub fn chooser_art(
    chooser: &OpenWithChooser,
    (scale, theme): (Scale, &Theme),
    viewport: Rect,
    visit: &mut dyn FnMut(ShownArt<'_>),
) {
    if let Some(band) = open_with_identity_rect(viewport, scale, theme) {
        let slot = identity_slot(band, scale);
        visit(ShownArt {
            request: chooser_identity(chooser).art,
            side: slot.width,
            at: slot,
        });
    }
    let Some((rows, _)) = open_with_rows(chooser, viewport, scale, theme) else {
        return;
    };
    let offset = chooser.offset();
    for index in rows.visible_range(offset) {
        let (Some(candidate), Some(bounds)) =
            (chooser.candidates().get(index), rows.row_rect(index))
        else {
            break;
        };
        let Some(at) = rows.shown_rect(offset, index) else {
            continue;
        };
        let (request, side) = candidate_art(candidate, bounds, scale, theme);
        visit(ShownArt { request, side, at });
    }
}

#[cfg(test)]
#[path = "render_shown_tests.rs"]
mod tests;
