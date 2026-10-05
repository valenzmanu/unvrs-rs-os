//! Flight log rendering: level filter, search, and colour.
use crate::theme::*;
use ratatui::{
    style::{Color, Style},
    text::{Line, Span},
};
use std::collections::VecDeque;

/// True when `line` passes the level `filter` (None, "ERROR" or "WARN") and contains `query`.
///
/// `query` is case-insensitive and whitespace separated: every term must match (AND).
/// A `pid:2` term matches the `PID 2` token rather than a bare `2` anywhere in the line.
pub fn matches(line: &str, filter: Option<&str>, query: &str) -> bool {
    let upper = line.to_ascii_uppercase();
    let level = match filter {
        Some("ERROR") => ["ERROR", "FAIL", "OFFLINE", "BLOCKED"]
            .iter()
            .any(|word| upper.contains(word)),
        Some("WARN") => upper.contains("ATTENTION") || upper.contains("CANCEL"),
        _ => true,
    };
    level
        && query
            .split_whitespace()
            .all(|term| upper.contains(&needle(term)))
}

/// The uppercase text a search term looks for; `pid:2` widens to the `PID 2` token.
fn needle(term: &str) -> String {
    let term = term.to_ascii_uppercase();
    match term.strip_prefix("PID:") {
        Some(pid) if !pid.is_empty() => format!("PID {pid}"),
        _ => term,
    }
}

/// `12 of 200 events · filter ERROR · search "boot"` — the flight log panel title.
#[allow(dead_code)] // seam: the lead swaps the FLIGHT LOGS title over to this
pub fn summary(tail: &VecDeque<String>, filter: Option<&str>, query: &str) -> String {
    let shown = tail
        .iter()
        .filter(|line| matches(line, filter, query))
        .count();
    let mut text = format!("{shown} of {} events", tail.len());
    if let Some(filter) = filter {
        text.push_str(&format!(" · filter {filter}"));
    }
    if !query.trim().is_empty() {
        text.push_str(&format!(" · search \"{}\"", query.trim()));
    }
    text
}

/// Tail lines look like `0012  PID 2 / done / text` (seconds since launch, two spaces, event).
pub fn lines(tail: &VecDeque<String>, filter: Option<&str>, query: &str) -> Vec<Line<'static>> {
    let out: Vec<Line<'static>> = tail
        .iter()
        .filter(|line| matches(line, filter, query))
        .map(|line| render(line, query))
        .collect();
    if out.is_empty() {
        let what = if !query.trim().is_empty() {
            format!("\"{}\"", query.trim())
        } else if let Some(filter) = filter {
            format!("filter {filter}")
        } else {
            "this view".into()
        };
        return vec![Line::styled(
            format!("No events match {what} · Backspace edits · Esc leaves logs"),
            style(DIM),
        )];
    }
    out
}

/// Split the `0012  ` launch-clock prefix off a tail line.
fn split_clock(line: &str) -> (Option<u64>, &str) {
    match line.split_once("  ") {
        Some((head, rest))
            if !head.is_empty() && head.bytes().all(|b| b.is_ascii_digit()) && head.len() <= 10 =>
        {
            (head.parse().ok(), rest)
        }
        _ => (None, line),
    }
}

/// Fixed-width badge and colour for an event, read off the leading words.
fn badge(event: &str) -> (&'static str, Color) {
    let upper = event.to_ascii_uppercase();
    let has = |word: &str| upper.contains(word);
    if has("ERROR") || has("FAIL") || has("OFFLINE") || has("BLOCKED") || has("TIMEOUT") {
        ("ERR ", PINK)
    } else if has("ATTENTION") || has("CANCEL") || has("NEEDS_CAPTAIN") {
        ("WARN", GOLD)
    } else if has("SUCCESS") || has("HANDOFF_OK") || has("PASS") || has("ONLINE") {
        (" OK ", GREEN)
    } else if upper.starts_with("HANDOFF") {
        ("HAND", CYAN)
    } else if upper.starts_with("MAIL") {
        ("MAIL", GOLD)
    } else if upper.starts_with("BOOT") {
        ("BOOT", GREEN)
    } else if has("/ TOOL") || has("/ CTL") {
        ("TOOL", DIM)
    } else {
        ("INFO", DIM)
    }
}

/// Seats keep the same tint everywhere they appear.
fn pid_color(pid: usize) -> Color {
    [CYAN, GOLD, GREEN, PINK][pid % 4]
}

