//! Pipeline engine — fetch, parse, extract, and format web data.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::Duration;

use anyhow::{Context, Result, bail};
use regex::Regex;
use serde::Serialize;
use serde_json::{Value, json};
use tracing::debug;

use crate::adapter::{Adapter, Command, FieldDef, ParamDef, SourceFormat, Transform};
use crate::browser::{AgentBrowserFetcher, BrowserFetcher};
use crate::output::OutputFormat;

const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);
const MAX_RETRIES: u32 = 3;
const USER_AGENT: &str = "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/120.0.0.0 Safari/537.36";

static VERBOSE: AtomicBool = AtomicBool::new(false);
static TIMEOUT_SECS: AtomicU64 = AtomicU64::new(0);

/// Enable verbose request logging (`-v` / `ANYCLI_VERBOSE`).
pub fn set_verbose(enabled: bool) {
    VERBOSE.store(enabled, Ordering::Relaxed);
}

/// Override the default request timeout (seconds). `0` restores the 30s client default.
pub fn set_timeout_secs(secs: u64) {
    TIMEOUT_SECS.store(secs, Ordering::Relaxed);
}

fn verbose_enabled() -> bool {
    VERBOSE.load(Ordering::Relaxed) || std::env::var_os("ANYCLI_VERBOSE").is_some()
}

fn global_timeout() -> Option<Duration> {
    let secs = TIMEOUT_SECS.load(Ordering::Relaxed);
    if secs == 0 {
        None
    } else {
        Some(Duration::from_secs(secs))
    }
}

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

    /// Keep only the requested columns, in the given order.
    pub fn project_fields(&mut self, fields: &[String]) {
        for item in &mut self.items {
            if let Value::Object(map) = item {
                let mut next = serde_json::Map::new();
                for key in fields {
                    if let Some(val) = map.get(key) {
                        next.insert(key.clone(), val.clone());
                    }
                }
                *map = next;
            }
        }
    }

    /// Sort items by a field. Numeric strings compare as numbers.
    pub fn sort_by(&mut self, field: &str, reverse: bool) {
        self.items.sort_by(|a, b| {
            let va = a.get(field).map(sort_key).unwrap_or_default();
            let vb = b.get(field).map(sort_key).unwrap_or_default();
            let ord = match (&va, &vb) {
                (SortKey::Num(x), SortKey::Num(y)) => x.partial_cmp(y).unwrap_or(std::cmp::Ordering::Equal),
                _ => va.as_str().cmp(vb.as_str()),
            };
            if reverse { ord.reverse() } else { ord }
        });
        self.count = self.items.len();
    }

    /// Drop the first `n` items.
    pub fn skip(&mut self, n: usize) {
        let n = n.min(self.items.len());
        self.items.drain(..n);
        self.count = self.items.len();
    }

    /// Keep at most `n` items.
    pub fn truncate(&mut self, n: usize) {
        self.items.truncate(n);
        self.count = self.items.len();
    }

    /// Keep the first occurrence of each value in `field`.
    pub fn unique_by(&mut self, field: &str) {
        let mut seen = std::collections::HashSet::new();
        self.items.retain(|item| {
            let key = item.get(field).map(json_to_plain).unwrap_or_default();
            seen.insert(key)
        });
        self.count = self.items.len();
    }
}

#[derive(Clone)]
enum SortKey {
    Num(f64),
    Str(String),
}

impl SortKey {
    fn as_str(&self) -> &str {
        match self {
            SortKey::Str(s) => s,
            SortKey::Num(_) => "",
        }
    }
}

impl Default for SortKey {
    fn default() -> Self {
        SortKey::Str(String::new())
    }
}

fn sort_key(v: &Value) -> SortKey {
    match v {
        Value::Number(n) => n.as_f64().map(SortKey::Num).unwrap_or_else(|| SortKey::Str(n.to_string())),
        Value::String(s) => {
            if let Ok(n) = s.parse::<f64>() {
                SortKey::Num(n)
            } else {
                SortKey::Str(s.clone())
            }
        }
        Value::Bool(b) => SortKey::Str(b.to_string()),
        Value::Null => SortKey::Str(String::new()),
        other => SortKey::Str(other.to_string()),
    }
}

/// The pipeline engine.
pub struct Pipeline {
    browser: Option<Box<dyn BrowserFetcher>>,
    client: reqwest::Client,
}

impl Pipeline {
    /// Create a pipeline with no browser support.
    /// `format: browser` adapters will use `agent-browser` CLI as fallback.
    pub fn new() -> Self {
        Self {
            browser: None,
            client: build_client().unwrap_or_else(|_| reqwest::Client::new()),
        }
    }

