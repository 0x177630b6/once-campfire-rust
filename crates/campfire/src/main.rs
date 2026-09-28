//! The Campfire server: controllers, channels, jobs and integrations wired over the crates.

mod active_storage;
mod app;
mod channels;
mod concerns;
mod config;
mod controllers;
mod integrations;
mod jobs;
mod rich_text;

/// jemalloc: the room page alone makes thousands of allocations per request, across as many
/// threads as the blocking pool grows to.
#[global_allocator]
static GLOBAL: tikv_jemallocator::Jemalloc = tikv_jemallocator::Jemalloc;

fn main() -> anyhow::Result<()> {
    app::run()
}
