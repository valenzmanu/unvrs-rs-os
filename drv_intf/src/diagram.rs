//! Ratatui diagram components v0: agent diagram source becomes game-like nodes, links and bars.
//!
//! COMMS feeds this module the body of a fenced block tagged `flow`, `diagram` or `mermaid`.
//! Everything is drawn onto a fixed-width character canvas in the CRT palette, so a rendered
//! diagram never exceeds the width it was given and never panics on a tiny one.
//!
//! # Grammar v0 (teach this to the seats)
//!
//! ```text
//! graph LR            direction header; LR/RL lay left→right, TD/TB stack top→down (flowchart = graph)
//! A[Build]            node declaration: id in front, title in [brackets]
//! A[Build] :active    optional state, one of done | active | blocked | todo (colours the node)
//! A -> B -> C         edges and chains; A --> B and A -->|label| B (mermaid) mean the same
//! A -> B: deploy      edge label after a colon (a colon holding a state word is a state, not a label)
//! coverage: 7/10      progress bar row: label, colon, done/total
//! ```
//!
//! Lines starting with `#` and mermaid `%%` comments are ignored. Anything the grammar does not
//! cover is ignored too; a block that is mostly unknown renders as `None` so COMMS can show the
//! raw source. Graphs too tangled for a map (cycles, wide fan-out, many skip edges) degrade to a
//! tidy edge-list component rather than disappearing.
use crate::theme::*;
use ratatui::{
    style::Color,
    text::{Line, Span},
};

#[derive(Clone, Copy, PartialEq, Eq)]
enum NodeState {
    Done,
    Active,
    Blocked,
    Todo,
    Plain,
}

impl NodeState {
    fn parse(word: &str) -> Option<Self> {
        match word.trim().to_ascii_lowercase().as_str() {
            "done" | "ok" | "pass" => Some(Self::Done),
            "active" | "wip" | "running" => Some(Self::Active),
            "blocked" | "fail" | "failed" => Some(Self::Blocked),
            "todo" | "next" | "queued" => Some(Self::Todo),
            _ => None,
        }
    }
    fn glyph(self) -> char {
        match self {
            Self::Done => '█',
            Self::Active => '▓',
            Self::Blocked => '▚',
            Self::Todo => '░',
            Self::Plain => '▪',
        }
    }
    fn color(self) -> Color {
        match self {
            Self::Done => GREEN,
            Self::Active => CYAN,
            Self::Blocked => PINK,
            Self::Todo => DIM,
            Self::Plain => CYAN,
        }
    }
}

struct Node {
    id: String,
    title: String,
    state: NodeState,
}

struct Edge {
    from: usize,
    to: usize,
    label: Option<String>,
}

#[derive(Default)]
struct Diagram {
    nodes: Vec<Node>,
    edges: Vec<Edge>,
    bars: Vec<(String, usize, usize)>,
    stacked: bool,
}

impl Diagram {
    fn ensure(&mut self, id: &str, title: Option<String>, state: Option<NodeState>) -> usize {
        let index = match self.nodes.iter().position(|node| node.id == id) {
            Some(index) => index,
            None => {
                self.nodes.push(Node {
                    id: id.to_owned(),
                    title: id.to_owned(),
                    state: NodeState::Plain,
                });
                self.nodes.len() - 1
            }
        };
        if let Some(title) = title.filter(|title| !title.trim().is_empty()) {
            self.nodes[index].title = title;
        }
        if let Some(state) = state {
            self.nodes[index].state = state;
        }
        index
    }
}

/// Display width of `text` in terminal cells.
fn cells(text: &str) -> usize {
    Line::from(text.to_owned()).width()
}

/// A fixed-size character grid. Every write is bounds checked, so nothing can overflow `width`.
struct Canvas {
    width: usize,
    rows: Vec<Vec<(char, Color)>>,
}

