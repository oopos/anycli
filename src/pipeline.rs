//! Pipeline engine — fetch, parse, extract, and format web data.

use std::collections::HashMap;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use regex::Regex;
use serde::Serialize;
use serde_json::{Value, json};
use tracing::debug;

use crate::adapter::{Adapter, Command, FieldDef, SourceFormat, Transform};
use crate::browser::{AgentBrowserFetcher, BrowserFetcher};
use crate::output::OutputFormat;

const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);
const USER_AGENT: &str = "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/120.0.0.0 Safari/537.36";

/// Result of executing an adapter command.
#[derive(Debug, Clone, Serialize)]
pub struct PipelineResult {
    /// Adapter name.
    pub adapter: String,
    /// Command name.
    pub command: String,
    /// Extracted items as JSON objects.
    pub items: Vec<Value>,
    /// Number of items.
    pub count: usize,
}

impl PipelineResult {
    /// Format the result in the specified output format.
    pub fn format(&self, fmt: OutputFormat) -> Result<String> {
        crate::output::format_result(self, fmt)
    }
}

/// The pipeline engine.
pub struct Pipeline {
    browser: Option<Box<dyn BrowserFetcher>>,
}

impl Pipeline {
    /// Create a pipeline with no browser support.
    /// `format: browser` adapters will use `agent-browser` CLI as fallback.
    pub fn new() -> Self {
        Self { browser: None }
    }

    /// Create a pipeline with a custom browser fetcher.
    /// Used by rsclaw to inject its CDP-based implementation.
    pub fn with_browser(fetcher: impl BrowserFetcher + 'static) -> Self {
        Self {
            browser: Some(Box::new(fetcher)),
        }
    }

    /// Execute an adapter command with the given parameters (static method for backwards compat).
    ///
    /// Parameters are passed as `(key, value)` pairs. The URL template
    /// `{param}` placeholders are replaced with actual values.
    pub async fn execute(
        adapter: &Adapter,
        command_name: &str,
        params: &[(&str, &str)],
    ) -> Result<PipelineResult> {
        let pipeline = Self::new();
        pipeline.run(adapter, command_name, params).await
    }

    /// Execute an adapter command using this pipeline instance.
    pub async fn run(
        &self,
        adapter: &Adapter,
        command_name: &str,
        params: &[(&str, &str)],
    ) -> Result<PipelineResult> {
        let cmd = adapter
            .commands
            .get(command_name)
            .with_context(|| {
                let available: Vec<&str> = adapter.commands.keys().map(|s| s.as_str()).collect();
                format!(
                    "command `{}` not found in adapter `{}`. available: {}",
                    command_name,
                    adapter.name,
                    available.join(", ")
                )
            })?;

        let param_map: HashMap<&str, &str> = params.iter().copied().collect();

        // Validate required params.
        for (name, def) in &cmd.params {
            if def.required && !param_map.contains_key(name.as_str()) {
                bail!("required parameter `{name}` not provided");
            }
        }

        // Build URL with param substitution.
        let url = build_url(&adapter.base_url, &cmd.url, &param_map, &cmd.params)?;
        debug!(url, adapter = adapter.name, command = command_name, "fetching");

        // Fetch.
        let body = match cmd.format {
            SourceFormat::Browser => self.browser_fetch(&url).await?,
            SourceFormat::BrowserApi => {
                let js = cmd.evaluate.as_deref()
                    .ok_or_else(|| anyhow::anyhow!("browser_api format requires an 'evaluate' field"))?;
                self.browser_eval(&url, js).await?
            }
            _ => fetch(&url, &cmd.headers).await?,
        };

        // Extract items.
        let mut items = if let Some(ref fetch_each) = cmd.fetch_each {
            // fetch_each mode: initial response is ID list, fetch each detail.
            let ids = extract_id_list(&body, cmd)?;

            // Apply limit before fetching details.
            let limit = param_map
                .get("limit")
                .and_then(|s| s.parse::<usize>().ok())
                .unwrap_or(ids.len());
            let ids = &ids[..limit.min(ids.len())];

            fetch_each_item(&adapter.base_url, fetch_each, ids, &cmd.headers).await?
        } else {
            match cmd.format {
                SourceFormat::Html | SourceFormat::Browser => extract_html(&body, cmd)?,
                SourceFormat::Json | SourceFormat::BrowserApi => extract_json(&body, cmd)?,
                SourceFormat::Xml => extract_xml(&body, cmd)?,
            }
        };

        // Apply limit (for non-fetch_each mode).
        if cmd.fetch_each.is_none() {
            if let Some(limit_str) = param_map.get("limit") {
                if let Ok(limit) = limit_str.parse::<usize>() {
                    items.truncate(limit);
                }
            }
        }

        let count = items.len();
        Ok(PipelineResult {
            adapter: adapter.name.clone(),
            command: command_name.to_owned(),
            items,
            count,
        })
    }

