//! Cosmetic Galaga break drawn in half-block pixels, the same visual language
//! as the bridge header logo. Score, lives and waves live in memory for the
//! length of this overlay only: nothing here is persisted, counted as work, or
//! reported anywhere else. Crew attention always wins and pauses play.
use crate::theme::*;
use ratatui::{Frame, layout::Rect, style::Color, widgets::Clear};
use std::time::{Duration, Instant};

/// Logical playfield units are half-block "pixels": one cell wide, half a cell tall.
const COLS: u16 = 8;
const ROWS: u16 = 4;
const FOE_W: i32 = 8;
const FOE_H: i32 = 6;
const PITCH_X: i32 = 12;
const PITCH_Y: i32 = 9;
const SHIP_W: i32 = 9;
const SHIP_H: i32 = 7;
const TOP_Y: i32 = 4;
const DESCEND: i32 = 3;
const SHOT_SPEED: u16 = 6;
const BOMB_SPEED: u16 = 3;
const BOOM_LIFE: u8 = 9;
/// Field used while no `draw` has reported a real size yet (headless steps, tests).
const DEF_W: u16 = 160;
const DEF_H: u16 = 70;

const SHIP_ART: [&str; 7] = [
    "    #    ",
    "   ###   ",
    "   #.#   ",
    "  #####  ",
    " ##+++## ",
    "###+++###",
    "#.#   #.#",
];
#[rustfmt::skip]
const FOE_ART: [[&str; 6]; 3] = [
    [   // bee
        " ##  ## ",
        "  ####  ",
        " #+##+# ",
        "########",
        "# #  # #",
        "#      #",
    ],
    [   // butterfly
        "#      #",
        "##    ##",
        " #+##+# ",
        " ###### ",
        "  #..#  ",
        " #    # ",
    ],
    [   // flagship
        "  ####  ",
        " ###### ",
        "##+..+##",
        "########",
        " # ## # ",
        "#  ##  #",
    ],
];
const FOE_PAL: [[Color; 3]; 3] = [
    [GOLD, PINK, WHITE],
    [PINK, CYAN, WHITE],
    [GREEN, GOLD, CYAN],
];
const SHIP_PAL: [Color; 3] = [CYAN, GOLD, WHITE];
const SPARKS: [(i32, i32); 8] = [
    (0, -2),
    (2, -2),
    (3, 0),
    (2, 2),
    (0, 2),
    (-2, 2),
    (-3, 0),
    (-2, -2),
];

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
struct Foe {
    col: u16,
    row: u16,
    kind: u8,
    /// `(x, y, vx)` in field pixels while this foe is dive-bombing.
    dive: Option<(i32, i32, i32)>,
}

pub struct Game {
    pub active: bool,
    /// Player fighter's left edge, in field pixels.
    pub x: u16,
    /// The single player bullet, in field pixels.
    pub shot: Option<(u16, u16)>,
    pub score: u32,
    pub lives: u8,
    pub wave: u16,
    foes: Vec<Foe>,
    bombs: Vec<(u16, u16)>,
    /// Explosions as `(x, y, age)`.
    booms: Vec<(i32, i32, u8)>,
    ox: i32,
    oy: i32,
    dir: i32,
    invuln: u8,
    over: u8,
    field_w: u16,
    field_h: u16,
    frame: u32,
    rng: u32,
    last: Instant,
}

impl Default for Game {
    fn default() -> Self {
        let mut game = Self {
            active: false,
            x: 0,
            shot: None,
            score: 0,
            lives: 3,
            wave: 1,
            foes: Vec::new(),
            bombs: Vec::new(),
            booms: Vec::new(),
            ox: 0,
            oy: TOP_Y,
            dir: 1,
            invuln: 0,
            over: 0,
            field_w: DEF_W,
            field_h: DEF_H,
            frame: 0,
            rng: 0x2545_f491,
            last: Instant::now(),
        };
        game.restart();
        game
    }
}

