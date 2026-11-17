//! Adding this device to a person from the page, like the desktop app does
//! it: the flow runs on a thread of its own, the page polls its steps in
//! `api/status` and answers the check code with `api/join/answer`.

use std::io::{self, Write};
use std::sync::Arc;

use lz_client::devices::{Check, Dialog};
use lz_client::qr::{self, Qr};
use serde::Serialize;
use tokio::sync::{mpsc, oneshot};

use crate::server::{Shared, runtime};

/// Most progress lines kept for the page.
const MAX_PROGRESS: usize = 50;

/// Adding this device: the steps for the page and the flow in progress.
#[derive(Default)]
pub struct Join {
    /// What the page shows; `None` before a flow or after cancelling.
    pub view: Option<JoinView>,
    /// The running flow, if any.
    flow: Option<Flow>,
    /// Counts the flows begun, so that a cancelled flow that is still
    /// winding down changes nothing.
    generation: u64,
}

impl Join {
    pub fn running(&self) -> bool {
        self.flow.is_some()
    }
}

/// How the page answers and aborts the running flow.
struct Flow {
    answers: mpsc::UnboundedSender<bool>,
    cancel: Option<oneshot::Sender<()>>,
}

/// `join` of `api/status`.
#[derive(Clone, Debug, Serialize)]
pub struct JoinView {
    /// `device_code`, `check_device`, `confirm_device`, `check_handover`,
    /// `confirm_handover`, `done` or `failed`.
    pub step: &'static str,
    pub code: Option<String>,
    /// Only with `device_code`.
    pub qr: Option<Qr>,
    pub progress: Vec<String>,
    /// The failure, in the language of the page.
    pub error: Option<String>,
}

/// Why a flow could not start.
pub enum Refused {
    /// Another flow runs.
    Running,
    /// There is a state file already.
    HasDevice,
}

/// Starts adding this device in the background.
pub fn start(shared: &Arc<Shared>, relay: String, name: String) -> Result<(), Refused> {
    let mut join = shared.join.lock().expect("lock poisoned");
    if join.running() {
        return Err(Refused::Running);
    }
    if shared.state.exists() {
        return Err(Refused::HasDevice);
    }
    let (answers, answered) = mpsc::unbounded_channel();
    let (cancel, cancelled) = oneshot::channel();
    join.generation += 1;
    let generation = join.generation;
    join.flow = Some(Flow {
        answers,
        cancel: Some(cancel),
    });
    join.view = None;
    drop(join);

    let shared = Arc::clone(shared);
    std::thread::spawn(move || {
        let outcome = run(&shared, generation, &relay, &name, answered, cancelled);
        match outcome {
            Ok(true) => {
                if let Err(error) = shared.ensure_token() {
                    eprintln!("lz-server: {error}");
                }
                shared.wake_listener();
                finish(&shared, generation, "done", None);
            }
            // Cancelled: `cancel` has reset everything already.
            Ok(false) => {}
            Err(error) => finish(&shared, generation, "failed", Some(error)),
        }
    });
    Ok(())
}

/// Runs the flow; true if it succeeded, false if cancelled.
fn run(
    shared: &Arc<Shared>,
    generation: u64,
    relay: &str,
    name: &str,
    answers: mpsc::UnboundedReceiver<bool>,
    cancelled: oneshot::Receiver<()>,
) -> Result<bool, String> {
    let runtime = runtime().map_err(|error| error.to_string())?;
    let sink = ProgressSink {
        shared: Arc::clone(shared),
        generation,
        pending: Vec::new(),
    };
    let mut ctx = shared.ctx(Box::new(sink));
    let mut dialog = PageDialog {
        shared: Arc::clone(shared),
        generation,
        answers,
    };
    runtime.block_on(async {
        tokio::select! {
            result = ctx.join_device(relay, name, None, &mut dialog) => {
                result.map(|()| true).map_err(|error| error.0)
            }
            _ = cancelled => Ok(false),
        }
    })
}

