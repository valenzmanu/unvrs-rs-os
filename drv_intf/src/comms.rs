//! COMMS transcript rendering.
//!
//! Two renderers share this module:
//!
//! * [`plain_lines`] (aliased as [`comms_lines`]) is the flat one: colour per line plus aligned
//!   pipe tables. LOGS and TELEMETRY use it, because those panels *are* dumps.
//! * [`conversation_lines`] is the COMMS one. It reads the transcript as a conversation:
//!   speakers get a gutter and a rule, tool runs and `unvrs ctl` dumps fold into a single dim
//!   chip, handoff and Boot cards fold into a one-line chip, and diagram fences become
//!   [`crate::diagram`] components. Errors and captain attention are never folded away.
//!   `expanded` (the Ctrl+O / `/expand` toggle) shows every folded line again.
//!
//! Both are called once per frame on a transcript that grows while an agent streams, so every
//! pass is a single linear walk over `content.lines()` with no regex and no backtracking.
use crate::theme::*;
use ratatui::{
    style::Color,
    text::{Line, Span},
};

const HINT: &str = "  (Ctrl+O expand)";
const CARD_FIELDS: [&str; 7] = [
    "FOCUS:",
    "GOAL:",
    "DONE:",
    "OPEN:",
    "ARTIFACTS:",
    "SUGGESTED SKILLS:",
    "CREATED:",
];
/// The Boot card fields, in the order the lead writes them. All but the first few are optional,
/// so the fold reads whichever ones arrived and never assumes a shape.
const BOOT_FIELDS: [&str; 10] = [
    "PICKER:",
    "NOUL:",
    "CONFIDENCE:",
    "CHANNEL:",
    "STRIPPED:",
    "NEST:",
    "INSTALL:",
    "PATHS:",
    "REFUSED:",
    "NOTE:",
];

fn line_style(line: &str, seat: &str) -> Color {
    if line == "CAPTAIN / MAILBOX" {
        GOLD
    } else if line == seat || line == "COPILOT" {
        CYAN
    } else if line.starts_with('[') {
        DIM
    } else if line.contains("FAILED") || line.starts_with("ERROR") || line.contains("FAIL") {
        PINK
    } else {
        WHITE
    }
}

fn table_cells(line: &str) -> Option<Vec<&str>> {
    let cells = line
        .trim()
        .strip_prefix('|')?
        .strip_suffix('|')?
        .split('|')
        .map(str::trim)
        .collect::<Vec<_>>();
    (cells.len() > 1).then_some(cells)
}

fn table_rule(cells: &[&str]) -> bool {
    cells
        .iter()
        .all(|cell| !cell.is_empty() && cell.chars().all(|ch| ch == '-' || ch == ':'))
}

fn aligned_table(rows: &[Vec<&str>], width: u16, seat: &str) -> Vec<Line<'static>> {
    let columns = rows
        .iter()
        .map(Vec::len)
        .max()
        .unwrap_or(0)
        .min((width as usize).div_ceil(4));
    if columns < 2 {
        return Vec::new();
    }
    let mut widths = (0..columns)
        .map(|column| {
            rows.iter()
                .filter_map(|row| row.get(column))
                .map(|cell| Line::from(*cell).width())
                .max()
                .unwrap_or(1)
                .max(1)
                .min(width as usize)
        })
        .collect::<Vec<_>>();
    let available = width as usize - columns.saturating_sub(1) * 3;
    while widths.iter().sum::<usize>() > available {
        let Some((index, _)) = widths.iter().enumerate().max_by_key(|(_, width)| *width) else {
            break;
        };
        if widths[index] <= 1 {
            break;
        }
        widths[index] -= 1;
    }
    rows.iter()
        .enumerate()
        .map(|(index, row)| {
            let text = (0..columns)
                .map(|column| {
                    let cell = one_line(
                        row.get(column).copied().unwrap_or(""),
                        widths[column] as u16,
                    );
                    format!(
                        "{cell}{}",
                        " ".repeat(
                            widths[column].saturating_sub(Line::from(cell.as_str()).width())
                        )
                    )
                })
                .collect::<Vec<_>>()
                .join(" │ ");
            Line::styled(
                text.clone(),
                style(if index == 0 {
                    DIM
                } else {
                    line_style(&text, seat)
                }),
            )
        })
        .collect()
}