impl Game {
    pub fn left(&mut self) {
        self.x = self.x.saturating_sub(3);
    }
    pub fn right(&mut self) {
        let max = (self.field_w as i32 - SHIP_W).max(0) as u16;
        self.x = (self.x + 3).min(max);
    }
    pub fn fire(&mut self) {
        if self.shot.is_none() && self.over == 0 {
            let top = self.player_top().max(0) as u16;
            self.shot = Some((self.x + SHIP_W as u16 / 2, top));
        }
    }
    /// Advances play, rate limited so the 80 ms bridge loop stays smooth.
    pub fn step(&mut self) {
        if !self.active || self.last.elapsed() < Duration::from_millis(55) {
            return;
        }
        self.last = Instant::now();
        self.advance();
    }

    /// Full restart: new flight, zero score, wave one.
    pub fn restart(&mut self) {
        self.score = 0;
        self.lives = 3;
        self.wave = 1;
        self.over = 0;
        self.invuln = 0;
        self.booms.clear();
        self.x = ((self.field_w as i32 - SHIP_W).max(0) / 2) as u16;
        self.reset_wave();
    }

    /// Formation columns that fit the current field; the grid shrinks on small terminals.
    fn cols(&self) -> u16 {
        (((self.field_w as i32 - FOE_W).max(0) / PITCH_X + 1).clamp(1, COLS as i32)) as u16
    }
    /// Formation rows that leave the player room to breathe under them.
    fn rows(&self) -> u16 {
        (((self.player_top() - TOP_Y - 4 - FOE_H).max(0) / PITCH_Y + 1).clamp(1, ROWS as i32))
            as u16
    }
    fn form_w(&self) -> i32 {
        (self.cols() as i32 - 1) * PITCH_X + FOE_W
    }

    fn reset_wave(&mut self) {
        let (rows, cols) = (self.rows(), self.cols());
        self.foes = (0..rows)
            .flat_map(|row| {
                (0..cols).map(move |col| Foe {
                    col,
                    row,
                    kind: match row {
                        0 => 2,
                        1 => 1,
                        _ => 0,
                    },
                    dive: None,
                })
            })
            .collect();
        self.ox = (self.field_w as i32 - self.form_w()).max(0) / 2;
        self.oy = TOP_Y;
        self.dir = 1;
        self.shot = None;
        self.bombs.clear();
    }

    fn roll(&mut self) -> u32 {
        let mut x = self.rng;
        x ^= x << 13;
        x ^= x >> 17;
        x ^= x << 5;
        self.rng = x;
        x
    }
    fn below(&mut self, n: usize) -> usize {
        if n == 0 { 0 } else { self.roll() as usize % n }
    }

    fn player_top(&self) -> i32 {
        self.field_h as i32 - SHIP_H - 1
    }
    fn player_rect(&self) -> (i32, i32, i32, i32) {
        (self.x as i32, self.player_top(), SHIP_W, SHIP_H)
    }
    fn foe_pos(&self, foe: &Foe) -> (i32, i32) {
        match foe.dive {
            Some((x, y, _)) => (x, y),
            None => (
                self.ox + foe.col as i32 * PITCH_X,
                self.oy + foe.row as i32 * PITCH_Y,
            ),
        }
    }

    /// Records the drawable field size and clamps every position into it.
    fn adapt(&mut self, w: u16, h: u16) {
        if w == self.field_w && h == self.field_h {
            return;
        }
        self.field_w = w.max(1);
        self.field_h = h.max(1);
        self.x = self.x.min((self.field_w as i32 - SHIP_W).max(0) as u16);
        self.ox = self
            .ox
            .clamp(0, (self.field_w as i32 - self.form_w()).max(0));
        self.oy = self.oy.clamp(0, (self.player_top() - 1).max(0));
        // A shrunken field carries a shrunken formation; survivors keep their place.
        let (rows, cols) = (self.rows(), self.cols());
        self.foes
            .retain(|foe| foe.dive.is_some() || (foe.row < rows && foe.col < cols));
        let (fw, fh) = (self.field_w, self.field_h);
        for foe in &mut self.foes {
            if let Some((x, y, vx)) = foe.dive {
                foe.dive = Some((
                    x.clamp(0, (fw as i32 - FOE_W).max(0)),
                    y.clamp(0, fh as i32),
                    vx,
                ));
            }
        }
        self.bombs.retain(|&(bx, by)| bx < fw && by < fh);
        if let Some((sx, sy)) = self.shot
            && (sx >= fw || sy >= fh)
        {
            self.shot = None;
        }
    }

