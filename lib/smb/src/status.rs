//! NT status codes (MS-ERREF §2.3): what a server answers a request with.

/// An NT status.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
pub struct NtStatus(u32);

impl NtStatus {
    /// The request succeeded.
    pub const SUCCESS: Self = Self(0x0000_0000);
    /// An interim response: the final one follows asynchronously.
    pub const PENDING: Self = Self(0x0000_0103);
    /// A change notification overflowed: re-read the whole directory.
    pub const NOTIFY_ENUM_DIR: Self = Self(0x0000_010c);
    /// Success, with more data than the response held.
    pub const BUFFER_OVERFLOW: Self = Self(0x8000_0005);
    /// A directory enumeration has no more entries.
    pub const NO_MORE_FILES: Self = Self(0x8000_0006);
    /// Authentication needs another round.
    pub const MORE_PROCESSING_REQUIRED: Self = Self(0xc000_0016);
    /// The request is not implemented.
    pub const NOT_IMPLEMENTED: Self = Self(0xc000_0002);
    /// The information class is not one the server knows.
    pub const INVALID_INFO_CLASS: Self = Self(0xc000_0003);
    /// The handle is not open.
    pub const INVALID_HANDLE: Self = Self(0xc000_0008);
    /// A parameter is invalid.
    pub const INVALID_PARAMETER: Self = Self(0xc000_000d);
    /// No such file.
    pub const NO_SUCH_FILE: Self = Self(0xc000_000f);
    /// The device does not perform this request.
    pub const INVALID_DEVICE_REQUEST: Self = Self(0xc000_0010);
    /// A read started at or past the end of the file.
    pub const END_OF_FILE: Self = Self(0xc000_0011);
    /// Access is denied.
    pub const ACCESS_DENIED: Self = Self(0xc000_0022);
    /// The object is of another type than the request needs.
    pub const OBJECT_TYPE_MISMATCH: Self = Self(0xc000_0024);
    /// The name is not valid on the server.
    pub const OBJECT_NAME_INVALID: Self = Self(0xc000_0033);
    /// The named object does not exist.
    pub const OBJECT_NAME_NOT_FOUND: Self = Self(0xc000_0034);
    /// The named object already exists.
    pub const OBJECT_NAME_COLLISION: Self = Self(0xc000_0035);
    /// A directory in the path does not exist.
    pub const OBJECT_PATH_NOT_FOUND: Self = Self(0xc000_003a);
    /// The path's syntax is bad.
    pub const OBJECT_PATH_SYNTAX_BAD: Self = Self(0xc000_003b);
    /// Another open's sharing mode excludes this one.
    pub const SHARING_VIOLATION: Self = Self(0xc000_0043);
    /// A byte-range lock conflicts.
    pub const FILE_LOCK_CONFLICT: Self = Self(0xc000_0054);
    /// The file is being deleted.
    pub const DELETE_PENDING: Self = Self(0xc000_0056);
    /// No such user.
    pub const NO_SUCH_USER: Self = Self(0xc000_0064);
    /// The password is wrong.
    pub const WRONG_PASSWORD: Self = Self(0xc000_006a);
    /// The user name or password is wrong.
    pub const LOGON_FAILURE: Self = Self(0xc000_006d);
    /// An account restriction refused the logon.
    pub const ACCOUNT_RESTRICTION: Self = Self(0xc000_006e);
    /// The password has expired.
    pub const PASSWORD_EXPIRED: Self = Self(0xc000_0071);
    /// The account is disabled.
    pub const ACCOUNT_DISABLED: Self = Self(0xc000_0072);
    /// The disk is full.
    pub const DISK_FULL: Self = Self(0xc000_007f);
    /// The server lacks the resources to complete the request.
    pub const INSUFFICIENT_RESOURCES: Self = Self(0xc000_009a);
    /// The medium is write-protected.
    pub const MEDIA_WRITE_PROTECTED: Self = Self(0xc000_00a2);
    /// The request timed out on the server.
    pub const IO_TIMEOUT: Self = Self(0xc000_00b5);
    /// A directory was named where a file is needed.
    pub const FILE_IS_A_DIRECTORY: Self = Self(0xc000_00ba);
    /// The request is not supported.
    pub const NOT_SUPPORTED: Self = Self(0xc000_00bb);
    /// The share was deleted while connected.
    pub const NETWORK_NAME_DELETED: Self = Self(0xc000_00c9);
    /// No share by that name.
    pub const BAD_NETWORK_NAME: Self = Self(0xc000_00cc);
    /// A rename crosses devices.
    pub const NOT_SAME_DEVICE: Self = Self(0xc000_00d4);
    /// The directory is not empty.
    pub const DIRECTORY_NOT_EMPTY: Self = Self(0xc000_0101);
    /// A file was named where a directory is needed.
    pub const NOT_A_DIRECTORY: Self = Self(0xc000_0103);
    /// The name is too long.
    pub const NAME_TOO_LONG: Self = Self(0xc000_0106);
    /// The request was cancelled.
    pub const CANCELLED: Self = Self(0xc000_0120);
    /// The file cannot be deleted.
    pub const CANNOT_DELETE: Self = Self(0xc000_0121);
    /// The handle was closed.
    pub const FILE_CLOSED: Self = Self(0xc000_0128);
    /// The client's clock is too far from the domain controller's.
    pub const TIME_DIFFERENCE_AT_DC: Self = Self(0xc000_0133);
    /// The session was deleted on the server.
    pub const USER_SESSION_DELETED: Self = Self(0xc000_0203);
    /// The server lacks the resources for another session.
    pub const INSUFF_SERVER_RESOURCES: Self = Self(0xc000_0205);
    /// The object was not found.
    pub const NOT_FOUND: Self = Self(0xc000_0225);
    /// The path is served by another server: a DFS referral is needed.
    pub const PATH_NOT_COVERED: Self = Self(0xc000_0257);
    /// The session's credentials have expired.
    pub const NETWORK_SESSION_EXPIRED: Self = Self(0xc000_035c);

    /// The status of raw code `code`.
    #[must_use]
    pub const fn from_code(code: u32) -> Self {
        Self(code)
    }

    /// The raw code.
    #[must_use]
    pub const fn code(self) -> u32 {
        self.0
    }

    /// Whether the severity is an error (`0b11`), as opposed to success,
    /// information or a warning.
    #[must_use]
    pub const fn is_error(self) -> bool {
        self.0 >> 30 == 0b11
    }
}
