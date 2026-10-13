//! The `Run` binary of the PCM5122 driver, autoloaded into user space by
//! `devmgr` for a discovered `ti,pcm5122` node: it reaches the part over the
//! I²C target channel its node's link names, sets it up to follow its
//! interface's clocks, and serves `codec-v1` under the node's codec duty. On
//! the host it is an inert stub.

#![cfg_attr(freestanding, no_std)]
#![cfg_attr(freestanding, no_main)]
#![deny(missing_docs)]

#[cfg(freestanding)]
mod program {
    use tairix_abi::hwlink::{sole_link, LinkRole};
    use tairix_abi::DriverError;
    use tairix_drv_audio_pcm5122::{Pcm5122, REQUIRED_CAPABILITIES};
    use tairix_drvrt::exit::{end, error, step, DriverExit};
    use tairix_drvrt::{RtDriverHost, RtGrantSyscalls};
    use tairix_linkclient::{I2cTarget, RtLinkCall};
    use tairix_rt::shm::MappedGrant;
    use tairix_rt::ClockDelay;

    fn main() -> i32 {
        let caps = REQUIRED_CAPABILITIES.iter().copied().collect();
        let Ok(host) = RtDriverHost::from_grants_query(caps, RtGrantSyscalls, None) else {
            return end(
                DriverExit::NoHost,
                &[step("pcm5122: the node's grants could not be read")],
            );
        };
        let duty = host
            .resources()
            .find_map(|r| r.link_duty().ok())
            .filter(|duty| duty.role() == LinkRole::Codec);
        let (Some(duty), Some(link)) = (duty, sole_link(host.resources(), LinkRole::I2c)) else {
            return end(
                DriverExit::NoResources,
                &[step(
                    "pcm5122: the node carries no codec duty and I2C target link",
                )],
            );
        };
        let port = match I2cTarget::open(RtLinkCall::new(link.endpoint()), link, MappedGrant::map) {
            Ok(port) => port,
            Err(err) => {
                return end(
                    DriverExit::BringupFailed,
                    &[
                        step("pcm5122: the part's I2C channel could not be opened"),
                        error(DriverError::from_errno(err)),
                    ],
                );
            }
        };
        let mut codec = Pcm5122::new(port, ClockDelay::new());
        if let Err(err) = codec.bring_up() {
            return end(
                DriverExit::BringupFailed,
                &[step("pcm5122: the part would not set up"), error(err)],
            );
        }
        tairix_codec::serve(codec, &duty)
    }

    tairix_rt::entry!(main);
}

#[cfg(not(freestanding))]
fn main() {}
