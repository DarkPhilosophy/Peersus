//! `peersus-diag watch <program> <folder>`: opt-in crash capture for the program that started it.
//!
//! A crashed process cannot be trusted to describe its own crash: its heap, stack or locks may be what broke. So the
//! program that wants capture (the owner, the window) starts this helper, allows it as its tracer, and the helper watches
//! it from outside with `ptrace(2)`. Nothing of crash handling is compiled into the watched program: it starts a process
//! and names it as its tracer, nothing more.
//!
//! While the watched program runs normally the helper sleeps in `waitpid`; only signals delivered to the program wake it.
//! When a signal arrives that would end the program (a segmentation fault, abort, bus error, illegal instruction,
//! arithmetic fault or bad system call that no handler of the program catches), the helper stops every thread, writes a
//! **minidump** of the stopped process (the format Firefox and Chromium use; `minidump-stackwalk` reads it) and a short
//! text beside it, then lets the program go on, so that it ends exactly as it would have without the helper. A signal the
//! program catches is handed back to it untouched: Rust's own handler, for example, turns a stack overflow into an abort,
//! and the abort is what is captured.
//!
//! The helper watches only its parent, never a process it is told about. It leaves when the program ends, replaces itself
//! (`exec`), or the helper is stopped; a tracer that goes away releases the program at once.
//!
//! Protocol on the standard streams: the helper reads one line (the parent has named it as tracer), attaches, and answers
//! `ready`, or `denied` when the system refuses it (Yama `ptrace_scope` 2 or 3, a debugger already attached).

#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "linux")]
mod report;

#[cfg(target_os = "linux")]
fn main() -> std::process::ExitCode {
    linux::main()
}

#[cfg(not(target_os = "linux"))]
fn main() -> std::process::ExitCode {
    println!("unsupported");
    std::process::ExitCode::from(2)
}
