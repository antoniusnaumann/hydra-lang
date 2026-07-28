//! The `hydra` command: the three tools the spec asks for.

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use hydra::check::{check_file, CheckOptions};
use hydra::errors::Severity;
use hydra::format::format_source;
use hydra::value::to_text;
use hydra::vm::{run_file, Options};

const USAGE: &str = "\
hydra — the Hydra language (see spec/hydra_spec.md)

usage:
  hydra run FILE.hy [options]     run a program
  hydra check FILE.hy [options]   report what is guaranteed to crash
  hydra fmt FILE.hy [options]     print the canonical form
  hydra tokens FILE.hy            print token classes for editor tooling (§13)
  hydra grammar [--theme]         print a TextMate grammar, or its colours

run options:
  --strict            a crash in a dead trail is fatal, so tests fail on bugs
                      production would swallow (§9.5)
  --quiet             do not report dead-trail crashes on stderr
  --step-budget N     statement boundaries a trail runs before the scheduler
                      looks at the others (default 1)
  --dump-scope        print the program's toplevel bindings when it finishes

check options:
  --extern a,b,c      names the host supplies, so they are not reported as
                      undeclared. Needed until there is a standard library:
                      see QUESTIONS.md §1

fmt options:
  --write             rewrite the file in place
  --check             exit non-zero if the file is not already formatted

environment:
  HYDRA_PATH          `:`-separated directories searched after the importing
                      file's own directory (§7)
";

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.is_empty() {
        eprint!("{USAGE}");
        return ExitCode::from(2);
    }
    match args[0].as_str() {
        "help" | "-h" | "--help" => {
            print!("{USAGE}");
            ExitCode::SUCCESS
        }
        "version" | "--version" => {
            println!("hydra {}", env!("CARGO_PKG_VERSION"));
            ExitCode::SUCCESS
        }
        "run" => command(&args[1..], run),
        "check" => command(&args[1..], check),
        "fmt" => command(&args[1..], fmt),
        "tokens" => command(&args[1..], tokens),
        "grammar" => {
            if flag(&args[1..], "--theme") {
                print!("{}", hydra::editor::theme_json());
            } else {
                print!("{}", hydra::editor::tmlanguage_json());
            }
            ExitCode::SUCCESS
        }
        other => {
            eprintln!("hydra: unknown command `{other}`");
            eprint!("{USAGE}");
            ExitCode::from(2)
        }
    }
}

fn command(args: &[String], f: fn(&Path, &[String]) -> ExitCode) -> ExitCode {
    let Some(file) = args.iter().find(|a| !a.starts_with("--")) else {
        eprintln!("hydra: expected a file");
        return ExitCode::from(2);
    };
    let path = PathBuf::from(file);
    if !path.is_file() {
        eprintln!("hydra: no such file: {}", path.display());
        return ExitCode::from(2);
    }
    f(&path, args)
}

fn flag(args: &[String], name: &str) -> bool {
    args.iter().any(|a| a == name)
}

fn value(args: &[String], name: &str) -> Option<String> {
    let prefix = format!("{name}=");
    if let Some(found) = args.iter().find(|a| a.starts_with(&prefix)) {
        return Some(found[prefix.len()..].to_string());
    }
    let index = args.iter().position(|a| a == name)?;
    args.get(index + 1).cloned()
}

fn run(path: &Path, args: &[String]) -> ExitCode {
    let mut options = Options {
        strict: flag(args, "--strict"),
        report_dead_crashes: !flag(args, "--quiet"),
        ..Options::default()
    };
    if let Some(budget) = value(args, "--step-budget").and_then(|v| v.parse::<u32>().ok()) {
        options.step_budget = budget.max(1);
    }

    match run_file(path, options) {
        Err(error) => {
            eprintln!("{error}");
            ExitCode::from(2)
        }
        Ok(result) => {
            if flag(args, "--dump-scope") {
                // A stopgap: without a standard library a program has no way to
                // say what it computed (QUESTIONS.md §1).
                let mut names = result.root_scope.names();
                names.sort();
                for name in names {
                    if let Some(cell) = result.root_scope.get_local(&name) {
                        println!("{name} = {}", to_text(&cell.borrow()));
                    }
                }
            }
            match result.crash {
                Some(crash) => {
                    eprintln!("{crash}");
                    ExitCode::from(1)
                }
                None => ExitCode::SUCCESS,
            }
        }
    }
}

fn check(path: &Path, args: &[String]) -> ExitCode {
    let mut options = CheckOptions::from_env();
    if let Some(list) = value(args, "--extern") {
        options.externs = list.split(',').filter(|s| !s.is_empty()).map(str::to_string).collect();
    }
    let report = check_file(path, &options);
    for diagnostic in report.sorted() {
        match diagnostic.severity {
            Severity::Error => eprintln!("{diagnostic}"),
            Severity::Warning => println!("{diagnostic}"),
        }
    }
    let errors = report.errors().count();
    let warnings = report.warnings().count();
    if errors == 0 && warnings == 0 {
        println!("{}: no findings", path.display());
    }
    if errors > 0 {
        ExitCode::from(1)
    } else {
        ExitCode::SUCCESS
    }
}

/// §13's token classes for one file, one per line: `line:col len class`.
fn tokens(path: &Path, _args: &[String]) -> ExitCode {
    let file = path.display().to_string();
    let src = match std::fs::read_to_string(path) {
        Ok(src) => src,
        Err(e) => {
            eprintln!("hydra: cannot read {file}: {e}");
            return ExitCode::from(2);
        }
    };
    match hydra::editor::semantic_tokens(&src, &file) {
        Ok(tokens) => {
            for token in tokens {
                println!("{}:{} {} {}", token.line, token.col, token.len, token.class);
            }
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("{error}");
            ExitCode::from(2)
        }
    }
}

fn fmt(path: &Path, args: &[String]) -> ExitCode {
    let file = path.display().to_string();
    let src = match std::fs::read_to_string(path) {
        Ok(src) => src,
        Err(e) => {
            eprintln!("hydra: cannot read {file}: {e}");
            return ExitCode::from(2);
        }
    };
    let formatted = match format_source(&src, &file) {
        Ok(formatted) => formatted,
        Err(error) => {
            eprintln!("{error}");
            return ExitCode::from(2);
        }
    };

    if flag(args, "--check") {
        if formatted == src {
            return ExitCode::SUCCESS;
        }
        eprintln!("{file}: not formatted");
        return ExitCode::from(1);
    }

    if flag(args, "--write") {
        if formatted == src {
            return ExitCode::SUCCESS;
        }
        if let Err(e) = std::fs::write(path, formatted) {
            eprintln!("hydra: cannot write {file}: {e}");
            return ExitCode::from(2);
        }
        println!("{file}: formatted");
        return ExitCode::SUCCESS;
    }

    print!("{formatted}");
    ExitCode::SUCCESS
}