    fn advance(&mut self) {
        self.frame = self.frame.wrapping_add(1);
        for boom in &mut self.booms {
            boom.2 = boom.2.saturating_add(1);
        }
        self.booms.retain(|b| b.2 < BOOM_LIFE);
        if self.over > 0 {
            self.over -= 1;
            if self.over == 0 {
                self.restart();
            }
            return;
        }
        self.invuln = self.invuln.saturating_sub(1);
        self.move_shot();
        self.move_formation();
        self.move_divers();
        self.spawn_dive();
        self.enemy_fire();
        self.move_bombs();
        self.check_player();
        if self.foes.is_empty() {
            self.wave = self.wave.saturating_add(1);
            self.score += 150;
            self.reset_wave();
        }
    }

    fn move_shot(&mut self) {
        let Some((sx, sy)) = self.shot else { return };
        if sy <= SHOT_SPEED {
            self.shot = None;
            return;
        }
        let sy = sy - SHOT_SPEED;
        self.shot = Some((sx, sy));
        let point = (sx as i32, sy as i32, 1, 3);
        if let Some(i) = self.foes.iter().position(|foe| {
            let (fx, fy) = self.foe_pos(foe);
            overlap(point, (fx, fy, FOE_W, FOE_H))
        }) {
            let foe = self.foes.remove(i);
            let (fx, fy) = self.foe_pos(&foe);
            self.booms.push((fx + FOE_W / 2, fy + FOE_H / 2, 0));
            self.score += match foe.kind {
                2 => 150,
                1 => 80,
                _ => 50,
            } * if foe.dive.is_some() { 2 } else { 1 };
            self.shot = None;
        }
    }

    fn move_formation(&mut self) {
        let speed = 1 + self.wave.min(5) as i32;
        if self.frame.is_multiple_of(2) {
            let span = (self.field_w as i32 - self.form_w()).max(0);
            self.ox += self.dir * speed;
            if self.ox <= 0 {
                self.ox = 0;
                self.dir = 1;
                self.oy += DESCEND;
            } else if self.ox >= span {
                self.ox = span;
                self.dir = -1;
                self.oy += DESCEND;
            }
        }
        // The formation landing on the player costs a life and pushes it back up.
        let deepest = self
            .foes
            .iter()
            .filter(|f| f.dive.is_none())
            .map(|f| self.foe_pos(f).1 + FOE_H)
            .max()
            .unwrap_or(0);
        if deepest >= self.player_top() {
            self.oy = TOP_Y;
            self.hit_player();
        }
    }

    fn move_divers(&mut self) {
        let fall = 2 + (self.wave.min(6) as i32) / 2;
        let edge = (self.field_w as i32 - FOE_W).max(0);
        let floor = self.field_h as i32;
        for foe in &mut self.foes {
            let Some((mut x, mut y, mut vx)) = foe.dive else {
                continue;
            };
            y += fall;
            x += vx;
            if x <= 0 || x >= edge {
                x = x.clamp(0, edge);
                vx = -vx;
            }
            foe.dive = if y > floor { None } else { Some((x, y, vx)) };
        }
        // A diver that flew off the bottom rejoins the formation, unless a resize
        // has since shrunk the grid out from under its slot.
        let (rows, cols) = (self.rows(), self.cols());
        self.foes
            .retain(|foe| foe.dive.is_some() || (foe.row < rows && foe.col < cols));
    }

