//! Port of `ErrorHandlers.cs`: the C# hooked unhandled and unobserved exceptions so a fault was
//! logged rather than lost. Rust's equivalent is the panic hook.

use basis_network_core::BNL;

pub struct ErrorHandlers;

impl ErrorHandlers {
    pub fn attach_global_handlers() {
        std::panic::set_hook(Box::new(|info| {
            BNL::log_error(format!("Fatal exception: {info}"));
            BNL::log_error(format!("Stack trace: {}", std::backtrace::Backtrace::force_capture()));
        }));
    }
}
