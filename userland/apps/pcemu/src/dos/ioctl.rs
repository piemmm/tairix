//! Function `44h`, IOCTL, and `69h`, a disk's serial number and label: a
//! handle's or drive's device information, and the requests that reach a
//! driver's own controls — control data, status, removability, generic
//! IOCTL and the logical drive map — each only where the driver's
//! attributes say it takes them.

use super::data::{cds, sft, sysvars, DRIVES, SYSVARS};
use super::device::generic::{DISK, GET_MEDIA_ID, SET_MEDIA_ID};
use super::device::{attribute, command, field, status, Device};
use super::engine::{locals, take, Far, Fault, Flow, Frame, Id, Kernel, Outcome, Routine, Saved};
use super::file::info;
use super::fs::{cds_at, error};

locals! {
    /// Function `44h` subfunction `function`, or, for `media` set, function
    /// `69h` subfunction `function`. The answer is written to the caller's
    /// registers.
    pub struct Ioctl {
        function: u8,
        media: bool,
    }
}

const CALLED: u8 = 1;

impl Routine for Ioctl {
    const ID: Id = Id::Ioctl;

    fn step(&mut self, k: &mut Kernel<'_, '_>, phase: &mut u8, child: Outcome) -> Flow {
        if *phase == CALLED {
            let word = u16::try_from(take!(child)).unwrap_or(0);
            return self.answered(k, word);
        }
        if self.media {
            return match self.function {
                0x00 => Self::generic_drive(k, phase, GET_MEDIA_ID),
                0x01 => Self::generic_drive(k, phase, SET_MEDIA_ID),
                _ => refuse(error::INVALID_FUNCTION),
            };
        }
        match self.function {
            0x00 => Self::info(k),
            0x01 => Self::set_info(k),
            0x02 | 0x03 | 0x06 | 0x07 | 0x0C | 0x10 => self.handle_request(k, phase),
            0x04 | 0x05 | 0x08 | 0x0D | 0x0E | 0x0F | 0x11 => self.drive_request(k, phase),
            0x09 => Self::drive_attributes(k),
            0x0A => {
                let handle = k.caller(Saved::Bx);
                match k.handle(handle) {
                    Ok((_, entry)) => {
                        let word = k.sft16(entry, sft::INFO);
                        k.set_caller(Saved::Dx, word);
                        Flow::Return(Ok(0))
                    }
                    Err(fault) => Flow::Return(Err(fault)),
                }
            }
            0x0B => {
                let retries = k.caller(Saved::Dx);
                let delay = k.caller(Saved::Cx);
                k.set16(sysvar(sysvars::SHARE_RETRIES), retries);
                k.set16(sysvar(sysvars::SHARE_DELAY), delay);
                Flow::Return(Ok(0))
            }
            _ => refuse(error::INVALID_FUNCTION),
        }
    }
}

/// What subfunction `09h` adds to a drive's attribute word.
pub mod drive_bits {
    /// The network serves the drive.
    pub const REMOTE: u16 = 0x1000;
    /// The drive is shared.
    pub const SHARED: u16 = 0x0200;
    /// `SUBST` made the drive.
    pub const SUBST: u16 = 0x8000;
    /// What makes a drive the network's to the utilities that refuse one.
    pub const NETWORK: u16 = REMOTE | SHARED;
}

fn refuse(code: u16) -> Flow {
    Flow::Return(Err(Fault::Error(code)))
}

/// Where a List of Lists field lies in the kernel's segment.
fn sysvar(field: i16) -> u16 {
    u16::try_from(i32::from(SYSVARS) + i32::from(field)).unwrap_or(0)
}

/// The driver and unit of the drive BL names, `0` the current drive; a
/// shared drive has no driver to ask, as DOS refuses a network drive's.
fn drive_driver(k: &mut Kernel<'_, '_>) -> Result<(Far, u8), u16> {
    let bl = k.caller8(Saved::Bx, false);
    let drive = match bl {
        0 => k.current_drive(),
        n => n - 1,
    };
    if k.is_network(drive) {
        return Err(error::INVALID_FUNCTION);
    }
    k.dpb(drive)
        .map(|d| (d.driver, d.unit))
        .ok_or(error::INVALID_DRIVE)
}

