//! A console entry point makes interactive Windows shells wait for the result.
//! The adjacent GUI binary owns the CLI grammar and the local IPC protocol.
use std::io::{self, Write};
use std::process::{Command, ExitCode, Stdio};

fn run() -> io::Result<ExitCode> {
    let manager = std::env::current_exe()?.with_file_name("rustile.exe");
    let output = Command::new(&manager)
        .arg("--cli")
        .args(std::env::args_os().skip(1))
        .stdin(Stdio::null())
        .output()
        .map_err(|err| io::Error::new(err.kind(), format!("{}: {err}", manager.display())))?;
    io::stdout().write_all(&output.stdout)?;
    io::stderr().write_all(&output.stderr)?;
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
