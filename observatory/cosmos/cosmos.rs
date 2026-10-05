//! Cosmos: the Observatory hero. A galaxy merger simulated and rasterized entirely here,
//! compiled to `wasm32-unknown-unknown` by `observatory/build.rs` with no crates at all.
//!
//! Physics: restricted N-body in the Toomre & Toomre (1972) style. Two galaxies, each a
//! dark halo with a flat rotation curve plus a supermassive black hole, pull on hundreds
//! of thousands of test stars and on each other; dynamical friction makes them merge. The
//! stars are integrated four at a time with wasm SIMD (symplectic Euler, fixed substeps).
//!
//! Rendering: a pinhole camera projects every star (SIMD), splats it bilinearly into a
//! float RGBA accumulation buffer, then one SIMD pass tone-maps to RGBA8 and clears. The
//! page uploads that buffer as a texture; a small shader adds lensing and bloom.
//!
//! Flight: the ship is under way. Three parallax layers of near stars stream past a
//! camera flying towards the merger, growing and brightening as they approach, and the
//! heading drifts slowly around it. They are placed and moved entirely on the GPU (a
//! hash of the instance id in a field that wraps around the camera), so the CPU only
//! integrates one position per frame. Reduced motion freezes it with the rest.
//!
//! Mood: this is the view from the bridge, so it stays calm. Time runs slowly, stars
//! twinkle over seconds, nothing flashes: an epoch fades in from dark (first light) and
//! out again (dusk), and the merger's gravitational wave is a soft ripple.
//!
//! Interface: raw `extern "C"` exports, one linear memory, a bump arena grown by hand.
#![no_std]
#![allow(static_mut_refs, clippy::all)]

use core::arch::wasm32::*;

#[panic_handler]
fn panic(_: &core::panic::PanicInfo) -> ! {
    unreachable()
}

unsafe extern "C" {
    static __heap_base: u8;
}

// ---------------------------------------------------------------- scalar math (no libm)

const PI: f32 = 3.141_592_7;
const TAU: f32 = 6.283_185_5;

#[inline(always)]
fn sqrtf(x: f32) -> f32 {
    f32x4_extract_lane::<0>(f32x4_sqrt(f32x4_splat(x)))
}
#[inline(always)]
fn floorf(x: f32) -> f32 {
    f32x4_extract_lane::<0>(f32x4_floor(f32x4_splat(x)))
}
fn sinf(x: f32) -> f32 {
    let mut x = x - TAU * floorf(x / TAU + 0.5);
    if x > PI * 0.5 {
        x = PI - x;
    } else if x < -PI * 0.5 {
        x = -PI - x;
    }
    let x2 = x * x;
    x * (1.0 - x2 / 6.0 * (1.0 - x2 / 20.0 * (1.0 - x2 / 42.0 * (1.0 - x2 / 72.0))))
}
fn cosf(x: f32) -> f32 {
    sinf(x + PI * 0.5)
}
fn lnf(x: f32) -> f32 {
    if x <= 0.0 {
        return -30.0;
    }
    let bits = x.to_bits();
    let e = ((bits >> 23) & 255) as i32 - 127;
    let m = f32::from_bits((bits & 0x007f_ffff) | 0x3f80_0000);
    let s = (m - 1.0) / (m + 1.0);
    let s2 = s * s;
    e as f32 * 0.693_147_2 + 2.0 * s * (1.0 + s2 * (1.0 / 3.0 + s2 * (0.2 + s2 * (1.0 / 7.0 + s2 / 9.0))))
}
fn expf(x: f32) -> f32 {
    let x = if x < -80.0 {
        -80.0
    } else if x > 80.0 {
        80.0
    } else {
        x
    };
    let k = floorf(x / 0.693_147_2 + 0.5);
    let r = x - k * 0.693_147_2;
    let p = 1.0 + r * (1.0 + r * (0.5 + r * (1.0 / 6.0 + r * (1.0 / 24.0 + r * (1.0 / 120.0 + r / 720.0)))));
    p * f32::from_bits(((k as i32 + 127).clamp(1, 254) as u32) << 23)
}
fn clampf(x: f32, a: f32, b: f32) -> f32 {
    if x < a {
        a
    } else if x > b {
        b
    } else {
        x
    }
}
fn mix(a: f32, b: f32, t: f32) -> f32 {
    a + (b - a) * t
}

#[derive(Clone, Copy)]
struct V3 {
    x: f32,
    y: f32,
    z: f32,
}
const fn v3(x: f32, y: f32, z: f32) -> V3 {
    V3 { x, y, z }
}
impl V3 {
    fn add(self, o: V3) -> V3 {
        v3(self.x + o.x, self.y + o.y, self.z + o.z)
    }
    fn sub(self, o: V3) -> V3 {
        v3(self.x - o.x, self.y - o.y, self.z - o.z)
    }
    fn mul(self, k: f32) -> V3 {
        v3(self.x * k, self.y * k, self.z * k)
    }
    fn dot(self, o: V3) -> f32 {
        self.x * o.x + self.y * o.y + self.z * o.z
    }
    fn cross(self, o: V3) -> V3 {
        v3(
            self.y * o.z - self.z * o.y,
            self.z * o.x - self.x * o.z,
            self.x * o.y - self.y * o.x,
        )
    }
    fn len(self) -> f32 {
        sqrtf(self.dot(self))
    }
    fn norm(self) -> V3 {
        let l = self.len();
        if l > 1e-9 {
            self.mul(1.0 / l)
        } else {
            v3(0.0, 0.0, 1.0)
        }
    }
}

// ---------------------------------------------------------------- rng

static mut RNG: u32 = 0x9e37_79b9;
fn rnd() -> f32 {
    unsafe {
        let mut x = RNG;
        x ^= x << 13;
        x ^= x >> 17;
        x ^= x << 5;
        RNG = x;
        (x >> 8) as f32 * (1.0 / 16_777_216.0)
    }
}
fn rnd_range(a: f32, b: f32) -> f32 {
    a + (b - a) * rnd()
}
fn gauss() -> f32 {
    let u1 = rnd().max(1e-7);
    let u2 = rnd();
    sqrtf(-2.0 * lnf(u1)) * cosf(TAU * u2)
}
fn rand_dir() -> V3 {
    let z = rnd_range(-1.0, 1.0);
    let a = rnd() * TAU;
    let r = sqrtf((1.0 - z * z).max(0.0));
    v3(r * cosf(a), r * sinf(a), z)
}

// ---------------------------------------------------------------- state

#[derive(Clone, Copy)]
struct Core {
    p: V3,
    v: V3,
    v0sq: f32, // halo strength: flat rotation speed squared
    rc2: f32,  // halo core radius squared
    mbh: f32,  // black hole mass
    eb2: f32,  // black hole softening squared
    alive: bool,
}
const CORE0: Core = Core {
    p: v3(0.0, 0.0, 0.0),
    v: v3(0.0, 0.0, 0.0),
    v0sq: 0.0,
    rc2: 1.0,
    mbh: 0.0,
    eb2: 0.01,
    alive: false,
};

const MAX_BEACONS: usize = 24;
/// A comet's whole life, and how long its head takes to cross (seconds): an unhurried arc.
const COMET_S: f32 = 9.0;
const COMET_CROSS_S: f32 = 6.5;
const MAX_COMETS: usize = 6;
const BG_STARS: usize = 2400;

struct State {
    cap: usize,
    n: usize,
    px: *mut f32,
    py: *mut f32,
    pz: *mut f32,
    vx: *mut f32,
    vy: *mut f32,
    vz: *mut f32,
    col: *mut u32,
    part_end: usize,
    top: usize,
    w: usize,
    h: usize,
    win: [f32; 4],
    /// Where to frame the galaxy (the cockpit's clear sight, or the whole glass), as
    /// fractions of the canvas; `framed` is where it is framed now, gliding towards it.
    focus: [f32; 4],
    framed: [f32; 4],
    focus_set: bool,
    still: bool,
    cores: [Core; 2],
    // clocks (seconds of real time unless noted)
    phase: u32,
    phase_t: f32,
    sim_t: f32, // simulation units
    acc_dt: f32,
    epoch: u32,
    peri: u32,
    last_sep: f32,
    closing: bool,
    wave_age: f32,
    wave_pos: V3,
    origin: V3,
    // camera
    target: V3,
    dist: f32,
    az: f32,
    el_t: f32,
    // data signals
    workers: u32,
    calls: u32,
    beacons: [u32; MAX_BEACONS],
    nbeacons: usize,
    comets: [f32; MAX_COMETS], // age in seconds, <0 = unused
    comet_seed: [f32; MAX_COMETS],
    info: [f32; 32],
    exposure: f32,
    clock: f32,
}

static mut S: State = State {
    cap: 0,
    n: 0,
    px: 0 as *mut f32,
    py: 0 as *mut f32,
    pz: 0 as *mut f32,
    vx: 0 as *mut f32,
    vy: 0 as *mut f32,
    vz: 0 as *mut f32,
    col: 0 as *mut u32,
    part_end: 0,
    top: 0,
    w: 0,
    h: 0,
    win: [0.0, 0.0, 1.0, 1.0],
    focus: [0.03, 0.04, 0.97, 0.72],
    framed: [0.03, 0.04, 0.97, 0.72],
    focus_set: false,
    still: false,
    cores: [CORE0; 2],
    phase: 0,
    phase_t: 0.0,
    sim_t: 0.0,
    acc_dt: 0.0,
    epoch: 0,
    peri: 0,
    last_sep: 0.0,
    closing: true,
    wave_age: -1.0,
    wave_pos: v3(0.0, 0.0, 0.0),
    origin: v3(0.0, 0.0, 0.0),
    target: v3(0.0, 0.0, 0.0),
    dist: 30.0,
    az: 0.6,
    el_t: 0.0,
    workers: 0,
    calls: 0,
    beacons: [0; MAX_BEACONS],
    nbeacons: 0,
    comets: [-1.0; MAX_COMETS],
    comet_seed: [0.0; MAX_COMETS],
    info: [0.0; 32],
    exposure: 1.0,
    clock: 0.0,
};

// Zero-initialised, so they cost nothing in the module (.bss).
static mut BG: [[f32; 4]; BG_STARS] = [[0.0; 4]; BG_STARS]; // direction xyz + brightness
static mut BGC: [u32; BG_STARS] = [0; BG_STARS];

