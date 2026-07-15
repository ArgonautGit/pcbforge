//! ING-6 — the one place in the workspace that shells out to `kicad-cli`.
//!
//! kicad-cli's flag surface churns across releases, so nothing here is
//! assumed: [`KicadCli::discover`] locates the binary and each export method
//! first parses `kicad-cli <subcommand> --help` and verifies every flag it
//! is about to pass, returning [`KicadCliError::MissingFlag`] naming the
//! exact flag and subcommand when the installed version lacks one.
//!
//! All other crates and tests must call through this module rather than
//! spawning `kicad-cli` themselves (backlog ING-6 constraint).

use std::fmt;
use std::path::{Path, PathBuf};
use std::process::Command;

/// Errors from locating or driving `kicad-cli`.
#[derive(Debug)]
pub enum KicadCliError {
    /// The binary was not found on PATH (or failed to run at all).
    NotFound(String),
    /// The installed kicad-cli lacks a flag we require.
    MissingFlag {
        subcommand: String,
        flag: String,
        version: String,
    },
    /// The subprocess ran but exited nonzero.
    Failed { command: String, stderr: String },
    /// Output the wrapper could not interpret.
    UnexpectedOutput { command: String, detail: String },
}

impl fmt::Display for KicadCliError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            KicadCliError::NotFound(detail) => {
                write!(f, "kicad-cli not found or not runnable: {detail}")
            }
            KicadCliError::MissingFlag {
                subcommand,
                flag,
                version,
            } => write!(
                f,
                "installed kicad-cli ({version}) lacks required flag {flag} on `kicad-cli {subcommand}`"
            ),
            KicadCliError::Failed { command, stderr } => {
                write!(f, "`{command}` failed:\n{stderr}")
            }
            KicadCliError::UnexpectedOutput { command, detail } => {
                write!(f, "`{command}` output not understood: {detail}")
            }
        }
    }
}

impl std::error::Error for KicadCliError {}

/// Handle to a verified `kicad-cli` binary.
pub struct KicadCli {
    bin: PathBuf,
    version: String,
}

