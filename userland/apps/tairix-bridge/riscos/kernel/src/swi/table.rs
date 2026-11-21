//! The kernel's SWIs, by number: those it performs in Rust, those that are
//! their vectors, and those the VDU drivers perform.

use crate::convert::CONVERT_VARIFORM;
use crate::env::{BREAKPOINT, CALLBACK};
use crate::os::{Entry, Fail, Os, Regs};
use crate::swi::names::{CONVERT_DATE_AND_TIME, CONVERT_HEX1, CONVERT_STANDARD_DATE_AND_TIME};
use crate::swi::os as number;
use crate::swi::Handler;
use crate::vectors::{self, Carry};

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
    if (CONVERT_HEX1..=CONVERT_VARIFORM).contains(&number) {
        return Some(Os::conversion);
    }
    let handler: Handler<'o> = match number {
        number::WRITE_C => Os::write_c_swi,
        number::WRITE_S => Os::write_s,
        number::WRITE_0 => Os::write_0,
        number::NEW_LINE => Os::new_line,
        number::READ_C => Os::read_c,
        number::CLI => Os::os_cli,
        number::BYTE => Os::byte,
        number::WORD => Os::word,
        number::FILE => Os::os_file,
        number::ARGS => Os::os_args,
        number::BGET => Os::os_bget,
        number::BPUT => Os::os_bput,
        number::GBPB => Os::os_gbpb,
        number::FIND => Os::os_find,
        number::READ_LINE => Os::read_line,
        number::CONTROL => Os::control,
        number::GET_ENV => Os::get_env,
        number::EXIT => Os::exit,
        number::SET_ENV => Os::set_env,
        number::INT_ON => Os::int_on,
        number::INT_OFF => Os::int_off,
        number::CALL_BACK => Os::callback_swi,
        number::ENTER_OS => Os::enter_os,
        number::BREAK_PT => Os::break_point,
        number::BREAK_CTRL => Os::break_ctrl,
        number::UNUSED_SWI => Os::unused_swi,
        number::UPDATE_MEMC => Os::update_memc,
        number::SET_CALLBACK => Os::set_callback,
        number::MOUSE => Os::mouse,
        number::HEAP => Os::os_heap,
        number::MODULE => Os::os_module,
        number::CLAIM => Os::claim,
        number::RELEASE => Os::release,
        number::READ_UNSIGNED => Os::read_unsigned_swi,
        number::GENERATE_EVENT => Os::generate_event,
        number::READ_VAR_VAL => Os::read_var_val,
        number::SET_VAR_VAL => Os::set_var_val,
        number::GS_INIT => Os::gs_init,
        number::GS_READ => Os::gs_read,
        number::GS_TRANS => Os::gs_trans,
        number::BINARY_TO_DECIMAL => Os::binary_to_decimal,
        number::FS_CONTROL => Os::os_fs_control,
        number::CHANGE_DYNAMIC_AREA => Os::change_dynamic_area,
        number::GENERATE_ERROR => Os::generate_error,
        number::READ_ESCAPE_STATE => Os::read_escape_state,
        number::EVALUATE_EXPRESSION => Os::evaluate_expression,
        number::SPRITE_OP => Os::os_sprite_op,
        number::SERVICE_CALL => Os::service_call,
        number::UPCALL => Os::up_call,
        number::CALL_A_VECTOR => Os::call_a_vector,
        number::SWI_NUMBER_TO_STRING => Os::swi_number_to_string,
        number::SWI_NUMBER_FROM_STRING => Os::swi_number_from_string,
        number::VALIDATE_ADDRESS => Os::validate_address,
        number::CALL_AFTER => Os::call_after,
        number::CALL_EVERY => Os::call_every,
        number::REMOVE_TICKER_EVENT => Os::remove_ticker_event,
        number::INSTALL_KEY_HANDLER => Os::install_key_handler,
        number::CHANGE_ENVIRONMENT => Os::change_environment,
        number::READ_MONOTONIC_TIME => Os::read_monotonic_time,
        number::SUBSTITUTE_ARGS => Os::substitute_args_plain,
        number::PRETTY_PRINT => Os::pretty_print,
        number::WRITE_N => Os::write_n,
        number::ADD_TO_VECTOR => Os::add_to_vector,
        number::WRITE_ENV => Os::write_env,
        number::READ_ARGS => Os::read_args_swi,
        number::READ_RAM_FS_LIMITS => Os::read_ramfs_limits,
        number::CLAIM_DEVICE_VECTOR => Os::claim_device_vector,
        number::RELEASE_DEVICE_VECTOR => Os::release_device_vector,
        number::DELINK_APPLICATION => Os::delink_application,
        number::RELINK_APPLICATION => Os::relink_application,
        number::HEAP_SORT => Os::heap_sort_legacy,
        number::EXIT_AND_DIE => Os::exit_and_die,
        number::READ_MEM_MAP_INFO => Os::read_mem_map_info,
        number::READ_MEM_MAP_ENTRIES => Os::read_mem_map_entries,
        number::SET_MEM_MAP_ENTRIES => Os::set_mem_map_entries,
        number::ADD_CALLBACK => Os::add_callback,
        number::READ_DEFAULT_HANDLER => Os::read_default_handler,
        number::SERIAL_OP => Os::serial_op,
        number::READ_SYS_INFO => Os::read_sys_info,
        number::CONFIRM => Os::confirm,
        number::CRC => Os::crc,
        number::READ_DYNAMIC_AREA => Os::read_dynamic_area,
        number::PRINT_CHAR => Os::print_char,
        number::CHANGE_REDIRECTION => Os::change_redirection,
        number::REMOVE_CALLBACK => Os::remove_callback,
        number::FIND_MEM_MAP_ENTRIES => Os::find_mem_map_entries,
        number::DYNAMIC_AREA => Os::dynamic_area,
        number::ABORT_TRAP => Os::abort_trap,
        number::MEMORY => Os::os_memory,
        number::CLAIM_PROCESSOR_VECTOR => Os::claim_processor_vector,
        number::RESET => Os::reset,
        number::MMU_CONTROL => Os::mmu_control,
        number::RESYNC_TIME => Os::resync_time,
        number::PLATFORM_FEATURES => Os::platform_features,
        number::SYNCHRONISE_CODE_AREAS => Os::synchronise_code_areas,
        number::AMB_CONTROL => Os::amb_control,
        number::NV_MEMORY => Os::nv_memory,
        number::TASK_CONTROL => Os::task_control,
        number::HARDWARE => Os::hardware,
        number::IIC_OP => Os::iic_op,
        number::LEAVE_OS => Os::leave_os,
        number::READ_LINE32 => Os::read_line32,
        number::SUBSTITUTE_ARGS32 => Os::substitute_args32,
        number::HEAP_SORT32 => Os::heap_sort32,
        number::PLOT => Os::plot,
        number::READ_PALETTE
        | number::READ_VDU_VARIABLES
        | number::READ_POINT
        | number::READ_MODE_VARIABLE
        | number::REMOVE_CURSORS
        | number::RESTORE_CURSORS
        | number::CHECK_MODE_VALID
        | number::CLAIM_SCREEN_MEMORY
        | number::SET_ECF_ORIGIN
        | number::CHANGED_BOX
        | number::SET_COLOUR
        | number::POINTER
        | number::SCREEN_MODE => Os::vdu_swi,
        CONVERT_STANDARD_DATE_AND_TIME => Os::convert_standard_date_and_time,
        CONVERT_DATE_AND_TIME => Os::convert_date_and_time,
        _ => return None,
    };
    Some(handler)
}
