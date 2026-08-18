//! Adapter registry — discover and load adapters from built-in and user directories.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use include_dir::{Dir, include_dir};
use tracing::debug;

use crate::adapter::Adapter;

/// Built-in adapter YAML files, embedded at compile time.
/// Adding a file under `adapters/` is enough — no registry edit required.
static BUILTIN_DIR: Dir<'_> = include_dir!("$CARGO_MANIFEST_DIR/adapters");

/// Adapter registry holding all available adapters.
#[derive(Debug)]
pub struct Registry {
    adapters: HashMap<String, Adapter>,
    /// Canonical adapter name → original YAML source.
    sources: HashMap<String, String>,
}

impl Registry {
    /// Load all adapters: built-in + user directory.
    ///
    /// User adapters from `~/.anycli/adapters/` override built-in ones
    /// with the same name.
    pub fn load() -> Result<Self> {
        let mut adapters = HashMap::new();
        let mut sources = HashMap::new();

        // Load built-in adapters from the embedded adapters/ directory.
        for file in BUILTIN_DIR.files() {
            let path = file.path();
            let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");
            if ext != "yaml" && ext != "yml" {
                continue;
            }
            let yaml = file.contents_utf8().ok_or_else(|| {
                anyhow::anyhow!("built-in adapter {} is not valid UTF-8", path.display())
            })?;
            let adapter = serde_yaml_ng::from_str::<Adapter>(yaml)
                .with_context(|| format!("failed to parse built-in adapter `{}`", path.display()))?;
            sources.insert(adapter.name.clone(), yaml.to_string());
            adapters.insert(adapter.name.clone(), adapter);
        }

        // Load user adapters (override built-in).
        if let Some(user_dir) = user_adapter_dir() {
            if user_dir.is_dir() {
                load_dir(&user_dir, &mut adapters, &mut sources)?;
            }
        }

        Ok(Self { adapters, sources })
    }

    /// Load adapters from a specific directory (in addition to built-in).
    pub fn load_with_dir(extra_dir: &Path) -> Result<Self> {
        let mut registry = Self::load()?;
        if extra_dir.is_dir() {
            load_dir(extra_dir, &mut registry.adapters, &mut registry.sources)?;
        }
        Ok(registry)
    }

    /// Find an adapter by name or alias.
    pub fn find(&self, name: &str) -> Result<&Adapter> {
        if let Some(adapter) = self.adapters.get(name) {
            return Ok(adapter);
        }
        if let Some(adapter) = self.adapters.values().find(|a| {
            a.name == name || a.aliases.iter().any(|alias| alias == name)
        }) {
            return Ok(adapter);
        }
        let available: Vec<&str> = self.list().iter().map(|a| a.name.as_str()).collect();
        let hint = crate::pipeline::suggest(name, available.iter().copied());
        anyhow::bail!("adapter `{name}` not found{hint}")
    }

    /// List all available adapters, de-duplicated by canonical name.
    pub fn list(&self) -> Vec<&Adapter> {
        let mut adapters: Vec<&Adapter> = self.adapters.values().collect();
        adapters.sort_by_key(|a| &a.name);
        adapters.dedup_by(|a, b| a.name == b.name);
        adapters
    }

    /// Search loaded adapters by name, alias, tag, or description.
    pub fn search(&self, query: &str) -> Vec<&Adapter> {
        let q = query.to_lowercase();
        self.list()
            .into_iter()
            .filter(|a| {
                a.name.to_lowercase().contains(&q)
                    || a.description.to_lowercase().contains(&q)
                    || a.aliases.iter().any(|alias| alias.to_lowercase().contains(&q))
                    || a.tags.iter().any(|tag| tag.to_lowercase().contains(&q))
            })
            .collect()
    }

    /// Original YAML for an adapter (user override, else built-in).
    pub fn source_yaml(&self, name: &str) -> Result<&str> {
        let adapter = self.find(name)?;
        self.sources
            .get(&adapter.name)
            .or_else(|| self.sources.get(name))
            .map(String::as_str)
            .ok_or_else(|| anyhow::anyhow!("no YAML source for adapter `{}`", adapter.name))
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
fn load_dir(
    dir: &Path,
    adapters: &mut HashMap<String, Adapter>,
    sources: &mut HashMap<String, String>,
) -> Result<()> {
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
                sources.insert(adapter.name.clone(), content);
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
        assert!(registry.len() >= 100, "expected 100+ built-in adapters, got {}", registry.len());
        assert!(registry.find("hackernews").is_ok());
        assert!(registry.find("wikipedia").is_ok());
        assert!(registry.find("bilibili").is_ok());
        assert!(registry.find("arxiv").is_ok());
        assert!(registry.find("github-trending").is_ok());
        assert!(registry.find("hf").is_ok());
        assert_eq!(registry.find("hf").unwrap().name, "huggingface");
        assert!(registry.find("hn").is_ok());
        assert_eq!(registry.find("hn").unwrap().name, "hackernews");
        assert!(registry.find("packagist").is_ok());
        assert!(registry.find("brew").unwrap().name == "homebrew");
        // aliases should not duplicate the list
        let names: Vec<&str> = registry.list().iter().map(|a| a.name.as_str()).collect();
        let hf_count = names.iter().filter(|n| **n == "huggingface").count();
        assert_eq!(hf_count, 1);
        assert!(registry.source_yaml("hackernews").unwrap().contains("name: hackernews"));
        assert!(registry.source_yaml("hf").unwrap().contains("name: huggingface"));
    }

    #[test]
    fn unknown_adapter_errors() {
        let registry = Registry::load().expect("load");
        assert!(registry.find("nonexistent").is_err());
    }

    #[test]
    fn search_matches_name_alias_and_description() {
        let registry = Registry::load().expect("load");
        let hits = registry.search("juejin");
        assert!(hits.iter().any(|a| a.name == "juejin"));
        let hits = registry.search("jj");
        assert!(hits.iter().any(|a| a.name == "juejin"));
        let hits = registry.search("biomedical");
        assert!(hits.iter().any(|a| a.name == "pubmed"));
    }
}
