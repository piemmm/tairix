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
    use tairix_abi::driver::DeviceWake;
    use tairix_abi::hwlink::{LinkRequest, LinkRole};
    use tairix_abi::{DriverError, MmioMapper, HW_NODE_MAX_RESOURCES};
    use tairix_audiochan::cyclic::LinkDma;
    use tairix_audiochan::serve;
    use tairix_drv_audio_bcm2711_pwm::jack::Jack;
    use tairix_drv_audio_bcm2711_pwm::pwm::{self, Pwm};
    use tairix_drv_audio_bcm2711_pwm::{timing, CLOCK_HZ, REQUIRED_CAPABILITIES};
    use tairix_drvrt::exit::{end, error, step, DriverExit};
    use tairix_drvrt::{RtDriverHost, RtGrantSyscalls};
    use tairix_linkclient::{ClockClient, DmaClient, RtLinkCall};

    /// The dither's seed: its sequence wants decorrelation, not secrecy.
    const DITHER_SEED: u64 = 0x5057_4D44_4954_4852;

    fn main() -> i32 {
        let caps = REQUIRED_CAPABILITIES.iter().copied().collect();
        let Ok(host) = RtDriverHost::from_grants_query(caps, RtGrantSyscalls, None) else {
            return end(
                DriverExit::NoHost,
                &[step("pwm audio: the node's grants could not be read")],
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
            return end(DriverExit::NoResources, &[step("pwm audio: the node names no single register window, DMA request line and clock")]);
        };
        let Ok(regs) = host.map_window(base, len) else {
            return end(
                DriverExit::BringupFailed,
                &[step("pwm audio: the register window would not map")],
            );
        };
        let Ok(block) = Pwm::new(&regs) else {
            return end(
                DriverExit::BringupFailed,
                &[step("pwm audio: the register window is too short")],
            );
        };
        let mut clock = ClockClient::new(RtLinkCall::new(clock_link.endpoint()), clock_link);
        let clock_hz = match clock.run(CLOCK_HZ) {
            Ok(hz) => hz,
            Err(err) => {
                return end(
                    DriverExit::BringupFailed,
                    &[
                        step("pwm audio: the PWM clock would not run"),
                        error(DriverError::from_errno(err)),
                    ],
                );
            }
        };
        let Some((levels, rate)) = timing(clock_hz) else {
            return end(
                DriverExit::BringupFailed,
                &[step("pwm audio: the PWM clock runs too slow to shape onto")],
            );
        };
        if let Err(err) = block.run(levels) {
            return end(
                DriverExit::BringupFailed,
                &[step("pwm audio: the PWM block would not start"), error(err)],
            );
        }
        let client = match DmaClient::open(RtLinkCall::new(line.endpoint()), line) {
            Ok(client) => client,
            Err(err) => {
                return end(
                    DriverExit::BringupFailed,
                    &[
                        step("pwm audio: no DMA channel opened on the request line"),
                        error(DriverError::from_errno(err)),
                    ],
                );
            }
        };
        let channel = LinkDma::new(client, base + pwm::FIFO);
        let mut jack = match Jack::new(channel, rate, levels, DITHER_SEED) {
            Ok(jack) => jack,
            Err(err) => {
                return end(
                    DriverExit::BringupFailed,
                    &[step("pwm audio: too few levels to shape onto"), error(err)],
                )
            }
        };
        if let Err(err) = jack.bring_up() {
            return end(
                DriverExit::BringupFailed,
                &[
                    step("pwm audio: the jack would not ramp to silence"),
                    error(err),
                ],
            );
        }
        serve(jack, &[DeviceWake::CallReply(line.endpoint())])
    }

    tairix_rt::entry!(main);
}

#[cfg(not(freestanding))]
fn main() {}
