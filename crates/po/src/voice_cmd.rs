//! `/voix` (voice): settings and installation of voice dictation (whisper.cpp).
//!
//! Recording and transcription live in `po_tui::voice`; here is what can be configured and
//! downloaded. Nothing is downloaded without `/voix installer`.

use std::path::Path;

use po_i18n::t;
use po_tui::voice::{self, Settings};

use crate::commands::Reply;

pub async fn command(argument: Option<&str>, rest: &[&str], reply: &mut Reply) {
    let mut settings = Settings::load();
    match argument {
        None | Some("etat" | "état" | "status") => describe(&settings, reply).await,
        Some("on" | "oui") => {
            settings.enabled = true;
            save(&settings, reply);
            match settings.missing() {
                Some(missing) => reply.warn(t!("cmd.voice.enabled_but", missing = missing)),
                None => reply.plain(t!("cmd.voice.enabled")),
            }
        }
        Some("off" | "non") => {
            settings.enabled = false;
            save(&settings, reply);
            reply.plain(t!("cmd.voice.disabled"));
        }
        Some("modele" | "modèle" | "model") => match rest.first().copied() {
            Some(model @ ("small" | "base" | "turbo")) => {
                settings.model = model.to_owned();
                save(&settings, reply);
                reply.plain(t!("cmd.voice.model_set", model = model));
                if settings.model_path().is_none() {
                    reply.warn(t!("cmd.voice.model_missing"));
                }
            }
            _ => reply.warn(t!("cmd.voice.models")),
        },
        Some("moteur" | "engine") => match rest.first().copied() {
            Some("cpu" | "processeur" | "processor") => {
                settings.gpu = None;
                save(&settings, reply);
                reply.plain(t!("cmd.voice.engine_cpu"));
            }
            Some("gpu") => {
                let chosen = match rest.get(1).and_then(|n| n.parse::<u32>().ok()) {
                    Some(index) => Some(index),
                    None => freest_gpu().await,
                };
                match chosen {
                    Some(index) => {
                        settings.gpu = Some(index);
                        save(&settings, reply);
                        reply.plain(t!("cmd.voice.engine_gpu", index = index));
                        if settings.whisper_cli().is_none() {
                            reply.warn(t!("cmd.voice.cuda_missing"));
                        }
                    }
                    None => reply.warn(t!("cmd.voice.no_gpu")),
                }
            }
            _ => reply.warn(t!("cmd.voice.usage_engine")),
        },
        Some("langue" | "language") => match rest.first() {
            Some(language) => {
                settings.language = language.to_string();
                save(&settings, reply);
                reply.plain(t!("cmd.voice.language_set", language = language));
            }
            None => reply.warn(t!("cmd.voice.usage_language")),
        },
        Some("micros" | "mics") => {
            let mics = voice::microphones().await;
            if mics.is_empty() {
                reply.warn(t!("cmd.voice.no_mic"));
            }
            let best = voice::best_microphone(&mics);
            for mic in mics {
                let mark = if settings.microphone.as_deref() == Some(mic.as_str()) {
                    t!("cmd.voice.mic_kept")
                } else if settings.microphone.is_none() && best.as_deref() == Some(mic.as_str()) {
                    t!("cmd.voice.mic_auto")
                } else {
                    ""
                };
                reply.plain(format!("{mic}{mark}"));
            }
        }
        Some("micro") => {
            let name = rest.join(" ");
            settings.microphone = (!name.is_empty() && name != "auto").then_some(name);
            save(&settings, reply);
            reply.plain(t!("cmd.voice.mic_set", mic = settings.microphone.as_deref().unwrap_or(t!("cmd.voice.automatic"))));
        }
        Some("installer" | "install") => install(&settings, reply).await,
        Some("tester" | "test") => match rest.first() {
            Some(file) => match voice::transcribe(&settings, Path::new(&rest.join(" "))).await {
                Ok(text) if text.is_empty() => reply.warn(t!("cmd.voice.nothing_heard", file = file)),
                Ok(text) => reply.plain(t!("cmd.voice.transcribed", text = text)),
                Err(err) => reply.warn(err),
            },
            None => reply.warn(t!("cmd.voice.usage_test")),
        },
        Some(other) => reply.warn(t!("cmd.voice.unknown_sub", sub = other)),
    }
}

fn save(settings: &Settings, reply: &mut Reply) {
    if let Err(err) = settings.save() {
        reply.warn(t!("cmd.voice.not_saved", err = err));
    }
}

async fn describe(settings: &Settings, reply: &mut Reply) {
    reply.field(t!("cmd.voice.f_dictation"), if settings.enabled { t!("cmd.voice.on") } else { t!("cmd.voice.off_hint") });
    reply.field(
        t!("cmd.voice.f_engine"),
        settings.gpu.map(|g| t!("cmd.voice.gpu_n", index = g)).unwrap_or_else(|| t!("cmd.voice.cpu").into()),
    );
    reply.field(t!("cmd.voice.f_model"), &settings.model);
    reply.field(t!("cmd.voice.f_language"), &settings.language);
    reply.field(t!("cmd.voice.f_mic"), settings.microphone.as_deref().unwrap_or(t!("cmd.voice.mic_default")));
    match settings.missing() {
        Some(missing) => reply.warn(missing),
        None => reply.dim(t!("cmd.voice.ready")),
    }
}

