//! What `FDISK` says, as DOS 4.0's says it: its screens' messages in the
//! form its display engine draws them (see [`super::screen`]), and the few
//! it writes to the standard error. The restart asks for a DOS diskette,
//! TAIRiX DOS having no installation diskette.

pub const OPTIONS: &str = "^0433^<H>FDISK Options ^0604^<H>1. <R>Create DOS Partition or Logical DOS Drive ^0104^<H>2. <R>Set active partition ^0104^<H>3. <R>Delete DOS Partition or Logical DOS Drive ^0104^<H>4. <R>Display partition information ^1104^<R>Press <H>Esc<R> to exit FDISK";
pub const CHOOSE: &str = "^0804^<R>Choose one of the following:";
pub const NEXT_DISK: &str = "^1404^<H>5. <R>Select next fixed disk drive";
pub const CURRENT_DISK: &str = "^0604^<R>Current fixed disk drive: <H><I>";
pub const NOTHING_ACTIVE: &str = "^2004^<H>WARNING! <R>No partitions are set active - disk 1 is not startable unless ^0104^<R>a partition is set active";
pub const ENTER_CHOICE: &str = "^1704^<H>Enter choice: [<S> ]";
pub const CREATE_TITLE: &str = "^0420^<H>Create DOS Partition or Logical DOS Drive";
pub const CREATE_PARTITIONS: &str =
    "^1004^<H>1. <R>Create Primary DOS Partition ^0104^<H>2. <R>Create Extended DOS Partition";
pub const CREATE_LOGICAL_OPTION: &str =
    "^1204^<H>3. <R>Create Logical DOS Drive(s) in the Extended DOS Partition";
pub const RETURN: &str = "^2404^<R>Press <H>Esc<R> to return to FDISK Options";
pub const PRIMARY_TITLE: &str = "^0427^<H>Create Primary DOS Partition";
pub const MAXIMUM_ACTIVE: &str = "^0804^<R>Do you wish to use the maximum available size for a Primary DOS Partition ^0104^<R>and make the partition active (<Y>/<N>).....................? <H>[<S> ]";
pub const MAXIMUM: &str = "^0804^<R>Do you wish to use the maximum available size for a Primary DOS Partition ^0104^<R>(<Y>/<N>)...................................................? <H>[<S> ]";
pub const TABLE: &str = "^0804^<R>Partition Status   Type    Size in Mbytes   Percentage of Disk Used ^0104^<R> <II> <I>        <I>    <IIIIIII>       <IIII>        <IIII> ^0104^<R> <II> <I>        <I>    <IIIIIII>       <IIII>        <IIII> ^0104^<R> <II> <I>        <I>    <IIIIIII>       <IIII>        <IIII> ^0104^<R> <II> <I>        <I>    <IIIIIII>       <IIII>        <IIII>";
pub const DISK_SPACE: &str =
    "^1404^<R>Total disk space is <HIIIIR> Mbytes (1 Mbyte = 1048576 bytes)";
pub const MAXIMUM_SPACE: &str =
    "^1504^<RC>Maximum space available for partition is <HIIIIR> Mbytes (<HIIIIR>)";
pub const PRIMARY_SIZE: &str = "^1804^<RC>Enter partition size in Mbytes or percent of disk space (%) to ^0104^<RC>create a Primary DOS Partition.................................: <H>[<IIISI>]";
pub const EXTENDED_TITLE: &str = "^0427^<H>Create Extended DOS Partition";
pub const EXTENDED_SIZE: &str = "^1804^<RC>Enter partition size in Mbytes or percent of disk space (%) to ^0104^<RC>create an Extended DOS Partition..............................: <H>[<IIISI>]";
pub const CONTINUE: &str = "^2404^<R>Press <H>Esc<R> to continue";
pub const LOGICAL_TITLE: &str =
    "^0112^<HC>Create Logical DOS Drive(s) in the Extended DOS Partition";
