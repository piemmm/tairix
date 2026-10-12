//! The `Run` program of a BCM2711 HDMI transmitter: autoloaded by `devmgr`
//! against an HDMI node once the clocks, the DDC bus, the interrupt
//! controller and the DMA controller it links to serve, it learns which
//! transmitter the node is from the bind key it matched, opens its links and
//! serves its encoder endpoint and, beside it, the audio device channel its
//! sink plays, for the life of the process. Sound that cannot be had — no
//! audio clock or DMA line, or no channel opened on it — is stated, and the
//! transmitter serves video alone.
//!
//! On the host it is an inert stub so the workspace build, clippy and fmt
//! still cover the file.

#![cfg_attr(freestanding, no_std)]
#![cfg_attr(freestanding, no_main)]
#![deny(missing_docs)]

#[cfg(freestanding)]
mod program {
    use tairix_abi::driver::{named_register_window, DeviceWake};
    use tairix_abi::hwlink::{LinkRequest, LinkRole};
    use tairix_abi::hwtree::HwProperty;
    use tairix_abi::RegisterWindow;
    use tairix_audiochan::cyclic::LinkDma;
    use tairix_audiochan::Channel;
    use tairix_drv_display_bcm2711_hdmi::{
        AudioLinks, Hdmi, Links, Record, Shared, Variant, Windows, AUDIO_CLOCK, AUDIO_DMA,
        BIND_KEYS, BVB_CLOCK, DDC_LINK, HOTPLUG_LINES, HSM_CLOCK, REQUIRED_CAPABILITIES, VARIANTS,
        WINDOWS,
    };
    use tairix_drvrt::serve::{exit, log_event, DutyNode};
    use tairix_drvrt::Recorder;
    use tairix_linkclient::{ClockClient, DmaClient, I2cClient, IrqLineClient, RtLinkCall};
    use tairix_log::{EventId, Field, FieldValue, Level};
    use tairix_rt::shm::MappedGrant;
    use tairix_rt::ClockDelay;

    const BRINGUP_FAILED: EventId = EventId(24_910);
    const SINK_UNREADABLE: EventId = EventId(24_911);
    const LINK_LOST: EventId = EventId(24_912);
    const UNSCRAMBLED: EventId = EventId(24_913);
    const SILENT: EventId = EventId(24_914);

    /// State that the transmitter carries no sound, and why.
    fn silent(cause: &'static str) {
        log_event(
            Level::Warn,
            SILENT,
            "hdmi: the transmitter carries no sound; its video is served alone",
            &[Field {
                key: "cause",
                value: FieldValue::Str(cause),
            }],
        );
    }

    /// The system log, as the transmitter records to it.
    struct Log;

    impl Recorder<Record> for Log {
        fn record(&mut self, record: Record) {
            match record {
                Record::Unreadable => log_event(
                    Level::Warn,
                    SINK_UNREADABLE,
                    "hdmi: the sink's EDID could not be read; it is given 1024x768 as DVI",
                    &[],
                ),
                Record::LinkLost { link } => log_event(
                    Level::Info,
                    LINK_LOST,
                    "hdmi: the sink forgot its link's scrambling; the output is set up again",
                    &[Field {
                        key: "link",
                        value: FieldValue::UnsignedInt(u64::from(link)),
                    }],
                ),
                Record::Unscrambled => log_event(
                    Level::Info,
                    UNSCRAMBLED,
                    "hdmi: the sink has not seen the link scrambled; it is told again",
                    &[],
                ),
            }
        }
    }

    /// The transmitter the node is, by the rule the device manager matched.
    fn variant() -> Option<Variant> {
        let record = tairix_rt::hw_self_node().ok()?;
        tairix_devmatch::matched_bind_key(record.match_keys(), BIND_KEYS)
            .and_then(|at| VARIANTS.get(at).copied())
    }

    /// The node's windows, in [`WINDOWS`] order.
    fn windows(node: &DutyNode) -> Result<Windows<RegisterWindow>, i32> {
        let [core, dvp, phy, rm, packet, csc, hd] = WINDOWS;
        Ok(Windows {
            core: node.map_named(core)?,
            dvp: node.map_named(dvp)?,
            phy: node.map_named(phy)?,
            rm: node.map_named(rm)?,
            packet: node.map_named(packet)?,
            csc: node.map_named(csc)?,
            hd: node.map_named(hd)?,
        })
    }

