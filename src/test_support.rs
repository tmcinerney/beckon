//! Helpers shared by unit tests.

use std::{
    path::PathBuf,
    sync::atomic::{AtomicU64, Ordering},
    time::{SystemTime, UNIX_EPOCH},
};

/// A temp path no other test in any concurrent test process will be handed.
///
/// AIDEV-NOTE: the counter is what makes this collision-free. Tests run in
/// parallel threads of one process, and a pid + wall-clock name collided there:
/// macOS clocks tick in microseconds, so two threads read the same "nanos".
/// The pid separates concurrent processes; the timestamp only keeps a later
/// run with a recycled pid clear of anything a crashed run left behind.
pub(crate) fn unique_temp_path(name: &str) -> PathBuf {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let sequence = NEXT.fetch_add(1, Ordering::Relaxed);
    let started = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_micros();
    std::env::temp_dir().join(format!(
        "beckon-{name}-{}-{started}-{sequence}",
        std::process::id()
    ))
}
