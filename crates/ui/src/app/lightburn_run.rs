//! A background "load + run in LightBurn" job, chained after a placement
//! export. Drives [`drivers::lightburn`] over UDP on a std thread and reports
//! phase transitions back to the UI over an mpsc channel — the same
//! non-blocking shape as [`VerbJob`](super::VerbJob).

use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::path::PathBuf;
use std::sync::mpsc::Sender;
use std::time::{Duration, Instant};

use drivers::lightburn::{LightburnClient, Reply};

use super::*;

/// Coarse phase of a LightBurn run, surfaced in the log + `debug_summary`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum LightburnPhase {
    /// Connecting, selecting the device, loading the file, gating on STATUS.
    Loading,
    /// START has been sent.
    Starting,
    /// Polling STATUS while the job burns.
    Running,
    /// The job finished (STATUS returned to idle).
    Done,
    /// The run failed; the note carries why.
    Error,
}

/// One phase transition (or in-phase warning) from the worker thread.
pub(super) struct LightburnEvent {
    phase: LightburnPhase,
    message: String,
    /// Rendered as a warning/error log line.
    err: bool,
}

impl LightburnEvent {
    /// A phase transition. `Error` events are flagged as errors automatically.
    fn phase(phase: LightburnPhase, message: impl Into<String>) -> Self {
        Self {
            err: matches!(phase, LightburnPhase::Error),
            phase,
            message: message.into(),
        }
    }

    /// A non-fatal warning that keeps the run in its current `phase`.
    fn warn(phase: LightburnPhase, message: impl Into<String>) -> Self {
        Self {
            phase,
            message: message.into(),
            err: true,
        }
    }
}

/// A LightBurn run on a background thread. Mirrors [`VerbJob`](super::VerbJob):
/// events stream over `rx`, `done` flips when the thread exits, and `phase` /
/// `note` snapshot the latest transition (updated on drain).
pub(super) struct LightburnRun {
    rx: Receiver<LightburnEvent>,
    done: Arc<AtomicBool>,
    phase: LightburnPhase,
    note: String,
    /// The run only loads the file (no START) — the drill-emit contract.
    /// Read by the load-only run tests, which assert START is never sent.
    #[cfg_attr(not(test), allow(dead_code))]
    load_only: bool,
}

impl LightburnRun {
    pub(super) fn finished(&self) -> bool {
        self.done.load(Ordering::Relaxed)
    }

    /// True for a load-only run: the worker never sends START. Only the tests
    /// read this; the UI distinguishes the two runs at the spawn site.
    #[cfg_attr(not(test), allow(dead_code))]
    pub(super) fn load_only(&self) -> bool {
        self.load_only
    }

    /// Drain pending events into log lines, updating the phase/note snapshot.
    fn drain(&mut self) -> Vec<LogLine> {
        let mut lines = Vec::new();
        while let Ok(ev) = self.rx.try_recv() {
            self.phase = ev.phase;
            self.note = ev.message.clone();
            lines.push(LogLine {
                text: format!("LightBurn: {}", ev.message),
                err: ev.err,
            });
        }
        lines
    }

    /// The greppable state token for `debug_summary` (`loading` / `starting` /
    /// `running` / `done` / `err:<short>`).
    pub(super) fn token(&self) -> String {
        match self.phase {
            LightburnPhase::Loading => "loading".into(),
            LightburnPhase::Starting => "starting".into(),
            LightburnPhase::Running => "running".into(),
            LightburnPhase::Done => "done".into(),
            LightburnPhase::Error => {
                let short: String = self
                    .note
                    .lines()
                    .next()
                    .unwrap_or("")
                    .chars()
                    .take(60)
                    .collect();
                format!("err:{short}")
            }
        }
    }
}

impl ConsoleApp {
    /// Drain the LightBurn run into the log, keeping the UI repainting while it
    /// works. The finished run is kept (inert) so `debug_summary` can still
    /// report the terminal `done` / `err` state; a fresh run replaces it.
    pub(super) fn pump_lightburn(&mut self, ctx: &Context) {
        let (lines, finished) = match &mut self.runtime.lightburn_run {
            Some(run) => {
                let mut lines = run.drain();
                let finished = run.finished();
                if finished {
                    lines.extend(run.drain()); // stragglers after the flag
                }
                (lines, finished)
            }
            None => return,
        };
        for l in lines {
            self.runtime.log.push(l);
        }
        if self.runtime.log.len() > 500 {
            let drop = self.runtime.log.len() - 500;
            self.runtime.log.drain(0..drop);
        }
        if !finished {
            ctx.request_repaint();
        }
    }

    /// A LightBurn run is currently in flight (loading/starting/running) —
    /// used to disable the buttons that would start another on top of it.
    pub(super) fn lightburn_busy(&self) -> bool {
        self.runtime
            .lightburn_run
            .as_ref()
            .is_some_and(|r| !r.finished())
    }