impl KicadCli {
    /// Locate `kicad-cli` on PATH (or at `$PCBFORGE_KICAD_CLI`) and confirm
    /// it runs by querying its version.
    pub fn discover() -> Result<Self, KicadCliError> {
        let bin = std::env::var_os("PCBFORGE_KICAD_CLI")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("kicad-cli"));
        let out = Command::new(&bin)
            .arg("version")
            .output()
            .map_err(|e| KicadCliError::NotFound(e.to_string()))?;
        if !out.status.success() {
            return Err(KicadCliError::NotFound(format!(
                "`kicad-cli version` exited {}",
                out.status
            )));
        }
        let version = String::from_utf8_lossy(&out.stdout).trim().to_string();
        Ok(KicadCli { bin, version })
    }

    /// The version string reported by the binary.
    pub fn version(&self) -> &str {
        &self.version
    }

    /// `--help` text for a subcommand path like `["pcb", "export", "svg"]`.
    fn help(&self, subcommand: &[&str]) -> Result<String, KicadCliError> {
        let out = Command::new(&self.bin)
            .args(subcommand)
            .arg("--help")
            .output()
            .map_err(|e| KicadCliError::NotFound(e.to_string()))?;
        // kicad-cli prints help on stdout; take stderr too, defensively.
        Ok(format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        ))
    }

    /// Verify every flag in `flags` appears in the subcommand's help text.
    fn require_flags(&self, subcommand: &[&str], flags: &[&str]) -> Result<(), KicadCliError> {
        let help = self.help(subcommand)?;
        for flag in flags {
            if !help.contains(flag) {
                return Err(KicadCliError::MissingFlag {
                    subcommand: subcommand.join(" "),
                    flag: (*flag).to_string(),
                    version: self.version.clone(),
                });
            }
        }
        Ok(())
    }

    fn run(&self, args: &[&str]) -> Result<String, KicadCliError> {
        let out = Command::new(&self.bin)
            .args(args)
            .output()
            .map_err(|e| KicadCliError::NotFound(e.to_string()))?;
        let command = format!("kicad-cli {}", args.join(" "));
        if !out.status.success() {
            return Err(KicadCliError::Failed {
                command,
                stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
            });
        }
        Ok(String::from_utf8_lossy(&out.stdout).into_owned())
    }

    /// Export Gerbers for the named layers. Returns the plotted file paths
    /// (parsed from kicad-cli's own "Plotted to '…'" lines, so no filename
    /// scheme is assumed).
    pub fn export_gerbers(
        &self,
        board: &Path,
        layers: &[&str],
        out_dir: &Path,
    ) -> Result<Vec<PathBuf>, KicadCliError> {
        let sub = ["pcb", "export", "gerbers"];
        self.require_flags(&sub, &["--layers", "--output"])?;
        std::fs::create_dir_all(out_dir).ok();
        let layer_list = layers.join(",");
        let stdout = self.run(&[
            "pcb",
            "export",
            "gerbers",
            "--layers",
            &layer_list,
            board.to_str().unwrap_or_default(),
            "-o",
            out_dir.to_str().unwrap_or_default(),
        ])?;
        let files: Vec<PathBuf> = stdout
            .lines()
            .filter_map(|l| l.split_once('\'').map(|(_, rest)| rest))
            .filter_map(|rest| rest.rsplit_once('\'').map(|(path, _)| PathBuf::from(path)))
            .filter(|p| p.is_file())
            .collect();
        if files.is_empty() {
            return Err(KicadCliError::UnexpectedOutput {
                command: "kicad-cli pcb export gerbers".into(),
                detail: format!("no 'Plotted to' lines found in: {stdout}"),
            });
        }
        Ok(files)
    }

    /// Export the two Gerbers a job needs — the conductor and the board
    /// outline — from `board`, writing them into `out_dir` under stable names
    /// (`copper.gbr`, `outline.gbr`) regardless of kicad-cli's own naming.
    /// Each layer is exported on its own call so the mapping is unambiguous.
    /// Returns `(copper, outline)`.
    pub fn export_job_gerbers(
        &self,
        board: &Path,
        out_dir: &Path,
        copper_layer: &str,
        outline_layer: &str,
    ) -> Result<(PathBuf, PathBuf), KicadCliError> {
        std::fs::create_dir_all(out_dir).ok();
        let copper = self.export_one_gerber(board, copper_layer, out_dir, "copper.gbr")?;
        let outline = self.export_one_gerber(board, outline_layer, out_dir, "outline.gbr")?;
        Ok((copper, outline))
    }

    /// Export a single `layer` and move the plotted file to `out_dir/dest_name`.
    fn export_one_gerber(
        &self,
        board: &Path,
        layer: &str,
        out_dir: &Path,
        dest_name: &str,
    ) -> Result<PathBuf, KicadCliError> {
        let plotted = self.export_gerbers(board, &[layer], out_dir)?;
        let src = plotted
            .into_iter()
            .next()
            .ok_or_else(|| KicadCliError::UnexpectedOutput {
                command: format!("kicad-cli pcb export gerbers --layers {layer}"),
                detail: "no plotted file for the layer".into(),
            })?;
        let dest = out_dir.join(dest_name);
        if src != dest {
            // rename within a dir usually works; fall back to copy across fs.
            std::fs::rename(&src, &dest)
                .or_else(|_| std::fs::copy(&src, &dest).map(|_| ()))
                .map_err(|e| KicadCliError::UnexpectedOutput {
                    command: format!("move {} -> {}", src.display(), dest.display()),
                    detail: e.to_string(),
                })?;
        }
        Ok(dest)
    }

    /// Export one layer as a board-area-only, black-and-white SVG (the form
    /// the golden raster comparisons consume).
    pub fn export_svg(
        &self,
        board: &Path,
        layer: &str,
        out_file: &Path,
    ) -> Result<(), KicadCliError> {
        let sub = ["pcb", "export", "svg"];
        self.require_flags(
            &sub,
            &[
                "--layers",
                "--black-and-white",
                "--exclude-drawing-sheet",
                "--page-size-mode",
                "--output",
            ],
        )?;
        self.run(&[
            "pcb",
            "export",
            "svg",
            "--layers",
            layer,
            "--black-and-white",
            "--exclude-drawing-sheet",
            "--page-size-mode",
            "2",
            board.to_str().unwrap_or_default(),
            "-o",
            out_file.to_str().unwrap_or_default(),
        ])?;
        if !out_file.is_file() {
            return Err(KicadCliError::UnexpectedOutput {
                command: "kicad-cli pcb export svg".into(),
                detail: format!("expected output file {} missing", out_file.display()),
            });
        }
        Ok(())
    }

    /// Export the Excellon drill file(s). Returns the created paths (parsed
    /// from kicad-cli's "Created file '…'" lines).
    pub fn export_drill(
        &self,
        board: &Path,
        out_dir: &Path,
    ) -> Result<Vec<PathBuf>, KicadCliError> {
        let sub = ["pcb", "export", "drill"];
        self.require_flags(&sub, &["--output"])?;
        std::fs::create_dir_all(out_dir).ok();
        // kicad-cli 7 requires the output dir to end with a separator.
        let mut dir = out_dir.to_str().unwrap_or_default().to_string();
        if !dir.ends_with('/') {
            dir.push('/');
        }
        let stdout = self.run(&[
            "pcb",
            "export",
            "drill",
            board.to_str().unwrap_or_default(),
            "-o",
            &dir,
        ])?;
        let files: Vec<PathBuf> = stdout
            .lines()
            .filter_map(|l| l.split_once('\'').map(|(_, rest)| rest))
            .filter_map(|rest| rest.rsplit_once('\'').map(|(path, _)| PathBuf::from(path)))
            .filter(|p| p.is_file())
            .collect();
        if files.is_empty() {
            return Err(KicadCliError::UnexpectedOutput {
                command: "kicad-cli pcb export drill".into(),
                detail: format!("no 'Created file' lines found in: {stdout}"),
            });
        }
        Ok(files)
    }
}