/// Flight field: instances per parallax layer (near, mid, far; the shader's split) and
/// the ship's speed in world units per second.
const FLY_N: u32 = 4800;
const FLY_SPEED: f32 = 5.5;
/// The field repeats every FLY_WRAP units (a multiple of every layer's period).
const FLY_WRAP: f32 = 720.0;
/// The ship's position in the field (wrapped) and its heading this frame.
static mut FLY_P: [f32; 3] = [0.0; 3];

fn st() -> &'static mut State {
    unsafe { &mut S }
}

// Simulation constants.
const DT: f32 = 0.025; // one substep, simulation units
const TIME_SCALE: f32 = 1.3; // simulation units per real second: a slow drift
const MAX_SUB: u32 = 4;
const BANG_S: f32 = 6.0; // first light: the new pair fades in from dark
const FADE_S: f32 = 7.0; // dusk: the remnant fades out before the next epoch
const REMNANT_S: f32 = 24.0;
const MAX_EPOCH_UNITS: f32 = 190.0;

// Phases reported to the page.
const PH_BANG: u32 = 0; // first light
const PH_APPROACH: u32 = 1;
const PH_PASSAGE: u32 = 2;
const PH_TAILS: u32 = 3;
const PH_BINARY: u32 = 4;
const PH_MERGER: u32 = 5;
const PH_REMNANT: u32 = 6;
const PH_FADE: u32 = 7; // dusk

// ---------------------------------------------------------------- arena

fn grow_to(end: usize) {
    let have = memory_size::<0>() * 65536;
    if end > have {
        memory_grow::<0>((end - have + 65535) / 65536);
    }
}
fn alloc(bytes: usize) -> usize {
    let s = st();
    let p = (s.top + 15) & !15;
    s.top = p + bytes;
    grow_to(s.top);
    p
}

// ---------------------------------------------------------------- scene generation

fn rgb(r: f32, g: f32, b: f32) -> u32 {
    let q = |x: f32| clampf(x, 0.0, 255.0) as u32;
    q(r) | (q(g) << 8) | (q(b) << 16)
}

const WHITE: [f32; 3] = [218.0, 232.0, 241.0];
const CYAN: [f32; 3] = [99.0, 235.0, 233.0];
const GOLD: [f32; 3] = [255.0, 207.0, 107.0];
const PINK: [f32; 3] = [244.0, 119.0, 172.0];
const LAVENDER: [f32; 3] = [176.0, 160.0, 236.0];

fn mixc(a: [f32; 3], b: [f32; 3], t: f32, k: f32) -> u32 {
    rgb(mix(a[0], b[0], t) * k, mix(a[1], b[1], t) * k, mix(a[2], b[2], t) * k)
}

struct Galaxy {
    c: Core,
    e1: V3,
    e2: V3,
    n: V3,
    a1: V3, // accretion disk plane
    a2: V3,
    spin: f32,
    rd: f32,
    rmax: f32,
    arm: f32,
    pitch: f32,
    outer: [f32; 3],
    knot: [f32; 3],
}

fn vcirc(c: &Core, r: f32) -> f32 {
    let r2 = r * r;
    let q = r2 + c.eb2;
    sqrtf(c.v0sq * r2 / (r2 + c.rc2) + c.mbh * r2 / (q * sqrtf(q)))
}

fn basis(n: V3) -> (V3, V3) {
    let a = if n.z.abs() < 0.9 {
        v3(0.0, 0.0, 1.0)
    } else {
        v3(1.0, 0.0, 0.0)
    };
    let e1 = a.cross(n).norm();
    (e1, n.cross(e1))
}

fn tilt(axis_angle: f32, incl: f32) -> V3 {
    // normal tilted from +z by `incl` towards direction `axis_angle`
    v3(sinf(incl) * cosf(axis_angle), sinf(incl) * sinf(axis_angle), cosf(incl))
}

/// One star of galaxy `g`. Returns (position, velocity, color, is_beacon_candidate).
fn star(g: &Galaxy) -> (V3, V3, u32, bool) {
    let u = rnd();
    let c = &g.c;
    if u < 0.03 {
        // accretion disk around the black hole: hot, fast, thin
        let t = rnd();
        let t = t * t;
        let r = 0.11 + 0.42 * t;
        let a = rnd() * TAU;
        let (ca, sa) = (cosf(a), sinf(a));
        let pos =
            g.a1.mul(r * ca)
                .add(g.a2.mul(r * sa))
                .add(g.a1.cross(g.a2).mul(gauss() * 0.004));
        let tang = g.a1.mul(-sa).add(g.a2.mul(ca));
        let v = tang.mul(vcirc(c, r) * g.spin);
        let hot = 1.0 - t;
        let col = if t < 0.35 {
            mixc([255.0, 250.0, 235.0], GOLD, t / 0.35, 0.35 + 0.5 * hot)
        } else {
            mixc(GOLD, PINK, (t - 0.35) / 0.65, 0.5)
        };
        return (c.p.add(pos), c.v.add(v), col, false);
    }
    if u < 0.16 {
        // bulge: a flattened, warm, dynamically hot swarm
        let r = 0.08 + 0.55 * (gauss().abs() + gauss().abs() * 0.5);
        let d = rand_dir();
        let dn = g.n.mul(d.dot(g.n) * 0.4);
        let pos = d.sub(g.n.mul(d.dot(g.n))).add(dn).mul(r);
        let rhat = pos.norm();
        let tdisk = g.n.cross(rhat).mul(g.spin);
        let rp = rand_dir();
        let perp = rp.sub(rhat.mul(rp.dot(rhat))).norm();
        let dir = tdisk.mul(0.75).add(perp.mul(0.66)).norm();
        let v = dir.mul(vcirc(c, pos.len()) * 0.95);
        let k = rnd_range(0.25, 0.55);
        let col = mixc(GOLD, [255.0, 170.0, 110.0], rnd(), k);
        return (c.p.add(pos), c.v.add(v), col, false);
    }
    // disk: exponential profile, two logarithmic arms
    let mut r = -g.rd * lnf(rnd().max(1e-6) * rnd().max(1e-6));
    if r > g.rmax {
        r = rnd() * g.rmax;
    }
    r = r.max(0.25);
    let in_arm = rnd() < 0.7;
    let a = if in_arm {
        let k = if rnd() < 0.5 { 0.0 } else { PI };
        g.arm + k + g.spin * lnf(r / 0.4) / g.pitch + gauss() * (0.1 + 0.035 * r)
    } else {
        rnd() * TAU
    };
    let (ca, sa) = (cosf(a), sinf(a));
    let h = gauss() * (0.035 + 0.012 * r);
    let pos = g.e1.mul(r * ca).add(g.e2.mul(r * sa)).add(g.n.mul(h));
    let tang = g.e1.mul(-sa).add(g.e2.mul(ca));
    let disp = rand_dir().mul(0.035);
    let v = tang.mul(vcirc(c, r) * g.spin).add(disp);
    let t = clampf(r / g.rmax, 0.0, 1.0);
    // warm old stars inside, the galaxy's own colour in the arms, pink star-forming knots
    let base = |k: f32| {
        if t < 0.22 {
            mixc(GOLD, WHITE, t / 0.22, k)
        } else {
            mixc(WHITE, g.outer, clampf((t - 0.22) * 2.2, 0.0, 1.0), k)
        }
    };
    let col = if in_arm && rnd() < 0.05 {
        mixc(g.knot, WHITE, 0.15, rnd_range(1.3, 2.2))
    } else if in_arm {
        base(rnd_range(0.5, 0.95))
    } else {
        base(rnd_range(0.18, 0.4))
    };
    (c.p.add(pos), c.v.add(v), col, (2.6..5.4).contains(&r))
}

fn new_epoch(seed: u32) {
    let s = st();
    unsafe {
        COLORS_DIRTY = true;
        RNG = seed ^ 0x2545_f491 ^ (s.epoch.wrapping_mul(0x9e37_79b9));
        if RNG == 0 {
            RNG = 1;
        }
    }
    for _ in 0..8 {
        rnd();
    }
    s.epoch += 1;
    // two galaxies on a bound, eccentric, prograde-ish encounter in the xy-plane
    let ma = 1.0;
    let mb = rnd_range(0.55, 0.9);
    let sep = rnd_range(19.0, 22.0);
    let va = rnd_range(0.52, 0.62);
    let orient = rnd() * TAU;
    let dhat = v3(cosf(orient), sinf(orient), rnd_range(-0.12, 0.12)).norm();
    let that = v3(0.0, 0.0, 1.0).cross(dhat).norm();
    let d = dhat.mul(sep);
    let dv = that.mul(va).add(dhat.mul(-rnd_range(0.05, 0.16)));
    let m = ma + mb;
    let ca = Core {
        p: d.mul(-mb / m),
        v: dv.mul(-mb / m),
        v0sq: ma,
        rc2: 0.36,
        mbh: 0.05,
        eb2: 0.0036,
        alive: true,
    };
    let cb = Core {
        p: d.mul(ma / m),
        v: dv.mul(ma / m),
        v0sq: mb,
        rc2: 0.25,
        mbh: 0.05 * mb,
        eb2: 0.0036,
        alive: true,
    };
    let na = tilt(rnd() * TAU, rnd_range(0.15, 0.6));
    let spin_b = if rnd() < 0.78 { 1.0 } else { -1.0 };
    let nb = tilt(rnd() * TAU, rnd_range(0.3, 1.1));
    let mk = |c: Core, n: V3, spin: f32, rd: f32, rmax: f32, outer, knot| {
        let (e1, e2) = basis(n);
        let an = n.add(rand_dir().mul(0.35)).norm();
        let (a1, a2) = basis(an);
        Galaxy {
            c,
            e1,
            e2,
            n,
            a1,
            a2,
            spin,
            rd,
            rmax,
            arm: rnd() * TAU,
            pitch: rnd_range(0.28, 0.42),
            outer,
            knot,
        }
    };
    let (outer_a, knot_a, outer_b, knot_b) = if s.epoch % 2 == 0 {
        (CYAN, PINK, LAVENDER, CYAN)
    } else {
        (LAVENDER, CYAN, CYAN, PINK)
    };
    let ga = mk(ca, na, 1.0, 1.35, 6.8, outer_a, knot_a);
    let gb = mk(cb, nb, spin_b, 1.05, 5.0, outer_b, knot_b);
    let frac_a = ma / m + 0.06;
    s.nbeacons = 0;
    for i in 0..s.cap {
        let from_a = rnd() < frac_a;
        let (p, v, col, cand) = star(if from_a { &ga } else { &gb });
        unsafe {
            *s.px.add(i) = p.x;
            *s.py.add(i) = p.y;
            *s.pz.add(i) = p.z;
            *s.vx.add(i) = v.x;
            *s.vy.add(i) = v.y;
            *s.vz.add(i) = v.z;
            *s.col.add(i) = col;
        }
        if from_a && cand && s.nbeacons < MAX_BEACONS {
            s.beacons[s.nbeacons] = i as u32;
            s.nbeacons += 1;
        }
    }
    s.cores = [ca, cb];
    s.origin = v3(0.0, 0.0, 0.0);
    s.target = v3(0.0, 0.0, 0.0);
    s.phase = PH_BANG;
    s.phase_t = 0.0;
    s.sim_t = 0.0;
    s.acc_dt = 0.0;
    s.peri = 0;
    s.last_sep = sep;
    s.closing = true;
    s.wave_age = -1.0;
    s.dist = 34.0;
}