impl Canvas {
    fn new(width: usize, height: usize) -> Self {
        Self {
            width,
            rows: vec![vec![(' ', DIM); width]; height],
        }
    }
    fn put(&mut self, x: usize, y: usize, ch: char, color: Color) {
        if x < self.width && y < self.rows.len() {
            // One grid cell must stay one terminal cell or the canvas width would drift.
            let ch = if cells(&ch.to_string()) == 1 {
                ch
            } else {
                '·'
            };
            self.rows[y][x] = (ch, color);
        }
    }
    /// Draws a connector, fusing it with whatever already crosses that cell so fan-outs
    /// read as `┬` / `┴` instead of the last corner drawn winning.
    fn join(&mut self, x: usize, y: usize, ch: char, color: Color) {
        const BOX: [(char, u8); 11] = [
            ('─', 0b0011),
            ('│', 0b1100),
            ('╭', 0b0110),
            ('╮', 0b0101),
            ('╰', 0b1010),
            ('╯', 0b1001),
            ('┬', 0b0111),
            ('┴', 0b1011),
            ('├', 0b1110),
            ('┤', 0b1101),
            ('┼', 0b1111),
        ];
        let mask = |ch: char| {
            BOX.iter()
                .find(|(key, _)| *key == ch)
                .map(|(_, bits)| *bits)
        };
        let fused = match (
            self.rows
                .get(y)
                .and_then(|row| row.get(x))
                .map(|cell| cell.0),
            mask(ch),
        ) {
            (Some(old), Some(bits)) => mask(old)
                .map(|was| was | bits)
                .and_then(|bits| BOX.iter().find(|(_, key)| *key == bits))
                .map(|(ch, _)| *ch)
                .unwrap_or(ch),
            _ => ch,
        };
        self.put(x, y, fused, color);
    }
    /// True when every cell of `x..x + len` on row `y` is blank or plain horizontal rule.
    fn clear_run(&self, x: usize, y: usize, len: usize) -> bool {
        self.rows.get(y).is_some_and(|row| {
            x + len <= self.width
                && row[x..x + len]
                    .iter()
                    .all(|(ch, _)| *ch == ' ' || *ch == '─')
        })
    }
    fn text(&mut self, x: usize, y: usize, text: &str, color: Color) {
        for (offset, ch) in text.chars().enumerate() {
            self.put(x + offset, y, ch, color);
        }
    }
    fn lines(&self) -> Vec<Line<'static>> {
        self.rows
            .iter()
            .map(|row| {
                let mut end = row.len();
                while end > 0 && row[end - 1].0 == ' ' {
                    end -= 1;
                }
                let mut spans: Vec<Span<'static>> = Vec::new();
                let mut run = String::new();
                let mut color = WHITE;
                for &(ch, cell) in &row[..end] {
                    if !run.is_empty() && cell != color {
                        spans.push(Span::styled(std::mem::take(&mut run), style(color)));
                    }
                    color = cell;
                    run.push(ch);
                }
                if !run.is_empty() {
                    spans.push(Span::styled(run, style(color)));
                }
                Line::from(spans)
            })
            .collect()
    }
}

/// Splits `line` on `->` / `-->` arrows into node tokens plus the `|label|` sitting on each arrow.
fn split_arrows(line: &str) -> Option<(Vec<String>, Vec<Option<String>>)> {
    let chars = line.chars().collect::<Vec<_>>();
    let mut tokens = Vec::new();
    let mut labels = Vec::new();
    let mut current = String::new();
    let mut index = 0;
    while index < chars.len() {
        let arrow = if chars[index] == '-'
            && chars.get(index + 1) == Some(&'-')
            && chars.get(index + 2) == Some(&'>')
        {
            Some(3)
        } else if chars[index] == '-' && chars.get(index + 1) == Some(&'>') {
            Some(2)
        } else {
            None
        };
        let Some(arrow) = arrow else {
            current.push(chars[index]);
            index += 1;
            continue;
        };
        tokens.push(std::mem::take(&mut current).trim().to_owned());
        index += arrow;
        while chars.get(index) == Some(&' ') {
            index += 1;
        }
        let mut label = None;
        if chars.get(index) == Some(&'|')
            && let Some(offset) = chars[index + 1..].iter().position(|ch| *ch == '|')
        {
            label = Some(chars[index + 1..index + 1 + offset].iter().collect());
            index += offset + 2;
        }
        labels.push(label);
    }
    (!labels.is_empty()).then(|| {
        tokens.push(current.trim().to_owned());
        (tokens, labels)
    })
}

