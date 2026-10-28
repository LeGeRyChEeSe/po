//! Reading the GGUF header.
//!
//! Sizing a local server by guesswork yields either a model that spills into RAM and crawls, or
//! a needlessly trimmed context window. The metadata needed for the computation — layer count,
//! embedding dimension, attention heads — is in the file: we read it. Only the header is walked,
//! never the weights, so the operation stays instant even on a 40 GB file.

use std::collections::HashMap;
use std::fs::File;
use std::io::{self, BufReader, Read};
use std::path::Path;

const MAGIC: &[u8; 4] = b"GGUF";
/// Safeguard: a healthy header does not exceed a few thousand entries. Beyond that we refuse
/// rather than allocate on the strength of an integer read from a possibly corrupt file.
const MAX_KV_COUNT: u64 = 100_000;
const MAX_STRING_LEN: u64 = 64 * 1024;
const MAX_ARRAY_LEN: u64 = 1_000_000;

#[derive(Debug, thiserror::Error)]
pub enum GgufError {
    #[error("{}", po_i18n::t!("local.gguf.io", err = .0))]
    Io(#[from] io::Error),
    #[error("{}", po_i18n::t!("local.gguf.not_gguf"))]
    NotGguf,
    #[error("{}", po_i18n::t!("local.gguf.unsupported_version", version = .0))]
    UnsupportedVersion(u32),
    #[error("{}", po_i18n::t!("local.gguf.malformed", detail = .0))]
    Malformed(String),
}

#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    U64(u64),
    I64(i64),
    F64(f64),
    Bool(bool),
    String(String),
    /// Arrays are not kept: none of the fields we need is one, and a vocabulary's token lists
    /// weigh hundreds of megabytes.
    Skipped,
}

impl Value {
    pub fn as_u64(&self) -> Option<u64> {
        match self {
            Self::U64(v) => Some(*v),
            Self::I64(v) if *v >= 0 => Some(*v as u64),
            _ => None,
        }
    }

    pub fn as_str(&self) -> Option<&str> {
        match self {
            Self::String(s) => Some(s),
            _ => None,
        }
    }
}

/// What the header tells us about a model, reduced to what sizing needs.
#[derive(Debug, Clone)]
pub struct GgufInfo {
    pub architecture: String,
    /// Number of transformer layers: determines the CPU/GPU split (`-ngl`).
    pub block_count: u32,
    pub embedding_length: u32,
    pub head_count: u32,
    /// Key/value heads: lower than `head_count` with GQA, which shrinks the cache accordingly.
    pub head_count_kv: u32,
    /// Maximum context the model was trained for.
    pub context_length: u32,
    /// File size, a direct approximation of the in-memory weight.
    pub file_size_bytes: u64,
    pub tensor_count: u64,
    /// Name announced by the metadata, otherwise the file name.
    pub name: String,
}

impl GgufInfo {
    /// Dimension of one attention head.
    pub fn head_dim(&self) -> u32 {
        if self.head_count == 0 {
            return 0;
        }
        self.embedding_length / self.head_count
    }

    /// KV cache bytes for `context` tokens and `parallel` simultaneous slots.
    ///
    /// Two tensors (K and V), one entry per layer, `head_count_kv` heads of `head_dim`
    /// elements. `bytes_per_element` is 2 for f16/bf16, 1 for an 8-bit quantized cache.
    pub fn kv_cache_bytes(&self, context: u32, parallel: u32, bytes_per_element: u64) -> u64 {
        self.block_count as u64 * self.kv_cache_bytes_per_layer(context, parallel, bytes_per_element)
    }

    /// KV cache bytes for **a single** layer.
    ///
    /// The distinction is not cosmetic: with partial offload, llama.cpp places each layer's
    /// cache in the memory where that layer lives. Charging the GPU for the cache of layers that
    /// stayed on the CPU amounts to reserving room for a tensor it will never host — and to
    /// offloading far fewer layers than it could.
    pub fn kv_cache_bytes_per_layer(
        &self,
        context: u32,
        parallel: u32,
        bytes_per_element: u64,
    ) -> u64 {
        let per_token = 2 * self.head_count_kv as u64 * self.head_dim() as u64 * bytes_per_element;
        per_token * context as u64 * parallel.max(1) as u64
    }