pub const DRIVES_LEFT: &str = "^0300^<H>Drv Volume Label  Mbytes  System  Usage^0100^<H><II>  <RIIIIIIIIIII>    <IIII>  <IIIIIIII>  <IIII>^0100^<H><II>  <RIIIIIIIIIII>    <IIII>  <IIIIIIII>  <IIII>^0100^<H><II>  <RIIIIIIIIIII>    <IIII>  <IIIIIIII>  <IIII>^0100^<H><II>  <RIIIIIIIIIII>    <IIII>  <IIIIIIII>  <IIII>^0100^<H><II>  <RIIIIIIIIIII>    <IIII>  <IIIIIIII>  <IIII>^0100^<H><II>  <RIIIIIIIIIII>    <IIII>  <IIIIIIII>  <IIII>";
pub const DRIVES_LEFT_MORE: &str = "^1000^<H><II>  <RIIIIIIIIIII>    <IIII>  <IIIIIIII>  <IIII>^0100^<H><II>  <RIIIIIIIIIII>    <IIII>  <IIIIIIII>  <IIII>^0100^<H><II>  <RIIIIIIIIIII>    <IIII>  <IIIIIIII>  <IIII>^0100^<H><II>  <RIIIIIIIIIII>    <IIII>  <IIIIIIII>  <IIII>^0100^<H><II>  <RIIIIIIIIIII>    <IIII>  <IIIIIIII>  <IIII>^0100^<H><II>  <RIIIIIIIIIII>    <IIII>  <IIIIIIII>  <IIII>";
pub const DRIVES_RIGHT: &str = "^0341^<H>Drv Volume Label  Mbytes  System  Usage^0141^<H><II>  <RIIIIIIIIIII>    <IIII>  <IIIIIIII>  <IIII>^0141^<H><II>  <RIIIIIIIIIII>    <IIII>  <IIIIIIII>  <IIII>^0141^<H><II>  <RIIIIIIIIIII>    <IIII>  <IIIIIIII>  <IIII>^0141^<H><II>  <RIIIIIIIIIII>    <IIII>  <IIIIIIII>  <IIII>^0141^<H><II>  <RIIIIIIIIIII>    <IIII>  <IIIIIIII>  <IIII>^0141^<H><II>  <RIIIIIIIIIII>    <IIII>  <IIIIIIII>  <IIII>";
pub const DRIVES_RIGHT_MORE: &str = "^1041^<H><II>  <RIIIIIIIIIII>    <IIII>  <IIIIIIII>  <IIII>^0141^<H><II>  <RIIIIIIIIIII>    <IIII>  <IIIIIIII>  <IIII>^0141^<H><II>  <RIIIIIIIIIII>    <IIII>  <IIIIIIII>  <IIII>^0141^<H><II>  <RIIIIIIIIIII>    <IIII>  <IIIIIIII>  <IIII>^0141^<H><II>  <RIIIIIIIIIII>    <IIII>  <IIIIIIII>  <IIII>";
pub const EXTENDED_SPACE: &str =
    "^1704^<RC>Total Extended DOS Partition size is <HIIIIR> Mbytes (1 MByte = 1048576 bytes)";
pub const MAXIMUM_LOGICAL: &str =
    "^1804^<RC>Maximum space available for logical drive is <HIIIIR> Mbytes <H>(<IIII>)";
pub const LOGICAL_SIZE: &str =
    "^2004^<RC>Enter logical drive size in Mbytes or percent of disk space (%)...<H>[<IIISI>]";
pub const ACTIVE_TITLE: &str = "^0430^<H>Set Active Partition";
pub const WHICH_ACTIVE: &str =
    "^1604^<R>Enter the number of the partition you want to make active...........: <H>[<S> ]";
pub const DELETE_TITLE: &str = "^0419^<H>Delete DOS Partition or Logical DOS Drive";
pub const DELETE_PARTITIONS: &str =
    "^1004^<HC>1.  <R>Delete Primary DOS Partition ^0104^<HC>2.  <R>Delete Extended DOS Partition";
pub const DELETE_LOGICAL_OPTION: &str =
    "^1204^<HC>3.  <R>Delete Logical DOS Drive(s) in the Extended DOS Partition";
