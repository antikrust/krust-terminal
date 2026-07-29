//! Local model support via Ollama.
//!
//! Bork already ships an Ollama *harness* (see
//! `ai::agent_sdk::driver::harness::ollama_stream_exec`) that runs two
//! hardcoded code models. This module is the user-facing half: which models
//! exist, which are installed, whether one stands a chance on this machine,
//! and pulling the ones you choose.
//!
//! The division of responsibility is deliberate — Ollama owns the models and
//! the disk they occupy. We surface what it offers and warn about what looks
//! unwise; we never veto a download.

pub mod capability;
pub mod catalog;
pub mod remote;
pub mod service;
