//! A console entry point makes interactive Windows shells wait for the result.
//! The adjacent GUI binary owns the CLI grammar and the local IPC protocol.
#[path = "../cli_error.rs"]
mod cli_error;

use std::io::{self, Write};
use std::process::{Command, ExitCode, Stdio};

fn run() -> Result<ExitCode, String> {
    let manager = std::env::current_exe()
        .map_err(|err| format!("Could not locate rustilectl.exe: {}", cli_error::io(err)))?
        .with_file_name("rustile.exe");
    let output = Command::new(&manager)
        .arg("--cli")
        .args(std::env::args_os().skip(1))
        .stdin(Stdio::null())
        .output()
        .map_err(|err| {
            format!(
                "Could not run {}: {}",
                manager.display(),
                cli_error::io(err)
            )
        })?;
    io::stdout()
        .write_all(&output.stdout)
        .map_err(cli_error::io)?;
    io::stderr()
        .write_all(&output.stderr)
        .map_err(cli_error::io)?;
    Ok(if output.status.success() {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    })
}

fn main() -> ExitCode {
    run().unwrap_or_else(|err| {
        eprintln!("rustilectl: {err}");
        ExitCode::FAILURE
    })
}
