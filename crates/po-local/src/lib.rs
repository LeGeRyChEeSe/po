//! `po-local` — control plane for local inference.
//!
//! Where other harnesses stop at being a client of an OpenAI-compatible endpoint, this crate
//! covers the whole chain: discovering the hardware, reading the models' geometry, deriving
//! launch parameters that hold, then driving the server.

pub mod catalog;
pub mod gguf;
pub mod hardware;
pub mod kvstore;
pub mod llama;
pub mod plan;
pub mod prefs;
pub mod quant;
pub mod recommend;
pub mod strata;
pub mod server;
pub mod supervisor;

pub use gguf::{GgufInfo, read_header};
pub use hardware::{Gpu, GpuBackend, Hardware};
pub use server::{ModelStatus, RemoteModel, ServerMode};
pub use catalog::{CatalogEntry, scan};
pub use supervisor::{ServerState, Supervisor, SupervisorError};
pub use plan::{CacheType, Compromise, LaunchPlan, PlanRequest, plan};