fn gen_background() {
    for i in 0..BG_STARS {
        let d = rand_dir();
        let q = rnd();
        let q3 = q * q * q;
        let b = q3 * q3 * 2.4 + 0.12;
        unsafe {
            BG[i] = [d.x, d.y, d.z, b];
        }
        let pick = rnd();
        let c = if pick < 0.55 {
            [150.0, 170.0, 200.0]
        } else if pick < 0.75 {
            CYAN
        } else if pick < 0.9 {
            GOLD
        } else {
            PINK
        };
        unsafe {
            BGC[i] = rgb(c[0], c[1], c[2]);
        }
    }
}

// ---------------------------------------------------------------- physics

#[inline(always)]
fn pull(x: v128, y: v128, z: v128, c: &Core, ax: &mut v128, ay: &mut v128, az: &mut v128) {
    let dx = f32x4_sub(f32x4_splat(c.p.x), x);
    let dy = f32x4_sub(f32x4_splat(c.p.y), y);
    let dz = f32x4_sub(f32x4_splat(c.p.z), z);
    let r2 = f32x4_add(f32x4_add(f32x4_mul(dx, dx), f32x4_mul(dy, dy)), f32x4_mul(dz, dz));
    let halo = f32x4_div(f32x4_splat(c.v0sq), f32x4_add(r2, f32x4_splat(c.rc2)));
    let q = f32x4_add(r2, f32x4_splat(c.eb2));
    let inv = f32x4_div(f32x4_splat(1.0), f32x4_sqrt(q));
    let bh = f32x4_mul(f32x4_splat(c.mbh), f32x4_mul(inv, f32x4_mul(inv, inv)));
    let f = f32x4_add(halo, bh);
    *ax = f32x4_add(*ax, f32x4_mul(dx, f));
    *ay = f32x4_add(*ay, f32x4_mul(dy, f));
    *az = f32x4_add(*az, f32x4_mul(dz, f));
}

fn step_stars(dt: f32) {
    let s = st();
    let n = s.n;
    let dtv = f32x4_splat(dt);
    let c0 = s.cores[0];
    let c1 = s.cores[1];
    let two = c1.alive;
    let mut i = 0;
    while i < n {
        unsafe {
            let x = v128_load(s.px.add(i) as *const v128);
            let y = v128_load(s.py.add(i) as *const v128);
            let z = v128_load(s.pz.add(i) as *const v128);
            let mut ax = f32x4_splat(0.0);
            let mut ay = ax;
            let mut az = ax;
            pull(x, y, z, &c0, &mut ax, &mut ay, &mut az);
            if two {
                pull(x, y, z, &c1, &mut ax, &mut ay, &mut az);
            }
            let vx = f32x4_add(v128_load(s.vx.add(i) as *const v128), f32x4_mul(ax, dtv));
            let vy = f32x4_add(v128_load(s.vy.add(i) as *const v128), f32x4_mul(ay, dtv));
            let vz = f32x4_add(v128_load(s.vz.add(i) as *const v128), f32x4_mul(az, dtv));
            v128_store(s.vx.add(i) as *mut v128, vx);
            v128_store(s.vy.add(i) as *mut v128, vy);
            v128_store(s.vz.add(i) as *mut v128, vz);
            v128_store(s.px.add(i) as *mut v128, f32x4_add(x, f32x4_mul(vx, dtv)));
            v128_store(s.py.add(i) as *mut v128, f32x4_add(y, f32x4_mul(vy, dtv)));
            v128_store(s.pz.add(i) as *mut v128, f32x4_add(z, f32x4_mul(vz, dtv)));
        }
        i += 4;
    }
}

fn field(c: &Core, d: V3) -> V3 {
    // acceleration towards core c of a body at offset d from it
    let r2 = d.dot(d);
    let q = r2 + c.eb2;
    let f = c.v0sq / (r2 + c.rc2) + c.mbh / (q * sqrtf(q));
    d.mul(-f)
}

fn step_cores(dt: f32) {
    let s = st();
    if !s.cores[1].alive {
        let c = &mut s.cores[0];
        c.p = c.p.add(c.v.mul(dt));
        return;
    }
    let (a, b) = (s.cores[0], s.cores[1]);
    let d = b.p.sub(a.p);
    let r = d.len();
    let mut aa = field(&b, d.mul(-1.0));
    let mut ab = field(&a, d);
    // dynamical friction: the halos soak up relative motion once they overlap
    let m = a.v0sq + b.v0sq;
    let rel = b.v.sub(a.v);
    let k = 0.045 * clampf(1.0 - r / 9.0, 0.0, 1.0) + 0.3 * clampf(1.0 - r / 1.4, 0.0, 1.0);
    ab = ab.sub(rel.mul(k * a.v0sq / m));
    aa = aa.add(rel.mul(k * b.v0sq / m));
    let sa = &mut s.cores[0];
    sa.v = sa.v.add(aa.mul(dt));
    sa.p = sa.p.add(sa.v.mul(dt));
    let sb = &mut s.cores[1];
    sb.v = sb.v.add(ab.mul(dt));
    sb.p = sb.p.add(sb.v.mul(dt));
    // pericentre bookkeeping
    let sep = s.cores[1].p.sub(s.cores[0].p).len();
    let closing = sep < s.last_sep;
    if s.closing && !closing {
        s.peri += 1;
    }
    s.closing = closing;
    s.last_sep = sep;
    if sep < 0.16 && s.sim_t > 8.0 {
        // coalescence: one black hole, one halo, and a gravitational wave
        let (a, b) = (s.cores[0], s.cores[1]);
        let m = a.v0sq + b.v0sq;
        let wa = a.v0sq / m;
        let wb = b.v0sq / m;
        let p = a.p.mul(wa).add(b.p.mul(wb));
        let c = &mut s.cores[0];
        c.p = p;
        c.v = a.v.mul(wa).add(b.v.mul(wb));
        c.v0sq = m * 0.92;
        c.rc2 = 0.5;
        c.mbh = a.mbh + b.mbh;
        s.cores[1].alive = false;
        s.cores[1].p = p;
        s.wave_age = 0.0;
        s.wave_pos = p;
        s.phase = PH_MERGER;
        s.phase_t = 0.0;
    }
}

fn separation() -> f32 {
    let s = st();
    s.cores[1].p.sub(s.cores[0].p).len()
}

fn advance(dt_real: f32) {
    let s = st();
    s.clock += dt_real;
    s.phase_t += dt_real;
    if s.wave_age >= 0.0 {
        s.wave_age += dt_real;
    }
    for c in s.comets.iter_mut() {
        if *c >= 0.0 {
            *c += dt_real;
            if *c > COMET_S {
                *c = -1.0;
            }
        }
    }
    match s.phase {
        PH_BANG => {
            if s.phase_t >= BANG_S {
                s.phase = PH_APPROACH;
                s.phase_t = 0.0;
            }
            return;
        }
        PH_FADE => {
            if s.phase_t >= FADE_S {
                new_epoch(s.epoch.wrapping_mul(747_796_405).wrapping_add(2_891_336_453));
            }
        }
        _ => {}
    }
    s.acc_dt += dt_real * TIME_SCALE;
    let mut sub = 0;
    while s.acc_dt >= DT && sub < MAX_SUB {
        step_stars(DT);
        step_cores(DT);
        s.sim_t += DT;
        s.acc_dt -= DT;
        sub += 1;
    }
    if s.acc_dt > DT * 2.0 {
        s.acc_dt = 0.0; // we fell behind: slow time instead of spiralling
    }
    // narrative
    let sep = separation();
    let merged = !s.cores[1].alive;
    s.phase = match s.phase {
        PH_FADE => PH_FADE,
        PH_MERGER if s.phase_t < 3.5 => PH_MERGER,
        PH_MERGER => {
            s.phase_t = 0.0;
            PH_REMNANT
        }
        PH_REMNANT if s.phase_t > REMNANT_S => {
            s.phase_t = 0.0;
            PH_FADE
        }
        PH_REMNANT => PH_REMNANT,
        _ if merged => PH_REMNANT,
        _ if s.sim_t > MAX_EPOCH_UNITS => {
            s.phase_t = 0.0;
            PH_FADE
        }
        _ if s.peri >= 1 && sep < 1.8 => PH_BINARY,
        _ if s.peri == 0 && sep > 7.0 => PH_APPROACH,
        _ if s.peri <= 1 && sep < 9.0 && s.sim_t < 60.0 => PH_PASSAGE,
        _ => PH_TAILS,
    };
}

// ---------------------------------------------------------------- camera

struct Cam {
    eye_t: V3,
    right: V3,
    up: V3,
    fwd: V3,
    dist: f32,
    f: f32,
    hx: f32,
    hy: f32,
}

/// The ship's window in render pixels: x0, y0, x1, y1 (top-left origin).
fn window_px() -> [f32; 4] {
    let s = st();
    let (w, h) = (s.w as f32, s.h as f32);
    [s.win[0] * w, s.win[1] * h, s.win[2] * w, s.win[3] * h]
}

