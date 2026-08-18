//! YAML adapter schema — declarative web data extraction definitions.

use std::collections::HashMap;

use indexmap::IndexMap;
use serde::Deserialize;
use serde_json::Value;

/// A declarative adapter that defines how to extract structured data from a website.
#[derive(Debug, Clone, Deserialize)]
pub struct Adapter {
    /// Adapter name (e.g., "hackernews").
    pub name: String,
    /// Human-readable description.
    pub description: String,
    /// Base URL for all commands (e.g., "https://news.ycombinator.com").
    pub base_url: String,
    /// Adapter version.
    #[serde(default)]
    pub version: String,
    /// Alternate names that resolve to this adapter (e.g., `hf` → huggingface).
    #[serde(default)]
    pub aliases: Vec<String>,
    /// Search tags used by the hub index and `anycli list`.
    #[serde(default)]
    pub tags: Vec<String>,
    /// Available commands (e.g., "top", "search", "item").
    pub commands: HashMap<String, Command>,
}

/// A single command within an adapter.
#[derive(Debug, Clone, Deserialize)]
pub struct Command {
    /// Human-readable description.
    pub description: String,
    /// URL path, may contain `{param}` placeholders. Relative to `base_url`.
    /// Optional for `static` / `desktop` commands that do not fetch a URL.
    #[serde(default)]
    pub url: String,
    /// Source format of the response.
    #[serde(default)]
    pub format: SourceFormat,
    /// Regex pattern that splits the page into repeated items.
    /// Each match becomes one row in the output.
    /// Uses `(?s)` dotall mode internally.
    #[serde(default)]
    pub selector: Option<String>,
    /// Fields to extract from each matched item.
    /// Keys are column names, values define extraction rules.
    #[serde(default)]
    pub fields: IndexMap<String, FieldDef>,
    /// Inline dataset for `format: static` commands.
    #[serde(default)]
    pub data: Option<Value>,
    /// Column order for `format: static` when `fields` is omitted.
    #[serde(default)]
    pub columns: Vec<String>,
    /// Parameter definitions for this command (order preserved for positional args).
    #[serde(default)]
    pub params: IndexMap<String, ParamDef>,
    /// Extra HTTP headers to send with the request.
    #[serde(default)]
    pub headers: HashMap<String, String>,
    /// HTTP method (GET, POST, PUT, DELETE). Defaults to GET.
    /// When `body` is set and method is omitted, POST is used.
    #[serde(default)]
    pub method: Option<String>,
    /// JSON request body. `{param}` placeholders are substituted from CLI params.
    /// Brace-heavy strings (e.g. GraphQL) only replace known param names.
    #[serde(default)]
    pub body: Option<Value>,
    /// Override Content-Type for the request body.
    #[serde(default)]
    pub content_type: Option<String>,
    /// Per-command request timeout in seconds.
    #[serde(default)]
    pub timeout: Option<u64>,
    /// JavaScript to evaluate in browser context (for `browser_api`/`desktop` format).
    /// The JS should return a JSON string (use JSON.stringify).
    /// `{param}` / `${{param}}` placeholders are substituted from CLI params.
    #[serde(default)]
    pub evaluate: Option<String>,

    /// CDP port or app name to connect to (for `desktop` format).
    /// Can be a port number (e.g. "9222") or app name for auto-discovery.
    #[serde(default)]
    pub cdp_target: Option<String>,

    /// URL pattern to intercept network responses (for `intercept` format).
    /// Captures the first matching response body as the data source.
    #[serde(default)]
    pub intercept_pattern: Option<String>,

    /// Fetch each item individually by ID.
    /// The first response returns an array of IDs; each ID is fetched
    /// via `fetch_each.url` (with `{id}` placeholder) to build the final items.
    #[serde(default)]
    pub fetch_each: Option<FetchEach>,
}

/// Fetch-each definition: the initial response is an ID list, and each
/// item is fetched individually from a detail URL.
#[derive(Debug, Clone, Deserialize)]
pub struct FetchEach {
    /// URL template with `{id}` placeholder (e.g., "/item/{id}.json").
    pub url: String,
    /// Source format of the detail response.
    #[serde(default)]
    pub format: SourceFormat,
    /// Fields to extract from each detail response.
    pub fields: IndexMap<String, FieldDef>,
}

/// Source format of the HTTP response.
#[derive(Debug, Clone, Default, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum SourceFormat {
    #[default]
    Html,
    Json,
    Xml,
    /// Browser render mode: navigate + get rendered HTML.
    Browser,
    /// Browser API mode: navigate to URL then eval JS to get JSON data.
    /// Uses Chrome profile cookies for authenticated API calls.
    BrowserApi,
    /// Desktop mode: connect to running Electron/native app via CDP, eval JS.
    /// For controlling Cursor, ChatGPT Desktop, Discord, etc.
    Desktop,
    /// Intercept mode: open page in browser, capture network response matching a pattern.
    Intercept,
    /// Static mode: return inline `data` from the adapter YAML (no network).
    Static,
}

/// Defines how to extract a single field from a matched item block.
#[derive(Debug, Clone, Deserialize)]
pub struct FieldDef {
    /// Regex with one capture group for HTML extraction.
    #[serde(default)]
    pub pattern: Option<String>,
    /// Dot-separated path for JSON extraction (e.g., "data.title").
    /// Supports `[]` no-ops, `[n]` indices, and `field[n].nested` (e.g. `weatherDesc[0].value`).
    /// `@index` is the current row index. Selector `$` means the whole document is one item.
    #[serde(default)]
    pub json_path: Option<String>,
    /// Fallback JSON paths tried in order when `json_path` is missing.
    #[serde(default)]
    pub alt_paths: Vec<String>,
    /// Build this field from a template using `{field}`, `{json_key}`, or `{param}`.
    #[serde(default)]
    pub template: Option<String>,
    /// Default value if extraction fails.
    #[serde(default)]
    pub default: Option<String>,
    /// Post-processing transform: "strip_html", "trim", "decode_entities", "to_number", "add_one".
    #[serde(default)]
    pub transform: Option<Transform>,
}

/// Post-processing transform for extracted field values.
#[derive(Debug, Clone, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum Transform {
    StripHtml,
    Trim,
    DecodeEntities,
    ToNumber,
    AddOne,
}

/// CLI parameter definition.
#[derive(Debug, Clone, Deserialize)]
pub struct ParamDef {
    /// Parameter type hint.
    #[serde(rename = "type", default = "default_string")]
    pub param_type: String,
    /// Whether this parameter is required.
    #[serde(default)]
    pub required: bool,
    /// Default value.
    #[serde(default)]
    pub default: Option<serde_json::Value>,
    /// Human-readable description.
    #[serde(default)]
    pub description: Option<String>,
    /// Bind leftover positional CLI args to this parameter (in definition order).
    #[serde(default)]
    pub positional: bool,
    /// Allowed values; rejected if the provided value is not in the list.
    #[serde(default)]
    pub choices: Vec<String>,
}

fn default_string() -> String {
    "string".to_owned()
}
