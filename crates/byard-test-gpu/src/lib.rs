//! One GPU device per test process, used by one test at a time.
//!
//! `cargo test` runs a binary's tests on parallel threads of one process.
//! Every GPU test used to build its own `wgpu::Instance`, adapter and device,
//! so a binary with eight GPU tests had eight D3D12 devices being created,
//! used and destroyed at once. On the Windows CI runner that process
//! occasionally died with `STATUS_ACCESS_VIOLATION` (0xc0000005) before any
//! test in it reported: twice in a hundred runs, in two different binaries,
//! each time a binary that passed unchanged, byte for byte, on the rerun. No
//! upload in the engine declares a layout larger than the data it passes, and
//! the engine itself never holds more than one device, so the crash lives in
//! the one thing only the tests did: several instances and devices alive in
//! one process at the same time.
//!
//! A debug build makes that worse on D3D12 in particular. Each new instance
//! turns the debug layer on again (`ID3D12Debug::EnableDebugLayer`, process
//! wide), and turning it on while another device exists is documented to
//! remove that device.
//!
//! So a test process now has exactly one instance, one adapter and one device,
//! created on first use and kept until the process exits, and a test holds a
//! [`Turn`] for as long as it touches the GPU. That removes the concurrent
//! creation, the concurrent teardown, and the concurrent use, and it is faster
//! than it was: a software adapter spends about a second creating a device,
//! and that second is now paid once per binary rather than once per test.
//!
//! A `Turn` must be bound to a name that lives as long as the test does:
//!
//! ```ignore
//! let Some((device, queue, _turn)) = byard_test_gpu::device(limits) else {
//!     return;
//! };
//! ```
//!
//! `_turn` keeps it; a bare `_` would drop it at the end of the statement and
//! let the next test onto the GPU while this one is still drawing.

use std::marker::PhantomData;
use std::sync::{Arc, Condvar, Mutex, OnceLock, PoisonError};
use std::thread::{self, ThreadId};

/// Exclusive use of the process's GPU, released on drop.
///
/// Re-entrant on one thread, so a helper that takes a turn can be called from
/// a test that already holds one. Not `Send`: a turn is released by the thread
/// that took it.
#[must_use = "the turn is released as soon as it is dropped"]
pub struct Turn {
    _owner_thread: PhantomData<*const ()>,
}

struct Holder {
    thread: Option<ThreadId>,
    depth: usize,
}

static HOLDER: Mutex<Holder> = Mutex::new(Holder {
    thread: None,
    depth: 0,
});
static RELEASED: Condvar = Condvar::new();

/// Waits until no other thread holds the GPU, then holds it.
pub fn turn() -> Turn {
    let me = thread::current().id();
    // A test that panicked while holding the lock leaves nothing half-written
    // behind: the two fields are updated together under it.
    let mut holder = HOLDER.lock().unwrap_or_else(PoisonError::into_inner);
    while holder.thread.is_some_and(|t| t != me) {
        holder = RELEASED
            .wait(holder)
            .unwrap_or_else(PoisonError::into_inner);
    }
    holder.thread = Some(me);
    holder.depth += 1;
    Turn {
        _owner_thread: PhantomData,
    }
}

impl Drop for Turn {
    fn drop(&mut self) {
        let mut holder = HOLDER.lock().unwrap_or_else(PoisonError::into_inner);
        holder.depth -= 1;
        if holder.depth == 0 {
            holder.thread = None;
            RELEASED.notify_one();
        }
    }
}

struct Shared {
    // Kept alive for as long as the adapter and device are, which is the life
    // of the process.
    _instance: wgpu::Instance,
    adapter: wgpu::Adapter,
    device: OnceLock<Option<(Arc<wgpu::Device>, Arc<wgpu::Queue>)>>,
}

static SHARED: OnceLock<Option<Shared>> = OnceLock::new();