fn camera(dt_real: f32) -> Cam {
    let s = st();
    let (a, b) = (s.cores[0], s.cores[1]);
    let tgt = if b.alive {
        let m = a.v0sq + b.v0sq;
        a.p.mul(a.v0sq / m).add(b.p.mul(b.v0sq / m))
    } else {
        a.p
    };
    let k = 1.0 - expf(-dt_real * 1.2);
    s.target = s.target.add(tgt.sub(s.target).mul(k));
    s.az += dt_real * 0.035;
    s.el_t += dt_real;
    let el = 0.62 + 0.22 * sinf(s.el_t * 0.043);
    let fwd = v3(-cosf(el) * cosf(s.az), -cosf(el) * sinf(s.az), -sinf(el));
    let right = fwd.cross(v3(0.0, 0.0, 1.0)).norm();
    let up = right.cross(fwd);
    // frame the merger inside the focus (the cockpit's sight, or the whole glass), a
    // little above its centre; when the focus moves the camera glides there
    if dt_real == 0.0 {
        s.framed = s.focus;
    } else {
        let k = 1.0 - expf(-dt_real * 1.4);
        for i in 0..4 {
            s.framed[i] = mix(s.framed[i], s.focus[i], k);
        }
    }
    let [x0, y0, x1, y1] = {
        let (w, h) = (s.w as f32, s.h as f32);
        [s.framed[0] * w, s.framed[1] * h, s.framed[2] * w, s.framed[3] * h]
    };
    let (w, h) = ((x1 - x0).max(8.0), (y1 - y0).max(8.0));
    let f = (w * 0.5).min(h * 1.9);
    let d = if b.alive { a.p.sub(b.p) } else { v3(0.0, 0.0, 0.0) };
    let disk = if b.alive { 7.5 } else { 10.5 };
    let ex = d.dot(right).abs() * 0.5 + disk;
    let ey = d.dot(up).abs() * 0.5 + disk * 0.8;
    let want = clampf((f * ex / (0.47 * w)).max(f * ey / (0.44 * h)), 16.0, 90.0);
    s.dist = if dt_real == 0.0 {
        want
    } else {
        mix(s.dist, want, 1.0 - expf(-dt_real * 0.5))
    };
    Cam {
        eye_t: s.target,
        right,
        up,
        fwd,
        dist: s.dist,
        f,
        hx: (x0 + x1) * 0.5,
        hy: y0 + h * 0.44,
    }
}

fn project(cam: &Cam, p: V3) -> Option<(f32, f32, f32)> {
    let rel = p.sub(cam.eye_t);
    let z = rel.dot(cam.fwd) + cam.dist;
    if z < 0.5 {
        return None;
    }
    let inv = 1.0 / z;
    Some((
        cam.hx + cam.f * rel.dot(cam.right) * inv,
        cam.hy - cam.f * rel.dot(cam.up) * inv,
        z,
    ))
}

const GREEN_V: [f32; 3] = [130.0 / 255.0, 226.0 / 255.0, 168.0 / 255.0];
const PINK_V: [f32; 3] = [244.0 / 255.0, 119.0 / 255.0, 172.0 / 255.0];
const GOLD_V: [f32; 3] = [255.0 / 255.0, 207.0 / 255.0, 107.0 / 255.0];

fn rand_dir_seeded(seed: f32) -> V3 {
    let a = seed * 17.0;
    let b = seed * 5.3;
    v3(cosf(a) * cosf(b), sinf(a) * cosf(b), sinf(b))
}

// ---------------------------------------------------------------- GPU
//
// The page hands this module a WebGL2 context through a thin import table ("gl"): each
// import forwards one call and keeps handles as small integers. Everything that decides
// the picture lives here: shaders, buffers, render targets, uniforms and the passes.
//
// Frame: nebula (half res) -> HDR scene (nebula + instanced stars + glints, additive)
// -> bloom (5-level dual filter) -> composite to the screen: gravitational lensing, the
// ship's window with its frame and hull, a gas giant, filmic tone map, grain.

#[link(wasm_import_module = "gl")]
unsafe extern "C" {
    fn gl_log(p: *const u8, n: usize);
    fn gl_program(vs: *const u8, vl: usize, fs: *const u8, fl: usize) -> u32;
    fn gl_uniform(prog: u32, name: *const u8, n: usize) -> u32;
    fn gl_buffer() -> u32;
    fn gl_buffer_data(buf: u32, p: *const u8, bytes: usize);
    fn gl_vao() -> u32;
    /// ty: 0 float, 1 unsigned byte (normalized)
    fn gl_attrib(vao: u32, buf: u32, loc: u32, size: u32, ty: u32, stride: u32, off: u32, divisor: u32);
    /// A colour render target with a texture; hdr asks for RGBA16F. 0 = failed.
    fn gl_target(w: u32, h: u32, hdr: u32) -> u32;
    fn gl_free_target(t: u32);
    /// 0 = the canvas.
    fn gl_bind_target(t: u32, w: u32, h: u32);
    fn gl_use(prog: u32);
    fn gl_bind_vao(vao: u32);
    fn gl_texture(unit: u32, t: u32);
    fn gl_u1i(l: u32, a: i32);
    fn gl_u1f(l: u32, a: f32);
    fn gl_u2f(l: u32, a: f32, b: f32);
    fn gl_u3f(l: u32, a: f32, b: f32, c: f32);
    fn gl_u4f(l: u32, a: f32, b: f32, c: f32, d: f32);
    /// 0 off, 1 additive
    fn gl_blend(mode: u32);
    /// mode: 0 triangles, 1 triangle strip
    fn gl_draw(mode: u32, first: u32, count: u32, instances: u32);
}

fn log(msg: &str) {
    unsafe { gl_log(msg.as_ptr(), msg.len()) }
}

const FULL_VS: &str = "#version 300 es
void main() {
  vec2 p = vec2(float((gl_VertexID << 1) & 2), float(gl_VertexID & 2));
  gl_Position = vec4(p * 2.0 - 1.0, 0.0, 1.0);
}";

const NOISE: &str = "
float h3(vec3 p) { p = fract(p * 0.3183099 + 0.1); p *= 17.0; return fract(p.x * p.y * p.z * (p.x + p.y + p.z)); }
float n3(vec3 x) {
  vec3 i = floor(x); vec3 f = fract(x); f = f * f * (3.0 - 2.0 * f);
  return mix(mix(mix(h3(i), h3(i + vec3(1, 0, 0)), f.x), mix(h3(i + vec3(0, 1, 0)), h3(i + vec3(1, 1, 0)), f.x), f.y),
             mix(mix(h3(i + vec3(0, 0, 1)), h3(i + vec3(1, 0, 1)), f.x), mix(h3(i + vec3(0, 1, 1)), h3(i + vec3(1, 1, 1)), f.x), f.y), f.z);
}
float fbm(vec3 p) { float a = 0.5, s = 0.0; for (int i = 0; i < 5; i++) { s += a * n3(p); p = p * 2.03 + vec3(1.7, 9.2, 3.1); a *= 0.5; } return s; }
float h2(vec2 p) { return fract(sin(dot(p, vec2(127.1, 311.7))) * 43758.5453); }
";

/// Deep space around the merger: emission nebulae, dust lanes and a faint galactic band,
/// looked up by view direction so it turns with the camera.
const NEBULA_FS: &str = "#version 300 es
precision highp float;
uniform vec3 u_r; uniform vec3 u_u; uniform vec3 u_f;
uniform vec4 u_cam;   // focal px, centre x, centre y (render px), target scale
uniform vec2 u_tres;  // this target's size
uniform vec4 u_mood;  // time, fade, calls, workers
out vec4 o;
#NOISE
void main() {
  vec2 p = vec2(gl_FragCoord.x, u_tres.y - gl_FragCoord.y) / u_cam.w;
  float f = u_cam.x * 0.9;
  vec3 d = normalize(u_f + u_r * ((p.x - u_cam.y) / f) - u_u * ((p.y - u_cam.z) / f));
  // the clouds drift on their own, very slowly (minutes to cross), and breathe their hues
  float t = u_mood.x;
  vec3 q = d * 2.3 + vec3(t * 0.0021, t * 0.0013, t * 0.0017);
  vec3 w = vec3(fbm(q), fbm(q + vec3(5.2, 1.3, 2.8)), fbm(q + vec3(2.2, 7.1, 4.4)));
  float n = fbm(q + 2.4 * w);
  float band = exp(-pow(dot(d, normalize(vec3(0.35, 0.55, 0.76))) * 2.6, 2.0));
  float dens = smoothstep(0.34, 0.92, n);
  float dust = smoothstep(0.44, 0.8, fbm(q * 2.7 + w * 1.7));
  vec3 violet = vec3(0.36, 0.09, 0.48), teal = vec3(0.03, 0.36, 0.48), ember = vec3(1.0, 0.46, 0.2);
  vec3 rose = vec3(0.62, 0.14, 0.36), blue = vec3(0.08, 0.16, 0.52);
  vec3 col = mix(violet, teal, smoothstep(0.35, 0.72, w.x));
  col = mix(col, rose, smoothstep(0.5, 0.8, w.z) * (0.55 + 0.2 * sin(t * 0.021)));
  col = mix(col, blue, smoothstep(0.55, 0.85, 1.0 - w.x) * 0.5);
  col = mix(col, ember, smoothstep(0.62, 0.92, w.y) * 0.55);
  vec3 e = col * (dens * dens * 0.46 + band * 0.08 * (0.4 + n));
  e *= 1.0 - 0.8 * dust * (0.4 + 0.6 * band);
  e += vec3(0.004, 0.006, 0.014) + band * vec3(0.012, 0.010, 0.02);
  o = vec4(e * u_mood.y, 1.0);
}";