    /// The place-section token for `debug_summary`: `idle` (nothing queued or
    /// running), `pending` (queued behind the export verb), or the active/last
    /// run's [`token`](LightburnRun::token).
    pub(super) fn lightburn_token(&self) -> String {
        match &self.runtime.lightburn_run {
            Some(run) => run.token(),
            None if self.runtime.pending_lightburn.is_some() => "pending".into(),
            None => "idle".into(),
        }
    }
}

/// Spawn a run using the target/reply port from the environment (defaults to
/// the documented `127.0.0.1:19840` / reply `19841`). `PCBFORGE_LIGHTBURN_ADDR`
/// and `PCBFORGE_LIGHTBURN_REPLY_PORT` override them (read here, at spawn time
/// — headless tests point these at a fake server on ephemeral ports).
pub(super) fn spawn_lightburn_run(lbrn2: PathBuf, device: String) -> LightburnRun {
    let (target, reply_port) = env_endpoints();
    spawn_lightburn_run_at(lbrn2, device, target, reply_port, true)
}

/// Spawn a **load-only** run: connect, select the device, FORCELOAD the file,
/// and stop — START is never sent, so nothing burns until the operator presses
/// play in LightBurn themselves. Same env overrides as [`spawn_lightburn_run`].
pub(super) fn spawn_lightburn_load(lbrn2: PathBuf, device: String) -> LightburnRun {
    let (target, reply_port) = env_endpoints();
    spawn_lightburn_run_at(lbrn2, device, target, reply_port, false)
}

/// Target/reply endpoints from the environment, with the documented defaults.
fn env_endpoints() -> (SocketAddr, u16) {
    let target = std::env::var("PCBFORGE_LIGHTBURN_ADDR")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(drivers::lightburn::DEFAULT_TARGET);
    let reply_port = std::env::var("PCBFORGE_LIGHTBURN_REPLY_PORT")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(drivers::lightburn::DEFAULT_REPLY_PORT);
    (target, reply_port)
}

/// Poll STATUS this often while a job runs.
const POLL_INTERVAL: Duration = Duration::from_millis(500);
/// If STATUS never reports busy within this window, assume a very short job
/// completed and finish with a warning rather than waiting forever.
const NEVER_BUSY_GRACE: Duration = Duration::from_secs(10);
/// Hard ceiling on a single run.
const HARD_TIMEOUT: Duration = Duration::from_secs(30 * 60);

/// Spawn a run against an explicit `target` / `reply_port` (the env-free core;
/// tests call this directly to dodge the process-global env-var race).
pub(super) fn spawn_lightburn_run_at(
    lbrn2: PathBuf,
    device: String,
    target: SocketAddr,
    reply_port: u16,
    start_job: bool,
) -> LightburnRun {
    let (tx, rx) = mpsc::channel::<LightburnEvent>();
    let done = Arc::new(AtomicBool::new(false));
    let done_t = done.clone();
    thread::spawn(move || {
        run_worker(&tx, &lbrn2, &device, target, reply_port, start_job);
        done_t.store(true, Ordering::Relaxed);
    });
    LightburnRun {
        rx,
        done,
        phase: LightburnPhase::Loading,
        note: if start_job {
            "starting LightBurn run".into()
        } else {
            "loading in LightBurn".into()
        },
        load_only: !start_job,
    }
}

