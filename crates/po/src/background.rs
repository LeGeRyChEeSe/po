//! Background server management: loading outside of a session, unloading on demand,
//! and handling session exit confirmation.

use std::io::Write;
use std::time::Duration;

use po_agent::Agent;
use po_local::BackgroundServerInfo;
use crate::engines_ui::Engines;
use crate::local_ui::LocalManager;

/// Kills a process and its child processes.
fn kill_process_tree(pid: u32) {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        let _ = std::process::Command::new("taskkill")
            .args(["/F", "/PID", &pid.to_string(), "/T"])
            .creation_flags(0x0800_0000)
            .output();
    }
    #[cfg(unix)]
    {
        let _ = std::process::Command::new("kill")
            .args(["-9", &pid.to_string()])
            .output();
    }
}

/// On Windows, finds any process listening on `port` and terminates it.
#[cfg(windows)]
fn kill_port_owner(port: u16) {
    use std::os::windows::process::CommandExt;
    if let Ok(output) = std::process::Command::new("cmd")
        .args(["/C", &format!("netstat -ano | findstr :{port}")])
        .creation_flags(0x0800_0000)
        .output()
    {
        let text = String::from_utf8_lossy(&output.stdout);
        for line in text.lines() {
            if line.contains("LISTENING")
                && let Some(pid_str) = line.split_whitespace().last()
                    && let Ok(pid) = pid_str.parse::<u32>() {
                        kill_process_tree(pid);
                    }
        }
    }
}

/// The Strata server Po started for `port`: its command line names the config copy in Po's
/// `po-strata\<port>` work folder, which only Po creates. The launcher at the top of its tree.
/// Found by command line, not by the port's owner: Windows can report the listening socket as
/// owned by PID 0 once the process that opened it handed it down (seen after a reload), and the
/// server was then left running, only emptied.
#[cfg(windows)]
fn po_started_strata_on(port: u16) -> Option<u32> {
    use std::os::windows::process::CommandExt;
    let script = format!(
        "$all = @(Get-CimInstance Win32_Process | Where-Object {{ $_.CommandLine -match 'po-strata[\\\\/]{port}[\\\\/]' }}); \
         $ids = $all | ForEach-Object {{ $_.ProcessId }}; \
         $all | Where-Object {{ $ids -notcontains $_.ParentProcessId }} | Select-Object -First 1 -ExpandProperty ProcessId"
    );
    let output = std::process::Command::new("powershell")
        .args(["-NoProfile", "-Command", &script])
        .creation_flags(0x0800_0000)
        .output()
        .ok()?;
    String::from_utf8_lossy(&output.stdout).trim().parse().ok()
}

/// Unloads the model left in the background, without printing; with `probe_known_ports`, also
/// what answers on Strata's and Po's llama.cpp ports (only `po unload`, asked by the user: a
/// session that ends must not unload a model another session or the user started).
/// Returns true if at least one server/model was unloaded.
pub async fn unload_internal(probe_known_ports: bool) -> bool {
    let mut any = false;

    // A record whose server no longer answers is only forgotten: after a reboot its pid may
    // belong to any other program.
    if let Some(info) = BackgroundServerInfo::load() {
        any = true;
        let alive = info.is_alive().await;
        if alive && info.engine == "strata" {
            // Asked to unload first, then stopped: only the tree Po started, never every
            // strata.exe of the machine (the user may run their own).
            let client = reqwest::Client::builder().timeout(Duration::from_secs(2)).build().ok();
            if let Some(c) = client {
                let _ = c.post(format!("http://127.0.0.1:{}/v1/unload", info.port)).json(&serde_json::json!({})).send().await;
            }
            if let Some(pid) = info.pid {
                kill_process_tree(pid);
            }
        } else if alive && info.engine == "llama.cpp" {
            if let Some(pid) = info.pid {
                kill_process_tree(pid);
            }
            #[cfg(windows)]
            {
                use std::os::windows::process::CommandExt;
                let _ = std::process::Command::new("wsl.exe")
                    .args(["--exec", "pkill", "-f", &format!("llama-server.*--port {}", info.port)])
                    .creation_flags(0x0800_0000)
                    .output();
                kill_port_owner(info.port);
            }
            #[cfg(unix)]
            {
                let _ = std::process::Command::new("pkill")
                    .args(["-f", &format!("llama-server.*--port {}", info.port)])
                    .output();
            }
        }
        BackgroundServerInfo::forget();
    }

    if !probe_known_ports {
        return any;
    }

    // Check if Strata answers on 8080:
    let client = reqwest::Client::builder().timeout(Duration::from_millis(500)).build().ok();
    if let Some(c) = &client
        && let Ok(res) = c.get("http://127.0.0.1:8080/v1/status").send().await
            && let Ok(status) = res.json::<serde_json::Value>().await
                && status["loaded"].as_bool().unwrap_or(false) {
                    any = true;
                    let _ = c.post("http://127.0.0.1:8080/v1/unload").json(&serde_json::json!({})).send().await;
                }

    // A Strata Po started itself (its config copy lives in Po's `po-strata` folder) is stopped,
    // not only unloaded, even when its record was lost: `po unload` must free the machine. One
    // the user started stays running, emptied.
    #[cfg(windows)]
    if let Some(pid) = po_started_strata_on(8080) {
        any = true;
        kill_process_tree(pid);
    }

    // Check if llama.cpp answers on 18080:
    if let Some(c) = &client
        && let Ok(res) = c.get("http://127.0.0.1:18080/health").send().await
            && res.status().is_success() {
                any = true;
                #[cfg(windows)]
                {
                    use std::os::windows::process::CommandExt;
                    let _ = std::process::Command::new("wsl.exe")
                        .args(["--exec", "pkill", "-f", "llama-server.*--port 18080"])
                        .creation_flags(0x0800_0000)
                        .output();
                    kill_port_owner(18080);
                }
                #[cfg(unix)]
                {
                    let _ = std::process::Command::new("pkill")
                        .args(["-f", "llama-server.*--port 18080"])
                        .output();
                }
            }

    any
}