/// Flight stars: instance i is hashed to a point in a box that wraps around the camera
/// (period L per layer), projected with the galaxy camera and drawn as a short streak
/// from where it was a moment ago to where it is, so they stream past and grow.
const FLY_VS: &str = "#version 300 es
uniform vec3 u_p;   // the ship in the field
uniform vec3 u_v;   // velocity x streak time (world units)
uniform vec3 u_r; uniform vec3 u_u; uniform vec3 u_f;
uniform vec4 u_cam; // focal px, centre x, centre y, px scale
uniform vec4 u_k;   // gain, fade, 0, 0
uniform vec2 u_res;
out vec3 v_c; out vec2 v_q; out vec2 v_lw; out float v_b;
float hash(float n) { return fract(sin(n) * 43758.5453123); }
void main() {
  int i = gl_InstanceID;
  float id = float(i);
  // parallax layers: near (sparse, big), mid, far (dense, small, slow on screen)
  float L = i < 700 ? 36.0 : (i < 2000 ? 90.0 : 240.0);
  float size = i < 700 ? 0.06 : (i < 2000 ? 0.12 : 0.26);
  float lum = i < 700 ? 1.0 : (i < 2000 ? 0.75 : 0.55);
  vec3 h = vec3(hash(id * 12.9898 + 1.3), hash(id * 78.233 + 4.1), hash(id * 37.719 + 7.7));
  vec3 rel = mod(h * L - u_p + 0.5 * L, L) - 0.5 * L;
  vec3 prev = rel + u_v;
  float z = dot(rel, u_f);
  float zp = dot(prev, u_f);
  vec2 corner = vec2(float(gl_VertexID & 1), float((gl_VertexID >> 1) & 1)) * 2.0 - 1.0;
  if (z < 0.35 || zp < 0.35) { gl_Position = vec4(2.0, 2.0, 2.0, 1.0); v_c = vec3(0.0); v_q = vec2(0.0); v_lw = vec2(0.0, 1.0); v_b = 0.0; return; }
  vec2 s0 = u_cam.yz + u_cam.x * vec2(dot(rel, u_r), -dot(rel, u_u)) / z;
  vec2 s1 = u_cam.yz + u_cam.x * vec2(dot(prev, u_r), -dot(prev, u_u)) / zp;
  // nearer is bigger and brighter; it fades in at the layer's far edge and out at the hull
  float r = clamp(u_cam.x * size / z, 0.8 * u_cam.w, 3.2 * u_cam.w);
  float fade = smoothstep(0.5 * L, 0.32 * L, length(rel)) * smoothstep(0.35, 1.6, z);
  vec2 a = s0 - s1;
  float len = length(a);
  vec2 dir = len > 0.01 ? a / len : vec2(1.0, 0.0);
  vec2 nrm = vec2(-dir.y, dir.x);
  float along = corner.x > 0.0 ? len + 2.5 * r : -2.5 * r;
  vec2 s = s1 + dir * along + nrm * corner.y * 2.5 * r;
  v_q = vec2(along, corner.y * 2.5 * r);
  v_lw = vec2(len, r);
  vec3 tint = mix(vec3(0.72, 0.84, 1.0), vec3(1.0, 0.88, 0.72), h.x * h.x);
  v_c = tint;
  v_b = u_k.x * u_k.y * lum * fade * clamp(6.0 / z, 0.08, 4.0);
  gl_Position = vec4(s.x / u_res.x * 2.0 - 1.0, 1.0 - s.y / u_res.y * 2.0, 0.0, 1.0);
}";

const FLY_FS: &str = "#version 300 es
precision highp float;
in vec3 v_c; in vec2 v_q; in vec2 v_lw; in float v_b;
out vec4 o;
void main() {
  float sg = max(v_lw.y * 0.42, 0.45);
  vec2 d = vec2(v_q.x - clamp(v_q.x, 0.0, v_lw.x), v_q.y);
  // a streak keeps the star's light: longer is dimmer per pixel
  float k = 1.0 / (1.0 + v_lw.x / (sg * 4.0));
  o = vec4(v_c * v_b * k * exp(-dot(d, d) / (2.0 * sg * sg)), 1.0);
}";

const BLIT_FS: &str = "#version 300 es
precision highp float;
uniform sampler2D u_src; uniform vec2 u_dst;
out vec4 o;
void main() { o = vec4(texture(u_src, gl_FragCoord.xy / u_dst).rgb, 1.0); }";

/// Galaxy stars: one instanced quad per simulated star, projected here from its 3-D
/// position; a Gaussian footprint that carries a fixed amount of light.
const STAR_VS: &str = "#version 300 es
layout(location = 0) in float a_x;
layout(location = 1) in float a_y;
layout(location = 2) in float a_z;
layout(location = 3) in vec4 a_c;
uniform vec3 u_o; uniform vec3 u_eye; uniform vec3 u_r; uniform vec3 u_u; uniform vec3 u_f;
uniform vec4 u_cam;  // dist, focal, centre x, centre y
uniform vec4 u_k;    // expand, gain, max gain, radius px
uniform vec2 u_res;
out vec3 v_c; out vec2 v_q;
void main() {
  vec2 corner = vec2(float(gl_VertexID & 1), float((gl_VertexID >> 1) & 1)) * 2.0 - 1.0;
  vec3 p = (vec3(a_x, a_y, a_z) - u_o) * u_k.x + (u_o - u_eye);
  float z = dot(p, u_f) + u_cam.x;
  if (z < 0.5) { gl_Position = vec4(2.0, 2.0, 2.0, 1.0); v_c = vec3(0.0); v_q = vec2(0.0); return; }
  float inv = 1.0 / z;
  vec2 s = vec2(u_cam.z + u_cam.y * dot(p, u_r) * inv, u_cam.w - u_cam.y * dot(p, u_u) * inv);
  float b = min(u_k.y * u_cam.x * inv, u_k.z);
  v_q = corner * u_k.w;
  v_c = a_c.rgb * b;
  s += v_q;
  gl_Position = vec4(s.x / u_res.x * 2.0 - 1.0, 1.0 - s.y / u_res.y * 2.0, 0.0, 1.0);
}";

const STAR_FS: &str = "#version 300 es
precision highp float;
in vec3 v_c; in vec2 v_q;
out vec4 o;
void main() {
  float g = exp(-dot(v_q, v_q) * 1.18) * 0.376;  // sigma 0.65 px, unit integral
  o = vec4(v_c * g, 1.0);
}";

/// Glints: background stars, crew beacons, comet trails. Screen-space instances with a
/// soft core and optional four-point diffraction spikes.
const GLINT_VS: &str = "#version 300 es
layout(location = 0) in vec4 a_p;  // x, y, radius, core sigma (px)
layout(location = 1) in vec4 a_c;  // rgb * intensity, spikes
uniform vec2 u_res;
out vec2 v_q; out vec4 v_c; out vec2 v_rs;
void main() {
  vec2 corner = vec2(float(gl_VertexID & 1), float((gl_VertexID >> 1) & 1)) * 2.0 - 1.0;
  v_q = corner * a_p.z; v_c = a_c; v_rs = a_p.zw;
  vec2 s = a_p.xy + v_q;
  gl_Position = vec4(s.x / u_res.x * 2.0 - 1.0, 1.0 - s.y / u_res.y * 2.0, 0.0, 1.0);
}";

const GLINT_FS: &str = "#version 300 es
precision highp float;
in vec2 v_q; in vec4 v_c; in vec2 v_rs;
out vec4 o;
void main() {
  float sg = max(v_rs.y, 0.4);
  float core = exp(-dot(v_q, v_q) / (2.0 * sg * sg)) / (6.2832 * sg * sg);
  vec2 a = abs(v_q);
  float len = v_rs.x * 0.32;
  float spike = exp(-a.y * 1.3) * exp(-a.x / len) + exp(-a.x * 1.3) * exp(-a.y / len);
  float edge = 1.0 - smoothstep(v_rs.x * 0.7, v_rs.x, length(v_q));
  o = vec4(v_c.rgb * (core + v_c.w * spike * 0.05) * edge, 1.0);
}";

/// Bloom: 13-tap downsample (Jimenez 2014), then a tent upsample added back up the chain.
const DOWN_FS: &str = "#version 300 es
precision highp float;
uniform sampler2D u_src; uniform vec2 u_dst; uniform vec2 u_texel; uniform float u_first;
out vec4 o;
vec3 t(vec2 uv) { return texture(u_src, uv).rgb; }
void main() {
  vec2 uv = gl_FragCoord.xy / u_dst; vec2 d = u_texel;
  vec3 a = t(uv + d * vec2(-2, 2)), b = t(uv + d * vec2(0, 2)), c = t(uv + d * vec2(2, 2));
  vec3 e = t(uv + d * vec2(-2, 0)), f = t(uv), g = t(uv + d * vec2(2, 0));
  vec3 h = t(uv + d * vec2(-2, -2)), i = t(uv + d * vec2(0, -2)), j = t(uv + d * vec2(2, -2));
  vec3 k = t(uv + d * vec2(-1, 1)), l = t(uv + d * vec2(1, 1)), m = t(uv + d * vec2(-1, -1)), n = t(uv + d * vec2(1, -1));
  vec3 s = f * 0.125 + (a + c + h + j) * 0.03125 + (b + e + g + i) * 0.0625 + (k + l + m + n) * 0.125;
  if (u_first > 0.5) { float br = max(s.r, max(s.g, s.b)); s *= br / (br + 0.35); }
  o = vec4(max(s, 0.0), 1.0);
}";

const UP_FS: &str = "#version 300 es
precision highp float;
uniform sampler2D u_src; uniform vec2 u_dst; uniform vec2 u_texel;
out vec4 o;
vec3 t(vec2 uv) { return texture(u_src, uv).rgb; }
void main() {
  vec2 uv = gl_FragCoord.xy / u_dst; vec2 d = u_texel;
  vec3 s = t(uv) * 4.0
    + (t(uv + d * vec2(-1, 0)) + t(uv + d * vec2(1, 0)) + t(uv + d * vec2(0, -1)) + t(uv + d * vec2(0, 1))) * 2.0
    + t(uv + d * vec2(-1, -1)) + t(uv + d * vec2(1, -1)) + t(uv + d * vec2(-1, 1)) + t(uv + d * vec2(1, 1));
  o = vec4(s / 16.0, 1.0);
}";

/// The shot: lensing, bloom, the ship's window and hull, a gas giant, tone map.
const COMP_FS: &str = "#version 300 es
precision highp float;
uniform sampler2D u_scene; uniform sampler2D u_bloom; uniform sampler2D u_amb;
uniform vec2 u_res;
uniform vec3 u_bh0; uniform vec3 u_bh1;
uniform vec4 u_wave;  // x, y, age, on
uniform vec4 u_bang;  // x, y, flash, progress (unused: first light has no flash)
uniform vec4 u_win;   // x0, y0, x1, y1 (px, top-left origin)
uniform vec4 u_mood;  // time, calls, workers, still
uniform vec4 u_look;  // exposure, bloom, planet, 0
out vec4 o;
#NOISE
const vec3 GOLD = vec3(1.0, 0.81, 0.42);
const vec3 CYAN = vec3(0.39, 0.92, 0.91);
const vec3 PINK = vec3(0.96, 0.47, 0.67);
const vec3 AMBER = vec3(1.0, 0.55, 0.16);

