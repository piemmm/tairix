//! The `Run` binary of the PCM5102A driver, autoloaded into user space by
//! `devmgr` for a discovered `ti,pcm5102a` node: it serves `codec-v1` under
//! the node's codec duty. On the host it is an inert stub.

#![cfg_attr(freestanding, no_std)]
#![cfg_attr(freestanding, no_main)]
#![deny(missing_docs)]

#[cfg(freestanding)]
mod program {
    use tairix_abi::hwlink::LinkRole;
    use tairix_audiochan::{exit, fail};
    use tairix_caps::CapabilitySet;
    use tairix_drv_audio_pcm5102a::{Pcm5102a, REQUIRED_CAPABILITIES};
    use tairix_drvrt::{RtDriverHost, RtGrantSyscalls};

    fn main() -> i32 {
        let mut caps = CapabilitySet::empty();
        for cap in REQUIRED_CAPABILITIES {
            caps.insert(*cap);
        }
        let Ok(host) = RtDriverHost::from_grants_query(caps, RtGrantSyscalls, None) else {
            return fail(
                exit::NO_HOST,
                "pcm5102a: the node's grants could not be read",
                None,
            );
        };
        let Some(duty) = host
            .resources()
            .find_map(|r| r.link_duty().ok())
            .filter(|duty| duty.role() == LinkRole::Codec)
        else {
            return fail(
                exit::NO_RESOURCES,
                "pcm5102a: the node carries no codec duty",
                None,
            );
        };
        tairix_codec::serve(Pcm5102a, &duty)
    }

    tairix_rt::entry!(main);
}

#[cfg(not(freestanding))]
fn main() {}
