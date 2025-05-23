#![allow(clippy::missing_safety_doc)]

use std::ffi::c_char;

use crate::sys;

/// The function type to pass as a logger to libinput
pub type Logger = Option<unsafe extern "C" fn(sys::libinput_log_priority, *const c_char)>;

unsafe extern "C" {
    fn colpetto_inner_set_log_callback(callback: Logger);
    fn colpetto_inner_get_log_handler() -> sys::libinput_log_handler;
}

pub(crate) fn setup_logger(libinput: *mut sys::libinput, logger: Logger) {
    if logger.is_none() {
        return;
    }

    unsafe {
        colpetto_inner_set_log_callback(logger);
    }

    unsafe {
        sys::libinput_log_set_priority(
            libinput,
            sys::libinput_log_priority::LIBINPUT_LOG_PRIORITY_DEBUG,
        );
    }

    unsafe {
        sys::libinput_log_set_handler(libinput, colpetto_inner_get_log_handler());
    }
}
