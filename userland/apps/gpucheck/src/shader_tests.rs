//! A kernel told by its first word and held to the one interface `gpucheck`
//! runs; an honest kernel, one reaching past every memory it can, and one
//! that never ends, each run on the software device; and each difference
//! between two answers found where it first is.

use alloc::string::{String, ToString};
use alloc::sync::Arc;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicU64, Ordering};

use tairix_abi::time::MonotonicClock;
use tairix_gpu::{Device, Error, Loss};
use tairix_spirv::{Admitted, Module, Version};
use tairix_wgsl::Options;

use super::{
    compare_kernel, marked_at, run_kernel, Answered, Kernel, KernelDifference, KernelError,
    Outcome, Source,
};
use crate::reference;
use tairix_test_shadercheck::{ENDLESS, HONEST, PAST};

/// A clock that never moves: no deadline passes.
struct Still;

impl MonotonicClock for Still {
    fn now_ns(&self) -> u64 {
        0
    }
}

/// A clock a tenth of a second on at each reading: work that never ends
/// meets its deadline after a bounded count of readings.
#[derive(Default)]
struct Stepping(AtomicU64);

impl MonotonicClock for Stepping {
    fn now_ns(&self) -> u64 {
        self.0.fetch_add(100_000_000, Ordering::SeqCst)
    }
}

const MARKER: u32 = 0x8bad_f00d;

fn wgsl(source: &str) -> Admitted {
    let compiled = tairix_wgsl::compile(source, &Options::default()).expect("compiled");
    Admitted::new(Module::decode(&compiled.module).expect("decoded")).expect("admitted")
}

fn kernel(source: &str) -> Kernel {
    Kernel::new(wgsl(source)).expect("a kernel")
}

fn device(clock: Arc<dyn MonotonicClock + Send + Sync>) -> Device {
    reference(clock).expect("a software device")
}

fn run(kernel: &Kernel, words: u32) -> Outcome {
    run_kernel(&device(Arc::new(Still)), kernel, (words, MARKER)).expect("run")
}

#[test]
fn a_module_is_told_by_its_magic_word_in_either_order_and_source_by_being_text() {
    let module = wgsl(HONEST).module().bytes().to_vec();
    assert_eq!(Source::of(&module), Some(Source::Spirv(&module)));
    let swapped: Vec<u8> = module
        .chunks(4)
        .flat_map(|word| word.iter().rev().copied())
        .collect();
    assert_eq!(Source::of(&swapped), Some(Source::Spirv(&swapped)));
    assert_eq!(Source::of(HONEST.as_bytes()), Some(Source::Wgsl(HONEST)));
    assert_eq!(Source::of(&[0xff, 0xfe, 0x00, 0x41]), None, "neither");
    assert_eq!(
        Source::of(b""),
        Some(Source::Wgsl("")),
        "empty text, refused by the compile"
    );
}

#[test]
fn a_kernel_binds_one_storage_buffer_at_set_0_binding_0_through_one_compute_entry_point() {
    let refused = |source: &str| Kernel::new(wgsl(source)).err();
    assert_eq!(
        refused("@fragment fn main() -> @location(0) vec4f { return vec4f(1.0); }"),
        Some(KernelError::EntryPoints(0))
    );
    let two = "
        @group(0) @binding(0) var<storage, read_write> data: array<u32>;
        @compute @workgroup_size(1) fn a() { data[0] = 1u; }
        @compute @workgroup_size(1) fn b() { data[0] = 2u; }
    ";
    assert_eq!(refused(two), Some(KernelError::EntryPoints(2)));
    let uniform = "
        @group(0) @binding(0) var<storage, read_write> data: array<u32>;
        @group(0) @binding(1) var<uniform> scale: u32;
        @compute @workgroup_size(1) fn main() { data[0] = scale; }
    ";
    assert_eq!(refused(uniform), Some(KernelError::Interface));
    let elsewhere = "
        @group(1) @binding(0) var<storage, read_write> data: array<u32>;
        @compute @workgroup_size(1) fn main() { data[0] = 1u; }
    ";
    assert_eq!(refused(elsewhere), Some(KernelError::Interface));
}

#[test]
fn a_spirv_kernel_is_decoded_and_admitted_here() {
    let module = wgsl(HONEST).module().bytes().to_vec();
    let decoded = Kernel::decode(&module).expect("a kernel");
    assert_eq!(decoded.groups(256), 4);
    assert!(matches!(
        Kernel::decode(&module[..module.len() - 2]),
        Err(KernelError::Decoded(_))
    ));
    let refused = tairix_spirv::assemble::assemble(
        "OpCapability Shader\nOpCapability Int8\nOpMemoryModel Logical GLSL450\n",
        Version::NEWEST,
    );
    assert!(matches!(
        Kernel::decode(&refused),
        Err(KernelError::Refused(_))
    ));
}

