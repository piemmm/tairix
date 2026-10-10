//! A configuration's alternate settings and interface associations: what an
//! interface can be switched to, every endpoint each setting brings, and which
//! interfaces form one function (`plans/SOUND.md` SND6).
//!
//! A streaming interface's default setting moves nothing — the bus must be able
//! to enumerate it without reserving bandwidth — and each further setting
//! brings the periodic endpoints one stream shape needs. Selecting one is the
//! host controller's act ([`crate::transport::control_permitted`] refuses it to
//! a class driver); this module is the reading both sides take of what a
//! setting contains.

use tairix_abi::DriverError;
use tairix_inline::BitSet256;

use crate::descriptor::{
    descriptors, ConfigurationHeader, Malformed, DESC_TYPE_ENDPOINT, DESC_TYPE_INTERFACE,
    DESC_TYPE_INTERFACE_ASSOCIATION, DESC_TYPE_SS_ENDPOINT_COMPANION, ENDPOINT_DESCRIPTOR_LEN,
    INTERFACE_DESCRIPTOR_LEN,
};
use crate::periodic::{EndpointDescriptor, SsCompanion};

/// Endpoints one alternate setting may bring: a validation bound on
/// device-supplied data, not a capacity. A setting stating more is refused.
pub const MAX_ALT_ENDPOINTS: usize = 8;

/// Byte length of an interface association descriptor.
const ASSOCIATION_LEN: usize = 8;

/// One alternate setting of one interface and every endpoint it brings.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct AltSetting {
    /// `bInterfaceNumber`.
    pub interface: u8,
    /// `bAlternateSetting`.
    pub alternate: u8,
    /// `(bInterfaceClass << 16) | (bInterfaceSubClass << 8) |
    /// bInterfaceProtocol`.
    pub class24: u32,
    endpoints: [Option<EndpointDescriptor>; MAX_ALT_ENDPOINTS],
}

impl AltSetting {
    /// The setting's endpoints, in descriptor order.
    pub fn endpoints(&self) -> impl Iterator<Item = &EndpointDescriptor> {
        self.endpoints.iter().flatten()
    }

    /// The endpoint at `address`, if this setting brings it.
    #[must_use]
    pub fn endpoint(&self, address: u8) -> Option<&EndpointDescriptor> {
        self.endpoints()
            .find(|endpoint| endpoint.address == address)
    }

    /// The setting's endpoints as a mask of Device Context Indices.
    #[must_use]
    pub fn dci_mask(&self) -> u32 {
        self.endpoints()
            .fold(0, |mask, endpoint| mask | 1 << endpoint.dci())
    }
}

/// The descriptors after a configuration's own header.
///
/// # Errors
///
/// [`DriverError::BadMagic`] for bytes that do not open with a configuration
/// descriptor.
fn body(config: &[u8]) -> Result<&[u8], DriverError> {
    let header = ConfigurationHeader::decode(config).map_err(|Malformed| DriverError::BadMagic)?;
    config.get(header.length..).ok_or(DriverError::BadMagic)
}

/// Setting `alternate` of interface `interface` in the configuration
/// descriptor `config`, with every endpoint and `SuperSpeed` companion it
/// brings.
///
/// # Errors
///
/// * [`DriverError::NotFound`] when the configuration has no such setting.
/// * [`DriverError::BadMagic`] for a malformed configuration, a malformed
///   endpoint or companion within the setting, two endpoints at one address,
///   two descriptors of the same setting, or more than [`MAX_ALT_ENDPOINTS`]
///   endpoints.
pub fn alternate_setting(
    config: &[u8],
    interface: u8,
    alternate: u8,
) -> Result<AltSetting, DriverError> {
    let mut found: Option<AltSetting> = None;
    let mut collecting = false;
    let mut count = 0usize;
    for descriptor in descriptors(body(config)?) {
        let descriptor = descriptor.map_err(|Malformed| DriverError::BadMagic)?;
        match descriptor[1] {
            DESC_TYPE_INTERFACE => {
                if descriptor.len() < INTERFACE_DESCRIPTOR_LEN {
                    return Err(DriverError::BadMagic);
                }
                collecting = descriptor[2] == interface && descriptor[3] == alternate;
                if collecting {
                    if found.is_some() {
                        return Err(DriverError::BadMagic);
                    }
                    found = Some(AltSetting {
                        interface,
                        alternate,
                        class24: (u32::from(descriptor[5]) << 16)
                            | (u32::from(descriptor[6]) << 8)
                            | u32::from(descriptor[7]),
                        endpoints: [None; MAX_ALT_ENDPOINTS],
                    });
                }
            }
            DESC_TYPE_ENDPOINT if collecting => {
                let endpoint = EndpointDescriptor::decode(descriptor)?;
                let Some(setting) = found.as_mut() else {
                    return Err(DriverError::BadMagic);
                };
                if setting.endpoint(endpoint.address).is_some() || count == MAX_ALT_ENDPOINTS {
                    return Err(DriverError::BadMagic);
                }
                setting.endpoints[count] = Some(endpoint);
                count += 1;
            }
            DESC_TYPE_SS_ENDPOINT_COMPANION if collecting => {
                let companion = SsCompanion::decode(descriptor)?;
                if let Some(last) = count
                    .checked_sub(1)
                    .and_then(|at| found.as_mut()?.endpoints[at].as_mut())
                {
                    last.companion = Some(companion);
                }
            }
            _ => {}
        }
    }
    found.ok_or(DriverError::NotFound)
}