/// Walks `content` emitting aligned tables and one coloured line per source line.
/// This is the flat renderer LOGS and TELEMETRY want; COMMS wants [`conversation_lines`].
pub fn plain_lines(content: &str, width: u16, seat: &str) -> Vec<Line<'static>> {
    let source = content.lines().collect::<Vec<_>>();
    let mut output = Vec::new();
    let mut index = 0;
    while index < source.len() {
        if table_cells(source[index]).is_none() {
            output.push(Line::styled(
                source[index].to_owned(),
                style(line_style(source[index], seat)),
            ));
            index += 1;
            continue;
        }
        index = push_table(&source, index, width, seat, &mut output);
    }
    output
}

/// Kept so existing call sites keep compiling; identical to [`plain_lines`].
pub fn comms_lines(content: &str, width: u16, seat: &str) -> Vec<Line<'static>> {
    plain_lines(content, width, seat)
}

/// Consumes the pipe-table run starting at `index` and returns the index just past it.
fn push_table(
    source: &[&str],
    index: usize,
    width: u16,
    seat: &str,
    output: &mut Vec<Line<'static>>,
) -> usize {
    let start = index;
    let mut index = index;
    while index < source.len() && table_cells(source[index]).is_some() {
        index += 1;
    }
    let rows = source[start..index]
        .iter()
        .filter_map(|line| table_cells(line))
        .filter(|cells| !table_rule(cells))
        .collect::<Vec<_>>();
    if rows.len() < 2 {
        output.extend(
            source[start..index]
                .iter()
                .map(|line| Line::styled((*line).to_owned(), style(line_style(line, seat)))),
        );
    } else {
        output.extend(aligned_table(&rows, width, seat));
    }
    index
}

/// The ``` fence tag, if `line` opens or closes a fenced block.
fn fence(line: &str) -> Option<&str> {
    line.trim_start().strip_prefix("```").map(str::trim)
}

fn is_diagram_tag(tag: &str) -> bool {
    matches!(
        tag.to_ascii_lowercase().as_str(),
        "flow" | "diagram" | "mermaid"
    )
}

fn is_tool(line: &str) -> bool {
    // Accept an unclosed `[ ` too: the last line of a streaming transcript is often half written,
    // and folding it straight away keeps the chip from flickering into prose and back.
    line.trim_start().starts_with("[ ")
}

fn tool_title(line: &str) -> String {
    line.trim()
        .trim_start_matches('[')
        .trim_end_matches(']')
        .trim()
        .to_owned()
}

/// True for the machine chatter agents paste back: `pid: 3`, `status: "delivered"`, `help[1]:`,
/// `unvrs ctl ...` and bare JSON braces. Keys must be machine-shaped, so `FOCUS:` and
/// `Transport failed:` stay prose.
fn is_ctl(line: &str) -> bool {
    let line = line.trim();
    if line.is_empty() {
        return false;
    }
    if line.starts_with("unvrs ") || matches!(line, "{" | "}" | "[" | "]" | "{}" | "[]") {
        return true;
    }
    let Some((key, _)) = line.split_once(':') else {
        return false;
    };
    let key = key.trim().trim_matches('"');
    !key.is_empty()
        && key.len() <= 24
        && key.chars().all(|ch| {
            ch.is_ascii_lowercase() || ch.is_ascii_digit() || matches!(ch, '_' | '-' | '[' | ']')
        })
}

/// PINK for failures, GOLD for things the captain must see. Never folded, in either mode.
fn attention(line: &str) -> Option<(Color, &'static str)> {
    let line = line.trim();
    if line.starts_with("ERROR")
        || line.starts_with("TURN FAILED")
        || line.starts_with("BOOT TIMEOUT")
        || line.starts_with("Transport failed")
        || line.starts_with("ADMIT REFUSED")
        || line.contains("FAILED")
    {
        Some((PINK, "▲ "))
    } else if line.starts_with("CAPTAIN INPUT:")
        || line.starts_with("MISSION ADMITTED")
        || line.starts_with("Ownership transferred")
        || line.contains("ATTENTION")
    {
        Some((GOLD, "◆ "))
    } else {
        None
    }
}

/// Terminal cells `text` occupies.
fn cells(text: &str) -> usize {
    Line::from(text.to_owned()).width()
}