/// One tail line as `T+00:12  BADGE  message`, with PID tints and search highlights.
fn render(line: &str, query: &str) -> Line<'static> {
    let (secs, event) = split_clock(line);
    let mut spans = Vec::new();
    match secs {
        Some(secs) => spans.push(Span::styled(
            format!("T+{:02}:{:02} ", secs / 60, secs % 60),
            style(DIM),
        )),
        None => spans.push(Span::styled("        ", style(DIM))),
    }
    let (badge_text, badge_color) = badge(event);
    spans.push(Span::styled(badge_text, style(badge_color)));
    spans.push(Span::raw("  "));
    spans.extend(message(event, query));
    Line::from(spans)
}

/// Style every char of the event text, then coalesce runs into spans.
fn message(event: &str, query: &str) -> Vec<Span<'static>> {
    let chars: Vec<char> = event.chars().collect();
    let plain = style(WHITE);
    let mut styles = vec![plain; chars.len()];
    tint_pids(&chars, &mut styles);
    let highlight = Style::default().fg(BG).bg(GOLD);
    for term in query.split_whitespace() {
        let term: Vec<char> = needle(term).chars().collect();
        for at in find_all(&chars, &term) {
            for s in &mut styles[at..at + term.len()] {
                *s = highlight;
            }
        }
    }
    let mut spans: Vec<Span<'static>> = Vec::new();
    let mut run = String::new();
    let mut current = plain;
    for (ch, s) in chars.iter().zip(styles) {
        if s != current && !run.is_empty() {
            spans.push(Span::styled(std::mem::take(&mut run), current));
        }
        current = s;
        run.push(*ch);
    }
    if !run.is_empty() {
        spans.push(Span::styled(run, current));
    }
    spans
}

/// Give each `PID <n>` token its seat colour.
fn tint_pids(chars: &[char], styles: &mut [Style]) {
    let pid: Vec<char> = "PID ".chars().collect();
    for at in find_all(chars, &pid) {
        let mut end = at + pid.len();
        let mut value = 0usize;
        while end < chars.len() && chars[end].is_ascii_digit() {
            value = value * 10 + chars[end] as usize - '0' as usize;
            end += 1;
        }
        if end > at + pid.len() {
            let tint = style(pid_color(value));
            for s in &mut styles[at..end] {
                *s = tint;
            }
        }
    }
}

