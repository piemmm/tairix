//! The `Run` binary of the PCM5122 driver, autoloaded into user space by
//! `devmgr` for a discovered `ti,pcm5122` node: it reaches the part over the
//! transfer endpoint its node's grant names, sets it up to follow its
//! interface's clocks, and serves `codec-v1` under the node's codec duty. On
//! the host it is an inert stub.

#![cfg_attr(freestanding, no_std)]
#![cfg_attr(freestanding, no_main)]
#![deny(missing_docs)]

#[cfg(freestanding)]
mod program {
    use tairix_abi::hwlink::LinkRole;
    use tairix_audiochan::{exit, fail};
    use tairix_caps::CapabilitySet;
    use tairix_drv_audio_pcm5122::{Pcm5122, REQUIRED_CAPABILITIES};
    use tairix_drvrt::{RtDriverHost, RtGrantSyscalls};
    use tairix_rt::ClockDelay;

    fn main() -> i32 {
        let mut caps = CapabilitySet::empty();
        for cap in REQUIRED_CAPABILITIES {
            caps.insert(*cap);
        }
        let Ok(host) = RtDriverHost::from_grants_query(caps, RtGrantSyscalls, None) else {
            return fail(
                exit::NO_HOST,
                "pcm5122: the node's grants could not be read",
                None,
            );
        };
        let duty = host
            .resources()
            .find_map(|r| r.link_duty().ok())
            .filter(|duty| duty.role() == LinkRole::Codec);
        let (Some(duty), Some(_)) = (duty, host.endpoint_grant()) else {
            return fail(
                exit::NO_RESOURCES,
                "pcm5122: the node carries no codec duty and transfer endpoint",
                None,
            );
        };
        let mut codec = Pcm5122::new(&host, ClockDelay::new());
        if let Err(err) = codec.bring_up() {
            return fail(
                exit::BRINGUP_FAILED,
                "pcm5122: the part would not set up",
                Some(err),
            );
        }
        tairix_codec::serve(codec, &duty)
    }

    tairix_rt::entry!(main);
}

#[cfg(not(freestanding))]
fn main() {}
