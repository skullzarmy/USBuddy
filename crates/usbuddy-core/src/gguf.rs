//! Minimal GGUF metadata reader.
//!
//! Reads just the architecture-relevant metadata from a GGUF file's header —
//! enough to compute KV-cache size per token and cap the runtime UI's
//! context slider to what the model was trained for. Tensor data is never
//! touched; the file is opened, the first few MB are read, and that's it.
//!
//! GGUF format reference:
//! <https://github.com/ggml-org/ggml/blob/master/docs/gguf.md>
//!
//! Why this exists: the previous RAM-fit advisor used a single hardcoded
//! `kv_bytes_per_token = 131_072` (an 8B-GQA assumption). It was wildly
//! optimistic for non-GQA 7B models like Mistral and overly pessimistic for
//! smaller models like Gemma 2B — so the badge mostly stayed green
//! regardless of context. Real numbers from the model fix this.

use std::fs::File;
use std::io::Read;
use std::path::Path;

use serde::{Deserialize, Serialize};

const GGUF_MAGIC: &[u8; 4] = b"GGUF";
/// 4 MiB is well above the metadata section size for any model we ship
/// (typically <1 MiB even with the full tokenizer vocab embedded).
const HEADER_READ_BYTES: usize = 4 * 1024 * 1024;

/// The architecture fields the RAM-fit advisor needs.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ArchMeta {
    pub architecture: String,
    /// Number of transformer blocks (layers).
    pub block_count: u32,
    /// Number of attention heads.
    pub head_count: u32,
    /// Number of KV heads (equals `head_count` when the model isn't GQA).
    pub head_count_kv: u32,
    /// Hidden dimension.
    pub embedding_length: u32,
    /// Maximum context the model was trained for, in tokens.
    pub context_length: u32,
}

impl ArchMeta {
    /// KV-cache bytes per token, assuming f16 cache (llama.cpp's default).
    ///
    /// Formula: `2 (K + V) * layers * kv_heads * head_dim * 2 bytes (f16)`,
    /// where `head_dim = embedding_length / head_count`.
    pub fn kv_bytes_per_token_f16(&self) -> u64 {
        let head_dim = (self.embedding_length / self.head_count.max(1)) as u64;
        2 * self.block_count as u64 * self.head_count_kv as u64 * head_dim * 2
    }
}

/// Reads `path` and returns its [`ArchMeta`] if it's a GGUF v2/v3 file with
/// the expected fields. Returns `None` for non-GGUF files, IO failures, or
/// when any of the required fields are missing — callers fall back to a
/// conservative heuristic in that case.
pub fn read_arch_meta(path: &Path) -> Option<ArchMeta> {
    let mut file = File::open(path).ok()?;
    let mut buf = vec![0u8; HEADER_READ_BYTES];
    let n = file.read(&mut buf).ok()?;
    buf.truncate(n);
    parse(&buf)
}

// ---------------------------------------------------------------------------
// Parser internals
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ValueType {
    U8,
    I8,
    U16,
    I16,
    U32,
    I32,
    F32,
    Bool,
    String,
    Array,
    U64,
    I64,
    F64,
}

impl ValueType {
    fn from_u32(v: u32) -> Option<Self> {
        Some(match v {
            0 => Self::U8,
            1 => Self::I8,
            2 => Self::U16,
            3 => Self::I16,
            4 => Self::U32,
            5 => Self::I32,
            6 => Self::F32,
            7 => Self::Bool,
            8 => Self::String,
            9 => Self::Array,
            10 => Self::U64,
            11 => Self::I64,
            12 => Self::F64,
            _ => return None,
        })
    }
}

struct Cursor<'a> {
    buf: &'a [u8],
    pos: usize,
}

