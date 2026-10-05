//! Shared CRT palette and pixel helpers. One visual language for every panel.
use ratatui::{
    Frame,
    layout::Rect,
    style::{Color, Style},
    text::Line,
    widgets::{Block, BorderType, Borders, Paragraph},
};

pub const BG: Color = Color::Rgb(8, 12, 28);
pub const PANEL: Color = Color::Rgb(12, 19, 38);
pub const CYAN: Color = Color::Rgb(99, 235, 233);
pub const DIM: Color = Color::Rgb(89, 113, 143);
pub const WHITE: Color = Color::Rgb(218, 232, 241);
pub const GOLD: Color = Color::Rgb(255, 207, 107);
pub const PINK: Color = Color::Rgb(244, 119, 172);
pub const GREEN: Color = Color::Rgb(130, 226, 168);
pub fn style(c: Color) -> Style {
    Style::default().fg(c)
}
pub fn block(title: impl Into<String>, color: Color) -> Block<'static> {
    Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::QuadrantOutside)
        .border_style(style(color))
        .title(format!(" {} ", title.into()))
        .style(Style::default().bg(PANEL).fg(WHITE))
}
pub fn status(s: &str) -> Color {
    match s {
        "working" => CYAN,
        "idle" => GREEN,
        "blocked" => GOLD,
        "offline" => PINK,
        _ => DIM,
    }
}
pub fn pixel(f: &mut Frame, r: Rect, x: u16, y: u16, s: &str, c: Color) {
    if x < r.width && y < r.height {
        let width = (r.width - x).min(s.chars().count() as u16);
        f.render_widget(
            Paragraph::new(s).style(style(c)),
            Rect::new(r.x + x, r.y + y, width, 1),
        );
    }
}

pub fn one_line(text: &str, width: u16) -> String {
    let text = text.split_whitespace().collect::<Vec<_>>().join(" ");
    let mut output = String::new();
    let mut truncated = false;
    for ch in text.chars() {
        if Line::from(output.as_str()).width() + Line::from(ch.to_string()).width() > width as usize
        {
            truncated = true;
            break;
        }
        output.push(ch);
    }
    if truncated && width > 0 {
        while Line::from(output.as_str()).width() + 1 > width as usize {
            output.pop();
        }
        output.push('…');
    }
    output
}
