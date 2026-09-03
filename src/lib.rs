//! Core logic for viscous, a VISCA PTZ camera control program.
//!
//! Everything that decides what the camera is asked to do lives here — the
//! transports and the connection handshake, the movement model, the response
//! curve, the key bindings, the controller mapping and the worker thread that
//! owns the wire. The front ends are thin by design: the window
//! (`egui_gui/`), the full-screen terminal ([`app`]) and the bare command mode
//! ([`cli`]) all drive the same [`worker::Intent`]s, so none of them can
//! quietly grow its own idea of how the camera behaves.

pub mod app;
pub mod cli;
pub mod config;
pub mod connection;
pub mod deflection;
pub mod drives;
pub mod focus;
pub mod gamepad;
pub mod keymap;
pub mod nudge;
pub mod pan_tilt;
pub mod path;
pub mod power;
pub mod preset;
pub mod session;
pub mod shot;
pub mod state;
pub mod title;
pub mod ui;
pub mod worker;
pub mod zoom;
