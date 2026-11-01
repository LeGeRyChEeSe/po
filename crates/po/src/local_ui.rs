//! Interactive management of local models from within the session.
//!
//! This is where "select a model" becomes "a server is running and answering": the catalog is
//! scanned, the user picks a number, Po stops the running server (if any), starts a new one
//! with parameters derived from the hardware, waits until it is really ready, then registers
//! the model with the router — which can from then on hand sub-agents to it.

use std::collections::HashMap;
use std::sync::Arc;

use po_agent::Agent;
use po_core::{Capabilities, ModelSpec, Origin, Pricing};
use po_local::{CatalogEntry, Hardware, PlanRequest, Supervisor, catalog};
use po_provider::OpenAiProvider;

use crate::config::guess_tier;

/// Maximum number of models loaded at the same time in the router. Beyond that, the memory of
/// an ordinary development machine cannot keep up, and llama.cpp evicts on its own.
const MAX_LOADED_MODELS: u32 = 2;

/// State of local management over the course of the session.
pub struct LocalManager {
    pub supervisor: Option<Supervisor>,
    pub hardware: Hardware,
    pub catalog: Vec<CatalogEntry>,
    /// Identifier, in the router, of the model currently served by our server.
    pub active_model_id: Option<String>,
    /// Last use of each model, in activation order. Used to decide which one to unload when
    /// memory runs short: the least recently used goes first.
    last_used: HashMap<String, u64>,
    /// Monotonic counter: a real timestamp would be needlessly precise for a mere ordering.
    usage_clock: u64,
    /// The engines (`/moteurs`): what was detected, and the Strata server Po started.
    pub engines: crate::engines_ui::Engines,
}

/// Width reserved for the model name in the tabular form.
const LABEL_COLS: usize = 36;

/// Below this width, the listing uses two lines per model.
///
/// The threshold comes from the wide line itself: four columns of indentation, six for the
/// number, thirty-six for the name, then the size and the plan. Below that, it folds.
const WIDE_LISTING_COLS: usize = 64;

/// Display width of a string, ideographs and emoji included.
fn display_width(text: &str) -> usize {
    po_term::width::display_width(text, po_term::UnicodeLevel::Full)
}

/// Cuts a line to the available width, marking the cut.
///
/// The marker is not a nicety: a model is chosen by its name, and two variants of the same
/// family often differ only by their end. Seen on screen from a 56-column Android terminal,
/// `Hermes3.6-…-V6-APEX-Compact` stopped at "Compa", with nothing to say that something was
/// missing.
fn truncate_line(line: &str, columns: usize) -> String {
    po_term::width::truncate_with_ellipsis(line, columns, po_term::UnicodeLevel::Full)
}

/// Terminal width in columns.
fn terminal_columns() -> usize {
    po_term::terminal_size().0 as usize
}

impl LocalManager {
    pub fn detect() -> Self {
        let hardware = Hardware::detect();
        let catalog = catalog::scan(&hardware, &PlanRequest::default());
        Self {
            supervisor: Supervisor::discover().ok(),
            hardware,
            catalog,
            active_model_id: None,
            last_used: HashMap::new(),
            usage_clock: 0,
            engines: Default::default(),
        }
    }

    /// Like [`detect`](Self::detect), without scanning the disk for GGUF files. The scan also
    /// walks the WSL distributions and took 2.7 s at startup (measured); when models are already
    /// reachable, it waits for the first request (`/local`, Ctrl+L), which refreshes it anyway.
    /// Nothing detected, nothing running: for tests that bring their own findings.
    #[cfg(test)]
    pub fn empty() -> Self {
        Self {
            supervisor: None,
            hardware: Hardware::default(),
            catalog: Vec::new(),
            active_model_id: None,
            last_used: HashMap::new(),
            usage_clock: 0,
            engines: Default::default(),
        }
    }

    pub fn detect_lazy() -> Self {
        Self {
            supervisor: Supervisor::discover().ok(),
            hardware: Hardware::detect(),
            catalog: Vec::new(),
            active_model_id: None,
            last_used: HashMap::new(),
            usage_clock: 0,
            engines: Default::default(),
        }
    }

