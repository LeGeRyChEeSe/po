//! `po-local` - control plane for local inference.
//!
//! Where other harnesses stop at being a client of an OpenAI-compatible endpoint, this crate
//! covers the whole chain: discovering the hardware, reading the models' geometry, deriving
//! launch parameters that hold, then driving the server.

pub mod background;
pub mod catalog;
pub mod gguf;
pub mod hardware;
pub mod kvstore;
pub mod llama;
pub mod placement;
pub mod plan;
pub mod prefs;
pub mod quant;
pub mod recommend;
pub mod strata;
pub mod usage;
pub mod server;
pub mod supervisor;

/// Runs `spawn` with Po's own console handles kept from the child. Windows hands a new process
/// every inheritable handle of its parent, not only the three it is given: a server meant to
/// outlive Po would keep Po's output pipe open, and `po load | tail` would never end (observed).
/// The handles get their flag back right after.
pub(crate) fn spawn_detached<T>(spawn: impl FnOnce() -> T) -> T {
    #[cfg(windows)]
    {
        use windows_sys::Win32::Foundation::{GetHandleInformation, HANDLE_FLAG_INHERIT, INVALID_HANDLE_VALUE, SetHandleInformation};
        use windows_sys::Win32::System::Console::{GetStdHandle, STD_ERROR_HANDLE, STD_INPUT_HANDLE, STD_OUTPUT_HANDLE};
        let mut cleared = Vec::new();
        for which in [STD_INPUT_HANDLE, STD_OUTPUT_HANDLE, STD_ERROR_HANDLE] {
            // SAFETY: plain Win32 calls on this process's own standard handles.
            unsafe {
                let handle = GetStdHandle(which);
                let mut flags = 0u32;
                if handle.is_null() || handle == INVALID_HANDLE_VALUE || GetHandleInformation(handle, &mut flags) == 0 {
                    continue;
                }
                if flags & HANDLE_FLAG_INHERIT != 0 && SetHandleInformation(handle, HANDLE_FLAG_INHERIT, 0) != 0 {
                    cleared.push(handle);
                }
            }
        }
        let out = spawn();
        for handle in cleared {
            // SAFETY: the same handles, given their flag back.
            unsafe {
                SetHandleInformation(handle, HANDLE_FLAG_INHERIT, HANDLE_FLAG_INHERIT);
            }
        }
        out
    }
    #[cfg(not(windows))]
    spawn()
}

pub use gguf::{GgufInfo, read_header};
pub use hardware::{Gpu, GpuBackend, Hardware};
pub use server::{ModelStatus, RemoteModel, ServerMode};
pub use background::BackgroundServerInfo;
pub use catalog::{CatalogEntry, scan};
pub use supervisor::{ServerState, Supervisor, SupervisorError};
pub use plan::{CacheType, Compromise, LaunchPlan, PlanRequest, plan};
