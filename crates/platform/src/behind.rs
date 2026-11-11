//! Keeping a program's windows behind the person's work while we picture it (Windows).
//!
//! A check starts a build to take its picture while the person works in other windows. Builds are
//! asked to open that window behind and without focus, but one made before that, or one that
//! doesn't, comes up on top, where a stray click lands in it. Starting a program without showing
//! it in front needs `STARTUPINFO`'s show mode, which std has only unstable
//! (`CommandExt::show_window`), and this workspace forbids `unsafe`. So a PowerShell helper does
//! it from outside, the way [`crate::keep_awake`] holds the computer awake: it lowers each window
//! of the program's process tree to the bottom without activating it, as soon as it shows.

use crate::process::{Process, RunningProcess, StdinMode};
use std::process::ChildStdin;
use std::time::Duration;

/// How long the helper may take to be ready (PowerShell compiles its Windows calls first, about
/// a second); the program waits for it, so a helper that hasn't started by then is given up on.
const READY_WITHIN: Duration = Duration::from_secs(15);

/// The helper, ready for the program's process id. It ends by itself when that program or this
/// process ends; dropping it stops it at once.
pub(crate) struct KeepBehind {
    helper: RunningProcess,
    stdin: Option<ChildStdin>,
}

impl KeepBehind {
    /// Start the helper for a program that may run for `limit`, and wait until it's ready. `None`
    /// off Windows, or when the helper couldn't start (logged): the program then opens as it
    /// would have anyway, which never fails a run.
    pub(crate) fn start(limit: Duration) -> Option<KeepBehind> {
        if !cfg!(windows) {
            return None;
        }
        start_for(std::process::id(), limit)
    }

    /// Keep process `pid`'s windows (and its descendants') behind from now on.
    pub(crate) fn watch(&mut self, pid: u32) {
        use std::io::Write as _;
        if let Some(mut stdin) = self.stdin.take() {
            if let Err(e) = writeln!(stdin, "{pid}").and_then(|()| stdin.flush()) {
                log::warn!("couldn't hand process {pid} to the keep-behind helper: {e}");
            }
            // Kept open: the helper reads one line, and an early end of input would stop it.
            self.stdin = Some(stdin);
        }
    }
}

#[cfg(windows)]
fn start_for(owner: u32, limit: Duration) -> Option<KeepBehind> {
    use base64::Engine as _;
    const SCRIPT: &str = include_str!("behind_windows.ps1");
    let script = SCRIPT.replace("OWNER_PID", &owner.to_string());
    // -EncodedCommand takes the script as base64 of UTF-16LE, so nothing in it needs quoting.
    let utf16: Vec<u8> = script.encode_utf16().flat_map(u16::to_le_bytes).collect();
    let encoded = base64::engine::general_purpose::STANDARD.encode(utf16);
    let windir = std::env::var_os("SystemRoot").map_or_else(|| std::path::PathBuf::from(r"C:\Windows"), std::path::PathBuf::from);
    let shell = windir.join(r"System32\WindowsPowerShell\v1.0\powershell.exe");
    let (ready, is_ready) = std::sync::mpsc::sync_channel(1);
    let helper = Process::new(shell)
        .args(["-NoProfile", "-NonInteractive", "-ExecutionPolicy", "Bypass", "-EncodedCommand", &encoded])
        .stdin(StdinMode::Piped)
        .timeout(limit.saturating_add(Duration::from_secs(60)))
        .label("keep behind")
        .on_stdout_line(move |line| {
            if line.trim_start_matches('\u{feff}').trim() == "ready" {
                let _ = ready.try_send(());
            }
        })
        .spawn();
    let mut helper = match helper {
        Ok(h) => h,
        Err(e) => {
            log::warn!("couldn't start the helper that keeps a check's window behind: {}", e.message);
            return None;
        }
    };
    if is_ready.recv_timeout(READY_WITHIN).is_err() {
        let why = match helper.wait_timeout(Duration::ZERO) {
            Some(Ok(out)) => format!("it ended: {}", crate::process::output_excerpt(&out.stderr_text())),
            Some(Err(e)) => e.message.clone(),
            None => format!("not ready after {} s", READY_WITHIN.as_secs()),
        };
        log::warn!("the helper that keeps a check's window behind didn't start, so the window may open on top: {why}");
        return None;
    }
    let stdin = helper.take_stdin();
    Some(KeepBehind { helper, stdin })
}

#[cfg(not(windows))]
fn start_for(_owner: u32, _limit: Duration) -> Option<KeepBehind> {
    None
}

impl std::fmt::Debug for KeepBehind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("KeepBehind").field("helper", &self.helper.pid()).finish()
    }
}

#[cfg(all(test, windows))]
mod tests {
    use super::*;

    /// The helper's Windows calls compile and it says it's ready; it ends by itself when the
    /// program it watches ends, and when its owner ends before handing it one. No window is
    /// opened: tests never start GUI programs.
    #[test]
    fn the_helper_starts_and_ends_with_the_program_it_watches() {
        let sleeper =
            || Process::new("powershell").args(["-NoProfile", "-NonInteractive", "-Command", "Start-Sleep -Seconds 60"]).timeout(Duration::from_secs(90));
        let mut program = sleeper().spawn().expect("the stand-in program starts");
        let mut behind = KeepBehind::start(Duration::from_secs(60)).expect("the helper is ready");
        behind.watch(program.pid());
        assert!(behind.helper.wait_timeout(Duration::from_secs(2)).is_none(), "watching while the program runs");
        assert!(program.kill_and_wait(Duration::from_secs(5)), "the stand-in program stops");
        assert!(behind.helper.wait_timeout(Duration::from_secs(10)).is_some(), "the helper ends with the program");

        let mut owner = sleeper().spawn().expect("the stand-in owner starts");
        let mut orphan = start_for(owner.pid(), Duration::from_secs(60)).expect("the helper is ready");
        assert!(owner.kill_and_wait(Duration::from_secs(5)), "the stand-in owner stops");
        assert!(orphan.helper.wait_timeout(Duration::from_secs(10)).is_some(), "the helper ends with its owner");
    }
}