fn shared() -> Option<&'static Shared> {
    SHARED
        .get_or_init(|| {
            let instance = wgpu::Instance::new(
                wgpu::InstanceDescriptor::new_without_display_handle_from_env(),
            );
            let adapter = pollster::block_on(
                instance.request_adapter(&wgpu::RequestAdapterOptions::default()),
            )
            .ok()?;
            Some(Shared {
                _instance: instance,
                adapter,
                device: OnceLock::new(),
            })
        })
        .as_ref()
}

/// The process's adapter, for a test that needs a device other than the
/// shared one (a feature the shared device does not request, say). `None`
/// when the machine has no adapter, which a test treats as a skip.
///
/// The device the caller requests from it must not outlive the turn.
#[must_use]
pub fn adapter() -> Option<(&'static wgpu::Adapter, Turn)> {
    let turn = turn();
    Some((&shared()?.adapter, turn))
}

/// The process's shared device and queue, with the turn that entitles the
/// caller to use them. `None` when the machine has no adapter, which a test
/// treats as a skip.
///
/// `limits` decides the device's limits the first time any test asks, and is
/// ignored after that; every caller passes the engine's own
/// `byard_core::engine::device_limits`, so a test runs the pipelines the
/// engine runs. No optional features are requested.
#[must_use]
pub fn device(
    limits: fn(&wgpu::Adapter) -> wgpu::Limits,
) -> Option<(Arc<wgpu::Device>, Arc<wgpu::Queue>, Turn)> {
    let turn = turn();
    let shared = shared()?;
    let (device, queue) = shared
        .device
        .get_or_init(|| {
            pollster::block_on(shared.adapter.request_device(&wgpu::DeviceDescriptor {
                label: Some("Byard - Shared Test Device"),
                required_features: wgpu::Features::empty(),
                required_limits: limits(&shared.adapter),
                memory_hints: wgpu::MemoryHints::Performance,
                ..Default::default()
            }))
            .ok()
            .map(|(device, queue)| (Arc::new(device), Arc::new(queue)))
        })
        .as_ref()?;
    Some((Arc::clone(device), Arc::clone(queue), turn))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Duration;

    #[test]
    fn no_two_threads_hold_a_turn_at_once() {
        static INSIDE: AtomicUsize = AtomicUsize::new(0);
        static MOST: AtomicUsize = AtomicUsize::new(0);
        let threads: Vec<_> = (0..8)
            .map(|_| {
                thread::spawn(|| {
                    for _ in 0..20 {
                        let _turn = turn();
                        let now = INSIDE.fetch_add(1, Ordering::SeqCst) + 1;
                        MOST.fetch_max(now, Ordering::SeqCst);
                        thread::sleep(Duration::from_micros(200));
                        INSIDE.fetch_sub(1, Ordering::SeqCst);
                    }
                })
            })
            .collect();
        for t in threads {
            t.join().unwrap();
        }
        assert_eq!(MOST.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn a_thread_that_holds_a_turn_can_take_another() {
        // A helper that takes a turn, called from a test that already holds
        // one, must not wait on itself.
        let outer = turn();
        let inner = turn();
        drop(inner);
        // Still held: another thread cannot get in until `outer` goes.
        let other = thread::spawn(|| {
            let _turn = turn();
        });
        thread::sleep(Duration::from_millis(20));
        assert!(!other.is_finished(), "the outer turn was released early");
        drop(outer);
        other.join().unwrap();
    }

    #[test]
    fn a_panicking_holder_does_not_lock_everyone_else_out() {
        let _ = thread::spawn(|| {
            let _turn = turn();
            panic!("a failing test");
        })
        .join();
        let _turn = turn();
    }

    #[test]
    fn every_caller_gets_the_same_device() {
        fn limits(adapter: &wgpu::Adapter) -> wgpu::Limits {
            adapter.limits()
        }
        let Some((first, _, first_turn)) = device(limits) else {
            eprintln!("no GPU adapter, skipping shared-device test");
            return;
        };
        drop(first_turn);
        let (second, _, _turn) = device(limits).expect("the adapter did not go away");
        assert!(Arc::ptr_eq(&first, &second));
    }
}