    fn spawn_dive(&mut self) {
        let every = 34u32.saturating_sub(self.wave.min(5) as u32 * 5).max(9);
        if !self.frame.is_multiple_of(every) {
            return;
        }
        if self.foes.iter().filter(|f| f.dive.is_some()).count() >= 2 {
            return;
        }
        let idle: Vec<usize> = self
            .foes
            .iter()
            .enumerate()
            .filter(|(_, f)| f.dive.is_none())
            .map(|(i, _)| i)
            .collect();
        if idle.is_empty() {
            return;
        }
        let pick = idle[self.below(idle.len())];
        let (fx, fy) = self.foe_pos(&self.foes[pick]);
        let vx = if fx > self.x as i32 { -2 } else { 2 };
        self.foes[pick].dive = Some((fx, fy, vx));
    }

    fn enemy_fire(&mut self) {
        if !self.frame.is_multiple_of(7) || self.bombs.len() >= 6 || self.foes.is_empty() {
            return;
        }
        let divers: Vec<usize> = self
            .foes
            .iter()
            .enumerate()
            .filter(|(_, f)| f.dive.is_some())
            .map(|(i, _)| i)
            .collect();
        let pick = if divers.is_empty() {
            if self.below(3) > 0 {
                return;
            }
            self.below(self.foes.len())
        } else {
            divers[self.below(divers.len())]
        };
        let (fx, fy) = self.foe_pos(&self.foes[pick]);
        let bx = (fx + FOE_W / 2).clamp(0, self.field_w as i32 - 1) as u16;
        let by = (fy + FOE_H).clamp(0, self.field_h as i32 - 1) as u16;
        self.bombs.push((bx, by));
    }

    fn move_bombs(&mut self) {
        let floor = self.field_h;
        let player = self.player_rect();
        let safe = self.invuln > 0;
        let mut struck = false;
        let mut survivors = Vec::with_capacity(self.bombs.len());
        for (bx, by) in std::mem::take(&mut self.bombs) {
            let by = by + BOMB_SPEED;
            if by >= floor {
                continue;
            }
            if !safe && overlap((bx as i32, by as i32, 1, 3), player) {
                struck = true;
                continue;
            }
            survivors.push((bx, by));
        }
        self.bombs = survivors;
        if struck {
            self.hit_player();
        }
    }

    fn check_player(&mut self) {
        if self.invuln > 0 {
            return;
        }
        let player = self.player_rect();
        if let Some(i) = self.foes.iter().position(|foe| {
            foe.dive.is_some() && {
                let (fx, fy) = self.foe_pos(foe);
                overlap((fx, fy, FOE_W, FOE_H), player)
            }
        }) {
            let foe = self.foes.remove(i);
            let (fx, fy) = self.foe_pos(&foe);
            self.booms.push((fx + FOE_W / 2, fy + FOE_H / 2, 0));
            self.hit_player();
        }
    }

    fn hit_player(&mut self) {
        if self.invuln > 0 || self.over > 0 {
            return;
        }
        let (px, py) = (self.x as i32 + SHIP_W / 2, self.player_top() + SHIP_H / 2);
        self.booms.push((px, py, 0));
        self.bombs.clear();
        self.shot = None;
        for foe in &mut self.foes {
            foe.dive = None;
        }
        self.lives = self.lives.saturating_sub(1);
        if self.lives == 0 {
            self.over = 24;
        } else {
            self.invuln = 20;
        }
    }
}

fn overlap(a: (i32, i32, i32, i32), b: (i32, i32, i32, i32)) -> bool {
    a.0 < b.0 + b.2 && b.0 < a.0 + a.2 && a.1 < b.1 + b.3 && b.1 < a.1 + a.3
}

/// Deterministic integer mix used for the starfield (no rand dependency).
fn hash(mut x: u32) -> u32 {
    x = x.wrapping_mul(0x2545_f491);
    x ^= x >> 15;
    x = x.wrapping_mul(0x27d4_eb2d);
    x ^= x >> 15;
    x
}

