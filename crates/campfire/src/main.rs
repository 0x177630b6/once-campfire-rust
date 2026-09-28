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

/// jemalloc's options (`malloc_conf`, under tikv-jemallocator's `_rjem_` prefix), read when it
/// starts, before `main`: no transparent huge pages for its regions either (see
/// [`disable_transparent_huge_pages`]).
#[unsafe(export_name = "_rjem_malloc_conf")]
pub static JEMALLOC_CONF: &std::ffi::CStr = c"thp:never";

fn main() -> anyhow::Result<()> {
    disable_transparent_huge_pages();
    app::run()
}

/// On kernels with transparent huge pages set to `always` (Debian's and Arch's default), every
/// thread's 2 MB stack and each of jemalloc's regions get backed by whole 2 MB pages as soon as
/// they're touched: an idle server took 160 MB on 32 cores instead of 18 MB. Nothing here is big
/// enough to gain from huge pages, so the process (and ffmpeg, which inherits it) opts out.
fn disable_transparent_huge_pages() {
    // SAFETY: PR_SET_THP_DISABLE takes plain integer arguments and only changes this process's
    // memory policy.
    unsafe {
        libc::prctl(libc::PR_SET_THP_DISABLE, 1, 0, 0, 0);
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn transparent_huge_pages_are_disabled() {
        super::disable_transparent_huge_pages();
        // SAFETY: as above.
        assert_eq!(unsafe { libc::prctl(libc::PR_GET_THP_DISABLE, 0, 0, 0, 0) }, 1);
    }
}