/// Parses `id[Title]: suffix` into its id, title, state and trailing edge label.
fn parse_token(token: &str) -> (String, Option<String>, Option<NodeState>, Option<String>) {
    let token = token.trim();
    let (head, tail) = match (token.find('['), token.find(']')) {
        (Some(open), Some(close)) if close > open => (
            token[..open].to_owned(),
            format!("{}|{}", &token[open + 1..close], &token[close + 1..]),
        ),
        _ => (token.to_owned(), String::new()),
    };
    let (title, rest) = match tail.split_once('|') {
        Some((title, rest)) => (Some(title.trim().to_owned()), rest.to_owned()),
        None => (None, token.to_owned()),
    };
    let (id, suffix) = if title.is_some() {
        (head.trim().to_owned(), rest)
    } else {
        match rest.split_once(':') {
            Some((id, suffix)) => (id.trim().to_owned(), suffix.to_owned()),
            None => (rest.trim().to_owned(), String::new()),
        }
    };
    let suffix = suffix.trim().trim_start_matches(':').trim();
    let (state, label) = match NodeState::parse(suffix) {
        _ if suffix.is_empty() => (None, None),
        Some(state) => (Some(state), None),
        None => (None, Some(suffix.to_owned())),
    };
    (id, title, state, label)
}

fn parse_bar(line: &str) -> Option<(String, usize, usize)> {
    let (label, rest) = line.rsplit_once(':')?;
    let (done, total) = rest.trim().split_once('/')?;
    let done = done.trim().parse::<usize>().ok()?;
    let total = total.trim().parse::<usize>().ok()?;
    let label = label.trim();
    (total > 0 && !label.is_empty()).then(|| (label.to_owned(), done.min(total), total))
}

fn parse(source: &str) -> Option<Diagram> {
    let mut diagram = Diagram::default();
    let (mut known, mut junk) = (0usize, 0usize);
    for raw in source.lines() {
        let line = raw.split("%%").next().unwrap_or("").trim();
        if line.is_empty() || line.starts_with('#') || line.starts_with("//") {
            continue;
        }
        let lower = line.to_ascii_lowercase();
        if lower.starts_with("graph") || lower.starts_with("flowchart") {
            diagram.stacked = lower.contains(" td") || lower.contains(" tb");
            known += 1;
            continue;
        }
        if let Some((tokens, labels)) = split_arrows(line) {
            let parsed = tokens
                .iter()
                .map(|token| parse_token(token))
                .collect::<Vec<_>>();
            if parsed.iter().any(|(id, ..)| id.is_empty()) {
                junk += 1;
                continue;
            }
            let mut previous = None;
            for (index, (id, title, state, label)) in parsed.into_iter().enumerate() {
                let node = diagram.ensure(&id, title, state);
                if let Some(from) = previous {
                    let label = labels
                        .get(index - 1)
                        .cloned()
                        .flatten()
                        .or(label)
                        .map(|label| label.trim().to_owned())
                        .filter(|label| !label.is_empty());
                    diagram.edges.push(Edge {
                        from,
                        to: node,
                        label,
                    });
                }
                previous = Some(node);
            }
            known += 1;
            continue;
        }
        if let Some(bar) = parse_bar(line) {
            diagram.bars.push(bar);
            known += 1;
            continue;
        }
        let (id, title, state, _) = parse_token(line);
        if !id.is_empty() && (title.is_some() || state.is_some()) {
            diagram.ensure(&id, title, state);
            known += 1;
            continue;
        }
        junk += 1;
    }
    (known > 0 && junk <= known).then_some(diagram)
}

fn box_width(node: &Node) -> usize {
    cells(&node.title) + 6
}

fn draw_box(canvas: &mut Canvas, x: usize, y: usize, width: usize, node: &Node) {
    if width < 7 {
        return;
    }
    let color = node.state.color();
    canvas.put(x, y, '╭', color);
    canvas.put(x + width - 1, y, '╮', color);
    canvas.put(x, y + 2, '╰', color);
    canvas.put(x + width - 1, y + 2, '╯', color);
    for offset in 1..width - 1 {
        canvas.put(x + offset, y, '─', color);
        canvas.put(x + offset, y + 2, '─', color);
        canvas.put(x + offset, y + 1, ' ', WHITE);
    }
    canvas.put(x, y + 1, '│', color);
    canvas.put(x + width - 1, y + 1, '│', color);
    canvas.put(x + 2, y + 1, node.state.glyph(), color);
    let title = one_line(&node.title, (width - 6) as u16);
    canvas.text(x + 4, y + 1, &title, WHITE);
}

