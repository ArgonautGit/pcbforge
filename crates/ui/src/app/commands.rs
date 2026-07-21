use super::*;

impl ConsoleApp {
    pub fn refresh(&mut self) {
        self.runtime.status = status::snapshot(&self.db_path);
    }

    /// Start `pcbforge <args>` on a background thread; its output streams into
    /// the log via [`pump_verb`](Self::pump_verb). Non-blocking — the GUI stays
    /// responsive. One verb at a time; a second is refused while one runs.
    /// Returns `true` if the job actually started (so a caller chaining more
    /// work — e.g. the LightBurn run — can tell a refused click from a real
    /// launch and not arm the follow-up against a job it never started).
    pub fn run_verb(&mut self, args: &[String]) -> bool {
        if self
            .runtime
            .verb_job
            .as_ref()
            .is_some_and(|j| !j.finished())
        {
            self.runtime.log.push(LogLine {
                text: "a job is already running — wait for it to finish".into(),
                err: true,
            });
            return false;
        }
        self.runtime.verb_job = Some(spawn_verb(&self.cli_cmd, args));
        true
    }

    /// Drain any streamed verb output into the log; on completion, refresh the
    /// status snapshot. Called every frame.
    pub(super) fn pump_verb(&mut self, ctx: &Context) {
        let Some(job) = &self.runtime.verb_job else {
            return;
        };
        let (mut lines, finished) = (job.drain(), job.finished());
        if finished {
            lines.extend(job.drain()); // catch any stragglers after the flag
        }
        // Read the exit result before clearing the job below, so the follow-up
        // LightBurn chain sees the true success/failure of the run that just
        // finished (not a default).
        let succeeded = finished && job.succeeded();
        for l in lines {
            self.runtime.log.push(l);
        }
        if self.runtime.log.len() > 500 {
            let drop = self.runtime.log.len() - 500;
            self.runtime.log.drain(0..drop);
        }
        if finished {
            self.runtime.verb_job = None;
            self.refresh();
            self.chain_lightburn_after_verb(succeeded);
        } else {
            ctx.request_repaint();
        }
    }

    /// After a verb finishes, kick off a queued "run in LightBurn" if the
    /// placement export requested one. A failed export skips the run (and says
    /// so); a success spawns the [`LightburnRun`], unless one is already going.
    pub(super) fn chain_lightburn_after_verb(&mut self, succeeded: bool) {
        let Some(path) = self.runtime.pending_lightburn.take() else {
            return;
        };
        if !succeeded {
            self.runtime.log.push(LogLine {
                text: "LightBurn run skipped — the register export did not finish cleanly".into(),
                err: true,
            });
            return;
        }
        if self
            .runtime
            .lightburn_run
            .as_ref()
            .is_some_and(|r| !r.finished())
        {
            self.runtime.log.push(LogLine {
                text: "LightBurn run skipped — one is already in progress".into(),
                err: true,
            });
            return;
        }
        let device = self.placement.lightburn_device.clone();
        self.runtime.log.push(LogLine {
            text: format!(
                "LightBurn: loading {} and running on {device}",
                path.display()
            ),
            err: false,
        });
        self.runtime.lightburn_run = Some(spawn_lightburn_run(path, device));
    }

    /// (Re)build the preview texture from the active side's Gerbers (the back
    /// side is shown mirrored, exactly as it will burn).
    pub fn render_preview(&mut self, ctx: &Context) {
        match self.active_job() {
            Ok((board, copper, ablate)) => {
                let img = preview::rasterize(
                    &[
                        preview::Layer {
                            polys: &board,
                            color: preview::BOARD,
                        },
                        preview::Layer {
                            polys: &ablate,
                            color: preview::ABLATE,
                        },
                        preview::Layer {
                            polys: &copper,
                            color: preview::COPPER,
                        },
                    ],
                    preview::BOARD,
                    40.0,
                    900,
                );
                let side = match self.job.side {
                    Side::Front => "front",
                    Side::Back => "back (mirrored)",
                };
                self.job.preview_note = format!(
                    "{side}: {} copper region(s), {} to-ablate region(s), offset {} mm",
                    copper.len(),
                    ablate.len(),
                    self.job.offset_mm
                );
                self.job.preview_tex =
                    Some(ctx.load_texture("job-preview", img, TextureOptions::NEAREST));
            }
            Err(e) => {
                self.job.preview_tex = None;
                self.job.preview_note = e;
            }
        }
    }
}

