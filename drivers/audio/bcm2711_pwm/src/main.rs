//! The `Run` binary of the headphone-jack driver, autoloaded into user space
//! by `devmgr` for a discovered `tairix,bcm2711-pwm-audio` node.
//!
//! It runs the PWM clock its clock link names, starts the PWM block at the
//! levels that clock gives, opens a channel on its DMA request line, ramps
//! the jack to silence, and serves `audiochan-v1`, waking on each period
//! boundary the DMA controller answers. On the host it is an inert stub.

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
    use tairix_drv_audio_bcm2711_pwm::jack::Jack;
    use tairix_drv_audio_bcm2711_pwm::pwm::{self, Pwm};
    use tairix_drv_audio_bcm2711_pwm::{timing, CLOCK_HZ, REQUIRED_CAPABILITIES};
    use tairix_drvrt::{RtDriverHost, RtGrantSyscalls};
    use tairix_linkclient::{ClockClient, DmaClient, RtLinkCall};

    /// The dither's seed: its sequence wants decorrelation, not secrecy.
    const DITHER_SEED: u64 = 0x5057_4D44_4954_4852;

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
                "pwm audio: the node's grants could not be read",
                None,
            );
        };
        let mut granted = [None; HW_NODE_MAX_RESOURCES];
        for (slot, resource) in granted.iter_mut().zip(host.resources()) {
            *slot = Some(*resource);
        }
        let resources = || granted.iter().flatten();
        let request_for = |role| {
            resources()
                .filter_map(|r| r.link_request().ok())
                .find(|request: &LinkRequest| request.role() == role)
        };
        let (Ok((base, len)), Some(line), Some(clock_link)) = (
            sole_register_window(resources()),
            request_for(LinkRole::Dma),
            request_for(LinkRole::Clock),
        ) else {
            return fail(
                exit::NO_RESOURCES,
                "pwm audio: the node names no single register window, DMA request line and clock",
                None,
            );
        };
        let Ok(regs) = host.map_window(base, len) else {
            return fail(
                exit::BRINGUP_FAILED,
                "pwm audio: the register window would not map",
                None,
            );
        };
        let Ok(block) = Pwm::new(&regs) else {
            return fail(
                exit::BRINGUP_FAILED,
                "pwm audio: the register window is too short",
                None,
            );
        };
        let mut clock = ClockClient::new(RtLinkCall::new(clock_link.endpoint()), clock_link);
        let clock_hz = match clock.run(CLOCK_HZ) {
            Ok(hz) => hz,
            Err(err) => {
                return fail(
                    exit::BRINGUP_FAILED,
                    "pwm audio: the PWM clock would not run",
                    Some(DriverError::from_errno(err)),
                );
            }
        };
        let Some((levels, rate)) = timing(clock_hz) else {
            return fail(
                exit::BRINGUP_FAILED,
                "pwm audio: the PWM clock runs too slow to shape onto",
                None,
            );
        };
        if let Err(err) = block.run(levels) {
            return fail(
                exit::BRINGUP_FAILED,
                "pwm audio: the PWM block would not start",
                Some(err),
            );
        }
        let client = match DmaClient::open(RtLinkCall::new(line.endpoint()), line) {
            Ok(client) => client,
            Err(err) => {
                return fail(
                    exit::BRINGUP_FAILED,
                    "pwm audio: no DMA channel opened on the request line",
                    Some(DriverError::from_errno(err)),
                );
            }
        };
        let channel = LinkDma::new(client, base + pwm::FIFO);
        let mut jack = match Jack::new(channel, rate, levels, DITHER_SEED) {
            Ok(jack) => jack,
            Err(err) => {
                return fail(
                    exit::BRINGUP_FAILED,
                    "pwm audio: too few levels to shape onto",
                    Some(err),
                )
            }
        };
        if let Err(err) = jack.bring_up() {
            return fail(
                exit::BRINGUP_FAILED,
                "pwm audio: the jack would not ramp to silence",
                Some(err),
            );
        }
        serve(jack, &[Wake::CallReply(line.endpoint())])
    }

    tairix_rt::entry!(main);
}

#[cfg(not(freestanding))]
fn main() {}