/// Resolve a user-supplied path to a `.kicad_pcb` board file: a board file is
/// returned as-is; a directory is searched for exactly one `*.kicad_pcb`
/// (so the program can be pointed at a KiCad *project*, not just a board).
pub fn resolve_board(path: &Path) -> Result<PathBuf, KicadCliError> {
    if path.is_file() {
        return Ok(path.to_path_buf());
    }
    if path.is_dir() {
        let mut boards: Vec<PathBuf> = std::fs::read_dir(path)
            .map_err(|e| KicadCliError::NotFound(format!("{}: {e}", path.display())))?
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.extension().is_some_and(|x| x == "kicad_pcb"))
            .collect();
        boards.sort();
        return match boards.len() {
            1 => Ok(boards.remove(0)),
            0 => Err(KicadCliError::NotFound(format!(
                "no .kicad_pcb in {}",
                path.display()
            ))),
            n => Err(KicadCliError::UnexpectedOutput {
                command: format!("resolve board in {}", path.display()),
                detail: format!("{n} .kicad_pcb files — name the one you want"),
            }),
        };
    }
    Err(KicadCliError::NotFound(format!(
        "{} is not a file or directory",
        path.display()
    )))
}

/// True when `kicad-cli` is usable here — for tests that self-skip.
pub fn available() -> bool {
    KicadCli::discover().is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn repo_root() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .ancestors()
            .nth(2)
            .unwrap()
            .to_path_buf()
    }

    fn tmp_dir(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("pcbforge-ing6-{tag}-{}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn discovers_and_exports_on_a_sample_project() {
        if !available() {
            eprintln!("SKIP: kicad-cli not installed");
            return;
        }
        let cli = KicadCli::discover().unwrap();
        assert!(!cli.version().is_empty());
        let board = repo_root().join("samples/kicad/valdemo2.kicad_pcb");
        assert!(board.is_file(), "sample board missing");

        let dir = tmp_dir("gerbers");
        let gerbers = cli
            .export_gerbers(&board, &["F.Cu", "Edge.Cuts"], &dir)
            .unwrap();
        assert_eq!(gerbers.len(), 2, "one file per requested layer");
        assert!(gerbers.iter().all(|p| p.is_file()));

        let svg = tmp_dir("svg").join("fcu.svg");
        cli.export_svg(&board, "F.Cu", &svg).unwrap();
        assert!(std::fs::read_to_string(&svg).unwrap().contains("<svg"));

        let drills = cli.export_drill(&board, &tmp_dir("drill")).unwrap();
        assert!(!drills.is_empty());
        let drl = std::fs::read_to_string(&drills[0]).unwrap();
        assert!(drl.starts_with("M48"), "Excellon header expected");
    }

    #[test]
    fn export_job_gerbers_writes_stable_names() {
        if !available() {
            eprintln!("SKIP: kicad-cli not installed");
            return;
        }
        let cli = KicadCli::discover().unwrap();
        let board = repo_root().join("samples/kicad/valdemo2.kicad_pcb");
        let dir = tmp_dir("job-gerbers");
        let (copper, outline) = cli
            .export_job_gerbers(&board, &dir, "F.Cu", "Edge.Cuts")
            .unwrap();
        assert_eq!(copper, dir.join("copper.gbr"));
        assert_eq!(outline, dir.join("outline.gbr"));
        assert!(copper.is_file() && outline.is_file());
        // Both are real Gerber output (G-code-ish `%` header).
        assert!(std::fs::read_to_string(&copper).unwrap().contains('%'));
    }

    #[test]
    fn resolve_board_takes_a_file_or_a_project_dir() {
        // A board file resolves to itself.
        let board = repo_root().join("samples/kicad/valdemo2.kicad_pcb");
        assert_eq!(resolve_board(&board).unwrap(), board);
        // The samples/kicad dir has >1 board → ambiguous, a named error.
        let dir = repo_root().join("samples/kicad");
        assert!(resolve_board(&dir).is_err());
        // A dir with exactly one board resolves to it.
        let one = tmp_dir("one-board");
        let only = one.join("proj.kicad_pcb");
        std::fs::copy(&board, &only).unwrap();
        assert_eq!(resolve_board(&one).unwrap(), only);
        // A missing path errors.
        assert!(resolve_board(Path::new("/no/such/path")).is_err());
    }

    #[test]
    fn missing_flag_is_a_named_error() {
        if !available() {
            eprintln!("SKIP: kicad-cli not installed");
            return;
        }
        let cli = KicadCli::discover().unwrap();
        let err = cli
            .require_flags(&["pcb", "export", "svg"], &["--flag-that-will-never-exist"])
            .expect_err("bogus flag must be reported");
        let msg = err.to_string();
        assert!(
            msg.contains("--flag-that-will-never-exist") && msg.contains("pcb export svg"),
            "error must name flag and subcommand: {msg}"
        );
    }
}
