//! The GPU walk — every device a driver serves — and the row each is printed
//! as.

use alloc::format;
use alloc::string::String;

use tairix_abi::gpu::GpuDeviceDescriptor;
use tairix_abi::sysinfo::SysinfoQueryId;
use tairix_abi::Errno;

use crate::list::{walk_records, ListError, WalkStep};
use crate::transport::Transport;

/// Records requested per GPU page: as many as one reply holds.
pub const GPU_PAGE: u16 = tairix_abi::reply_page(GpuDeviceDescriptor::WIRE_LEN);

/// Page through every served GPU ([`SysinfoQueryId::GPU_DEVICES`]) and hand
/// each decoded [`GpuDeviceDescriptor`] to `sink`, in the hardware tree's
/// order.
///
/// # Errors
///
/// * [`ListError::Call`] — the transport failed, or the reply was
///   structurally invalid.
/// * [`ListError::Sink`] — `sink` refused a record; the walk stops there.
pub fn for_each_gpu_device(
    transport: &dyn Transport,
    sink: impl FnMut(&GpuDeviceDescriptor) -> Result<WalkStep, Errno>,
) -> Result<(), ListError> {
    walk_records(
        transport,
        SysinfoQueryId::GPU_DEVICES,
        GpuDeviceDescriptor::WIRE_LEN,
        GPU_PAGE,
        GpuDeviceDescriptor::from_le_bytes,
        sink,
    )
}

/// The columns a [`render_gpu_device`] row fills.
pub const GPU_DEVICE_HEADER: &str = "node  channel";

/// One served GPU's row: the node of the device, and the channel a client
/// opens it on.
#[must_use]
pub fn render_gpu_device(device: &GpuDeviceDescriptor) -> String {
    format!("{:<4}  {:#018x}", device.node, device.endpoint)
}

#[cfg(test)]
mod tests {
    use alloc::vec::Vec;

    use tairix_abi::gpu::{GpuDeviceDescriptor, GPU_CHANNEL_ENDPOINTS};
    use tairix_abi::sysinfo::SysinfoQueryId;

    use super::{for_each_gpu_device, render_gpu_device, GPU_DEVICE_HEADER};
    use crate::list::WalkStep;
    use crate::testing::PagedFixture;

    #[test]
    fn every_served_gpu_is_walked_and_rendered_under_its_header() {
        let devices: Vec<GpuDeviceDescriptor> = (1..=3u32)
            .map(|node| GpuDeviceDescriptor {
                endpoint: GPU_CHANNEL_ENDPOINTS.endpoint(node),
                node,
            })
            .collect();
        let fixture = PagedFixture::new(devices.iter().map(|d| d.to_le_bytes().to_vec()));
        let mut seen = Vec::new();
        for_each_gpu_device(&fixture, |device| {
            seen.push(*device);
            Ok(WalkStep::Continue)
        })
        .expect("walked");
        assert_eq!(seen, devices);
        assert!(fixture
            .seen
            .borrow()
            .iter()
            .all(|&query| query == SysinfoQueryId::GPU_DEVICES));
        let row = render_gpu_device(&devices[0]);
        assert!(row.starts_with("1     0x"), "{row}");
        assert_eq!(GPU_DEVICE_HEADER.find("channel"), row.find("0x"));
    }
}