/// `▌ CAPTAIN ────────` — the speaker gutter that makes a turn readable at a glance.
/// Built by hand rather than through `one_line`, which collapses the spacer away.
fn speaker_rule(name: &str, color: Color, width: u16) -> Line<'static> {
    let head = one_line(&format!("▌ {name}"), width);
    let rest = (width as usize).saturating_sub(cells(&head));
    Line::from(vec![
        Span::styled(head, style(color)),
        Span::styled(
            format!(
                "{}{}",
                " ".repeat(rest.min(1)),
                "─".repeat(rest.saturating_sub(1))
            ),
            style(DIM),
        ),
    ])
}

fn chip(text: &str, width: u16, color: Color) -> Line<'static> {
    Line::styled(one_line(text, width), style(color))
}

fn code_lines(source: &[&str], width: u16) -> Vec<Line<'static>> {
    source
        .iter()
        .map(|line| Line::styled(one_line(line, width), style(DIM)))
        .collect()
}

/// Folds a handoff card into `⇄ HANDOFF work-7 pi → codex · owner PID 2 · FOCUS: …`.
fn handoff_chip(header: &str, fields: &[&str], width: u16) -> Line<'static> {
    let parts = header.trim().split(" / ").collect::<Vec<_>>();
    let focus = fields
        .iter()
        .find_map(|line| line.trim().strip_prefix("FOCUS:"))
        .unwrap_or("")
        .trim();
    let owner = parts
        .iter()
        .find_map(|part| part.trim().strip_prefix("owner="))
        .map(|owner| format!(" · owner PID {owner}"))
        .unwrap_or_default();
    let route = parts
        .get(2)
        .map(|route| format!(" {route}"))
        .unwrap_or_default();
    chip(
        &format!(
            "⇄ HANDOFF {}{route}{owner}{}",
            parts.get(1).copied().unwrap_or(""),
            if focus.is_empty() {
                String::new()
            } else {
                format!(" · FOCUS: {focus}")
            }
        ),
        width,
        GOLD,
    )
}

fn handoff_expanded(header: &str, fields: &[&str], width: u16) -> Vec<Line<'static>> {
    let mut out = vec![chip(&format!("⇄ {}", header.trim()), width, GOLD)];
    out.extend(fields.iter().map(|line| {
        let (key, value) = line.split_once(':').unwrap_or((line, ""));
        let key = format!("  {}: ", key.trim());
        if cells(&key) >= width as usize {
            return Line::styled(one_line(line, width), style(DIM));
        }
        let room = (width as usize - cells(&key)) as u16;
        Line::from(vec![
            Span::styled(key, style(DIM)),
            Span::styled(one_line(value, room), style(WHITE)),
        ])
    }));
    out
}

/// The value of `key` in a card body, or `None` when the lead left the field out.
fn card_field<'a>(fields: &[&'a str], key: &str) -> Option<&'a str> {
    fields
        .iter()
        .find_map(|line| line.trim_start().strip_prefix(key))
        .map(str::trim)
}

/// `mattpocock/tdd, mattpocock/diagnosing-bugs` → `nest tdd, diagnosing-bugs`. The pool is the
/// captain's business, not the chip's; an empty nest says so in words.
fn boot_nest(value: &str) -> String {
    if value.starts_with("(empty") {
        return "nest empty (no skill needed)".to_owned();
    }
    let ids = value
        .split(',')
        .map(|id| id.trim().rsplit('/').next().unwrap_or("").to_owned())
        .collect::<Vec<_>>()
        .join(", ");
    format!("nest {ids}")
}

/// A refused boot reads PINK with a `▲` and carries its stage and reason; a ready one is GREEN.
fn boot_verdict<'a>(parts: &[&str], fields: &[&'a str]) -> (Color, &'static str, Option<&'a str>) {
    let reason = card_field(fields, "REFUSED:");
    if reason.is_some() || parts.last().is_some_and(|part| part.trim() == "REFUSED") {
        (PINK, "▲ ", Some(reason.unwrap_or("")))
    } else {
        (GREEN, "⬢ ", None)
    }
}