fn legend_line(diagram: &Diagram, edge: &Edge, width: u16) -> Line<'static> {
    let label = edge
        .label
        .as_deref()
        .map(|label| format!(" · {label}"))
        .unwrap_or_default();
    Line::styled(
        one_line(
            &format!(
                "  ↳ {} ─▶ {}{label}",
                diagram.nodes[edge.from].title, diagram.nodes[edge.to].title
            ),
            width,
        ),
        style(DIM),
    )
}

/// The honest fallback: a tidy list of links when the graph is too tangled to map.
fn edge_list(diagram: &Diagram, width: u16) -> Vec<Line<'static>> {
    let mut out = vec![Line::styled(
        one_line(
            &format!(
                "▚▞ MAP / {} nodes · {} links",
                diagram.nodes.len(),
                diagram.edges.len()
            ),
            width,
        ),
        style(CYAN),
    )];
    for edge in &diagram.edges {
        let (from, to) = (&diagram.nodes[edge.from], &diagram.nodes[edge.to]);
        let label = edge
            .label
            .as_deref()
            .map(|label| format!(" · {label}"))
            .unwrap_or_default();
        out.push(Line::styled(
            one_line(
                &format!(
                    " {} {} ─▶ {} {}{label}",
                    from.state.glyph(),
                    from.title,
                    to.state.glyph(),
                    to.title
                ),
                width,
            ),
            style(WHITE),
        ));
    }
    for node in diagram
        .nodes
        .iter()
        .enumerate()
        .filter(|(index, _)| {
            !diagram
                .edges
                .iter()
                .any(|edge| edge.from == *index || edge.to == *index)
        })
        .map(|(_, node)| node)
    {
        out.push(Line::styled(
            one_line(&format!(" {} {}", node.state.glyph(), node.title), width),
            style(node.state.color()),
        ));
    }
    out
}

/// Nodes with no links: a wrapping grid of cards.
fn card_grid(diagram: &Diagram, width: usize) -> Vec<Line<'static>> {
    let mut rows: Vec<Vec<usize>> = vec![Vec::new()];
    let mut used = 0usize;
    for (index, node) in diagram.nodes.iter().enumerate() {
        let box_width = box_width(node).min(width);
        let gap = usize::from(!rows.last().map(Vec::is_empty).unwrap_or(true)) * 2;
        if used + gap + box_width > width && !rows.last().map(Vec::is_empty).unwrap_or(true) {
            rows.push(Vec::new());
            used = 0;
        }
        used += usize::from(used > 0) * 2 + box_width;
        rows.last_mut().expect("row").push(index);
    }
    let mut canvas = Canvas::new(width, rows.len() * 4 - 1);
    for (row, indices) in rows.iter().enumerate() {
        let mut x = 0;
        for index in indices {
            let node = &diagram.nodes[*index];
            let box_width = box_width(node).min(width);
            draw_box(&mut canvas, x, row * 4, box_width, node);
            x += box_width + 2;
        }
    }
    canvas.lines()
}

struct Layout {
    layer: Vec<usize>,
    position: Vec<usize>,
    columns: Vec<Vec<usize>>,
}

fn layout(diagram: &Diagram) -> Layout {
    let count = diagram.nodes.len();
    let mut layer = vec![0usize; count];
    for _ in 0..count.min(32) {
        let mut changed = false;
        for edge in &diagram.edges {
            if edge.from != edge.to
                && layer[edge.to] < layer[edge.from] + 1
                && layer[edge.from] + 1 < count
            {
                layer[edge.to] = layer[edge.from] + 1;
                changed = true;
            }
        }
        if !changed {
            break;
        }
    }
    let depth = layer.iter().copied().max().unwrap_or(0) + 1;
    let mut columns = vec![Vec::new(); depth];
    let mut position = vec![0usize; count];
    for (index, level) in layer.iter().enumerate() {
        position[index] = columns[*level].len();
        columns[*level].push(index);
    }
    Layout {
        layer,
        position,
        columns,
    }
}