/// The worker body: connect, load, and — for a full run (`start_job`) — gate,
/// start, and poll to completion; a load-only run finishes right after the
/// FORCELOAD. Every exit path sends a terminal [`LightburnPhase`] event
/// (`Done` or `Error`).
fn run_worker(
    tx: &Sender<LightburnEvent>,
    lbrn2: &std::path::Path,
    device: &str,
    target: SocketAddr,
    reply_port: u16,
    start_job: bool,
) {
    let reply_bind = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), reply_port);
    let client = match LightburnClient::connect(target, reply_bind) {
        Ok(c) => c,
        Err(e) => {
            let _ = tx.send(LightburnEvent::phase(
                LightburnPhase::Error,
                format!("could not open a UDP socket for LightBurn: {e}"),
            ));
            return;
        }
    };

    // PING — a closed app (or one with a modal dialog) won't cleanly reply, so
    // any non-OK or io error maps to the same friendly message.
    match client.ping() {
        Ok(Reply::Ok) => {}
        _ => {
            let _ = tx.send(LightburnEvent::phase(
                LightburnPhase::Error,
                "LightBurn not responding (not running, or a dialog is open?)",
            ));
            return;
        }
    }

    let _ = tx.send(LightburnEvent::phase(
        LightburnPhase::Loading,
        format!("selecting {device} and loading {}", lbrn2.display()),
    ));

    // Device select: a wrong/absent name shouldn't abort the load — warn and
    // press on (LightBurn keeps its current device).
    if let Some(desc) = non_ok(client.select_laser(device)) {
        let _ = tx.send(LightburnEvent::warn(
            LightburnPhase::Loading,
            format!("device select for '{device}' returned {desc} — continuing"),
        ));
    }

    let path = lbrn2.to_string_lossy();
    if let Some(desc) = non_ok(client.force_load(&path)) {
        let _ = tx.send(LightburnEvent::phase(
            LightburnPhase::Error,
            format!("FORCELOAD failed ({desc})"),
        ));
        return;
    }

    // Load-only: the file is in LightBurn, and that's the whole contract —
    // START stays with the operator.
    if !start_job {
        let _ = tx.send(LightburnEvent::phase(
            LightburnPhase::Done,
            format!(
                "loaded {} — NOT started; press ▶ in LightBurn to burn it",
                lbrn2.display()
            ),
        ));
        return;
    }

    // STATUS gate: only start from a connected + idle machine.
    match client.status() {
        Ok(Reply::Ok) => {}
        Ok(Reply::Busy) => {
            let _ = tx.send(LightburnEvent::phase(
                LightburnPhase::Error,
                "the laser is busy — wait for the current job to finish",
            ));
            return;
        }
        other => {
            let _ = tx.send(LightburnEvent::phase(
                LightburnPhase::Error,
                format!("STATUS check failed ({})", describe(other)),
            ));
            return;
        }
    }

    let _ = tx.send(LightburnEvent::phase(
        LightburnPhase::Starting,
        "starting the job",
    ));
    if let Some(desc) = non_ok(client.start()) {
        let _ = tx.send(LightburnEvent::phase(
            LightburnPhase::Error,
            format!("START failed ({desc})"),
        ));
        return;
    }

    let _ = tx.send(LightburnEvent::phase(
        LightburnPhase::Running,
        "job running",
    ));
    poll_to_completion(tx, &client);
}

/// Poll STATUS until the job finishes. On a galvo the busy→idle edge tracks the
/// real burn: once busy has been seen, the next idle means done. A job that
/// never registers busy within [`NEVER_BUSY_GRACE`] finishes with a warning.
fn poll_to_completion(tx: &Sender<LightburnEvent>, client: &LightburnClient) {
    let start = Instant::now();
    let mut busy_seen = false;
    loop {
        thread::sleep(POLL_INTERVAL);
        if start.elapsed() > HARD_TIMEOUT {
            let _ = tx.send(LightburnEvent::phase(
                LightburnPhase::Error,
                "timed out after 30 min waiting for the job to finish",
            ));
            return;
        }
        match status_with_retry(client) {
            Ok(Reply::Busy) => busy_seen = true,
            Ok(Reply::Ok) => {
                if busy_seen {
                    let _ = tx.send(LightburnEvent::phase(LightburnPhase::Done, "job finished"));
                    return;
                }
                if start.elapsed() > NEVER_BUSY_GRACE {
                    let _ = tx.send(LightburnEvent::warn(
                        LightburnPhase::Done,
                        "status never went busy — verify the job ran",
                    ));
                    return;
                }
            }
            Ok(Reply::Unknown) => {
                let _ = tx.send(LightburnEvent::phase(
                    LightburnPhase::Error,
                    "STATUS returned '?' (unknown command)",
                ));
                return;
            }
            Err(e) => {
                let _ = tx.send(LightburnEvent::phase(
                    LightburnPhase::Error,
                    format!("lost contact with LightBurn while polling: {e}"),
                ));
                return;
            }
        }
    }
}

/// STATUS with a couple of retries, to ride out transient datagram loss.
fn status_with_retry(client: &LightburnClient) -> std::io::Result<Reply> {
    let mut last = None;
    for _ in 0..3 {
        match client.status() {
            Ok(r) => return Ok(r),
            Err(e) => last = Some(e),
        }
    }
    Err(last.expect("at least one attempt"))
}

/// `None` when the reply is `OK`; otherwise a short description of what it was
/// (used for the warn-but-continue and fatal command paths).
fn non_ok(reply: std::io::Result<Reply>) -> Option<String> {
    match reply {
        Ok(Reply::Ok) => None,
        other => Some(describe(other)),
    }
}

