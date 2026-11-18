//! The format's refusals as a TAIRiX driver reports them.

use tairix_abi::DriverError;

use crate::{Error, Fault, Structure};

#[test]
fn each_refusal_reaches_a_driver_as_the_error_it_has_always_meant() {
    for (fault, error) in [
        (Fault::DiscFull, DriverError::NoSpace),
        (Fault::Compaction, DriverError::NoSpace),
        (Fault::MapFull, DriverError::NoSpace),
        (Fault::DirectoryFull, DriverError::NoSpace),
        (Fault::TooLarge, DriverError::NoSpace),
        (Fault::NameTooLong, DriverError::LengthOutOfRange),
        (Fault::BadName, DriverError::OutOfRange),
        (Fault::Exists, DriverError::AlreadyExists),
        (Fault::NoFragment, DriverError::NotFound),
        (Fault::Broken(Structure::Map), DriverError::BadMagic),
        (Fault::Unsupported, DriverError::Unsupported),
        (Fault::NoMemory, DriverError::OutOfMemory),
    ] {
        assert_eq!(DriverError::from(fault), error, "{fault:?}");
        assert_eq!(
            DriverError::from(Error::<DriverError>::Fault(fault)),
            error,
            "{fault:?}"
        );
    }
    assert_eq!(
        DriverError::from(Error::Disc(DriverError::DeviceFault)),
        DriverError::DeviceFault,
        "a disc's own failure passes through untouched"
    );
}
