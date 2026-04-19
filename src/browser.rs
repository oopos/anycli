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
pub struct AgentBrowserFetcher {
    /// Chrome profile to use (e.g., "Default" to reuse login state).
    profile: Option<String>,
}

impl AgentBrowserFetcher {
    /// Create with default Chrome profile (reuses user's login state).
    pub fn new() -> Self {
        Self {
            profile: Some("Default".to_owned()),
        }
    }

    /// Create without a Chrome profile (fresh session).
    pub fn headless() -> Self {
        Self { profile: None }
    }

    /// Create with a specific Chrome profile name.
    pub fn with_profile(profile: impl Into<String>) -> Self {
        Self {
            profile: Some(profile.into()),
        }
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

        // Build command: open URL then get rendered HTML
        let mut cmd = tokio::process::Command::new("agent-browser");

        // Use Chrome profile to reuse login state
        if let Some(ref profile) = self.profile {
            cmd.args(["--profile", profile]);
        }

        cmd.args(["open", url]);
        let open_output = cmd.output().await.context("failed to run agent-browser open")?;

        if !open_output.status.success() {
            let stderr = String::from_utf8_lossy(&open_output.stderr);
            bail!("agent-browser open failed: {stderr}");
        }

        // Wait briefly for JS rendering to complete
        tokio::time::sleep(std::time::Duration::from_secs(2)).await;

        // Get the rendered HTML from <body>
        let mut get_cmd = tokio::process::Command::new("agent-browser");
        get_cmd.args(["get", "html", "body"]);

        let output = get_cmd.output().await.context("failed to run agent-browser get html")?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            bail!("agent-browser get html failed: {stderr}");
        }

        String::from_utf8(output.stdout)
            .context("agent-browser output is not valid UTF-8")
    }
}