void lens(vec3 bh, vec2 frag, inout vec2 src, inout float shade, inout vec3 glow) {
  if (bh.z < 0.4) return;
  vec2 d = frag - bh.xy;
  float r2 = max(dot(d, d), 1e-3);
  float r = sqrt(r2);
  float e = bh.z;
  src -= d * (e * e / r2);
  float rs = e * 0.5;
  shade *= smoothstep(rs * 0.9, rs * 1.06, r);
  float w = rs * 0.06 + 0.7;
  glow += mix(GOLD, vec3(1.0), 0.35) * exp(-pow((r - rs * 1.12) / w, 2.0)) * 1.4;
  glow += PINK * exp(-pow((r - rs * 1.5) / (w * 3.0), 2.0)) * 0.12;
}

vec3 scene(vec2 px) { return texture(u_scene, vec2(px.x, u_res.y - px.y) / u_res).rgb; }
vec3 bloom(vec2 px) { return texture(u_bloom, vec2(px.x, u_res.y - px.y) / u_res).rgb; }

float sdBox(vec2 p, vec2 b, float r) { vec2 q = abs(p) - b + r; return length(max(q, 0.0)) + min(max(q.x, q.y), 0.0) - r; }

vec3 aces(vec3 x) { return clamp((x * (2.51 * x + 0.03)) / (x * (2.43 * x + 0.59) + 0.14), 0.0, 1.0); }

vec4 planet(vec2 f, float t) {
  vec2 c = (u_win.xy + u_win.zw) * 0.5, hb = (u_win.zw - u_win.xy) * 0.5;
  bool wide = hb.x > hb.y * 1.15;
  vec2 pc = c + hb * (wide ? vec2(0.66, 1.02) : vec2(0.42, 0.98));
  float R = wide ? hb.y * 0.62 : hb.x * 0.78;
  vec2 q = (f - pc) / R;
  float r = length(q);
  vec3 L = normalize(vec3(-0.75, 0.55, 0.38));
  if (r > 1.0) {
    float halo = exp(-(r - 1.0) * 26.0) * smoothstep(-0.2, 0.6, dot(normalize(vec3(q.x, -q.y, 0.0)), L));
    return vec4(vec3(0.28, 0.55, 1.0) * halo * 0.55, 0.0);
  }
  float z = sqrt(1.0 - r * r);
  vec3 n = vec3(q.x, -q.y, z);
  float ti = 0.35; vec3 m = vec3(n.x * cos(ti) - n.y * sin(ti), n.x * sin(ti) + n.y * cos(ti), n.z);
  float lat = m.y, lon = atan(m.z, m.x) + t * 0.006;
  vec3 sp = vec3(cos(lon) * 1.6, lat * 7.0, sin(lon) * 1.6);
  float w = fbm(sp * vec3(1.0, 0.35, 1.0) + vec3(0.0, fbm(sp * 1.7) * 1.4, 0.0));
  float bands = 0.5 + 0.5 * sin(lat * 17.0 + w * 6.5);
  vec3 col = mix(vec3(0.10, 0.20, 0.30), vec3(0.34, 0.52, 0.60), bands);
  col = mix(col, vec3(0.62, 0.30, 0.36), smoothstep(0.58, 0.82, w) * 0.55);
  col *= 0.85 + 0.3 * fbm(sp * 4.0);
  float ndl = dot(n, L);
  float lit = smoothstep(-0.08, 0.55, ndl);
  vec3 c3 = col * lit * 0.95;
  float fres = pow(1.0 - z, 2.5);
  c3 += vec3(0.25, 0.5, 1.0) * fres * smoothstep(-0.25, 0.4, ndl) * 0.9;
  c3 += vec3(0.02, 0.025, 0.04) * (1.0 - lit);  // night side, lit by the galaxy
  float edge = smoothstep(1.0, 1.0 - 2.0 / R, r);
  return vec4(c3, edge);
}

void main() {
  vec2 frag = vec2(gl_FragCoord.x, u_res.y - gl_FragCoord.y);
  float unit = u_res.y / 400.0;
  float time = u_mood.x;
  vec3 col;
  // the sky, edge to edge: no window frame or hull
  {
    vec2 src = frag; float shade = 1.0; vec3 glow = vec3(0.0);
    lens(u_bh0, frag, src, shade, glow);
    lens(u_bh1, frag, src, shade, glow);
    if (u_wave.w > 0.5) {
      vec2 dd = frag - u_wave.xy;
      float r = length(dd) + 1e-3;
      // a soft ripple, spreading slowly
      float R = u_wave.z * 70.0 * unit;
      float w = 30.0 + R * 0.15;
      float env = exp(-pow((r - R) / w, 2.0)) * exp(-u_wave.z * 0.3);
      float s = sin((r - R) * 0.06 / unit);
      src += dd / r * s * env * 5.0 * unit;
      glow += CYAN * env * max(s, 0.0) * 0.08;
    }
    vec3 c = scene(src) + bloom(src) * u_look.y;
    c = c * shade + glow * shade + glow * 0.25;
    if (u_look.z > 0.5) {
      vec4 pl = planet(frag, time);
      c = c * (1.0 - pl.a) + pl.rgb;
    }
    col = c;
  }
  col *= u_look.x;
  col = aces(col);
  vec2 v = frag / u_res - 0.5;
  col *= 1.0 - 0.28 * dot(v * vec2(0.8, 1.2), v * vec2(0.8, 1.2));
  col = pow(col, vec3(1.0 / 2.2));
  col += (h2(frag + fract(time) * 91.0) - 0.5) * (1.5 / 255.0);
  o = vec4(col, 1.0);
}";

const MAX_GLINTS: usize = BG_STARS + 64 + MAX_COMETS * 90;
const BLOOM_LEVELS: usize = 5;

struct Gpu {
    ok: bool,
    p_neb: u32,
    p_blit: u32,
    p_star: u32,
    p_glint: u32,
    p_down: u32,
    p_up: u32,
    p_comp: u32,
    p_fly: u32,
    l_fly: [u32; 10],
    l_neb: [u32; 7],
    l_blit: [u32; 2],
    l_star: [u32; 9],
    l_glint: [u32; 1],
    l_down: [u32; 4],
    l_up: [u32; 3],
    l_comp: [u32; 12],
    vao_full: u32,
    vao_star: u32,
    vao_glint: u32,
    b_x: u32,
    b_y: u32,
    b_z: u32,
    b_c: u32,
    b_glint: u32,
    t_neb: u32,
    t_hdr: u32,
    bloom: [u32; BLOOM_LEVELS],
    bsize: [(u32, u32); BLOOM_LEVELS],
    neb_size: (u32, u32),
    colors_dirty: bool,
}

static mut G: Gpu = Gpu {
    ok: false,
    p_neb: 0,
    p_blit: 0,
    p_star: 0,
    p_glint: 0,
    p_down: 0,
    p_up: 0,
    p_comp: 0,
    p_fly: 0,
    l_fly: [0; 10],
    l_neb: [0; 7],
    l_blit: [0; 2],
    l_star: [0; 9],
    l_glint: [0; 1],
    l_down: [0; 4],
    l_up: [0; 3],
    l_comp: [0; 12],
    vao_full: 0,
    vao_star: 0,
    vao_glint: 0,
    b_x: 0,
    b_y: 0,
    b_z: 0,
    b_c: 0,
    b_glint: 0,
    t_neb: 0,
    t_hdr: 0,
    bloom: [0; BLOOM_LEVELS],
    bsize: [(0, 0); BLOOM_LEVELS],
    neb_size: (0, 0),
    colors_dirty: true,
};
static mut GLINTS: [[f32; 8]; MAX_GLINTS] = [[0.0; 8]; MAX_GLINTS];
static mut SRC: [u8; 16384] = [0; 16384];

fn gpu() -> &'static mut Gpu {
    unsafe { &mut G }
}

/// Splices the shared noise functions into a shader (no alloc: a static scratch buffer).
fn program(vs: &str, fs: &str) -> u32 {
    let (fs_ptr, fs_len) = match fs.find("#NOISE") {
        None => (fs.as_ptr(), fs.len()),
        Some(at) => unsafe {
            let parts = [&fs.as_bytes()[..at], NOISE.as_bytes(), &fs.as_bytes()[at + 6..]];
            let mut n = 0;
            for p in parts {
                if n + p.len() > SRC.len() {
                    log("shader too long");
                    return 0;
                }
                SRC[n..n + p.len()].copy_from_slice(p);
                n += p.len();
            }
            (SRC.as_ptr(), n)
        },
    };
    unsafe { gl_program(vs.as_ptr(), vs.len(), fs_ptr, fs_len) }
}

fn locs<const N: usize>(p: u32, names: [&str; N]) -> [u32; N] {
    let mut out = [0u32; N];
    for (i, n) in names.iter().enumerate() {
        out[i] = unsafe { gl_uniform(p, n.as_ptr(), n.len()) };
    }
    out
}

fn gpu_setup() -> bool {
    let g = gpu();
    g.p_neb = program(FULL_VS, NEBULA_FS);
    g.p_blit = program(FULL_VS, BLIT_FS);
    g.p_star = program(STAR_VS, STAR_FS);
    g.p_glint = program(GLINT_VS, GLINT_FS);
    g.p_down = program(FULL_VS, DOWN_FS);
    g.p_up = program(FULL_VS, UP_FS);
    g.p_comp = program(FULL_VS, COMP_FS);
    g.p_fly = program(FLY_VS, FLY_FS);
    if [g.p_neb, g.p_blit, g.p_star, g.p_glint, g.p_down, g.p_up, g.p_comp, g.p_fly].contains(&0) {
        return false;
    }
    g.l_fly = locs(g.p_fly, ["u_p", "u_v", "u_r", "u_u", "u_f", "u_cam", "u_k", "u_res", "_", "_"]);
    g.l_neb = locs(g.p_neb, ["u_r", "u_u", "u_f", "u_cam", "u_tres", "u_mood", "_"]);
    g.l_blit = locs(g.p_blit, ["u_src", "u_dst"]);
    g.l_star = locs(g.p_star, ["u_o", "u_eye", "u_r", "u_u", "u_f", "u_cam", "u_k", "u_res", "_"]);
    g.l_glint = locs(g.p_glint, ["u_res"]);
    g.l_down = locs(g.p_down, ["u_src", "u_dst", "u_texel", "u_first"]);
    g.l_up = locs(g.p_up, ["u_src", "u_dst", "u_texel"]);
    g.l_comp = locs(
        g.p_comp,
        ["u_scene", "u_bloom", "u_amb", "u_res", "u_bh0", "u_bh1", "u_wave", "u_bang", "u_win", "u_mood", "u_look", "_"],
    );
    unsafe {
        g.vao_full = gl_vao();
        g.b_x = gl_buffer();
        g.b_y = gl_buffer();
        g.b_z = gl_buffer();
        g.b_c = gl_buffer();
        g.b_glint = gl_buffer();
        g.vao_star = gl_vao();
        gl_attrib(g.vao_star, g.b_x, 0, 1, 0, 4, 0, 1);
        gl_attrib(g.vao_star, g.b_y, 1, 1, 0, 4, 0, 1);
        gl_attrib(g.vao_star, g.b_z, 2, 1, 0, 4, 0, 1);
        gl_attrib(g.vao_star, g.b_c, 3, 4, 1, 4, 0, 1);
        g.vao_glint = gl_vao();
        gl_attrib(g.vao_glint, g.b_glint, 0, 4, 0, 32, 0, 1);
        gl_attrib(g.vao_glint, g.b_glint, 1, 4, 0, 32, 16, 1);
    }
    g.colors_dirty = true;
    true
}