fn draw_side_by_side(diagram: &Diagram, plan: &Layout, width: usize) -> Option<Vec<Line<'static>>> {
    let widths = diagram.nodes.iter().map(box_width).collect::<Vec<_>>();
    let columns = plan
        .columns
        .iter()
        .map(|column| column.iter().map(|index| widths[*index]).max().unwrap_or(0))
        .collect::<Vec<_>>();
    let mut gaps = (0..plan.columns.len().saturating_sub(1))
        .map(|boundary| {
            diagram
                .edges
                .iter()
                .filter(|edge| {
                    plan.layer[edge.from] == boundary && plan.layer[edge.to] == boundary + 1
                })
                .filter_map(|edge| edge.label.as_deref().map(cells))
                .max()
                // A label sits on the far side of the bend column: 2·label + 8 buys it room.
                .map(|label| label * 2 + 8)
                .unwrap_or(5)
                .max(5)
        })
        .collect::<Vec<_>>();
    let total = |gaps: &[usize]| columns.iter().sum::<usize>() + gaps.iter().sum::<usize>();
    if total(&gaps) > width {
        gaps = gaps.iter().map(|_| 5).collect();
    }
    if total(&gaps) > width {
        return None;
    }
    let mut x = Vec::with_capacity(columns.len());
    let mut cursor = 0;
    for (index, column) in columns.iter().enumerate() {
        x.push(cursor);
        cursor += column + gaps.get(index).copied().unwrap_or(0);
    }
    let height = plan
        .columns
        .iter()
        .map(|column| (column.len() * 4).saturating_sub(1))
        .max()
        .unwrap_or(1);
    let mut canvas = Canvas::new(width, height);
    for (level, column) in plan.columns.iter().enumerate() {
        for (row, index) in column.iter().enumerate() {
            draw_box(
                &mut canvas,
                x[level],
                row * 4,
                columns[level],
                &diagram.nodes[*index],
            );
        }
    }
    let mut legend = Vec::new();
    let mut labels = Vec::new();
    for edge in &diagram.edges {
        let level = plan.layer[edge.from];
        if plan.layer[edge.to] != level + 1 {
            legend.push(edge);
            continue;
        }
        let (top, bottom) = (
            plan.position[edge.from] * 4 + 1,
            plan.position[edge.to] * 4 + 1,
        );
        let start = x[level] + columns[level];
        let arrow = start + gaps[level] - 1;
        let bus = start + gaps[level] / 2;
        if top == bottom {
            for cell in start..arrow {
                canvas.join(cell, top, '─', DIM);
            }
        } else {
            for cell in start..bus {
                canvas.join(cell, top, '─', DIM);
            }
            for cell in bus + 1..arrow {
                canvas.join(cell, bottom, '─', DIM);
            }
            for cell in top.min(bottom) + 1..top.max(bottom) {
                canvas.join(bus, cell, '│', DIM);
            }
            canvas.join(bus, top, if bottom > top { '╮' } else { '╯' }, DIM);
            canvas.join(bus, bottom, if bottom > top { '╰' } else { '╭' }, DIM);
        }
        canvas.put(arrow, bottom, '▶', CYAN);
        if edge.label.is_some() {
            labels.push((edge, bottom, bus + 1, arrow));
        }
    }
    // Labels go on last: a bend drawn later must not eat the text of an earlier edge.
    for (edge, row, from, to) in labels {
        let label = edge.label.as_deref().unwrap_or_default();
        let room = to.saturating_sub(from);
        let at = from + room.saturating_sub(cells(label)) / 2;
        if room >= cells(label) + 2 && canvas.clear_run(at, row, cells(label)) {
            canvas.text(at, row, label, GOLD);
        } else {
            legend.push(edge);
        }
    }
    let mut out = canvas.lines();
    out.extend(
        legend
            .into_iter()
            .map(|edge| legend_line(diagram, edge, width as u16)),
    );
    Some(out)
}

