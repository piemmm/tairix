//! The service calls' numbers, and the reasons of those that take one, for
//! the kernel and every module that issues or takes them.

/// `Service_UKCommand`.
pub const UK_COMMAND: u32 = 0x04;
/// `Service_UKByte`.
pub const UK_BYTE: u32 = 0x07;
/// `Service_UKWord`.
pub const UK_WORD: u32 = 0x08;
/// `Service_Help`.
pub const HELP: u32 = 0x09;
/// `Service_ReleaseFIQ`.
pub const RELEASE_FIQ: u32 = 0x0B;
/// `Service_ClaimFIQ`.
pub const CLAIM_FIQ: u32 = 0x0C;
/// `Service_Reset`.
pub const RESET: u32 = 0x27;
/// `Service_UKConfig`.
pub const UK_CONFIG: u32 = 0x28;
/// `Service_UKStatus`.
pub const UK_STATUS: u32 = 0x29;
/// `Service_NewApplication`.
pub const NEW_APPLICATION: u32 = 0x2A;
/// `Service_FSRedeclare`: every filing system asked to register again, as
/// FileSwitch does at its start.
pub const FS_REDECLARE: u32 = 0x40;
/// `Service_LookupFileType`, offered a type no variable names.
pub const LOOKUP_FILE_TYPE: u32 = 0x42;
/// `Service_International`, by its reasons in [`international`].
pub const INTERNATIONAL: u32 = 0x43;
/// `Service_PreReset`.
pub const PRE_RESET: u32 = 0x45;
/// `Service_ModeChange`.
pub const MODE_CHANGE: u32 = 0x46;
/// `Service_ClaimFIQinBackground`.
pub const CLAIM_FIQ_IN_BACKGROUND: u32 = 0x47;
/// `Service_PreModeChange`.
pub const PRE_MODE_CHANGE: u32 = 0x4D;
/// `Service_ModeExtension`.
pub const MODE_EXTENSION: u32 = 0x50;
/// `Service_ModeTranslation`.
pub const MODE_TRANSLATION: u32 = 0x51;
/// `Service_ResourceFSStarted`.
pub const RESOURCE_FS_STARTED: u32 = 0x59;
/// `Service_ResourceFSDying`.
pub const RESOURCE_FS_DYING: u32 = 0x5A;
/// `Service_MessageFileClosed`.
pub const MESSAGE_FILE_CLOSED: u32 = 0x5E;
/// `Service_ResourceFSStarting`: R2 the registration to call, R3 its R3.
pub const RESOURCE_FS_STARTING: u32 = 0x60;
/// `Service_TerritoryManagerLoaded`: the Manager is there to register with.
pub const TERRITORY_MANAGER_LOADED: u32 = 0x64;
/// `Service_ValidateAddress`: a range `OS_ValidateAddress` finds unmapped,
/// offered to the modules that map their own.
pub const VALIDATE_ADDRESS: u32 = 0x6D;
/// `Service_CloseFile`: an object and everything beneath it to be closed.
pub const CLOSE_FILE: u32 = 0x68;
/// `Service_SwitchingOutputToSprite`: output switched to or from a sprite,
/// R2 to R5 the call's R0 to R3.
pub const SWITCHING_OUTPUT_TO_SPRITE: u32 = 0x72;
/// `Service_PostInit`, once every ROM module has started.
pub const POST_INIT: u32 = 0x73;
/// `Service_TerritoryStarted`: the upper-case table may have changed.
pub const TERRITORY_STARTED: u32 = 0x75;
/// `Service_MonitorLeadTranslation`: the display a monitor lead describes.
pub const MONITOR_LEAD_TRANSLATION: u32 = 0x76;
/// `Service_ModeChanging`.
pub const MODE_CHANGING: u32 = 0x89;
/// `Service_SyntaxError`.
pub const SYNTAX_ERROR: u32 = 0x8C;
/// `Service_EnumerateScreenModes`.
pub const ENUMERATE_SCREEN_MODES: u32 = 0x8D;
/// `Service_ModeFileChanged`.
pub const MODE_FILE_CHANGED: u32 = 0x94;
/// `Service_Hardware`.
pub const HARDWARE: u32 = 0xD9;
/// `Service_RTCSynchronised`: the real time taken from the clock again.
pub const RTC_SYNCHRONISED: u32 = 0xDD;
/// `Service_TimeZoneChanged`.
pub const TIME_ZONE_CHANGED: u32 = 0x8_1080;

/// [`INTERNATIONAL`]'s reasons, in R2.
pub mod international {
    /// The number of the country named at R3, in R4.
    pub const COUNTRY_NUMBER: u32 = 0;
    /// The number of the alphabet named at R3, in R4.
    pub const ALPHABET_NUMBER: u32 = 1;
    /// Country R3's name.
    pub const COUNTRY_NAME: u32 = 2;
    /// Alphabet R3's name.
    pub const ALPHABET_NAME: u32 = 3;
    /// Country R3's alphabet, in R4.
    pub const COUNTRY_ALPHABET: u32 = 4;
    /// Characters R4 to R5 of alphabet R3 defined.
    pub const DEFINE: u32 = 5;
    /// Keyboard R3, of alphabet R4, selected.
    pub const KEYBOARD: u32 = 6;
    /// Character R3 defined as code point R4's glyph.
    pub const DEFINE_UCS: u32 = 7;
    /// Alphabet R3's Unicode table, in R4.
    pub const UCS_TABLE: u32 = 8;
    /// Country R3's ISO 3166 code.
    pub const ISO_3166: u32 = 9;
}