impl Ioctl {
    /// Subfunction `09h`: the drive's driver's attribute word, `SUBST`'s
    /// drive marked in its top bit; a network drive's is the remote bit
    /// alone.
    fn drive_attributes(k: &mut Kernel<'_, '_>) -> Flow {
        let drive = match k.caller8(Saved::Bx, false) {
            0 => k.current_drive(),
            n => n - 1,
        };
        let flags = if drive < DRIVES {
            k.get16(cds_at(drive) + cds::FLAGS)
        } else {
            0
        };
        if flags & cds::NETWORK != 0 {
            k.set_caller(Saved::Dx, drive_bits::REMOTE);
            return Flow::Return(Ok(0));
        }
        let (driver, _) = match drive_driver(k) {
            Ok(found) => found,
            Err(code) => return refuse(code),
        };
        let mut attributes = k.attributes(driver);
        if flags & cds::SUBST != 0 {
            attributes |= drive_bits::SUBST;
        }
        k.set_caller(Saved::Dx, attributes);
        Flow::Return(Ok(0))
    }

    /// Subfunction `00h`: the handle's device information word, a device's
    /// with whether its driver takes IOCTL reads and writes.
    fn info(k: &mut Kernel<'_, '_>) -> Flow {
        let handle = k.caller(Saved::Bx);
        let (_, entry) = match k.handle(handle) {
            Ok(found) => found,
            Err(fault) => return Flow::Return(Err(fault)),
        };
        let mut word = k.sft16(entry, sft::INFO) & !info::STAMPED;
        if let Some(device) = k.sft_device(entry) {
            word |= k.attributes(device) & attribute::IOCTL;
        }
        k.set_caller(Saved::Dx, word);
        k.set_caller(Saved::Ax, word);
        Flow::Return(Ok(0))
    }

    /// Subfunction `01h`: a device handle's low information bits — binary
    /// mode among them — set; a file's cannot be.
    fn set_info(k: &mut Kernel<'_, '_>) -> Flow {
        let handle = k.caller(Saved::Bx);
        let (_, entry) = match k.handle(handle) {
            Ok(found) => found,
            Err(fault) => return Flow::Return(Err(fault)),
        };
        let [dl, dh] = k.caller(Saved::Dx).to_le_bytes();
        if dh != 0 || k.sft_device(entry).is_none() {
            return refuse(error::INVALID_FUNCTION);
        }
        let word = k.sft16(entry, sft::INFO);
        let kept = word & (info::DEVICE | 0x1F | 0xFF00);
        let set = u16::from(dl) & (info::RAW | info::NOT_EOF);
        k.set_sft16(entry, sft::INFO, kept | set);
        Flow::Return(Ok(0))
    }

    /// A request to the device a handle is open on, or a file's status.
    fn handle_request(self, k: &mut Kernel<'_, '_>, phase: &mut u8) -> Flow {
        let handle = k.caller(Saved::Bx);
        let (_, entry) = match k.handle(handle) {
            Ok(found) => found,
            Err(fault) => return Flow::Return(Err(fault)),
        };
        let Some(device) = k.sft_device(entry) else {
            return match self.function {
                0x06 => {
                    let left = k.sft32(entry, sft::POSITION) < k.sft32(entry, sft::SIZE);
                    k.set_caller8(Saved::Ax, false, if left { 0xFF } else { 0x00 });
                    Flow::Return(Ok(0))
                }
                0x07 => {
                    k.set_caller8(Saved::Ax, false, 0xFF);
                    Flow::Return(Ok(0))
                }
                _ => refuse(error::INVALID_FUNCTION),
            };
        };
        self.request(k, phase, device, 0)
    }

    /// A request to a drive's driver.
    fn drive_request(self, k: &mut Kernel<'_, '_>, phase: &mut u8) -> Flow {
        let (driver, unit) = match drive_driver(k) {
            Ok(found) => found,
            Err(code) => return refuse(code),
        };
        self.request(k, phase, driver, unit)
    }

