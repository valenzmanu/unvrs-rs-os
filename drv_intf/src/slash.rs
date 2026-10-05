//! `/` slash menu: command table, suggestions and the popup above the input bar.
use crate::theme::*;
use ratatui::{
    Frame,
    layout::Rect,
    style::{Color, Style},
    text::{Line, Span},
    widgets::{Clear, Paragraph},
};

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Id {
    Remember,
    Recall,
    Forget,
    DeleteNote,
    Scope,
    Developer,
    Onboard,
    More,
    Missions,
    Admit,
    Boot,
    Picker,
    Launch,
    Mission,
    Spawn,
    Comms,
    Logs,
    Errors,
    Search,
    Obs,
    TestHandoffs,
    Galaga,
    Motion,
    Expand,
    Cancel,
    Menu,
    Help,
    New,
    Quit,
}
pub struct Cmd {
    pub id: Id,
    /// Includes the leading slash, e.g. `/mission`.
    pub name: &'static str,
    /// Argument hint, e.g. `<goal>`; empty when the command takes none.
    pub args: &'static str,
    pub desc: &'static str,
}
pub const COMMANDS: &[Cmd] = &[
    Cmd {
        id: Id::Remember,
        name: "/remember",
        args: "<note>",
        desc: "Keep a note · --pin --open --scope",
    },
    Cmd {
        id: Id::Recall,
        name: "/recall",
        args: "<query>",
        desc: "Find across the universe",
    },
    Cmd {
        id: Id::Forget,
        name: "/forget",
        args: "<id>",
        desc: "Retire a note; keep it findable",
    },
    Cmd {
        id: Id::DeleteNote,
        name: "/delete-note",
        args: "<id>",
        desc: "Permanently remove a note",
    },
    Cmd {
        id: Id::Scope,
        name: "/scope",
        args: "<area:name>",
        desc: "Change area · path:rel mission:id",
    },
    Cmd {
        id: Id::Developer,
        name: "/developer",
        args: "",
        desc: "Show or hide system events",
    },
    Cmd {
        id: Id::Onboard,
        name: "/onboard",
        args: "",
        desc: "Explore your universe",
    },
    Cmd {
        id: Id::More,
        name: "/more",
        args: "",
        desc: "More actions",
    },
    Cmd {
        id: Id::Missions,
        name: "/missions",
        args: "",
        desc: "List mission files · Enter admits one",
    },
    Cmd {
        id: Id::Admit,
        name: "/admit",
        args: "<mission>",
        desc: "Admit a mission file (outcome + done check)",
    },
    Cmd {
        id: Id::Boot,
        name: "/boot",
        args: "[pid] [rules|jev]",
        desc: "Nest this seat: pick skills, install, ready",
    },
    Cmd {
        id: Id::Picker,
        name: "/picker",
        args: "[rules|jev]",
        desc: "Choose the Boot skill picker backend",
    },
    Cmd {
        id: Id::Launch,
        name: "/launch",
        args: "",
        desc: "New L2 worker on the admitted mission",
    },
    Cmd {
        id: Id::Mission,
        name: "/mission",
        args: "<goal>",
        desc: "Launch a new L2 worker on this goal",
    },
    Cmd {
        id: Id::Spawn,
        name: "/spawn",
        args: "<goal>",
        desc: "New worker under the selected seat (L2→L3)",
    },
    Cmd {
        id: Id::Comms,
        name: "/comms",
        args: "",
        desc: "Back to the conversation with this seat",
    },
    Cmd {
        id: Id::Logs,
        name: "/logs",
        args: "",
        desc: "Flight logs · every event this flight",
    },
    Cmd {
        id: Id::Errors,
        name: "/errors",
        args: "",
        desc: "Flight logs · errors only",
    },
    Cmd {
        id: Id::Search,
        name: "/search",
        args: "<text>",
        desc: "Search the flight logs for text",
    },
    Cmd {
        id: Id::Obs,
        name: "/obs",
        args: "",
        desc: "Observation deck · ship telemetry",
    },
    Cmd {
        id: Id::TestHandoffs,
        name: "/test-handoffs",
        args: "",
        desc: "Run the live 12-edge handoff matrix",
    },
    Cmd {
        id: Id::Galaga,
        name: "/galaga",
        args: "",
        desc: "Play or resume Galaga while you wait",
    },
    Cmd {
        id: Id::Motion,
        name: "/motion",
        args: "",
        desc: "Toggle cockpit animation on or off",
    },
    Cmd {
        id: Id::Expand,
        name: "/expand",
        args: "",
        desc: "Show or hide tool and ctl detail in COMMS",
    },
    Cmd {
        id: Id::Cancel,
        name: "/cancel",
        args: "",
        desc: "Cancel the selected seat's current turn",
    },
    Cmd {
        id: Id::Menu,
        name: "/menu",
        args: "",
        desc: "Full command palette (Ctrl+K)",
    },
    Cmd {
        id: Id::Help,
        name: "/help",
        args: "",
        desc: "Controls · keyboard and mouse reference",
    },
    Cmd {
        id: Id::New,
        name: "/new",
        args: "",
        desc: "Archive this session, start a fresh flight",
    },
    Cmd {
        id: Id::Quit,
        name: "/quit",
        args: "",
        desc: "End the flight · restored on next launch",
    },
];