fn gpu_targets(w: u32, h: u32) {
    let g = gpu();
    unsafe {
        for t in [g.t_neb, g.t_hdr].iter().chain(g.bloom.iter()) {
            if *t != 0 {
                gl_free_target(*t);
            }
        }
        g.neb_size = ((w / 2).max(1), (h / 2).max(1));
        g.t_neb = gl_target(g.neb_size.0, g.neb_size.1, 1);
        g.t_hdr = gl_target(w, h, 1);
        for i in 0..BLOOM_LEVELS {
            let s = ((w >> (i + 1)).max(1), (h >> (i + 1)).max(1));
            g.bsize[i] = s;
            g.bloom[i] = gl_target(s.0, s.1, 1);
        }
    }
}

fn u3(l: u32, v: V3) {
    unsafe { gl_u3f(l, v.x, v.y, v.z) }
}

fn fullscreen() {
    unsafe {
        gl_bind_vao(gpu().vao_full);
        gl_draw(0, 0, 3, 1);
    }
}

fn render(dt_real: f32) {
    let s = st();
    let g = gpu();
    if !g.ok || s.w < 4 || s.h < 4 {
        return;
    }
    let cam = camera(dt_real);
    let (w, h) = (s.w as f32, s.h as f32);
    let [wx0, wy0, wx1, wy1] = window_px();
    let win_area = ((wx1 - wx0) * (wy1 - wy0)).max(1.0);
    let mut gain = 0.055 * win_area / (s.n.max(1) as f32) * s.exposure;
    let mut expand = 1.0;
    let flash = 0.0;
    // the sky's brightness and the beacons' follow the epoch's life, so dusk hands over to
    // first light without a jump
    let mut fade = 1.0;
    let mut life = 1.0;
    match s.phase {
        PH_BANG => {
            // first light: the pair brightens out of the dark, unfolding a little
            let t = clampf(s.phase_t / BANG_S, 0.0, 1.0);
            let e = t * t * (3.0 - 2.0 * t);
            expand = 0.9 + 0.1 * e;
            gain *= e;
            fade = 0.65 + 0.35 * e;
            life = e;
        }
        PH_FADE => {
            // dusk: the remnant dims away where it is; the sky dims a little with it
            let t = clampf(s.phase_t / FADE_S, 0.0, 1.0);
            let e = t * t * (3.0 - 2.0 * t);
            gain *= 1.0 - e;
            expand = 1.0 + 0.04 * e;
            fade = 1.0 - e * 0.35;
            life = 1.0 - e;
        }
        _ => {}
    }
    let o = s.origin;
    let clock = s.clock;
    let px_scale = (h / 900.0).max(0.6);

    // ---- flight: the ship flies towards the merger; its heading wanders slowly around
    // it (minutes per swing), so the point the stars stream from drifts across the sky
    let fly_dir = cam
        .fwd
        .add(cam.right.mul(0.16 * sinf(clock * 0.031)))
        .add(cam.up.mul(0.10 * sinf(clock * 0.023 + 1.3)))
        .norm();
    unsafe {
        let step = fly_dir.mul(FLY_SPEED * dt_real);
        for (k, d) in [step.x, step.y, step.z].into_iter().enumerate() {
            let p = FLY_P[k] + d;
            FLY_P[k] = p - FLY_WRAP * floorf(p / FLY_WRAP);
        }
    }

    // ---- glints: background sky, crew beacons, comets
    let mut ng = 0usize;
    let mut push = |x: f32, y: f32, r: f32, sg: f32, c: [f32; 3], k: f32, spikes: f32| {
        if ng >= MAX_GLINTS || x < -r || y < -r || x > w + r || y > h + r {
            return;
        }
        unsafe {
            GLINTS[ng] = [x, y, r, sg, c[0] * k, c[1] * k, c[2] * k, spikes];
        }
        ng += 1;
    };
    for i in 0..BG_STARS {
        let [dx, dy, dz, b] = unsafe { BG[i] };
        let d = v3(dx, dy, dz);
        let z = d.dot(cam.fwd);
        if z < 0.05 {
            continue;
        }
        let x = cam.hx + cam.f * 0.9 * d.dot(cam.right) / z;
        let y = cam.hy - cam.f * 0.9 * d.dot(cam.up) / z;
        // a slow shimmer (periods of 8 to 25 s), never a blink
        let tw = 0.86 + 0.14 * sinf(clock * (0.25 + (i % 7) as f32 * 0.08) + i as f32);
        let c = unsafe { BGC[i] };
        let rgbf = [(c & 255) as f32 / 255.0, ((c >> 8) & 255) as f32 / 255.0, ((c >> 16) & 255) as f32 / 255.0];
        let bright = b > 1.2;
        push(
            x,
            y,
            if bright { 14.0 * px_scale } else { 2.5 },
            0.7 * px_scale.min(1.6),
            rgbf,
            b * tw * 1.1 * fade,
            if bright { b * 0.7 } else { 0.0 },
        );
    }
    let beacon_gain = life;
    let nw = (s.workers as usize).min(12).min(s.nbeacons);
    let nc = (s.calls as usize).min(12).min(s.nbeacons.saturating_sub(12));
    for k in 0..(nw + nc) {
        let idx = if k < nw { s.beacons[k] } else { s.beacons[12 + k - nw] } as usize;
        if idx >= s.n {
            continue;
        }
        let p = unsafe { v3(*s.px.add(idx), *s.py.add(idx), *s.pz.add(idx)) };
        let p = o.add(p.sub(o).mul(expand));
        if let Some((x, y, _)) = project(&cam, p) {
            // beacons breathe slowly (6 to 7 s): workers green, open calls pink
            let (c, pulse) = if k < nw {
                (GREEN_V, 0.85 + 0.2 * sinf(clock * 0.9 + k as f32 * 1.7))
            } else {
                (PINK_V, 0.8 + 0.3 * sinf(clock * 1.1 + k as f32))
            };
            push(x, y, 26.0 * px_scale, 1.3 * px_scale, c, 18.0 * pulse * beacon_gain, 6.0);
        }
    }
    for ci in 0..MAX_COMETS {
        let age = s.comets[ci];
        if age < 0.0 {
            continue;
        }
        let seed = s.comet_seed[ci];
        let (a, b) = (
            s.cores[0].p,
            if s.cores[1].alive {
                s.cores[1].p
            } else {
                s.cores[0].p.add(v3(cosf(seed * 9.0) * 6.0, sinf(seed * 9.0) * 6.0, 1.5))
            },
        );
        let (from, to) = if seed > 0.5 { (a, b) } else { (b, a) };
        let mid = from
            .add(to)
            .mul(0.5)
            .add(v3(0.0, 0.0, 2.5 + from.sub(to).len() * 0.25))
            .add(rand_dir_seeded(seed).mul(2.0));
        let head = clampf(age / COMET_CROSS_S, 0.0, 1.0);
        let cf = clampf((COMET_S - age) / 2.0, 0.0, 1.0) * clampf(age / 1.2, 0.0, 1.0);
        let samples = 90;
        for j in 0..samples {
            let u = head - j as f32 * 0.004;
            if u < 0.0 {
                break;
            }
            let q = from
                .mul((1.0 - u) * (1.0 - u))
                .add(mid.mul(2.0 * u * (1.0 - u)))
                .add(to.mul(u * u));
            if let Some((x, y, _)) = project(&cam, q) {
                let t = 1.0 - j as f32 / samples as f32;
                if j == 0 {
                    push(x, y, 22.0 * px_scale, 1.4 * px_scale, GOLD_V, 30.0 * cf, 5.0);
                } else {
                    push(x, y, 4.0 * px_scale, 1.1 * px_scale, GOLD_V, 3.0 * t * cf, 0.0);
                }
            }
        }
    }

    unsafe {
        // ---- uploads
        gl_buffer_data(g.b_x, s.px as *const u8, s.n * 4);
        gl_buffer_data(g.b_y, s.py as *const u8, s.n * 4);
        gl_buffer_data(g.b_z, s.pz as *const u8, s.n * 4);
        if g.colors_dirty || COLORS_DIRTY {
            gl_buffer_data(g.b_c, s.col as *const u8, s.cap * 4);
            g.colors_dirty = false;
            COLORS_DIRTY = false;
        }
        gl_buffer_data(g.b_glint, GLINTS.as_ptr() as *const u8, ng * 32);

        // ---- nebula, half resolution
        let (nw_, nh_) = g.neb_size;
        gl_bind_target(g.t_neb, nw_, nh_);
        gl_blend(0);
        gl_use(g.p_neb);
        let l = g.l_neb;
        u3(l[0], cam.right);
        u3(l[1], cam.up);
        u3(l[2], cam.fwd);
        gl_u4f(l[3], cam.f, cam.hx, cam.hy, nw_ as f32 / w);
        gl_u2f(l[4], nw_ as f32, nh_ as f32);
        gl_u4f(l[5], clock, fade, s.calls as f32, s.workers as f32);
        fullscreen();

        // ---- HDR scene
        gl_bind_target(g.t_hdr, s.w as u32, s.h as u32);
        gl_use(g.p_blit);
        gl_texture(0, g.t_neb);
        gl_u1i(g.l_blit[0], 0);
        gl_u2f(g.l_blit[1], w, h);
        fullscreen();
        gl_blend(1);
        gl_use(g.p_star);
        let l = g.l_star;
        u3(l[0], o);
        u3(l[1], cam.eye_t);
        u3(l[2], cam.right);
        u3(l[3], cam.up);
        u3(l[4], cam.fwd);
        gl_u4f(l[5], cam.dist, cam.f, cam.hx, cam.hy);
        gl_u4f(l[6], expand, gain, gain * 3.0, 2.2);
        gl_u2f(l[7], w, h);
        gl_bind_vao(g.vao_star);
        gl_draw(1, 0, 4, s.n as u32);
        gl_use(g.p_glint);
        gl_u2f(g.l_glint[0], w, h);
        gl_bind_vao(g.vao_glint);
        if ng > 0 {
            gl_draw(1, 0, 4, ng as u32);
        }
        // flight: stars of the near field stream past towards the camera
        gl_use(g.p_fly);
        let l = g.l_fly;
        gl_u3f(l[0], FLY_P[0], FLY_P[1], FLY_P[2]);
        // the streak: where each star was 1/24 s ago (none while held still)
        let blur = if dt_real > 0.0 { FLY_SPEED / 24.0 } else { 0.0 };
        u3(l[1], fly_dir.mul(blur));
        u3(l[2], cam.right);
        u3(l[3], cam.up);
        u3(l[4], cam.fwd);
        gl_u4f(l[5], cam.f, cam.hx, cam.hy, px_scale);
        gl_u4f(l[6], 1.4 * s.exposure, fade, 0.0, 0.0);
        gl_u2f(l[7], w, h);
        gl_bind_vao(g.vao_full);
        gl_draw(1, 0, 4, FLY_N);

        // ---- bloom
        gl_blend(0);
        gl_use(g.p_down);
        gl_u1i(g.l_down[0], 0);
        let mut src = g.t_hdr;
        let mut src_size = (s.w as u32, s.h as u32);
        for i in 0..BLOOM_LEVELS {
            let (bw, bh) = g.bsize[i];
            gl_bind_target(g.bloom[i], bw, bh);
            gl_texture(0, src);
            gl_u2f(g.l_down[1], bw as f32, bh as f32);
            gl_u2f(g.l_down[2], 1.0 / src_size.0 as f32, 1.0 / src_size.1 as f32);
            gl_u1f(g.l_down[3], if i == 0 { 1.0 } else { 0.0 });
            fullscreen();
            src = g.bloom[i];
            src_size = (bw, bh);
        }
        gl_blend(1);
        gl_use(g.p_up);
        gl_u1i(g.l_up[0], 0);
        for i in (0..BLOOM_LEVELS - 1).rev() {
            let (bw, bh) = g.bsize[i];
            let (sw, sh) = g.bsize[i + 1];
            gl_bind_target(g.bloom[i], bw, bh);
            gl_texture(0, g.bloom[i + 1]);
            gl_u2f(g.l_up[1], bw as f32, bh as f32);
            gl_u2f(g.l_up[2], 1.0 / sw as f32, 1.0 / sh as f32);
            fullscreen();
        }

        // ---- composite
        gl_blend(0);
        gl_bind_target(0, s.w as u32, s.h as u32);
        gl_use(g.p_comp);
        let l = g.l_comp;
        gl_texture(0, g.t_hdr);
        gl_texture(1, g.bloom[0]);
        gl_texture(2, g.bloom[BLOOM_LEVELS - 1]);
        gl_u1i(l[0], 0);
        gl_u1i(l[1], 1);
        gl_u1i(l[2], 2);
        gl_u2f(l[3], w, h);
    }

    // black holes for the lensing, and numbers for the page
    let mut info = [0f32; 32];
    info[0] = s.phase as f32;
    info[1] = flash;
    for (k, c) in s.cores.iter().enumerate() {
        let base = 2 + k * 3;
        if c.alive || (k == 0) {
            let p = o.add(c.p.sub(o).mul(expand));
            if let Some((x, y, z)) = project(&cam, p) {
                info[base] = x;
                info[base + 1] = y;
                let lens = if s.phase == PH_BANG {
                    clampf(s.phase_t / BANG_S, 0.0, 1.0)
                } else if s.phase == PH_FADE {
                    1.0 - clampf(s.phase_t / FADE_S, 0.0, 1.0)
                } else {
                    1.0
                };
                info[base + 2] = cam.f * 1.9 * sqrtf(c.mbh) / z * lens;
            }
        }
    }
    info[8] = s.wave_age;
    if s.wave_age >= 0.0 {
        if let Some((x, y, _)) = project(&cam, s.wave_pos) {
            info[9] = x;
            info[10] = y;
        }
    }
    info[11] = if s.phase == PH_BANG { s.phase_t / BANG_S } else { 1.0 };
    info[12] = s.sim_t;
    info[13] = s.n as f32;
    info[14] = s.epoch as f32;
    info[15] = separation();
    if let Some((x, y, _)) = project(&cam, o) {
        info[16] = x;
        info[17] = y;
    }
    info[18] = ng as f32;
    s.info = info;
    let l = g.l_comp;
    unsafe {
        gl_u3f(l[4], info[2], info[3], info[4]);
        gl_u3f(l[5], info[5], info[6], info[7]);
        gl_u4f(l[6], info[9], info[10], info[8].max(0.0), if info[8] >= 0.0 && info[8] < 16.0 { 1.0 } else { 0.0 });
        gl_u4f(l[7], info[16], info[17], info[1], info[11]);
        gl_u4f(l[8], wx0, wy0, wx1, wy1);
        gl_u4f(l[9], clock, s.calls as f32, s.workers as f32, if s.still { 1.0 } else { 0.0 });
        gl_u4f(l[10], 1.0, 0.4, 0.0, 0.0);
        fullscreen();
    }
}