/// Unloads any active local model from outside a session.
pub async fn unload() -> Result<(), String> {
    let unloaded = unload_internal(true).await;
    if unloaded {
        println!("{}", po_i18n::t!("chat.background.unloaded"));
    } else {
        println!("{}", po_i18n::t!("chat.background.none_running"));
    }
    Ok(())
}

/// Loads a model in the background outside of a session.
pub async fn load(target: Option<&str>) -> Result<(), String> {
    let mut local = LocalManager::detect();
    let mut engines = Engines::default();
    crate::engines_ui::detect(&mut local, &mut engines).await;
    let all = crate::engines_ops::all_models(&local, engines.report.as_ref());

    let Some(target) = target.map(str::trim).filter(|s| !s.is_empty()) else {
        println!("{}", po_i18n::t!("chat.background.available"));
        if all.is_empty() {
            println!("  {}", po_i18n::t!("cmd.local.none"));
        } else {
            for (i, m) in all.iter().enumerate() {
                println!("  [{}] {}", i + 1, m.name());
            }
        }
        println!("\n{}", po_i18n::t!("chat.background.usage_load"));
        return Ok(());
    };

    let matched = if let Ok(idx) = target.parse::<usize>() {
        if idx >= 1 && idx <= all.len() {
            vec![all[idx - 1].clone()]
        } else {
            Vec::new()
        }
    } else {
        crate::engines_ops::find(&all, target)
    };

    if matched.is_empty() {
        println!("{}", po_i18n::t!("chat.background.not_found", target = target));
        println!("\n{}", po_i18n::t!("chat.background.available"));
        for (i, m) in all.iter().enumerate() {
            println!("  [{}] {}", i + 1, m.name());
        }
        return Ok(());
    }

    if matched.len() > 1 {
        println!("{}", po_i18n::t!("chat.background.ambiguous", target = target));
        for m in &matched {
            println!("  - {}", m.name());
        }
        return Ok(());
    }

    let model = &matched[0];
    let model_name = model.name().to_owned();

    // Check if this model is already active:
    if let Some(info) = BackgroundServerInfo::load()
        && info.model == model_name && info.is_alive().await {
            println!("{}", po_i18n::t!("chat.background.already_running", model = model_name, port = info.port));
            println!("{}", po_i18n::t!("chat.background.unload_hint"));
            return Ok(());
        }

    // Stop any previous model:
    if BackgroundServerInfo::load().is_some() {
        println!("{}", po_i18n::t!("chat.background.stopping_prev"));
        let _ = unload_internal(false).await;
    }

    println!("{}", po_i18n::t!("chat.background.loading", model = model_name));
    let start_time = std::time::Instant::now();

    match model {
        crate::engines_ops::Model::Llama(idx, label) => {
            let Some(entry) = local.catalog.get(*idx) else {
                return Err(po_i18n::t!("chat.localui.no_such_model", index = idx + 1).to_owned());
            };
            if !entry.plan.viable {
                return Err(po_i18n::t!("chat.localui.does_not_fit_now", label = label).to_owned());
            }
            let Some(supervisor) = local.supervisor.as_mut() else {
                return Err(po_i18n::t!("chat.localui.server_missing_install").to_owned());
            };
            let port = supervisor.start(&entry.path, &entry.plan).await.map_err(|e| format!("{label}: {e}"))?;
            let pid = supervisor.running().and_then(|r| r.pid());
            supervisor.detach();
            let info = BackgroundServerInfo {
                pid,
                port,
                engine: "llama.cpp".to_string(),
                model: label.clone(),
                endpoint: format!("http://127.0.0.1:{port}/v1"),
                started_at: std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_secs(),
            };
            let _ = info.save();
            let secs = format!("{:.1}", start_time.elapsed().as_secs_f64());
            println!("{}", po_i18n::t!("chat.background.loaded", model = label, port = port, secs = secs));
            println!("{}", po_i18n::t!("chat.background.unload_hint"));
        }
        crate::engines_ops::Model::Strata(idx, name) => {
            let Some(report) = engines.report.as_ref() else {
                return Err(po_i18n::t!("engines.strata.missing").to_owned());
            };
            let Some(install) = report.strata_install() else {
                return Err(po_i18n::t!("engines.strata.missing").to_owned());
            };
            let Some(config) = install.configs.get(*idx) else {
                return Err(format!("Strata config not found at index {idx}"));
            };
            let prefs = po_local::prefs::LocalPrefs::load();
            let profile = prefs.model(name);
            let port = config.port;
            let overrides = crate::engines_ops::strata_overrides(&profile, prefs.game_mode);
            po_local::strata::preflight(config, port).await.map_err(|e| e.to_string())?;
            let mut server = po_local::strata::start_watch(install, config, port, po_local::strata::READY_TIMEOUT, &overrides, None)
                .await
                .map_err(|e| e.to_string())?;
            let pid = server.pid();
            server.detach();
            let info = BackgroundServerInfo {
                pid,
                port,
                engine: "strata".to_string(),
                model: name.clone(),
                endpoint: format!("http://127.0.0.1:{port}/v1"),
                started_at: std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_secs(),
            };
            let _ = info.save();
            let secs = format!("{:.1}", start_time.elapsed().as_secs_f64());
            println!("{}", po_i18n::t!("chat.background.loaded", model = name, port = port, secs = secs));
            println!("{}", po_i18n::t!("chat.background.unload_hint"));
        }
    }
    Ok(())
}