#[test]
fn a_buffer_takes_one_invocation_a_word() {
    let honest = kernel(HONEST);
    assert_eq!(honest.groups(1), 1);
    assert_eq!(honest.groups(64), 1);
    assert_eq!(honest.groups(65), 2);
    assert_eq!(honest.groups(256), 4);
    assert_eq!(kernel(ENDLESS).groups(3), 3);
}

#[test]
fn an_honest_kernel_leaves_what_it_computes_and_agrees_with_itself() {
    let honest = kernel(HONEST);
    let Outcome::Ran(left) = run(&honest, 100) else {
        panic!("ran");
    };
    let expected: Vec<u32> = (0..100).map(|i| i * 3 + 1).collect();
    assert_eq!(left, expected);
    let again = run(&honest, 100);
    assert_eq!(
        compare_kernel(&again, &Outcome::Ran(expected), MARKER),
        Ok(())
    );
}

#[test]
fn a_kernel_reaching_past_every_memory_reads_zeros_and_holds_no_marker() {
    let Outcome::Ran(left) = run(&kernel(PAST), 128) else {
        panic!("ran");
    };
    let expected: Vec<u32> = (0..128).map(|i| 2 * i).collect();
    assert_eq!(left, expected, "every read past what it reaches is zero");
    assert_eq!(marked_at(&left, MARKER), None);
}

#[test]
fn a_kernel_that_never_ends_loses_its_context_and_the_device_serves_on() {
    let endless = kernel(ENDLESS);
    let clock: Arc<dyn MonotonicClock + Send + Sync> = Arc::new(Stepping::default());
    let outcome = run_kernel(&device(clock), &endless, (4, MARKER)).expect("run");
    assert_eq!(
        outcome,
        Outcome::Lost {
            loss: Loss::Overran,
            serves: true
        }
    );
    assert_eq!(compare_kernel(&outcome, &outcome, MARKER), Ok(()));
}

#[test]
fn each_difference_is_found_where_it_first_is() {
    let ran = |words: &[u32]| Outcome::Ran(words.to_vec());
    let reference = ran(&[1, 2, 3, 4]);
    assert_eq!(
        compare_kernel(&ran(&[1, MARKER, 9, 4]), &reference, MARKER),
        Err(KernelDifference::Neighbour { at: 1 }),
        "the marker before the first difference"
    );
    assert_eq!(
        compare_kernel(&ran(&[1, 2, 9, MARKER]), &reference, MARKER),
        Err(KernelDifference::Neighbour { at: 3 }),
        "the marker however late"
    );
    let marked = ran(&[MARKER, 2, 3, 4]);
    assert_eq!(
        compare_kernel(&marked, &marked, MARKER),
        Ok(()),
        "a word both left"
    );
    assert_eq!(
        compare_kernel(&ran(&[1, 2, 9, 4]), &reference, MARKER),
        Err(KernelDifference::Word {
            at: 2,
            device: 9,
            reference: 3
        })
    );
    let lost = |loss, serves| Outcome::Lost { loss, serves };
    assert_eq!(
        compare_kernel(&lost(Loss::Overran, true), &reference, MARKER),
        Err(KernelDifference::Answered {
            device: Answered::Lost(Loss::Overran),
            reference: Answered::Ran
        })
    );
    assert_eq!(
        compare_kernel(&lost(Loss::Fault, true), &lost(Loss::Overran, true), MARKER),
        Err(KernelDifference::Answered {
            device: Answered::Lost(Loss::Fault),
            reference: Answered::Lost(Loss::Overran)
        })
    );
    assert_eq!(
        compare_kernel(
            &lost(Loss::Overran, false),
            &lost(Loss::Overran, true),
            MARKER
        ),
        Err(KernelDifference::Stopped)
    );
    assert_eq!(
        compare_kernel(&Outcome::Refused(Error::Unsupported), &reference, MARKER),
        Err(KernelDifference::Answered {
            device: Answered::Refused(Error::Unsupported),
            reference: Answered::Ran
        })
    );
    assert_eq!(marked_at(&[1, MARKER, MARKER], MARKER), Some(1));
    assert_eq!(marked_at(&[1, 2], MARKER), None);
}

#[test]
fn a_kernel_never_answered_differs_from_one_lost_and_says_so() {
    let lost = Outcome::Lost {
        loss: Loss::Overran,
        serves: true,
    };
    let difference = compare_kernel(&Outcome::Unanswered, &lost, MARKER);
    assert_eq!(
        difference,
        Err(KernelDifference::Answered {
            device: Answered::Unanswered,
            reference: Answered::Lost(Loss::Overran)
        })
    );
    assert_eq!(
        difference.map_err(|found| found.to_string()),
        Err(String::from(
            "the kernel was never answered where on the software device it lost: it ran past its deadline"
        ))
    );
}
