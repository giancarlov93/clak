//! `clak`: the command line an agent uses to drive the programs connected to Clak.
//! All the work is in the library; here only the exit code.

use std::process::ExitCode;

fn main() -> ExitCode {
    let exit = clak_agent::cli::main();
    ExitCode::from(u8::try_from(exit).unwrap_or(1))
}
