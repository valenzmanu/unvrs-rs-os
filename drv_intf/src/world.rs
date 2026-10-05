//! A small, self-contained pixel world behind the bridge and its boarding scenes.
use crate::theme::{BG, CYAN, DIM, GOLD, GREEN, PANEL, PINK, WHITE};
use ratatui::{Frame, layout::Rect, style::Color};

const TITLES: [&str; 4] = [
    "THE FLIGHT IS A UNIVERSE",
    "CAPTAIN HOLDS CONTEXT",
    "TOOLS ABOARD",
    "LANDING: WORKING COCKPIT",
];

/// Draw the quiet overworld or one of the four short onboarding scenes.
pub fn draw(f: &mut Frame, area: Rect, tick: u64, scene: Option<usize>) {
    if area.width == 0 || area.height == 0 {
        return;
    }
    fill(f, area, BG);
    let sky = Rect::new(area.x, area.y, area.width, area.height.saturating_sub(1));
    stars(f, sky, tick);
    if scene.is_none() {
        resting_world(f, sky, tick);
        return;
    }

    let scene = scene.unwrap_or_default().min(TITLES.len() - 1);
    title(f, area, TITLES[scene]);
    match scene {
        0 => universe(f, sky, tick),
        1 => captain(f, sky, tick),
        2 => tools(f, sky, tick),
        _ => landing(f, sky, tick),
    }
    hint(f, area, "Enter: next  /  Esc: return");
}

fn fill(f: &mut Frame, r: Rect, color: Color) {
    let b = f.buffer_mut();
    for y in r.y..r.y.saturating_add(r.height) {
        for x in r.x..r.x.saturating_add(r.width) {
            b[(x, y)].set_symbol(" ").set_bg(color).set_fg(color);
        }
    }
}

fn text(f: &mut Frame, r: Rect, x: u16, y: u16, s: &str, color: Color) {
    if y >= r.height || x >= r.width {
        return;
    }
    let b = f.buffer_mut();
    for (i, ch) in s.chars().take((r.width - x) as usize).enumerate() {
        b[(r.x + x + i as u16, r.y + y)]
            .set_symbol(&ch.to_string())
            .set_fg(color)
            .set_bg(BG);
    }
}

/// Logical pixels are packed into a terminal cell with the Unicode upper half block.
fn px(f: &mut Frame, r: Rect, x: i32, y: i32, color: Color) {
    if x < 0 || y < 0 || x >= r.width as i32 || y >= r.height as i32 * 2 {
        return;
    }
    let cell = &mut f.buffer_mut()[(r.x + x as u16, r.y + y as u16 / 2)];
    cell.set_symbol("▀");
    if y % 2 == 0 {
        cell.set_fg(color);
    } else {
        cell.set_bg(color);
    }
}

fn stars(f: &mut Frame, r: Rect, tick: u64) {
    let count = (r.width as u32 * r.height as u32 / 5).clamp(8, 90);
    for i in 0..count {
        let seed = hash(i);
        let layer = seed % 3;
        let x = (seed >> 8) % r.width.max(1) as u32;
        let y = ((seed >> 16) + tick as u32 * (layer + 1)) % (r.height.max(1) as u32 * 2);
        px(f, r, x as i32, y as i32, [DIM, CYAN, WHITE][layer as usize]);
    }
}

fn resting_world(f: &mut Frame, r: Rect, tick: u64) {
    if r.height < 12 {
        return;
    }
    // The bridge overlays its notes above and its input below this band.
    let floor = (r.height as i32 * 2 - 22).max(r.height as i32);
    let cx = r.width as i32 * 3 / 4;
    planet(
        f,
        r,
        cx,
        floor - 18,
        (r.width as i32 / 4).clamp(12, 34),
        15,
        tick,
        [PANEL, DIM, GREEN],
    );
    for x in 0..r.width as i32 {
        let far = 4 + (hash(x as u32 / 3 + tick as u32 / 12) % 7) as i32;
        let near = 3 + (hash(x as u32 + tick as u32 / 5) % 9) as i32;
        for y in 0..far {
            px(f, r, x, floor - 8 - y, PANEL);
        }
        for y in 0..near {
            px(f, r, x, floor - y, if y < 2 { GREEN } else { DIM });
        }
    }
    sprite(
        f,
        r,
        r.width as i32 / 5 - 12,
        floor - 17 + (tick as i32 / 10 % 2),
        &[
            "             #             ",
            "            ###            ",
            "          ###+###          ",
            "        #####+#####        ",
            "     ###..#######..###     ",
            "   ###....###+###....###   ",
            " ##......###. .###......## ",
            "     #       #       #     ",
        ],
        [CYAN, WHITE, DIM],
    );
}

fn title(f: &mut Frame, r: Rect, s: &str) {
    if r.height < 2 || r.width < 8 {
        return;
    }
    let n = s.len().min(r.width as usize);
    text(f, r, (r.width - n as u16) / 2, 0, &s[..n], CYAN);
}

