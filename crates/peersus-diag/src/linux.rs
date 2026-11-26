//! Watching the parent with `ptrace(2)` and capturing it when a signal would end it (see the crate's words).
use crate::report;
use nix::{
    errno::Errno,
    sys::{
        ptrace::{self, Options},
        signal::{Signal, kill},
        wait::{WaitPidFlag, WaitStatus, waitpid},
    },
    unistd::{Pid, getppid},
};
use std::{
    collections::HashSet,
    fs,
    io::{BufRead, Write},
    path::{Path, PathBuf},
    process::ExitCode,
    time::{Duration, Instant},
};

/// How long the other threads get to stop before the dump is written with those that did.
const STOP_WAIT: Duration = Duration::from_secs(3);

pub fn main() -> ExitCode {
    let arguments: Vec<String> = std::env::args().skip(1).collect();
    let [command, program, folder] = arguments.as_slice() else {
        eprintln!("usage: peersus-diag watch <program> <folder>");
        return ExitCode::from(64);
    };
    // A program name becomes part of a file name: only a plain word is taken.
    if command != "watch"
        || program.is_empty()
        || !program
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-')
    {
        eprintln!("usage: peersus-diag watch <program> <folder>");
        return ExitCode::from(64);
    }
    let folder = PathBuf::from(folder);
    if report::private_folder(&folder).is_err() {
        answer("denied folder");
        return ExitCode::from(1);
    }
    // The parent names this process as its tracer, then says so with one line.
    let mut line = String::new();
    if std::io::stdin().lock().read_line(&mut line).is_err() || line.trim() != "traced" {
        return ExitCode::from(1);
    }
    let parent = getppid();
    let mut watch = Watch {
        process: parent,
        program: program.clone(),
        folder,
        threads: HashSet::new(),
    };
    if let Err(error) = watch.attach() {
        answer(&format!("denied {error}"));
        return ExitCode::from(1);
    }
    answer("ready");
    watch.run();
    ExitCode::SUCCESS
}

fn answer(line: &str) {
    let mut out = std::io::stdout().lock();
    let _ = writeln!(out, "{line}");
    let _ = out.flush();
}

struct Watch {
    process: Pid,
    program: String,
    folder: PathBuf,
    threads: HashSet<Pid>,
}

/// The threads of `process` now, from `/proc`.
fn threads_of(process: Pid) -> Vec<Pid> {
    fs::read_dir(format!("/proc/{process}/task"))
        .map(|entries| {
            entries
                .filter_map(|entry| entry.ok()?.file_name().to_str()?.parse().ok())
                .map(Pid::from_raw)
                .collect()
        })
        .unwrap_or_default()
}

/// Whether `process` has a handler of its own for `signal` (the `SigCgt` mask in `/proc`): the program will deal with it.
fn caught(process: Pid, signal: Signal) -> bool {
    let Ok(status) = fs::read_to_string(format!("/proc/{process}/status")) else {
        return false;
    };
    status
        .lines()
        .find_map(|line| line.strip_prefix("SigCgt:"))
        .and_then(|mask| u64::from_str_radix(mask.trim(), 16).ok())
        .is_some_and(|mask| mask & (1u64 << (signal as i32 - 1)) != 0)
}

/// The signals whose default action ends the program with a core: the crashes worth a dump.
fn crash_signal(signal: Signal) -> bool {
    matches!(
        signal,
        Signal::SIGSEGV
            | Signal::SIGBUS
            | Signal::SIGILL
            | Signal::SIGFPE
            | Signal::SIGABRT
            | Signal::SIGSYS
    )
}

fn thread_name(thread: Pid) -> String {
    fs::read_to_string(format!("/proc/{thread}/comm"))
        .map(|name| {
            name.trim()
                .chars()
                .filter(|c| !c.is_control())
                .take(16)
                .collect()
        })
        .unwrap_or_default()
}

impl Watch {
    /// Attaches to every thread of the process, until a pass over `/proc` finds none new. Threads made later are
    /// attached by the kernel (`PTRACE_O_TRACECLONE`). Seizing does not stop a thread.
    fn attach(&mut self) -> Result<(), Errno> {
        let options = Options::PTRACE_O_TRACECLONE | Options::PTRACE_O_TRACEEXEC;
        ptrace::seize(self.process, options)?;
        self.threads.insert(self.process);
        loop {
            let mut fresh = false;
            for thread in threads_of(self.process) {
                if self.threads.contains(&thread) {
                    continue;
                }
                match ptrace::seize(thread, options) {
                    Ok(()) => {
                        self.threads.insert(thread);
                        fresh = true;
                    }
                    // Gone between the listing and the call.
                    Err(Errno::ESRCH) => {}
                    Err(error) => return Err(error),
                }
            }
            if !fresh {
                return Ok(());
            }
        }
    }

