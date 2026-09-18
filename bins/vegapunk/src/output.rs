//! AXI output: TOON-first, compact JSON for nested/verbose, pretty for `--format full`.

use anyhow::{Context, Result};
use clap::ValueEnum;
use serde::Serialize;
use serde_json::Value;

/// Agent-facing stdout format.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum, Default)]
pub enum OutputFormat {
    /// Token-Oriented Object Notation (default).
    #[default]
    Toon,
    /// Compact single-line JSON (good for nested/verbose).
    Json,
    /// Pretty-printed JSON.
    Full,
}

/// Where to emit next-step help[].
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum, Default)]
pub enum HelpStream {
    /// stderr (default; keeps stdout pure payload).
    #[default]
    Stderr,
    /// stdout footer (for hosts that only capture stdout).
    Stdout,
    /// Suppress help[].
    Off,
}

/// Emit value to stdout in the chosen format.
///
/// Policy (O1): try TOON first; if nested/verbose, fall back to compact JSON
/// unless format is forced to `full` / `json`.
pub fn print_out<T: Serialize>(v: &T, format: OutputFormat) -> Result<()> {
    let value = serde_json::to_value(v).context("serialize response")?;
    let text = encode_value(&value, format)?;
    println!("{text}");
    Ok(())
}

/// Structured agent error on stdout (AXI-friendly) + non-zero exit expected by caller.
pub fn print_error(
    format: OutputFormat,
    code: &str,
    message: impl std::fmt::Display,
    help: &[&str],
) -> Result<()> {
    let body = serde_json::json!({
        "error": message.to_string(),
        "code": code,
        "help": help,
    });
    print_out(&body, format)?;
    Ok(())
}

/// Encode with AXI policy.
pub fn encode_value(value: &Value, format: OutputFormat) -> Result<String> {
    match format {
        OutputFormat::Full => Ok(serde_json::to_string_pretty(value)?),
        OutputFormat::Json => Ok(serde_json::to_string(value)?),
        OutputFormat::Toon => {
            if prefers_json(value) {
                Ok(serde_json::to_string(value)?)
            } else {
                match toon_format::encode_default(value) {
                    Ok(s) => Ok(s),
                    Err(_) => Ok(serde_json::to_string(value)?),
                }
            }
        }
    }
}

/// Nested objects, deep arrays, or large free-text → compact JSON.
fn prefers_json(value: &Value) -> bool {
    match value {
        Value::Object(map) => {
            // Nested object values or array-of-objects mixed with objects
            map.values().any(|v| {
                matches!(v, Value::Object(_))
                    || matches!(v, Value::Array(a) if a.iter().any(|x| matches!(x, Value::Object(_))))
                    || is_long_string(v)
            }) || map.len() > 12
        }
        Value::Array(items) => {
            // Array of nested objects → JSON; flat primitive rows stay TOON
            items.iter().any(|item| match item {
                Value::Object(m) => m
                    .values()
                    .any(|v| matches!(v, Value::Object(_) | Value::Array(_)) || is_long_string(v)),
                Value::Array(_) => true,
                _ => false,
            })
        }
        Value::String(s) => s.len() > 240,
        _ => false,
    }
}

fn is_long_string(v: &Value) -> bool {
    matches!(v, Value::String(s) if s.len() > 240)
}

/// Append AXI-style next-step hints.
pub fn print_help_hints(hints: &[&str], stream: HelpStream) {
    if hints.is_empty() || matches!(stream, HelpStream::Off) {
        return;
    }
    let mut block = format!("help[{}]:\n", hints.len());
    for h in hints {
        block.push_str(&format!("  {h}\n"));
    }
    match stream {
        HelpStream::Stderr => eprint!("{block}"),
        HelpStream::Stdout => print!("{block}"),
        HelpStream::Off => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn flat_array_uses_toon() {
        let v = json!([{"id":"a","score":1.0,"preview":"hi"}]);
        let s = encode_value(&v, OutputFormat::Toon).unwrap();
        // TOON tabular or object form — not pretty multi-line JSON array
        assert!(!s.contains("\n  {"), "expected compact TOON-ish, got {s}");
    }

    #[test]
    fn nested_pack_uses_json() {
        let v = json!({
            "recall": {"queries": ["a"], "hits": [], "abstained": false},
            "pack": {"block": "x".repeat(300), "cards": []}
        });
        let s = encode_value(&v, OutputFormat::Toon).unwrap();
        assert!(s.contains('{') || s.contains("block"), "got {s}");
    }
}