    /// Refreshes the catalog and the memory measurements. Free VRAM changes constantly;
    /// a plan computed ten minutes ago no longer describes the machine.
    pub fn refresh(&mut self) {
        self.hardware = Hardware::detect();
        self.catalog = catalog::scan(&self.hardware, &PlanRequest::default());
    }

    pub fn has_server_binary(&self) -> bool {
        self.supervisor.is_some()
    }

    /// Returns the displayable list of the models found, with their feasibility.
    pub fn listing(&self) -> Vec<String> {
        self.listing_for_width(terminal_columns())
    }

    /// Listing formatted for a given width.
    ///
    /// On a phone screen, a sixty-column line wraps in the middle of a word and the table
    /// becomes unreadable. Below this threshold, the name and the plan go on two lines rather
    /// than relying on the terminal's wrapping, which does not know where to cut.
    pub fn listing_for_width(&self, columns: usize) -> Vec<String> {
        let narrow = columns < WIDE_LISTING_COLS;
        let mut lines = Vec::new();
        for (index, entry) in self.catalog.iter().enumerate() {
            let state = if !entry.plan.viable {
                po_i18n::t!("chat.local.does_not_fit").to_owned()
            } else {
                entry.plan.summary()
            };
            let label = entry.label();
            let active = if self.active_model_id.as_deref() == Some(label.as_str()) {
                " ←"
            } else {
                ""
            };
            if narrow {
                // The number stays at the head of the first line: it is what gets typed next.
                // And each line is cut to the real width: letting the terminal wrap a
                // fifty-column name brings back exactly the mess we were avoiding.
                lines.push(truncate_line(&format!("  [{}] {label}{active}", index + 1), columns));
                lines.push(truncate_line(
                    &format!("      {} {} · {state}", entry.size_mib(), po_i18n::t!("chat.unit_mib")),
                    columns,
                ));
            } else {
                // A name longer than its column pushes everything else off screen: we cut it,
                // then cut the whole line, so the terminal never has to wrap in the middle of
                // a word.
                let name = truncate_line(&label, LABEL_COLS);
                let padding = LABEL_COLS.saturating_sub(display_width(&name));
                lines.push(truncate_line(
                    &format!(
                        "  [{}] {name}{} {:>6} {}  {state}{active}",
                        index + 1,
                        " ".repeat(padding),
                        entry.size_mib(),
                        po_i18n::t!("chat.unit_mib")
                    ),
                    columns,
                ));
            }
        }
        lines
    }

