//! Browser fetcher trait and implementations (rsclaw browser + agent-browser CLI).

use anyhow::{Context, Result, bail};

/// Trait for fetching rendered HTML from a URL using a browser engine.
#[async_trait::async_trait]
pub trait BrowserFetcher: Send + Sync {
    /// Navigate to the URL and return the fully rendered HTML.
    async fn fetch(&self, url: &str) -> Result<String>;

    /// Navigate to the URL, then evaluate JS and return the result.
    async fn eval(&self, url: &str, js: &str) -> Result<String>;

    /// Connect to a desktop app via CDP and evaluate JS.
    async fn desktop_eval(&self, target: &str, js: &str) -> Result<String>;

    /// Navigate to URL and intercept a network response matching the pattern.
    async fn intercept(&self, url: &str, pattern: &str) -> Result<String>;
}

/// Auto-detecting browser fetcher. Prefers rsclaw, falls back to agent-browser.
pub struct AgentBrowserFetcher {
    backend: BrowserBackend,
}

enum BrowserBackend {
    Rsclaw,
    AgentBrowser { profile: Option<String> },
}

impl AgentBrowserFetcher {
    /// Create with auto-detection: rsclaw preferred, then agent-browser.
    pub fn new() -> Self {
        if Self::rsclaw_available() {
            Self { backend: BrowserBackend::Rsclaw }
        } else {
            Self {
                backend: BrowserBackend::AgentBrowser {
                    profile: Some("Default".to_owned()),
                },
            }
        }
    }

    /// Create without a Chrome profile (fresh session).
    pub fn headless() -> Self {
        if Self::rsclaw_available() {
            Self { backend: BrowserBackend::Rsclaw }
        } else {
            Self {
                backend: BrowserBackend::AgentBrowser { profile: None },
            }
        }
    }

    /// Create with a specific Chrome profile name (agent-browser only).
    pub fn with_profile(profile: impl Into<String>) -> Self {
        Self {
            backend: BrowserBackend::AgentBrowser {
                profile: Some(profile.into()),
            },
        }
    }

    /// Check if rsclaw browser is available.
    fn rsclaw_available() -> bool {
        std::process::Command::new("rsclaw")
            .args(["browser", "url"])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .map(|s| s.success())
            .unwrap_or(false)
    }

    /// Check if agent-browser is available on PATH.
    pub fn is_available() -> bool {
        Self::rsclaw_available()
            || std::process::Command::new("agent-browser")
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
        match &self.backend {
            BrowserBackend::Rsclaw => rsclaw_fetch(url).await,
            BrowserBackend::AgentBrowser { profile } => agent_browser_fetch(url, profile.as_deref()).await,
        }
    }

    async fn eval(&self, url: &str, js: &str) -> Result<String> {
        match &self.backend {
            BrowserBackend::Rsclaw => rsclaw_eval(url, js).await,
            BrowserBackend::AgentBrowser { profile } => agent_browser_eval(url, js, profile.as_deref()).await,
        }
    }

    async fn desktop_eval(&self, target: &str, js: &str) -> Result<String> {
        // Desktop eval only supported via agent-browser --cdp/--auto-connect
        let mut cmd = tokio::process::Command::new("agent-browser");
        if target.chars().all(|c| c.is_ascii_digit()) {
            cmd.args(["--cdp", target]);
        } else {
            cmd.arg("--auto-connect");
        }
        cmd.args(["eval", js]);

        let output = cmd.output().await.context("failed to run desktop eval")?;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            bail!("desktop eval failed: {stderr}");
        }
        String::from_utf8(output.stdout).context("output is not valid UTF-8")
    }

    async fn intercept(&self, url: &str, pattern: &str) -> Result<String> {
        match &self.backend {
            BrowserBackend::Rsclaw => rsclaw_intercept(url, pattern).await,
            BrowserBackend::AgentBrowser { profile } => agent_browser_intercept(url, pattern, profile.as_deref()).await,
        }
    }
}

/// Strip rsclaw's status prefix lines (e.g. "Connected to existing Chrome\n")
fn strip_rsclaw_prefix(s: &str) -> String {
    let mut content_start = 0;
    for line in s.lines() {
        if line.starts_with("Connected to")
            || line.starts_with("Navigated to")
            || line.starts_with("Launched")
        {
            content_start += line.len() + 1;
        } else {
            break;
        }
    }
    s[content_start.min(s.len())..].to_owned()
}

// ─── rsclaw browser backend ───────────────────────────────────────────────────