    /// Average weight of one layer, used to decide how many to offload to the GPU.
    pub fn bytes_per_layer(&self) -> u64 {
        if self.block_count == 0 {
            return 0;
        }
        self.file_size_bytes / self.block_count as u64
    }

    /// Are the fields essential to the computation present?
    pub fn is_usable(&self) -> bool {
        self.block_count > 0 && self.embedding_length > 0 && self.head_count > 0
    }
}

/// Reads the header of a GGUF file without touching the weights.
pub fn read_header(path: &Path) -> Result<GgufInfo, GgufError> {
    let file = File::open(path)?;
    let file_size_bytes = file.metadata()?.len();
    read_header_from(BufReader::with_capacity(64 * 1024, file), file_size_bytes, path)
}

/// Same reading, but from any stream.
///
/// A GGUF header is read front to back, never seeking backwards: that is what allows parsing
/// it as it flows through a pipe. Useful when the file cannot be opened directly — a model
/// living in WSL, for example, which only the distribution can serve.
///
/// `path` is only used to name the model when its metadata does not.
pub fn read_header_from(
    mut reader: impl Read,
    file_size_bytes: u64,
    path: &Path,
) -> Result<GgufInfo, GgufError> {
    let mut magic = [0u8; 4];
    reader.read_exact(&mut magic)?;
    if &magic != MAGIC {
        return Err(GgufError::NotGguf);
    }

    let version = read_u32(&mut reader)?;
    if !(2..=3).contains(&version) {
        return Err(GgufError::UnsupportedVersion(version));
    }

    let tensor_count = read_u64(&mut reader)?;
    let kv_count = read_u64(&mut reader)?;
    if kv_count > MAX_KV_COUNT {
        return Err(GgufError::Malformed(po_i18n::t!("local.gguf.too_many_entries", count = kv_count)));
    }

    let mut kv = HashMap::new();
    for _ in 0..kv_count {
        let key = read_string(&mut reader)?;
        let value = read_value(&mut reader)?;
        kv.insert(key, value);
    }

    let architecture = kv
        .get("general.architecture")
        .and_then(Value::as_str)
        .unwrap_or("unknown")
        .to_owned();

    // Dimensional keys are prefixed by the architecture: `llama.block_count`,
    // `qwen3.block_count`... hence the lookup by suffix.
    let dim = |suffix: &str| -> u32 {
        kv.get(&format!("{architecture}.{suffix}"))
            .and_then(Value::as_u64)
            .unwrap_or(0) as u32
    };

    let head_count = dim("attention.head_count");
    let head_count_kv = match dim("attention.head_count_kv") {
        // Without declared GQA, each head has its own cache.
        0 => head_count,
        n => n,
    };

    let name = kv
        .get("general.name")
        .and_then(Value::as_str)
        .map(str::to_owned)
        .unwrap_or_else(|| {
            path.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default()
        });

    Ok(GgufInfo {
        block_count: dim("block_count"),
        embedding_length: dim("embedding_length"),
        context_length: dim("context_length"),
        head_count,
        head_count_kv,
        architecture,
        file_size_bytes,
        tensor_count,
        name,
    })
}

fn read_u32(reader: &mut impl Read) -> Result<u32, GgufError> {
    let mut buf = [0u8; 4];
    reader.read_exact(&mut buf)?;
    Ok(u32::from_le_bytes(buf))
}

fn read_u64(reader: &mut impl Read) -> Result<u64, GgufError> {
    let mut buf = [0u8; 8];
    reader.read_exact(&mut buf)?;
    Ok(u64::from_le_bytes(buf))
}

fn read_string(reader: &mut impl Read) -> Result<String, GgufError> {
    let len = read_u64(reader)?;
    if len > MAX_STRING_LEN {
        return Err(GgufError::Malformed(po_i18n::t!("local.gguf.string_too_long", len = len)));
    }
    let mut buf = vec![0u8; len as usize];
    reader.read_exact(&mut buf)?;
    Ok(String::from_utf8_lossy(&buf).into_owned())
}

fn read_value(reader: &mut impl Read) -> Result<Value, GgufError> {
    let type_id = read_u32(reader)?;
    read_typed(reader, type_id)
}