    /// Starts the model at the given index (1-based) and registers it with the agent.
    pub async fn activate(&mut self, index: usize, agent: &mut Agent) -> Result<String, String> {
        let Some(entry) = self.catalog.get(index.wrapping_sub(1)) else {
            return Err(po_i18n::t!("chat.localui.no_such_model", index = index));
        };
        let Some(supervisor) = self.supervisor.as_mut() else {
            return Err(po_i18n::t!("chat.localui.server_missing_install").to_owned());
        };
        // The plan was computed for the freest card: the server will not see any other.
        supervisor.set_env(self.hardware.device_pin());

        let entry = entry.clone();
        let label = entry.label();

        if supervisor.already_serving(&entry.path) {
            return Ok(po_i18n::t!("chat.localui.already_served", label = label));
        }
        if !entry.plan.viable {
            return Err(po_i18n::t!("chat.localui.does_not_fit_now", label = label));
        }

        // Two or more models in the same directory: go through llama.cpp's router mode, which
        // lets them coexist in memory. A single model does not justify it — a dedicated server
        // starts faster and uses less.
        let siblings = self
            .catalog
            .iter()
            .filter(|other| other.path.parent() == entry.path.parent())
            .count();
        let models_dir = entry.path.parent().map(|p| p.to_path_buf());
        // What had to be unloaded to make room: the user must know, otherwise a model
        // disappears from their session without explanation.
        let mut evicted: Vec<String> = Vec::new();

        let port = match (siblings > 1, models_dir) {
            (true, Some(dir)) => {
                if supervisor.is_router() {
                    // The router is already running: loading one more model does not cost a
                    // restart, and the models already loaded keep serving.
                    let port = supervisor.running().map(|s| s.port).unwrap_or_default();

                    // Before adding a model, check that memory keeps up. Letting llama.cpp
                    // evict at random would mean losing, every other time, the model the user
                    // just used.
                    let needed = entry.plan.estimated_vram_mib;
                    let free = Hardware::detect().usable_vram_mib(0);
                    if needed > 0 && free < needed {
                        let loaded = supervisor.list_models().await.unwrap_or_default();
                        for victim in
                            po_local::server::eviction_order(&loaded, &self.last_used, &label)
                        {
                            if supervisor.unload_model(&victim).await.is_ok() {
                                evicted.push(victim.clone());
                                agent.router_mut().remove_model(&victim);
                                if Hardware::detect().usable_vram_mib(0) >= needed {
                                    break;
                                }
                            }
                        }
                    }

                    supervisor
                        .load_model(&label)
                        .await
                        .map_err(|err| format!("{label}: {err}"))?;
                    port
                } else {
                    let threads = self.hardware.inference_threads();
                    let port = supervisor
                        .start_router(&dir, MAX_LOADED_MODELS, threads, entry.plan.context)
                        .await
                        .map_err(|err| po_i18n::t!("chat.localui.router_error", err = err))?;
                    supervisor
                        .load_model(&label)
                        .await
                        .map_err(|err| format!("{label}: {err}"))?;
                    port
                }
            }
            _ => supervisor
                .start(&entry.path, &entry.plan)
                .await
                .map_err(|err| format!("{label}: {err}"))?,
        };

        let base_url = format!("http://127.0.0.1:{port}/v1");
        let spec = ModelSpec {
            id: label.clone(),
            display_name: format!("{label} (local)"),
            origin: Origin::Local { backend: "llama.cpp".into(), endpoint: base_url.clone() },
            tier: guess_tier(&label),
            capabilities: Capabilities { tools: true, ..Default::default() },
            // The context actually granted is the plan's, not the model's: announcing more to
            // the router would cause overflows on the server side.
            context_window: entry.plan.context,
            max_output_tokens: 4096,
            pricing: Pricing::FREE,
        };

        // In router mode, the previous model stays loaded and reachable: removing it from the
        // fleet would deprive the session of a still-living source. Outside router mode, its
        // server has just been stopped, so it must disappear.
        let router_mode = self
            .supervisor
            .as_ref()
            .is_some_and(po_local::Supervisor::is_router);
        if let Some(previous) = self.active_model_id.take()
            && !router_mode
        {
            agent.router_mut().remove_model(&previous);
        }
        agent.router_mut().add_model(spec.clone());
        agent.attach(
            spec.id.clone(),
            Arc::new(OpenAiProvider::local("llama.cpp", base_url)),
        );
        self.active_model_id = Some(label.clone());
        self.usage_clock += 1;
        self.last_used.insert(label.clone(), self.usage_clock);

        let mut message = po_i18n::t!("chat.localui.served", label = label, port = port, summary = entry.plan.summary());
        for compromise in &entry.plan.compromises {
            message.push_str(&format!("\n    · {compromise}"));
        }
        for victim in &evicted {
            message.push_str(&format!("\n    · {}", po_i18n::t!("chat.localui.evicted", model = victim)));
        }
        Ok(message)
    }

    /// Does the running server accept unloading a model without stopping?
    ///
    /// A single-model server has nothing to unload: asking it would produce a command that
    /// fails silently. The question is therefore asked before offering the option.
    pub async fn supports_unload(&self) -> bool {
        let Some(supervisor) = self.supervisor.as_ref() else {
            return false;
        };
        match supervisor.list_models().await {
            Ok(models) => po_local::ServerMode::infer(&models).supports_unload(),
            Err(_) => false,
        }
    }

