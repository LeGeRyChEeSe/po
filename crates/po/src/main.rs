//! Po entry point.
//!
//! Three commands: `chat` drives an agent session, `local` inventories the hardware and
//! computes a launch plan, `doctor` explains what Po understood about the terminal.

mod background;

/// Held by every test that points `PO_LOCAL_PREFS` at a temporary file: the variable is global
/// to the process, and a test clearing it while another writes sent that write to the user's
/// real files (observed: `background_server.json` deleted by `cargo test`).
#[cfg(test)]
pub(crate) static PREFS_ENV: std::sync::Mutex<()> = std::sync::Mutex::new(());
mod chat;
mod commands;
mod catalog;
mod config;
mod engines_ops;
mod engines_ui;
mod github;
mod headless;
mod instructions;
mod kv;
mod labs;
mod labs_git;
mod local_ui;
mod menus;
mod profile;
mod roles;
mod setup;
mod rc;
mod sandbox;
mod service;
mod settings_panel;
mod running;
mod senses;
mod unload;
mod update;
mod voice_cmd;

use std::io::Write;

use po_term::{RenderProfile, Terminal};

fn main() -> std::process::ExitCode {
    // Without this, output redirected to a pipe closed early (`po doctor | head`) makes the
    // program panic instead of ending cleanly.
    po_term::restore_default_sigpipe();

    let args: Vec<String> = std::env::args().skip(1).collect();
    let runtime = match tokio::runtime::Builder::new_multi_thread().enable_all().build() {
        Ok(runtime) => runtime,
        Err(err) => {
            eprintln!("po: {}", po_i18n::t!("chat.main.runtime_failed", err = err));
            return std::process::ExitCode::FAILURE;
        }
    };
    match runtime.block_on(run(&args)) {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("po: {err}");
            std::process::ExitCode::FAILURE
        }
    }
}

async fn run(args: &[String]) -> Result<(), String> {
    let mut forced: Option<RenderProfile> = None;
    let mut command: Option<&str> = None;
    let mut extra: Option<String> = None;
    let mut demo = false;
    let mut budget: Option<po_router::BudgetMode> = None;
    let mut assume_yes = false;
    let mut prompt: Option<String> = None;
    let mut resume = chat::Resume::No;
    let mut assume_tty = false;
    let mut headless = false;

    let mut iter = args.iter();
    while let Some(arg) = iter.next() {
        match arg.as_str() {
            "-h" | "--help" => {
                print_help();
                return Ok(());
            }
            "-V" | "--version" => {
                println!("po {} ({}, {})", env!("CARGO_PKG_VERSION"), env!("PO_COMMIT"), env!("PO_COMMIT_DATE"));
                return Ok(());
            }
            "--render" => {
                let value = iter.next().ok_or_else(|| po_i18n::t!("chat.main.render_needs_value").to_owned())?;
                forced = Some(value.parse()?);
            }
            "--demo" => demo = true,
            "--tty" => assume_tty = true,
            // Absent from the help: what Po's service starts for the phone.
            "--headless" => headless = true,
            "--yes" | "-y" => assume_yes = true,
            "--mode" => {
                let value = iter.next().ok_or_else(|| po_i18n::t!("chat.main.mode_needs_value").to_owned())?;
                budget = Some(
                    chat::parse_budget(value)
                        .ok_or_else(|| po_i18n::t!("chat.main.unknown_mode", mode = value))?,
                );
            }
            // The work mode for this launch: auto, auto-bypass, plan, builder, bypass.
            "--work" => {
                let value = iter.next().ok_or_else(|| po_i18n::t!("chat.main.mode_needs_value").to_owned())?;
                if po_agent::work_mode::WorkMode::parse(value).is_none() {
                    return Err(po_i18n::t!("chat.main.unknown_mode", mode = value));
                }
                // Read once the session is open (chat.rs), as the variable of the same name.
                unsafe { std::env::set_var("PO_WORK_MODE", value) };
            }
            "-p" | "--prompt" => {
                let value = iter.next().ok_or_else(|| po_i18n::t!("chat.main.prompt_needs_value").to_owned())?;
                prompt = Some(value.clone());
            }
            "-c" | "--continue" => resume = chat::Resume::Last,
            "--resume" => {
                let value = iter.next().ok_or_else(|| po_i18n::t!("chat.main.resume_needs_value").to_owned())?;
                resume = chat::Resume::Id(value.clone());
            }
            other if other.starts_with('-') => return Err(po_i18n::t!("chat.main.unknown_option", option = other)),
            other if command.is_none() => command = Some(other),
            other if extra.is_none() => extra = Some(other.to_owned()),
            other => {
                if let Some(e) = extra.as_mut() {
                    e.push(' ');
                    e.push_str(other);
                } else {
                    return Err(po_i18n::t!("chat.main.unexpected_argument", arg = other));
                }
            }
        }
    }

    match command {
        None | Some("chat") => {
            // A session someone works in, not a one-off request from a script.
            if prompt.is_none() && !headless {
                update::spawn_check();
            }
            let mut terminal = Terminal::detect(forced);
            chat::run(&mut terminal, budget, assume_yes, prompt, resume, headless).await
        }
        Some("service") => service::command(extra.as_deref()).await,
        Some("update" | "mettre-a-jour") => update::command(extra.as_deref()).await,
        // Development tool, absent from the help: a session against a built-in fake server
        // (fake sub-agents, no real model) to try out the interface.
        Some("dev") => {
            let slots = extra.as_deref().and_then(|n| n.parse::<usize>().ok()).unwrap_or(4).clamp(1, 8);
            let port = sandbox::start(slots).await.map_err(|e| po_i18n::t!("chat.main.fake_server_error", err = e))?;
            let dir = sandbox::prepare(port).map_err(|e| po_i18n::t!("chat.main.sandbox_error", err = e))?;
            std::env::set_current_dir(&dir).map_err(|e| e.to_string())?;
            println!("{}", po_i18n::t!("chat.main.sandbox_banner", port = port, slots = slots));
            println!("{}\n", po_i18n::t!("chat.main.sandbox_hint"));
            let mut terminal = Terminal::detect(forced);
            chat::run(&mut terminal, budget, assume_yes, prompt, resume, false).await
        }
        Some("load" | "charger" | "start") => crate::background::load(extra.as_deref()).await,
        Some("unload" | "decharger" | "décharger" | "stop") => crate::background::unload().await,
        Some("sessions") => sessions(extra.as_deref()),
        Some("doctor") => doctor(forced, demo, assume_tty),
        Some("local") => {
            if let Some(sub) = extra.as_deref() {
                let lower = sub.to_lowercase();
                if lower == "unload" || lower == "decharger" || lower == "décharger" || lower == "stop" {
                    return crate::background::unload().await;
                }
                if lower == "load" || lower == "charger" || lower == "start" {
                    return crate::background::load(None).await;
                }
                if let Some(rest) = lower.strip_prefix("load ")
                    .or_else(|| lower.strip_prefix("charger "))
                    .or_else(|| lower.strip_prefix("start "))
                {
                    return crate::background::load(Some(rest.trim())).await;
                }
            }
            local(extra.as_deref())
        },
        Some(other) => Err(po_i18n::t!("chat.main.unknown_command", command = other)),
    }
}