    /// Create a pipeline with a custom browser fetcher.
    /// Used by rsclaw to inject its CDP-based implementation.
    pub fn with_browser(fetcher: impl BrowserFetcher + 'static) -> Self {
        Self {
            browser: Some(Box::new(fetcher)),
            client: build_client().unwrap_or_else(|_| reqwest::Client::new()),
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
        let (command_name, cmd) = adapter.command(command_name).ok_or_else(|| {
            let available: Vec<&str> = adapter.commands.keys().map(|s| s.as_str()).collect();
            let hint = suggest(command_name, available.iter().copied());
            anyhow::anyhow!(
                "command `{}` not found in adapter `{}`. available: {}{}",
                command_name,
                adapter.name,
                available.join(", "),
                hint
            )
        })?;

        let param_map = resolve_params(&cmd.params, params)?;

        // Build URL with param substitution.
        let url = build_url(&adapter.base_url, &cmd.url, &param_map, &cmd.params)?;
        debug!(url, adapter = adapter.name, command = command_name, "fetching");

        let timeout = cmd
            .timeout
            .map(Duration::from_secs)
            .or_else(global_timeout);
        let method = http_method(cmd.method.as_deref(), cmd.body.is_some());
        let body = cmd
            .body
            .as_ref()
            .map(|b| substitute_json(b, &param_map));

        // Fetch (or use inline static data).
        let response_body = match cmd.format {
            SourceFormat::Static => String::new(),
            SourceFormat::Browser => self.browser_fetch(&url).await?,
            SourceFormat::BrowserApi => {
                let js = cmd
                    .evaluate
                    .as_deref()
                    .ok_or_else(|| anyhow::anyhow!("browser_api format requires an 'evaluate' field"))?;
                self.browser_eval(&url, &substitute_eval(js, &param_map))
                    .await?
            }
            SourceFormat::Desktop => {
                let js = cmd
                    .evaluate
                    .as_deref()
                    .ok_or_else(|| anyhow::anyhow!("desktop format requires an 'evaluate' field"))?;
                let target = cmd.cdp_target.as_deref().unwrap_or("auto");
                self.desktop_eval(target, &substitute_eval(js, &param_map))
                    .await?
            }
            SourceFormat::Intercept => {
                let pattern = cmd.intercept_pattern.as_deref().ok_or_else(|| {
                    anyhow::anyhow!("intercept format requires an 'intercept_pattern' field")
                })?;
                self.browser_intercept(&url, pattern).await?
            }
            _ => {
                self.fetch(
                    &url,
                    &cmd.headers,
                    method,
                    body.as_ref(),
                    cmd.content_type.as_deref(),
                    timeout,
                )
                .await?
            }
        };

        // Extract items.
        let mut items = if cmd.format == SourceFormat::Static {
            extract_static(cmd, &param_map)?
        } else if let Some(ref fetch_each) = cmd.fetch_each {
            let ids = extract_id_list(&response_body, cmd)?;

            let limit = param_map
                .get("limit")
                .and_then(|s| s.parse::<usize>().ok())
                .unwrap_or(ids.len());
            let ids = &ids[..limit.min(ids.len())];

            fetch_each_item(
                &self.client,
                &adapter.base_url,
                fetch_each,
                ids,
                &cmd.headers,
                timeout,
            )
            .await?
        } else {
            match cmd.format {
                SourceFormat::Html | SourceFormat::Browser => extract_html(&response_body, cmd)?,
                SourceFormat::Json
                | SourceFormat::BrowserApi
                | SourceFormat::Desktop
                | SourceFormat::Intercept => extract_json(&response_body, cmd, &param_map)?,
                SourceFormat::Xml => extract_xml(&response_body, cmd)?,
                SourceFormat::Static => extract_static(cmd, &param_map)?,
            }
        };

        if cmd.skip > 0 {
            let n = cmd.skip.min(items.len());
            items.drain(..n);
        }

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

    async fn fetch(
        &self,
        url: &str,
        headers: &HashMap<String, String>,
        method: &str,
        body: Option<&Value>,
        content_type: Option<&str>,
        timeout: Option<Duration>,
    ) -> Result<String> {
        fetch_with_client(
            &self.client,
            url,
            headers,
            method,
            body,
            content_type,
            timeout,
        )
        .await
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

    /// Connect to desktop app via CDP and evaluate JS.
    async fn desktop_eval(&self, target: &str, js: &str) -> Result<String> {
        if let Some(ref fetcher) = self.browser {
            fetcher.desktop_eval(target, js).await
        } else {
            let fallback = AgentBrowserFetcher::new();
            fallback.desktop_eval(target, js).await
        }
    }

    /// Navigate to URL and intercept network response matching pattern.
    async fn browser_intercept(&self, url: &str, pattern: &str) -> Result<String> {
        if let Some(ref fetcher) = self.browser {
            fetcher.intercept(url, pattern).await
        } else {
            let fallback = AgentBrowserFetcher::new();
            fallback.intercept(url, pattern).await
        }
    }
}

impl Default for Pipeline {
    fn default() -> Self {
        Self::new()
    }
}

fn build_client() -> Result<reqwest::Client> {
    Ok(reqwest::Client::builder()
        .timeout(REQUEST_TIMEOUT)
        .user_agent(USER_AGENT)
        .gzip(true)
        .brotli(true)
        .deflate(true)
        .build()?)
}

/// Merge defaults, apply overrides, and validate required params / choices.
pub(crate) fn resolve_params(
    defs: &indexmap::IndexMap<String, ParamDef>,
    params: &[(&str, &str)],
) -> Result<HashMap<String, String>> {
    let mut map = HashMap::new();

    for (name, def) in defs {
        if let Some(ref default_val) = def.default {
            map.insert(name.clone(), json_to_plain(default_val));
        }
    }

    for (key, val) in params {
        map.insert((*key).to_owned(), (*val).to_owned());
    }

    for (name, def) in defs {
        if def.required && !map.contains_key(name) {
            bail!("required parameter `{name}` not provided");
        }
        if !def.choices.is_empty() {
            if let Some(val) = map.get(name) {
                if !def.choices.iter().any(|c| c == val) {
                    bail!(
                        "parameter `{name}` must be one of: {}",
                        def.choices.join(", ")
                    );
                }
            }
        }
    }

    Ok(map)
}

fn json_to_plain(val: &Value) -> String {
    match val {
        Value::String(s) => s.clone(),
        Value::Number(n) => n.to_string(),
        Value::Bool(b) => b.to_string(),
        Value::Null => String::new(),
        other => other.to_string(),
    }
}

fn http_method(method: Option<&str>, has_body: bool) -> &'static str {
    match method.map(|m| m.to_ascii_uppercase()) {
        Some(m) if m == "POST" => "POST",
        Some(m) if m == "PUT" => "PUT",
        Some(m) if m == "PATCH" => "PATCH",
        Some(m) if m == "DELETE" => "DELETE",
        Some(_) => "GET",
        None if has_body => "POST",
        None => "GET",
    }
}

/// Build the full URL by substituting `{param}` placeholders.
pub(crate) fn build_url(
    base: &str,
    path: &str,
    params: &HashMap<String, String>,
    defs: &indexmap::IndexMap<String, ParamDef>,
) -> Result<String> {
    let mut url_path = substitute_known(path, params, true);

    // Apply defaults for remaining placeholders (already in params via resolve_params,
    // but keep this for callers that skip resolve_params).
    for (key, def) in defs {
        let placeholder = format!("{{{key}}}");
        if url_path.contains(&placeholder) {
            if let Some(ref default_val) = def.default {
                url_path = url_path.replace(&placeholder, &encode_url_value(&json_to_plain(default_val)));
            }
        }
    }

    // Check for unresolved *known-style* placeholders that still look like params.
    if let Some(missing) = first_unresolved_param(&url_path, defs) {
        bail!("unresolved placeholder `{{{missing}}}` in URL: {url_path}");
    }

    let base = base.trim_end_matches('/');
    if url_path.starts_with("http://") || url_path.starts_with("https://") {
        Ok(url_path)
    } else if url_path.is_empty() {
        Ok(base.to_owned())
    } else {
        Ok(format!("{base}{url_path}"))
    }
}

fn first_unresolved_param(
    url: &str,
    defs: &indexmap::IndexMap<String, ParamDef>,
) -> Option<String> {
    for (key, _) in defs {
        if url.contains(&format!("{{{key}}}")) {
            return Some(key.clone());
        }
    }
    None
}

/// Replace `{name}` for known params only (so GraphQL `{ posts { title } }` is left intact).
fn substitute_known(input: &str, params: &HashMap<String, String>, encode: bool) -> String {
    let mut out = input.to_owned();
    // Longer keys first so `{limit}` is not partially eaten by a shorter name.
    let mut keys: Vec<&String> = params.keys().collect();
    keys.sort_by_key(|k| std::cmp::Reverse(k.len()));
    for key in keys {
        let placeholder = format!("{{{key}}}");
        if !out.contains(&placeholder) {
            continue;
        }
        let value = if encode {
            encode_url_value(&params[key])
        } else {
            params[key].clone()
        };
        out = out.replace(&placeholder, &value);
    }
    out
}

fn encode_url_value(val: &str) -> String {
    if val.starts_with("http://") || val.starts_with("https://") {
        val.to_owned()
    } else {
        urlencoding::encode(val).into_owned()
    }
}

fn substitute_json(value: &Value, params: &HashMap<String, String>) -> Value {
    match value {
        Value::String(s) => {
            let replaced = substitute_known(s, params, false);
            if replaced == *s {
                Value::String(s.clone())
            } else if let Ok(n) = replaced.parse::<i64>() {
                json!(n)
            } else if let Ok(n) = replaced.parse::<f64>() {
                json!(n)
            } else {
                Value::String(replaced)
            }
        }
        Value::Array(arr) => Value::Array(arr.iter().map(|v| substitute_json(v, params)).collect()),
        Value::Object(map) => {
            let mut next = serde_json::Map::new();
            for (k, v) in map {
                next.insert(k.clone(), substitute_json(v, params));
            }
            Value::Object(next)
        }
        other => other.clone(),
    }
}

/// Substitute `${{param}}` and `{param}` in browser/desktop JS snippets.
pub(crate) fn substitute_eval(js: &str, params: &HashMap<String, String>) -> String {
    let mut out = js.to_owned();
    let mut keys: Vec<&String> = params.keys().collect();
    keys.sort_by_key(|k| std::cmp::Reverse(k.len()));
    for key in keys {
        let escaped = js_escape(&params[key]);
        out = out.replace(&format!("${{{{{key}}}}}"), &escaped);
        out = out.replace(&format!("{{{key}}}"), &escaped);
    }
    out
}

fn js_escape(s: &str) -> String {
    s.replace('\\', "\\\\")
        .replace('\'', "\\'")
        .replace('"', "\\\"")
        .replace('\n', "\\n")
        .replace('`', "\\`")
}

async fn fetch_with_client(
    client: &reqwest::Client,
    url: &str,
    headers: &HashMap<String, String>,
    method: &str,
    body: Option<&Value>,
    content_type: Option<&str>,
    timeout: Option<Duration>,
) -> Result<String> {
    let mut last_err: Option<anyhow::Error> = None;

    for attempt in 0..MAX_RETRIES {
        if verbose_enabled() {
            let extra = if attempt > 0 {
                format!(" (retry {attempt})")
            } else {
                String::new()
            };
            eprintln!("{method} {url}{extra}");
        }

        match fetch_once(client, url, headers, method, body, content_type, timeout).await {
            Ok(text) => return Ok(text),
            Err(e) => {
                let retry = is_retryable(&e) && attempt + 1 < MAX_RETRIES;
                last_err = Some(e);
                if !retry {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(250 * 2u64.pow(attempt))).await;
            }
        }
    }

    Err(last_err.unwrap_or_else(|| anyhow::anyhow!("failed to fetch {url}")))
}

fn is_retryable(err: &anyhow::Error) -> bool {
    let msg = format!("{err:#}");
    msg.contains("HTTP 429")
        || msg.contains("HTTP 500")
        || msg.contains("HTTP 502")
        || msg.contains("HTTP 503")
        || msg.contains("HTTP 504")
        || msg.contains("failed to fetch")
        || msg.contains("timed out")
        || msg.contains("error sending request")
}

async fn fetch_once(
    client: &reqwest::Client,
    url: &str,
    headers: &HashMap<String, String>,
    method: &str,
    body: Option<&Value>,
    content_type: Option<&str>,
    timeout: Option<Duration>,
) -> Result<String> {
    let mut req = match method {
        "POST" => client.post(url),
        "PUT" => client.put(url),
        "PATCH" => client.patch(url),
        "DELETE" => client.delete(url),
        _ => client.get(url),
    };

    if let Some(t) = timeout {
        req = req.timeout(t);
    }

    let mut has_content_type = false;
    let mut has_accept = false;
    for (k, v) in headers {
        if k.eq_ignore_ascii_case("content-type") {
            has_content_type = true;
        }
        if k.eq_ignore_ascii_case("accept") {
            has_accept = true;
        }
        req = req.header(k.as_str(), v.as_str());
    }

    if !has_accept {
        req = req.header("Accept", "application/json, text/html;q=0.9, */*;q=0.8");
    }

    if let Some(body) = body {
        if let Some(ct) = content_type {
            if !has_content_type {
                req = req.header("Content-Type", ct);
            }
        } else if !has_content_type {
            req = req.header("Content-Type", "application/json");
        }
        req = req.json(body);
    }

    let resp = req
        .send()
        .await
        .with_context(|| format!("failed to fetch {url}"))?;
    let status = resp.status();
    let text = resp
        .text()
        .await
        .with_context(|| format!("failed to read body from {url}"))?;
    if !status.is_success() {
        let snippet: String = text.chars().take(240).collect::<String>().replace('\n', " ");
        if snippet.is_empty() {
            bail!("HTTP {status} from {url}");
        }
        bail!("HTTP {status} from {url}: {snippet}");
    }

    Ok(text)
}

/// Extract items from an HTML page using regex patterns.
fn extract_html(html: &str, cmd: &Command) -> Result<Vec<Value>> {
    let blocks = if let Some(ref selector) = cmd.selector {
        let re = Regex::new(&format!("(?s){selector}"))
            .with_context(|| format!("invalid selector regex: {selector}"))?;
        re.find_iter(html)
            .map(|m| m.as_str().to_owned())
            .collect::<Vec<_>>()
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

/// Return inline YAML `data` for static commands, optionally filtered by params.
fn extract_static(cmd: &Command, params: &HashMap<String, String>) -> Result<Vec<Value>> {
    let data = cmd.data.clone().unwrap_or(json!([]));
    let mut items = match data {
        Value::Array(arr) => arr,
        other => vec![other],
    };

    if !cmd.fields.is_empty() {
        let mut projected = Vec::with_capacity(items.len());
        for (index, element) in items.iter().enumerate() {
            projected.push(extract_object(element, &cmd.fields, params, index)?);
        }
        items = projected;
    } else if !cmd.columns.is_empty() {
        items = items
            .into_iter()
            .map(|item| {
                if let Value::Object(map) = item {
                    let mut next = serde_json::Map::new();
                    for col in &cmd.columns {
                        if let Some(val) = map.get(col) {
                            next.insert(col.clone(), val.clone());
                        }
                    }
                    Value::Object(next)
                } else {
                    item
                }
            })
            .collect();
    }

    for (key, val) in params {
        if key == "limit" || val.eq_ignore_ascii_case("all") {
            continue;
        }
        if !cmd.params.contains_key(key) {
            continue;
        }
        items.retain(|item| match item.get(key) {
            Some(v) => json_to_plain(v) == *val,
            None => true,
        });
    }

    Ok(items)
}

/// Extract items from a JSON response.
fn extract_json(
    body: &str,
    cmd: &Command,
    params: &HashMap<String, String>,
) -> Result<Vec<Value>> {
    let root: Value = serde_json::from_str(body).context("invalid JSON response")?;
    extract_json_value(&root, cmd, params)
}

fn extract_json_value(
    root: &Value,
    cmd: &Command,
    params: &HashMap<String, String>,
) -> Result<Vec<Value>> {
    let array = if let Some(ref selector) = cmd.selector {
        if selector == "$" || selector == "." {
            vec![root.clone()]
        } else {
            match resolve_json(root, selector) {
                Some(Value::Array(arr)) => arr,
                Some(other) => vec![other],
                None => Vec::new(),
            }
        }
    } else if let Some(arr) = root.as_array() {
        arr.clone()
    } else {
        vec![root.clone()]
    };

    let mut items = Vec::with_capacity(array.len());
    for (index, element) in array.iter().enumerate() {
        let item = extract_object(element, &cmd.fields, params, index)?;
        if !is_blank_item(&item) {
            items.push(item);
        }
    }

    Ok(items)
}

fn extract_object(
    element: &Value,
    fields: &indexmap::IndexMap<String, FieldDef>,
    params: &HashMap<String, String>,
    index: usize,
) -> Result<Value> {
    let mut obj = serde_json::Map::new();

    for (field_name, field_def) in fields {
        if field_def.template.is_some() && field_def.json_path.is_none() && field_def.alt_paths.is_empty()
        {
            continue;
        }
        let val = extract_field_json(element, field_def, index)?;
        obj.insert(field_name.clone(), val);
    }

    for (field_name, field_def) in fields {
        if let Some(ref tpl) = field_def.template {
            let rendered = render_template(tpl, element, &obj, params);
            obj.insert(field_name.clone(), Value::String(rendered));
        }
    }

    Ok(Value::Object(obj))
}

fn is_blank_item(v: &Value) -> bool {
    match v.as_object() {
        Some(map) if !map.is_empty() => map.values().all(|x| match x {
            Value::Null => true,
            Value::String(s) => s.is_empty(),
            _ => false,
        }),
        _ => false,
    }
}

/// Extract a single field from a JSON element.
fn extract_field_json(element: &Value, def: &FieldDef, index: usize) -> Result<Value> {
    let mut paths: Vec<&str> = Vec::new();
    if let Some(ref path) = def.json_path {
        paths.push(path.as_str());
    }
    for path in &def.alt_paths {
        paths.push(path.as_str());
    }

    for path in paths {
        if path == "@index" {
            return Ok(apply_transform_value(json!(index), &def.transform));
        }
        if let Some(v) = resolve_json(element, path) {
            if !v.is_null() {
                return Ok(apply_transform_value(v, &def.transform));
            }
        }
    }

    Ok(match &def.default {
        Some(d) => apply_transform_value(json!(d), &def.transform),
        None => Value::Null,
    })
}

/// Navigate a JSON value by a path (borrowed). Filter expressions (`[?k==v]`)
/// cannot be borrowed and return `None`; use [`resolve_json`] for those.
#[cfg(test)]
pub(crate) fn navigate_json<'a>(val: &'a Value, path: &str) -> Option<&'a Value> {
    if path.is_empty() || path == "[]" || path == "$" || path == "." {
        return Some(val);
    }
    let mut current = val;
    for token in path_tokens(path)? {
        current = match token {
            PathToken::Skip => current,
            PathToken::Filter { .. } => return None,
            PathToken::Key(key) => match current {
                Value::Object(map) => map.get(key)?,
                Value::Array(arr) => {
                    let idx: usize = key.parse().ok()?;
                    arr.get(idx)?
                }
                _ => return None,
            },
            PathToken::Index(idx) => current.as_array()?.get(idx)?,
        };
    }
    Some(current)
}

/// Like [`navigate_json`], but returns an owned value so JSONPath filters
/// (`results[?kind=='podcast-episode']`) can produce a new array.
fn resolve_json(val: &Value, path: &str) -> Option<Value> {
    if path.is_empty() || path == "[]" || path == "$" || path == "." {
        return Some(val.clone());
    }
    let mut current = val.clone();
    for token in path_tokens(path)? {
        current = match token {
            PathToken::Skip => current,
            PathToken::Key(key) => match &current {
                Value::Object(map) => map.get(key)?.clone(),
                Value::Array(arr) => {
                    if let Ok(idx) = key.parse::<usize>() {
                        arr.get(idx)?.clone()
                    } else {
                        Value::Array(
                            arr.iter()
                                .filter_map(|item| match item {
                                    Value::Object(map) => map.get(key).cloned(),
                                    _ => None,
                                })
                                .collect(),
                        )
                    }
                }
                _ => return None,
            },
            PathToken::Index(idx) => current.as_array()?.get(idx)?.clone(),
            PathToken::Filter { key, value } => {
                let arr = current.as_array()?;
                Value::Array(
                    arr.iter()
                        .filter(|item| json_matches_filter(item, key, value))
                        .cloned()
                        .collect(),
                )
            }
        };
    }
    Some(current)
}

fn json_matches_filter(item: &Value, key: &str, expected: &str) -> bool {
    item.get(key).is_some_and(|v| json_to_plain(v) == expected)
}

#[derive(Debug, Clone, Copy)]
enum PathToken<'a> {
    Skip,
    Key(&'a str),
    Index(usize),
    Filter { key: &'a str, value: &'a str },
}

fn path_tokens(path: &str) -> Option<Vec<PathToken<'_>>> {
    let bytes = path.as_bytes();
    let mut tokens = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'.' {
            i += 1;
            continue;
        }
        if bytes[i] == b'[' {
            let close = path[i + 1..].find(']')?;
            let inner = &path[i + 1..i + 1 + close];
            if inner.is_empty() {
                tokens.push(PathToken::Skip);
            } else if let Some(filter) = parse_filter(inner) {
                tokens.push(filter);
            } else {
                tokens.push(PathToken::Index(inner.parse().ok()?));
            }
            i = i + 2 + close;
            continue;
        }
        let rest = &path[i..];
        let len = rest.find(['.', '[']).unwrap_or(rest.len());
        if len == 0 {
            return None;
        }
        tokens.push(PathToken::Key(&path[i..i + len]));
        i += len;
    }
    Some(tokens)
}

