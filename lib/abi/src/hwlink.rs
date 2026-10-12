//! Links between hardware-tree nodes (`plans/SUPPLIERS.md` SL1): a supplier
//! node's duty to serve one endpoint for its role, and each consumer's request
//! naming that endpoint and a selector in the supplier's own binding.
//!
//! The supplier is the gate: it believes a request a caller quotes only once
//! the kernel confirms the caller holds it (`call_peer_holds`), so a request
//! is authority to call its supplier and never to serve it.

use crate::driver::clock::CLOCK_CONTROLLER_ENDPOINTS;
use crate::driver::codec::CODEC_ENDPOINTS;
use crate::driver::dmaengine::DMA_CONTROLLER_ENDPOINTS;
use crate::driver::encoder::ENCODER_ENDPOINTS;
use crate::driver::i2c::I2C_CONTROLLER_ENDPOINTS;
use crate::driver::irqline::INTERRUPT_LINE_ENDPOINTS;
use crate::hwtree::{HwResource, NodeEndpointBlock};
use crate::le::{put_u64, read_u64};
use crate::origin::{ProcId, PROC_ID_LEN};
use crate::Errno;

/// The most selector cells a request carries; discovery drops a wider entry
/// rather than truncating it.
pub const LINK_SELECTOR_MAX_CELLS: usize = 2;

/// The longest name a request carries; a longer one leaves it unnamed.
pub const LINK_NAME_MAX: usize = 8;

/// What a link's supplier serves its consumers.
#[repr(u8)]
#[derive(Copy, Clone, Debug, Eq, PartialEq, Ord, PartialOrd, Hash)]
pub enum LinkRole {
    /// A DMA controller's request line (`dmaengine-v1`).
    Dma = 1,
    /// A clock controller's clock (`clock-v1`).
    Clock = 2,
    /// An audio codec's digital audio interface (`codec-v1`).
    Codec = 3,
    /// A second-level interrupt controller's line (`irqline-v1`).
    InterruptLine = 4,
    /// An I²C controller's target, or its whole bus (`i2c-v1`).
    I2c = 5,
    /// A display engine's encoder (`encoder-v1`).
    Encoder = 6,
}

impl LinkRole {
    /// Every role.
    pub const ALL: &'static [Self] = &[
        Self::Dma,
        Self::Clock,
        Self::Codec,
        Self::InterruptLine,
        Self::I2c,
        Self::Encoder,
    ];

    /// The node-indexed endpoints the role's suppliers serve.
    #[must_use]
    pub const fn endpoints(self) -> NodeEndpointBlock {
        match self {
            Self::Dma => DMA_CONTROLLER_ENDPOINTS,
            Self::Clock => CLOCK_CONTROLLER_ENDPOINTS,
            Self::Codec => CODEC_ENDPOINTS,
            Self::InterruptLine => INTERRUPT_LINE_ENDPOINTS,
            Self::I2c => I2C_CONTROLLER_ENDPOINTS,
            Self::Encoder => ENCODER_ENDPOINTS,
        }
    }

    /// The role whose endpoints include `endpoint`.
    #[must_use]
    pub fn of_endpoint(endpoint: u64) -> Option<Self> {
        Self::ALL
            .iter()
            .copied()
            .find(|role| role.endpoints().contains(endpoint))
    }
}

/// A supplier node's duty: the endpoint it serves its role on and, for a DMA
/// controller whose tree states them, the channels this system may use,
/// numbered from the node's own first channel.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct LinkDuty {
    role: LinkRole,
    endpoint: u64,
    channels: Option<u64>,
}

impl LinkDuty {
    /// A duty to serve `endpoint`.
    ///
    /// # Errors
    ///
    /// [`Errno::OutOfRange`] for an endpoint in no role's block, or channels
    /// stated for a role other than [`LinkRole::Dma`].
    pub fn new(endpoint: u64, channels: Option<u64>) -> Result<Self, Errno> {
        let role = LinkRole::of_endpoint(endpoint).ok_or(Errno::OutOfRange)?;
        if channels.is_some() && role != LinkRole::Dma {
            return Err(Errno::OutOfRange);
        }
        Ok(Self {
            role,
            endpoint,
            channels,
        })
    }

    /// What the supplier serves.
    #[must_use]
    pub const fn role(&self) -> LinkRole {
        self.role
    }

    /// The endpoint the supplier serves.
    #[must_use]
    pub const fn endpoint(&self) -> u64 {
        self.endpoint
    }

    /// A DMA controller's usable channels, bit `n` for the node's channel
    /// `n`, or [`None`] when the tree stated no mask.
    #[must_use]
    pub const fn channels(&self) -> Option<u64> {
        self.channels
    }
}

/// One link a consumer's description names: the supplier endpoint serving it,
/// the selector in the supplier's own binding, the entry's position in the
/// consumer's list, and its name.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct LinkRequest {
    role: LinkRole,
    endpoint: u64,
    index: u8,
    cells: u8,
    selector: [u32; LINK_SELECTOR_MAX_CELLS],
    name: [u8; LINK_NAME_MAX],
}

