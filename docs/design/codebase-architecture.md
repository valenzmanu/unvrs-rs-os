# Codebase architecture (Locked)

**Status:** Locked direction · 2026-09-21. Directory layout amended 2026-09-22 for voyage 0.6: modules are siblings at the repo root. One front file per module.  
**Behaviour SoT:** [uke-design.md](./uke-design.md). This doc is crate/layout only — not behaviour law.

Product source lives at the **repo root**. `behaviour-eval/` evaluates a **tagged** snapshot; it does not own the implementation.

---

## Product vs eval

| Tree | Role |
| --- | --- |
| `uke/`, `drv_*/`, `unvrs/` | Product source. Siblings at the repo root. |
| `docs/design/` | Behaviour + BSLA + this layout |
| `behaviour-eval/` | LEARN, smokes, journals, host configs — **pins a git tag / Cargo version** |
| `legacy/` | Reference only |

**Versioning**

| Thing | Form |
| --- | --- |
| Git tag / Cargo | `vX.Y.Z` / `X.Y.Z` |
| Voyage (BSLA eval) | `X.Y.Z-eval.N` — evaluates that tagged code |

```
  tag vX.Y.Z  ──build──►  unvrs/
       ▲
       │ pin
  voyage X.Y.Z-eval.N  ──smokes + LEARN──► verdict
```

---

## Crate map (names)

Rust packages are `snake_case`. Design names stay `uKe` / `DrvAgent` / …

```
unvrs-rs/
├── Cargo.toml                 # workspace list only
├── uke/                       # uKe. src/lib.rs is the interface
├── drv_agent/
├── drv_hdff/
├── drv_obs/
├── drv_intf/
├── unvrs/                     # the one process. src/main.rs
├── docs/design/
└── behaviour-eval/            # no product src
```

`src/lib.rs` is the only front door of `uke` and of each driver. Further files are private implementation, added when `lib.rs` is too big to read. No `entry.rs` beside `lib.rs`. No `main.rs` in a driver. The bus is the seam callers cross. Tests cross that same seam.

```
  unvrs/src/main.rs
       │
       ▼
     uke  (PID · mail · MissionRegistry · memory interface)
       │
       ├── drv_hdff
       ├── drv_agent ── harness CPUs (pi|codex|claude|cursor)
       ├── drv_obs
       └── drv_intf
```

| Crate | Design name | Owns |
| --- | --- | --- |
| `uke` | **uKe** | PID table, mailboxes, MissionRegistry, memory interface, spawn policy |
| `drv_agent` | **DrvAgent** | Bind PID → harness CPU; catalogue install |
| `drv_hdff` | **DrvHdff** | Handoff; later swap + rehydrate |
| `drv_obs` | **DrvObs** | Bus / roster truth |
| `drv_intf` | **DrvIntf** | Captain channel: bridge rendering into host harnesses; Ratatui operator console (uke-design v1 §7) |
| `unvrs` | process | `fn main`. Wire the crates. `ctl` / doctor / cockpit entry |

**Deferred crates** (stub inside `uke` until a real second adapter earns a seam): `drv_boot`, `drv_auth`, `uos`. DrvEcon is implemented as `uke::drv_econ`: deterministic policy/profile routing with catalog discovery in `drv_agent` and admission/receipt ownership in the kernel.

---

## Design principles (for this layout)

- **Deep modules:** small interface, lots of behaviour behind it (callers and tests cross the same seam).
- **Real seams only:** one adapter = keep internal; two+ adapters = crate/seam (DrvAgent already qualifies).
- **Drivers talk through uKe** — no private driver↔driver back-channels (see uke-design).

---

## Current vs target

| Today | Target |
| --- | --- |
| Packages under `crates/` and `bins/unvrs` | Siblings at the repo root, as above. Voyage 0.6 does this move. |
| `drv_intf/src/bridge.rs` still holds the flight loop | New commands, including memory, are on `uke`'s interface. The cockpit calls that interface. |
| Eval pins a tag | `cargo install --path unvrs` |

---

## Related

- [uke-design.md](./uke-design.md) — behaviour  
- [construction-method.md](./construction-method.md) — BSLA / SemVer eval ids  
- [build-status-2026-09-21.md](./build-status-2026-09-21.md) — built vs missing  