/// Hardware inventory and, if a GGUF is given, the launch plan that follows from it.
fn local(model_path: Option<&str>) -> Result<(), String> {
    let hardware = po_local::Hardware::detect();

    println!("{}", po_i18n::t!("chat.local.hardware"));
    println!("{}", po_i18n::t!("chat.local.cores", n = hardware.cpu_threads));
    if hardware.ram_total_mib > 0 {
        println!("{}", po_i18n::t!("chat.local.ram", free = hardware.ram_free_mib, total = hardware.ram_total_mib));
    }
    if hardware.gpus.is_empty() {
        println!("{}", po_i18n::t!("chat.local.no_gpu"));
    }
    // A card name is long: "NVIDIA GeForce RTX 4070 SUPER (CUDA) - 405 MiB free of 12282"
    // is 85 columns, and a phone window over SSH has 77. Left to the terminal's wrapping, the
    // figure would be cut in the middle; we break the line ourselves, at the right place.
    let columns = po_term::terminal_size().0 as usize;
    for gpu in &hardware.gpus {
        let head = format!("  GPU               : {} ({})", gpu.name, gpu.backend.label());
        let memory = po_i18n::t!("chat.local.gpu_memory", free = gpu.vram_free_mib, total = gpu.vram_total_mib);
        let whole = format!("{head} {memory}");
        if po_term::width::display_width(&whole, po_term::UnicodeLevel::Bmp) <= columns {
            println!("{whole}");
        } else {
            println!("{head}");
            println!("                      {memory}");
        }
    }

    let Some(path) = model_path else {
        println!("\n{}", po_i18n::t!("chat.local.give_gguf"));
        println!("  {}", po_i18n::t!("chat.local.usage"));
        return Ok(());
    };

    let info = po_local::read_header(std::path::Path::new(path)).map_err(|e| format!("{path}: {e}"))?;

    println!("\n{}", po_i18n::t!("chat.local.model"));
    println!("{}", po_i18n::t!("chat.local.name", value = info.name));
    println!("{}", po_i18n::t!("chat.local.architecture", value = info.architecture));
    println!("{}", po_i18n::t!("chat.local.layers", value = info.block_count));
    println!("{}", po_i18n::t!("chat.local.heads", heads = info.head_count, kv = info.head_count_kv));
    println!("{}", po_i18n::t!("chat.local.trained_context", n = info.context_length));
    println!("{}", po_i18n::t!("chat.local.disk_size", n = info.file_size_bytes / (1024 * 1024)));

    let plan = po_local::plan(&info, &hardware, &po_local::PlanRequest::default());

    println!("\n{}", po_i18n::t!("chat.local.plan"));
    println!("  {}", plan.summary());
    if !plan.compromises.is_empty() {
        println!("\n{}", po_i18n::t!("chat.local.compromises"));
        for compromise in &plan.compromises {
            println!("    - {compromise}");
        }
    }
    if plan.viable {
        println!("\n  llama-server -m {path} \\\n    {}", plan.to_args().join(" "));
    } else {
        println!("\n{}", po_i18n::t!("chat.local.no_launch"));
    }
    Ok(())
}