fn parse_filter(inner: &str) -> Option<PathToken<'_>> {
    let expr = inner.strip_prefix('?')?.trim();
    let (key, raw) = expr.split_once("==")?;
    let key = key.trim();
    if key.is_empty() {
        return None;
    }
    Some(PathToken::Filter {
        key,
        value: unquote(raw.trim()),
    })
}

fn unquote(s: &str) -> &str {
    if s.len() >= 2
        && ((s.starts_with('\'') && s.ends_with('\'')) || (s.starts_with('"') && s.ends_with('"')))
    {
        &s[1..s.len() - 1]
    } else {
        s
    }
}

fn render_template(
    tpl: &str,
    element: &Value,
    obj: &serde_json::Map<String, Value>,
    params: &HashMap<String, String>,
) -> String {
    let re = Regex::new(r"\{([A-Za-z0-9_-]+)\}").expect("template regex");
    re.replace_all(tpl, |caps: &regex::Captures| {
        let key = &caps[1];
        if let Some(v) = obj.get(key) {
            if !v.is_null() {
                return json_to_plain(v);
            }
        }
        if let Some(v) = element.get(key) {
            if !v.is_null() {
                return json_to_plain(v);
            }
        }
        if let Some(v) = params.get(key) {
            return v.clone();
        }
        String::new()
    })
    .into_owned()
}