pub const DELETE_PRIMARY_TITLE: &str = "^0426^<H>Delete Primary DOS Partition";
pub const DELETE_PRIMARY_WARNING: &str = "^1604^<HBC>WARNING! <OR>Data in the deleted Primary DOS Partition will be lost. ^0104^<RC>Do you wish to continue (<Y>/<N>).................? <H>[<S> ]";
pub const DELETE_EXTENDED_TITLE: &str = "^0426^<H>Delete Extended DOS Partition";
pub const DELETE_EXTENDED_WARNING: &str = "^1604^<HBC>WARNING! <OR>Data in the deleted Extended DOS Partition will be lost. ^0104^<RC>Do you wish to continue (<Y>/<N>).................? <H>[<S> ]";
pub const DELETE_LOGICAL_TITLE: &str =
    "^0112^<H>Delete Logical DOS Drive(s) in the Extended DOS Partition";
pub const DELETE_LOGICAL_WARNING: &str = "^1904^<HBC>WARNING! <OR>Data in a deleted Logical DOS Drive will be lost. ^0104^<RC>What drive do you want to delete...............................? <H>[<S> ]";
pub const SURE: &str = "^2204^<R>Are you sure (<Y>/<N>)..............................? <H>[<S> ]";
pub const ENTER_LABEL: &str =
    "^2104^<R>Enter Volume Label..............................? <H>[<S>           ]";
pub const DISPLAY_TITLE: &str = "^0426^<H>Display Partition Information";
pub const SHOW_DRIVES: &str = "^1704^<RC>The Extended DOS Partition contains Logical DOS Drives. ^0104^<RC>Do you want to display the logical drive information (<Y>/<N>)......?<H>[<S> ]";
pub const DRIVES_TITLE: &str = "^0121^<H>Display Logical DOS Drive Information";
pub const RESTART: &str = "^1304^<H>System will now restart ^0204^<R>Insert DOS diskette in drive A: ^0104^<R>Press any key when ready . . .<S>";
pub const PRIMARY_DELETED: &str = "^2104^<CH>Primary DOS Partition deleted ^0100^<CW>";
pub const EXTENDED_DELETED: &str = "^2104^<CH>Extended DOS Partition deleted ^0100^<CW>";
pub const DRIVE_DELETED: &[u8] = b"Drive deleted";
pub const MADE_ACTIVE: &str = "^2104^<CH>Partition <I> made active ^0100^<CW>";
pub const PRIMARY_CREATED: &str = "^2104^<CH>Primary DOS Partition created ^0100^<CW>";
pub const EXTENDED_CREATED: &str = "^2104^<CH>Extended DOS Partition created ^0100^<CW>";
pub const LOGICAL_CREATED: &str =
    "^2204^<CH>Logical DOS Drive created, drive letters changed or added<W>";
pub const NO_PARTITIONS: &str = "^2104^<CH>No partitions defined ^0100^<C>";
pub const NO_DRIVES: &str = "^1004^<CH>No logical drives defined ^0100^<C>";
pub const LETTERS_CHANGED: &str = "^1804^<CH>Drive letters have been changed or deleted<W>";
pub const PRIMARY_CREATED_LETTERS: &str =
    "^2104^<CH>Primary DOS Partition created, drive letters changed or added ^0100^<CW>";
pub const WRITE_FAILED: &str = "^2204^<CH>Error writing fixed disk. ^0100^<CW>";
pub const NO_PRIMARY: &str = "^2204^<CH>No Primary DOS Partition to delete. ^0100^<CW>";
pub const NO_EXTENDED: &str = "^2204^<CH>No Extended DOS Partition to delete. ^0100^<CW>";
pub const PRIMARY_EXISTS: &str = "^2204^<CH>Primary DOS Partition already exists. ^0100^<CW>";
pub const EXTENDED_EXISTS: &str = "^2204^<CH>Extended DOS Partition already exists. ^0100^<CW>";
pub const NO_SPACE: &str = "^2204^<CH>No space to create a DOS partition. ^0100^<CW>";
pub const LOGICAL_TOO_LARGE: &str =
    "^2204^<CH>Requested logical drive size exceeds the maximum available space.<W> ^0100^<CW>";
