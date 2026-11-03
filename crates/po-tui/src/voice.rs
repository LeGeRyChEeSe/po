//! Voice dictation: the microphone of the machine Po runs on, transcribed by whisper.cpp.
//!
//! Disabled by default. Once enabled (`/voice on`, `/voice install`), a "dictate" button appears
//! under the input: one touch starts the recording, a second one stops it, and the transcribed
//! text is inserted into the input without being sent - it is reread before sending.
//!
//! Recording goes through ffmpeg and transcription through `whisper-cli`, two programs launched
//! for the length of one sentence: nothing stays in memory between two dictations, which
//! matters on a machine whose memory is already used by the local model. Measured on a Ryzen 5
//! 5600X for a 6 s sentence: `small` 3.5 s on the CPU, `large-v3-turbo` 2 s on an RTX 3070 Ti
//! (12 s on the CPU).

use std::path::PathBuf;
use std::process::Stdio;
use std::time::Instant;

use serde_json::{Value, json};
use tokio::io::AsyncWriteExt;

/// Version of whisper.cpp downloaded by `/voice install`.
pub const WHISPER_RELEASE: &str = "b5454";

/// Dictation settings, kept from one session to the next.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Settings {
    pub enabled: bool,
    /// Graphics card (`nvidia-smi` index); `None`: the CPU.
    pub gpu: Option<u32>,
    /// `small`, `base` or `turbo`.
    pub model: String,
    /// Spoken language (`fr`), or `auto`.
    pub language: String,
    /// Microphone name; `None`: the most likely one.
    pub microphone: Option<String>,
}

impl Default for Settings {
    fn default() -> Self {
        Self { enabled: false, gpu: None, model: "small".into(), language: "fr".into(), microphone: None }
    }
}

/// Po's folder in the user's profile (`%APPDATA%\po`, `~/.config/po`).
fn config_dir() -> Option<PathBuf> {
    let base = std::env::var_os("APPDATA")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))?;
    Some(base.join("po"))
}

/// Where the downloaded binaries and models live (`%LOCALAPPDATA%\po\whisper`).
pub fn install_dir() -> Option<PathBuf> {
    let base = std::env::var_os("LOCALAPPDATA")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local").join("share")))?;
    Some(base.join("po").join("whisper"))
}

impl Settings {
    pub fn load() -> Self {
        let Some(text) = config_dir().and_then(|d| std::fs::read_to_string(d.join("voice.json")).ok()) else {
            return Self::default();
        };
        let Ok(value) = serde_json::from_str::<Value>(&text) else { return Self::default() };
        let default = Self::default();
        Self {
            enabled: value["enabled"].as_bool().unwrap_or(false),
            gpu: value["gpu"].as_u64().map(|g| g as u32),
            model: value["model"].as_str().unwrap_or(&default.model).to_owned(),
            language: value["language"].as_str().unwrap_or(&default.language).to_owned(),
            microphone: value["microphone"].as_str().map(str::to_owned),
        }
    }

    pub fn save(&self) -> std::io::Result<()> {
        let dir = config_dir().ok_or_else(|| std::io::Error::other(po_i18n::t!("tui.voice.no_config_dir")))?;
        std::fs::create_dir_all(&dir)?;
        let value = json!({
            "enabled": self.enabled,
            "gpu": self.gpu,
            "model": self.model,
            "language": self.language,
            "microphone": self.microphone,
        });
        std::fs::write(dir.join("voice.json"), serde_json::to_string_pretty(&value).unwrap_or_default())
    }

    /// File of the selected whisper.cpp model.
    pub fn model_file(&self) -> String {
        model_file(&self.model)
    }