/// Extract a flat list of IDs from the initial response (for fetch_each mode).
fn extract_id_list(body: &str, cmd: &Command) -> Result<Vec<String>> {
    let root: Value = serde_json::from_str(body).context("invalid JSON response")?;

    let array = if let Some(ref selector) = cmd.selector {
        match resolve_json(&root, selector) {
            Some(Value::Array(arr)) => arr,
            Some(other) => vec![other],
            None => Vec::new(),
        }
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
    client: &reqwest::Client,
    base_url: &str,
    fe: &crate::adapter::FetchEach,
    ids: &[String],
    headers: &HashMap<String, String>,
    timeout: Option<Duration>,
) -> Result<Vec<Value>> {
    let base = base_url.trim_end_matches('/');

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

    let fetches = urls.iter().map(|url| {
        fetch_with_client(client, url, headers, "GET", None, None, timeout)
    });
    let results = futures::future::join_all(fetches).await;
    let empty_params = HashMap::new();

    let mut items = Vec::with_capacity(ids.len());
    let mut last_err: Option<anyhow::Error> = None;
    let mut failures = 0usize;
    for result in results {
        let body = match result {
            Ok(b) => b,
            Err(e) => {
                failures += 1;
                last_err = Some(e);
                continue;
            }
        };

        match fe.format {
            SourceFormat::Json => {
                let root: Value = match serde_json::from_str(&body) {
                    Ok(v) => v,
                    Err(e) => {
                        failures += 1;
                        last_err = Some(e.into());
                        continue;
                    }
                };
                items.push(extract_object(&root, &fe.fields, &empty_params, items.len())?);
            }
            _ => {
                let mut obj = serde_json::Map::new();
                for (field_name, field_def) in &fe.fields {
                    let val = extract_field_html(&body, field_def)?;
                    obj.insert(field_name.clone(), val);
                }
                items.push(Value::Object(obj));
            }
        }
    }

    if items.is_empty() && failures > 0 {
        return Err(last_err.unwrap_or_else(|| anyhow::anyhow!("all fetch_each requests failed")))
            .with_context(|| format!("all {failures} fetch_each requests failed"));
    }

    Ok(items)
}

/// Extract items from an XML response (simple regex-based).
fn extract_xml(body: &str, cmd: &Command) -> Result<Vec<Value>> {
    extract_html(body, cmd)
}

/// Apply a transform to an extracted string value.
fn apply_transform(val: String, transform: &Option<Transform>) -> String {
    match apply_transform_value(Value::String(val), transform) {
        Value::String(s) => s,
        other => json_to_plain(&other),
    }
}

fn apply_transform_value(val: Value, transform: &Option<Transform>) -> Value {
    match transform {
        None => val,
        Some(Transform::Trim) => Value::String(json_to_plain(&val).trim().to_owned()),
        Some(Transform::StripHtml) => Value::String(strip_html(&json_to_plain(&val))),
        Some(Transform::DecodeEntities) => Value::String(decode_entities(&json_to_plain(&val))),
        Some(Transform::ToNumber) => {
            let digits: String = json_to_plain(&val)
                .chars()
                .filter(|c| c.is_ascii_digit() || *c == '.' || *c == '-')
                .collect();
            if let Ok(n) = digits.parse::<i64>() {
                json!(n)
            } else if let Ok(n) = digits.parse::<f64>() {
                json!(n)
            } else {
                Value::String(digits)
            }
        }
        Some(Transform::AddOne) => {
            let n = json_to_plain(&val).parse::<i64>().unwrap_or(0) + 1;
            json!(n)
        }
        Some(Transform::Join) => match val {
            Value::Array(arr) => Value::String(
                arr.iter()
                    .map(json_to_plain)
                    .filter(|s| !s.is_empty() && s != "null")
                    .collect::<Vec<_>>()
                    .join(", "),
            ),
            other => Value::String(json_to_plain(&other)),
        },
        Some(Transform::First) => match val {
            Value::Array(arr) => arr
                .into_iter()
                .find(|v| match v {
                    Value::Null => false,
                    Value::String(s) if s.is_empty() => false,
                    _ => true,
                })
                .unwrap_or(Value::Null),
            other => other,
        },
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

/// Suggest similar names when a command/adapter is missing.
pub fn suggest<'a>(needle: &str, available: impl Iterator<Item = &'a str>) -> String {
    let n = needle.to_lowercase();
    let mut scored: Vec<(&str, usize)> = available
        .map(|name| {
            let h = name.to_lowercase();
            let dist = levenshtein(&n, &h);
            (name, dist)
        })
        .filter(|(name, d)| *d <= 3 || (n.len() >= 3 && name.to_lowercase().contains(&n)))
        .collect();
    scored.sort_by_key(|(_, d)| *d);
    let names: Vec<&str> = scored.into_iter().take(5).map(|(n, _)| n).collect();
    if names.is_empty() {
        String::new()
    } else {
        format!(". did you mean: {}?", names.join(", "))
    }
}

fn levenshtein(a: &str, b: &str) -> usize {
    let a: Vec<char> = a.chars().collect();
    let b: Vec<char> = b.chars().collect();
    let mut prev: Vec<usize> = (0..=b.len()).collect();
    for (i, ca) in a.iter().enumerate() {
        let mut cur = vec![i + 1];
        for (j, cb) in b.iter().enumerate() {
            let cost = if ca == cb { 0 } else { 1 };
            cur.push((prev[j + 1] + 1).min(cur[j] + 1).min(prev[j] + cost));
        }
        prev = cur;
    }
    prev[b.len()]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::adapter::ParamDef;
    use indexmap::IndexMap;

    fn param(default: Option<&str>, required: bool) -> ParamDef {
        ParamDef {
            param_type: "string".into(),
            required,
            default: default.map(|d| json!(d)),
            description: None,
            positional: false,
            choices: vec![],
        }
    }

    #[test]
    fn resolve_params_applies_defaults() {
        let mut defs = IndexMap::new();
        defs.insert("limit".into(), param(Some("10"), false));
        defs.insert("query".into(), param(None, true));
        let map = resolve_params(&defs, &[("query", "rust")]).unwrap();
        assert_eq!(map.get("limit").unwrap(), "10");
        assert_eq!(map.get("query").unwrap(), "rust");
    }

    #[test]
    fn build_url_encodes_query_values() {
        let mut defs = IndexMap::new();
        defs.insert("query".into(), param(None, true));
        let mut params = HashMap::new();
        params.insert("query".into(), "large language model".into());
        let url = build_url(
            "https://example.com",
            "/search?q={query}",
            &params,
            &defs,
        )
        .unwrap();
        assert_eq!(url, "https://example.com/search?q=large%20language%20model");
    }

    #[test]
    fn build_url_keeps_full_urls_unencoded() {
        let defs = IndexMap::new();
        let mut params = HashMap::new();
        params.insert(
            "url".into(),
            "https://mp.weixin.qq.com/s/abc".into(),
        );
        let url = build_url("", "{url}", &params, &defs).unwrap();
        assert_eq!(url, "https://mp.weixin.qq.com/s/abc");
    }

    #[test]
    fn json_path_skips_array_marker() {
        let v = json!({"eid": "abc", "title": "hello"});
        assert_eq!(navigate_json(&v, "[].eid").unwrap(), &json!("abc"));
        assert_eq!(navigate_json(&v, "title").unwrap(), &json!("hello"));
    }

    #[test]
    fn json_path_bracket_indices() {
        let desc = json!({"weatherDesc": [{"value": "Sunny"}]});
        assert_eq!(
            navigate_json(&desc, "weatherDesc[0].value").unwrap(),
            &json!("Sunny")
        );
        let kline = json!(["t", "1.0", "2.0", "0.5", "1.5"]);
        assert_eq!(navigate_json(&kline, "[1]").unwrap(), &json!("1.0"));
        let nested = json!({"hourly": [null, null, null, null, {"weatherDesc": [{"value": "Rain"}]}]});
        assert_eq!(
            navigate_json(&nested, "hourly[4].weatherDesc[0].value").unwrap(),
            &json!("Rain")
        );
    }

    #[test]
    fn extract_json_applies_strip_html_and_templates() {
        let yaml = r#"
name: demo
description: demo
base_url: https://example.com
commands:
  search:
    description: search
    url: /x
    format: json
    selector: hits
    fields:
      title:
        json_path: title
        transform: strip_html
      url:
        template: "https://example.com/posts/{id}/{slug}"
      rank:
        json_path: "@index"
        transform: add_one
"#;
        let adapter: Adapter = serde_yaml_ng::from_str(yaml).unwrap();
        let cmd = adapter.commands.get("search").unwrap();
        let body = r#"{"hits":[{"title":"<em>Hi</em>","id":"1","slug":"hi"}]}"#;
        let items = extract_json(body, cmd, &HashMap::new()).unwrap();
        assert_eq!(items[0]["title"], json!("Hi"));
        assert_eq!(items[0]["url"], json!("https://example.com/posts/1/hi"));
        assert_eq!(items[0]["rank"], json!(1));
    }

    #[test]
    fn substitute_eval_replaces_mustache() {
        let mut params = HashMap::new();
        params.insert("text".into(), "hello \"world\"".into());
        let js = "const text = `${{text}}`;";
        assert_eq!(substitute_eval(js, &params), r#"const text = `hello \"world\"`;"#);
    }

    #[test]
    fn graphql_body_only_replaces_params() {
        let mut params = HashMap::new();
        params.insert("limit".into(), "5".into());
        let body = json!({
            "query": "{ posts(input: {terms: {view: \"top\", limit: {limit}}}) { title } }"
        });
        let out = substitute_json(&body, &params);
        let q = out["query"].as_str().unwrap();
        assert!(q.contains("limit: 5"));
        assert!(q.contains("{ posts"));
        assert!(q.contains("{ title }"));
    }

    #[test]
    fn extract_static_filters_by_param() {
        let yaml = r#"
name: demo
description: demo
base_url: https://example.com
commands:
  models:
    description: models
    format: static
    columns: ["type", "model"]
    data:
      - { type: image, model: flux }
      - { type: video, model: kling }
    params:
      type:
        type: string
        default: all
"#;
        let adapter: Adapter = serde_yaml_ng::from_str(yaml).unwrap();
        let cmd = adapter.commands.get("models").unwrap();
        let mut params = HashMap::new();
        params.insert("type".into(), "image".into());
        let items = extract_static(cmd, &params).unwrap();
        assert_eq!(items.len(), 1);
        assert_eq!(items[0]["model"], json!("flux"));
    }

    #[test]
    fn retryable_errors() {
        let e = anyhow::anyhow!("HTTP 503 from https://example.com");
        assert!(is_retryable(&e));
        let e = anyhow::anyhow!("HTTP 404 from https://example.com");
        assert!(!is_retryable(&e));
    }

    #[test]
    fn suggest_close_names() {
        let hint = suggest("hackernew", ["hackernews", "wikipedia"].into_iter());
        assert!(hint.contains("hackernews"));
    }

    #[test]
    fn json_path_filter_by_field() {
        let root = json!({
            "results": [
                {"kind": "podcast", "trackName": "Show"},
                {"kind": "podcast-episode", "trackName": "Ep 1"},
                {"kind": "podcast-episode", "trackName": "Ep 2"}
            ]
        });
        let selected = resolve_json(&root, "results[?kind=='podcast-episode']").unwrap();
        assert_eq!(selected.as_array().unwrap().len(), 2);
        assert_eq!(selected[0]["trackName"], json!("Ep 1"));
    }

    #[test]
    fn join_transform_flattens_arrays() {
        let yaml = r#"
name: demo
description: demo
base_url: https://example.com
commands:
  search:
    description: search
    url: /x
    format: json
    fields:
      tags:
        json_path: tags
        transform: join
"#;
        let adapter: Adapter = serde_yaml_ng::from_str(yaml).unwrap();
        let cmd = adapter.commands.get("search").unwrap();
        let body = r#"{"tags":["rust","cli"]}"#;
        let items = extract_json(body, cmd, &HashMap::new()).unwrap();
        assert_eq!(items[0]["tags"], json!("rust, cli"));
    }

    #[test]
    fn command_alias_resolves() {
        let yaml = r#"
name: demo
description: demo
base_url: https://example.com
commands:
  rate:
    description: rates
    aliases: ["rates"]
    url: /x
    format: json
    fields: {}
"#;
        let adapter: Adapter = serde_yaml_ng::from_str(yaml).unwrap();
        let (name, _) = adapter.command("rates").unwrap();
        assert_eq!(name, "rate");
    }

    #[test]
    fn json_path_plucks_array_of_objects() {
        let root = json!({
            "author": [
                {"family": "Mineault", "given": "Patrick"},
                {"family": "Ng", "given": "Andrew"}
            ]
        });
        assert_eq!(
            resolve_json(&root, "author[].family").unwrap(),
            json!(["Mineault", "Ng"])
        );
        assert_eq!(
            resolve_json(&root, "author.family").unwrap(),
            json!(["Mineault", "Ng"])
        );
    }

    #[test]
    fn first_transform_skips_empty() {
        let yaml = r#"
name: demo
description: demo
base_url: https://example.com
commands:
  search:
    description: search
    url: /x
    format: json
    selector: "$"
    fields:
      phonetic:
        json_path: phonetics[].text
        transform: first
"#;
        let adapter: Adapter = serde_yaml_ng::from_str(yaml).unwrap();
        let cmd = adapter.commands.get("search").unwrap();
        let body = r#"{"phonetics":[{"audio":"a.mp3"},{"text":"/həˈloʊ/"}]}"#;
        let items = extract_json(body, cmd, &HashMap::new()).unwrap();
        assert_eq!(items[0]["phonetic"], json!("/həˈloʊ/"));
    }

    #[test]
    fn unique_by_keeps_first() {
        let mut result = PipelineResult {
            adapter: "demo".into(),
            command: "x".into(),
            items: vec![
                json!({"title": "a", "n": 1}),
                json!({"title": "b", "n": 2}),
                json!({"title": "a", "n": 3}),
            ],
            count: 3,
        };
        result.unique_by("title");
        assert_eq!(result.items.len(), 2);
        assert_eq!(result.items[1]["title"], json!("b"));
    }
}
