//! Browser fetcher trait and default agent-browser CLI implementation.

use anyhow::{Context, Result, bail};

/// Trait for fetching rendered HTML from a URL using a browser engine.
///
/// Implementations can use CDP, Playwright, agent-browser CLI, etc.
/// This allows `rsclaw` to inject its own CDP-based fetcher while
/// standalone `anycli` uses the `agent-browser` CLI.
#[async_trait::async_trait]
pub trait BrowserFetcher: Send + Sync {
    /// Navigate to the URL and return the fully rendered HTML.
    async fn fetch(&self, url: &str) -> Result<String>;
}

/// Default implementation that shells out to `agent-browser` CLI.
///
/// Requires `agent-browser` to be installed (`npm install -g agent-browser`).
pub struct AgentBrowserFetcher;

impl AgentBrowserFetcher {
    pub fn new() -> Self {
        Self
    }

    /// Check if agent-browser is available on PATH.
    pub fn is_available() -> bool {
        std::process::Command::new("agent-browser")
            .arg("--version")
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .map(|s| s.success())
            .unwrap_or(false)
    }
}

#[async_trait::async_trait]
impl BrowserFetcher for AgentBrowserFetcher {
    async fn fetch(&self, url: &str) -> Result<String> {
        if !Self::is_available() {
            bail!(
                "agent-browser is not installed.\n\
                 Install it with: npm install -g agent-browser\n\
                 This adapter requires a browser to render JavaScript."
            );
        }

        // Use agent-browser to get rendered HTML
        let output = tokio::process::Command::new("agent-browser")
            .args(["snapshot", "--url", url, "--format", "html"])
            .output()
            .await
            .context("failed to run agent-browser")?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            bail!("agent-browser failed: {stderr}");
        }

        String::from_utf8(output.stdout)
            .context("agent-browser output is not valid UTF-8")
    }
}
