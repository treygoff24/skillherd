//! Parse, resolve, dispatch, report. Nothing else lives here.

use std::io::{IsTerminal, Write};
use std::process::ExitCode;

use clap::Parser;

use skillherd::cli::Cli;
use skillherd::error::{EXIT_INTERNAL, Error};
use skillherd::{commands, human, output, paths::Paths};

fn main() -> ExitCode {
    let code = real_main();
    ExitCode::from(u8::try_from(code).unwrap_or(EXIT_INTERNAL as u8))
}

fn real_main() -> i32 {
    // Clap's global flags otherwise consume matching flags after `npx` too.
    // Split only at the first command, never at a value or a later argument.
    let mut argv: Vec<_> = std::env::args_os().collect();
    let mut npx_args = Vec::new();
    let mut i = 1;
    while i < argv.len() {
        if ["--pool", "--pool-remote", "--correlation-id"]
            .iter()
            .any(|flag| argv[i] == *flag)
        {
            i += 2;
        } else if argv[i] == "npx" {
            npx_args = argv.split_off(i + 1);
            break;
        } else if !argv[i].to_string_lossy().starts_with('-') || argv[i] == "--" {
            break;
        } else {
            i += 1;
        }
    }
    let mut cli = match Cli::try_parse_from(argv) {
        Ok(cli) => cli,
        Err(err) => match err.kind() {
            // Help and version are intentionally human-readable.
            clap::error::ErrorKind::DisplayHelp | clap::error::ErrorKind::DisplayVersion => {
                err.exit()
            }
            _ => {
                let pretty = std::env::args().any(|arg| arg == "--pretty");
                let error = Error::usage(err.to_string().trim());
                let _ = output::write_error(&error, None, pretty);
                return error.exit_code();
            }
        },
    };
    if let skillherd::cli::Command::Npx(args) = &mut cli.command {
        args.args = npx_args;
    }
    output::set_correlation_id(cli.global.correlation_id.clone());
    let pretty = cli.global.pretty;

    // Schema describes the command contract, not this machine. It must work
    // without HOME, local configuration, or a filesystem layout.
    if let skillherd::cli::Command::Schema(args) = &cli.command {
        let result = commands::schema::run(args, &Paths::fixture("/"), cli.global.dry_run);
        return match result {
            Ok(data) => {
                match output::write_success(&output::success_envelope(data, None), pretty) {
                    Ok(()) => 0,
                    Err(err) if err.kind() == std::io::ErrorKind::BrokenPipe => 0,
                    Err(err) => {
                        let error = Error::io("writing schema to stdout", &err);
                        let _ = output::write_error(&error, None, pretty);
                        error.exit_code()
                    }
                }
            }
            Err(error) => {
                let _ = output::write_error(&error, None, pretty);
                error.exit_code()
            }
        };
    }

    // The npx adapter preserves upstream streams and needs no skill library.
    if let skillherd::cli::Command::Npx(args) = &cli.command {
        return match commands::npx::run(args, &cli.global) {
            Err(error) => {
                let _ = output::write_error(&error, None, pretty);
                error.exit_code()
            }
            Ok(_) => EXIT_INTERNAL, // successful exec never returns
        };
    }

    let paths = match Paths::resolve_pool(
        cli.global.pool.as_deref(),
        cli.global.pool_remote.as_deref(),
    )
    .and_then(|paths| paths.validate().map(|()| paths))
    {
        Ok(paths) => paths,
        Err(error) => {
            let _ = output::write_error(&error, None, pretty);
            return error.exit_code();
        }
    };

    // On a terminal a person is reading: the commands with a summary print
    // it instead of the envelope, unless `--json` (or `--pretty`) asks for it.
    let summary_kind = human::kind(&cli.command)
        .filter(|_| !cli.global.json && !pretty && std::io::stdout().is_terminal());
    let dry_run = cli.global.dry_run;
    let result = commands::dispatch(cli, &paths);
    let text = match (&result, summary_kind) {
        (Ok(envelope), Some(kind)) => human::summary(kind, envelope, dry_run),
        _ => None,
    };
    if let Some(text) = text {
        let mut stdout = std::io::stdout().lock();
        // A closed pipe or terminal is not a command failure; the work is done.
        let _ = stdout
            .write_all(text.as_bytes())
            .and_then(|()| stdout.flush());
        return output::exit_code();
    }
    match result {
        Ok(envelope) => match output::write_success(&envelope, pretty) {
            Ok(()) => output::exit_code(),
            // The consumer stopped reading (`skillherd list | head`); the work is
            // done and a broken pipe is not a command failure.
            Err(err) if err.kind() == std::io::ErrorKind::BrokenPipe => output::exit_code(),
            Err(err) => {
                let error = Error::io("writing the envelope to stdout", &err);
                let _ = output::write_error(&error, Some(&paths), pretty);
                error.exit_code()
            }
        },
        Err(error) => {
            let _ = output::write_error(&error, Some(&paths), pretty);
            error.exit_code()
        }
    }
}
