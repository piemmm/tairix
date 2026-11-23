//! The work held to two software devices, which agree, and to a device
//! whose copies corrupt a byte, whose difference is found.

use alloc::sync::Arc;
use alloc::vec;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;

use tairix_abi::time::MonotonicClock;
use tairix_gpu::{
    Commands, Device, DeviceInfo, Error, Features, FormatFeatures, FormatTable, Hardware, Job,
    JobCommand, MemoryProperties, QueueCapabilities,
};

use super::{compare, reference, work, Answer, Difference};

struct Still;

impl MonotonicClock for Still {
    fn now_ns(&self) -> u64 {
        0
    }
}

fn software() -> Device {
    reference(Arc::new(Still)).expect("a software device")
}

fn expected() -> Answer {
    work(&software()).expect("the reference ran")
}

/// Memory of the test's own device.
#[derive(Debug)]
struct Bytes(Mutex<Vec<u8>>);

/// A device running transfers on the CPU, its copies flipping the first
/// byte they write where `corrupt` says.
struct Flawed {
    corrupt: AtomicBool,
}

impl Flawed {
    fn index(at: u64) -> usize {
        usize::try_from(at).expect("small")
    }
}

impl Hardware for Flawed {
    type Context = ();
    type Memory = Bytes;

    fn create_context(&self) -> Result<(), Error> {
        Ok(())
    }

    fn allocate(&self, _: &Arc<()>, _: u32, size: u64) -> Result<Bytes, Error> {
        Ok(Bytes(Mutex::new(vec![0; Self::index(size)])))
    }

    fn write(&self, memory: &Bytes, offset: u64, data: &[u8]) -> Result<(), Error> {
        let at = Self::index(offset);
        memory.0.lock().expect("unpoisoned")[at..at + data.len()].copy_from_slice(data);
        Ok(())
    }

    fn read(&self, memory: &Bytes, offset: u64, out: &mut [u8]) -> Result<(), Error> {
        let at = Self::index(offset);
        out.copy_from_slice(&memory.0.lock().expect("unpoisoned")[at..at + out.len()]);
        Ok(())
    }

    fn run(&self, job: Job<Self>) {
        for command in job.commands().iter() {
            match command {
                JobCommand::Copy { src, dst, spans } => {
                    for span in spans {
                        let mut bytes = vec![0; Self::index(span.len)];
                        let _ = self.read(src, span.src, &mut bytes);
                        if self.corrupt.load(Ordering::SeqCst) {
                            bytes[0] ^= 1;
                        }
                        let _ = self.write(dst, span.dst, &bytes);
                    }
                }
                JobCommand::Fill {
                    dst,
                    offset,
                    size,
                    word,
                } => {
                    let bytes: Vec<u8> = (0..size / 4).flat_map(|_| word.to_le_bytes()).collect();
                    let _ = self.write(dst, offset, &bytes);
                }
                JobCommand::Update { dst, offset, data } => {
                    let _ = self.write(dst, offset, data);
                }
            }
        }
        job.finish(Ok(()));
    }

    fn run_bytes(_: Commands<'_, Self>) -> u64 {
        0
    }

    fn lost(&self, _: &Arc<()>) {}
}

/// The test's device: the software device's description narrowed to
/// transfers, its one memory type host-visible.
fn flawed(corrupt: bool) -> Device {
    let reference = software();
    let mut info: DeviceInfo = reference.info().clone();
    info.capabilities.clear();
    info.features = Features::NONE;
    info.formats = FormatTable::from_fn(|_| FormatFeatures::NONE);
    for family in &mut info.queue_families {
        family.capabilities = QueueCapabilities::TRANSFER;
    }
    assert!(info.memory_types[0]
        .properties
        .contains(MemoryProperties::HOST_VISIBLE));
    Device::hardware(
        Flawed {
            corrupt: AtomicBool::new(corrupt),
        },
        info,
        &[1],
        Arc::new(Still),
    )
    .expect("a device")
}

#[test]
fn the_work_leaves_its_pattern_its_fill_and_both_copied_back() {
    let answer = work(&software()).expect("ran");
    assert_eq!(&answer.bytes[..4096], super::pattern(4096).as_slice());
    assert_eq!(&answer.bytes[4096..4100], &0x5a5a_a5a5_u32.to_le_bytes());
    // Copied out at 400 and back from 0, so the copy begins 400 bytes of
    // zeroes before the pattern.
    let back = 16 << 10;
    assert!(answer.bytes[back..back + 400].iter().all(|&byte| byte == 0));
    assert_eq!(
        &answer.bytes[back + 400..back + 400 + 4096],
        super::pattern(4096).as_slice()
    );
    assert!(
        answer.refused.is_some(),
        "a fill past its buffer is refused"
    );
}

#[test]
fn two_devices_running_the_work_alike_agree() {
    assert_eq!(compare(&software(), &expected()), Ok(()));
    assert_eq!(compare(&flawed(false), &expected()), Ok(()));
}

#[test]
fn a_device_corrupting_a_copy_is_found_at_the_first_byte_it_got_wrong() {
    let difference = compare(&flawed(true), &expected()).expect_err("found");
    let Difference::Byte {
        at,
        device,
        reference,
    } = difference
    else {
        panic!("a byte, not {difference:?}");
    };
    assert_eq!(device ^ reference, 1);
    assert_eq!(at, 16 << 10, "the first byte the copy back wrote");
}

#[test]
fn a_difference_reads_as_what_each_device_did() {
    let refused = work(&software()).expect("ran").refused;
    let text = alloc::format!(
        "{}",
        Difference::Refusal {
            device: None,
            reference: refused,
        }
    );
    assert!(
        text.starts_with(
            "a fill past its buffer was run where the software device's was refused ("
        ),
        "{text}"
    );
    let byte = Difference::Byte {
        at: 7,
        device: 1,
        reference: 0x10,
    };
    assert_eq!(
        alloc::format!("{byte}"),
        "byte 7 reads 0x01 where the software device left 0x10"
    );
}

/// Every locale's document names the switches the parser accepts: the
/// tokens are language-neutral, so each translation carries them as the
/// canonical one does. Read from the bundle's own on-disk `Help/` tree.
#[test]
fn help_documents_the_parser_switches() {
    let help_root = alloc::format!("{}/Help", env!("CARGO_MANIFEST_DIR"));
    for locale in tairix_help::REQUIRED_LOCALES {
        let path = alloc::format!("{help_root}/{locale}/gpucheck.md");
        let text =
            std::fs::read_to_string(&path).unwrap_or_else(|err| panic!("read {path}: {err}"));
        assert!(
            text.contains("`-h, -?`"),
            "{locale}/gpucheck.md must document `-h, -?`"
        );
    }
}
