//! The network map and inventory engine (`netmap.app`, `plans/NETMAP.md`).
//!
//! The host-testable core of the map: the device and service [`model`], the
//! [`source`] reads, the advertised services [`discover`] keeps current, the
//! passive [`mod@infer`]ence of what each host is, the [`ingest`] that projects
//! them into it, the deterministic radial
//! [`mod@layout`], the graph-space [`viewport`], the interactive [`view`]
//! that draws it and answers the pointer, and the context [`mod@menu`] a
//! secondary press opens. The library performs no I/O of its
//! own beyond reads and requests over injected seams; the `Run` binary is thin
//! glue over `tairix-rt`, so the tested code and the shipped code are the same
//! code.
//!
//! A label is display text the model holds as given, so a source that adds
//! peer-authored text escapes it before it reaches the model.

#![no_std]
#![forbid(unsafe_code)]
#![deny(missing_docs)]

extern crate alloc;

pub mod discover;
pub mod infer;
pub mod ingest;
pub mod layout;
pub mod menu;
pub mod model;
pub mod source;
pub mod view;
pub mod viewport;

pub use discover::{AdvertisedService, Directory, Discovery};
pub use infer::{infer, Evidence, Role, Seen, Signals};
pub use ingest::{assigned_oui, InterfaceObservation, Inventory};
pub use layout::{layout, RING_STEP};
pub use menu::{Command, NodeMenu, Steer, Subject};
pub use model::{
    Classification, DeviceClass, Edge, EdgeKind, Endpoint, InterfaceName, Liveness, Map, Node,
    NodeId, NodeKey, NodeKind, OsFamily, MAX_EDGES, MAX_LABEL_LEN, MAX_NODES,
};
pub use source::Snapshot;
pub use view::{node_look, os_emblem, Look, MapView, Reaction, NODE_RADIUS};
pub use viewport::{Viewport, DEFAULT_ZOOM_PER_MILLE, MAX_ZOOM_PER_MILLE, MIN_ZOOM_PER_MILLE};

/// The map window's preferred width when it opens, in logical pixels at the
/// reference density. The `Run` shell and the QEMU vertical's window
/// reconstruction read it from here, so neither carries its own copy.
pub const WIN_WIDTH: u32 = 960;
/// The map window's preferred height when it opens, in logical pixels
/// (see [`WIN_WIDTH`]).
pub const WIN_HEIGHT: u32 = 640;

/// The sizing the map's window asks the window manager for: resizable down to
/// the given floor, in physical pixels, with no ceiling.
#[must_use]
pub const fn win_sizing(
    min_width_px: u32,
    min_height_px: u32,
) -> tairix_abi::window_ipc::WindowSizing {
    tairix_abi::window_ipc::WindowSizing::Resizable {
        min_width_px,
        min_height_px,
        max_width_px: 0,
        max_height_px: 0,
    }
}

/// Whether the map's window is decorated resizable, which widens the furniture
/// band around its client. Derived from [`win_sizing`] so the window the app
/// opens and the footprint the QEMU vertical reconstructs cannot disagree.
pub const WIN_RESIZABLE: bool = win_sizing(0, 0).resizable();