fn draw_stacked(diagram: &Diagram, plan: &Layout, width: usize) -> Option<Vec<Line<'static>>> {
    let widths = diagram.nodes.iter().map(box_width).collect::<Vec<_>>();
    let spans = plan
        .columns
        .iter()
        .map(|column| {
            column.iter().map(|index| widths[*index]).sum::<usize>()
                + column.len().saturating_sub(1) * 2
        })
        .collect::<Vec<_>>();
    if spans.iter().copied().max().unwrap_or(0) > width {
        return None;
    }
    let mut x = vec![0usize; diagram.nodes.len()];
    for (level, column) in plan.columns.iter().enumerate() {
        let mut cursor = (width - spans[level]) / 2;
        for index in column {
            x[*index] = cursor;
            cursor += widths[*index] + 2;
        }
    }
    let mut canvas = Canvas::new(width, plan.columns.len() * 6 - 3);
    for (level, column) in plan.columns.iter().enumerate() {
        for index in column {
            draw_box(
                &mut canvas,
                x[*index],
                level * 6,
                widths[*index],
                &diagram.nodes[*index],
            );
        }
    }
    let mut legend = Vec::new();
    let mut labels = Vec::new();
    for edge in &diagram.edges {
        let level = plan.layer[edge.from];
        if plan.layer[edge.to] != level + 1 {
            legend.push(edge);
            continue;
        }
        let from = x[edge.from] + widths[edge.from] / 2;
        let to = x[edge.to] + widths[edge.to] / 2;
        let top = level * 6 + 3;
        canvas.join(from, top, '│', DIM);
        if from == to {
            canvas.join(from, top + 1, '│', DIM);
        } else {
            for cell in from.min(to) + 1..from.max(to) {
                canvas.join(cell, top + 1, '─', DIM);
            }
            canvas.join(from, top + 1, if to > from { '╰' } else { '╯' }, DIM);
            canvas.join(to, top + 1, if to > from { '╮' } else { '╭' }, DIM);
        }
        canvas.put(to, top + 2, '▼', CYAN);
        if edge.label.is_some() {
            labels.push((edge, top + 1, from.min(to) + 1, from.max(to)));
        }
    }
    for (edge, row, from, to) in labels {
        let label = edge.label.as_deref().unwrap_or_default();
        let room = to.saturating_sub(from);
        let at = from + room.saturating_sub(cells(label)) / 2;
        if room >= cells(label) + 2 && canvas.clear_run(at, row, cells(label)) {
            canvas.text(at, row, label, GOLD);
        } else {
            legend.push(edge);
        }
    }
    let mut out = canvas.lines();
    out.extend(
        legend
            .into_iter()
            .map(|edge| legend_line(diagram, edge, width as u16)),
    );
    Some(out)
}

fn graph_lines(diagram: &Diagram, width: usize) -> Vec<Line<'static>> {
    if width < 16 {
        return edge_list(diagram, width as u16);
    }
    if diagram.edges.is_empty() {
        return card_grid(diagram, width);
    }
    let plan = layout(diagram);
    let skips = diagram
        .edges
        .iter()
        .filter(|edge| plan.layer[edge.to] != plan.layer[edge.from] + 1)
        .count();
    let tangled = diagram.nodes.len() > 14
        || plan.columns.len() > 6
        || plan.columns.iter().map(Vec::len).max().unwrap_or(0) > 5
        || (diagram.edges.len() > 2 && skips * 2 > diagram.edges.len());
    if tangled {
        return edge_list(diagram, width as u16);
    }
    let first = if diagram.stacked {
        draw_stacked(diagram, &plan, width)
    } else {
        draw_side_by_side(diagram, &plan, width)
    };
    first
        .or_else(|| {
            if diagram.stacked {
                draw_side_by_side(diagram, &plan, width)
            } else {
                draw_stacked(diagram, &plan, width)
            }
        })
        .unwrap_or_else(|| edge_list(diagram, width as u16))
}

fn bar_lines(bars: &[(String, usize, usize)], width: u16) -> Vec<Line<'static>> {
    let longest = bars
        .iter()
        .map(|(label, ..)| cells(label))
        .max()
        .unwrap_or(0)
        .min(width as usize / 3);
    bars.iter()
        .map(|(label, done, total)| {
            let label = one_line(label, longest as u16);
            let ratio = format!("{done}/{total}");
            let frame = longest + cells(&ratio) + 2;
            if frame > width as usize {
                // Too narrow for a track: keep the reading, drop the bar.
                return Line::styled(one_line(&format!("{label} {ratio}"), width), style(DIM));
            }
            let track = (width as usize - frame).min(24);
            let filled = if track == 0 { 0 } else { track * done / total };
            Line::from(vec![
                Span::styled(
                    format!(
                        "{label}{} ",
                        " ".repeat(longest.saturating_sub(cells(&label)))
                    ),
                    style(WHITE),
                ),
                Span::styled(
                    "█".repeat(filled),
                    style(if done == total { GREEN } else { CYAN }),
                ),
                Span::styled("░".repeat(track - filled), style(DIM)),
                Span::styled(format!(" {ratio}"), style(GOLD)),
            ])
        })
        .collect()
}