/// A half-block pixel canvas: two vertical pixels per terminal cell.
struct Canvas {
    w: u16,
    h: u16,
    px: Vec<Option<Color>>,
}

impl Canvas {
    fn new(w: u16, h: u16) -> Self {
        Self {
            w,
            h,
            px: vec![None; w as usize * h as usize],
        }
    }
    fn set(&mut self, x: i32, y: i32, color: Color) {
        if x < 0 || y < 0 || x >= self.w as i32 || y >= self.h as i32 {
            return;
        }
        self.px[y as usize * self.w as usize + x as usize] = Some(color);
    }
    fn fill(&mut self, x: i32, y: i32, w: i32, h: i32, color: Color) {
        for dy in 0..h {
            for dx in 0..w {
                self.set(x + dx, y + dy, color);
            }
        }
    }
    fn sprite(&mut self, x: i32, y: i32, art: &[&str], pal: &[Color; 3]) {
        for (dy, row) in art.iter().enumerate() {
            for (dx, ch) in row.chars().enumerate() {
                let color = match ch {
                    '#' => pal[0],
                    '+' => pal[1],
                    '.' => pal[2],
                    _ => continue,
                };
                self.set(x + dx as i32, y + dy as i32, color);
            }
        }
    }
    fn blit(&self, f: &mut Frame, r: Rect) {
        let buf = f.buffer_mut();
        for cy in 0..r.height.min(self.h.div_ceil(2)) {
            for cx in 0..r.width.min(self.w) {
                let top = self.px[cy as usize * 2 * self.w as usize + cx as usize];
                let below = (cy as usize * 2 + 1) * self.w as usize + cx as usize;
                let bottom = self.px.get(below).copied().flatten();
                buf[(r.x + cx, r.y + cy)]
                    .set_symbol("▀")
                    .set_fg(top.unwrap_or(BG))
                    .set_bg(bottom.unwrap_or(BG));
            }
        }
    }
}

/// Draws the active game as a big centred overlay over `area` (the whole terminal).
pub fn draw(f: &mut Frame, area: Rect, game: &mut Game) {
    if area.width == 0 || area.height == 0 {
        return;
    }
    let w = (area.width as u32 * 85 / 100).max(1) as u16;
    let h = (area.height as u32 * 85 / 100).max(1) as u16;
    let r = Rect::new(
        area.x + (area.width - w) / 2,
        area.y + (area.height - h) / 2,
        w,
        h,
    );
    f.render_widget(Clear, r);
    if w < 4 || h < 3 {
        return;
    }
    let panel = block("GALAGA · ← → / A D · Space fire · Esc pause", CYAN)
        .title_bottom(" Crew attention pauses play · F8 resumes ");
    let inner = panel.inner(r);
    f.render_widget(panel, r);
    if inner.width == 0 || inner.height == 0 {
        return;
    }
    let hud = Rect::new(inner.x, inner.y, inner.width, 1);
    let field = Rect::new(
        inner.x,
        inner.y + 1,
        inner.width,
        inner.height.saturating_sub(1),
    );
    let lives = "▲".repeat(game.lives as usize);
    pixel(
        f,
        hud,
        0,
        0,
        &one_line(
            &format!(
                "WAVE {:02}   SCORE {:06}   LIVES {}",
                game.wave, game.score, lives
            ),
            hud.width,
        ),
        GOLD,
    );
    if field.width < 8 || field.height < 3 {
        return;
    }
    game.adapt(field.width, field.height * 2);
    let mut canvas = Canvas::new(game.field_w, game.field_h);
    paint(&mut canvas, game);
    canvas.blit(f, field);
    if game.over > 0 {
        banner(f, field, game);
    }
}