/// Aborts the flow if one runs and forgets the steps, in every state, so
/// that the page can start again.
pub fn cancel(shared: &Shared) {
    let mut join = shared.join.lock().expect("lock poisoned");
    if let Some(mut flow) = join.flow.take()
        && let Some(cancel) = flow.cancel.take()
    {
        let _ = cancel.send(());
    }
    join.generation += 1;
    join.view = None;
}

/// Passes the answer to the check code on; false if no flow runs.
pub fn answer(shared: &Shared, matches: bool) -> bool {
    let join = shared.join.lock().expect("lock poisoned");
    match &join.flow {
        Some(flow) => {
            let _ = flow.answers.send(matches);
            true
        }
        None => false,
    }
}

/// Changes the view, unless the flow of `generation` is over.
fn update(shared: &Shared, generation: u64, change: impl FnOnce(&mut JoinView)) {
    let mut join = shared.join.lock().expect("lock poisoned");
    if join.generation != generation {
        return;
    }
    let view = join.view.get_or_insert_with(|| JoinView {
        step: "device_code",
        code: None,
        qr: None,
        progress: Vec::new(),
        error: None,
    });
    change(view);
}

fn set_step(shared: &Shared, generation: u64, step: &'static str, code: Option<&str>) {
    let qr = (step == "device_code")
        .then_some(code)
        .flatten()
        .map(qr::modules);
    update(shared, generation, |view| {
        view.step = step;
        view.code = code.map(str::to_owned);
        view.qr = qr;
        view.error = None;
    });
}

/// Ends the flow of `generation` with `step`.
fn finish(shared: &Shared, generation: u64, step: &'static str, error: Option<String>) {
    update(shared, generation, |view| {
        view.step = step;
        view.code = None;
        view.qr = None;
        view.error = error;
    });
    let mut join = shared.join.lock().expect("lock poisoned");
    if join.generation == generation {
        join.flow = None;
    }
}

/// The page's side of the flow.
struct PageDialog {
    shared: Arc<Shared>,
    generation: u64,
    answers: mpsc::UnboundedReceiver<bool>,
}

impl Dialog for PageDialog {
    fn device_code(&mut self, code: &str) {
        set_step(&self.shared, self.generation, "device_code", Some(code));
    }

    fn check_code(&mut self, check: Check, code: &str) {
        let step = match check {
            Check::Device => "check_device",
            Check::Handover => "check_handover",
        };
        set_step(&self.shared, self.generation, step, Some(code));
    }

    async fn confirm(&mut self, check: Check, code: &str) -> bool {
        let step = match check {
            Check::Device => "confirm_device",
            Check::Handover => "confirm_handover",
        };
        // Answers given before the question do not count.
        while self.answers.try_recv().is_ok() {}
        set_step(&self.shared, self.generation, step, Some(code));
        // A closed channel means the flow was cancelled.
        self.answers.recv().await.unwrap_or(false)
    }
}

/// Whether a line of the flow's output is progress for the page: not the
/// QR code for terminals and not the bare code, which the page shows
/// itself (as in the desktop app).
pub fn is_progress(line: &str) -> bool {
    !line.is_empty()
        && !line.starts_with(lz_core::wire::device_code::PREFIX)
        && !line.chars().any(|c| matches!(c, '█' | '▀' | '▄'))
}

/// Collects the progress lines of the flow.
struct ProgressSink {
    shared: Arc<Shared>,
    generation: u64,
    pending: Vec<u8>,
}

impl Write for ProgressSink {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.pending.extend_from_slice(buf);
        while let Some(end) = self.pending.iter().position(|&b| b == b'\n') {
            let line: Vec<u8> = self.pending.drain(..=end).collect();
            let line = String::from_utf8_lossy(&line).trim_end().to_owned();
            if is_progress(&line) {
                update(&self.shared, self.generation, |view| {
                    if view.progress.len() == MAX_PROGRESS {
                        view.progress.remove(0);
                    }
                    view.progress.push(line);
                });
            }
        }
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}