/// Handles session exit: asks the user whether to unload or keep the model in memory.
pub async fn handle_session_exit(
    local: &mut LocalManager,
    agent: &mut Agent,
    interactive: bool,
) -> Result<(), String> {
    let mut model_name: Option<String> = None;
    let mut engine: Option<&'static str> = None;
    let mut port: u16 = 18080;
    let mut pid: Option<u32> = None;

    if let Some(s) = local.supervisor.as_ref().and_then(|s| s.running()) {
        model_name = Some(s.model_label());
        engine = Some("llama.cpp");
        port = s.port;
        pid = s.pid();
    } else if let Some(s) = local.engines.strata_server.as_ref() {
        model_name = Some(s.config.model_name.clone());
        engine = Some("strata");
        port = s.port;
        pid = s.pid();
    } else if let Some(id) = &local.active_model_id {
        model_name = Some(id.clone());
    } else if let Some(bg) = BackgroundServerInfo::load()
        && bg.is_alive().await {
            model_name = Some(bg.model.clone());
            engine = if bg.engine == "strata" { Some("strata") } else { Some("llama.cpp") };
            port = bg.port;
            pid = bg.pid;
        }

    let Some(name) = model_name else {
        return Ok(());
    };

    let is_background = BackgroundServerInfo::load().is_some_and(|b| b.model == name);

    // Already kept in the background (`po load`, or "keep" chosen at an earlier exit): nobody to
    // ask, so it outlives the session as meant. With someone at the keyboard the question is still
    // asked, keeping as the default: skipping it there meant that answering "keep" once silenced
    // the question for good (observed 2026-11-10).
    if is_background && !interactive {
        local.detach();
        return Ok(());
    }

    // Nobody to ask: a model this session started stops with it, as before.
    if !interactive {
        local.deactivate(agent).await;
        if let Some(server) = local.engines.strata_server.take() {
            server.stop().await;
        }
        return Ok(());
    }

    let default_choice = if is_background { 2 } else { 1 };
    let prompt_name = name.clone();

    let choice = tokio::task::spawn_blocking(move || {
        // From the cursor down, what the full screen left is cleared: the question was printed
        // over the input box otherwise.
        print!("[J");
        println!();
        println!("  {}", po_i18n::t!("chat.exit.prompt_title", model = &prompt_name));
        println!("  {}", po_i18n::t!("chat.exit.prompt_question"));
        println!("  {}", po_i18n::t!("chat.exit.prompt_hint"));
        println!();
        println!("    1. {}", po_i18n::t!("chat.exit.option_unload"));
        println!("    2. {}", po_i18n::t!("chat.exit.option_keep"));
        println!();
        print!("  {} [{default_choice}] : ", po_i18n::t!("chat.exit.choice"));
        let _ = std::io::stdout().flush();
        let mut line = String::new();
        let _ = std::io::stdin().read_line(&mut line);
        let trimmed = line.trim().to_lowercase();
        if trimmed.is_empty() {
            default_choice
        } else if trimmed == "2" || trimmed == "g" || trimmed == "garder" || trimmed == "k" || trimmed == "keep" {
            2
        } else {
            1
        }
    })
    .await
    .unwrap_or(default_choice);

    if choice == 2 && is_background {
        // Its record already says where it runs: the fallbacks above (a default port, no pid)
        // must not overwrite it.
        local.detach();
        println!("  {}", po_i18n::t!("chat.exit.kept", model = &name));
        println!("  {}", po_i18n::t!("chat.exit.kept_hint"));
    } else if choice == 2 {
        local.detach();
        let info = BackgroundServerInfo {
            pid,
            port,
            engine: engine.unwrap_or("llama.cpp").to_string(),
            model: name.clone(),
            endpoint: format!("http://127.0.0.1:{port}/v1"),
            started_at: std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_secs(),
        };
        let _ = info.save();
        println!("  {}", po_i18n::t!("chat.exit.kept", model = &name));
        println!("  {}", po_i18n::t!("chat.exit.kept_hint"));
    } else {
        local.deactivate(agent).await;
        if let Some(server) = local.engines.strata_server.take() {
            server.stop().await;
        }
        unload_internal(false).await;
        println!("  {}", po_i18n::t!("chat.exit.unloaded", model = &name));
    }

    Ok(())
}

