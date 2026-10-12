//! The `Run` entry-point binary of the NXP PCF8523 real-time-clock driver,
//! installed as a signed `/System/Drivers/` bundle and **autoloaded into user
//! space** by `devmgr` when a `nxp,pcf8523` node is discovered
//! (`plans/TIMESYNC.md` TS-4).
//!
//! The process reaches its chip over one thing only: the I²C target channel
//! its node's link names. The controller holds every message to the one
//! address that link reaches, so this driver cannot reach a neighbour on the
//! bus however it is compromised.
//!
//! It holds **no** clock authority either: the machine clock is set by the
//! one holder of `CAP_TIME_SET` (`userland/system/timed`), which reads the
//! RTC service endpoint this process serves and tags the reading `Firmware`
//! itself. That split is what makes the wall clock's provenance ladder
//! enforceable.
//!
//! It is a **pure-Rust** program linking the Rust userland runtime
//! `tairix-rt`, never the C ABI. Bring-up puts the part in 24-hour mode with its time circuits running. A bring-up failure exits with a
//! reserved fail-closed code, leaving the machine without a local time source
//! rather than wedged. On the host it is an inert stub so
//! `cargo build --workspace`, clippy, and fmt still cover the file.

#![cfg_attr(freestanding, no_std)]
#![cfg_attr(freestanding, no_main)]
#![deny(missing_docs)]

// --- Pure-Rust program --------------------------------------------------
#[cfg(freestanding)]
mod program {
    use tairix_abi::hwlink::{sole_link, LinkRole};
    use tairix_abi::rtc_ipc::{self, RTC_ENDPOINT};
    use tairix_abi::CapabilityId;
    use tairix_caps::CapabilitySet;
    use tairix_drv_rtc_pcf8523::Pcf8523;
    use tairix_drvrt::{RtDriverHost, RtGrantSyscalls};
    use tairix_linkclient::{I2cTarget, RtLinkCall};
    use tairix_rt::shm::MappedGrant;

    /// Exit code when the rt-backed driver host could not be built from the
    /// kernel-delivered grants. A reserved, fail-closed value.
    const EXIT_NO_HOST: i32 = 80;

    /// Exit code when the node links no one I²C target — an unbound or
    /// mis-provisioned node. A reserved, fail-closed value.
    const EXIT_NO_RESOURCES: i32 = 81;

    /// Exit code when the chip's channel could not be opened, or the chip
    /// brought up over it. A reserved value.
    const EXIT_BRINGUP_FAILED: i32 = 82;

    /// Exit code when the call endpoint could not be created — the id is
    /// already bound (another RTC driver serves it; the first RTC in
    /// hardware-tree order wins and this one stands down) or the driver lacks
    /// `CAP_IPC_BIND_PRIVILEGED`. A reserved, fail-closed value.
    const EXIT_ENDPOINT_FAILED: i32 = 83;

    /// Exit code when the serve loop's `call_recv` failed — a destroyed
    /// endpoint or a torn-down task, both terminal. Exiting fail-loud beats
    /// yield-retrying a dead channel forever, which is a busy spin.
    const EXIT_SERVE_FAILED: i32 = 84;

    /// Bound on the number of in-flight requests the endpoint queues. The
    /// driver answers each request before receiving the next and its one
    /// client asks a handful of times a day, so a small capacity suffices; it
    /// is a queue bound, not a hardware capacity.
    const ENDPOINT_CAPACITY: usize = 4;

    /// The required-sender capability set of the served endpoint.
    ///
    /// `CAP_TIME_SET` rather than a capability of its own: the only principal
    /// with a reason to read or write the board's clock chip is the one that
    /// sets the machine clock from it, so an existing capability already
    /// expresses the authority exactly and the kernel admits nobody else.
    fn endpoint_send_caps() -> CapabilitySet {
        let mut caps = CapabilitySet::empty();
        caps.insert(CapabilityId::TIME_SET);
        caps
    }

    /// Program entry point. `tairix-rt`'s `_start` calls it once the runtime
    /// is set up and routes its return value through the `exit` syscall.
    ///
    /// On success this never returns: the serve loop runs for the life of the
    /// driver process.
    fn main() -> i32 {
        let caps = tairix_drv_rtc_pcf8523::REQUIRED_CAPABILITIES
            .iter()
            .copied()
            .collect();
        let Ok(host) = RtDriverHost::from_grants_query(caps, RtGrantSyscalls, None) else {
            return EXIT_NO_HOST;
        };
        let Some(link) = sole_link(host.resources(), LinkRole::I2c) else {
            return EXIT_NO_RESOURCES;
        };
        let Ok(port) = I2cTarget::open(RtLinkCall::new(link.endpoint()), link, MappedGrant::map)
        else {
            return EXIT_BRINGUP_FAILED;
        };
        let Ok(mut rtc) = Pcf8523::open(port) else {
            return EXIT_BRINGUP_FAILED;
        };

        let send_caps = endpoint_send_caps();
        let recv_caps = CapabilitySet::empty();
        if tairix_rt::call_create(
            RTC_ENDPOINT,
            &send_caps,
            &recv_caps,
            rtc_ipc::REQUEST_LEN,
            rtc_ipc::REPLY_LEN,
            ENDPOINT_CAPACITY,
        ) != 0
        {
            return EXIT_ENDPOINT_FAILED;
        }

        serve(&mut rtc)
    }

    /// The serve loop: block in `call_recv` (a real park between requests),
    /// run the request against the chip, and answer with `call_reply`.
    ///
    /// A `call_recv` error is terminal — the endpoint is sized so an oversize
    /// request is refused at send time, leaving only a destroyed endpoint or
    /// a torn-down task — so it ends the driver fail-loud rather than
    /// yield-retrying a dead channel forever. A reply that fails to encode is
    /// dropped to a zero-length reply, which the client decodes as a
    /// fail-closed truncation.
    fn serve(rtc: &mut Pcf8523<I2cTarget<RtLinkCall, MappedGrant>>) -> i32 {
        let mut request = [0u8; rtc_ipc::REQUEST_LEN];
        let mut reply = [0u8; rtc_ipc::REPLY_LEN];
        loop {
            let mut ticket = 0u64;
            match tairix_rt::call_recv(RTC_ENDPOINT, &mut request, &mut ticket) {
                Ok(n) => {
                    let len = rtc_ipc::serve_request(rtc, &request[..n], &mut reply).unwrap_or(0);
                    let _ = tairix_rt::call_reply(RTC_ENDPOINT, ticket, &reply[..len]);
                }
                Err(_) => return EXIT_SERVE_FAILED,
            }
        }
    }

    tairix_rt::entry!(main);
}

#[cfg(not(freestanding))]
fn main() {}