pub const PARTITION_TOO_LARGE: &str =
    "^2204^<CH>Requested partition size exceeds the maximum available space.<W> ^0100^<CW>";
pub const NOTHING_TO_DELETE: &str = "^2204^<CH>No partitions to delete. ^0100^<CW>";
pub const ONLY_ONE_ACTIVE: &str =
    "^2204^<CH>The only startable partition on Drive 1 is already set active.<W> ^0100^<CW>";
pub const NOTHING_TO_ACTIVATE: &str = "^2204^<CH>No partitions to make active. ^0100^<CW>";
pub const NOT_STARTABLE: &str =
    "^2204^<CH>Partition selected (<I>) is not startable, active partition not changed.<W>";
pub const PRIMARY_FIRST: &str = "^2204^<CH>Cannot create Extended DOS Partition without ^0104^<CH>Primary DOS Partition on disk 1.<W>";
pub const EXTENDED_FULL: &str = "^2204^<CH>All available space in the Extended DOS Partition ^0104^<CH>is assigned to logical drives.<W>";
pub const DRIVES_EXIST: &str =
    "^2204^<CH>Cannot delete Extended DOS Partition while logical drives exist.<W>";
pub const ALL_DRIVES_DELETED: &str =
    "^2204^<CH>All logical drives deleted in the Extended DOS Partition.<W>";
pub const NOT_A_CHOICE: &str = "^2204^<C> ^0104^<CHI> is not a choice. Please enter <III>.<W>";
pub const ACTIVE_NOT_STARTABLE: &str =
    "^2204^<CH>WARNING! The partition set active is not startable.<W>";
pub const NONE_STARTABLE: &str = "^2204^<CH> Only non-startable partitions exist. ^0100^<CW>";
pub const FIRST_DISK_ONLY: &str = "^2204^<CH>Only partitions on Drive 1 can be made active.<W>";
pub const TOO_MANY_DRIVES: &str = "^2204^<CH>Maximum number of Logical DOS Drives installed.<W>";
pub const ZERO_SIZE: &str = "^2204^<CH>Cannot create a zero size partition. ^0100^<CW>";
pub const ALREADY_DELETED: &str = "^2204^<CH>Drive <II> already deleted. ^0100^<CW>";
pub const UNREADABLE: &str = "^2204^<CHB>Unable to access Drive <I>.<OW>";
pub const INVALID_ENTRY: &str = "^2304^<CH>Invalid entry, please enter <III>.<W>";
pub const EXTENDED_IN_THE_WAY: &str = "^2204^<CH>Cannot delete Primary DOS Partition on drive 1  ^0104^<CH>when an Extended DOS Partition exists.<W>";
pub const PRESS_ENTER: &str = "^2200^<C> ^0104^<CH>Invalid entry, please press Enter.<W>";
pub const LABEL_MISMATCH: &str = "^2204^<CH>Volume label does not match.<W>";
pub const EXTENDED_FIRST: &str = "^2204^<CH>Cannot create Logical DOS Drive without ^0104^<CH>an Extended DOS Partition on the current drive.<W>";
pub const NO_DRIVES_TO_DELETE: &str = "^2204^<CH>No Logical DOS Drive(s) to delete. ^0100^<CW>";

/// The name the program's first screen gives it, under the version.
pub const PROGRAM: &[u8] = b"Fixed Disk Setup Program";

/// What is written to the standard error where no screen is drawn: before
/// one is, and when partitions are made from the command line.
pub mod error {
    pub use crate::dos::message::BAD_PARAMETER;
    pub const NETWORK: &[u8] = b"Cannot FDISK with network loaded\r\n";
    pub const NO_DISKS: &[u8] = b"No fixed disks present\r\n";
    pub const READ_FAILED: &[u8] = b"Error reading fixed disk\r\n";
    pub const WRITE_FAILED: &[u8] = b"Error writing fixed disk\r\n";
}
