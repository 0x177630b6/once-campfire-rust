//! The Campfire server: controllers, channels, jobs and integrations wired over the crates.
//!
//! Ownership (see AGENTS.md): app core owns `main.rs`, `app.rs`, `config.rs`, `concerns/`,
//! `active_storage/`, `jobs/`, `rich_text.rs` and `controllers/mod.rs`; channels agent owns `channels/`;
//! integrations agent owns `integrations/`. Controller modules under `controllers/` are
//! assigned per wave.

mod active_storage;
mod app;
mod channels;
mod concerns;
mod config;
mod controllers;
mod integrations;
mod jobs;
mod rich_text;

fn main() -> anyhow::Result<()> {
    app::run()
}