    /// Unloads a single model, the server and the other models staying in service.
    ///
    /// This is the operation that gives router mode its meaning: freeing the memory of a model
    /// that became useless without interrupting the session or reloading the others.
    pub async fn unload(&mut self, label: &str, agent: &mut Agent) -> Result<String, String> {
        if !self.supports_unload().await {
            return Err(po_i18n::t!("chat.localui.single_model_server").to_owned());
        }
        let Some(supervisor) = self.supervisor.as_mut() else {
            return Err(po_i18n::t!("chat.localui.no_managed_server").to_owned());
        };
        supervisor
            .unload_model(label)
            .await
            .map_err(|err| format!("{label}: {err}"))?;

        agent.router_mut().remove_model(label);
        self.last_used.remove(label);
        if self.active_model_id.as_deref() == Some(label) {
            self.active_model_id = None;
        }
        Ok(po_i18n::t!("chat.localui.unloaded", label = label))
    }

    /// Stops the running server and removes its model from the router.
    pub async fn deactivate(&mut self, agent: &mut Agent) -> String {
        let Some(supervisor) = self.supervisor.as_mut() else {
            return po_i18n::t!("chat.localui.no_managed_server").to_owned();
        };
        if supervisor.running().is_none() {
            return po_i18n::t!("chat.localui.no_managed_server").to_owned();
        }
        supervisor.stop().await;
        if let Some(previous) = self.active_model_id.take() {
            agent.router_mut().remove_model(&previous);
            return po_i18n::t!("chat.localui.stopped_model", model = previous);
        }
        po_i18n::t!("chat.localui.stopped").to_owned()
    }

    /// Server state on one line, for the footer of the picker.
    pub fn server_state(&self) -> String {
        match &self.supervisor {
            Some(supervisor) => match supervisor.running() {
                Some(running) => format!("{} ({})", running.model_label(), supervisor.state().label()),
                None => po_i18n::t!("chat.localui.none_started").to_owned(),
            },
            None => po_i18n::t!("chat.localui.server_missing").to_owned(),
        }
    }

    /// Current state, shown in the status bar and by the dedicated command.
    pub fn status(&self) -> String {
        let gpu = match self.hardware.best_gpu() {
            Some(gpu) => po_i18n::t!(
                "chat.localui.gpu_line",
                name = gpu.name,
                free = gpu.vram_free_mib,
                total = gpu.vram_total_mib
            ),
            None => po_i18n::t!("chat.localui.no_gpu").to_owned(),
        };
        let router = self
            .supervisor
            .as_ref()
            .is_some_and(po_local::Supervisor::is_router);
        let server_text = match self.supervisor.as_ref().and_then(|s| s.running()) {
            Some(server) => po_i18n::t!(
                "chat.localui.server_line",
                model = server.model_label(),
                state = server.state.label(),
                port = server.port,
                secs = server.uptime().as_secs(),
                mode = if router {
                    po_i18n::t!("chat.localui.router_mode")
                } else {
                    po_i18n::t!("chat.localui.single_mode")
                }
            ),
            None if self.has_server_binary() => po_i18n::t!("chat.localui.none_started").to_owned(),
            None => po_i18n::t!("chat.localui.server_missing").to_owned(),
        };
        po_i18n::t!("chat.localui.status", gpu = gpu, server = server_text, n = self.catalog.len())
    }

    /// Frees the VRAM if the server has been idle long enough.
    pub async fn reap_idle(&mut self, agent: &mut Agent) -> Option<String> {
        let supervisor = self.supervisor.as_mut()?;
        if supervisor.reap_if_idle().await {
            let previous = self.active_model_id.take()?;
            agent.router_mut().remove_model(&previous);
            return Some(po_i18n::t!("chat.localui.idle_stopped", model = previous));
        }
        None
    }

