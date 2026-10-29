//! What a worker's facts mean for a caller: each error a connection or a
//! request met, as the `netshare-v1` refusal it answers.
//!
//! The worker reports what the server did; the front decides what that is,
//! so a compromised worker can choose among refusals but never widen what a
//! caller is given.

use tairix_abi::netshare_ipc::{NetshareError, Rejection, Shortfall};
use tairix_smb::status::NtStatus;
use tairix_smb::{Refusal, SmbError};

/// The refusal a connection's failure answers every call it held.
#[must_use]
pub fn connection(error: SmbError) -> NetshareError {
    match error {
        SmbError::Refused(refusal) => refused(refusal),
        SmbError::Integrity => NetshareError::Tampered,
        SmbError::TooLarge => NetshareError::TooLarge,
        SmbError::OutOfMemory => NetshareError::Failed,
        SmbError::Malformed
        | SmbError::Protocol
        | SmbError::NoCredit
        | SmbError::TimeOutOfRange
        | SmbError::Name
        | SmbError::Status(_)
        | SmbError::Rpc(_) => NetshareError::Protocol,
    }
}

const fn refused(refusal: Refusal) -> NetshareError {
    match refusal {
        Refusal::Dialect => NetshareError::Unsupported(Shortfall::Dialect),
        Refusal::Encryption => NetshareError::Unsupported(Shortfall::Encryption),
        Refusal::Signing => NetshareError::Unsupported(Shortfall::Signing),
        Refusal::Guest => NetshareError::Unsupported(Shortfall::Guest),
        Refusal::Mechanism | Refusal::Ntlm => NetshareError::Unsupported(Shortfall::Authentication),
        Refusal::Integrity | Refusal::Downgrade | Refusal::Validation => NetshareError::Tampered,
    }
}

/// The rejection a server's refusal of the credentials answers. A status
/// naming none of the reasons is the credentials': servers say no more.
#[must_use]
pub fn rejection(status: NtStatus) -> Rejection {
    match status {
        NtStatus::ACCOUNT_DISABLED => Rejection::Disabled,
        NtStatus::ACCOUNT_LOCKED_OUT => Rejection::Locked,
        NtStatus::PASSWORD_EXPIRED | NtStatus::PASSWORD_MUST_CHANGE => Rejection::Expired,
        NtStatus::ACCOUNT_RESTRICTION
        | NtStatus::INVALID_LOGON_HOURS
        | NtStatus::INVALID_WORKSTATION
        | NtStatus::ACCOUNT_EXPIRED => Rejection::Restricted,
        _ => Rejection::Credentials,
    }
}

/// The refusal a failed tree connect answers.
#[must_use]
pub fn tree(error: SmbError) -> NetshareError {
    match error {
        SmbError::Status(NtStatus::BAD_NETWORK_NAME | NtStatus::NETWORK_NAME_DELETED) => {
            NetshareError::NoSuchShare
        }
        SmbError::Status(_) => NetshareError::Denied,
        SmbError::TooLarge => NetshareError::Limit,
        error => connection(error),
    }
}

/// The refusal a failed listing answers.
#[must_use]
pub fn listing(error: SmbError) -> NetshareError {
    // `ERROR_ACCESS_DENIED`, as `NetrShareEnum` returns it.
    const RPC_ACCESS_DENIED: u32 = 5;
    match error {
        SmbError::Status(_) | SmbError::Rpc(RPC_ACCESS_DENIED) => NetshareError::Denied,
        error => connection(error),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_server_s_shortfall_is_named_and_tampering_is_called_tampering() {
        for (error, answer) in [
            (
                SmbError::Refused(Refusal::Dialect),
                NetshareError::Unsupported(Shortfall::Dialect),
            ),
            (
                SmbError::Refused(Refusal::Encryption),
                NetshareError::Unsupported(Shortfall::Encryption),
            ),
            (
                SmbError::Refused(Refusal::Signing),
                NetshareError::Unsupported(Shortfall::Signing),
            ),
            (
                SmbError::Refused(Refusal::Guest),
                NetshareError::Unsupported(Shortfall::Guest),
            ),
            (
                SmbError::Refused(Refusal::Mechanism),
                NetshareError::Unsupported(Shortfall::Authentication),
            ),
            (
                SmbError::Refused(Refusal::Ntlm),
                NetshareError::Unsupported(Shortfall::Authentication),
            ),
            (
                SmbError::Refused(Refusal::Downgrade),
                NetshareError::Tampered,
            ),
            (
                SmbError::Refused(Refusal::Validation),
                NetshareError::Tampered,
            ),
            (
                SmbError::Refused(Refusal::Integrity),
                NetshareError::Tampered,
            ),
            (SmbError::Integrity, NetshareError::Tampered),
            (SmbError::TooLarge, NetshareError::TooLarge),
            (SmbError::OutOfMemory, NetshareError::Failed),
            (SmbError::Malformed, NetshareError::Protocol),
            (SmbError::NoCredit, NetshareError::Protocol),
            (
                SmbError::Status(NtStatus::ACCESS_DENIED),
                NetshareError::Protocol,
            ),
        ] {
            assert_eq!(connection(error), answer, "{error:?}");
        }
    }

    #[test]
    fn each_rejection_status_is_its_reason_and_any_other_the_credentials() {
        for (status, reason) in [
            (NtStatus::LOGON_FAILURE, Rejection::Credentials),
            (NtStatus::WRONG_PASSWORD, Rejection::Credentials),
            (NtStatus::NO_SUCH_USER, Rejection::Credentials),
            (NtStatus::ACCOUNT_DISABLED, Rejection::Disabled),
            (NtStatus::ACCOUNT_LOCKED_OUT, Rejection::Locked),
            (NtStatus::PASSWORD_EXPIRED, Rejection::Expired),
            (NtStatus::PASSWORD_MUST_CHANGE, Rejection::Expired),
            (NtStatus::ACCOUNT_RESTRICTION, Rejection::Restricted),
            (NtStatus::INVALID_LOGON_HOURS, Rejection::Restricted),
            (NtStatus::INVALID_WORKSTATION, Rejection::Restricted),
            (NtStatus::ACCOUNT_EXPIRED, Rejection::Restricted),
            (NtStatus::from_code(0xc000_dead), Rejection::Credentials),
        ] {
            assert_eq!(rejection(status), reason, "{status:?}");
        }
    }

    #[test]
    fn a_tree_and_a_listing_answer_the_server_s_refusal() {
        assert_eq!(
            tree(SmbError::Status(NtStatus::BAD_NETWORK_NAME)),
            NetshareError::NoSuchShare
        );
        assert_eq!(
            tree(SmbError::Status(NtStatus::NETWORK_NAME_DELETED)),
            NetshareError::NoSuchShare
        );
        assert_eq!(
            tree(SmbError::Status(NtStatus::ACCESS_DENIED)),
            NetshareError::Denied
        );
        assert_eq!(tree(SmbError::TooLarge), NetshareError::Limit);
        assert_eq!(tree(SmbError::Protocol), NetshareError::Protocol);
        assert_eq!(listing(SmbError::Rpc(5)), NetshareError::Denied);
        assert_eq!(
            listing(SmbError::Status(NtStatus::ACCESS_DENIED)),
            NetshareError::Denied
        );
        assert_eq!(listing(SmbError::Rpc(0x1c01_0002)), NetshareError::Protocol);
        assert_eq!(listing(SmbError::TooLarge), NetshareError::TooLarge);
    }
}