    /// Fetch a URL using the browser (injected fetcher or agent-browser CLI fallback).
    async fn browser_fetch(&self, url: &str) -> Result<String> {
        if let Some(ref fetcher) = self.browser {
            fetcher.fetch(url).await
        } else {
            let fallback = AgentBrowserFetcher::new();
            fallback.fetch(url).await
        }
    }

    /// Navigate to URL and evaluate JS in browser context.
    async fn browser_eval(&self, url: &str, js: &str) -> Result<String> {
        if let Some(ref fetcher) = self.browser {
            fetcher.eval(url, js).await
        } else {
            let fallback = AgentBrowserFetcher::new();
            fallback.eval(url, js).await
        }
    }
}

/// Build the full URL by substituting `{param}` placeholders.
fn build_url(
    base: &str,
    path: &str,
    params: &HashMap<&str, &str>,
    defs: &HashMap<String, crate::adapter::ParamDef>,
) -> Result<String> {
    let mut url_path = path.to_owned();

    // Substitute {param} placeholders.
    for (key, val) in params {
        let placeholder = format!("{{{key}}}");
        url_path = url_path.replace(&placeholder, val);
    }

    // Apply defaults for remaining placeholders.
    for (key, def) in defs {
        let placeholder = format!("{{{key}}}");
        if url_path.contains(&placeholder) {
            if let Some(ref default_val) = def.default {
                let s = match default_val {
                    Value::String(s) => s.clone(),
                    other => other.to_string(),
                };
                url_path = url_path.replace(&placeholder, &s);
            }
        }
    }

    // Check for unresolved placeholders.
    if url_path.contains('{') {
        bail!("unresolved placeholder in URL: {url_path}");
    }

    let base = base.trim_end_matches('/');
    if url_path.starts_with("http://") || url_path.starts_with("https://") {
        Ok(url_path)
    } else {
        Ok(format!("{base}{url_path}"))
    }
}

/// Fetch a URL and return the response body as text.
async fn fetch(url: &str, headers: &HashMap<String, String>) -> Result<String> {
    let client = reqwest::Client::builder()
        .timeout(REQUEST_TIMEOUT)
        .user_agent(USER_AGENT)
        .build()?;

    let mut req = client.get(url);
    for (k, v) in headers {
        req = req.header(k.as_str(), v.as_str());
    }

    let resp = req.send().await.with_context(|| format!("failed to fetch {url}"))?;
    let status = resp.status();
    if !status.is_success() {
        bail!("HTTP {status} from {url}");
    }

    resp.text().await.with_context(|| format!("failed to read body from {url}"))
}

/// Extract items from an HTML page using regex patterns.
fn extract_html(html: &str, cmd: &Command) -> Result<Vec<Value>> {
    let blocks = if let Some(ref selector) = cmd.selector {
        let re = Regex::new(&format!("(?s){selector}"))
            .with_context(|| format!("invalid selector regex: {selector}"))?;
        re.find_iter(html).map(|m| m.as_str().to_owned()).collect::<Vec<_>>()
    } else {
        vec![html.to_owned()]
    };

    let mut items = Vec::with_capacity(blocks.len());
    for block in &blocks {
        let mut obj = serde_json::Map::new();
        let mut has_value = false;

        for (field_name, field_def) in &cmd.fields {
            let val = extract_field_html(block, field_def)?;
            if val != Value::Null {
                has_value = true;
            }
            obj.insert(field_name.clone(), val);
        }

        if has_value {
            items.push(Value::Object(obj));
        }
    }

    Ok(items)
}

/// Extract a single field from an HTML block.
fn extract_field_html(block: &str, def: &FieldDef) -> Result<Value> {
    let raw = if let Some(ref pattern) = def.pattern {
        let re = Regex::new(&format!("(?s){pattern}"))
            .with_context(|| format!("invalid field pattern: {pattern}"))?;
        re.captures(block)
            .and_then(|c| c.get(1))
            .map(|m| m.as_str().to_owned())
    } else {
        None
    };

    let val = match raw {
        Some(s) => apply_transform(s, &def.transform),
        None => def.default.clone().unwrap_or_default(),
    };

    if val.is_empty() {
        Ok(Value::Null)
    } else {
        Ok(Value::String(val))
    }
}

/// Extract items from a JSON response.
fn extract_json(body: &str, cmd: &Command) -> Result<Vec<Value>> {
    let root: Value = serde_json::from_str(body).context("invalid JSON response")?;

    // If selector is provided, use it as a JSON path to find the array.
    let array = if let Some(ref selector) = cmd.selector {
        navigate_json(&root, selector)
            .and_then(|v| v.as_array().cloned())
            .unwrap_or_default()
    } else if let Some(arr) = root.as_array() {
        arr.clone()
    } else {
        vec![root.clone()]
    };

    let mut items = Vec::with_capacity(array.len());
    for element in &array {
        let mut obj = serde_json::Map::new();
        for (field_name, field_def) in &cmd.fields {
            let val = extract_field_json(element, field_def)?;
            obj.insert(field_name.clone(), val);
        }
        items.push(Value::Object(obj));
    }

    Ok(items)
}