/// Renders diagram `source` (the body of a fenced diagram block) within `width` cells.
/// Returns None when the source is not a diagram this module understands.
pub fn render(source: &str, width: u16) -> Option<Vec<Line<'static>>> {
    let diagram = parse(source)?;
    let mut out = Vec::new();
    if !diagram.nodes.is_empty() {
        out.extend(graph_lines(&diagram, width as usize));
    }
    if !diagram.bars.is_empty() {
        if !out.is_empty() {
            out.push(Line::from(""));
        }
        out.extend(bar_lines(&diagram.bars, width));
    }
    (!out.is_empty()).then_some(out)
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

    #[test]
    fn fan_out_joins_connectors_instead_of_overwriting_them() {
        let lines = render(
            "graph TD\nA[Plan] -> B[Build]\nA -> C[Test]\nB -> D[Ship]\nC -> D",
            72,
        )
        .unwrap();
        let joined = text(&lines).join("\n");
        assert!(joined.contains('┴') && joined.contains('┬'));
    }

    #[test]
    fn a_crossing_bend_never_eats_a_label() {
        let lines = render(
            "graph LR\nA[Plan] -->|review| B[Build]\nA -> C[Test]\nC -> D[Ship]",
            72,
        )
        .unwrap();
        assert!(text(&lines).join("\n").contains("review"));
    }

    #[test]
    fn prose_is_not_a_diagram() {
        assert!(render("just some notes\nand another sentence here", 80).is_none());
    }

    #[test]
    fn chain_draws_titles_and_arrows_without_raw_source() {
        let lines = render(
            "graph LR\nA[Fetch]\nB[Build] :active\nA -> B -> C[Ship]",
            80,
        )
        .unwrap();
        let joined = text(&lines).join("\n");
        assert!(joined.contains("Fetch") && joined.contains("Build") && joined.contains("Ship"));
        assert!(!joined.contains("->"));
        assert!(joined.contains('▶'));
    }

    #[test]
    fn mermaid_labels_survive_somewhere() {
        let lines = render("flowchart LR\nA[Plan] -->|review| B[Ship]", 80).unwrap();
        assert!(text(&lines).join("\n").contains("review"));
    }

    #[test]
    fn colon_label_is_not_mistaken_for_state() {
        let lines = render("A[Plan] -> B[Ship]: deploy", 80).unwrap();
        assert!(text(&lines).join("\n").contains("deploy"));
    }

    #[test]
    fn bars_render_as_progress() {
        let lines = render("coverage: 7/10\nseats: 3/3", 60).unwrap();
        let joined = text(&lines).join("\n");
        assert!(joined.contains("coverage") && joined.contains("7/10") && joined.contains('█'));
    }

    #[test]
    fn tangled_graphs_degrade_to_an_edge_list() {
        let source = (0..9)
            .map(|index| format!("N{index} -> N{}", (index + 1) % 9))
            .collect::<Vec<_>>()
            .join("\n");
        let lines = render(&source, 80).unwrap();
        assert!(text(&lines)[0].contains("MAP /"));
    }

    #[test]
    fn every_line_fits_the_width_it_was_given() {
        let source = "graph LR\nA[Fetch the world] -->|a rather long label| B[Build]\nA -> C[Test]\nC -> D[Ship it now]\ncoverage: 7/10";
        for width in [1u16, 2, 3, 8, 13, 20, 33, 58, 120] {
            for lines in [render(source, width), render("A[One] -> B[Two]", width)]
                .into_iter()
                .flatten()
            {
                for line in &lines {
                    assert!(line.width() <= width as usize, "width {width} overflowed");
                }
            }
        }
    }

    #[test]
    fn tiny_widths_never_panic() {
        for width in 0..12u16 {
            let _ = render("graph TD\nA[Alpha] -> B[Beta]\nbar: 1/2", width);
        }
    }

    #[test]
    fn stacked_direction_is_honoured() {
        let lines = render("graph TD\nA[Alpha] -> B[Beta]", 80).unwrap();
        assert!(text(&lines).join("\n").contains('▼'));
    }
}
