//! End-to-end tests for the `hydra` command.

use std::process::{Command, Output};

fn hydra(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_hydra")).args(args).output().expect("runs")
}

fn stdout(out: &Output) -> String {
    String::from_utf8_lossy(&out.stdout).to_string()
}

fn stderr(out: &Output) -> String {
    String::from_utf8_lossy(&out.stderr).to_string()
}

#[test]
fn run_executes_a_program_and_can_show_what_it_left() {
    let out = hydra(&["run", "examples/trails.hy", "--dump-scope"]);
    assert!(out.status.success(), "{}", stderr(&out));
    let text = stdout(&out);
    // Value semantics: the callee could not touch the caller's value, but the
    // reference could (§5.1).
    assert!(text.contains("by_value = 0"), "{text}");
    assert!(text.contains("by_reference = 1"), "{text}");
    // The copy split from its source on the first write (§5.1).
    assert!(text.contains("same_buffer = .false"), "{text}");
    assert!(text.contains("untouched = 0"), "{text}");
    // Every trail of the `parallel` block ran (§9.3).
    assert!(text.contains("regions = [eu, us-east, ap]"), "{text}");
    // `alive()` is true outside any trail (§9.5).
    assert!(text.contains("outside = .true"), "{text}");
}

#[test]
fn run_exits_non_zero_on_a_crash_and_says_where() {
    let out = hydra(&["run", "tests/fixtures/crashes.hy"]);
    assert_eq!(out.status.code(), Some(1));
    let text = stderr(&out);
    assert!(text.contains("crash:"), "{text}");
    assert!(text.contains("no key .missing"), "{text}");
    // The diagnostic points at the line inside the function, and at the call.
    assert!(text.contains("crashes.hy:2"), "{text}");
    assert!(text.contains("called from tests/fixtures/crashes.hy:6"), "{text}");
}

#[test]
fn check_reports_findings_and_exits_non_zero_on_errors() {
    let out = hydra(&["check", "tests/fixtures/broken.hy"]);
    assert_eq!(out.status.code(), Some(1));
    let text = stderr(&out);
    assert!(text.contains("trail-local"), "{text}");
    assert!(text.contains("error:"), "{text}");
}

#[test]
fn check_is_quiet_on_a_clean_file() {
    let out = hydra(&["check", "examples/trails.hy"]);
    assert!(out.status.success(), "{}", stderr(&out));
    assert!(stdout(&out).contains("no findings"));
}

#[test]
fn check_takes_the_hosts_names() {
    // Without a standard library, a call to a placeholder is an undeclared
    // name (QUESTIONS.md §1).
    let out = hydra(&["check", "tests/fixtures/uses_extern.hy"]);
    assert_eq!(out.status.code(), Some(1));
    assert!(stderr(&out).contains("undeclared-name"), "{}", stderr(&out));

    let out = hydra(&["check", "tests/fixtures/uses_extern.hy", "--extern", "print"]);
    assert!(out.status.success(), "{}", stderr(&out));
}

#[test]
fn an_unresolvable_module_downgrades_name_resolution_to_a_warning() {
    // The spec's reference program imports `fmt`, `http` and `json`, none of
    // which exist. Guessing at the names they would export would break §11's
    // "only what is guaranteed" rule, so check says so and stops guessing.
    let out = hydra(&["check", "examples/deploy.hy"]);
    assert!(out.status.success(), "{}", stderr(&out));
    let text = stdout(&out);
    assert!(text.contains("unresolved-module"), "{text}");
    assert!(text.contains("cannot find module `fmt`"), "{text}");
}

#[test]
fn fmt_prints_the_canonical_form_and_check_verifies_it() {
    let out = hydra(&["fmt", "examples/deploy.hy", "--check"]);
    assert!(out.status.success(), "the examples are kept canonical: {}", stderr(&out));

    let out = hydra(&["fmt", "tests/fixtures/unformatted.hy"]);
    assert!(out.status.success());
    assert_eq!(stdout(&out), "if a\n\tx := 1 + 2\n\td := { .k : &x }\nend\n");

    let out = hydra(&["fmt", "tests/fixtures/unformatted.hy", "--check"]);
    assert_eq!(out.status.code(), Some(1));
}

#[test]
fn a_syntax_error_is_reported_by_every_tool() {
    for command in ["run", "check", "fmt"] {
        let out = hydra(&[command, "tests/fixtures/syntax_error.hy"]);
        assert!(!out.status.success(), "{command} should fail");
        assert!(stderr(&out).contains("same number of `||`"), "{command}: {}", stderr(&out));
    }
}

#[test]
fn usage_is_available() {
    let out = hydra(&["help"]);
    assert!(out.status.success());
    assert!(stdout(&out).contains("hydra run"));
    let out = hydra(&["nonsense"]);
    assert_eq!(out.status.code(), Some(2));
}