fn paint(canvas: &mut Canvas, game: &Game) {
    stars(canvas, game.frame);
    for foe in &game.foes {
        let (fx, fy) = game.foe_pos(foe);
        let kind = foe.kind as usize % FOE_ART.len();
        let mut pal = FOE_PAL[kind];
        if foe.dive.is_some() {
            pal[0] = WHITE;
        }
        canvas.sprite(fx, fy, &FOE_ART[kind], &pal);
    }
    for &(bx, by) in &game.bombs {
        canvas.fill(bx as i32, by as i32, 1, 3, PINK);
    }
    if let Some((sx, sy)) = game.shot {
        canvas.fill(sx as i32, sy as i32, 1, 3, GOLD);
        canvas.set(sx as i32, sy as i32 - 1, WHITE);
    }
    if game.over == 0 && (game.invuln == 0 || game.invuln.is_multiple_of(2)) {
        canvas.sprite(game.x as i32, game.player_top(), &SHIP_ART, &SHIP_PAL);
    }
    for &(bx, by, age) in &game.booms {
        let color = match age {
            0..=2 => WHITE,
            3..=5 => GOLD,
            _ => PINK,
        };
        if age < 3 {
            canvas.fill(bx - 2, by - 1, 5, 3, color);
        }
        let reach = age as i32;
        for (dx, dy) in SPARKS {
            canvas.set(bx + dx * reach / 2, by + dy * reach / 2, color);
        }
    }
}

fn stars(canvas: &mut Canvas, frame: u32) {
    let count = ((canvas.w as u32 * canvas.h as u32) / 90).clamp(20, 140);
    for i in 0..count {
        let seed = hash(i ^ 0x9e37_79b9);
        let layer = seed % 3;
        let x = (seed >> 8) % canvas.w.max(1) as u32;
        let y = ((seed >> 3) + frame * (layer + 1)) % canvas.h.max(1) as u32;
        let color = match layer {
            0 => DIM,
            1 => Color::Rgb(140, 165, 200),
            _ => WHITE,
        };
        canvas.set(x as i32, y as i32, color);
    }
}