impl LinkRequest {
    /// The link `selector` to the supplier serving `endpoint`, entry `index`
    /// of its consumer's list, named `name`.
    ///
    /// # Errors
    ///
    /// * [`Errno::OutOfRange`] — `endpoint` in no role's block, or a NUL in
    ///   `name`, which would make its padding ambiguous.
    /// * [`Errno::LengthOutOfRange`] — more than [`LINK_SELECTOR_MAX_CELLS`]
    ///   cells, or a name longer than [`LINK_NAME_MAX`].
    pub fn new(endpoint: u64, index: u8, selector: &[u32], name: &[u8]) -> Result<Self, Errno> {
        let role = LinkRole::of_endpoint(endpoint).ok_or(Errno::OutOfRange)?;
        if name.contains(&0) {
            return Err(Errno::OutOfRange);
        }
        let cells = u8::try_from(selector.len()).map_err(|_| Errno::LengthOutOfRange)?;
        if usize::from(cells) > LINK_SELECTOR_MAX_CELLS || name.len() > LINK_NAME_MAX {
            return Err(Errno::LengthOutOfRange);
        }
        let mut padded_selector = [0u32; LINK_SELECTOR_MAX_CELLS];
        padded_selector[..selector.len()].copy_from_slice(selector);
        let mut padded_name = [0u8; LINK_NAME_MAX];
        padded_name[..name.len()].copy_from_slice(name);
        Ok(Self {
            role,
            endpoint,
            index,
            cells,
            selector: padded_selector,
            name: padded_name,
        })
    }

    /// What the supplier serves.
    #[must_use]
    pub const fn role(&self) -> LinkRole {
        self.role
    }

    /// The endpoint of the supplier serving the link.
    #[must_use]
    pub const fn endpoint(&self) -> u64 {
        self.endpoint
    }

    /// The entry's position in its consumer's list.
    #[must_use]
    pub const fn index(&self) -> u8 {
        self.index
    }

    /// The selector cells, in the supplier's own binding.
    #[must_use]
    pub fn selector(&self) -> &[u32] {
        &self.selector[..usize::from(self.cells)]
    }

    /// The entry's name, empty when it had none or it did not fit.
    #[must_use]
    pub fn name(&self) -> &[u8] {
        let len = self
            .name
            .iter()
            .position(|&b| b == 0)
            .unwrap_or(LINK_NAME_MAX);
        &self.name[..len]
    }

    pub(crate) const fn cell_count(&self) -> u8 {
        self.cells
    }

    pub(crate) const fn selector_cells(&self) -> [u32; LINK_SELECTOR_MAX_CELLS] {
        self.selector
    }

    pub(crate) const fn name_bytes(&self) -> [u8; LINK_NAME_MAX] {
        self.name
    }
}

/// A region a link's supplier made for its consumer and delegated to it, as
/// the consumer maps it.
///
/// The kernel binds a delegated mapping to the process that delegated it, so
/// the consumer names the supplier's instance when it maps the grant; a
/// grantor that did not delegate `grant` maps nothing.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct RegionGrant {
    /// The kernel's handle for the consumer's mapping of the region.
    pub grant: u64,
    /// The supplier instance that delegated it.
    pub grantor: ProcId,
}

impl RegionGrant {
    /// Bytes of the wire form: the handle, then the grantor.
    pub const WIRE_LEN: usize = 8 + PROC_ID_LEN;

    /// The wire form.
    #[must_use]
    pub fn to_le_bytes(&self) -> [u8; Self::WIRE_LEN] {
        let mut out = [0u8; Self::WIRE_LEN];
        put_u64(&mut out, 0, self.grant);
        out[8..].copy_from_slice(&self.grantor.to_le_bytes());
        out
    }

    /// Decode the wire form.
    ///
    /// # Errors
    ///
    /// [`Errno::LengthOutOfRange`] for a wrong length, and
    /// [`Errno::OutOfRange`] for the reserved handle `0` or the kernel's own
    /// instance as grantor: neither names a region a supplier delegated.
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, Errno> {
        if bytes.len() != Self::WIRE_LEN {
            return Err(Errno::LengthOutOfRange);
        }
        let grant = read_u64(bytes, 0);
        let grantor = ProcId::from_bytes(&bytes[8..])?;
        if grant == 0 || grantor.is_kernel() {
            return Err(Errno::OutOfRange);
        }
        Ok(Self { grant, grantor })
    }
}

/// The one link of `role` among a node's `resources`: `None` when it holds
/// none, or more than one and so no one supplier.
#[must_use]
pub fn sole_link<'r>(
    resources: impl IntoIterator<Item = &'r HwResource>,
    role: LinkRole,
) -> Option<LinkRequest> {
    let mut links = resources
        .into_iter()
        .filter_map(|resource| resource.link_request().ok())
        .filter(|link| link.role() == role);
    match (links.next(), links.next()) {
        (Some(link), None) => Some(link),
        _ => None,
    }
}

#[cfg(test)]
#[path = "hwlink_tests.rs"]
mod tests;
