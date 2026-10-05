//! `unvrs ctl handoff` summary files: JSON, or flat TOON with inline primitive arrays.
use crate::HandoffSummary;
use anyhow::{Context, Result, ensure};
use serde_json::Value;

pub fn handoff_summary(text: &str) -> Result<HandoffSummary> {
    if text.trim_start().starts_with('{') {
        return Ok(serde_json::from_str(text)?);
    }
    let mut value = serde_json::Map::new();
    for (line_number, line) in text.lines().enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        ensure!(
            !line.starts_with(char::is_whitespace),
            "TOON line {}: nested values are unsupported",
            line_number + 1
        );
        let (header, raw) = line
            .split_once(':')
            .with_context(|| format!("TOON line {}: expected key: value", line_number + 1))?;
        let (key, parsed) =
            if let Some((key, count)) = header.strip_suffix(']').and_then(|h| h.rsplit_once('[')) {
                let count = count.parse::<usize>().with_context(|| {
                    format!("TOON line {}: invalid array count", line_number + 1)
                })?;
                let items = toon_values(raw.trim())?;
                ensure!(
                    items.len() == count,
                    "TOON line {}: declared {count} values, found {}",
                    line_number + 1,
                    items.len()
                );
                (
                    key,
                    Value::Array(items.into_iter().map(Value::String).collect()),
                )
            } else {
                let raw = raw.trim();
                let parsed = if matches!(
                    header,
                    "from_pid" | "from_rank" | "to_rank" | "to_pid" | "created_at"
                ) {
                    if raw == "null" {
                        Value::Null
                    } else {
                        Value::Number(
                            raw.parse::<u64>()
                                .with_context(|| {
                                    format!(
                                        "TOON line {}: expected an unsigned integer",
                                        line_number + 1
                                    )
                                })?
                                .into(),
                        )
                    }
                } else if raw == "null" {
                    Value::Null
                } else {
                    Value::String(toon_string(raw)?)
                };
                (header, parsed)
            };
        ensure!(
            value.insert(key.into(), parsed).is_none(),
            "TOON line {}: duplicate field {key}",
            line_number + 1
        );
    }
    Ok(serde_json::from_value(Value::Object(value))?)
}

fn toon_values(raw: &str) -> Result<Vec<String>> {
    if raw.is_empty() {
        return Ok(vec![]);
    }
    let mut values = Vec::new();
    let mut quoted = false;
    let mut escaped = false;
    let mut start = 0;
    for (index, character) in raw.char_indices() {
        match character {
            '\\' if quoted => escaped = !escaped,
            '"' if !escaped => quoted = !quoted,
            ',' if !quoted => {
                values.push(toon_string(raw[start..index].trim())?);
                start = index + 1;
            }
            _ => escaped = false,
        }
    }
    ensure!(!quoted, "TOON value has an unterminated quote");
    values.push(toon_string(raw[start..].trim())?);
    Ok(values)
}

fn toon_string(raw: &str) -> Result<String> {
    if raw.starts_with('"') {
        Ok(serde_json::from_str(raw).context("Invalid quoted TOON string")?)
    } else {
        ensure!(!raw.is_empty(), "TOON string value is empty");
        Ok(raw.into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn handoff_accepts_toon_and_json() {
        let toon = "work_id: work-1\nto_rank: 2\nto_harness: cursor\nnext_focus: continue\ngoal: finish\ndone[2]: first,\"second, with comma\"\nopen[0]:\nartifacts[1]: src/main.rs";
        let summary = handoff_summary(toon).unwrap();
        assert_eq!(summary.to_harness.as_deref(), Some("cursor"));
        assert_eq!(summary.done, ["first", "second, with comma"]);
        assert!(summary.validate().is_ok());
        let json = serde_json::to_string(&summary).unwrap();
        assert_eq!(handoff_summary(&json).unwrap().work_id, "work-1");
        assert!(handoff_summary("work_id: x\ndone[2]: one").is_err());
    }
}