    /// Binary folder: `cuda` for a graphics card, `cpu` otherwise.
    pub fn flavor(&self) -> &'static str {
        if self.gpu.is_some() { "cuda" } else { "cpu" }
    }

    /// `whisper-cli` to use: the one Po installed, otherwise the one on the PATH.
    pub fn whisper_cli(&self) -> Option<PathBuf> {
        let exe = if cfg!(windows) { "whisper-cli.exe" } else { "whisper-cli" };
        install_dir()
            .map(|d| d.join(self.flavor()).join(exe))
            .filter(|p| p.is_file())
            .or_else(|| which(exe))
    }

    pub fn model_path(&self) -> Option<PathBuf> {
        install_dir().map(|d| d.join("models").join(self.model_file())).filter(|p| p.is_file())
    }

    /// What is missing to dictate, if anything is missing.
    pub fn missing(&self) -> Option<String> {
        if which(if cfg!(windows) { "ffmpeg.exe" } else { "ffmpeg" }).is_none() {
            return Some(po_i18n::t!("tui.voice.no_ffmpeg").into());
        }
        if self.whisper_cli().is_none() {
            return Some(po_i18n::t!("tui.voice.no_whisper", flavor = self.flavor()));
        }
        if self.model_path().is_none() {
            return Some(po_i18n::t!("tui.voice.no_model", model = self.model));
        }
        None
    }
}

pub fn model_file(model: &str) -> String {
    match model {
        "turbo" => "ggml-large-v3-turbo-q5_0.bin".into(),
        "base" => "ggml-base-q5_1.bin".into(),
        _ => "ggml-small-q5_1.bin".into(),
    }
}

/// Download addresses: binary archive and model.
pub fn downloads(settings: &Settings) -> Option<(String, String)> {
    let archive = match (settings.flavor(), cfg!(windows)) {
        ("cuda", true) => "whisper-bin-win-cuda-12.4.0-x64.zip",
        ("cpu", true) => "whisper-bin-x64.zip",
        ("cpu", false) if cfg!(target_os = "linux") => "whisper-bin-ubuntu-x64.tar.gz",
        _ => return None,
    };
    Some((
        format!("https://github.com/ggml-org/whisper.cpp/releases/download/{WHISPER_RELEASE}/{archive}"),
        format!("https://huggingface.co/ggerganov/whisper.cpp/resolve/main/{}", settings.model_file()),
    ))
}

fn which(exe: &str) -> Option<PathBuf> {
    std::env::split_paths(&std::env::var_os("PATH")?).map(|dir| dir.join(exe)).find(|p| p.is_file())
}

/// Microphones seen by ffmpeg (DirectShow on Windows).
pub async fn microphones() -> Vec<String> {
    if !cfg!(windows) {
        return vec!["default".into()];
    }
    let output = tokio::process::Command::new("ffmpeg")
        .args(["-hide_banner", "-list_devices", "true", "-f", "dshow", "-i", "dummy"])
        .stdin(Stdio::null())
        .output()
        .await;
    match output {
        Ok(output) => parse_devices(&String::from_utf8_lossy(&output.stderr)),
        Err(_) => Vec::new(),
    }
}

/// Names of the audio inputs of an ffmpeg DirectShow listing.
pub fn parse_devices(listing: &str) -> Vec<String> {
    listing
        .lines()
        .filter(|l| l.trim_end().ends_with("(audio)"))
        .filter_map(|l| {
            let start = l.find('"')? + 1;
            let end = start + l[start..].find('"')?;
            Some(l[start..end].to_owned())
        })
        .collect()
}

/// The most likely microphone: a headset or a mic rather than a game controller, which often
/// announces one (observed: the PS5 controller came first).
pub fn best_microphone(devices: &[String]) -> Option<String> {
    let score = |name: &str| {
        let lower = name.to_lowercase();
        let mut score = 0;
        for good in ["micro", "mic", "casque", "headset", "headphone", "usb", "audio"] {
            if lower.contains(good) {
                score += 2;
            }
        }
        for bad in ["manette", "controller", "dualsense", "xbox", "stereo mix", "mixage"] {
            if lower.contains(bad) {
                score -= 10;
            }
        }
        score
    };
    devices.iter().max_by_key(|d| score(d)).cloned()
}

/// A recording in progress.
pub struct Recording {
    child: tokio::process::Child,
    pub path: PathBuf,
    pub started: Instant,
}