impl<'a> Cursor<'a> {
    fn read_u32(&mut self) -> Option<u32> {
        let end = self.pos.checked_add(4)?;
        let bytes = self.buf.get(self.pos..end)?;
        self.pos = end;
        Some(u32::from_le_bytes(bytes.try_into().ok()?))
    }
    fn read_u64(&mut self) -> Option<u64> {
        let end = self.pos.checked_add(8)?;
        let bytes = self.buf.get(self.pos..end)?;
        self.pos = end;
        Some(u64::from_le_bytes(bytes.try_into().ok()?))
    }
    fn read_string(&mut self) -> Option<String> {
        let len = self.read_u64()? as usize;
        let end = self.pos.checked_add(len)?;
        let bytes = self.buf.get(self.pos..end)?;
        self.pos = end;
        Some(std::str::from_utf8(bytes).ok()?.to_string())
    }
    fn skip_bytes(&mut self, n: usize) -> Option<()> {
        let end = self.pos.checked_add(n)?;
        if end > self.buf.len() {
            return None;
        }
        self.pos = end;
        Some(())
    }
    fn skip_value(&mut self, vt: ValueType) -> Option<()> {
        match vt {
            ValueType::U8 | ValueType::I8 | ValueType::Bool => self.skip_bytes(1)?,
            ValueType::U16 | ValueType::I16 => self.skip_bytes(2)?,
            ValueType::U32 | ValueType::I32 | ValueType::F32 => self.skip_bytes(4)?,
            ValueType::U64 | ValueType::I64 | ValueType::F64 => self.skip_bytes(8)?,
            ValueType::String => {
                let _ = self.read_string()?;
            }
            ValueType::Array => {
                let elem_type_raw = self.read_u32()?;
                let elem_type = ValueType::from_u32(elem_type_raw)?;
                let count = self.read_u64()?;
                for _ in 0..count {
                    self.skip_value(elem_type)?;
                }
            }
        }
        Some(())
    }
    /// Reads a value if its declared type is an unsigned-int-like scalar,
    /// returning it as u64. Returns `None` (without advancing) for anything
    /// else — caller should `skip_value` in that case.
    fn read_uint_value(&mut self, vt: ValueType) -> Option<u64> {
        let saved = self.pos;
        let v = match vt {
            ValueType::U8 => self.buf.get(self.pos).map(|&b| b as u64).inspect(|_| {
                self.pos += 1;
            }),
            ValueType::U16 => {
                let end = self.pos.checked_add(2)?;
                let bytes = self.buf.get(self.pos..end)?;
                self.pos = end;
                Some(u16::from_le_bytes(bytes.try_into().ok()?) as u64)
            }
            ValueType::U32 => self.read_u32().map(|x| x as u64),
            ValueType::U64 => self.read_u64(),
            ValueType::I32 => {
                let end = self.pos.checked_add(4)?;
                let bytes = self.buf.get(self.pos..end)?;
                self.pos = end;
                Some(i32::from_le_bytes(bytes.try_into().ok()?).max(0) as u64)
            }
            ValueType::I64 => {
                let end = self.pos.checked_add(8)?;
                let bytes = self.buf.get(self.pos..end)?;
                self.pos = end;
                Some(i64::from_le_bytes(bytes.try_into().ok()?).max(0) as u64)
            }
            _ => None,
        };
        if v.is_none() {
            self.pos = saved;
        }
        v
    }
}