/// Folds a Boot card into `⬢ BOOT msn_coding_tdd · jev · noul 0.65 · … · READY`.
///
/// Built without [`one_line`] on purpose: COMMS draws into a wrapping Paragraph, and every part
/// of this summary — the picker fallback, the refusal reason — is something the captain acts on.
fn boot_chip(header: &str, fields: &[&str]) -> Line<'static> {
    let parts = header.trim().split(" / ").collect::<Vec<_>>();
    let (color, marker, refused) = boot_verdict(&parts, fields);
    let mut summary = vec![format!(
        "BOOT {}",
        parts.get(1).copied().unwrap_or("").trim()
    )];
    if let Some(picker) = card_field(fields, "PICKER:") {
        summary.push(picker.to_owned());
    }
    // `n/a` is the picker saying it never scored; a `noul n/a` part would only be noise.
    for (label, key) in [("noul", "NOUL:"), ("conf", "CONFIDENCE:")] {
        if let Some(value) = card_field(fields, key).filter(|value| *value != "n/a") {
            summary.push(format!("{label} {value}"));
        }
    }
    if let Some(nest) = card_field(fields, "NEST:") {
        summary.push(boot_nest(nest));
    }
    if let Some(stripped) = card_field(fields, "STRIPPED:") {
        summary.push(format!(
            "stripped {} (no captain channel)",
            stripped.split_whitespace().next().unwrap_or("")
        ));
    }
    if let Some(install) = card_field(fields, "INSTALL:") {
        summary.push(install.to_owned());
    }
    match refused {
        Some(reason) => summary.push(format!("REFUSED at {reason}")),
        None => summary.extend(parts.get(4).map(|status| (*status).trim().to_owned())),
    }
    Line::styled(
        format!("{marker}{}{HINT}", summary.join(" · ")),
        style(color),
    )
}

fn boot_expanded(header: &str, fields: &[&str]) -> Vec<Line<'static>> {
    let parts = header.trim().split(" / ").collect::<Vec<_>>();
    let (color, marker, _) = boot_verdict(&parts, fields);
    let mut out = vec![Line::styled(
        format!("{marker}{}", header.trim()),
        style(color),
    )];
    out.extend(fields.iter().map(|line| {
        let (key, value) = line.trim_start().split_once(':').unwrap_or((line, ""));
        Line::from(vec![
            Span::styled(format!("  {}: ", key.trim()), style(DIM)),
            Span::styled(
                value.trim().to_owned(),
                style(if key.trim() == "REFUSED" { PINK } else { WHITE }),
            ),
        ])
    }));
    out
}

