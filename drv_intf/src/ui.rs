use crate::bridge::Ship;
use crate::comms::comms_lines;
use crate::theme::*;
use ratatui::{
    Frame,
    layout::{Constraint, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span, Text},
    widgets::{
        Block, Cell, Clear, Paragraph, Row, Scrollbar, ScrollbarOrientation, ScrollbarState, Table,
        Wrap,
    },
};

pub fn draw(f: &mut Frame, s: &mut Ship) {
    let area = f.area();
    if let Some(scene) = s.onboard.filter(|scene| *scene < 3) {
        crate::world::draw(f, area, if s.motion { s.tick } else { 0 }, Some(scene));
        return;
    }
    if !s.logs && !s.telemetry && !s.matrix_view && s.palette.is_none() {
        overworld(f, s);
        return;
    }
    f.render_widget(
        Block::default().style(Style::default().bg(BG).fg(WHITE)),
        area,
    );
    if area.width < 65 || area.height < 23 {
        f.render_widget(Paragraph::new("UNVRS / BRIDGE\n\nEnlarge the terminal to at least 65 × 23.\nRecommended: 140 × 44.\n\nF10 / Ctrl+Q to exit.").style(style(CYAN)).block(block("SIGNAL TOO SMALL",DIM)),area);
        return;
    }
    let rows = Layout::vertical([
        Constraint::Length(5),
        Constraint::Min(12),
        Constraint::Length(if s.input.contains('\n') { 5 } else { 3 }),
        Constraint::Length(2),
    ])
    .margin(1)
    .split(area);
    header(f, rows[0], s);
    let cols = Layout::horizontal([
        Constraint::Length(if area.width > 100 { 27 } else { 21 }),
        Constraint::Min(30),
    ])
    .spacing(1)
    .split(rows[1]);
    crew(f, cols[0], s);
    let right = Layout::vertical([
        Constraint::Length(if area.height >= 40 {
            14
        } else if area.height >= 32 {
            11
        } else {
            7
        }),
        Constraint::Min(5),
    ])
    .spacing(1)
    .split(cols[1]);
    crate::nav::draw(f, right[0], s);
    let a = &s.obs.crew[s.selected];
    let matrix_text = s.matrix_view.then(|| {
        s.handoff_matrix_text()
            .unwrap_or_else(|| "No handoff matrix is running.".into())
    });
    let title = if s.matrix_view {
        format!(
            "COMMS / {}",
            matrix_text
                .as_deref()
                .and_then(|text| text.lines().next())
                .unwrap_or("HANDOFF MATRIX")
                .replace("HANDOFF MATRIX / ", "")
        )
    } else if s.logs {
        format!(
            "FLIGHT LOGS / {} · {}",
            s.log_filter.unwrap_or("ALL EVENTS"),
            crate::logs::summary(&s.visible_events(), s.log_filter, &s.log_query)
        )
    } else if s.telemetry {
        "TELEMETRY / OBS / SCAN".into()
    } else {
        format!(
            "COMMS / PID {:02} / {} · {} · {}",
            a.pid, a.harness, a.model, a.effort
        )
    };
    let panel = block(title, CYAN);
    let inner = panel.inner(right[1]);
    f.render_widget(panel, right[1]);
    let table_height = if s.telemetry {
        inner.height.saturating_sub(2).min(5)
    } else {
        0
    };
    if s.telemetry && table_height >= 3 {
        let rows = s.obs.crew.iter().map(|a| {
            Row::new(vec![
                Cell::from(format!("{:02}", a.pid)),
                Cell::from(a.name.clone()),
                Cell::from(a.harness.clone()),
                Cell::from(a.state.clone()),
            ])
            .style(style(status(&a.state)))
        });
        f.render_widget(
            Table::new(
                rows,
                [
                    Constraint::Length(3),
                    Constraint::Min(8),
                    Constraint::Length(8),
                    Constraint::Length(8),
                ],
            )
            .header(Row::new(["PID", "SEAT", "HARNESS", "STATE"]).style(style(DIM)))
            .column_spacing(1),
            Rect::new(inner.x, inner.y, inner.width, table_height),
        );
    }
    let content_area = Rect::new(
        inner.x,
        inner.y + table_height,
        inner.width,
        inner.height.saturating_sub(table_height),
    );
    if s.matrix_view {
        draw_matrix(
            f,
            content_area,
            &mut s.scroll,
            &mut s.last_lines,
            matrix_text.as_deref().unwrap_or_default(),
        );
    } else {
        let lines = if s.logs {
            let mut lines = comms_lines(&s.log_paths(), content_area.width, &a.name);
            lines.push(Line::from(""));
            lines.extend(crate::logs::lines(
                &s.visible_events(),
                s.log_filter,
                &s.log_query,
            ));
            lines
        } else if s.telemetry {
            let content = format!(
                "SHIP / {} seats / {} working / {} transfers\nHEALTH / {} need attention\n\n{}",
                s.obs.crew.len(),
                s.obs.working(),
                s.obs.handoffs.len(),
                s.obs
                    .crew
                    .iter()
                    .filter(|a| matches!(a.state.as_str(), "offline" | "blocked"))
                    .count(),
                s.visible_events()
                    .iter()
                    .cloned()
                    .collect::<Vec<_>>()
                    .join("\n")
            );
            comms_lines(&content, content_area.width, &a.name)
        } else {
            crate::comms::conversation_lines(&a.transcript, content_area.width, &a.name, s.expanded)
        };
        let paragraph = Paragraph::new(Text::from(lines)).wrap(Wrap { trim: false });
        let total = paragraph.line_count(content_area.width);
        let max_scroll = total
            .saturating_sub(content_area.height as usize)
            .min(u16::MAX as usize) as u16;
        if s.scroll > 0 && s.last_lines > 0 {
            s.scroll = s
                .scroll
                .saturating_add(total.saturating_sub(s.last_lines).min(u16::MAX as usize) as u16);
        }
        s.scroll = s.scroll.min(max_scroll);
        s.last_lines = total;
        let offset = max_scroll.saturating_sub(s.scroll);
        f.render_widget(paragraph.scroll((offset, 0)), content_area);
        if total > content_area.height as usize {
            let mut scrollbar = ScrollbarState::new(total).position(offset as usize);
            f.render_stateful_widget(
                Scrollbar::new(ScrollbarOrientation::VerticalRight)
                    .thumb_style(style(CYAN))
                    .track_style(style(DIM)),
                content_area,
                &mut scrollbar,
            );
        }
        if s.scroll > 0 {
            pixel(
                f,
                right[1],
                right[1].width.saturating_sub(23),
                0,
                &format!(" ↑ {} lines / End live ", s.scroll),
                GOLD,
            );
        }
    }
    let searching = s.logs && !s.matrix_view && !s.input.starts_with('/');
    let title = if s.mission_input {
        "NEW MISSION → L2 / Enter launches · Shift/Alt+Enter line break · Esc cancels".into()
    } else if searching {
        "SEARCH LOGS / type to filter · pid:2 narrows · / commands · Esc back to COMMS".into()
    } else if s.input.starts_with('/') {
        "COMMAND / ↑ ↓ select · Tab complete · Enter run · Esc close".into()
    } else {
        format!(
            "CAPTAIN → {} / Enter sends · Shift/Alt+Enter line break",
            a.name
        )
    };
    let input = block(
        title,
        if s.mission_input {
            PINK
        } else if searching {
            CYAN
        } else {
            GOLD
        },
    );
    let inner = input.inner(rows[2]);
    f.render_widget(input, rows[2]);
    let (visible, cursor_row, cursor_col) = input_display(
        if searching { &s.log_query } else { &s.input },
        inner.width,
        inner.height,
    );
    f.render_widget(Paragraph::new(visible).style(style(GOLD)), inner);
    f.set_cursor_position((
        inner
            .x
            .saturating_add(2 + cursor_col)
            .min(inner.right().saturating_sub(1)),
        inner.y + cursor_row,
    ));
    let hint = if area.width > 110 {
        " / commands   Ctrl/Cmd+K menu   Tab / click crew   Enter send   Ctrl+O detail   F8 Galaga   F10 dock"
    } else {
        " / commands  Ctrl+K menu  Tab crew  F10 dock"
    };
    f.render_widget(Paragraph::new(vec![Line::styled(hint,style(DIM)),Line::styled(if s.notice.is_empty(){if s.demo {" TRAINING MODE / simulated crew · no model calls · no tool execution"}else{" LIVE / Full local access · flight saved in .unvrs/ · relaunch resumes this flight"}}else{&s.notice},style(if s.notice.is_empty(){DIM}else{GOLD}))]),rows[3]);
    let items = s.slash_items();
    if !items.is_empty() {
        crate::slash::draw(
            f,
            area,
            rows[2],
            &items,
            s.slash_selected.min(items.len() - 1),
        );
    }
    if s.game.active {
        crate::game::draw(f, area, &mut s.game);
    }
    if let Some(menu) = &s.palette {
        use crate::menu::{Form, Page};
        let items = s.palette_items();
        let r = palette_rect(area);
        f.render_widget(Clear, r);
        let panel = block(menu.title(), CYAN);
        let inner = panel.inner(r);
        f.render_widget(panel, r);
        let form = matches!(menu.page, Page::Compose(_));
        let label = if menu.page == Page::Help {
            "Keyboard and mouse reference"
        } else if form {
            "Write below · Enter submits"
        } else {
            "Search actions · type to filter"
        };
        pixel(f, inner, 1, 0, label, DIM);
        let max = inner.width.saturating_sub(5) as usize;
        let query: String = menu
            .query
            .chars()
            .rev()
            .take(max)
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect();
        pixel(f, inner, 1, 1, &format!("> {query}"), GOLD);
        f.set_cursor_position((
            inner.x + 3 + Line::from(query).width().min(max) as u16,
            inner.y + 1,
        ));
        let body = Rect::new(
            inner.x + 1,
            inner.y + 3,
            inner.width.saturating_sub(2),
            inner.height.saturating_sub(6),
        );
        let (start, visible) = palette_window(area, s);
        let explanation = match menu.page {
            Page::Compose(Form::Message(_)) => Some(
                "MESSAGE\nEnter sends to this seat. Escape returns without sending.\nYour main COMMS draft is preserved.",
            ),
            Page::Compose(Form::Mission) => Some(
                "MISSION GOAL\nDescribe the outcome for a new L2 worker.\nEnter launches the worker; Escape goes back.",
            ),
            Page::Compose(Form::Handoff(_, _)) => Some(
                "NEXT FOCUS / GOAL\nDescribe what the receiving L2 seat should do next.\nEnter transfers the work; Escape goes back.\nUse ctl handoff for a full summary with artifact links.",
            ),
            Page::Help => Some(
                "Tab / click       Select crew\nEnter             Send a COMMS message\nShift/Alt+Enter   Add a COMMS line\nCtrl/Cmd+K        Open commands, logs, Galaga\nF2 / F3           Mission / observation\n/missions /boot   Admit a mission · nest a seat\nF4 / F5           Cancel turn / recorder\nF6 / F7           Copilot / motion\nF8                Play or pause Galaga\nWheel / PgUp/Dn   Scroll history\nHome / End        Oldest / live output\nF10 / Ctrl+Q      End flight",
            ),
            _ => None,
        };
        if let Some(text) = explanation {
            f.render_widget(
                Paragraph::new(text)
                    .style(style(WHITE))
                    .wrap(Wrap { trim: false }),
                body,
            );
        } else if items.is_empty() {
            f.render_widget(
                Paragraph::new(
                    "No matching actions.\nBackspace to edit or Ctrl+U to clear search.",
                )
                .style(style(DIM)),
                body,
            );
        } else {
            for (row, (i, item)) in items
                .iter()
                .enumerate()
                .skip(start)
                .take(visible)
                .enumerate()
            {
                let selected = i == menu.selected;
                let line = Rect::new(body.x, body.y + row as u16 * 2, body.width, 1);
                f.render_widget(
                    Paragraph::new(format!(
                        "{} {}",
                        if selected { "▸" } else { " " },
                        item.label
                    ))
                    .style(
                        Style::default()
                            .fg(if !item.enabled {
                                DIM
                            } else if selected {
                                GOLD
                            } else {
                                WHITE
                            })
                            .bg(if selected {
                                Color::Rgb(30, 46, 66)
                            } else {
                                PANEL
                            }),
                    ),
                    line,
                );
                f.render_widget(
                    Paragraph::new(format!("  {}", item.detail)).style(style(DIM)),
                    Rect::new(line.x, line.y + 1, line.width, 1),
                );
            }
        }
        let footer = Rect::new(
            inner.x + 1,
            inner.bottom().saturating_sub(2),
            inner.width.saturating_sub(2),
            2,
        );
        let hint = if !menu.error.is_empty() {
            menu.error.clone()
        } else if menu.page == Page::Galaga {
            "← → / A D move · Space fires\nEscape back · crew attention always pauses play".into()
        } else {
            format!(
                "{} · Esc back · Ctrl+K close",
                if form {
                    "Enter submit"
                } else {
                    "↑ ↓ select · Enter open · click action"
                }
            )
        };
        f.render_widget(
            Paragraph::new(hint)
                .wrap(Wrap { trim: false })
                .style(style(if menu.error.is_empty() { DIM } else { PINK })),
            footer,
        );
    }
}
fn overworld(f: &mut Frame, s: &mut Ship) {
    let area = f.area();
    crate::world::draw(f, area, if s.motion { s.tick } else { 0 }, None);
    if area.width < 20 || area.height < 8 {
        return;
    }
    let a = &s.obs.crew[s.selected];
    let scope = s
        .memory
        .session(a.pid)
        .map(|v| v.active_scope)
        .unwrap_or_default();
    let name = scope.area.as_deref().unwrap_or("Universe");
    let rows = Layout::vertical([
        Constraint::Length(2),
        Constraint::Length(if area.height > 22 { 7 } else { 4 }),
        Constraint::Min(1),
        Constraint::Length(1),
        Constraint::Length(3),
        Constraint::Length(1),
    ])
    .margin(1)
    .split(area);
    f.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled("◆  UNVRS   ", style(CYAN)),
            Span::styled(name, style(WHITE)),
        ])),
        rows[0],
    );
    if let Some(mission) = &s.mission {
        pixel(f, rows[0], 0, 1, &format!("MISSION {}", mission.id), DIM);
    }
    let (notes, outside) = s.memory.open_notes(a.pid).unwrap_or_default();
    let title = format!("OPEN · {} here · {outside} elsewhere", notes.len());
    let panel = block(title, GREEN);
    let inner = panel.inner(rows[1]);
    f.render_widget(Clear, rows[1]);
    f.render_widget(panel, rows[1]);
    let mut lines = notes
        .iter()
        .take(inner.height as usize)
        .map(|n| {
            Line::from(vec![
                Span::styled(" ◆ ", style(GOLD)),
                Span::styled(format!("{}  {}", n.title, n.id), style(WHITE)),
            ])
        })
        .collect::<Vec<_>>();
    if notes.is_empty() {
        lines.push(Line::styled("Nothing open in this area", style(DIM)));
    }
    f.render_widget(Paragraph::new(lines), inner);
    let content =
        s.memory_output
            .as_deref()
            .unwrap_or(if s.comms_view { &a.transcript } else { "" });
    if !content.trim().is_empty() {
        let panel = block(
            if s.memory_output.is_some() {
                "MEMORY"
            } else {
                "CONVERSATION"
            },
            DIM,
        );
        let inner = panel.inner(rows[2]);
        f.render_widget(Clear, rows[2]);
        f.render_widget(panel, rows[2]);
        let lines = crate::comms::conversation_lines(content, inner.width, &a.name, s.expanded);
        let paragraph = Paragraph::new(lines).wrap(Wrap { trim: false });
        let total = paragraph.line_count(inner.width);
        let max = total
            .saturating_sub(inner.height as usize)
            .min(u16::MAX as usize) as u16;
        if s.scroll > 0 && s.last_lines > 0 {
            s.scroll = s
                .scroll
                .saturating_add(total.saturating_sub(s.last_lines).min(u16::MAX as usize) as u16);
        }
        s.scroll = s.scroll.min(max);
        s.last_lines = total;
        f.render_widget(paragraph.scroll((max.saturating_sub(s.scroll), 0)), inner);
    } else if rows[2].height > 2 {
        pixel(
            f,
            rows[2],
            2,
            1,
            "Your universe is here. /onboard explores it.",
            DIM,
        );
    }
    let state = match a.state.as_str() {
        "working" => "working",
        "blocked" | "offline" => "needs you",
        _ => "waiting",
    };
    f.render_widget(
        Paragraph::new(format!("◆ {} · {} · {state}", a.name, a.harness))
            .style(style(status(&a.state))),
        rows[3],
    );
    let panel = block(format!("Talk to {}", a.name), GOLD);
    let inner = panel.inner(rows[4]);
    f.render_widget(Clear, rows[4]);
    f.render_widget(panel, rows[4]);
    let (text, row, col) = input_display(&s.input, inner.width, inner.height);
    f.render_widget(Paragraph::new(text).style(style(WHITE)), inner);
    if inner.width > 0 && inner.height > 0 {
        f.set_cursor_position((
            inner.x.saturating_add(2 + col).min(inner.right() - 1),
            inner.y + row,
        ));
    }
    f.render_widget(
        Paragraph::new(if s.notice.is_empty() {
            "/ actions · Tab seat · Enter send"
        } else {
            &s.notice
        })
        .style(style(DIM)),
        rows[5],
    );
    let items = s.slash_items();
    if !items.is_empty() {
        crate::slash::draw(
            f,
            area,
            rows[4],
            &items,
            s.slash_selected.min(items.len() - 1),
        );
    }
    if s.game.active {
        crate::game::draw(f, area, &mut s.game);
    }
}