fn read_typed(reader: &mut impl Read, type_id: u32) -> Result<Value, GgufError> {
    let mut take = |n: usize| -> Result<Vec<u8>, GgufError> {
        let mut buf = vec![0u8; n];
        reader.read_exact(&mut buf)?;
        Ok(buf)
    };

    Ok(match type_id {
        0 => Value::U64(take(1)?[0] as u64),
        1 => Value::I64(take(1)?[0] as i8 as i64),
        2 => Value::U64(u16::from_le_bytes(take(2)?.try_into().unwrap()) as u64),
        3 => Value::I64(i16::from_le_bytes(take(2)?.try_into().unwrap()) as i64),
        4 => Value::U64(u32::from_le_bytes(take(4)?.try_into().unwrap()) as u64),
        5 => Value::I64(i32::from_le_bytes(take(4)?.try_into().unwrap()) as i64),
        6 => Value::F64(f32::from_le_bytes(take(4)?.try_into().unwrap()) as f64),
        7 => Value::Bool(take(1)?[0] != 0),
        8 => Value::String(read_string(reader)?),
        9 => {
            // Array: we skip its content without allocating it. Vocabularies make them hundreds
            // of thousands of entries long, and none is of any use to us.
            let element_type = read_u32(reader)?;
            let len = read_u64(reader)?;
            if len > MAX_ARRAY_LEN {
                return Err(GgufError::Malformed(po_i18n::t!("local.gguf.array_too_long", len = len)));
            }
            skip_array(reader, element_type, len)?;
            Value::Skipped
        }
        10 => Value::U64(read_u64(reader)?),
        11 => Value::I64(i64::from_le_bytes(take(8)?.try_into().unwrap())),
        12 => Value::F64(f64::from_le_bytes(take(8)?.try_into().unwrap())),
        other => return Err(GgufError::Malformed(po_i18n::t!("local.gguf.unknown_value_type", type_id = other))),
    })
}

