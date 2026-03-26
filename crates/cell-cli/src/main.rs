use std::io::{self, Write};
use std::process::ExitCode;

fn main() -> ExitCode {
    let args = std::env::args().skip(1).collect::<Vec<_>>();
    let parsed = cell_cli::parse_args(&args);

    if parsed.mode == Some(cell_protocol::OutputMode::Rpc) {
        return match cell_cli::run_rpc_stdio(&args) {
            Ok(exit_code) => ExitCode::from(exit_code as u8),
            Err(error) => {
                let _ = writeln!(io::stderr(), "{error}");
                ExitCode::from(1)
            }
        };
    }

    match cell_cli::run(&args) {
        Ok(cell_cli::RunResult::Completed {
            exit_code,
            stdout,
            stderr,
        }) => {
            if let Some(stdout) = stdout {
                print!("{stdout}");
                if !stdout.ends_with('\n') {
                    println!();
                }
            }
            if let Some(stderr) = stderr {
                let _ = writeln!(io::stderr(), "{stderr}");
            }
            ExitCode::from(exit_code as u8)
        }
        Err(error) => {
            let _ = writeln!(io::stderr(), "{error}");
            ExitCode::from(1)
        }
    }
}