fn describe(reply: std::io::Result<Reply>) -> String {
    match reply {
        Ok(Reply::Ok) => "OK".into(),
        Ok(Reply::Busy) => "busy (!)".into(),
        Ok(Reply::Unknown) => "unknown command (?)".into(),
        Err(e) => e.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::UdpSocket;
    use std::sync::{Arc, Mutex};

    fn loopback(port: u16) -> SocketAddr {
        SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), port)
    }

    /// A fake LightBurn that completes a run: OK to PING/LASER/FORCELOAD and the
    /// pre-START STATUS gate, then busy (`!`) on the first STATUS after it sees
    /// START and idle (`OK`) after — a busy→idle edge that finishes in two
    /// polls. Records every command it received.
    fn fake_lightburn_that_completes() -> (SocketAddr, Arc<Mutex<Vec<String>>>) {
        let socket = UdpSocket::bind(loopback(0)).unwrap();
        let addr = socket.local_addr().unwrap();
        let seen = Arc::new(Mutex::new(Vec::<String>::new()));
        let seen_t = seen.clone();
        thread::spawn(move || {
            let mut buf = [0u8; 512];
            let mut started = false;
            let mut polled_since_start = 0;
            loop {
                let Ok((n, src)) = socket.recv_from(&mut buf) else {
                    return;
                };
                let cmd = String::from_utf8_lossy(&buf[..n]).trim().to_string();
                seen_t.lock().unwrap().push(cmd.clone());
                let reply: &str = if cmd == "START" {
                    started = true;
                    "OK"
                } else if cmd == "STATUS" {
                    if !started {
                        "OK" // idle gate before START
                    } else {
                        polled_since_start += 1;
                        if polled_since_start == 1 { "!" } else { "OK" }
                    }
                } else {
                    "OK" // PING / LASER / FORCELOAD
                };
                let _ = socket.send_to(reply.as_bytes(), src);
            }
        });
        (addr, seen)
    }

    #[test]
    fn end_to_end_run_reaches_done_and_sends_forceload_and_start() {
        let (addr, seen) = fake_lightburn_that_completes();
        let path = PathBuf::from("C:\\jobs\\placed.lbrn2");
        let mut run = spawn_lightburn_run_at(path.clone(), "BSLFiber".into(), addr, 0, true);

        // Poll the run to completion (busy→idle finishes in ~2 polls ≈ 1 s).
        let deadline = Instant::now() + Duration::from_secs(20);
        while !run.finished() && Instant::now() < deadline {
            let _ = run.drain();
            thread::sleep(Duration::from_millis(20));
        }
        let _ = run.drain();
        assert!(run.finished(), "run finished");
        assert_eq!(run.token(), "done", "reached the done phase: {}", run.note);

        let cmds = seen.lock().unwrap();
        assert!(cmds.iter().any(|c| c == "PING"), "sent PING: {cmds:?}");
        assert!(
            cmds.iter().any(|c| c == "FORCELOAD:C:\\jobs\\placed.lbrn2"),
            "sent FORCELOAD with the path: {cmds:?}"
        );
        assert!(cmds.iter().any(|c| c == "START"), "sent START: {cmds:?}");
    }

    /// A load-only run (the drill-emit contract) FORCELOADs the file and
    /// finishes done — START is never sent, so nothing can burn.
    #[test]
    fn load_only_run_loads_the_file_and_never_sends_start() {
        let (addr, seen) = fake_lightburn_that_completes();
        let path = PathBuf::from("C:\\jobs\\drill.lbrn2");
        let mut run = spawn_lightburn_run_at(path.clone(), "BSLFiber".into(), addr, 0, false);
        assert!(run.load_only(), "the run reports itself load-only");

        let deadline = Instant::now() + Duration::from_secs(20);
        while !run.finished() && Instant::now() < deadline {
            let _ = run.drain();
            thread::sleep(Duration::from_millis(20));
        }
        let _ = run.drain();
        assert!(run.finished(), "run finished");
        assert_eq!(run.token(), "done", "loaded and done: {}", run.note);
        assert!(
            run.note.contains("NOT started"),
            "the done note says the job was not started: {}",
            run.note
        );

        let cmds = seen.lock().unwrap();
        assert!(
            cmds.iter().any(|c| c == "FORCELOAD:C:\\jobs\\drill.lbrn2"),
            "sent FORCELOAD with the path: {cmds:?}"
        );
        assert!(
            !cmds.iter().any(|c| c == "START"),
            "START must never be sent by a load-only run: {cmds:?}"
        );
    }

    #[test]
    fn a_dead_lightburn_fails_with_the_friendly_message() {
        // No server at this port: PING times out → the not-responding message.
        let mut run = spawn_lightburn_run_at(
            PathBuf::from("x.lbrn2"),
            "BSLFiber".into(),
            loopback(0), // port 0 is never listening
            0,
            true,
        );
        let deadline = Instant::now() + Duration::from_secs(20);
        while !run.finished() && Instant::now() < deadline {
            let _ = run.drain();
            thread::sleep(Duration::from_millis(20));
        }
        let _ = run.drain();
        assert!(run.finished(), "run finished");
        assert!(
            run.token().starts_with("err:") && run.note.contains("not responding"),
            "friendly not-responding error: token={} note={}",
            run.token(),
            run.note
        );
    }
}
