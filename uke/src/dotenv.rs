//! Minimal `.env` reader for local secrets such as `TYPESAFE_API_KEY`. Values are never logged.
use std::path::Path;

/// `KEY=value` lines; tolerates `export `, spaces around `=`, and matching quotes.
pub fn parse(text: &str) -> Vec<(String, String)> {
    text.lines()
        .filter_map(|line| {
            let line = line.trim();
            let line = line.strip_prefix("export ").unwrap_or(line);
            let (key, value) = line.split_once('=')?;
            let (key, mut value) = (key.trim(), value.trim());
            for quote in ['"', '\''] {
                if let Some(inner) = value
                    .strip_prefix(quote)
                    .and_then(|v| v.strip_suffix(quote))
                {
                    value = inner;
                }
            }
            let valid = !key.is_empty()
                && !key.starts_with('#')
                && key.chars().all(|c| c.is_ascii_alphanumeric() || c == '_');
            valid.then(|| (key.to_string(), value.to_string()))
        })
        .collect()
}

/// First `.env` found walking up from `start`. The process environment always wins.
pub fn lookup(start: &Path, key: &str) -> Option<String> {
    if let Ok(value) = std::env::var(key) {
        return Some(value);
    }
    start.ancestors().find_map(|dir| {
        let text = std::fs::read_to_string(dir.join(".env")).ok()?;
        parse(&text)
            .into_iter()
            .find_map(|(k, v)| (k == key).then_some(v))
    })
}

#[cfg(test)]
mod tests {
    #[test]
    fn parses_loose_env_lines() {
        let parsed = super::parse("# c\nA= one \nexport B=\"two words\"\nbad key=x\nC='3'\n");
        assert_eq!(
            parsed,
            [
                ("A".into(), "one".into()),
                ("B".into(), "two words".into()),
                ("C".into(), "3".into())
            ]
        );
    }
}