    /// Function `69h`: a generic IOCTL to the drive BL names.
    fn generic_drive(k: &mut Kernel<'_, '_>, phase: &mut u8, function: u8) -> Flow {
        let (driver, unit) = match drive_driver(k) {
            Ok(found) => found,
            Err(code) => return refuse(code),
        };
        if k.attributes(driver) & attribute::GENERIC == 0 {
            return refuse(error::INVALID_FUNCTION);
        }
        k.request(unit, command::GENERIC_IOCTL);
        k.set_request8(field::CATEGORY, DISK);
        k.set_request8(field::FUNCTION, function);
        let packet = k.caller_far(Saved::Ds, Saved::Dx);
        k.set_request_far(field::PACKET, packet);
        *phase = CALLED;
        Flow::Call(Frame::of(&Device { header: driver }))
    }

    /// Builds the request subfunction `function` makes of `device`, if its
    /// attributes say it takes it, and calls it.
    fn request(self, k: &mut Kernel<'_, '_>, phase: &mut u8, device: Far, unit: u8) -> Flow {
        let attributes = k.attributes(device);
        let (needed, command) = match self.function {
            0x02 | 0x04 => (attribute::IOCTL, command::IOCTL_INPUT),
            0x03 | 0x05 => (attribute::IOCTL, command::IOCTL_OUTPUT),
            0x06 => (0, command::INPUT_STATUS),
            0x07 => (0, command::OUTPUT_STATUS),
            0x08 => (attribute::OPEN_CLOSE, command::REMOVABLE),
            0x0C | 0x0D => (attribute::GENERIC, command::GENERIC_IOCTL),
            0x0E => (attribute::GENERIC, command::GET_LOGICAL),
            0x0F => (attribute::GENERIC, command::SET_LOGICAL),
            _ => (attribute::QUERY, command::IOCTL_QUERY),
        };
        if attributes & needed != needed {
            return refuse(error::INVALID_FUNCTION);
        }
        k.request(unit, command);
        match command {
            command::IOCTL_INPUT | command::IOCTL_OUTPUT => {
                let buffer = k.caller_far(Saved::Ds, Saved::Dx);
                k.set_request_far(field::ADDRESS, buffer);
                let count = k.caller(Saved::Cx);
                k.set_request16(field::COUNT, count);
            }
            command::GENERIC_IOCTL | command::IOCTL_QUERY => {
                let [function, category] = k.caller(Saved::Cx).to_le_bytes();
                k.set_request8(field::CATEGORY, category);
                k.set_request8(field::FUNCTION, function);
                let si = k.caller(Saved::Si);
                let di = k.caller(Saved::Di);
                k.set_request16(field::FUNCTION + 1, si);
                k.set_request16(field::FUNCTION + 3, di);
                let packet = k.caller_far(Saved::Ds, Saved::Dx);
                k.set_request_far(field::PACKET, packet);
            }
            _ => {}
        }
        *phase = CALLED;
        Flow::Call(Frame::of(&Device { header: device }))
    }

    /// The driver answered with status `word`.
    fn answered(self, k: &mut Kernel<'_, '_>, word: u16) -> Flow {
        let busy = word & status::BUSY != 0;
        if word & status::ERROR != 0 {
            let refused = matches!(word & 0xFF, 0x03) && matches!(self.function, 0x10 | 0x11);
            return refuse(if refused {
                error::INVALID_FUNCTION
            } else {
                super::device::extended(word)
            });
        }
        if self.media {
            return Flow::Return(Ok(0));
        }
        match self.function {
            0x02..=0x05 => {
                let moved = k.request16(field::COUNT);
                k.set_caller(Saved::Ax, moved);
            }
            0x06 | 0x07 => k.set_caller8(Saved::Ax, false, if busy { 0x00 } else { 0xFF }),
            0x08 => k.set_caller(Saved::Ax, u16::from(busy)),
            0x0E | 0x0F => {
                let unit = k.request8(field::UNIT);
                k.set_caller8(Saved::Ax, false, unit);
            }
            _ => k.set_caller(Saved::Ax, 0),
        }
        Flow::Return(Ok(0))
    }
}