fn header(f: &mut Frame, r: Rect, s: &Ship) {
    let logo = [
        "█ █ █▄ █ █ █ █▀▄ █▀▀",
        "█ █ █ ▀█ ▀▄▀ █▀▄ ▀▀█",
        "▀▀▀ ▀  ▀  ▀  ▀ ▀ ▀▀▀",
    ];
    for (y, line) in logo.iter().enumerate() {
        pixel(f, r, 0, y as u16, line, CYAN);
    }
    pixel(
        f,
        r,
        24,
        0,
        "U N V R S   /   C A P T A I N ' S   B R I D G E",
        WHITE,
    );
    pixel(f, r, 24, 2, "SECTOR 02    ·    OBS / HANDOFF", DIM);
    if r.width > 105 {
        let working = s.obs.crew.iter().filter(|a| a.state == "working").count();
        pixel(
            f,
            r,
            r.width - 27,
            0,
            if s.demo {
                "[ TRAINING FLIGHT ]"
            } else {
                "[ ACP / LIVE ]"
            },
            if s.demo { GOLD } else { GREEN },
        );
        pixel(
            f,
            r,
            r.width - 27,
            2,
            &format!(
                "T+{:02}:{:02}  {} IN FLIGHT",
                s.clock() / 60,
                s.clock() % 60,
                working
            ),
            WHITE,
        );
    }
    // Mission strip: what new seats nest on, and with which picker.
    let room = r.width.saturating_sub(24);
    let (text, color) = match &s.mission {
        Some(m) => (
            format!(
                "MISSION {} · {} │ PICKER {} │ {}",
                m.id,
                m.status,
                s.picker.as_str(),
                m.outcome
            ),
            GOLD,
        ),
        None => (
            format!(
                "MISSION none · /missions admits one │ PICKER {}",
                s.picker.as_str()
            ),
            DIM,
        ),
    };
    pixel(f, r, 24, 3, &one_line(&text, room), color);
    pixel(f, r, 0, 4, &"─".repeat(r.width as usize), DIM);
}
fn crew(f: &mut Frame, r: Rect, s: &Ship) {
    let panel = block("CREW MANIFEST", DIM);
    let inner = panel.inner(r);
    f.render_widget(panel, r);
    let mut lines = vec![
        Line::styled(" YOU / CAPTAIN", style(GOLD)),
        Line::styled(" Command authority", style(DIM)),
        Line::from(""),
    ];
    let visible = (inner.height.saturating_sub(3) / 5).max(1) as usize;
    let start = s.selected.saturating_sub(visible - 1);
    for (i, a) in s.obs.crew.iter().enumerate().skip(start).take(visible) {
        let selected = i == s.selected;
        lines.push(Line::from(vec![
            Span::styled(if selected { " ▸ " } else { "   " }, style(GOLD)),
            Span::styled(
                format!("L{} {}{}", a.rank, a.name, if a.unread { " *" } else { "" }),
                style(if selected { CYAN } else { WHITE }).add_modifier(if selected {
                    Modifier::BOLD
                } else {
                    Modifier::empty()
                }),
            ),
        ]));
        lines.push(Line::styled(
            format!(
                "   {:02}  {}  {}",
                a.pid,
                uke::glyph(&a.state),
                a.state.to_uppercase()
            ),
            style(status(&a.state)).add_modifier(
                if s.motion && a.pulse.elapsed().as_millis() < 1600 && s.tick % 4 < 2 {
                    Modifier::REVERSED
                } else {
                    Modifier::empty()
                },
            ),
        ));
        lines.push(Line::styled(
            format!(" {}", one_line(&a.mission, inner.width.saturating_sub(2))),
            style(DIM),
        ));
        lines.push(Line::styled(
            format!(
                " {} · {} · {}",
                a.harness,
                a.model.rsplit('/').next().unwrap_or(&a.model),
                a.effort
            ),
            style(DIM),
        ));
        lines.push(Line::from(""));
    }
    f.render_widget(Paragraph::new(lines), inner);
}
fn draw_matrix(f: &mut Frame, area: Rect, scroll: &mut u16, last_lines: &mut usize, text: &str) {
    let summary = text.lines().next().unwrap_or("HANDOFF MATRIX");
    f.render_widget(
        Paragraph::new(summary).style(style(GOLD)),
        Rect::new(area.x, area.y, area.width, 1),
    );
    let rows = text
        .lines()
        .filter_map(|line| {
            let cells = line
                .strip_prefix('|')?
                .strip_suffix('|')?
                .split('|')
                .map(str::trim)
                .collect::<Vec<_>>();
            (cells.len() == 4 && cells[0] != "From" && !cells[0].starts_with('-')).then(|| {
                [
                    cells[0].to_owned(),
                    cells[1].to_owned(),
                    cells[2].to_owned(),
                    one_line(cells[3], area.width.saturating_sub(34)),
                ]
            })
        })
        .collect::<Vec<_>>();
    let table_area = Rect::new(
        area.x,
        area.y + 1,
        area.width,
        area.height.saturating_sub(1),
    );
    let visible = table_area.height.saturating_sub(1) as usize;
    let max_scroll = rows.len().saturating_sub(visible).min(u16::MAX as usize) as u16;
    *scroll = (*scroll).min(max_scroll);
    *last_lines = rows.len();
    let active = rows
        .iter()
        .position(|row| matches!(row[2].as_str(), "requesting" | "delivered"))
        .or_else(|| rows.iter().rposition(|row| row[2] != "pending"))
        .unwrap_or(0);
    let offset = if *scroll == 0 {
        active.saturating_sub(visible.saturating_sub(1))
    } else {
        max_scroll.saturating_sub(*scroll) as usize
    };
    let table_rows = rows.iter().skip(offset).take(visible).map(|row| {
        Row::new(row.clone()).style(style(match row[2].as_str() {
            "PASS" => GREEN,
            "FAIL" => PINK,
            _ => WHITE,
        }))
    });
    f.render_widget(
        Table::new(
            table_rows,
            [
                Constraint::Length(8),
                Constraint::Length(8),
                Constraint::Length(10),
                Constraint::Min(8),
            ],
        )
        .header(Row::new(["FROM", "TO", "RESULT", "EVIDENCE"]).style(style(DIM)))
        .column_spacing(1),
        table_area,
    );
    if rows.len() > visible {
        let mut scrollbar = ScrollbarState::new(rows.len()).position(offset);
        f.render_stateful_widget(
            Scrollbar::new(ScrollbarOrientation::VerticalRight)
                .thumb_style(style(CYAN))
                .track_style(style(DIM)),
            table_area,
            &mut scrollbar,
        );
    }
}