// seam: the lead wires the COMMS call site (and the Ctrl+O / `/expand` toggle) to this.
/// Renders `content` as a conversation: speakers, folded noise, diagrams, loud errors.
/// `expanded` unfolds every chip back into its source lines.
pub fn conversation_lines(
    content: &str,
    width: u16,
    seat: &str,
    expanded: bool,
) -> Vec<Line<'static>> {
    let source = content.lines().collect::<Vec<_>>();
    let mut out: Vec<Line<'static>> = Vec::new();
    let mut index = 0;
    while index < source.len() {
        let line = source[index];

        // Fenced block: a diagram becomes a component, anything else stays dim code.
        if let Some(tag) = fence(line) {
            let close = (index + 1..source.len()).find(|probe| source[*probe].trim() == "```");
            let Some(close) = close else {
                // Still streaming in: show the raw block rather than guess at half a diagram.
                out.extend(code_lines(&source[index..], width));
                break;
            };
            let rendered = is_diagram_tag(tag)
                .then(|| crate::diagram::render(&source[index + 1..close].join("\n"), width))
                .flatten();
            match rendered {
                Some(lines) => out.extend(lines),
                None => out.extend(code_lines(&source[index..=close], width)),
            }
            index = close + 1;
            continue;
        }

        // Speaker turns.
        if line == "CAPTAIN / MAILBOX" {
            out.push(speaker_rule("CAPTAIN", GOLD, width));
            index += 1;
            continue;
        }
        if line == seat || line == "COPILOT" {
            out.push(speaker_rule(line, CYAN, width));
            index += 1;
            continue;
        }

        // Handoff card.
        if line.trim_start().starts_with("HANDOFF / ") {
            let mut end = index + 1;
            while end < source.len()
                && CARD_FIELDS
                    .iter()
                    .any(|field| source[end].trim_start().starts_with(field))
            {
                end += 1;
            }
            let fields = &source[index + 1..end];
            if expanded {
                out.extend(handoff_expanded(line, fields, width));
            } else {
                out.push(handoff_chip(line, fields, width));
            }
            index = end;
            continue;
        }

        // Boot card. Ahead of the ctl and attention passes, so an `INSTALL: pi failed · …` line
        // stays part of its card instead of shouting on its own.
        if line.trim_start().starts_with("BOOT / ") {
            let mut end = index + 1;
            while end < source.len()
                && BOOT_FIELDS
                    .iter()
                    .any(|field| source[end].trim_start().starts_with(field))
            {
                end += 1;
            }
            let fields = &source[index + 1..end];
            if expanded {
                out.extend(boot_expanded(line, fields));
            } else {
                out.push(boot_chip(line, fields));
            }
            index = end;
            continue;
        }

        // Tool runs: blank lines between two tool calls belong to the run.
        if is_tool(line) {
            let mut end = index;
            let mut titles: Vec<String> = Vec::new();
            while end < source.len() {
                if is_tool(source[end]) {
                    titles.push(tool_title(source[end]));
                    end += 1;
                    continue;
                }
                let mut probe = end;
                while probe < source.len() && source[probe].trim().is_empty() {
                    probe += 1;
                }
                if probe > end && probe < source.len() && is_tool(source[probe]) {
                    end = probe;
                    continue;
                }
                break;
            }
            if expanded {
                out.extend(
                    titles
                        .iter()
                        .map(|title| chip(&format!("▸ {title}"), width, DIM)),
                );
            } else {
                out.push(chip(
                    &format!(
                        "▸ {} tool call{} · last: {}{HINT}",
                        titles.len(),
                        if titles.len() == 1 { "" } else { "s" },
                        titles.last().map(String::as_str).unwrap_or("")
                    ),
                    width,
                    DIM,
                ));
            }
            index = end;
            continue;
        }

        if table_cells(line).is_some() {
            index = push_table(&source, index, width, seat, &mut out);
            continue;
        }

        // `unvrs ctl` output: only a real run of machine lines folds, so a lone `status: ok`
        // inside a sentence stays where the agent wrote it.
        if attention(line).is_none() && is_ctl(line) {
            let mut end = index;
            while end < source.len() && attention(source[end]).is_none() && is_ctl(source[end]) {
                end += 1;
            }
            if end - index >= 3 {
                if expanded {
                    out.extend(code_lines(&source[index..end], width));
                } else {
                    out.push(chip(
                        &format!(
                            "▸ ctl output · {} lines · {}{HINT}",
                            end - index,
                            source[index].trim()
                        ),
                        width,
                        DIM,
                    ));
                }
                index = end;
                continue;
            }
        }

        if line.trim().is_empty() {
            let mut end = index;
            while end < source.len() && source[end].trim().is_empty() {
                end += 1;
            }
            let trailing_blank = out.last().map(Line::width).unwrap_or(0) == 0;
            if expanded {
                out.extend(source[index..end].iter().map(|_| Line::from("")));
            } else if !out.is_empty() && !trailing_blank {
                out.push(Line::from(""));
            }
            index = end;
            continue;
        }

        if let Some((color, marker)) = attention(line) {
            out.push(Line::styled(
                format!("{marker}{}", line.trim()),
                style(color),
            ));
            index += 1;
            continue;
        }

        out.push(Line::styled(line.to_owned(), style(WHITE)));
        index += 1;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text(lines: &[Line<'static>]) -> Vec<String> {
        lines
            .iter()
            .map(|line| {
                line.spans
                    .iter()
                    .map(|span| span.content.as_ref())
                    .collect::<String>()
            })
            .collect()
    }

    const CARD: &str = "HANDOFF / work-7 / pi → codex / PID 1 L1 → PID 2 L2 / owner=2\nFOCUS: land the diagram seam\nGOAL: ship M5\nDONE: parser\nOPEN: polish\nARTIFACTS: src/diagram.rs\nSUGGESTED SKILLS: rust\nCREATED: 12";

    #[test]
    fn the_speaker_rule_keeps_its_spacer_and_fills_the_panel() {
        let line = &conversation_lines("CAPTAIN / MAILBOX", 24, "COPILOT", false)[0];
        assert_eq!(
            text(std::slice::from_ref(line))[0],
            "▌ CAPTAIN ──────────────"
        );
        assert_eq!(line.width(), 24);
    }

    #[test]
    fn expanded_handoff_fields_keep_their_indent_and_spacing() {
        let shown = text(&conversation_lines(CARD, 60, "COPILOT", true));
        assert!(shown.contains(&"  FOCUS: land the diagram seam".to_owned()));
        assert!(shown.contains(&"  CREATED: 12".to_owned()));
    }

    #[test]
    fn comms_formats_pipe_tables_without_losing_prose() {
        let lines = comms_lines(
            "Before\n| From | Result | Evidence |\n| --- | --- | --- |\n| codex | PASS | a long receipt |\nAfter",
            28,
            "COPILOT",
        );
        let text = text(&lines);
        assert_eq!(text[0], "Before");
        assert!(text[1].contains("From") && text[1].contains("Result"));
        assert!(text[2].contains("PASS"));
        assert_eq!(text[3], "After");
    }

    #[test]
    fn pipe_tables_stay_aligned_in_conversation_mode() {
        let lines = conversation_lines(
            "| From | Result |\n| --- | --- |\n| codex | PASS |",
            40,
            "COPILOT",
            false,
        );
        let text = text(&lines);
        assert_eq!(text[0].find('│'), text[1].find('│'));
        assert!(text[1].contains("PASS"));
    }

    #[test]
    fn a_run_of_tool_calls_folds_into_one_chip() {
        let transcript = "COPILOT\nlooking now\n\n[ Read src/ui.rs ]\n\n[ Grep comms_lines ]\n\n[ Edit src/comms.rs ]\n\ndone\n";
        let lines = conversation_lines(transcript, 60, "COPILOT", false);
        let text = text(&lines);
        let chips = text
            .iter()
            .filter(|line| line.contains("tool call"))
            .collect::<Vec<_>>();
        assert_eq!(chips.len(), 1);
        assert!(chips[0].contains("3 tool calls") && chips[0].contains("Edit src/comms.rs"));
        assert!(text.iter().any(|line| line.contains("looking now")));
        assert!(!text.iter().any(|line| line.contains("Read src/ui.rs")));
    }

    #[test]
    fn a_single_tool_call_reads_as_singular() {
        let lines = conversation_lines("[ Read one.rs ]\n", 60, "COPILOT", false);
        assert!(text(&lines)[0].contains("1 tool call ·"));
    }

    #[test]
    fn expanded_shows_every_folded_line() {
        let transcript =
            format!("[ Read a ]\n\n[ Read b ]\n{CARD}\npid: 3\nowner: 3\nstatus: \"delivered\"\n");
        let folded = text(&conversation_lines(&transcript, 70, "COPILOT", false)).join("\n");
        let shown = text(&conversation_lines(&transcript, 70, "COPILOT", true)).join("\n");
        assert!(!folded.contains("Read a") && shown.contains("Read a") && shown.contains("Read b"));
        assert!(!folded.contains("delivered") && shown.contains("delivered"));
        assert!(!folded.contains("SUGGESTED SKILLS") && shown.contains("SUGGESTED SKILLS"));
    }

    #[test]
    fn a_handoff_card_folds_into_a_chip() {
        let lines = conversation_lines(CARD, 100, "COPILOT", false);
        assert_eq!(lines.len(), 1);
        let chip = &text(&lines)[0];
        assert!(chip.starts_with("⇄ HANDOFF work-7"));
        assert!(chip.contains("pi → codex") && chip.contains("owner PID 2"));
        assert!(chip.contains("FOCUS: land the diagram seam"));
    }

    const BOOT: &str = "BOOT / msn_coding_tdd / PID 2 L2 / pi / READY\nPICKER: jev\nNOUL: 0.65\nCONFIDENCE: 1.00\nCHANNEL: captain channel (L2 focused)\nNEST: mattpocock/tdd, mattpocock/diagnosing-bugs\nINSTALL: pi installed 2 skills\nPATHS: .pi/skills/tdd/SKILL.md, .pi/skills/diagnosing-bugs/SKILL.md\nNOTE: closed choice over 12 candidates";

    const REFUSED_BOOT: &str = "BOOT / msn_coding_tdd / PID 2 L2 / pi / REFUSED\nPICKER: rules (fallback from jev: TYPESAFE_API_KEY is not set)\nNOUL: n/a\nCONFIDENCE: n/a\nNEST: (empty · no skill needed)\nINSTALL: pi failed · harness dir not writable\nREFUSED: install · pi harness dir not writable";

    #[test]
    fn a_ready_boot_card_folds_into_one_green_chip() {
        let lines = conversation_lines(BOOT, 60, "COPILOT", false);
        assert_eq!(lines.len(), 1);
        assert_eq!(
            text(&lines)[0],
            format!(
                "⬢ BOOT msn_coding_tdd · jev · noul 0.65 · conf 1.00 · nest tdd, diagnosing-bugs · pi installed 2 skills · READY{HINT}"
            )
        );
        assert_eq!(lines[0].style.fg, Some(GREEN));
    }

    #[test]
    fn a_stripped_boot_says_what_the_seat_lost() {
        let transcript = BOOT.replace(
            "NEST:",
            "STRIPPED: 22 interactive withheld · grill-me, grill-with-docs, to-prd\nNEST:",
        );
        let chip = text(&conversation_lines(&transcript, 60, "COPILOT", false))[0].clone();
        assert!(chip.contains("nest tdd, diagnosing-bugs · stripped 22 (no captain channel) · pi"));
    }

    #[test]
    fn an_empty_nest_reads_as_words_and_n_a_scores_are_dropped() {
        let chip = text(&conversation_lines(REFUSED_BOOT, 80, "COPILOT", false))[0].clone();
        assert!(chip.contains("nest empty (no skill needed)"));
        assert!(!chip.contains("noul") && !chip.contains("conf "));
    }

    #[test]
    fn a_refused_boot_is_pink_and_carries_its_stage_and_reason() {
        for expanded in [false, true] {
            let lines = conversation_lines(REFUSED_BOOT, 80, "COPILOT", expanded);
            assert_eq!(lines[0].style.fg, Some(PINK));
            assert!(text(&lines)[0].starts_with("▲ "));
            assert!(
                text(&lines)
                    .join("\n")
                    .contains("install · pi harness dir not writable")
            );
        }
        assert_eq!(
            text(&conversation_lines(REFUSED_BOOT, 80, "COPILOT", false))[0],
            format!(
                "▲ BOOT msn_coding_tdd · rules (fallback from jev: TYPESAFE_API_KEY is not set) · nest empty (no skill needed) · pi failed · harness dir not writable · REFUSED at install · pi harness dir not writable{HINT}"
            )
        );
    }

    #[test]
    fn an_expanded_boot_card_keeps_every_field_whole() {
        let shown = text(&conversation_lines(BOOT, 40, "COPILOT", true));
        assert_eq!(shown[0], "⬢ BOOT / msn_coding_tdd / PID 2 L2 / pi / READY");
        assert!(shown.contains(&"  PICKER: jev".to_owned()));
        assert!(shown.contains(&"  CHANNEL: captain channel (L2 focused)".to_owned()));
        assert!(shown.contains(
            &"  PATHS: .pi/skills/tdd/SKILL.md, .pi/skills/diagnosing-bugs/SKILL.md".to_owned()
        ));
        assert!(shown.contains(&"  NOTE: closed choice over 12 candidates".to_owned()));
        assert_eq!(shown.len(), 9);
    }

    #[test]
    fn a_boot_card_does_not_swallow_the_prose_after_it() {
        let transcript = format!("COPILOT\n{BOOT}\nready when you are");
        for expanded in [false, true] {
            let shown = text(&conversation_lines(&transcript, 80, "COPILOT", expanded));
            assert!(shown[0].starts_with("▌ COPILOT"));
            assert_eq!(shown.last().map(String::as_str), Some("ready when you are"));
        }
    }

    #[test]
    fn a_header_with_no_fields_yet_still_reads_as_a_chip() {
        let lines = conversation_lines(
            "BOOT / msn_coding_tdd / PID 2 L2 / pi / READY",
            60,
            "COPILOT",
            false,
        );
        assert_eq!(lines.len(), 1);
        assert_eq!(
            text(&lines)[0],
            format!("⬢ BOOT msn_coding_tdd · READY{HINT}")
        );
    }

    #[test]
    fn ctl_dumps_fold_but_a_lone_key_stays_prose() {
        let folded = text(&conversation_lines(
            "pid: 3\nowner: 3\nstatus: \"delivered\"\nhelp[1]:\n",
            60,
            "COPILOT",
            false,
        ));
        assert_eq!(folded.len(), 1);
        assert!(folded[0].contains("ctl output · 4 lines"));
        let kept = text(&conversation_lines(
            "the status: green\nand we move on",
            60,
            "COPILOT",
            false,
        ));
        assert!(kept.iter().any(|line| line.contains("status: green")));
    }

    #[test]
    fn errors_and_attention_are_never_folded() {
        for (transcript, needle) in [
            ("ERROR: transport closed", "transport closed"),
            ("TURN FAILED: rate limited", "rate limited"),
            ("Transport failed: broken pipe", "broken pipe"),
            ("BOOT TIMEOUT: seat 2", "seat 2"),
            ("CAPTAIN INPUT: hold position", "hold position"),
            ("Ownership transferred to PID 3", "PID 3"),
        ] {
            for expanded in [false, true] {
                let lines = conversation_lines(transcript, 60, "COPILOT", expanded);
                assert!(
                    text(&lines).iter().any(|line| line.contains(needle)),
                    "{transcript} vanished"
                );
            }
        }
    }

    #[test]
    fn an_error_beside_ctl_output_survives_the_fold() {
        let lines = conversation_lines(
            "pid: 3\nowner: 3\nERROR: transport closed\nstatus: \"lost\"\nhelp[1]:\nretry: yes",
            70,
            "COPILOT",
            false,
        );
        let text = text(&lines);
        assert!(text.iter().any(|line| line.contains("transport closed")));
        assert!(text.iter().any(|line| line.contains("ctl output")));
    }

    #[test]
    fn speakers_get_a_gutter_and_a_rule() {
        let lines = conversation_lines(
            "CAPTAIN / MAILBOX\nhello\n\nCOPILOT\nhi",
            30,
            "COPILOT",
            false,
        );
        let text = text(&lines);
        assert!(text[0].starts_with("▌ CAPTAIN") && text[0].len() > "▌ CAPTAIN".len());
        assert!(text.iter().any(|line| line.starts_with("▌ COPILOT")));
        assert!(lines[0].width() <= 30);
    }

    #[test]
    fn a_flow_fence_becomes_a_diagram_not_raw_source() {
        let transcript = "COPILOT\nhere is the plan\n\n```flow\ngraph LR\nA[Plan] -> B[Build] -> C[Ship]\n```\n\ndone";
        for expanded in [false, true] {
            let joined = text(&conversation_lines(transcript, 70, "COPILOT", expanded)).join("\n");
            assert!(joined.contains("Plan") && joined.contains("Build") && joined.contains("Ship"));
            assert!(!joined.contains("->") && !joined.contains("```"));
            assert!(joined.contains('▶'));
        }
    }

    #[test]
    fn an_unterminated_fence_falls_back_to_raw_code() {
        let lines = conversation_lines(
            "```flow\ngraph LR\nA[Plan] -> B[Build]",
            70,
            "COPILOT",
            false,
        );
        let joined = text(&lines).join("\n");
        assert!(joined.contains("```flow") && joined.contains("A[Plan] -> B[Build]"));
    }

    #[test]
    fn an_unparseable_diagram_falls_back_to_raw_code() {
        let joined = text(&conversation_lines(
            "```mermaid\nsequenceDiagram\n  Alice knows Bob\n```",
            70,
            "COPILOT",
            false,
        ))
        .join("\n");
        assert!(joined.contains("sequenceDiagram"));
    }

    /// Prose is left long on purpose: COMMS renders into a wrapping Paragraph. Everything the
    /// renderer *builds* — rules, chips, tables, diagrams — must fit unwrapped, or the layout
    /// breaks apart at narrow terminal widths.
    #[test]
    fn built_lines_never_exceed_the_width_they_were_given() {
        let transcript = format!(
            "CAPTAIN / MAILBOX\n{CARD}\n\nCOPILOT\n[ Read a very long file path that goes on ]\npid: 3\nowner: 3\nstatus: \"delivered\"\n\n| From | Result |\n| --- | --- |\n| codex | PASS |\n\n```flow\nA[One] -> B[Two]\n```\n"
        );
        for width in [0u16, 1, 4, 12, 30, 80] {
            for expanded in [false, true] {
                for line in conversation_lines(&transcript, width, "COPILOT", expanded) {
                    assert!(line.width() <= width as usize, "overflow at width {width}");
                }
            }
        }
    }

    #[test]
    fn prose_is_left_whole_for_the_wrapping_paragraph() {
        let long = "a sentence far wider than the panel it will be drawn into";
        let lines = conversation_lines(long, 10, "COPILOT", false);
        assert_eq!(text(&lines)[0], long);
    }

    #[test]
    fn blank_runs_collapse_when_folded_and_survive_when_expanded() {
        let folded = conversation_lines("a\n\n\n\nb", 20, "COPILOT", false);
        let shown = conversation_lines("a\n\n\n\nb", 20, "COPILOT", true);
        assert_eq!(folded.len(), 3);
        assert_eq!(shown.len(), 5);
    }
}
