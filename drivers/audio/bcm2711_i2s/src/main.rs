//! The `Run` binary of the PCM/I²S driver, autoloaded into user space by
//! `devmgr` for a discovered `brcm,bcm2835-i2s` node whose sound card links it
//! to a codec.
//!
//! It opens a DMA channel on the node's transmit request line, reaches its
//! codec and, where this side drives the bit clock, its clock through the
//! node's links, and serves `audiochan-v1`, waking on each period boundary the
//! DMA controller answers. On the host it is an inert stub.

#![cfg_attr(freestanding, no_std)]
#![cfg_attr(freestanding, no_main)]
#![deny(missing_docs)]

#[cfg(freestanding)]
mod program {
    use tairix_abi::driver::sole_register_window;
    use tairix_abi::hwlink::{LinkRequest, LinkRole};
    use tairix_abi::{DriverError, MmioMapper, HW_NODE_MAX_RESOURCES};
    use tairix_audiochan::cyclic::LinkDma;
    use tairix_audiochan::{exit, fail, serve, Wake};
    use tairix_caps::CapabilitySet;
    use tairix_drv_audio_bcm2711_i2s::interface::Interface;
    use tairix_drv_audio_bcm2711_i2s::pcm::{self, Pcm};
    use tairix_drv_audio_bcm2711_i2s::REQUIRED_CAPABILITIES;
    use tairix_drvrt::{RtDriverHost, RtGrantSyscalls};
    use tairix_linkclient::{ClockClient, CodecClient, DmaClient, RtLinkCall};

    /// The `dma-names` entry of the transmit request line.
    const TRANSMIT: &[u8] = b"tx";

    fn driver_caps() -> CapabilitySet {
        let mut caps = CapabilitySet::empty();
        for cap in REQUIRED_CAPABILITIES {
            caps.insert(*cap);
        }
        caps
    }

    fn main() -> i32 {
        let Ok(host) = RtDriverHost::from_grants_query(driver_caps(), RtGrantSyscalls, None) else {
            return fail(
                exit::NO_HOST,
                "i2s: the node's grants could not be read",
                None,
            );
        };
        let mut granted = [None; HW_NODE_MAX_RESOURCES];
        for (slot, resource) in granted.iter_mut().zip(host.resources()) {
            *slot = Some(*resource);
        }
        let links = || {
            granted
                .iter()
                .flatten()
                .filter_map(|resource| resource.link_request().ok())
        };
        let of_role = move |role| links().filter(move |link: &LinkRequest| link.role() == role);
        let dreq = of_role(LinkRole::Dma).find(|link| link.name() == TRANSMIT);
        // One interface drives one codec: a card linking more would need the
        // time slots no link describes.
        let mut codecs = of_role(LinkRole::Codec);
        let codec_link = match (codecs.next(), codecs.next()) {
            (Some(link), None) => Some(link),
            _ => None,
        };
        let clock_link = of_role(LinkRole::Clock).next();
        let (Ok((base, len)), Some(dreq), Some(codec_link)) = (
            sole_register_window(granted.iter().flatten()),
            dreq,
            codec_link,
        ) else {
            return fail(
                exit::NO_RESOURCES,
                "i2s: the node names no single register window, transmit request line and codec",
                None,
            );
        };
        let Ok(regs) = host.map_window(base, len) else {
            return fail(
                exit::BRINGUP_FAILED,
                "i2s: the register window would not map",
                None,
            );
        };
        let Ok(block) = Pcm::new(&regs) else {
            return fail(
                exit::BRINGUP_FAILED,
                "i2s: the register window is too short",
                None,
            );
        };
        let Ok(codec) = CodecClient::new(RtLinkCall::new(codec_link.endpoint()), codec_link) else {
            return fail(
                exit::NO_RESOURCES,
                "i2s: the codec link states no interface link",
                None,
            );
        };
        let clock = clock_link.map(|link| ClockClient::new(RtLinkCall::new(link.endpoint()), link));
        let client = match DmaClient::open(RtLinkCall::new(dreq.endpoint()), dreq) {
            Ok(client) => client,
            Err(err) => {
                return fail(
                    exit::BRINGUP_FAILED,
                    "i2s: no DMA channel opened on the transmit request line",
                    Some(DriverError::from_errno(err)),
                );
            }
        };
        let channel = LinkDma::new(client, base + pcm::FIFO);
        let interface = match Interface::new(block, channel, clock, codec) {
            Ok(interface) => interface,
            Err(err) => {
                return fail(
                    exit::BRINGUP_FAILED,
                    "i2s: the codec, its link and the bit clock admit no stream",
                    Some(err),
                );
            }
        };
        serve(interface, &[Wake::CallReply(dreq.endpoint())])
    }

    tairix_rt::entry!(main);
}

#[cfg(not(freestanding))]
fn main() {}