/// Shell `cmd[0] cmd[1..] args`, capturing stdout (info) and stderr (warn) as
/// log lines plus a header and an exit-status footer. A spawn failure — or an
/// empty command — is one error line.
pub fn run_capture(cmd: &[String], args: &[String]) -> Vec<LogLine> {
    let Some((program, prefix)) = cmd.split_first() else {
        return vec![LogLine {
            text: "no CLI command configured".into(),
            err: true,
        }];
    };
    let mut out = vec![LogLine {
        text: format!("$ {} {}", cmd.join(" "), args.join(" ")),
        err: false,
    }];
    match std::process::Command::new(program)
        .args(prefix)
        .args(args)
        .output()
    {
        Ok(o) => {
            for line in String::from_utf8_lossy(&o.stdout).lines() {
                out.push(LogLine {
                    text: line.to_string(),
                    err: false,
                });
            }
            for line in String::from_utf8_lossy(&o.stderr).lines() {
                out.push(LogLine {
                    text: line.to_string(),
                    err: true,
                });
            }
            out.push(LogLine {
                text: format!("[exit {}]", o.status.code().unwrap_or(-1)),
                err: !o.status.success(),
            });
        }
        Err(e) => out.push(LogLine {
            text: format!("failed to run `{program}`: {e}"),
            err: true,
        }),
    }
    out
}

/// A CLI verb running on a background thread. Its stdout/stderr stream over the
/// channel line-by-line so the GUI never blocks; `done` flips when the process
/// exits. Dropping the job detaches the reader threads (they end when the
/// child's pipes close).
pub struct VerbJob {
    rx: Receiver<LogLine>,
    done: Arc<AtomicBool>,
    /// The child's exit status was success. Meaningful once [`finished`] is
    /// true; the spawner sets it *before* flipping `done`, so a frame that sees
    /// `finished()` also sees the settled result.
    ///
    /// [`finished`]: VerbJob::finished
    succeeded: Arc<AtomicBool>,
}

impl VerbJob {
    /// Take all output lines available since the last poll (non-blocking).
    pub(super) fn drain(&self) -> Vec<LogLine> {
        let mut v = Vec::new();
        while let Ok(l) = self.rx.try_recv() {
            v.push(l);
        }
        v
    }
    pub(super) fn finished(&self) -> bool {
        self.done.load(Ordering::Relaxed)
    }
    /// Whether the child exited successfully (exit code 0). Only meaningful once
    /// [`finished`](VerbJob::finished) is true.
    pub(super) fn succeeded(&self) -> bool {
        self.succeeded.load(Ordering::Relaxed)
    }
}