/// Starts recording the microphone into a 16 kHz mono WAV file, whisper's format.
pub async fn start(settings: &Settings) -> Result<Recording, String> {
    if let Some(missing) = settings.missing() {
        return Err(missing);
    }
    let path = std::env::temp_dir().join(format!("po-dictation-{}.wav", std::process::id()));
    let mut command = tokio::process::Command::new("ffmpeg");
    command.args(["-hide_banner", "-loglevel", "error", "-y"]);
    if cfg!(windows) {
        let microphone = match &settings.microphone {
            Some(name) => name.clone(),
            None => best_microphone(&microphones().await).ok_or(po_i18n::t!("tui.voice.no_microphone"))?,
        };
        command.args(["-f", "dshow", "-i", &format!("audio={microphone}")]);
    } else if cfg!(target_os = "macos") {
        command.args(["-f", "avfoundation", "-i", ":0"]);
    } else {
        command.args(["-f", "pulse", "-i", "default"]);
    }
    command.args(["-ac", "1", "-ar", "16000"]).arg(&path);
    let child = command
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .map_err(|e| format!("ffmpeg: {e}"))?;
    Ok(Recording { child, path, started: Instant::now() })
}

impl Recording {
    /// Stops the recording cleanly ("q": ffmpeg closes the file) and transcribes it.
    pub async fn finish(mut self, settings: &Settings) -> Result<String, String> {
        if let Some(stdin) = self.child.stdin.as_mut() {
            let _ = stdin.write_all(b"q").await;
        }
        match tokio::time::timeout(std::time::Duration::from_secs(5), self.child.wait()).await {
            Ok(_) => {}
            Err(_) => {
                let _ = self.child.kill().await;
            }
        }
        // On silence, whisper readily invents a sentence ("Sous-titrage Société Radio-Canada",
        // observed): we only transcribe if there was speech.
        let level = std::fs::read(&self.path).ok().and_then(|bytes| speech_level(&bytes));
        let text = match level {
            Some(level) if level < SPEECH_LEVEL => Ok(String::new()),
            _ => transcribe(settings, &self.path).await,
        };
        let _ = std::fs::remove_file(&self.path);
        text
    }
}

/// Level below which a recording is considered silent: the loudest 30 ms window. Measured: 0
/// for a muted mic, 0.22 for a normally spoken sentence.
const SPEECH_LEVEL: f32 = 0.008;

/// Level of the loudest 30 ms window of a 16-bit PCM WAV (0 to 1).
pub fn speech_level(wav: &[u8]) -> Option<f32> {
    // RIFF chunks follow one another: we skip to `data` (ffmpeg writes a LIST chunk before).
    let mut at = 12;
    let data = loop {
        let header = wav.get(at..at + 8)?;
        let size = u32::from_le_bytes(header[4..8].try_into().ok()?) as usize;
        if &header[..4] == b"data" {
            break wav.get(at + 8..(at + 8 + size).min(wav.len()))?;
        }
        at += 8 + size + (size & 1);
    };
    let samples: Vec<f32> =
        data.chunks_exact(2).map(|b| f32::from(i16::from_le_bytes([b[0], b[1]])) / 32768.0).collect();
    let loudest = samples
        .chunks(480)
        .map(|w| (w.iter().map(|s| s * s).sum::<f32>() / w.len() as f32).sqrt())
        .fold(0.0, f32::max);
    Some(loudest)
}

/// Transcribes an audio file (16 kHz WAV).
pub async fn transcribe(settings: &Settings, audio: &std::path::Path) -> Result<String, String> {
    let cli = settings.whisper_cli().ok_or(po_i18n::t!("tui.voice.no_whisper_short"))?;
    let model = settings.model_path().ok_or_else(|| po_i18n::t!("tui.voice.no_model", model = settings.model))?;
    let threads = std::thread::available_parallelism().map(|n| n.get().min(8)).unwrap_or(4).to_string();
    let mut command = tokio::process::Command::new(&cli);
    command
        .arg("-m")
        .arg(&model)
        .arg("-f")
        .arg(audio)
        .args(["-l", &settings.language, "-nt", "-np", "-t", &threads])
        .stdin(Stdio::null());
    match settings.gpu {
        // nvidia-smi numbering: the same as CUDA's in that order.
        Some(gpu) => {
            command.env("CUDA_DEVICE_ORDER", "PCI_BUS_ID").env("CUDA_VISIBLE_DEVICES", gpu.to_string());
        }
        None => {
            command.arg("-ng");
        }
    }
    let output = command.output().await.map_err(|e| format!("whisper: {e}"))?;
    if !output.status.success() {
        let err = String::from_utf8_lossy(&output.stderr);
        let last = err.lines().rev().find(|l| !l.trim().is_empty()).unwrap_or(po_i18n::t!("tui.voice.failed"));
        return Err(format!("whisper: {last}"));
    }
    Ok(clean_transcript(&String::from_utf8_lossy(&output.stdout)))
}