fn banner(f: &mut Frame, field: Rect, game: &Game) {
    let lines = [
        format!("GAME OVER  ·  WAVE {}", game.wave),
        format!("SCORE {:06} (cosmetic, never logged)", game.score),
    ];
    let row = field.height / 2;
    for (i, text) in lines.iter().enumerate() {
        let text = one_line(text, field.width);
        let x = (field.width as usize).saturating_sub(text.chars().count()) as u16 / 2;
        pixel(
            f,
            field,
            x,
            row + i as u16,
            &text,
            if i == 0 { PINK } else { WHITE },
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::{Terminal, backend::TestBackend};

    fn playing() -> Game {
        Game {
            active: true,
            ..Game::default()
        }
    }

    fn render(w: u16, h: u16, game: &mut Game) -> ratatui::buffer::Buffer {
        let mut t = Terminal::new(TestBackend::new(w, h)).unwrap();
        t.draw(|f| {
            let area = f.area();
            draw(f, area, game);
        })
        .unwrap();
        t.backend().buffer().clone()
    }

    #[test]
    fn shot_destroys_a_foe_and_scores() {
        let mut game = playing();
        let foe = game.foes[0];
        let (fx, fy) = game.foe_pos(&foe);
        let count = game.foes.len();
        // One step below the foe's bottom row, so the step lands the bullet inside it.
        game.shot = Some((
            (fx + FOE_W / 2) as u16,
            (fy + FOE_H - 1) as u16 + SHOT_SPEED,
        ));
        game.move_shot();
        assert_eq!(game.foes.len(), count - 1, "the hit foe must be removed");
        assert_eq!(game.shot, None, "the bullet is spent on impact");
        assert!(game.score > 0, "kills score");
        assert_eq!(game.booms.len(), 1, "a kill spawns one explosion");
    }

    #[test]
    fn clearing_the_field_starts_a_faster_wave() {
        let mut game = playing();
        game.foes.clear();
        game.advance();
        assert_eq!(game.wave, 2);
        assert_eq!(game.foes.len(), (COLS * ROWS) as usize);
        assert_eq!(game.oy, TOP_Y, "a new wave starts back at the top");
    }

    #[test]
    fn losing_every_life_shows_a_banner_then_restarts() {
        let mut game = playing();
        game.score = 999;
        for _ in 0..3 {
            game.invuln = 0;
            game.hit_player();
        }
        assert_eq!(game.lives, 0);
        assert!(game.over > 0, "game over holds a banner");
        while game.over > 0 {
            game.advance();
        }
        assert_eq!(game.lives, 3, "the flight restarts by itself");
        assert_eq!(game.score, 0, "score is cosmetic and resets");
        assert_eq!(game.wave, 1);
    }

    #[test]
    fn an_inactive_game_never_steps() {
        let mut game = playing();
        game.active = false;
        game.last -= Duration::from_secs(1);
        let (frame, foes) = (game.frame, game.foes.clone());
        game.step();
        assert_eq!(game.frame, frame);
        assert_eq!(game.foes, foes);
        game.active = true;
        game.step();
        assert_eq!(game.frame, frame + 1, "an active game steps");
    }

    #[test]
    fn keys_move_the_fighter_and_stay_on_the_field() {
        let mut game = playing();
        let start = game.x;
        game.right();
        assert!(game.x >= start + 2, "movement is responsive at 80 ms");
        game.left();
        assert_eq!(game.x, start);
        for _ in 0..500 {
            game.left();
        }
        assert_eq!(game.x, 0);
        for _ in 0..500 {
            game.right();
        }
        assert!(game.x as i32 + SHIP_W <= game.field_w as i32);
    }

    #[test]
    fn draw_fills_most_of_the_terminal_with_pixel_glyphs() {
        let mut game = playing();
        let buf = render(140, 44, &mut game);
        let mut painted = (u16::MAX, u16::MAX, 0u16, 0u16);
        let mut pixels = 0;
        for y in 0..44 {
            for x in 0..140 {
                let cell = &buf[(x, y)];
                if cell.symbol() == " " && cell.bg == Color::Reset {
                    continue;
                }
                painted.0 = painted.0.min(x);
                painted.1 = painted.1.min(y);
                painted.2 = painted.2.max(x);
                painted.3 = painted.3.max(y);
                if cell.symbol() == "▀" {
                    pixels += 1;
                }
            }
        }
        let (w, h) = (painted.2 - painted.0 + 1, painted.3 - painted.1 + 1);
        assert!(w >= 140 * 80 / 100, "overlay spans {w} of 140 columns");
        assert!(h >= 44 * 80 / 100, "overlay spans {h} of 44 rows");
        assert!(pixels > 3000, "the field is drawn in half-block pixels");
        assert!(
            game.field_w >= 110 && game.field_h >= 60,
            "the logical field adapts to the drawn size: {}×{}",
            game.field_w,
            game.field_h
        );
    }

    #[test]
    fn draw_survives_any_terminal_size() {
        for (w, h) in [(1, 1), (2, 3), (8, 4), (65, 23), (40, 12)] {
            let mut game = playing();
            render(w, h, &mut game);
            game.advance();
            render(w, h, &mut game);
        }
        // At the bridge's minimum terminal the formation shrinks to fit rather than
        // clipping off-screen or landing on the player the moment play starts.
        let mut game = playing();
        render(65, 23, &mut game);
        game.reset_wave();
        assert!(game.cols() < COLS && game.rows() < ROWS, "the grid shrinks");
        assert!(game.form_w() <= game.field_w as i32, "the formation fits");
        game.advance();
        assert_eq!(game.lives, 3, "a fresh small field is survivable");

        // A resize must leave the state valid rather than panicking later.
        let mut game = playing();
        render(200, 60, &mut game);
        render(66, 24, &mut game);
        for _ in 0..200 {
            game.advance();
        }
        assert!(game.x as i32 + SHIP_W <= game.field_w as i32);
    }
}
