//! End-to-end tests for the `hydra` command.

use std::process::{Command, Output, Stdio};

fn hydra(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_hydra")).args(args).output().expect("runs")
}

fn hydra_stdin(args: &[&str], input: &str) -> Output {
    use std::io::Write;

    let mut child = Command::new(env!("CARGO_BIN_EXE_hydra"))
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("runs");
    child.stdin.take().expect("piped").write_all(input.as_bytes()).expect("writes");
    child.wait_with_output().expect("finishes")
}

fn stdout(out: &Output) -> String {
    String::from_utf8_lossy(&out.stdout).to_string()
}

fn stderr(out: &Output) -> String {
    String::from_utf8_lossy(&out.stderr).to_string()
}

#[test]
fn run_executes_a_program_and_it_can_print() {
    let out = hydra(&["run", "examples/trails.hy"]);
    assert!(out.status.success(), "{}", stderr(&out));
    let text = stdout(&out);
    // Value semantics: the copy split from its source on the first write, and
    // only the reference reached the caller's value (§5.1).
    assert!(text.contains("original is 0, the copy is 1"), "{text}");
    assert!(text.contains("through a reference: 1"), "{text}");
    // Every trail of the `parallel` block ran (§9.3).
    assert!(text.contains("regions: [eu, us-east, ap]"), "{text}");
    assert!(text.contains("3 trails reported"), "{text}");
    // `alive()` is true outside any trail (§9.5).
    assert!(text.contains("outside a trail, alive() is .true"), "{text}");
}

#[test]
fn print_ends_with_a_newline_unless_told_otherwise() {
    let out = hydra(&["run", "tests/fixtures/printing.hy"]);
    assert!(out.status.success(), "{}", stderr(&out));
    assert_eq!(stdout(&out), "one\ntwo three\n");
}

#[test]
fn dump_scope_still_shows_the_bindings() {
    let out = hydra(&["run", "tests/fixtures/printing.hy", "--dump-scope"]);
    assert!(stdout(&out).contains("greeting = one"), "{}", stdout(&out));
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
    let out = hydra(&["check", "tests/fixtures/printing.hy"]);
    assert!(out.status.success(), "{}", stderr(&out));
    assert!(stdout(&out).contains("no findings"));
}

#[test]
fn check_warns_about_a_reference_crossing_into_a_trail() {
    // The example does it deliberately: it is the only way two trails can fill
    // one list, and §11 wants a second look at every one (§9.2).
    let out = hydra(&["check", "examples/trails.hy"]);
    assert!(out.status.success(), "warnings are not errors: {}", stderr(&out));
    assert!(stdout(&out).contains("ref-into-trail"), "{}", stdout(&out));
}

#[test]
fn check_takes_the_hosts_names() {
    // Without a standard library, a call to a placeholder is an undeclared
    // name (QUESTIONS.md §1).
    let out = hydra(&["check", "tests/fixtures/uses_extern.hy"]);
    assert_eq!(out.status.code(), Some(1));
    assert!(stderr(&out).contains("undeclared-name"), "{}", stderr(&out));

    let out = hydra(&["check", "tests/fixtures/uses_extern.hy", "--extern", "read_file"]);
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
fn fmt_formats_standard_input_onto_standard_output() {
    // What an editor's format-on-save hands the formatter: the buffer, not a
    // path. Nothing on disk is touched.
    let source = std::fs::read_to_string("tests/fixtures/unformatted.hy").expect("reads");
    let out = hydra_stdin(&["fmt", "-"], &source);
    assert!(out.status.success(), "{}", stderr(&out));
    assert_eq!(stdout(&out), "if a\n\tx := 1 + 2\n\td := { .k : &x }\nend\n");

    let canonical = stdout(&out);

    let out = hydra_stdin(&["fmt", "-", "--check"], &source);
    assert_eq!(out.status.code(), Some(1));

    let out = hydra_stdin(&["fmt", "-", "--check"], &canonical);
    assert!(out.status.success(), "formatting is idempotent: {}", stderr(&out));

    // There is no file to write back to.
    let out = hydra_stdin(&["fmt", "-", "--write"], &source);
    assert_eq!(out.status.code(), Some(2));
    assert!(stderr(&out).contains("--write needs a file"), "{}", stderr(&out));
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