/// Transcribed text, without the marks whisper adds to silence ("[Music]", "(laughs)").
pub fn clean_transcript(raw: &str) -> String {
    let mut text = String::new();
    for line in raw.lines().map(str::trim).filter(|l| !l.is_empty()) {
        let is_marker = (line.starts_with('[') && line.ends_with(']')) || (line.starts_with('(') && line.ends_with(')'));
        // Sentences whisper invents on background noise: subtitle credits.
        let lower = line.to_lowercase();
        let phantom = ["sous-titrage", "sous-titres réalisés", "amara.org", "merci d'avoir regardé", "thanks for watching"]
            .iter()
            .any(|p| lower.contains(p));
        if is_marker || phantom {
            continue;
        }
        if !text.is_empty() {
            text.push(' ');
        }
        text.push_str(line);
    }
    text
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn microphones_are_read_from_ffmpegs_listing() {
        // Real output captured on the development machine.
        let listing = r#"[in#0 @ 0000026019167ac0] "Manette PS5 (DualSense Wireless Controller)" (audio)
[in#0 @ 0000026019167ac0]   Alternative name "@device_cm_{33D9A762}\wave_{A1}"
[in#0 @ 0000026019167ac0] "Casque (PRO X 2 LIGHTSPEED)" (audio)
[in#0 @ 0000026019167ac0] "OBS Virtual Camera" (video)"#;
        let mics = parse_devices(listing);
        assert_eq!(mics, ["Manette PS5 (DualSense Wireless Controller)", "Casque (PRO X 2 LIGHTSPEED)"]);
        assert_eq!(best_microphone(&mics).as_deref(), Some("Casque (PRO X 2 LIGHTSPEED)"), "not the controller");
    }

    #[test]
    fn silence_leaves_no_marks() {
        assert_eq!(clean_transcript(" Can you read\n the file?\n[Music]\n"), "Can you read the file?");
        assert_eq!(clean_transcript("[BLANK_AUDIO]\n"), "");
    }

    #[test]
    fn a_silence_is_not_sent_to_transcription() {
        let wav = |samples: &[i16]| {
            let data: Vec<u8> = samples.iter().flat_map(|s| s.to_le_bytes()).collect();
            let mut out = b"RIFF    WAVE".to_vec();
            out.extend(b"LIST   INFO");
            out.extend(b"data");
            out.extend((data.len() as u32).to_le_bytes());
            out.extend(data);
            out
        };
        assert_eq!(speech_level(&wav(&[0; 4800])), Some(0.0));
        let speech: Vec<i16> = (0..4800).map(|i| if i % 2 == 0 { 8000 } else { -8000 }).collect();
        assert!(speech_level(&wav(&speech)).unwrap() > SPEECH_LEVEL);
        assert_eq!(clean_transcript("Sous-titrage Société Radio-Canada\n"), "");
    }

    #[test]
    fn the_engine_picks_its_binary_and_model() {
        let mut s = Settings::default();
        assert!(!s.enabled, "disabled by default");
        assert_eq!((s.flavor(), s.model_file().as_str()), ("cpu", "ggml-small-q5_1.bin"));
        s.gpu = Some(0);
        s.model = "turbo".into();
        assert_eq!((s.flavor(), s.model_file().as_str()), ("cuda", "ggml-large-v3-turbo-q5_0.bin"));
        if cfg!(windows) {
            let (archive, model) = downloads(&s).unwrap();
            assert!(archive.ends_with("whisper-bin-win-cuda-12.4.0-x64.zip") && model.ends_with("turbo-q5_0.bin"));
        }
    }
}