#[cfg(test)]
// The lock only serialises the tests that share the prefs file; they run on one thread.
#[allow(clippy::await_holding_lock)]
mod tests {
    use super::*;

    fn test_agent() -> Agent {
        let dir = std::env::temp_dir();
        let router = po_router::Router::new(Vec::new(), po_router::BudgetMode::Balanced);
        let ctx = po_tools::ToolContext::new(po_tools::Workspace::new(&dir));
        Agent::new(router, po_tools::Registry::with_builtins(), ctx, "system", std::sync::Arc::new(po_agent::ReadOnly))
    }

    #[tokio::test]
    async fn exit_when_no_model_running_is_noop() {
        let mut local = LocalManager::empty();
        let mut agent = test_agent();
        assert!(handle_session_exit(&mut local, &mut agent, false).await.is_ok());
    }

    #[tokio::test]
    async fn exit_non_interactive_keeps_background_model() {
        let _guard = crate::PREFS_ENV.lock().unwrap_or_else(|e| e.into_inner());
        let dir = tempfile::tempdir().unwrap();
        let prefs_file = dir.path().join("local.json");
        unsafe { std::env::set_var("PO_LOCAL_PREFS", &prefs_file); }

        let bg = BackgroundServerInfo {
            pid: Some(999999),
            port: 18080,
            engine: "llama.cpp".to_string(),
            model: "test-bg-model".to_string(),
            endpoint: "http://127.0.0.1:18080/v1".to_string(),
            started_at: 100,
        };
        bg.save().unwrap();

        let mut local = LocalManager::empty();
        local.active_model_id = Some("test-bg-model".to_string());
        let mut agent = test_agent();

        let res = handle_session_exit(&mut local, &mut agent, false).await;
        assert!(res.is_ok());
        // Background file is still there because it was preserved
        assert!(BackgroundServerInfo::load().is_some());

        BackgroundServerInfo::forget();
        unsafe { std::env::remove_var("PO_LOCAL_PREFS"); }
    }

    #[tokio::test]
    async fn unload_cleans_background_server_info() {
        let _guard = crate::PREFS_ENV.lock().unwrap_or_else(|e| e.into_inner());
        let dir = tempfile::tempdir().unwrap();
        let prefs_file = dir.path().join("local.json");
        unsafe { std::env::set_var("PO_LOCAL_PREFS", &prefs_file); }

        let bg = BackgroundServerInfo {
            pid: None,
            port: 19999, // inactive port
            engine: "llama.cpp".to_string(),
            model: "dummy".to_string(),
            endpoint: "http://127.0.0.1:19999/v1".to_string(),
            started_at: 100,
        };
        bg.save().unwrap();
        assert!(BackgroundServerInfo::load().is_some());

        let unloaded = unload_internal(false).await;
        assert!(unloaded);
        assert!(BackgroundServerInfo::load().is_none());

        unsafe { std::env::remove_var("PO_LOCAL_PREFS"); }
    }
}
