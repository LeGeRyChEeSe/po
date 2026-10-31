//! The quantization of a model, read from its file name.
//!
//! The GGUF header does carry a `general.file_type`, but as a number whose table changes with
//! llama.cpp's versions, and a mixed quantization (Unsloth's `UD-Q4_K_XL`, the GSQ-RCO files'
//! `IQ3_XXS`) is better told by its name, which is what every publisher puts in the file name and
//! what the user recognises.

/// `Qwen3.8-27B-UD-Q4_K_XL.gguf` → `UD-Q4_K_XL`; `…-IQ3_XXS-00001-of-00002.gguf` → `IQ3_XXS`;
/// `gemma-4-12b-it-Q4_K_M` → `Q4_K_M`; `model-BF16` → `BF16`. `None` when nothing looks like one.
pub fn from_name(name: &str) -> Option<String> {
    let stem = name.strip_suffix(".gguf").or_else(|| name.strip_suffix(".GGUF")).unwrap_or(name);
    let parts: Vec<&str> = stem.split(['-', '.', ' ']).collect();
    // From the end: the quantization is the last thing a publisher writes, before the shard
    // number. `Qwen3.6` or `35B` must not be read as one.
    for (i, part) in parts.iter().enumerate().rev() {
        let upper = part.to_ascii_uppercase();
        if is_quant(&upper) {
            let unsloth = i > 0 && parts[i - 1].eq_ignore_ascii_case("ud");
            return Some(if unsloth { format!("UD-{upper}") } else { upper });
        }
    }
    None
}

fn is_quant(token: &str) -> bool {
    if matches!(token, "F16" | "BF16" | "F32" | "FP16" | "FP32" | "MXFP4" | "NVFP4" | "MXFP4_MOE") {
        return true;
    }
    // Q4_K_M, Q8_0, IQ3_XXS, IQ2_XS, TQ1_0, Q2_0, IQ4_NL…
    let rest = token.strip_prefix("IQ").or_else(|| token.strip_prefix("TQ")).or_else(|| token.strip_prefix('Q'));
    let Some(rest) = rest else { return false };
    let mut chars = rest.chars();
    let Some(bits) = chars.next() else { return false };
    if !bits.is_ascii_digit() {
        return false;
    }
    let tail: String = chars.collect();
    tail.is_empty() && token.starts_with("IQ") || tail.starts_with('_') && tail.len() > 1 && tail[1..].split('_').all(|p| !p.is_empty() && p.chars().all(|c| c.is_ascii_alphanumeric()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_publishers_names_are_read() {
        let cases = [
            ("Qwen3.8-27B-UD-Q4_K_XL.gguf", Some("UD-Q4_K_XL")),
            ("Qwen3.8-Flash-Next-GSQ-RCO-IQ3_XXS-00001-of-00002.gguf", Some("IQ3_XXS")),
            ("gemma-4-12b-it-Q4_K_M.gguf", Some("Q4_K_M")),
            ("Qwen3.6-35B-A3B-MXFP4_MOE.gguf", Some("MXFP4_MOE")),
            ("Qwen3.5-0.8B-Q4_K_M.gguf", Some("Q4_K_M")),
            ("Spark-X2.6-4B-Q8_0.gguf", Some("Q8_0")),
            ("OxCoder-9B-MERNIK-5100-Q4.gguf", None),
            ("Qwen3.8-27B-BF16-00001-of-00002.gguf", Some("BF16")),
            ("NVIDIA-Nemotron-3.5-Bolt-E4B-UD-Q4_K_XL.gguf", Some("UD-Q4_K_XL")),
            ("iq3_xxs", Some("IQ3_XXS")),
            ("model.gguf", None),
            ("Qwen3.6-35B", None),
            ("", None),
        ];
        for (name, expected) in cases {
            assert_eq!(from_name(name).as_deref(), expected, "{name}");
        }
    }
}