    /// Program entry point; on success it never returns.
    fn main() -> i32 {
        let caps = REQUIRED_CAPABILITIES.iter().copied().collect();
        let node = match DutyNode::open(caps, LinkRole::Encoder, BRINGUP_FAILED) {
            Ok(node) => node,
            Err(code) => return code,
        };
        let Some(variant) = variant() else {
            return node.fail(
                exit::NO_RESOURCES,
                "hdmi: the node matches neither transmitter's binding",
            );
        };
        let regs = match windows(&node) {
            Ok(regs) => regs,
            Err(code) => return code,
        };
        let links = || {
            node.resources()
                .filter_map(|resource| resource.link_request().ok())
        };
        let named = |role, name: &[u8]| {
            links().find(|link: &LinkRequest| link.role() == role && link.name() == name)
        };
        let line = |index| {
            links().find(|link: &LinkRequest| {
                link.role() == LinkRole::InterruptLine && link.index() == index
            })
        };
        let [connected, removed] = HOTPLUG_LINES;
        let (Some(hsm), Some(bvb), Some(ddc), Some(connected), Some(removed)) = (
            named(LinkRole::Clock, HSM_CLOCK),
            named(LinkRole::Clock, BVB_CLOCK),
            named(LinkRole::I2c, DDC_LINK),
            line(connected),
            line(removed),
        ) else {
            return node.fail(
                exit::NO_RESOURCES,
                "hdmi: the node lacks a video clock, its DDC bus or a hotplug line",
            );
        };
        let Ok((hd_base, _)) = named_register_window(node.resources(), WINDOWS[6]) else {
            return node.fail(exit::NO_RESOURCES, "hdmi: the node names no shared block");
        };
        let Ok(ddc) = I2cClient::open(RtLinkCall::new(ddc.endpoint()), ddc, MappedGrant::map)
        else {
            return node.fail(exit::BRINGUP_FAILED, "hdmi: the DDC bus would not open");
        };
        let sound = if let (Some(clock), Some(dma)) = (
            named(LinkRole::Clock, AUDIO_CLOCK),
            named(LinkRole::Dma, AUDIO_DMA),
        ) {
            if let Ok(channel) = DmaClient::open(RtLinkCall::new(dma.endpoint()), dma) {
                Some((clock, dma, channel))
            } else {
                silent("no DMA channel opened on its audio line");
                None
            }
        } else {
            silent("the node links no audio clock or audio DMA line");
            None
        };
        // Both lines answer through their controller's one endpoint.
        let wakes = [DeviceWake::CallReply(connected.endpoint())];
        let audio_endpoint = sound.as_ref().map(|(_, dma, _)| dma.endpoint());
        let links = Links {
            hsm: ClockClient::new(RtLinkCall::new(hsm.endpoint()), hsm),
            bvb: ClockClient::new(RtLinkCall::new(bvb.endpoint()), bvb),
            ddc,
            hotplug: [
                IrqLineClient::new(RtLinkCall::new(connected.endpoint()), connected),
                IrqLineClient::new(RtLinkCall::new(removed.endpoint()), removed),
            ],
            audio: sound.map(|(clock, _, channel)| AudioLinks {
                clock: ClockClient::new(RtLinkCall::new(clock.endpoint()), clock),
                dma: LinkDma::new(channel, hd_base + variant.mai_data() as u64),
            }),
        };
        let coexists = node.host().property(HwProperty::RadioCoexistence).is_some();
        let Ok(hdmi) = Hdmi::open(regs, variant, links, ClockDelay::new(), coexists, Log) else {
            return node.fail(
                exit::BRINGUP_FAILED,
                "hdmi: the transmitter would not come up",
            );
        };
        let Some(audio_endpoint) = audio_endpoint else {
            return tairix_encoder::serve(hdmi, &node.duty(), &wakes);
        };
        let (encoder, audio) = Shared::pair(hdmi);
        let channel = match Channel::publish(audio) {
            Ok(channel) => channel,
            Err(code) => return code,
        };
        let audio_wakes = [DeviceWake::CallReply(audio_endpoint)];
        tairix_encoder::serve_with_audio(encoder, &node.duty(), &wakes, channel, &audio_wakes)
    }

    tairix_rt::entry!(main);
}

// On the host the program's real entry is not compiled, so this inert `main`
// keeps the crate building under the host tooling.
#[cfg(not(freestanding))]
fn main() {}
