//! Output formatting — JSON, table, CSV, and Markdown.

use anyhow::{Result, bail};
use serde::Deserialize;

use crate::pipeline::PipelineResult;

/// Supported output formats.
#[derive(Debug, Clone, Copy, Default, Deserialize, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum OutputFormat {
    #[default]
    Table,
    Json,
    Csv,
    Markdown,
    Yaml,
}

impl std::str::FromStr for OutputFormat {
    type Err = anyhow::Error;
    fn from_str(s: &str) -> Result<Self> {
        match s.to_lowercase().as_str() {
            "table" => Ok(Self::Table),
            "json" => Ok(Self::Json),
            "csv" => Ok(Self::Csv),
            "markdown" | "md" => Ok(Self::Markdown),
            "yaml" | "yml" => Ok(Self::Yaml),
            _ => bail!("unknown format `{s}`. supported: table, json, md, yaml, csv"),
        }
    }
}

/// Format a pipeline result into the specified output format.
pub fn format_result(result: &PipelineResult, fmt: OutputFormat) -> Result<String> {
    match fmt {
        OutputFormat::Table => format_table(result),
        OutputFormat::Json => format_json(result),
        OutputFormat::Csv => format_csv(result),
        OutputFormat::Markdown => format_markdown(result),
        OutputFormat::Yaml => format_yaml(result),
    }
}

fn format_json(result: &PipelineResult) -> Result<String> {
    Ok(serde_json::to_string_pretty(&result.items)?)
}

fn format_table(result: &PipelineResult) -> Result<String> {
    if result.items.is_empty() {
        return Ok("(no results)".to_owned());
    }

    let keys = collect_keys(result);
    let mut widths: Vec<usize> = keys.iter().map(|k| k.len()).collect();

    // Collect cell values and compute column widths.
    let mut rows: Vec<Vec<String>> = Vec::with_capacity(result.items.len());
    for item in &result.items {
        let mut row = Vec::with_capacity(keys.len());
        for (i, key) in keys.iter().enumerate() {
            let val = cell_value(item, key);
            if val.len() > widths[i] {
                widths[i] = val.len().min(60);
            }
            row.push(val);
        }
        rows.push(row);
    }

    let mut out = String::new();

    // Header.
    for (i, key) in keys.iter().enumerate() {
        if i > 0 { out.push_str(" | "); }
        out.push_str(&pad(key, widths[i]));
    }
    out.push('\n');

    // Separator.
    for (i, w) in widths.iter().enumerate() {
        if i > 0 { out.push_str("-+-"); }
        out.push_str(&"-".repeat(*w));
    }
    out.push('\n');

    // Rows.
    for row in &rows {
        for (i, val) in row.iter().enumerate() {
            if i > 0 { out.push_str(" | "); }
            out.push_str(&pad(&truncate(val, widths[i]), widths[i]));
        }
        out.push('\n');
    }

    Ok(out)
}

fn format_csv(result: &PipelineResult) -> Result<String> {
    if result.items.is_empty() {
        return Ok(String::new());
    }

    let keys = collect_keys(result);
    let mut out = keys.join(",");
    out.push('\n');

    for item in &result.items {
        let row: Vec<String> = keys.iter().map(|k| csv_escape(&cell_value(item, k))).collect();
        out.push_str(&row.join(","));
        out.push('\n');
    }

    Ok(out)
}

fn format_markdown(result: &PipelineResult) -> Result<String> {
    if result.items.is_empty() {
        return Ok("*No results*".to_owned());
    }

    let keys = collect_keys(result);
    let mut out = String::new();

    // Header.
    out.push('|');
    for key in &keys {
        out.push_str(&format!(" {} |", key));
    }
    out.push('\n');

    // Separator.
    out.push('|');
    for _ in &keys {
        out.push_str(" --- |");
    }
    out.push('\n');

    // Rows.
    for item in &result.items {
        out.push('|');
        for key in &keys {
            let val = cell_value(item, key).replace('|', "\\|");
            out.push_str(&format!(" {} |", val));
        }
        out.push('\n');
    }

    Ok(out)
}

fn format_yaml(result: &PipelineResult) -> Result<String> {
    Ok(serde_yaml_ng::to_string(&result.items)?)
}

/// Collect ordered field names from the first item.
fn collect_keys(result: &PipelineResult) -> Vec<String> {
    result
        .items
        .first()
        .and_then(|item| item.as_object())
        .map(|obj| obj.keys().cloned().collect())
        .unwrap_or_default()
}

/// Get a cell value as a display string.
fn cell_value(item: &serde_json::Value, key: &str) -> String {
    match item.get(key) {
        Some(serde_json::Value::String(s)) => s.clone(),
        Some(serde_json::Value::Number(n)) => n.to_string(),
        Some(serde_json::Value::Bool(b)) => b.to_string(),
        Some(serde_json::Value::Null) | None => String::new(),
        Some(other) => other.to_string(),
    }
}

fn pad(s: &str, width: usize) -> String {
    let char_len = s.chars().count();
    if char_len >= width {
        s.chars().take(width).collect()
    } else {
        format!("{s}{}", " ".repeat(width - char_len))
    }
}

fn truncate(s: &str, max: usize) -> String {
    let chars: Vec<char> = s.chars().collect();
    if chars.len() <= max {
        s.to_owned()
    } else if max > 3 {
        let truncated: String = chars[..max - 3].iter().collect();
        format!("{truncated}...")
    } else {
        chars[..max].iter().collect()
    }
}

fn csv_escape(s: &str) -> String {
    if s.contains(',') || s.contains('"') || s.contains('\n') {
        format!("\"{}\"", s.replace('"', "\"\""))
    } else {
        s.to_owned()
    }
}
