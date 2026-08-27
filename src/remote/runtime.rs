//! The one async runtime in the desktop binary.
//!
//! FlightDeck's TUI event loop is, and stays, **synchronous**: it owns
//! `AppState`, renders on a fixed tick, and never awaits anything. Two features
//! nevertheless need async I/O — the relay client ([`crate::remote::client`]) and
//! the embedded web server (`src/web/server.rs`, axum). `specs/WEB_INTERFACE.md`
//! D6 settles how: **one** tokio runtime, owned by a dedicated thread, with both
//! transports spawned onto it and the TUI talking to them over channels.
//!
//! ```text
//!  TUI thread (sync)                     "flightdeck-tokio" thread
//!  ─────────────────                     ─────────────────────────
//!  shared()  ────────── first call ────▶ build multi-thread Runtime
//!     │                                  publish Handle, then park
//!     │                                  (block_on(pending) forever)
//!     ├─ handle().spawn(relay session) ──▶ ┐
//!     └─ handle().spawn(axum server)   ──▶ ┤ 2 worker threads
//!            std::sync::mpsc channels  ◀───┘
//! ```
//!
//! ## Why a dedicated owner thread rather than a runtime in a `static`
//!
//! The runtime is created *inside* a thread that then blocks forever on a
//! pending future. That thread is the runtime's owner for the life of the
//! process, which buys three things a bare `static Runtime` does not:
//!
//! * `Runtime::drop` blocks, and panics outright if it runs from inside a
//!   runtime context. Parking the owner means the value is never dropped from
//!   the wrong place — the process exits and the OS reclaims it.
//! * The thread has a name, so `flightdeck-tokio` and its
//!   `flightdeck-tokio-worker-N` workers are identifiable in a sampler or a
//!   crash report — the same courtesy the old `flightdeck-remote` thread gave.
//! * Runtime construction can fail (it spawns threads). Doing it off the TUI
//!   thread lets [`try_shared`] report that as `None` instead of unwinding
//!   through the render loop; the relay client then simply does not start, which
//!   is exactly how a failed `std::thread::spawn` behaved before this port.
//!
//! ## Why multi-thread, and why only two workers
//!
//! Multi-thread because there are two independent consumers with different
//! latency needs: the relay client must fire its 20 s ping and notice a dead
//! link on time even while axum is streaming terminal bytes to a browser, and
//! neither should be able to head-of-line block the other. A current-thread
//! runtime would put them on one queue, so one long poll (a large TLS write, a
//! synchronous `RemoteStore` save) would delay the other's timers.
//!
//! Two workers, not `available_parallelism()`, because this is a *terminal app*
//! running alongside the user's editors and agents: the honest workload is one
//! relay socket plus a handful of loopback web clients, and a 16-thread pool
//! would be 14 idle threads charged to the user's machine for nothing. Two
//! keeps the "one task cannot stall the other" property that motivated
//! multi-thread in the first place. The blocking pool is left at its default and
//! is only used by tokio's own DNS resolution.
//!
//! ## Contract for callers
//!
//! [`shared`]/[`try_shared`] hand back a `&'static` handle, so a caller never
//! owns or shuts down the runtime; work is stopped by cancelling the *task*
//! (see [`crate::remote::client::RemoteHandle::stop`]), never by tearing the
//! runtime down under the other consumer. Do not call [`SharedRuntime::spawn`]
//! with a future that blocks: use `tokio::task::spawn_blocking` for anything
//! that parks a thread.

use std::sync::mpsc::channel;
use std::sync::OnceLock;

use tokio::runtime::{Builder, Handle};

/// The thread that builds and then owns the runtime.
const OWNER_THREAD: &str = "flightdeck-tokio";
/// Prefix for the runtime's worker threads.
const WORKER_THREAD_PREFIX: &str = "flightdeck-tokio-worker";
/// See the module docs: two workers, deliberately not one per core.
const WORKER_THREADS: usize = 2;

/// A handle onto the process-wide runtime. Obtained from [`shared`] /
/// [`try_shared`]; never constructed by callers, and never dropped.
pub struct SharedRuntime {
    handle: Handle,
}

impl SharedRuntime {
    /// The tokio [`Handle`], for anything the two helpers below do not cover
    /// (`Handle::enter`, `Handle::block_on` from a *non*-async thread, handing a
    /// handle to a library that wants one).
    pub fn handle(&self) -> &Handle {
        &self.handle
    }

    /// Spawn a task onto the shared runtime. Callable from any thread, including
    /// the synchronous TUI thread — this is the seam the TUI crosses to start
    /// the relay client, and the one `src/web/server.rs` uses to start axum.
    pub fn spawn<F>(&self, future: F) -> tokio::task::JoinHandle<F::Output>
    where
        F: std::future::Future + Send + 'static,
        F::Output: Send + 'static,
    {
        self.handle.spawn(future)
    }
}

