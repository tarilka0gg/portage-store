use crate::portage::emerge::{self, EmergeEvent, Job};
use gtk::glib;
use std::sync::OnceLock;
use tokio::runtime::Runtime;

/// GTK owns the main thread; tokio runs everything that would block it
/// (eix invocations, emerge subprocesses) on this shared runtime.
fn runtime() -> &'static Runtime {
    static RT: OnceLock<Runtime> = OnceLock::new();
    RT.get_or_init(|| Runtime::new().expect("failed to start tokio runtime"))
}

/// Runs blocking work off the main thread and delivers its result back on
/// it. GTK widgets aren't thread-safe, so `on_done` deliberately runs on
/// the GTK main context, where touching them is legal.
pub fn spawn_blocking<T, F, G>(work: F, on_done: G)
where
    F: FnOnce() -> T + Send + 'static,
    T: Send + 'static,
    G: Fn(T) + 'static,
{
    let (tx, rx) = async_channel::bounded(1);
    runtime().spawn_blocking(move || {
        let _ = tx.send_blocking(work());
    });
    glib::spawn_future_local(async move {
        if let Ok(result) = rx.recv().await {
            on_done(result);
        }
    });
}

/// Runs blocking work that produces results one at a time, delivering each
/// to `on_item` as it arrives rather than batching everything into one
/// final callback. `work` gets a blocking sender to push items with as it
/// goes — most useful for a background loop that should stay off the GTK
/// main thread its *entire* run, not just its first step.
///
/// Deliberately still a single `spawn_blocking` call underneath, i.e. the
/// work closure runs on one thread from tokio's blocking pool rather than
/// fanning out across many: callers doing several slow network requests
/// (icon lookups, for instance) should loop over them sequentially inside
/// `work` instead of spawning one `spawn_blocking` per request, since a
/// dozen-plus concurrent `curl` processes contend for bandwidth and CPU
/// for no benefit when nothing is waiting on all of them at once anyway.
pub fn spawn_stream<T, F, G>(work: F, on_item: G)
where
    F: FnOnce(async_channel::Sender<T>) + Send + 'static,
    T: Send + 'static,
    G: Fn(T) + 'static,
{
    let (tx, rx) = async_channel::unbounded();
    runtime().spawn_blocking(move || work(tx));
    glib::spawn_future_local(async move {
        while let Ok(item) = rx.recv().await {
            on_item(item);
        }
    });
}

/// Runs an emerge job, streaming its output lines to `on_line` and its exit
/// status to `on_done`, both on the GTK main context.
pub fn spawn_job<L, D>(job: Job, on_line: L, on_done: D)
where
    L: Fn(String) + 'static,
    D: Fn(bool) + 'static,
{
    let (tx, rx) = async_channel::bounded(256);
    runtime().spawn(emerge::run(job, tx));
    glib::spawn_future_local(async move {
        while let Ok(event) = rx.recv().await {
            match event {
                EmergeEvent::Line(line) => on_line(line),
                EmergeEvent::Finished { success } => {
                    on_done(success);
                    break;
                }
                EmergeEvent::FailedToStart(err) => {
                    on_line(format!("Failed to start emerge: {err}"));
                    on_done(false);
                    break;
                }
            }
        }
    });
}