    /// Records a use, to push back the release deadline.
    pub fn touch(&mut self) {
        if let Some(server) = self.supervisor.as_mut().and_then(Supervisor::running_mut) {
            server.touch();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A manager holding a single model, with the real geometry of the machine's 27B.
    fn test_manager() -> LocalManager {
        let info = po_local::gguf::GgufInfo {
            architecture: "qwen35".into(),
            name: "Hermes3.6-35B-A3B-Uncensored-Genesis-V6-APEX-Compact".into(),
            block_count: 65,
            embedding_length: 3072,
            head_count: 24,
            head_count_kv: 4,
            context_length: 262_144,
            file_size_bytes: 8592 * 1024 * 1024,
            tensor_count: 800,
        };
        let hardware = Hardware::default();
        let plan = po_local::plan::plan(&info, &hardware, &PlanRequest::default());
        LocalManager {
            supervisor: None,
            hardware,
            catalog: vec![CatalogEntry {
                // Real name read from the machine: 52 columns, longer than many screens.
                path: "/models/Hermes3.6-35B-A3B-Uncensored-Genesis-V6-APEX-Compact.gguf".into(),
                info,
                plan,
            }],
            active_model_id: None,
            last_used: HashMap::new(),
            usage_clock: 0,
            engines: Default::default(),
        }
    }

    #[test]
    fn a_long_name_does_not_overflow_the_tabular_form() {
        po_i18n::set_lang(po_i18n::Lang::Fr);
        // 77 columns: the window of a Termius session on a phone, measured. The name
        // `Hermes3.6-35B-A3B-Uncensored-Genesis-V6-APEX-Compact` alone takes 52, and overflowed
        // the 36-column field reserved for it — the first three models wrapped in the middle
        // of a word.
        let manager = test_manager();
        for line in manager.listing_for_width(77) {
            assert!(line.chars().count() <= 77, "\"{line}\" exceeds the available width");
        }
    }

    #[test]
    fn a_truncated_name_says_so() {
        // 56 columns: the window of an Android terminal in portrait, measured from Termux.
        // A model name is necessarily cut there; it must at least be visible, or two variants
        // that differ only by their suffix would be confused.
        let manager = test_manager();
        let lines = manager.listing_for_width(56);
        assert!(lines.iter().any(|l| l.contains('…')), "no line shows that it was cut: {lines:?}");
    }

    #[test]
    fn a_wide_screen_keeps_the_tabular_form() {
        po_i18n::set_lang(po_i18n::Lang::Fr);
        let manager = test_manager();
        let lines = manager.listing_for_width(100);
        assert_eq!(lines.len(), 1, "one model, one line");
        assert!(lines[0].contains("[1]"));
        assert!(lines[0].contains("Mio"));
    }

    #[test]
    fn a_phone_screen_splits_over_two_lines() {
        po_i18n::set_lang(po_i18n::Lang::Fr);
        // 44 columns: a Termux terminal in portrait. The tabular line would wrap in the middle
        // of a word there, and the number to type would be lost.
        let manager = test_manager();
        let lines = manager.listing_for_width(44);
        assert_eq!(lines.len(), 2, "name then plan");
        assert!(lines[0].starts_with("  [1] "), "the number stays at the head");
        assert!(lines[1].contains("Mio"));
        for line in &lines {
            assert!(line.chars().count() <= 44, "\"{line}\" exceeds the available width");
        }
    }

    #[test]
    fn an_empty_catalog_gives_an_empty_list() {
        po_i18n::set_lang(po_i18n::Lang::Fr);
        let manager = LocalManager {
            supervisor: None,
            hardware: Hardware::default(),
            catalog: Vec::new(),
            active_model_id: None,
            last_used: HashMap::new(),
            usage_clock: 0,
            engines: Default::default(),
        };
        assert!(manager.listing().is_empty());
        assert!(!manager.has_server_binary());
        assert!(manager.status().contains("llama-server introuvable"));
    }

    #[test]
    fn status_reports_the_absence_of_a_gpu() {
        po_i18n::set_lang(po_i18n::Lang::Fr);
        let manager = LocalManager {
            supervisor: None,
            hardware: Hardware::default(),
            catalog: Vec::new(),
            active_model_id: None,
            last_used: HashMap::new(),
            usage_clock: 0,
            engines: Default::default(),
        };
        assert!(manager.status().contains("aucun GPU"));
    }
}