/// Lists the saved sessions, or deletes one.
fn sessions(argument: Option<&str>) -> Result<(), String> {
    let store = po_agent::SessionStore::default_location();

    if let Some(id) = argument.and_then(|arg| arg.strip_prefix("rm:")) {
        store.delete(id).map_err(|err| err.to_string())?;
        // Its parked server cache goes with it.
        if let Some(dir) = crate::kv::register_dir() {
            po_local::kvstore::forget_everywhere(&dir, id);
        }
        println!("{}", po_i18n::t!("chat.sessions.deleted", id = id));
        return Ok(());
    }

    let sessions = store.list();
    if sessions.is_empty() {
        println!("{}", po_i18n::t!("chat.sessions.none", path = store.root().display()));
        return Ok(());
    }

    let here = std::env::current_dir().ok();
    for saved in &sessions {
        // Sessions of the current directory are the ones `--continue` will resume:
        // marking them saves comparing paths by eye.
        let marker = match &here {
            Some(path) if path.display().to_string() == saved.workspace => "*",
            _ => " ",
        };
        println!(
            "{marker} {:<20} {:<20} {:>3} {}  {}",
            saved.id,
            saved.saved_at,
            saved.exchange_count(),
            po_i18n::t!("chat.sessions.exchanges"),
            saved.title
        );
    }
    println!("\n{}", po_i18n::t!("chat.sessions.footer_continue"));
    println!("{}", po_i18n::t!("chat.sessions.footer_resume"));
    println!("{}", po_i18n::t!("chat.sessions.footer_remove"));
    Ok(())
}

fn doctor(forced: Option<RenderProfile>, demo: bool, assume_tty: bool) -> Result<(), String> {
    let terminal = if assume_tty { Terminal::detect_attached(forced) } else { Terminal::detect(forced) };
    println!("{}", terminal.report());

    if !demo {
        return Ok(());
    }

    // Check under real conditions: we take over the terminal as the session will, draw a
    // frame, then give it back. If the shell is clean afterwards - no residue, no lost
    // cursor, no broken echo - the guard does its job.
    let guard = terminal.enter().map_err(|e| po_i18n::t!("chat.doctor.take_failed", err = e))?;
    let caps = terminal.caps();
    let profile = guard.profile();
    let chars = profile.box_chars(caps);
    let width = (caps.size.0 as usize).clamp(20, 60);

    let mut out = std::io::stdout();
    let inner = width.saturating_sub(2);
    let title = po_i18n::t!("chat.doctor.title", profile = profile);
    let title = po_term::width::truncate_with_ellipsis(&title, inner, caps.unicode);
    let title = po_term::width::pad_to(&title, inner, caps.unicode);

    let top: String = std::iter::repeat_n(chars.horizontal, inner).collect();
    let _ = write!(
        out,
        "\r\n{}{top}{}\r\n{}{title}{}\r\n{}{top}{}\r\n",
        chars.top_left, chars.top_right, chars.vertical, chars.vertical, chars.bottom_left, chars.bottom_right,
    );
    let _ = write!(out, "\r\n{}\r\n", po_i18n::t!("chat.doctor.demo_window"));
    let _ = out.flush();

    std::thread::sleep(std::time::Duration::from_secs(2));
    drop(guard);

    println!("{}", po_i18n::t!("chat.doctor.restored"));
    Ok(())
}

fn print_help() {
    println!("{}", po_i18n::t!("chat.help.text", version = env!("CARGO_PKG_VERSION")));
}
