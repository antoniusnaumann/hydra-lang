//! Values a statement produces and nothing consumes (spec §8.1): dropped in a
//! function unless they start with `.reject`, which returns from it; printed at
//! the top level of a file.

use std::process::Command;

use hydra::check::{check_program, CheckOptions};
use hydra::parser::parse;
use hydra::value::to_text;
use hydra::vm::{run_source, Options, RunResult};

fn opts() -> Options {
    Options { search_path: Vec::new(), threads: 1, step_budget: 1, ..Options::default() }
}

fn run(src: &str) -> RunResult {
    run_source(src, "t.hy", opts()).expect("compiles")
}

fn eval(src: &str, name: &str) -> String {
    let result = run(src);
    if let Some(crash) = &result.crash {
        panic!("unexpected crash: {crash}");
    }
    to_text(&result.root_scope.lookup(name).unwrap_or_else(|| panic!("no `{name}`")).read().unwrap().clone())
}

fn crash_of(src: &str) -> String {
    run(src).crash.map(|c| c.message).unwrap_or_else(|| "<no crash>".into())
}

fn codes(src: &str) -> Vec<&'static str> {
    let program = parse(src, "t.hy").expect("parses");
    check_program(&program, &CheckOptions::default()).codes()
}

const HELPERS: &str = "
fn reject_if(cond)
\tif cond
\t\treturn .reject, \"invalid value\"
\tend
end
fn guard(x)
\treject_if(x < 0)
end
";

// --- in a function ------------------------------------------------------------

#[test]
fn a_rejection_passes_through_helpers_without_being_spelled_out() {
    let src = format!(
        "{HELPERS}
fn foo(x)
\tguard(x)
\treturn x
end
ok := foo(3)
bad, why := foo(-3)
"
    );
    assert_eq!(eval(&src, "ok"), "3");
    // The whole result, unchanged.
    assert_eq!(eval(&src, "bad"), ".reject");
    assert_eq!(eval(&src, "why"), "invalid value");
}

#[test]
fn a_propagated_rejection_falls_back_like_a_returned_one() {
    // At the overload boundary the two are the same thing (§3).
    let src = format!(
        "{HELPERS}
fn parse(x)
\treturn \"careful\"
end
fn parse(x)
\tguard(x)
\treturn \"quick\"
end
fn spelled(x)
\treturn \"careful\"
end
fn spelled(x)
\tif x < 0
\t\treturn .reject
\tend
\treturn \"quick\"
end
a := parse(1)
b := parse(-1)
c := spelled(1)
d := spelled(-1)
"
    );
    assert_eq!(eval(&src, "a"), "quick");
    assert_eq!(eval(&src, "b"), "careful");
    assert_eq!(eval(&src, "c"), "quick");
    assert_eq!(eval(&src, "d"), "careful");
}

#[test]
fn a_consumed_rejection_is_an_ordinary_value() {
    let src = format!(
        "{HELPERS}
fn keeps(x)
\tresult := reject_if(x < 0)
\t_ = reject_if(x < 0)
\treturn result
end
fn ignores(x)
\t_ = guard(x)
\treturn \"went on\"
end
kept := keeps(-1)
went := ignores(-1)
"
    );
    assert_eq!(eval(&src, "kept"), ".reject");
    assert_eq!(eval(&src, "went"), "went on");
}

#[test]
fn a_statement_in_a_branch_or_a_loop_is_still_a_statement() {
    let src = format!(
        "{HELPERS}
fn scan(items)
\tfor item in items
\t\tif item == 2
\t\t\treject_if(.true)
\t\tend
\tend
\treturn \"clean\"
end
clean := scan([1, 3])
dirty := scan([1, 2, 3])
"
    );
    assert_eq!(eval(&src, "clean"), "clean");
    assert_eq!(eval(&src, "dirty"), ".reject");
}

#[test]
fn a_bare_reject_symbol_is_a_rejection_too() {
    let src = "fn f(x)\n\tif x\n\t\t.reject\n\tend\n\treturn 1\nend\na := f(.false)\nb := f(.true)\n";
    assert_eq!(eval(src, "a"), "1");
    assert_eq!(eval(src, "b"), ".reject");
}

#[test]
fn an_ordinary_value_in_a_function_is_dropped() {
    let src = "fn f()\n\t42\n\t\"text\"\nend\nx := f()\n";
    assert_eq!(eval(src, "x"), ".null");
}

// --- where there is no call to hand it back to ---------------------------------

#[test]
fn a_rejection_that_reaches_the_top_level_is_a_crash() {
    let crash = crash_of(&format!("{HELPERS}guard(-1)\n"));
    assert!(crash.contains("unhandled rejection: invalid value"), "{crash}");
    // Which call ran out of candidates, and what it said.
    assert!(crash.contains("guard(x) rejected it: invalid value"), "{crash}");
}

#[test]
fn what_a_consumed_rejection_said_is_not_blamed_on_the_next() {
    let crash = crash_of(&format!("{HELPERS}x := guard(-1)\n.reject\n"));
    assert!(crash.contains("unhandled rejection"), "{crash}");
    assert!(!crash.contains("guard"), "{crash}");
}

#[test]
fn a_trail_in_a_function_cannot_hand_the_call_back() {
    let src = format!(
        "{HELPERS}
fn f(x)
\tparallel
\t\tguard(x) || y := 1
\tend
end
f(-1)
"
    );
    let crash = crash_of(&src);
    assert!(crash.contains("cannot return from its function"), "{crash}");
}

// --- what `check` says ----------------------------------------------------------

#[test]
fn an_underscore_consumes_without_binding() {
    assert_eq!(codes("fn pair()\n\treturn 1, 2\nend\n_ = pair()\na, _ := pair()\nprint(a)\n_ := 3\n"), Vec::<&str>::new());
}

// --- at the top level -------------------------------------------------------------

#[test]
fn the_top_level_prints_what_nothing_consumed() {
    let out = Command::new(env!("CARGO_BIN_EXE_hydra"))
        .args(["run", "tests/fixtures/unconsumed.hy"])
        .output()
        .expect("runs");
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        "1, 2\ntext\n7\n1\n10\n20\nbranch\n"
    );
}
