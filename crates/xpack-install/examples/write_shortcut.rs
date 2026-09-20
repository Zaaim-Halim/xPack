//! Writes one `.lnk` file, so that CI can ask Windows to resolve it.
//!
//! The shortcut writer is checked field by field by unit tests that run on
//! every platform. Those tests prove the bytes match the specification; they
//! cannot prove that the Windows shell agrees. This example exists so a
//! Windows CI job can write a real shortcut and read it back through the same
//! COM object Explorer uses — which is the only check that actually answers
//! the question.
//!
//! An example rather than a test because it has to produce a file at a path
//! the job chooses, and because it is useful to run by hand when changing the
//! serialiser.
//!
//! ```text
//! cargo run -p xpack-install --example write_shortcut -- <output.lnk> <target.exe>
//! ```

use std::path::Path;
use std::process::ExitCode;

use xpack_install::integration::lnk::Shortcut;

fn main() -> ExitCode {
    let arguments: Vec<String> = std::env::args().skip(1).collect();
    let [output, target] = arguments.as_slice() else {
        eprintln!("usage: write_shortcut <output.lnk> <target>");
        return ExitCode::FAILURE;
    };

    let shortcut = Shortcut::new(Path::new(target)).with_description(Some("written by xPack"));

    match std::fs::write(output, shortcut.to_bytes()) {
        Ok(()) => {
            println!("wrote {output} pointing at {target}");
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("could not write {output}: {error}");
            ExitCode::FAILURE
        }
    }
}