fn input_display(input: &str, width: u16, height: u16) -> (String, u16, u16) {
    let width = width.saturating_sub(2).max(1) as usize;
    let mut lines = Vec::new();
    for line in input.split('\n') {
        if line.is_empty() {
            lines.push(String::new());
        } else {
            let mut chunk = String::new();
            for ch in line.chars() {
                let ch_width = Line::from(ch.to_string()).width();
                if !chunk.is_empty() && Line::from(chunk.as_str()).width() + ch_width > width {
                    lines.push(chunk);
                    chunk = String::new();
                }
                chunk.push(ch);
            }
            if !chunk.is_empty() {
                lines.push(chunk);
            }
        }
    }
    let cursor = lines.last().cloned().unwrap_or_default();
    let start = lines.len().saturating_sub(height as usize);
    let visible = lines[start..]
        .iter()
        .enumerate()
        .map(|(i, line)| format!("{}{}", if i == 0 { "> " } else { "  " }, line))
        .collect::<Vec<_>>()
        .join("\n");
    (
        visible,
        lines.len().saturating_sub(start + 1) as u16,
        Line::from(cursor).width() as u16,
    )
}

fn centered(area: Rect, width: u16, height: u16) -> Rect {
    let w = width.min(area.width);
    let h = height.min(area.height);
    Rect::new(
        area.x + (area.width - w) / 2,
        area.y + (area.height - h) / 2,
        w,
        h,
    )
}
fn palette_rect(area: Rect) -> Rect {
    centered(area, 74, 24.min(area.height.saturating_sub(2)))
}
fn palette_window(area: Rect, s: &Ship) -> (usize, usize) {
    let visible = (palette_rect(area).height.saturating_sub(8) / 2).max(1) as usize;
    let selected = s.palette.as_ref().map(|m| m.selected).unwrap_or(0);
    (selected.saturating_sub(visible - 1), visible)
}
pub fn hit_palette(area: Rect, s: &Ship, x: u16, y: u16) -> Option<usize> {
    s.palette.as_ref()?;
    if area.width < 65 || area.height < 23 {
        return None;
    }
    let r = palette_rect(area);
    let (start, visible) = palette_window(area, s);
    let top = r.y + 4;
    if x <= r.x || x >= r.right() - 1 || y < top || y >= top + visible as u16 * 2 {
        return None;
    }
    let index = start + ((y - top) / 2) as usize;
    (index < s.palette_items().len()).then_some(index)
}
pub fn hit_crew(area: Rect, s: &Ship, x: u16, y: u16) -> Option<usize> {
    if area.width < 65 || area.height < 23 {
        return None;
    }
    let width = if area.width > 100 { 27 } else { 21 };
    let inner_height = area.height.saturating_sub(14);
    let visible = (inner_height.saturating_sub(3) / 5).max(1) as usize;
    let start = s.selected.saturating_sub(visible - 1);
    if x <= 1 || x >= width || y < 10 {
        return None;
    }
    let row = ((y - 10) / 5) as usize;
    (row < visible && start + row < s.obs.crew.len()).then_some(start + row)
}

#[cfg(test)]
mod tests {
    use super::input_display;
    use crate::theme::one_line;

    #[test]
    fn multiline_input_keeps_the_latest_lines_and_cursor() {
        let (text, row, column) = input_display("one\ntwo\nthree", 12, 2);
        assert_eq!(text, "> two\n  three");
        assert_eq!((row, column), (1, 5));
    }

    #[test]
    fn multiline_input_wraps_by_terminal_cells() {
        let (text, row, column) = input_display("a😀bc", 7, 2);
        assert_eq!(text, "> a😀bc");
        assert_eq!((row, column), (0, 5));
    }

    #[test]
    fn one_line_never_overflows_a_narrow_unicode_cell() {
        assert_eq!(one_line("界界", 3), "界…");
        assert_eq!(one_line("界", 1), "…");
    }
}