/// NVIDIA card with the most free memory: the one the local model is not using.
async fn freest_gpu() -> Option<u32> {
    let output = tokio::process::Command::new("nvidia-smi")
        .args(["--query-gpu=index,memory.free", "--format=csv,noheader,nounits"])
        .output()
        .await
        .ok()?;
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter_map(|l| {
            let (index, free) = l.split_once(',')?;
            Some((index.trim().parse::<u32>().ok()?, free.trim().parse::<u64>().ok()?))
        })
        .max_by_key(|(_, free)| *free)
        .map(|(index, _)| index)
}

/// Downloads the whisper.cpp binary of the chosen engine and the model, if missing.
async fn install(settings: &Settings, reply: &mut Reply) {
    let Some(dir) = voice::install_dir() else {
        reply.warn(t!("cmd.voice.no_install_dir"));
        return;
    };
    let Some((archive_url, model_url)) = voice::downloads(settings) else {
        reply.warn(t!("cmd.voice.no_binary"));
        return;
    };
    let target = dir.join(settings.flavor());
    if settings.whisper_cli().is_none() {
        let archive = dir.join(archive_url.rsplit('/').next().unwrap_or("whisper.zip"));
        if let Err(err) = download(&archive_url, &archive).await {
            reply.warn(t!("cmd.voice.download_whisper", err = err));
            return;
        }
        if let Err(err) = extract(&archive, &target).await {
            reply.warn(t!("cmd.voice.extract_failed", err = err));
            return;
        }
        let _ = std::fs::remove_file(&archive);
        reply.plain(t!(
            "cmd.voice.whisper_installed",
            release = voice::WHISPER_RELEASE,
            flavor = settings.flavor(),
            path = target.display()
        ));
    } else {
        reply.dim(t!("cmd.voice.whisper_present"));
    }
    if settings.model_path().is_none() {
        let model = dir.join("models").join(settings.model_file());
        if let Err(err) = download(&model_url, &model).await {
            reply.warn(t!("cmd.voice.download_model", err = err));
            return;
        }
        reply.plain(t!("cmd.voice.model_downloaded", model = settings.model));
    } else {
        reply.dim(t!("cmd.voice.model_present", model = settings.model));
    }
    match settings.missing() {
        None => reply.plain(if settings.enabled { t!("cmd.voice.ready_short") } else { t!("cmd.voice.ready_enable") }),
        Some(missing) => reply.warn(missing),
    }
}

/// Downloads into a temporary file, renamed at the end: an interruption does not leave a
/// truncated file that Po would then take for good.
async fn download(url: &str, path: &Path) -> Result<(), String> {
    use futures_util::StreamExt;
    use tokio::io::AsyncWriteExt;
    if let Some(parent) = path.parent() {
        tokio::fs::create_dir_all(parent).await.map_err(|e| e.to_string())?;
    }
    let response = reqwest::get(url).await.map_err(|e| e.to_string())?;
    if !response.status().is_success() {
        return Err(format!("{url}: {}", response.status()));
    }
    let partial = path.with_extension("part");
    let mut file = tokio::fs::File::create(&partial).await.map_err(|e| e.to_string())?;
    let mut stream = response.bytes_stream();
    while let Some(chunk) = stream.next().await {
        file.write_all(&chunk.map_err(|e| e.to_string())?).await.map_err(|e| e.to_string())?;
    }
    file.flush().await.map_err(|e| e.to_string())?;
    drop(file);
    tokio::fs::rename(&partial, path).await.map_err(|e| e.to_string())
}

/// Extracts the archive, then moves the contents of `Release/` (Windows) or `build/bin/` up to
/// the root of the folder: `whisper-cli` is then found directly there.
async fn extract(archive: &Path, target: &Path) -> Result<(), String> {
    let _ = tokio::fs::remove_dir_all(target).await;
    tokio::fs::create_dir_all(target).await.map_err(|e| e.to_string())?;
    // `tar` can read zip files on Windows 10 and later, and everywhere else.
    let status = tokio::process::Command::new("tar")
        .arg("-xf")
        .arg(archive)
        .arg("-C")
        .arg(target)
        .status()
        .await
        .map_err(|e| format!("tar: {e}"))?;
    if !status.success() {
        return Err(t!("cmd.voice.tar_failed").into());
    }
    let exe = if cfg!(windows) { "whisper-cli.exe" } else { "whisper-cli" };
    if target.join(exe).is_file() {
        return Ok(());
    }
    let inner = find_dir_with(target, exe).ok_or_else(|| t!("cmd.voice.not_in_archive").to_owned())?;
    for entry in std::fs::read_dir(&inner).map_err(|e| e.to_string())?.flatten() {
        let _ = std::fs::rename(entry.path(), target.join(entry.file_name()));
    }
    Ok(())
}

fn find_dir_with(root: &Path, file: &str) -> Option<std::path::PathBuf> {
    for entry in std::fs::read_dir(root).ok()?.flatten() {
        let path = entry.path();
        if path.is_dir() {
            if path.join(file).is_file() {
                return Some(path);
            }
            if let Some(found) = find_dir_with(&path, file) {
                return Some(found);
            }
        }
    }
    None
}