/// Extract a single field from a JSON element.
fn extract_field_json(element: &Value, def: &FieldDef) -> Result<Value> {
    if let Some(ref path) = def.json_path {
        let val = navigate_json(element, path);
        match val {
            Some(v) if !v.is_null() => Ok(v.clone()),
            _ => match &def.default {
                Some(d) => Ok(json!(d)),
                None => Ok(Value::Null),
            },
        }
    } else {
        Ok(match &def.default {
            Some(d) => json!(d),
            None => Value::Null,
        })
    }
}

/// Navigate a JSON value by dot-separated path (e.g., "data.items" or "title").
fn navigate_json<'a>(val: &'a Value, path: &str) -> Option<&'a Value> {
    let mut current = val;
    for segment in path.split('.') {
        match current {
            Value::Object(map) => {
                current = map.get(segment)?;
            }
            Value::Array(arr) => {
                if let Ok(idx) = segment.parse::<usize>() {
                    current = arr.get(idx)?;
                } else {
                    return None;
                }
            }
            _ => return None,
        }
    }
    Some(current)
}

/// Extract a flat list of IDs from the initial response (for fetch_each mode).
fn extract_id_list(body: &str, cmd: &Command) -> Result<Vec<String>> {
    let root: Value = serde_json::from_str(body).context("invalid JSON response")?;

    let array = if let Some(ref selector) = cmd.selector {
        navigate_json(&root, selector)
            .and_then(|v| v.as_array().cloned())
            .unwrap_or_default()
    } else if let Some(arr) = root.as_array() {
        arr.clone()
    } else {
        vec![root]
    };

    Ok(array
        .iter()
        .map(|v| match v {
            Value::Number(n) => n.to_string(),
            Value::String(s) => s.clone(),
            other => other.to_string(),
        })
        .collect())
}

/// Fetch each item by ID and extract fields from the detail response.
/// Fetches all items concurrently for performance.
async fn fetch_each_item(
    base_url: &str,
    fe: &crate::adapter::FetchEach,
    ids: &[String],
    headers: &HashMap<String, String>,
) -> Result<Vec<Value>> {
    let base = base_url.trim_end_matches('/');

    // Build URLs for all IDs.
    let urls: Vec<String> = ids
        .iter()
        .map(|id| {
            let path = fe.url.replace("{id}", id);
            if path.starts_with("http://") || path.starts_with("https://") {
                path
            } else {
                format!("{base}{path}")
            }
        })
        .collect();

    // Fetch all concurrently.
    let fetches = urls.iter().map(|url| fetch(url, headers));
    let results = futures::future::join_all(fetches).await;

    // Extract fields from each response, preserving order.
    let mut items = Vec::with_capacity(ids.len());
    for result in results {
        let body = match result {
            Ok(b) => b,
            Err(_) => continue,
        };

        match fe.format {
            SourceFormat::Json => {
                let root: Value = match serde_json::from_str(&body) {
                    Ok(v) => v,
                    Err(_) => continue,
                };
                let mut obj = serde_json::Map::new();
                for (field_name, field_def) in &fe.fields {
                    let val = extract_field_json(&root, field_def)?;
                    obj.insert(field_name.clone(), val);
                }
                items.push(Value::Object(obj));
            }
            SourceFormat::Html | SourceFormat::Xml | SourceFormat::Browser | SourceFormat::BrowserApi => {
                let mut obj = serde_json::Map::new();
                for (field_name, field_def) in &fe.fields {
                    let val = extract_field_html(&body, field_def)?;
                    obj.insert(field_name.clone(), val);
                }
                items.push(Value::Object(obj));
            }
        }
    }

    Ok(items)
}

/// Extract items from an XML response (simple regex-based).
fn extract_xml(body: &str, cmd: &Command) -> Result<Vec<Value>> {
    // XML extraction reuses the HTML path — regex-based, no full parser.
    extract_html(body, cmd)
}

/// Apply a transform to an extracted string value.
fn apply_transform(val: String, transform: &Option<Transform>) -> String {
    match transform {
        None => val.trim().to_owned(),
        Some(Transform::Trim) => val.trim().to_owned(),
        Some(Transform::StripHtml) => strip_html(&val),
        Some(Transform::DecodeEntities) => decode_entities(&val),
        Some(Transform::ToNumber) => {
            // Keep only digits, dots, minus.
            val.chars().filter(|c| c.is_ascii_digit() || *c == '.' || *c == '-').collect()
        }
    }
}

/// Strip HTML tags from a string.
fn strip_html(s: &str) -> String {
    let re = Regex::new(r"<[^>]+>").expect("strip_html regex");
    let cleaned = re.replace_all(s, "");
    decode_entities(cleaned.trim())
}

/// Decode common HTML entities.
fn decode_entities(s: &str) -> String {
    s.replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&apos;", "'")
        .replace("&#x27;", "'")
        .replace("&nbsp;", " ")
}
