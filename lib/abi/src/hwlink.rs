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
use crate::hwtree::NodeEndpointBlock;
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
}

impl LinkRole {
    /// Every role.
    pub const ALL: &'static [Self] = &[Self::Dma, Self::Clock, Self::Codec];

    /// The node-indexed endpoints the role's suppliers serve.
    #[must_use]
    pub const fn endpoints(self) -> NodeEndpointBlock {
        match self {
            Self::Dma => DMA_CONTROLLER_ENDPOINTS,
            Self::Clock => CLOCK_CONTROLLER_ENDPOINTS,
            Self::Codec => CODEC_ENDPOINTS,
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

#[cfg(test)]
#[path = "hwlink_tests.rs"]
mod tests;
