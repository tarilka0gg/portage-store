//! A seam between "run a subprocess" and everything this app does with
//! the result — most modules under `src/portage/` never needed this: they
//! already separate a thin `Command`-running wrapper from a pure parsing
//! function, and every existing test feeds the parser a hand-written
//! string fixture, never a real subprocess. This trait exists for the two
//! places that pattern doesn't reach on its own: `priv_write.rs` (no
//! separate "parse" step to test at all — it's the write itself that
//! matters, and it had zero tests of any kind) and `reverse_deps.rs`
//! (`equery` calls interleaved *recursively* with tree construction,
//! where the thing worth testing — does the recursion actually respect
//! its own depth/fan-out/call-budget limits — can't be exercised by
//! swapping in a fixture string once).
use std::io;
use std::process::Output;

/// Runs a subprocess and waits for it to finish. Deliberately narrow —
/// covers exactly the two shapes `priv_write.rs`/`reverse_deps.rs` need
/// (a plain call, and one with content piped to stdin), not every
/// possible `Command` configuration. `emerge::run`'s async, incrementally
/// streamed job output is a different shape entirely (a live child
/// process read line-by-line as it runs, not a single final `Output`) and
/// deliberately isn't covered here — forcing it through this trait would
/// either weaken it for every other caller or need a second, more
/// complex trait for that one call site.
pub trait CommandRunner {
    fn output(&self, program: &str, args: &[&str]) -> io::Result<Output>;

    fn output_with_stdin(&self, program: &str, args: &[&str], stdin: &[u8]) -> io::Result<Output>;
}

/// The real thing — every non-test caller uses this.
pub struct RealCommandRunner;

impl CommandRunner for RealCommandRunner {
    fn output(&self, program: &str, args: &[&str]) -> io::Result<Output> {
        std::process::Command::new(program).args(args).output()
    }

    fn output_with_stdin(&self, program: &str, args: &[&str], stdin: &[u8]) -> io::Result<Output> {
        use std::io::Write;
        use std::process::Stdio;
        let mut child = std::process::Command::new(program)
            .args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()?;
        child.stdin.take().expect("piped stdin").write_all(stdin)?;
        child.wait_with_output()
    }
}

#[cfg(test)]
pub mod fake {
    use super::*;
    use std::cell::RefCell;
    use std::collections::HashMap;
    use std::os::unix::process::ExitStatusExt;
    use std::process::ExitStatus;

    fn exit_status(code: i32) -> ExitStatus {
        ExitStatus::from_raw(code << 8)
    }

    pub fn ok(stdout: &str) -> Output {
        Output { status: exit_status(0), stdout: stdout.as_bytes().to_vec(), stderr: Vec::new() }
    }

    pub fn err(stderr: &str) -> Output {
        Output { status: exit_status(1), stdout: Vec::new(), stderr: stderr.as_bytes().to_vec() }
    }

    /// Canned responses keyed by program name only (not full argv) — real
    /// captured invocations in this app vary their arguments per call
    /// (a path, an atom, a commit message), and matching on the program
    /// alone is enough for every test this backs today; a test that needs
    /// to distinguish two calls to the *same* program can layer its own
    /// `RefCell<Vec<Output>>` queue on top instead of this map growing a
    /// second matching dimension it mostly wouldn't use.
    pub struct FakeCommandRunner {
        pub responses: RefCell<HashMap<String, Output>>,
        /// Every `(program, args)` pair actually requested, in order —
        /// lets a test assert not just "what would this return" but "did
        /// the code under test actually ask for the right thing."
        pub calls: RefCell<Vec<(String, Vec<String>)>>,
    }

    impl FakeCommandRunner {
        pub fn new() -> Self {
            Self { responses: RefCell::new(HashMap::new()), calls: RefCell::new(Vec::new()) }
        }

        pub fn respond(&self, program: &str, output: Output) {
            self.responses.borrow_mut().insert(program.to_string(), output);
        }

        fn record_and_respond(&self, program: &str, args: &[&str]) -> io::Result<Output> {
            self.calls.borrow_mut().push((program.to_string(), args.iter().map(|a| a.to_string()).collect()));
            self.responses
                .borrow()
                .get(program)
                .cloned()
                .ok_or_else(|| io::Error::other(format!("FakeCommandRunner: no response configured for {program}")))
        }
    }

    impl Default for FakeCommandRunner {
        fn default() -> Self {
            Self::new()
        }
    }

    impl CommandRunner for FakeCommandRunner {
        fn output(&self, program: &str, args: &[&str]) -> io::Result<Output> {
            self.record_and_respond(program, args)
        }

        fn output_with_stdin(&self, program: &str, args: &[&str], _stdin: &[u8]) -> io::Result<Output> {
            self.record_and_respond(program, args)
        }
    }
}