/// Commands to suggest while `input` is a bare `/word` (no whitespace yet); empty otherwise.
pub fn suggestions(input: &str) -> Vec<&'static Cmd> {
    if !input.starts_with('/') || input.contains('\n') || input.chars().any(char::is_whitespace) {
        return vec![];
    }
    if input == "/" {
        return [Id::Remember, Id::Missions, Id::Boot, Id::Onboard, Id::More]
            .into_iter()
            .filter_map(|id| COMMANDS.iter().find(|c| c.id == id))
            .collect();
    }
    if input == "/more" {
        return COMMANDS.iter().filter(|c| c.id != Id::More).collect();
    }
    let needle = input.to_ascii_lowercase();
    // Rank 0: name prefix, 1: name substring, 2: description substring. Table order breaks ties.
    let mut ranked: Vec<(u8, &'static Cmd)> = vec![];
    for cmd in COMMANDS {
        let name = cmd.name.to_ascii_lowercase();
        let rank = if name.starts_with(&needle) {
            0
        } else if name.contains(&needle) {
            1
        } else if cmd
            .desc
            .to_ascii_lowercase()
            .contains(needle.trim_start_matches('/'))
        {
            2
        } else {
            continue;
        };
        ranked.push((rank, cmd));
    }
    ranked.sort_by_key(|(rank, _)| *rank);
    ranked.into_iter().map(|(_, cmd)| cmd).collect()
}
/// Exact command plus trimmed argument text, e.g. `/search boot` → (Search, "boot").
pub fn parse(input: &str) -> Option<(Id, &str)> {
    let input = input.trim();
    let (name, args) = match input.find(char::is_whitespace) {
        Some(at) => (&input[..at], input[at..].trim()),
        None => (input, ""),
    };
    COMMANDS
        .iter()
        .find(|cmd| cmd.name.eq_ignore_ascii_case(name))
        .map(|cmd| (cmd.id, args))
}
/// Text to drop into the input on Tab: `"/search "` for commands that take args, else `"/logs"`.
pub fn complete(cmd: &Cmd) -> String {
    if cmd.args.is_empty() {
        cmd.name.to_string()
    } else {
        format!("{} ", cmd.name)
    }
}
/// Popup directly above `anchor` (the input box), clamped inside `area`.
pub fn draw(f: &mut Frame, area: Rect, anchor: Rect, items: &[&Cmd], selected: usize) {
    if items.is_empty() || area.width < 6 || area.height < 3 {
        return;
    }
    // Space between the top of `area` and the anchor is all the popup may use.
    let ceiling = area.y;
    let floor = anchor.y.clamp(ceiling, area.bottom());
    let room = floor.saturating_sub(ceiling);
    if room < 3 {
        return;
    }
    let height = ((items.len().min(8) as u16) + 2).min(room);
    let visible = height.saturating_sub(2) as usize;
    if visible == 0 {
        return;
    }
    let width = anchor.width.clamp(6, area.width);
    let x = anchor
        .x
        .clamp(area.x, area.right().saturating_sub(width).max(area.x));
    let r = Rect::new(x, floor - height, width, height);
    let selected = selected.min(items.len() - 1);
    let start = (selected + 1).saturating_sub(visible);

    f.render_widget(Clear, r);
    let title = one_line(
        "/ COMMANDS · ↑↓ select · Tab complete · Enter run · Esc close",
        width.saturating_sub(4),
    );
    let panel = block(title, CYAN);
    let inner = panel.inner(r);
    f.render_widget(panel, r);
    if inner.width == 0 || inner.height == 0 {
        return;
    }
    // One column for `▸ /name <args>`, the rest for the description.
    let gutter = items
        .iter()
        .skip(start)
        .take(visible)
        .map(|cmd| {
            cmd.name.chars().count()
                + if cmd.args.is_empty() {
                    0
                } else {
                    1 + cmd.args.chars().count()
                }
        })
        .max()
        .unwrap_or(0)
        + 5;
    for (row, cmd) in items.iter().enumerate().skip(start).take(visible) {
        let on = row == selected;
        let y = inner.y + (row - start) as u16;
        if y >= inner.bottom() {
            break;
        }
        let mut used = 2;
        let mut spans = vec![
            Span::styled(
                if on { "▸ " } else { "  " },
                style(if on { GOLD } else { DIM }),
            ),
            Span::styled(cmd.name, style(if on { GOLD } else { CYAN })),
        ];
        used += cmd.name.chars().count();
        if !cmd.args.is_empty() {
            spans.push(Span::styled(format!(" {}", cmd.args), style(DIM)));
            used += 1 + cmd.args.chars().count();
        }
        let left = gutter.max(used + 1);
        if left < inner.width as usize {
            spans.push(Span::raw(" ".repeat(left - used)));
            let desc = one_line(cmd.desc, (inner.width as usize - left) as u16);
            spans.push(Span::styled(desc, style(if on { WHITE } else { DIM })));
        }
        f.render_widget(
            Paragraph::new(Line::from(spans)).style(Style::default().bg(if on {
                Color::Rgb(30, 46, 66)
            } else {
                PANEL
            })),
            Rect::new(inner.x, y, inner.width, 1),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::{Terminal, backend::TestBackend};

    fn names(input: &str) -> Vec<&'static str> {
        suggestions(input).iter().map(|cmd| cmd.name).collect()
    }

    #[test]
    fn every_id_has_exactly_one_command() {
        let ids = [
            Id::Remember,
            Id::Recall,
            Id::Forget,
            Id::DeleteNote,
            Id::Scope,
            Id::Developer,
            Id::Onboard,
            Id::More,
            Id::Missions,
            Id::Admit,
            Id::Boot,
            Id::Picker,
            Id::Launch,
            Id::Spawn,
            Id::Mission,
            Id::Comms,
            Id::Logs,
            Id::Errors,
            Id::Search,
            Id::Obs,
            Id::TestHandoffs,
            Id::Galaga,
            Id::Motion,
            Id::Expand,
            Id::Cancel,
            Id::Menu,
            Id::Help,
            Id::New,
            Id::Quit,
        ];
        assert_eq!(COMMANDS.len(), ids.len());
        for id in ids {
            assert_eq!(
                COMMANDS.iter().filter(|cmd| cmd.id == id).count(),
                1,
                "{id:?} needs exactly one command"
            );
        }
        for cmd in COMMANDS {
            assert!(cmd.name.starts_with('/'), "{} needs a slash", cmd.name);
            assert!(
                cmd.desc.chars().count() <= 48,
                "{} description too long",
                cmd.name
            );
        }
    }

    #[test]
    fn bare_slash_is_short_and_more_has_the_rest() {
        assert_eq!(
            names("/"),
            vec!["/remember", "/missions", "/boot", "/onboard", "/more"]
        );
    }

    #[test]
    fn whitespace_or_plain_text_closes_the_suggestions() {
        assert!(suggestions("/logs ").is_empty());
        assert!(suggestions("/mission ship it").is_empty());
        assert!(suggestions("hello").is_empty());
        assert!(suggestions("").is_empty());
        assert!(suggestions("/logs\n").is_empty());
    }

    #[test]
    fn prefix_matches_rank_above_substring_then_description() {
        // "/m" → prefix matches in table order; nothing else outranks them.
        let m = names("/m");
        assert_eq!(
            &m[..5],
            &["/more", "/missions", "/mission", "/motion", "/menu"]
        );
        // "/error" prefixes /errors; /logs only mentions "events", so the match is on description.
        let e = names("/error");
        assert_eq!(e[0], "/errors");
        // Substring on the name beats a description hit.
        let s = names("/hand");
        assert_eq!(s[0], "/test-handoffs");
    }

    #[test]
    fn description_matches_are_found_when_no_name_matches() {
        let hits = names("/telemetry");
        assert_eq!(hits, vec!["/obs"]);
    }

    #[test]
    fn parse_reads_args_case_insensitively() {
        assert_eq!(parse("/search boot pid"), Some((Id::Search, "boot pid")));
        assert_eq!(parse("/SeArCh   Boot  "), Some((Id::Search, "Boot")));
        assert_eq!(parse("/logs"), Some((Id::Logs, "")));
        assert_eq!(parse("  /quit  "), Some((Id::Quit, "")));
        assert_eq!(parse("/nope"), None);
        assert_eq!(parse("hello"), None);
    }

    #[test]
    fn boot_surface_parses_with_optional_args() {
        assert_eq!(parse("/admit coding-tdd"), Some((Id::Admit, "coding-tdd")));
        assert_eq!(parse("/boot 3 jev"), Some((Id::Boot, "3 jev")));
        assert_eq!(parse("/picker"), Some((Id::Picker, "")));
        assert_eq!(parse("/missions"), Some((Id::Missions, "")));
        assert_eq!(names("/boo")[0], "/boot");
    }

    #[test]
    fn test_handoffs_still_parses() {
        assert_eq!(parse("/test-handoffs"), Some((Id::TestHandoffs, "")));
    }

    #[test]
    fn complete_adds_a_space_only_for_commands_with_args() {
        let find = |name: &str| COMMANDS.iter().find(|c| c.name == name).unwrap();
        assert_eq!(complete(find("/search")), "/search ");
        assert_eq!(complete(find("/mission")), "/mission ");
        assert_eq!(complete(find("/logs")), "/logs");
    }

    fn render(w: u16, h: u16, anchor: Rect, items: &[&Cmd], selected: usize) -> String {
        let mut t = Terminal::new(TestBackend::new(w, h)).unwrap();
        t.draw(|f| {
            let area = f.area();
            draw(f, area, anchor, items, selected);
        })
        .unwrap();
        let buffer = t.backend().buffer().clone();
        (0..h)
            .map(|y| {
                (0..w)
                    .map(|x| buffer[(x, y)].symbol().to_string())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn popup_sits_directly_above_the_anchor_inside_the_area() {
        let items = suggestions("/");
        let anchor = Rect::new(3, 38, 120, 3);
        let mut t = Terminal::new(TestBackend::new(140, 44)).unwrap();
        t.draw(|f| draw(f, f.area(), anchor, &items, 0)).unwrap();
        let buffer = t.backend().buffer().clone();
        // 8 rows + 2 borders, bottom border on the row just above the anchor.
        let top = 38 - 7;
        assert_eq!(buffer[(3, top)].symbol(), "▛");
        assert_eq!(buffer[(3, 37)].symbol(), "▙");
        assert_eq!(buffer[(122, top)].symbol(), "▜");
        let row: String = (0..140).map(|x| buffer[(x, top + 1)].symbol()).collect();
        assert!(row.contains("▸ /remember"), "{row:?}");
        assert!(row.contains("Keep a note"), "{row:?}");
        assert_eq!(buffer[(6, top + 1)].style().fg, Some(GOLD));
        assert_eq!(buffer[(6, top + 2)].style().fg, Some(CYAN));
    }

    #[test]
    fn scroll_window_follows_the_selection() {
        let items = suggestions("/more");
        let text = render(80, 44, Rect::new(0, 20, 80, 3), &items, items.len() - 1);
        assert!(text.contains("/quit"), "{text}");
        assert!(!text.contains("/mission"), "{text}");
    }

    #[test]
    fn empty_items_and_tiny_rects_never_panic() {
        let items = suggestions("/");
        assert!(!render(40, 10, Rect::new(0, 5, 40, 3), &[], 0).contains('┌'));
        for (w, h) in [(1, 1), (2, 2), (4, 3), (6, 4), (10, 5), (65, 23)] {
            for anchor_y in 0..h {
                render(w, h, Rect::new(0, anchor_y, w, 1), &items, 14);
            }
        }
        // Anchor wider than / outside the area is clamped rather than fatal.
        render(20, 12, Rect::new(15, 9, 60, 3), &items, 3);
    }
}