fn parse(buf: &[u8]) -> Option<ArchMeta> {
    if buf.len() < 24 || &buf[0..4] != GGUF_MAGIC {
        return None;
    }
    let mut cur = Cursor { buf, pos: 4 };
    let _version = cur.read_u32()?;
    let _tensor_count = cur.read_u64()?;
    let kv_count = cur.read_u64()?;

    let mut architecture: Option<String> = None;
    let mut block_count: Option<u64> = None;
    let mut head_count: Option<u64> = None;
    let mut head_count_kv: Option<u64> = None;
    let mut embedding_length: Option<u64> = None;
    let mut context_length: Option<u64> = None;

    for _ in 0..kv_count {
        let key = cur.read_string()?;
        let vt = ValueType::from_u32(cur.read_u32()?)?;

        // Arch-prefixed keys vary by model family ("llama.block_count",
        // "qwen2.block_count", "gemma2.block_count"). Match on the suffix
        // so we don't need a per-arch table.
        if key == "general.architecture" {
            if vt == ValueType::String {
                architecture = cur.read_string();
            } else {
                cur.skip_value(vt)?;
            }
            continue;
        }
        let slot: Option<&mut Option<u64>> = if key.ends_with(".block_count") {
            Some(&mut block_count)
        } else if key.ends_with(".attention.head_count") {
            Some(&mut head_count)
        } else if key.ends_with(".attention.head_count_kv") {
            Some(&mut head_count_kv)
        } else if key.ends_with(".embedding_length") {
            Some(&mut embedding_length)
        } else if key.ends_with(".context_length") {
            Some(&mut context_length)
        } else {
            None
        };
        match slot {
            Some(s) => {
                if let Some(v) = cur.read_uint_value(vt) {
                    *s = Some(v);
                } else {
                    cur.skip_value(vt)?;
                }
            }
            None => cur.skip_value(vt)?,
        }
    }

    Some(ArchMeta {
        architecture: architecture?,
        block_count: u32::try_from(block_count?).ok()?,
        head_count: u32::try_from(head_count?).ok()?,
        head_count_kv: u32::try_from(head_count_kv.unwrap_or(head_count?)).ok()?,
        embedding_length: u32::try_from(embedding_length?).ok()?,
        context_length: u32::try_from(context_length?).ok()?,
    })
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a minimal valid GGUF v3 buffer with just the keys we care about,
    /// using arch "llama". Validates the parser end-to-end without needing
    /// a 4 GB model file in tree.
    fn build_synthetic_gguf(
        arch: &str,
        block_count: u32,
        head_count: u32,
        head_count_kv: u32,
        embedding_length: u32,
        context_length: u32,
    ) -> Vec<u8> {
        let mut b = Vec::new();
        b.extend_from_slice(GGUF_MAGIC);
        b.extend_from_slice(&3u32.to_le_bytes()); // version
        b.extend_from_slice(&0u64.to_le_bytes()); // tensor count
        // 6 metadata entries: architecture, block_count, head_count,
        // head_count_kv, embedding_length, context_length.
        b.extend_from_slice(&6u64.to_le_bytes());

        // Helper: push a string-valued kv.
        let push_str_kv = |b: &mut Vec<u8>, key: &str, val: &str| {
            b.extend_from_slice(&(key.len() as u64).to_le_bytes());
            b.extend_from_slice(key.as_bytes());
            b.extend_from_slice(&8u32.to_le_bytes()); // String type
            b.extend_from_slice(&(val.len() as u64).to_le_bytes());
            b.extend_from_slice(val.as_bytes());
        };
        // Helper: push a u32-valued kv.
        let push_u32_kv = |b: &mut Vec<u8>, key: &str, val: u32| {
            b.extend_from_slice(&(key.len() as u64).to_le_bytes());
            b.extend_from_slice(key.as_bytes());
            b.extend_from_slice(&4u32.to_le_bytes()); // U32 type
            b.extend_from_slice(&val.to_le_bytes());
        };

        push_str_kv(&mut b, "general.architecture", arch);
        push_u32_kv(&mut b, &format!("{arch}.block_count"), block_count);
        push_u32_kv(&mut b, &format!("{arch}.attention.head_count"), head_count);
        push_u32_kv(
            &mut b,
            &format!("{arch}.attention.head_count_kv"),
            head_count_kv,
        );
        push_u32_kv(
            &mut b,
            &format!("{arch}.embedding_length"),
            embedding_length,
        );
        push_u32_kv(&mut b, &format!("{arch}.context_length"), context_length);
        b
    }

    #[test]
    fn parses_synthetic_llama_3_1_8b_shape() {
        // Llama 3.1 8B: 32 layers, 32 heads, 8 KV heads (GQA),
        // hidden 4096, trained context 131072.
        let buf = build_synthetic_gguf("llama", 32, 32, 8, 4096, 131_072);
        let meta = parse(&buf).expect("parse");
        assert_eq!(meta.architecture, "llama");
        assert_eq!(meta.block_count, 32);
        assert_eq!(meta.head_count, 32);
        assert_eq!(meta.head_count_kv, 8);
        assert_eq!(meta.embedding_length, 4096);
        assert_eq!(meta.context_length, 131_072);
        // 2 * 32 * 8 * 128 * 2 = 131_072 bytes per token. The old hardcoded
        // constant got 8B-Llama exactly right, hence "always green."
        assert_eq!(meta.kv_bytes_per_token_f16(), 131_072);
    }

    #[test]
    fn parses_mistral_7b_non_gqa_shape() {
        // Mistral 7B v0.3: 32 layers, 32 heads, NO GQA → KV heads = 32.
        let buf = build_synthetic_gguf("llama", 32, 32, 32, 4096, 32_768);
        let meta = parse(&buf).expect("parse");
        // 2 * 32 * 32 * 128 * 2 = 524_288 bytes/token — 4× the old constant.
        assert_eq!(meta.kv_bytes_per_token_f16(), 524_288);
    }

    #[test]
    fn rejects_non_gguf() {
        assert!(parse(b"NOT-A-GGUF-FILE--PADDING-PADDING").is_none());
    }
}