static mut COLORS_DIRTY: bool = true;

// ---------------------------------------------------------------- exports

/// Allocates room for `cap` stars and starts a new epoch. Call `resize` after.
#[no_mangle]
pub extern "C" fn init(cap: u32, seed: u32) -> u32 {
    let s = st();
    let cap = ((cap as usize).max(64) + 3) & !3;
    s.top = unsafe { &__heap_base as *const u8 as usize };
    s.cap = cap;
    s.px = alloc(cap * 4) as *mut f32;
    s.py = alloc(cap * 4) as *mut f32;
    s.pz = alloc(cap * 4) as *mut f32;
    s.vx = alloc(cap * 4) as *mut f32;
    s.vy = alloc(cap * 4) as *mut f32;
    s.vz = alloc(cap * 4) as *mut f32;
    s.col = alloc(cap * 4) as *mut u32;
    s.part_end = s.top;
    s.w = 0;
    s.h = 0;
    s.n = cap;
    unsafe {
        RNG = seed | 1;
    }
    gen_background();
    new_epoch(seed);
    gpu().colors_dirty = true;
    cap as u32
}

/// Compiles the shaders and builds the buffers on the page's WebGL2 context. 1 = ready.
#[no_mangle]
pub extern "C" fn gpu_init() -> u32 {
    let ok = gpu_setup();
    gpu().ok = ok;
    ok as u32
}

/// Render size in pixels (re-creates the render targets).
#[no_mangle]
pub extern "C" fn resize(w: u32, h: u32) -> u32 {
    let s = st();
    s.w = (w as usize).max(4);
    s.h = (h as usize).max(4);
    if gpu().ok {
        gpu_targets(s.w as u32, s.h as u32);
    }
    1
}

/// Where the page's glass is, as fractions of the canvas (kept for the page's API).
#[no_mangle]
pub extern "C" fn set_window(x0: f32, y0: f32, x1: f32, y1: f32) {
    // The sky fills the whole canvas now (no window frame); the rect only seeds where
    // the galaxy is framed until the page sends a focus.
    let s = st();
    s.win = [0.0, 0.0, 1.0, 1.0];
    if x1 - x0 > 0.05 && y1 - y0 > 0.05 && !s.focus_set {
        s.focus = [x0, y0, x1, y1];
        s.framed = s.focus;
    }
}

/// Where to frame the galaxy, as fractions of the canvas (the cockpit's clear sight, or
/// the whole glass while the cockpit is folded away). The camera glides there.
#[no_mangle]
pub extern "C" fn set_focus(x0: f32, y0: f32, x1: f32, y1: f32) {
    let s = st();
    if x1 - x0 > 0.05 && y1 - y0 > 0.05 {
        s.focus = [x0, y0, x1, y1];
        if !s.focus_set {
            s.framed = s.focus;
            s.focus_set = true;
        }
    }
}

/// How many of the allocated stars to simulate and draw (adaptive quality).
#[no_mangle]
pub extern "C" fn set_count(n: u32) -> u32 {
    let s = st();
    s.n = ((n as usize).clamp(64, s.cap)) & !3;
    s.n as u32
}

/// Advance real time `dt` seconds and draw one frame.
#[no_mangle]
pub extern "C" fn frame(dt: f32) -> u32 {
    let dt = clampf(dt, 0.0, 0.1);
    advance(dt);
    render(dt);
    1
}

/// Run the simulation forward without drawing, then hold still (reduced motion).
#[no_mangle]
pub extern "C" fn presim(units: f32) {
    let s = st();
    s.phase = PH_APPROACH;
    s.phase_t = 0.0;
    let mut t = 0.0;
    while t < units && s.cores[1].alive {
        step_stars(DT);
        step_cores(DT);
        s.sim_t += DT;
        t += DT;
    }
    s.phase = PH_TAILS;
    s.dist = 34.0;
    let (a, b) = (s.cores[0], s.cores[1]);
    let m = a.v0sq + b.v0sq;
    s.target = a.p.mul(a.v0sq / m).add(b.p.mul(b.v0sq / m));
}

/// Reduced motion: the accent lights stop pulsing.
#[no_mangle]
pub extern "C" fn set_still(on: u32) {
    st().still = on != 0;
}

/// Live system signals: running workers and open captain calls.
#[no_mangle]
pub extern "C" fn signals(workers: u32, calls: u32) {
    let s = st();
    s.workers = workers;
    s.calls = calls;
}

/// A seat move, swap or handoff happened: streak a comet across the sky.
#[no_mangle]
pub extern "C" fn comet() {
    let s = st();
    for k in 0..MAX_COMETS {
        if s.comets[k] < 0.0 {
            s.comets[k] = 0.0;
            s.comet_seed[k] = rnd();
            return;
        }
    }
}

/// Exposure multiplier.
#[no_mangle]
pub extern "C" fn set_exposure(k: f32) {
    st().exposure = k;
}

/// Pointer to 32 f32s: phase, flash, two black holes (x, y, einstein radius px), wave age,
/// wave x, y, bang progress, sim time, star count, epoch, separation, origin x, y, glints.
#[no_mangle]
pub extern "C" fn info() -> u32 {
    st().info.as_ptr() as u32
}