/// Every interface number the configuration declares a setting of.
///
/// # Errors
///
/// [`DriverError::BadMagic`] for a malformed configuration.
pub fn interface_numbers(config: &[u8]) -> Result<BitSet256, DriverError> {
    let mut numbers = BitSet256::new();
    for descriptor in descriptors(body(config)?) {
        let descriptor = descriptor.map_err(|Malformed| DriverError::BadMagic)?;
        if descriptor[1] == DESC_TYPE_INTERFACE {
            if descriptor.len() < INTERFACE_DESCRIPTOR_LEN {
                return Err(DriverError::BadMagic);
            }
            numbers.insert(u16::from(descriptor[2]));
        }
    }
    Ok(numbers)
}

/// Whether `interface` is driven over the default control endpoint alone:
/// it has one setting, and that brings no endpoint. Such an interface — a
/// USB Audio 1.0 control interface is one — is a function of its own and
/// governs the streaming siblings it claims.
///
/// # Errors
///
/// [`DriverError::BadMagic`] for a malformed configuration, or an endpoint
/// descriptor of the interface shorter than one.
pub fn is_control_only(config: &[u8], interface: u8) -> Result<bool, DriverError> {
    let mut settings = 0usize;
    let mut endpoints = 0usize;
    let mut within = false;
    for descriptor in descriptors(body(config)?) {
        let descriptor = descriptor.map_err(|Malformed| DriverError::BadMagic)?;
        match descriptor[1] {
            DESC_TYPE_INTERFACE => {
                if descriptor.len() < INTERFACE_DESCRIPTOR_LEN {
                    return Err(DriverError::BadMagic);
                }
                within = descriptor[2] == interface;
                settings += usize::from(within);
            }
            DESC_TYPE_ENDPOINT if within => {
                if descriptor.len() < ENDPOINT_DESCRIPTOR_LEN {
                    return Err(DriverError::BadMagic);
                }
                endpoints += 1;
            }
            _ => {}
        }
    }
    Ok(settings == 1 && endpoints == 0)
}

/// Interfaces a configuration groups into one function (USB 2.0 Interface
/// Association Descriptor ECN).
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct InterfaceAssociation {
    /// `bFirstInterface`.
    pub first: u8,
    /// `bInterfaceCount`.
    pub count: u8,
    /// `(bFunctionClass << 16) | (bFunctionSubClass << 8) |
    /// bFunctionProtocol`.
    pub class24: u32,
}

impl InterfaceAssociation {
    /// Whether the function includes interface `interface`.
    #[must_use]
    pub const fn covers(&self, interface: u8) -> bool {
        interface >= self.first && (interface - self.first) < self.count
    }
}

/// The association covering `interface`, if the configuration states one.
///
/// # Errors
///
/// [`DriverError::BadMagic`] for a malformed configuration or association.
pub fn association_of(
    config: &[u8],
    interface: u8,
) -> Result<Option<InterfaceAssociation>, DriverError> {
    for descriptor in descriptors(body(config)?) {
        let descriptor = descriptor.map_err(|Malformed| DriverError::BadMagic)?;
        if descriptor[1] != DESC_TYPE_INTERFACE_ASSOCIATION {
            continue;
        }
        if descriptor.len() < ASSOCIATION_LEN || descriptor[3] == 0 {
            return Err(DriverError::BadMagic);
        }
        let association = InterfaceAssociation {
            first: descriptor[2],
            count: descriptor[3],
            class24: (u32::from(descriptor[4]) << 16)
                | (u32::from(descriptor[5]) << 8)
                | u32::from(descriptor[6]),
        };
        if association.covers(interface) {
            return Ok(Some(association));
        }
    }
    Ok(None)
}

#[cfg(test)]
#[path = "alternate_tests.rs"]
mod tests;
