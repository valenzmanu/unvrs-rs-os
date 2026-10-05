//! DrvIntf: the Ratatui operator console. A client of the headless kernel (0.7): the PID
//! table, bus socket, mailboxes and handoff live in `uke`; this crate renders and steers.
mod bridge;
pub use bridge::{handoff_smoke, run};
mod comms;
mod console;
mod diagram;
mod game;
mod logs;
mod menu;
mod nav;
mod nest;
mod session;
mod slash;
mod theme;
mod ui;

mod world;