async fn rsclaw_fetch(url: &str) -> Result<String> {
    let output = tokio::process::Command::new("rsclaw")
        .args(["browser", "open", url])
        .output()
        .await
        .context("failed to run rsclaw browser open")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!("rsclaw browser open failed: {stderr}");
    }

    // Wait for rendering
    tokio::time::sleep(std::time::Duration::from_secs(2)).await;

    let content = tokio::process::Command::new("rsclaw")
        .args(["browser", "content"])
        .output()
        .await
        .context("failed to run rsclaw browser content")?;

    if !content.status.success() {
        let stderr = String::from_utf8_lossy(&content.stderr);
        bail!("rsclaw browser content failed: {stderr}");
    }

    let raw = String::from_utf8(content.stdout).context("output is not valid UTF-8")?;
    // Strip "Connected to existing Chrome\n" prefix if present
    Ok(strip_rsclaw_prefix(&raw))
}

async fn rsclaw_eval(url: &str, js: &str) -> Result<String> {
    let output = tokio::process::Command::new("rsclaw")
        .args(["browser", "open", url])
        .output()
        .await
        .context("failed to run rsclaw browser open")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!("rsclaw browser open failed: {stderr}");
    }

    // Wait for page load
    tokio::time::sleep(std::time::Duration::from_secs(2)).await;

    let eval_output = tokio::process::Command::new("rsclaw")
        .args(["browser", "evaluate", js])
        .output()
        .await
        .context("failed to run rsclaw browser evaluate")?;

    if !eval_output.status.success() {
        let stderr = String::from_utf8_lossy(&eval_output.stderr);
        bail!("rsclaw browser evaluate failed: {stderr}");
    }

    let raw = String::from_utf8(eval_output.stdout).context("output is not valid UTF-8")?;

    // rsclaw returns {"action":"evaluate","result":"..."} — extract the result field
    if let Ok(v) = serde_json::from_str::<serde_json::Value>(&raw) {
        if let Some(result) = v.get("result") {
            return match result {
                serde_json::Value::String(s) => Ok(s.clone()),
                other => Ok(other.to_string()),
            };
        }
    }
    Ok(raw)
}

async fn rsclaw_intercept(url: &str, _pattern: &str) -> Result<String> {
    // rsclaw doesn't have network interception yet; fall back to eval
    rsclaw_fetch(url).await
}

// ─── agent-browser backend ────────────────────────────────────────────────────

async fn agent_browser_fetch(url: &str, profile: Option<&str>) -> Result<String> {
    let mut cmd = tokio::process::Command::new("agent-browser");
    if let Some(p) = profile {
        cmd.args(["--profile", p]);
    }
    cmd.args(["open", url]);
    let output = cmd.output().await.context("failed to run agent-browser open")?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!("agent-browser open failed: {stderr}");
    }

    tokio::time::sleep(std::time::Duration::from_secs(2)).await;

    let get_output = tokio::process::Command::new("agent-browser")
        .args(["get", "html", "body"])
        .output()
        .await
        .context("failed to run agent-browser get html")?;

    if !get_output.status.success() {
        let stderr = String::from_utf8_lossy(&get_output.stderr);
        bail!("agent-browser get html failed: {stderr}");
    }

    String::from_utf8(get_output.stdout).context("output is not valid UTF-8")
}

async fn agent_browser_eval(url: &str, js: &str, profile: Option<&str>) -> Result<String> {
    let mut cmd = tokio::process::Command::new("agent-browser");
    if let Some(p) = profile {
        cmd.args(["--profile", p]);
    }
    cmd.args(["open", url]);
    let output = cmd.output().await.context("failed to run agent-browser open")?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!("agent-browser open failed: {stderr}");
    }

    tokio::time::sleep(std::time::Duration::from_secs(2)).await;

    let eval_output = tokio::process::Command::new("agent-browser")
        .args(["eval", js])
        .output()
        .await
        .context("failed to run agent-browser eval")?;

    if !eval_output.status.success() {
        let stderr = String::from_utf8_lossy(&eval_output.stderr);
        bail!("agent-browser eval failed: {stderr}");
    }

    String::from_utf8(eval_output.stdout).context("output is not valid UTF-8")
}

async fn agent_browser_intercept(url: &str, pattern: &str, profile: Option<&str>) -> Result<String> {
    let mut cmd = tokio::process::Command::new("agent-browser");
    if let Some(p) = profile {
        cmd.args(["--profile", p]);
    }
    cmd.args(["open", url]);
    cmd.output().await.context("failed to open page")?;

    tokio::time::sleep(std::time::Duration::from_secs(3)).await;

    let req_output = tokio::process::Command::new("agent-browser")
        .args(["network", "requests", "--filter", pattern])
        .output()
        .await
        .context("failed to get network requests")?;

    if !req_output.status.success() {
        let stderr = String::from_utf8_lossy(&req_output.stderr);
        bail!("agent-browser network intercept failed: {stderr}");
    }

    String::from_utf8(req_output.stdout).context("output is not valid UTF-8")
}
