//! The kernel's SWIs, by number: those it performs in Rust, those that are
//! their vectors, and those the VDU drivers perform.

use crate::env::{BREAKPOINT, CALLBACK};
use crate::os::{Entry, Fail, Os, Regs};
use crate::swi::names::{CONVERT_DATE_AND_TIME, CONVERT_HEX1, CONVERT_STANDARD_DATE_AND_TIME};
use crate::swi::Handler;
use crate::vectors::{self, Carry};

/// The number conversions past `OS_ConvertHex1`.
const CONVERSIONS: u32 = 30;

macro_rules! through {
    ($($name:ident: $vector:expr, $carry:expr;)*) => {
        impl Os<'_> {
            $(
                /// A SWI that is its vector, interrupts enabled.
                fn $name(&mut self, regs: &mut Regs) -> Result<(), Fail> {
                    self.vectored($vector, regs, Entry::SupervisorIrq, $carry)
                }
            )*
        }
    };
}

through! {
    os_file: vectors::FILE, Carry::Vector;
    os_args: vectors::ARGS, Carry::Vector;
    os_bget: vectors::BGET, Carry::Vector;
    os_bput: vectors::BPUT, Carry::Caller;
    os_gbpb: vectors::GBPB, Carry::Vector;
    os_find: vectors::FIND, Carry::Vector;
    os_fs_control: vectors::FSC, Carry::Vector;
    os_sprite_op: vectors::SPRITE, Carry::Vector;
}

impl Os<'_> {
    /// `OS_CallBack`.
    fn callback_swi(&mut self, regs: &mut Regs) -> Result<(), Fail> {
        self.handler_and_buffer(CALLBACK, regs)
    }

    /// `OS_BreakCtrl`.
    fn break_ctrl(&mut self, regs: &mut Regs) -> Result<(), Fail> {
        self.handler_and_buffer(BREAKPOINT, regs)
    }

    /// One of the SWIs the VDU drivers perform, or not known without them.
    fn vdu_swi(&mut self, regs: &mut Regs) -> Result<(), Fail> {
        let number = self.world.performing;
        match self.world.vdu.and_then(|vdu| vdu.swi) {
            Some(swi) => swi(self, number, regs),
            None => Err(self.no_such_swi(number)),
        }
    }

    /// A number conversion.
    fn conversion(&mut self, regs: &mut Regs) -> Result<(), Fail> {
        let number = self.world.performing;
        self.convert(number, regs)
    }
}

/// The kernel's handler of OS SWI `number`, below `OS_WriteI`; none for a
/// number no SWI has.
#[expect(clippy::too_many_lines, reason = "one arm a SWI")]
pub(crate) fn os<'o>(number: u32) -> Option<Handler<'o>> {
    if (CONVERT_HEX1..CONVERT_HEX1 + CONVERSIONS).contains(&number) {
        return Some(Os::conversion);
    }
    let handler: Handler<'o> = match number {
        0x00 => Os::write_c_swi,
        0x01 => Os::write_s,
        0x02 => Os::write_0,
        0x03 => Os::new_line,
        0x04 => Os::read_c,
        0x05 => Os::os_cli,
        0x06 => Os::byte,
        0x07 => Os::word,
        0x08 => Os::os_file,
        0x09 => Os::os_args,
        0x0A => Os::os_bget,
        0x0B => Os::os_bput,
        0x0C => Os::os_gbpb,
        0x0D => Os::os_find,
        0x0E => Os::read_line,
        0x0F => Os::control,
        0x10 => Os::get_env,
        0x11 => Os::exit,
        0x12 => Os::set_env,
        0x13 => Os::int_on,
        0x14 => Os::int_off,
        0x15 => Os::callback_swi,
        0x16 => Os::enter_os,
        0x17 => Os::break_point,
        0x18 => Os::break_ctrl,
        0x19 => Os::unused_swi,
        0x1A => Os::update_memc,
        0x1B => Os::set_callback,
        0x1C => Os::mouse,
        0x1D => Os::os_heap,
        0x1E => Os::os_module,
        0x1F => Os::claim,
        0x20 => Os::release,
        0x21 => Os::read_unsigned_swi,
        0x22 => Os::generate_event,
        0x23 => Os::read_var_val,
        0x24 => Os::set_var_val,
        0x25 => Os::gs_init,
        0x26 => Os::gs_read,
        0x27 => Os::gs_trans,
        0x28 => Os::binary_to_decimal,
        0x29 => Os::os_fs_control,
        0x2A => Os::change_dynamic_area,
        0x2B => Os::generate_error,
        0x2C => Os::read_escape_state,
        0x2D => Os::evaluate_expression,
        0x2E => Os::os_sprite_op,
        0x30 => Os::service_call,
        0x33 => Os::up_call,
        0x34 => Os::call_a_vector,
        0x38 => Os::swi_number_to_string,
        0x39 => Os::swi_number_from_string,
        0x3A => Os::validate_address,
        0x3B => Os::call_after,
        0x3C => Os::call_every,
        0x3D => Os::remove_ticker_event,
        0x3E => Os::install_key_handler,
        0x40 => Os::change_environment,
        0x42 => Os::read_monotonic_time,
        0x43 => Os::substitute_args_plain,
        0x44 => Os::pretty_print,
        0x46 => Os::write_n,
        0x47 => Os::add_to_vector,
        0x48 => Os::write_env,
        0x49 => Os::read_args_swi,
        0x4A => Os::read_ramfs_limits,
        0x4B => Os::claim_device_vector,
        0x4C => Os::release_device_vector,
        0x4D => Os::delink_application,
        0x4E => Os::relink_application,
        0x4F => Os::heap_sort_legacy,
        0x50 => Os::exit_and_die,
        0x51 => Os::read_mem_map_info,
        0x52 => Os::read_mem_map_entries,
        0x53 => Os::set_mem_map_entries,
        0x54 => Os::add_callback,
        0x55 => Os::read_default_handler,
        0x57 => Os::serial_op,
        0x58 => Os::read_sys_info,
        0x59 => Os::confirm,
        0x5B => Os::crc,
        0x5C => Os::read_dynamic_area,
        0x5D => Os::print_char,
        0x5E => Os::change_redirection,
        0x5F => Os::remove_callback,
        0x60 => Os::find_mem_map_entries,
        0x66 => Os::dynamic_area,
        0x67 => Os::abort_trap,
        0x68 => Os::os_memory,
        0x69 => Os::claim_processor_vector,
        0x6A => Os::reset,
        0x6B => Os::mmu_control,
        0x6C => Os::resync_time,
        0x6D => Os::platform_features,
        0x6E => Os::synchronise_code_areas,
        0x70 => Os::amb_control,
        0x76 => Os::nv_memory,
        0x78 => Os::task_control,
        0x7A => Os::hardware,
        0x7B => Os::iic_op,
        0x7C => Os::leave_os,
        0x7D => Os::read_line32,
        0x7E => Os::substitute_args32,
        0x7F => Os::heap_sort32,
        0x2F | 0x31 | 0x32 | 0x35 | 0x36 | 0x37 | 0x3F | 0x41 | 0x45 | 0x56 | 0x5A | 0x61
        | 0x64 | 0x65 => Os::vdu_swi,
        CONVERT_STANDARD_DATE_AND_TIME => Os::convert_standard_date_and_time,
        CONVERT_DATE_AND_TIME => Os::convert_date_and_time,
        _ => return None,
    };
    Some(handler)
}
