//! Chat persistence + runtime prefs.
//!
//! Saved chats live under `.usbuddy/chats/{uuid}.json` on the drive. They are
//! plaintext on purpose, matching the posture of `license-prefs.toml` and the
//! greppable `.usbuddy/` data dir: a passphrase-encrypted "vault" mode can
//! layer on later without changing filenames.
//!
//! Whether new turns are persisted at all is gated by the user-facing
//! incognito toggle in `runtime-prefs.toml`. Default is incognito (no writes).

use std::{
    fs,
    path::{Path, PathBuf},
};

use serde::{Deserialize, Serialize};
use usbuddy_core::atomic::{atomic_write_json, atomic_write_string};

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct RuntimePrefs {
    /// When false (default — privacy-first), conversations never touch the
    /// drive. When true, completed turns are persisted under
    /// `.usbuddy/chats/`. Flipped by the "Go incognito" / "Enable memory"
    /// toggle in the chat header.
    #[serde(default)]
    pub save_chats: bool,
}

impl RuntimePrefs {
    pub fn load(path: &Path) -> Self {
        match fs::read_to_string(path) {
            Ok(s) => toml::from_str(&s).unwrap_or_default(),
            Err(_) => Self::default(),
        }
    }

    pub fn save(&self, path: &Path) -> std::io::Result<()> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let body = toml::to_string_pretty(self)
            .map_err(|e| std::io::Error::other(format!("serialize prefs: {e}")))?;
        atomic_write_string(path, &body)
            .map_err(|e| std::io::Error::other(format!("write prefs: {e}")))
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatMessage {
    pub role: String,
    pub content: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Chat {
    pub id: String,
    pub title: String,
    #[serde(default)]
    pub model_id: Option<String>,
    pub created_epoch_secs: u64,
    pub updated_epoch_secs: u64,
    pub messages: Vec<ChatMessage>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ChatSummary {
    pub id: String,
    pub title: String,
    pub model_id: Option<String>,
    pub updated_epoch_secs: u64,
}

/// UUID v4 — 36 chars, lowercase hex with dashes. Anything else is rejected to
/// keep `chats_dir/{id}.json` from escaping the chats directory.
pub fn valid_id(id: &str) -> bool {
    if id.len() != 36 {
        return false;
    }
    id.bytes().enumerate().all(|(i, b)| match i {
        8 | 13 | 18 | 23 => b == b'-',
        _ => b.is_ascii_hexdigit() && !b.is_ascii_uppercase(),
    })
}

fn chat_path(chats_dir: &Path, id: &str) -> PathBuf {
    chats_dir.join(format!("{id}.json"))
}

pub fn list(chats_dir: &Path) -> std::io::Result<Vec<ChatSummary>> {
    let mut out = Vec::new();
    let entries = match fs::read_dir(chats_dir) {
        Ok(e) => e,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(out),
        Err(e) => return Err(e),
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let Some(name) = path.file_name().and_then(|s| s.to_str()) else {
            continue;
        };
        let Some(id) = name.strip_suffix(".json") else {
            continue;
        };
        if !valid_id(id) {
            continue;
        }
        let Ok(body) = fs::read_to_string(&path) else {
            continue;
        };
        let Ok(chat) = serde_json::from_str::<Chat>(&body) else {
            continue;
        };
        out.push(ChatSummary {
            id: chat.id,
            title: chat.title,
            model_id: chat.model_id,
            updated_epoch_secs: chat.updated_epoch_secs,
        });
    }
    out.sort_by_key(|c| std::cmp::Reverse(c.updated_epoch_secs));
    Ok(out)
}

pub fn read(chats_dir: &Path, id: &str) -> std::io::Result<Chat> {
    if !valid_id(id) {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "invalid chat id",
        ));
    }
    let body = fs::read_to_string(chat_path(chats_dir, id))?;
    serde_json::from_str(&body).map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))
}

pub fn write(chats_dir: &Path, chat: &Chat) -> std::io::Result<()> {
    if !valid_id(&chat.id) {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "invalid chat id",
        ));
    }
    fs::create_dir_all(chats_dir)?;
    atomic_write_json(&chat_path(chats_dir, &chat.id), chat)
        .map_err(|e| std::io::Error::other(format!("write chat: {e}")))
}

pub fn delete(chats_dir: &Path, id: &str) -> std::io::Result<()> {
    if !valid_id(id) {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "invalid chat id",
        ));
    }
    match fs::remove_file(chat_path(chats_dir, id)) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn id_validation_blocks_traversal() {
        assert!(valid_id("00000000-0000-4000-8000-000000000000"));
        assert!(!valid_id("../etc/passwd"));
        assert!(!valid_id("00000000-0000-4000-8000-00000000000"));
        assert!(!valid_id("00000000_0000_4000_8000_000000000000"));
        assert!(!valid_id("ABCDEFAB-0000-4000-8000-000000000000"));
    }

    #[test]
    fn roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let id = "11111111-2222-4333-8444-555555555555".to_string();
        let chat = Chat {
            id: id.clone(),
            title: "hello".into(),
            model_id: Some("m".into()),
            created_epoch_secs: 1,
            updated_epoch_secs: 2,
            messages: vec![ChatMessage {
                role: "user".into(),
                content: "hi".into(),
            }],
        };
        write(dir.path(), &chat).unwrap();
        let got = read(dir.path(), &id).unwrap();
        assert_eq!(got.title, "hello");
        assert_eq!(list(dir.path()).unwrap().len(), 1);
        delete(dir.path(), &id).unwrap();
        assert!(list(dir.path()).unwrap().is_empty());
    }
}
