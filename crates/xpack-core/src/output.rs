//! Printing a report that nobody may be reading.
//!
//! `println!` panics when stdout is gone: a pipe into `head` that has read
//! enough, a terminal that closed, a log collector that died. For a program
//! that prints while it works, that turns "nobody is reading the report" into
//! "the work stopped halfway" — an installer that panics between two steps
//! leaves an installation behind that neither version finished.
//!
//! So xPack's binaries print through [`outln!`](crate::outln) and
//! [`errln!`](crate::errln), which drop a line that cannot be written. The
//! exit status still reports what happened, which is where a script looks.

use std::fmt;
use std::io::Write;

/// Writes one line, discarding any failure to write it.
///
/// Every error is dropped, not only a broken pipe. A full disk under a
/// redirected stdout is the same situation: the report is lost, and the work
/// it describes should finish regardless.
pub fn write_line(writer: &mut impl Write, arguments: fmt::Arguments<'_>) {
    let _ = writer.write_fmt(arguments).and_then(|()| writer.write_all(b"\n"));
}

/// `println!` that never panics. See the [module](crate::output) documentation.
#[macro_export]
macro_rules! outln {
    () => {
        $crate::output::write_line(&mut ::std::io::stdout().lock(), ::std::format_args!(""))
    };
    ($($argument:tt)*) => {
        $crate::output::write_line(
            &mut ::std::io::stdout().lock(),
            ::std::format_args!($($argument)*),
        )
    };
}

/// `eprintln!` that never panics. See the [module](crate::output) documentation.
#[macro_export]
macro_rules! errln {
    () => {
        $crate::output::write_line(&mut ::std::io::stderr().lock(), ::std::format_args!(""))
    };
    ($($argument:tt)*) => {
        $crate::output::write_line(
            &mut ::std::io::stderr().lock(),
            ::std::format_args!($($argument)*),
        )
    };
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A reader that has gone away.
    struct Closed;

    impl Write for Closed {
        fn write(&mut self, _: &[u8]) -> std::io::Result<usize> {
            Err(std::io::Error::from(std::io::ErrorKind::BrokenPipe))
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Err(std::io::Error::from(std::io::ErrorKind::BrokenPipe))
        }
    }

    #[test]
    fn a_line_nobody_can_read_is_dropped_without_panicking() {
        write_line(&mut Closed, format_args!("installed {}", "1.0.0"));
    }

    #[test]
    fn a_line_is_written_whole_with_its_newline() {
        let mut out = Vec::new();
        write_line(&mut out, format_args!("{} {}", "installed", 42));
        write_line(&mut out, format_args!(""));
        assert_eq!(out, b"installed 42\n\n");
    }

    #[test]
    fn the_macros_accept_everything_println_does() {
        // Compiled, not run: they write to the real streams, which the test
        // harness does not capture. The forms are the ones the binaries use.
        let version = "1.0.0";
        let _ = || {
            crate::outln!();
            crate::outln!("plain");
            crate::outln!("installed {version}");
            crate::errln!("{} of {}", 1, 2);
            crate::errln!();
        };
    }
}
