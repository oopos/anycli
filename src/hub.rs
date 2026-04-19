//! Hub client — search, install, and update adapters from anycli-hub on GitHub.
//!
//! The hub is a GitHub repository (`oopos/anycli-hub`) containing:
//! - `index.json` — adapter metadata (name, description, version)
//! - `adapters/<name>.yaml` — adapter YAML files
//!
//! All operations use the GitHub raw content API (no auth required).

use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use tracing::debug;

const HUB_REPO: &str = "oopos/anycli-hub";
const HUB_BRANCH: &str = "main";
const REQUEST_TIMEOUT: Duration = Duration::from_secs(15);
const USER_AGENT: &str = "anycli/0.1";

/// Metadata for a single adapter in the hub index.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HubEntry {
    /// Adapter name.
    pub name: String,
    /// Short description.
    pub description: String,
    /// Version string.
    #[serde(default)]
    pub version: String,
    /// Author or contributor.
    #[serde(default)]
    pub author: Option<String>,
    /// Tags for search filtering.
    #[serde(default)]
    pub tags: Vec<String>,
}

/// Hub index containing all available adapters.
#[derive(Debug, Deserialize)]
pub struct HubIndex {
    pub adapters: Vec<HubEntry>,
}

/// Hub client for interacting with the adapter registry.
pub struct Hub {
    client: reqwest::Client,
    base_url: String,
}

impl Hub {
    /// Create a new hub client with the default repository.
    pub fn new() -> Result<Self> {
        Self::with_repo(HUB_REPO)
    }

    /// Create a hub client pointing to a custom repository.
    pub fn with_repo(repo: &str) -> Result<Self> {
        let client = reqwest::Client::builder()
            .timeout(REQUEST_TIMEOUT)
            .user_agent(USER_AGENT)
            .build()?;
        let base_url = format!(
            "https://raw.githubusercontent.com/{repo}/{HUB_BRANCH}"
        );
        Ok(Self { client, base_url })
    }

    /// Fetch the hub index and return all adapter entries.
    pub async fn fetch_index(&self) -> Result<Vec<HubEntry>> {
        let url = format!("{}/index.json", self.base_url);
        debug!(url, "fetching hub index");

        let resp = self.client.get(&url).send().await
            .context("failed to reach anycli hub")?;

        if !resp.status().is_success() {
            bail!("hub returned HTTP {}", resp.status());
        }

        let index: HubIndex = resp.json().await.context("invalid hub index")?;
        Ok(index.adapters)
    }

    /// Search adapters by query (matches name, description, tags).
    pub async fn search(&self, query: &str) -> Result<Vec<HubEntry>> {
        let entries = self.fetch_index().await?;
        let q = query.to_lowercase();

        let results: Vec<HubEntry> = entries
            .into_iter()
            .filter(|e| {
                e.name.to_lowercase().contains(&q)
                    || e.description.to_lowercase().contains(&q)
                    || e.tags.iter().any(|t| t.to_lowercase().contains(&q))
            })
            .collect();

        Ok(results)
    }

    /// Download and install an adapter to the local adapters directory.
    ///
    /// Returns the path where the adapter was saved.
    pub async fn install(&self, name: &str, adapters_dir: &Path) -> Result<PathBuf> {
        let url = format!("{}/adapters/{name}.yaml", self.base_url);
        debug!(url, name, "downloading adapter");

        let resp = self.client.get(&url).send().await
            .with_context(|| format!("failed to download adapter `{name}`"))?;

        if !resp.status().is_success() {
            if resp.status().as_u16() == 404 {
                bail!("adapter `{name}` not found in hub");
            }
            bail!("hub returned HTTP {} for `{name}`", resp.status());
        }

        let content = resp.text().await?;

        // Validate YAML before saving.
        serde_yaml_ng::from_str::<crate::adapter::Adapter>(&content)
            .with_context(|| format!("adapter `{name}` has invalid YAML"))?;

        // Ensure directory exists.
        std::fs::create_dir_all(adapters_dir)
            .with_context(|| format!("failed to create {}", adapters_dir.display()))?;

        let dest = adapters_dir.join(format!("{name}.yaml"));
        std::fs::write(&dest, &content)
            .with_context(|| format!("failed to write {}", dest.display()))?;

        Ok(dest)
    }

    /// Update all installed adapters by re-downloading from hub.
    ///
    /// Returns the number of adapters updated.
    pub async fn update(&self, adapters_dir: &Path) -> Result<(usize, usize)> {
        if !adapters_dir.is_dir() {
            return Ok((0, 0));
        }

        let entries = std::fs::read_dir(adapters_dir)?;
        let mut updated = 0;
        let mut total = 0;

        for entry in entries {
            let entry = entry?;
            let path = entry.path();
            let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");
            if ext != "yaml" && ext != "yml" {
                continue;
            }

            let name = path.file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or_default();

            if name.is_empty() {
                continue;
            }

            total += 1;

            // Try to download updated version.
            match self.install(name, adapters_dir).await {
                Ok(_) => {
                    updated += 1;
                    debug!(name, "updated adapter");
                }
                Err(e) => {
                    debug!(name, error = %e, "skipping adapter update");
                }
            }
        }

        Ok((updated, total))
    }
}

/// Default user adapter directory: `~/.anycli/adapters/`.
pub fn default_adapters_dir() -> Option<PathBuf> {
    dirs_next::home_dir().map(|h| h.join(".anycli").join("adapters"))
}
