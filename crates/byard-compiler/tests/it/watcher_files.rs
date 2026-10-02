//! The dev watcher and the files a project is built from besides its sources
//! (RFC-0022 §5's seed image): matched by path whatever their extension,
//! seen when removed, and followed when the manifest names a new one.
//!
//! Against the real file system and `notify`, so each wait is a poll with a
//! generous deadline rather than a fixed sleep.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use byard_compiler::interp::reload::{LatestWins, ParsedFile, start_watcher};

fn scratch(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("byard-watchfiles-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir.canonicalize().unwrap()
}

/// Waits until `count` passes `before`, or fails after a generous deadline.
fn wait_past(count: &AtomicUsize, before: usize, what: &str) {
    let deadline = Instant::now() + Duration::from_secs(15);
    while count.load(Ordering::SeqCst) <= before {
        assert!(Instant::now() < deadline, "{what}: no re-read");
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// Lets the watcher settle and drain any events still in flight, so the next
/// count is caused by the next change.
fn settle(count: &AtomicUsize) -> usize {
    std::thread::sleep(Duration::from_millis(700));
    count.load(Ordering::SeqCst)
}

fn write(path: &Path, bytes: &[u8]) {
    std::fs::write(path, bytes).unwrap();
}

#[test]
fn seed_images_are_followed_by_path_through_every_change() {
    let dir = scratch("seed");
    let (project, elsewhere, moved) = (dir.join("app"), dir.join("art"), dir.join("art2"));
    for d in [&project, &elsewhere, &moved] {
        std::fs::create_dir_all(d).unwrap();
    }
    let inside = project.join("brand.asset");
    let outside = elsewhere.join("logo.bin");
    let next = moved.join("logo2.bin");
    for f in [&inside, &outside, &next] {
        write(f, b"v1");
    }

    // What the manifest names after each re-read: switched to `next` once
    // `names_next` is set.
    let reads = Arc::new(AtomicUsize::new(0));
    let names_next = Arc::new(Mutex::new(false));
    let counted = Arc::clone(&reads);
    let switched = Arc::clone(&names_next);
    let named_then = vec![inside.clone(), outside.clone()];
    let watched_at_start = named_then.clone();
    let named_after = vec![inside.clone(), next.clone()];
    let (assets, _assets_rx) = crossbeam_channel::unbounded();
    let _watching = start_watcher(
        std::slice::from_ref(&project),
        &watched_at_start,
        Arc::new(LatestWins::<ParsedFile>::new()),
        assets,
        move || {
            counted.fetch_add(1, Ordering::SeqCst);
            let names_next = *switched.lock().unwrap();
            ParsedFile {
                views: Vec::new(),
                errors: Vec::new(),
                theme: None,
                watch_files: if names_next {
                    named_after.clone()
                } else {
                    named_then.clone()
                },
            }
        },
    )
    .unwrap();

    // Inside the project, with an extension nothing else re-reads on.
    let before = settle(&reads);
    write(&inside, b"v2");
    wait_past(
        &reads,
        before,
        "an odd-extension seed image inside the project",
    );

    // Outside the project, watched through its directory.
    let before = settle(&reads);
    write(&outside, b"v2");
    wait_past(&reads, before, "a seed image outside the project");

    // Removed: the manifest now names a file that is gone, which is an error
    // the re-read has to surface. (On macOS FSEvents also reports a removal
    // as a modification; on Linux it is only a `Remove`, which is what this
    // pins.)
    let before = settle(&reads);
    std::fs::remove_file(&outside).unwrap();
    wait_past(&reads, before, "a removed seed image");

    // The manifest moves to a new image elsewhere: once a re-read names it,
    // its own changes are seen.
    *names_next.lock().unwrap() = true;
    let before = settle(&reads);
    write(&inside, b"v3"); // any change, so the re-read that names `next` runs
    wait_past(&reads, before, "the re-read naming the new image");
    let before = settle(&reads);
    write(&next, b"v2");
    wait_past(&reads, before, "the image the manifest moved to");

    let _ = std::fs::remove_dir_all(&dir);
}