fn skip_array(
    reader: &mut impl Read,
    element_type: u32,
    len: u64,
) -> Result<(), GgufError> {
    // Fixed-size types are skipped in one go; strings and nested arrays must be walked, their
    // length being carried by each element.
    let fixed_size = match element_type {
        0 | 1 | 7 => Some(1),
        2..=3 => Some(2),
        4..=6 => Some(4),
        10..=12 => Some(8),
        _ => None,
    };

    match fixed_size {
        Some(size) => {
            // We consume rather than seek: a header must be readable as it flows through a pipe,
            // where there is no position to set — which is how a model served by a WSL
            // distribution arrives.
            io::copy(&mut reader.take(size * len), &mut io::sink())?;
        }
        None => {
            for _ in 0..len {
                read_typed(reader, element_type)?;
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    /// Builds a minimal but conforming synthetic GGUF, to test the parser without depending on
    /// a multi-gigabyte model on disk.
    struct GgufBuilder {
        kv: Vec<u8>,
        count: u64,
    }

    impl GgufBuilder {
        fn new() -> Self {
            Self { kv: Vec::new(), count: 0 }
        }

        fn string(mut self, key: &str, value: &str) -> Self {
            self.push_key(key);
            self.kv.extend_from_slice(&8u32.to_le_bytes());
            self.kv.extend_from_slice(&(value.len() as u64).to_le_bytes());
            self.kv.extend_from_slice(value.as_bytes());
            self.count += 1;
            self
        }

        fn u32(mut self, key: &str, value: u32) -> Self {
            self.push_key(key);
            self.kv.extend_from_slice(&4u32.to_le_bytes());
            self.kv.extend_from_slice(&value.to_le_bytes());
            self.count += 1;
            self
        }

        /// An array of strings, like a vocabulary: must be skipped without being allocated.
        fn string_array(mut self, key: &str, values: &[&str]) -> Self {
            self.push_key(key);
            self.kv.extend_from_slice(&9u32.to_le_bytes());
            self.kv.extend_from_slice(&8u32.to_le_bytes());
            self.kv.extend_from_slice(&(values.len() as u64).to_le_bytes());
            for value in values {
                self.kv.extend_from_slice(&(value.len() as u64).to_le_bytes());
                self.kv.extend_from_slice(value.as_bytes());
            }
            self.count += 1;
            self
        }

        fn u32_array(mut self, key: &str, values: &[u32]) -> Self {
            self.push_key(key);
            self.kv.extend_from_slice(&9u32.to_le_bytes());
            self.kv.extend_from_slice(&4u32.to_le_bytes());
            self.kv.extend_from_slice(&(values.len() as u64).to_le_bytes());
            for value in values {
                self.kv.extend_from_slice(&value.to_le_bytes());
            }
            self.count += 1;
            self
        }

        fn push_key(&mut self, key: &str) {
            self.kv.extend_from_slice(&(key.len() as u64).to_le_bytes());
            self.kv.extend_from_slice(key.as_bytes());
        }

        fn write(self, path: &Path) {
            let mut out = Vec::new();
            out.extend_from_slice(MAGIC);
            out.extend_from_slice(&3u32.to_le_bytes());
            out.extend_from_slice(&42u64.to_le_bytes()); // tensor_count
            out.extend_from_slice(&self.count.to_le_bytes());
            out.extend_from_slice(&self.kv);
            File::create(path).unwrap().write_all(&out).unwrap();
        }
    }

    fn temp_path(name: &str) -> std::path::PathBuf {
        let mut path = std::env::temp_dir();
        path.push(format!("po-gguf-{name}-{}.gguf", std::process::id()));
        path
    }

    fn qwen_model(path: &Path) {
        GgufBuilder::new()
            .string("general.architecture", "qwen3")
            .string("general.name", "Qwen3 27B")
            .u32("qwen3.block_count", 48)
            .u32("qwen3.embedding_length", 5120)
            .u32("qwen3.attention.head_count", 40)
            .u32("qwen3.attention.head_count_kv", 8)
            .u32("qwen3.context_length", 32768)
            .string_array("tokenizer.ggml.tokens", &["a", "b", "c"])
            .u32_array("tokenizer.ggml.token_type", &[1, 1, 1])
            .write(path);
    }

    #[test]
    fn reads_the_useful_dimensions() {
        let path = temp_path("qwen");
        qwen_model(&path);

        let info = read_header(&path).unwrap();
        assert_eq!(info.architecture, "qwen3");
        assert_eq!(info.name, "Qwen3 27B");
        assert_eq!(info.block_count, 48);
        assert_eq!(info.head_count_kv, 8);
        assert_eq!(info.context_length, 32768);
        assert_eq!(info.tensor_count, 42);
        assert_eq!(info.head_dim(), 128, "5120 / 40");
        assert!(info.is_usable());

        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn kv_cache_follows_gqa_geometry() {
        let path = temp_path("kv");
        qwen_model(&path);
        let info = read_header(&path).unwrap();

        // K and V, 48 layers, 8 KV heads of 128 elements: 98,304 elements per token,
        // i.e. 196,608 bytes in f16.
        let per_token = info.kv_cache_bytes(1, 1, 2);
        assert_eq!(per_token, 196_608);

        // Linear in context as in parallelism.
        assert_eq!(info.kv_cache_bytes(8192, 1, 2), 196_608 * 8192);
        assert_eq!(info.kv_cache_bytes(8192, 4, 2), 196_608 * 8192 * 4);
        // An 8-bit quantized cache halves the bill.
        assert_eq!(info.kv_cache_bytes(8192, 1, 1), 196_608 * 8192 / 2);

        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn without_declared_gqa_kv_heads_equal_heads() {
        let path = temp_path("nogqa");
        GgufBuilder::new()
            .string("general.architecture", "llama")
            .u32("llama.block_count", 32)
            .u32("llama.embedding_length", 4096)
            .u32("llama.attention.head_count", 32)
            .write(&path);

        let info = read_header(&path).unwrap();
        assert_eq!(info.head_count_kv, 32);
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn rejects_a_file_that_is_not_gguf() {
        let path = temp_path("bogus");
        File::create(&path).unwrap().write_all(b"this is not a model").unwrap();
        assert!(matches!(read_header(&path), Err(GgufError::NotGguf)));
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn a_model_without_dimensions_is_flagged_unusable() {
        let path = temp_path("empty");
        GgufBuilder::new().string("general.architecture", "mystery").write(&path);

        let info = read_header(&path).unwrap();
        assert!(!info.is_usable(), "we must refuse to size blindly");
        assert_eq!(info.head_dim(), 0, "and not divide by zero");
        assert_eq!(info.bytes_per_layer(), 0);
        std::fs::remove_file(&path).ok();
    }
}
