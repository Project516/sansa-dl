//! Running a child process that can be cancelled.

use rustix::process::{Pid, Signal, kill_process_group, set_parent_process_death_signal};
use std::io::{self, BufRead, BufReader, Read};
use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicI32, Ordering};

#[derive(Default)]
pub struct Cancel {
    flag: AtomicBool,
    pid: AtomicI32,
}

impl Cancel {
    pub fn is_cancelled(&self) -> bool {
        self.flag.load(Ordering::SeqCst)
    }

    /// Stops the child and everything it started.
    pub fn cancel(&self) {
        self.flag.store(true, Ordering::SeqCst);
        self.kill();
    }

    fn kill(&self) {
        if let Some(pid) = Pid::from_raw(self.pid.load(Ordering::SeqCst)) {
            let _ = kill_process_group(pid, Signal::TERM);
        }
    }
}

pub struct Output {
    pub success: bool,
    pub stderr: String,
}

/// Runs `program`, calling `on_line` for each stdout line. The child leads its
/// own process group so a cancel reaches its helpers too.
pub fn run(
    program: &Path,
    args: &[String],
    cancel: &Cancel,
    mut on_line: impl FnMut(&str),
) -> io::Result<Output> {
    let mut command = Command::new(program);
    command
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .process_group(0);
    // SAFETY: only the prctl syscall runs between fork and exec.
    unsafe {
        command.pre_exec(|| Ok(set_parent_process_death_signal(Some(Signal::TERM))?));
    }
    let mut child = command.spawn()?;
    cancel
        .pid
        .store(i32::try_from(child.id()).unwrap_or(0), Ordering::SeqCst);
    if cancel.is_cancelled() {
        cancel.kill();
    }
    let mut stderr = child.stderr.take().expect("piped");
    let errors = std::thread::spawn(move || {
        let mut text = String::new();
        let _ = stderr.read_to_string(&mut text);
        text
    });
    let stdout = child.stdout.take().expect("piped");
    let mut reader = BufReader::new(stdout);
    let mut line = String::new();
    loop {
        line.clear();
        match reader.read_line(&mut line) {
            Ok(0) | Err(_) => break,
            Ok(_) => on_line(&line),
        }
    }
    let status = child.wait()?;
    cancel.pid.store(0, Ordering::SeqCst);
    Ok(Output {
        success: status.success(),
        stderr: errors.join().unwrap_or_default(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sh(script: &str) -> (std::path::PathBuf, Vec<String>) {
        ("/bin/sh".into(), vec!["-c".into(), script.into()])
    }

    #[test]
    fn collects_lines_and_stderr() {
        let (program, args) = sh("echo one; echo two; echo oops >&2; exit 3");
        let mut lines = Vec::new();
        let out = run(&program, &args, &Cancel::default(), |l| {
            lines.push(l.trim().to_string())
        })
        .unwrap();
        assert_eq!(lines, ["one", "two"]);
        assert!(!out.success);
        assert_eq!(out.stderr.trim(), "oops");
    }

    #[test]
    fn cancel_stops_a_long_command_and_its_children() {
        let (program, args) = sh("echo started; sleep 30; echo late");
        let cancel = std::sync::Arc::new(Cancel::default());
        let handle = {
            let cancel = cancel.clone();
            std::thread::spawn(move || {
                let mut lines = Vec::new();
                let out = run(&program, &args, &cancel, |l| {
                    lines.push(l.trim().to_string())
                })
                .unwrap();
                (lines, out.success)
            })
        };
        std::thread::sleep(std::time::Duration::from_millis(300));
        let begin = std::time::Instant::now();
        cancel.cancel();
        let (lines, success) = handle.join().unwrap();
        assert!(begin.elapsed() < std::time::Duration::from_secs(5));
        assert_eq!(lines, ["started"]);
        assert!(!success);
    }

    #[test]
    fn missing_program_is_an_error() {
        let err = run(
            Path::new("/nonexistent/tool"),
            &[],
            &Cancel::default(),
            |_| {},
        )
        .err();
        assert!(err.is_some());
    }
}