    fn run(&mut self) {
        loop {
            let status = match waitpid(None, Some(WaitPidFlag::__WALL)) {
                Ok(status) => status,
                Err(Errno::EINTR) => continue,
                // No traced thread is left.
                Err(_) => return,
            };
            match status {
                WaitStatus::PtraceEvent(thread, _, event) if event == libc::PTRACE_EVENT_EXEC => {
                    // The program replaced itself: the new one starts its own helper if it wants one.
                    self.release_all(thread);
                    return;
                }
                WaitStatus::PtraceEvent(thread, _, event) => {
                    if event == libc::PTRACE_EVENT_CLONE
                        && let Ok(new) = ptrace::getevent(thread)
                        && let Ok(new) = i32::try_from(new)
                    {
                        self.threads.insert(Pid::from_raw(new));
                    }
                    // A new thread's first stop, a clone, or a group stop: carry on. (ptrace offers no safe way to keep
                    // a group stop while traced, so a SIGSTOP reaches the program as a pause that ends at once.)
                    let _ = ptrace::cont(thread, None);
                }
                WaitStatus::Stopped(thread, signal)
                    if crash_signal(signal) && !caught(self.process, signal) =>
                {
                    self.capture(thread, signal);
                    return;
                }
                WaitStatus::Stopped(thread, signal) => {
                    let _ = ptrace::cont(thread, signal);
                }
                WaitStatus::Exited(thread, _) | WaitStatus::Signaled(thread, _, _) => {
                    self.threads.remove(&thread);
                    if self.threads.is_empty() {
                        return;
                    }
                }
                _ => {}
            }
        }
    }

    /// Lets every thread go and stops watching.
    fn release_all(&mut self, stopped: Pid) {
        for thread in self.threads.iter().filter(|thread| **thread != stopped) {
            let _ = ptrace::detach(*thread, None);
        }
        let _ = ptrace::detach(stopped, None);
    }

    /// `blamed` is stopped with a signal that will end the program. Stops the whole program, hands it to the minidump
    /// writer stopped, then lets it go on to end as it would have.
    fn capture(&mut self, blamed: Pid, signal: Signal) {
        let code = ptrace::getsiginfo(blamed).map_or(0, |info| info.si_code);
        let thread = thread_name(blamed);
        // A group stop: every other thread stops where it is. The crashed thread stays in its signal stop.
        let _ = kill(self.process, Signal::SIGSTOP);
        let mut stopped = HashSet::new();
        let deadline = Instant::now() + STOP_WAIT;
        while stopped.len() < self.threads.len() - 1 && Instant::now() < deadline {
            match waitpid(None, Some(WaitPidFlag::__WALL)) {
                // The thread that took the SIGSTOP: let it start the group stop.
                Ok(WaitStatus::Stopped(thread, Signal::SIGSTOP)) => {
                    let _ = ptrace::cont(thread, Signal::SIGSTOP);
                }
                Ok(WaitStatus::PtraceEvent(thread, _, event))
                    if event == libc::PTRACE_EVENT_STOP =>
                {
                    stopped.insert(thread);
                }
                Ok(WaitStatus::PtraceEvent(thread, _, event)) => {
                    if event == libc::PTRACE_EVENT_CLONE
                        && let Ok(new) = ptrace::getevent(thread)
                        && let Ok(new) = i32::try_from(new)
                    {
                        self.threads.insert(Pid::from_raw(new));
                    }
                    let _ = ptrace::cont(thread, None);
                }
                Ok(WaitStatus::Stopped(thread, other)) => {
                    let _ = ptrace::cont(thread, other);
                }
                Ok(WaitStatus::Exited(thread, _) | WaitStatus::Signaled(thread, _, _)) => {
                    self.threads.remove(&thread);
                }
                Ok(_) | Err(Errno::EINTR) => {}
                Err(_) => break,
            }
        }
        // Detached, a thread in the group stop stays stopped; the crashed one joins the stop before it runs again. The
        // writer then attaches to the stopped threads itself.
        self.release_all(blamed);
        let path = report::dump_path(&self.folder, &self.program);
        let written = write_dump(&path, self.process, blamed, signal, code);
        let summary = report::summary(&report::Crash {
            program: &self.program,
            signal: signal.as_str(),
            code,
            thread: &thread,
            written,
        });
        let _ = report::private_file(&path.with_extension("txt"))
            .and_then(|mut file| file.write_all(summary.as_bytes()));
        report::keep_newest(&self.folder);
        // The signal was held back while the dump was written. It is sent again, now that nothing traces the program and
        // no handler of it catches the signal, so the program ends exactly as it would have: a fault would recur by itself
        // when its instruction runs again, but a signal sent by `kill` or `raise` would not.
        let _ = kill(self.process, signal);
        let _ = kill(self.process, Signal::SIGCONT);
    }
}

fn write_dump(
    path: &Path,
    process: Pid,
    blamed: Pid,
    signal: Signal,
    code: i32,
) -> Result<(), String> {
    let mut file = report::private_file(path).map_err(|error| error.to_string())?;
    let mut writer = minidump_writer::minidump_writer::MinidumpWriterConfig::new(
        process.as_raw(),
        blamed.as_raw(),
    );
    writer
        .set_minidump_size_limit(report::SIZE_LIMIT)
        .stop_timeout(STOP_WAIT);
    let result = writer
        .write(&mut file)
        .map(|_| ())
        .map_err(|error| error.to_string());
    if file.metadata().is_ok_and(|meta| meta.len() > 0) {
        let _ = report::mark_signal(&mut file, signal as u32, code);
    }
    result
}
