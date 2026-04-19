//! Adapter registry — discover and load adapters from built-in and user directories.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use tracing::debug;

use crate::adapter::Adapter;

// Built-in adapters embedded at compile time.
const BUILTIN_ADAPTERS: &[(&str, &str)] = &[
    ("hackernews", include_str!("../adapters/hackernews.yaml")),
    ("github-trending", include_str!("../adapters/github_trending.yaml")),
    ("arxiv", include_str!("../adapters/arxiv.yaml")),
    ("wikipedia", include_str!("../adapters/wikipedia.yaml")),
    ("bilibili", include_str!("../adapters/bilibili.yaml")),
    ("v2ex", include_str!("../adapters/v2ex.yaml")),
    ("douban", include_str!("../adapters/douban.yaml")),
    ("zhihu", include_str!("../adapters/zhihu.yaml")),
    ("weibo", include_str!("../adapters/weibo.yaml")),
    ("reddit", include_str!("../adapters/reddit.yaml")),
    ("stackoverflow", include_str!("../adapters/stackoverflow.yaml")),
    ("github", include_str!("../adapters/github.yaml")),
    ("devto", include_str!("../adapters/devto.yaml")),
    ("lobsters", include_str!("../adapters/lobsters.yaml")),
    ("huggingface", include_str!("../adapters/huggingface.yaml")),
    ("steam", include_str!("../adapters/steam.yaml")),
    ("36kr", include_str!("../adapters/36kr.yaml")),
    ("medium", include_str!("../adapters/medium.yaml")),
    ("bbc", include_str!("../adapters/bbc.yaml")),
    ("producthunt", include_str!("../adapters/producthunt.yaml")),
    ("xueqiu", include_str!("../adapters/xueqiu.yaml")),
];

/// Adapter registry holding all available adapters.
#[derive(Debug)]
pub struct Registry {
    adapters: HashMap<String, Adapter>,
}

impl Registry {
    /// Load all adapters: built-in + user directory.
    ///
    /// User adapters from `~/.anycli/adapters/` override built-in ones
    /// with the same name.
    pub fn load() -> Result<Self> {
        let mut adapters = HashMap::new();

        // Load built-in adapters.
        for (name, yaml) in BUILTIN_ADAPTERS {
            match serde_yaml_ng::from_str::<Adapter>(yaml) {
                Ok(adapter) => { adapters.insert(name.to_string(), adapter); }
                Err(e) => { debug!(name, error = %e, "failed to parse built-in adapter"); }
            }
        }

        // Load user adapters (override built-in).
        if let Some(user_dir) = user_adapter_dir() {
            if user_dir.is_dir() {
                load_dir(&user_dir, &mut adapters)?;
            }
        }

        Ok(Self { adapters })
    }

    /// Load adapters from a specific directory (in addition to built-in).
    pub fn load_with_dir(extra_dir: &Path) -> Result<Self> {
        let mut registry = Self::load()?;
        if extra_dir.is_dir() {
            load_dir(extra_dir, &mut registry.adapters)?;
        }
        Ok(registry)
    }

    /// Find an adapter by name.
    pub fn find(&self, name: &str) -> Result<&Adapter> {
        self.adapters.get(name).with_context(|| {
            let available: Vec<&str> = self.adapters.keys().map(|s| s.as_str()).collect();
            format!("adapter `{name}` not found. available: {}", available.join(", "))
        })
    }

    /// List all available adapter names.
    pub fn list(&self) -> Vec<&Adapter> {
        let mut adapters: Vec<&Adapter> = self.adapters.values().collect();
        adapters.sort_by_key(|a| &a.name);
        adapters
    }

    /// Number of loaded adapters.
    pub fn len(&self) -> usize {
        self.adapters.len()
    }

    /// Whether the registry is empty.
    pub fn is_empty(&self) -> bool {
        self.adapters.is_empty()
    }
}

/// Load all `.yaml` / `.yml` files from a directory into the adapter map.
fn load_dir(dir: &Path, adapters: &mut HashMap<String, Adapter>) -> Result<()> {
    let entries = std::fs::read_dir(dir)
        .with_context(|| format!("failed to read adapter dir: {}", dir.display()))?;

    for entry in entries {
        let entry = entry?;
        let path = entry.path();
        let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");
        if ext != "yaml" && ext != "yml" {
            continue;
        }

        let content = std::fs::read_to_string(&path)
            .with_context(|| format!("failed to read {}", path.display()))?;

        match serde_yaml_ng::from_str::<Adapter>(&content) {
            Ok(adapter) => {
                debug!(name = adapter.name, path = %path.display(), "loaded user adapter");
                adapters.insert(adapter.name.clone(), adapter);
            }
            Err(e) => {
                debug!(path = %path.display(), error = %e, "skipping invalid adapter");
            }
        }
    }

    Ok(())
}

/// Default user adapter directory: `~/.anycli/adapters/`.
fn user_adapter_dir() -> Option<PathBuf> {
    dirs_next::home_dir().map(|h| h.join(".anycli").join("adapters"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builtin_adapters_parse() {
        let registry = Registry::load().expect("load");
        assert!(registry.len() >= 5, "expected at least 5 built-in adapters");
        assert!(registry.find("hackernews").is_ok());
        assert!(registry.find("wikipedia").is_ok());
        assert!(registry.find("bilibili").is_ok());
        assert!(registry.find("arxiv").is_ok());
        assert!(registry.find("github-trending").is_ok());
    }

    #[test]
    fn unknown_adapter_errors() {
        let registry = Registry::load().expect("load");
        assert!(registry.find("nonexistent").is_err());
    }
}