/// Built at most once, on first use. `None` records a runtime that could not be
/// created at all, so the failure is remembered rather than retried on every
/// call (a machine that cannot spawn threads will not start spawning them).
static SHARED: OnceLock<Option<SharedRuntime>> = OnceLock::new();

/// The shared runtime, or `None` if it could not be created (thread exhaustion —
/// in practice never). Prefer this in any path that must degrade gracefully
/// instead of panicking; the relay client uses it so a runtime failure means
/// "FlightDeck Remote does not start", never "the TUI dies".
pub fn try_shared() -> Option<&'static SharedRuntime> {
    SHARED.get_or_init(start).as_ref()
}

/// The shared runtime, panicking if it could not be created.
///
/// For callers that genuinely cannot continue without it and are not inside the
/// render loop. If you are on the TUI thread, use [`try_shared`].
pub fn shared() -> &'static SharedRuntime {
    try_shared().expect("the shared tokio runtime could not be started")
}

/// Build the runtime on its own thread and publish its handle.
fn start() -> Option<SharedRuntime> {
    // A rendezvous over a plain std channel: the owner thread sends its handle
    // (or nothing, if the build failed) and we block here only for as long as
    // tokio takes to spin up its workers.
    let (tx, rx) = channel::<Handle>();
    std::thread::Builder::new()
        .name(OWNER_THREAD.to_string())
        .spawn(move || {
            let runtime = match Builder::new_multi_thread()
                .worker_threads(WORKER_THREADS)
                .thread_name(WORKER_THREAD_PREFIX)
                // `net` (the relay socket, DNS) + `time` (ping, liveness,
                // backoff). Both are always compiled in, so this cannot fail
                // for want of a feature.
                .enable_all()
                .build()
            {
                Ok(rt) => rt,
                Err(e) => {
                    // NOT `eprintln!`: a stray stderr line lands on top of the
                    // rendered frame while the TUI owns the alternate screen.
                    crate::remote::debuglog::log(&format!("runtime BUILD failed err={e}"));
                    return;
                }
            };
            // Publishing the handle before parking means `try_shared` returns as
            // soon as the runtime can accept work. If the receiver is already
            // gone there is nobody to serve, so wind down instead of parking.
            if tx.send(runtime.handle().clone()).is_err() {
                return;
            }
            // Own the runtime for the life of the process. `pending()` never
            // completes, so `runtime` is never dropped — see the module docs on
            // why that is the point rather than a leak.
            runtime.block_on(std::future::pending::<()>());
        })
        .ok()?;
    rx.recv().ok().map(|handle| SharedRuntime { handle })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The runtime is a singleton: repeated calls hand back the same handle, and
    /// work spawned from a synchronous thread actually runs.
    #[test]
    fn shared_runtime_is_a_singleton_that_runs_spawned_work() {
        let a = try_shared().expect("a test host can start a runtime");
        let b = try_shared().expect("a test host can start a runtime");
        assert!(
            std::ptr::eq(a, b),
            "every caller must get the SAME runtime — a second one would defeat D6"
        );

        // Two independent consumers, both spawned from this synchronous thread,
        // must both make progress (this is what `src/web/server.rs` will do
        // alongside the already-running relay client).
        let (tx, rx) = channel::<u8>();
        let tx2 = tx.clone();
        a.spawn(async move {
            tokio::time::sleep(std::time::Duration::from_millis(1)).await;
            let _ = tx.send(1);
        });
        b.spawn(async move {
            let _ = tx2.send(2);
        });
        let mut seen = vec![
            rx.recv_timeout(std::time::Duration::from_secs(5))
                .expect("first task ran"),
            rx.recv_timeout(std::time::Duration::from_secs(5))
                .expect("second task ran"),
        ];
        seen.sort_unstable();
        assert_eq!(seen, vec![1, 2]);
    }

    /// The worker pool is multi-thread, so one task cannot head-of-line block
    /// the other's timers — the property the relay client's ping/liveness
    /// deadlines depend on once axum shares the runtime.
    #[test]
    fn runtime_is_multi_thread_so_a_blocked_task_cannot_stall_a_timer() {
        let rt = try_shared().expect("a test host can start a runtime");
        let (tx, rx) = channel::<()>();
        // A task that parks its worker thread outright.
        rt.spawn(async {
            std::thread::sleep(std::time::Duration::from_millis(400));
        });
        // A second task must still get scheduled and fire its timer.
        rt.spawn(async move {
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            let _ = tx.send(());
        });
        assert!(
            rx.recv_timeout(std::time::Duration::from_millis(350))
                .is_ok(),
            "a second worker must run the timer while the first thread is parked"
        );
    }
}
