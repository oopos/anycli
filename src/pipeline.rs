//! Pipeline engine — fetch, parse, extract, and format web data.

use std::collections::HashMap;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use regex::Regex;
use serde::Serialize;
use serde_json::{Value, json};
use tracing::debug;

use crate::adapter::{Adapter, Command, FieldDef, SourceFormat, Transform};
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
pub struct Pipeline;

impl Pipeline {
    /// Execute an adapter command with the given parameters.
    ///
    /// Parameters are passed as `(key, value)` pairs. The URL template
    /// `{param}` placeholders are replaced with actual values.
    pub async fn execute(
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
        let body = fetch(&url, &cmd.headers).await?;

        // Extract items.
        let mut items = match cmd.format {
            SourceFormat::Html => extract_html(&body, cmd)?,
            SourceFormat::Json => extract_json(&body, cmd)?,
            SourceFormat::Xml => extract_xml(&body, cmd)?,
        };

        // Apply limit.
        if let Some(limit_str) = param_map.get("limit") {
            if let Ok(limit) = limit_str.parse::<usize>() {
                items.truncate(limit);
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
        let re = Regex::new(pattern)
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