/// ASCII-case-insensitive char-boundary-safe substring search; every start index.
fn find_all(haystack: &[char], needle: &[char]) -> Vec<usize> {
    let mut hits = vec![];
    if needle.is_empty() || needle.len() > haystack.len() {
        return hits;
    }
    for at in 0..=haystack.len() - needle.len() {
        if haystack[at..at + needle.len()]
            .iter()
            .zip(needle)
            .all(|(a, b)| a.eq_ignore_ascii_case(b))
        {
            hits.push(at);
        }
    }
    hits
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tail(items: &[&str]) -> VecDeque<String> {
        items.iter().map(|s| s.to_string()).collect()
    }
    fn text(line: &Line<'_>) -> String {
        line.spans
            .iter()
            .map(|s| s.content.as_ref())
            .collect::<String>()
    }

    const SAMPLE: [&str; 6] = [
        "0000  BRIDGE ONLINE / Captain has the helm",
        "0003  BOOT / PID 1 / L1 / COPILOT",
        "0012  PID 2 / done / surveyed the ⭐ système",
        "0040  HANDOFF / pi → codex / work-1 / PID 1 → 2 / mailbox delivered / owner=2",
        "0061  MAIL / PID 2 / queued",
        "0125  PID 3 / turn_error / transport offline",
    ];

    #[test]
    fn clock_prefix_becomes_a_relative_stamp() {
        let rendered = lines(&tail(&SAMPLE), None, "");
        assert!(text(&rendered[0]).starts_with("T+00:00 "));
        assert!(text(&rendered[2]).starts_with("T+00:12 "));
        assert!(text(&rendered[5]).starts_with("T+02:05 "));
        assert_eq!(rendered[0].spans[0].style.fg, Some(DIM));
        // A line without the launch clock still renders.
        let bare = lines(&tail(&["no clock here"]), None, "");
        assert!(text(&bare[0]).contains("no clock here"));
    }

    #[test]
    fn badges_label_errors_handoffs_and_boots() {
        let rendered = lines(&tail(&SAMPLE), None, "");
        let badge_of = |i: usize| {
            let line = &rendered[i];
            (line.spans[1].content.to_string(), line.spans[1].style.fg)
        };
        assert_eq!(badge_of(0), (" OK ".into(), Some(GREEN)));
        assert_eq!(badge_of(1), ("BOOT".into(), Some(GREEN)));
        assert_eq!(badge_of(2), ("INFO".into(), Some(DIM)));
        assert_eq!(badge_of(3), ("HAND".into(), Some(CYAN)));
        assert_eq!(badge_of(4), ("MAIL".into(), Some(GOLD)));
        assert_eq!(badge_of(5), ("ERR ".into(), Some(PINK)));
        // Matrix and attention shapes land on the same badges.
        let more = lines(
            &tail(&[
                "0007  HANDOFF_MATRIX / pi → codex / FAIL / 150s timeout",
                "0008  HANDOFF_MATRIX / pi → codex / requesting",
                "0009  PID 2 / needs_captain / approve the plan",
                "0010  PID 2 / tool / read src/ui.rs",
            ]),
            None,
            "",
        );
        assert_eq!(more[0].spans[1].content, "ERR ");
        assert_eq!(more[1].spans[1].content, "HAND");
        assert_eq!(more[2].spans[1].content, "WARN");
        assert_eq!(more[3].spans[1].content, "TOOL");
    }

    #[test]
    fn pid_tokens_keep_one_colour_per_seat() {
        let rendered = lines(&tail(&SAMPLE), None, "");
        let tint = |i: usize, token: &str| {
            let line = &rendered[i];
            line.spans
                .iter()
                .find(|s| s.content.contains(token))
                .map(|s| s.style.fg)
                .unwrap()
        };
        assert_eq!(tint(1, "PID 1"), tint(3, "PID 1"));
        assert_eq!(tint(2, "PID 2"), tint(4, "PID 2"));
        assert_ne!(tint(1, "PID 1"), tint(2, "PID 2"));
    }

    #[test]
    fn filter_and_query_terms_are_anded() {
        let events = tail(&SAMPLE);
        let kept =
            |filter, query: &str| SAMPLE.iter().filter(|l| matches(l, filter, query)).count();
        assert_eq!(lines(&events, None, "").len(), 6);
        assert_eq!(lines(&events, Some("ERROR"), "").len(), 1);
        // Two terms, both must hit, order independent, case insensitive.
        assert_eq!(kept(None, "handoff MAILBOX"), 1);
        assert_eq!(kept(None, "handoff queued"), 0);
        // `pid:2` means the PID 2 token, not the digit inside `PID 1 → 2`.
        assert_eq!(kept(None, "pid:2"), 2);
        assert_eq!(kept(Some("ERROR"), "pid:3"), 1);
        assert_eq!(kept(Some("ERROR"), "pid:1"), 0);
    }

    #[test]
    fn highlights_cover_the_match_around_non_ascii_text() {
        let events = tail(&SAMPLE);
        // ASCII case folds, non-ASCII compares as itself; neither slices mid-char.
        let rendered = lines(&events, None, "SYSTèME");
        assert_eq!(rendered.len(), 1);
        let hit = rendered[0]
            .spans
            .iter()
            .find(|s| s.style.bg == Some(GOLD))
            .expect("a highlighted span");
        assert_eq!(hit.content, "système");
        // The rest of the line survives intact, star and all.
        let whole = text(&rendered[0]);
        assert!(whole.ends_with("surveyed the ⭐ système"), "{whole}");
        // Two terms, one of them a multi-byte char: both highlight, offsets stay aligned.
        let star = lines(&events, None, "⭐ Système");
        assert_eq!(star.len(), 1);
        assert_eq!(
            star[0]
                .spans
                .iter()
                .filter(|s| s.style.bg == Some(GOLD))
                .map(|s| s.content.as_ref())
                .collect::<String>(),
            "⭐système"
        );
    }

    #[test]
    fn no_match_explains_itself() {
        let events = tail(&SAMPLE);
        let empty = lines(&events, None, "warp core");
        assert_eq!(empty.len(), 1);
        assert_eq!(
            text(&empty[0]),
            "No events match \"warp core\" · Backspace edits · Esc leaves logs"
        );
        assert_eq!(empty[0].style.fg, Some(DIM));
        let filtered = lines(&events, Some("WARN"), "");
        assert!(text(&filtered[0]).contains("No events match filter WARN"));
    }

    #[test]
    fn summary_reads_like_a_panel_title() {
        let events = tail(&SAMPLE);
        assert_eq!(summary(&events, None, ""), "6 of 6 events");
        assert_eq!(
            summary(&events, Some("ERROR"), "pid:3"),
            "1 of 6 events · filter ERROR · search \"pid:3\""
        );
        assert_eq!(
            summary(&events, None, " boot "),
            "1 of 6 events · search \"boot\""
        );
    }
}