fn hint(f: &mut Frame, r: Rect, s: &str) {
    if r.height < 2 || r.width < 12 {
        return;
    }
    let n = s.len().min(r.width as usize);
    text(f, r, (r.width - n as u16) / 2, r.height - 1, &s[..n], DIM);
}

fn sprite(f: &mut Frame, r: Rect, x: i32, y: i32, art: &[&str], shades: [Color; 3]) {
    for (dy, row) in art.iter().enumerate() {
        for (dx, ch) in row.chars().enumerate() {
            let color = match ch {
                '#' => shades[0],
                '+' => shades[1],
                '.' => shades[2],
                _ => continue,
            };
            px(f, r, x + dx as i32, y + dy as i32, color);
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn planet(
    f: &mut Frame,
    r: Rect,
    cx: i32,
    cy: i32,
    rx: i32,
    ry: i32,
    tick: u64,
    shades: [Color; 3],
) {
    for y in -ry..=ry {
        for x in -rx..=rx {
            if x * x * ry * ry + y * y * rx * rx > rx * rx * ry * ry {
                continue;
            }
            let ocean = y < -ry / 4 || (x + tick as i32 / 3).rem_euclid(9) < 2;
            let color = if ocean {
                shades[0]
            } else if y < 0 {
                shades[1]
            } else {
                shades[2]
            };
            px(f, r, cx + x, cy + y, color);
        }
    }
}

fn landscape(f: &mut Frame, r: Rect, tick: u64) {
    let floor = r.height as i32 * 2 - 2;
    for x in 0..r.width as i32 {
        let ridge = 3 + (hash(x as u32 + tick as u32 / 4) % 7) as i32;
        for y in 0..ridge {
            px(f, r, x, floor - y, if y < 2 { GREEN } else { PANEL });
        }
    }
}

fn universe(f: &mut Frame, r: Rect, tick: u64) {
    let cx = r.width as i32 * 2 / 3;
    let cy = r.height as i32 + 5;
    planet(
        f,
        r,
        cx,
        cy,
        (r.width as i32 / 3).clamp(12, 36),
        15,
        tick,
        [DIM, GREEN, GOLD],
    );
    landscape(f, r, tick);
    sprite(
        f,
        r,
        r.width as i32 / 5 - 13,
        r.height as i32 * 2 - 20,
        &[
            "             #             ",
            "            ###            ",
            "          ###+###          ",
            "        #####+#####        ",
            "     ###..#######..###     ",
            "   ###....###+###....###   ",
            " ##......###. .###......## ",
            "     #       #       #     ",
        ],
        [CYAN, WHITE, DIM],
    );
    if r.width >= 32 && r.height >= 8 {
        text(f, r, 2, 2, "THIS FLIGHT IS A UNIVERSE", WHITE);
    }
}

fn captain(f: &mut Frame, r: Rect, tick: u64) {
    landscape(f, r, tick);
    let x = r.width as i32 / 2 - 11;
    let y = r.height as i32 * 2 - 25 + (tick as i32 / 8 % 2);
    sprite(
        f,
        r,
        x,
        y,
        &[
            "         +++++         ",
            "       +++###+++       ",
            "      ++##.###++       ",
            "     +##.....###+      ",
            "     ###..#..###.      ",
            "     #######.####      ",
            "    ###+++++++###      ",
            "   ###..#####..###     ",
            "  ##....##.##....##    ",
            " ##.....#   #.....##   ",
            "     ###     ###       ",
        ],
        [GOLD, WHITE, PINK],
    );
    if r.width >= 32 && r.height >= 9 {
        text(f, r, 2, 2, "CAPTAIN OWNS CONTEXT", CYAN);
        text(f, r, 2, 3, "MEMORIES / SKILLS", PINK);
    }
    if r.width >= 64 && r.height >= 20 {
        text(f, r, 8, 9, "MEMORY", PINK);
        text(f, r, r.width - 17, 9, "SKILLS", GREEN);
        sprite(
            f,
            r,
            12,
            r.height as i32 * 2 - 31 + (tick as i32 / 6 % 2),
            &[
                "   +++   ",
                " ++###++ ",
                "##..#..##",
                " ++###++ ",
                "   +++   ",
            ],
            [PINK, WHITE, DIM],
        );
        sprite(
            f,
            r,
            r.width as i32 - 21,
            r.height as i32 * 2 - 31,
            &[
                "#       #",
                " ##+#+## ",
                "   ###   ",
                " ##+#+## ",
                "#       #",
            ],
            [GREEN, WHITE, DIM],
        );
    }
}

fn tools(f: &mut Frame, r: Rect, tick: u64) {
    landscape(f, r, tick);
    let left = 2;
    let right = r.width / 2 + 1;
    if r.width >= 80 && r.height >= 24 {
        let right = r.width / 2 + 8;
        text(f, r, 10, 4, "CLAUDE CODE", PINK);
        text(f, r, right, 4, "CODEX", CYAN);
        text(f, r, 10, r.height / 2 + 3, "CURSOR", GOLD);
        text(f, r, right, r.height / 2 + 3, "PI", GREEN);
        sprite(
            f,
            r,
            14,
            14 + (tick as i32 % 2),
            &[
                "   +++++   ",
                " ++#####++ ",
                "##..#.#..##",
                "##.##+##.##",
                "##..#.#..##",
                " ++#####++ ",
                "   +++++   ",
            ],
            [PINK, WHITE, DIM],
        );
        sprite(
            f,
            r,
            right as i32 + 3,
            14,
            &[
                "###########",
                "##+++++++##",
                "##+.....+##",
                "##+.###.+##",
                "##+.....+##",
                "##+++++++##",
                "###########",
            ],
            [CYAN, WHITE, DIM],
        );
        sprite(
            f,
            r,
            14,
            r.height as i32 + 10 + (tick as i32 % 2),
            &[
                "#         #",
                " ##+     ##",
                "  ###   ## ",
                "   ##+###  ",
                "  ###   ## ",
                " ##+     ##",
                "#         #",
            ],
            [GOLD, WHITE, DIM],
        );
        sprite(
            f,
            r,
            right as i32 + 3,
            r.height as i32 + 10,
            &[
                "   #####   ",
                " ##.....## ",
                "##..###..##",
                "##.##+##.##",
                "##..###..##",
                " ##.....## ",
                "   #####   ",
            ],
            [GREEN, WHITE, DIM],
        );
    } else if r.width >= 32 && r.height >= 12 {
        text(f, r, left, 2, "CLAUDE CODE", PINK);
        text(f, r, right, 2, "CODEX", CYAN);
        text(f, r, left, 8, "CURSOR", GOLD);
        text(f, r, right, 8, "PI", GREEN);
        sprite(
            f,
            r,
            left as i32 + 3,
            6 + (tick as i32 % 2),
            &[" +++ ", "##.##", "#...#", " ### "],
            [PINK, WHITE, DIM],
        );
        sprite(
            f,
            r,
            right as i32 + 2,
            6,
            &["####", "#++#", "#..#", "####"],
            [CYAN, WHITE, DIM],
        );
        sprite(
            f,
            r,
            left as i32 + 3,
            18 + (tick as i32 % 2),
            &["#   #", " ##+ ", " ##+ ", "#   #"],
            [GOLD, WHITE, DIM],
        );
        sprite(
            f,
            r,
            right as i32 + 3,
            18,
            &[" ### ", "#...#", " ##+ ", "#...#"],
            [GREEN, WHITE, DIM],
        );
    } else if r.width >= 16 && r.height >= 7 {
        text(f, r, 1, 2, "CLAUDE / CODEX", PINK);
        text(f, r, 1, 4, "CURSOR / PI", GOLD);
    }
}

fn landing(f: &mut Frame, r: Rect, tick: u64) {
    let x = r.width as i32 / 2 - 22;
    let y = r.height as i32 * 2 - 25;
    landscape(f, r, tick);
    sprite(
        f,
        r,
        x,
        y,
        &[
            "      .####................####.      ",
            "    .###....############....###.    ",
            "   ##....###++++++++++###....##   ",
            " .##...###++..........++###...##. ",
            "##...###++....######....++###...##",
            "##...###++....######....++###...##",
            " .##...###++..........++###...##. ",
            "   ##....###++++++++++###....##   ",
            "    .###....############....###.    ",
            "      ##......########......##      ",
            "    ##....##....##....##....##    ",
            "  ##....##......##......##....##  ",
        ],
        [CYAN, WHITE, DIM],
    );
    if r.width >= 32 && r.height >= 9 {
        text(f, r, (r.width - 18) / 2, 2, "READY TO WORK", WHITE);
    }
}

fn hash(mut x: u32) -> u32 {
    x = x.wrapping_add(0x9e37_79b9);
    x ^= x >> 16;
    x = x.wrapping_mul(0x85eb_ca6b);
    x ^ (x >> 13)
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::{Terminal, backend::TestBackend};

    fn render(w: u16, h: u16, tick: u64, scene: Option<usize>) -> ratatui::buffer::Buffer {
        let mut terminal = Terminal::new(TestBackend::new(w, h)).unwrap();
        terminal.draw(|f| draw(f, f.area(), tick, scene)).unwrap();
        terminal.backend().buffer().clone()
    }

    fn contents(buffer: &ratatui::buffer::Buffer) -> String {
        buffer.content().iter().map(|cell| cell.symbol()).collect()
    }

    #[test]
    fn scenes_fit_and_animate() {
        for (scene, title) in TITLES.iter().enumerate() {
            for (w, h) in [(1, 1), (40, 15), (80, 24), (140, 44)] {
                let frame = render(w, h, 0, Some(scene));
                if w >= 40 {
                    assert!(contents(&frame).contains(title));
                }
            }
        }
        let tools = contents(&render(40, 15, 0, Some(2)));
        for label in ["CLAUDE CODE", "CODEX", "CURSOR", "PI"] {
            assert!(tools.contains(label));
        }
        assert_ne!(render(80, 24, 0, Some(0)), render(80, 24, 3, Some(0)));
    }
}