/// Spawn `cmd[0] cmd[1..] args`, streaming stdout (info) and stderr (warn)
/// lines over the returned job — without blocking the caller (FLD-9).
pub fn spawn_verb(cmd: &[String], args: &[String]) -> VerbJob {
    let (tx, rx) = mpsc::channel::<LogLine>();
    let done = Arc::new(AtomicBool::new(false));
    let done_t = done.clone();
    let succeeded = Arc::new(AtomicBool::new(false));
    let succeeded_t = succeeded.clone();
    let cmd = cmd.to_vec();
    let args = args.to_vec();
    thread::spawn(move || {
        let _ = tx.send(LogLine {
            text: format!("$ {} {}", cmd.join(" "), args.join(" ")),
            err: false,
        });
        let Some((program, prefix)) = cmd.split_first() else {
            let _ = tx.send(LogLine {
                text: "no CLI command configured".into(),
                err: true,
            });
            done_t.store(true, Ordering::Relaxed);
            return;
        };
        let spawned = StdCommand::new(program)
            .args(prefix)
            .args(&args)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn();
        let mut child = match spawned {
            Ok(c) => c,
            Err(e) => {
                let _ = tx.send(LogLine {
                    text: format!("failed to run `{program}`: {e}"),
                    err: true,
                });
                done_t.store(true, Ordering::Relaxed);
                return;
            }
        };
        // Read stdout and stderr concurrently so a full pipe can't deadlock.
        let txo = tx.clone();
        let ho = child.stdout.take().map(|o| {
            thread::spawn(move || {
                for line in BufReader::new(o).lines().map_while(Result::ok) {
                    let _ = txo.send(LogLine {
                        text: line,
                        err: false,
                    });
                }
            })
        });
        let txe = tx.clone();
        let he = child.stderr.take().map(|e| {
            thread::spawn(move || {
                for line in BufReader::new(e).lines().map_while(Result::ok) {
                    let _ = txe.send(LogLine {
                        text: line,
                        err: true,
                    });
                }
            })
        });
        if let Some(h) = ho {
            let _ = h.join();
        }
        if let Some(h) = he {
            let _ = h.join();
        }
        let (code, ok) = match child.wait() {
            Ok(s) => (s.code().unwrap_or(-1), s.success()),
            Err(_) => (-1, false),
        };
        let _ = tx.send(LogLine {
            text: format!("[exit {code}]"),
            err: !ok,
        });
        // Publish the result BEFORE `done`, so a poller that observes
        // `finished()` also observes the settled `succeeded()`.
        succeeded_t.store(ok, Ordering::Relaxed);
        done_t.store(true, Ordering::Relaxed);
    });
    VerbJob {
        rx,
        done,
        succeeded,
    }
}

/// (board, kept-copper, to-ablate) region sets in the Gerber frame.
pub type JobShapes = (
    Vec<pcb_core::Poly>,
    Vec<pcb_core::Poly>,
    Vec<pcb_core::Poly>,
);

/// The job's board, kept-copper, and to-ablate regions in the Gerber frame —
/// the shared geometry behind the preview and the drag-to-place overlay. A
/// *view* computation (pure geometry via `cam::noncopper`), not engine logic;
/// the actual job is still produced by shelling `pcbforge`.
pub fn job_shapes(
    copper_path: &str,
    outline_path: &str,
    offset_mm: f64,
) -> Result<JobShapes, String> {
    let copper_path = crate::clean_path(copper_path);
    let outline_path = crate::clean_path(outline_path);
    if copper_path.is_empty() {
        return Err("set a copper Gerber path first".into());
    }
    let copper = ingest::gerber::load_gerber(std::path::Path::new(&copper_path))
        .map_err(|e| format!("copper: {}", e.msg))?
        .polys;
    let board = if outline_path.is_empty() {
        cam::noncopper::board_region_bbox(&copper, NM_PER_MM) // 1 mm margin
    } else {
        let o = ingest::gerber::load_gerber(std::path::Path::new(&outline_path))
            .map_err(|e| format!("outline: {}", e.msg))?
            .polys;
        cam::noncopper::board_region_from_outline(&o)
    };
    if board.is_empty() {
        return Err("empty board region".into());
    }
    let offset_nm = (offset_mm * NM_PER_MM as f64).round() as Nm;
    let ablate = cam::noncopper::noncopper(&board, &copper, offset_nm);
    Ok((board, copper, ablate))
}

/// Build a preview image from Gerber paths: invert copper → non-copper (the
/// same geometry `emit` burns) and rasterize board/copper/ablate. Returns the
/// image and a caption.
pub fn preview_image(
    copper_path: &str,
    outline_path: &str,
    offset_mm: f64,
) -> Result<(ColorImage, String), String> {
    let (board, copper, ablate) = job_shapes(copper_path, outline_path, offset_mm)?;
    let img = preview::rasterize(
        &[
            Layer {
                polys: &board,
                color: preview::BOARD,
            },
            Layer {
                polys: &ablate,
                color: preview::ABLATE,
            },
            Layer {
                polys: &copper,
                color: preview::COPPER,
            },
        ],
        preview::BOARD,
        40.0,
        900,
    );
    let note = format!(
        "{} copper region(s), {} to-ablate region(s), offset {offset_mm} mm",
        copper.len(),
        ablate.len(),
    );
    Ok((img, note))
}
