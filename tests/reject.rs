//! `reject()` (spec §3): a function that looks at the values and hands the call
//! back to resolution, which tries the next candidate that accepts them.

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

// --- falling back (§3) --------------------------------------------------------

#[test]
fn a_rejected_call_falls_back_to_the_earlier_function() {
    // Two of the same shape: the later one wins, and rejecting hands the call
    // to the one behind it.
    let src = "
fn parse(text)
\treturn \"first\"
end
fn parse(text)
\tif len(text) > 3
\t\treject(\"too long for the second one\")
\tend
\treturn \"second\"
end
short := parse(\"ab\")
long := parse(\"abcdef\")
";
    assert_eq!(eval(src, "short"), "second");
    assert_eq!(eval(src, "long"), "first");
}

#[test]
fn a_reject_can_fall_through_more_than_one() {
    let src = "
fn pick(n)
\treturn \"first\"
end
fn pick(n)
\treject()
end
fn pick(n)
\treject()
end
answered := pick(1)
";
    assert_eq!(eval(src, "answered"), "first");
}

#[test]
fn the_later_import_is_tried_first_and_can_hand_back() {
    // The same rule across files: most recent `use` first (§7).
    let src = "
use decode_base as *
use decode_better as *
easy := decode(\"easy\")
hard := decode(\"hard\")
";
    let result = run_source(src, "tests/fixtures/main.hy", opts()).expect("compiles");
    assert!(result.crash.is_none(), "{:?}", result.crash);
    let read =
        |name: &str| to_text(&result.root_scope.lookup(name).expect("binding").read().unwrap().clone());
    assert_eq!(read("easy"), "better: easy");
    assert_eq!(read("hard"), "base: hard");
}

#[test]
fn a_reject_reaches_a_call_written_through_a_dot() {
    let src = "
fn describe(x)
\treturn \"fallback\"
end
fn describe(x)
\treject()
end
answered := \"abc\".describe()
";
    assert_eq!(eval(src, "answered"), "fallback");
}

// --- when nobody takes it -----------------------------------------------------

#[test]
fn every_refusal_is_printed_with_the_crash() {
    let src = "
fn pick(n)
\treject(\"the first wants something else\")
end
fn pick(n)
\treject()
end
x := pick(1)
";
    let crash = crash_of(src);
    assert!(crash.contains("no `pick` took 1 argument(s)"), "{crash}");
    // The one that ran last is nearest the call, so it is listed first.
    assert!(crash.contains("pick(n) rejected it\n"), "{crash}");
    assert!(crash.contains("pick(n) rejected it: the first wants something else"), "{crash}");
}

#[test]
fn a_lone_function_that_rejects_says_so() {
    let crash = crash_of("fn only(n)\n\treject(\"nothing behind me\")\nend\nx := only(1)\n");
    assert!(crash.contains("no `only` took 1 argument(s)"), "{crash}");
    assert!(crash.contains("only(n) rejected it: nothing behind me"), "{crash}");
}

#[test]
fn reject_belongs_in_a_function() {
    let crash = crash_of("reject(\"nope\")\n");
    assert!(crash.contains("belongs in a function"), "{crash}");
    // A trail body is not a function body either.
    let crash = crash_of("parallel\n\treject(\"x\") || y := 1\nend\n");
    assert!(crash.contains("belongs in a function"), "{crash}");
}

// --- what `check` says (§3, §11) ----------------------------------------------

#[test]
fn a_shadow_that_never_rejects_is_an_error() {
    // The earlier one could never run, which is worth an error rather than a
    // warning: overloading by shape is what shares a name, and `reject()` is
    // what shares a shape.
    assert_eq!(
        codes("fn f(a)\n\treturn 1\nend\nfn f(a)\n\treturn 2\nend\n"),
        vec!["unreachable-overload"]
    );
    // With a `reject()`, both are reachable.
    assert_eq!(
        codes("fn f(a)\n\treturn 1\nend\nfn f(a)\n\treject()\nend\n"),
        Vec::<&str>::new()
    );
    // A shape of its own is the other way out.
    assert_eq!(
        codes("fn f(a)\n\treturn 1\nend\nfn f(a, b)\n\treturn 2\nend\n"),
        Vec::<&str>::new()
    );
}

#[test]
fn a_wider_shape_shadows_a_narrower_one() {
    // Every call `f(a)` takes, `f(a, b = 1)` takes too.
    assert_eq!(
        codes("fn f(a)\n\treturn 1\nend\nfn f(a, b = 1)\n\treturn 2\nend\n"),
        vec!["unreachable-overload"]
    );
    // The other way round shadows nothing: `f(a, b)` needs two.
    assert_eq!(
        codes("fn f(a, b = 1)\n\treturn 1\nend\nfn f(a, b)\n\treturn 2\nend\n"),
        Vec::<&str>::new()
    );
}

#[test]
fn a_variadic_never_shadows_a_concrete_arity() {
    // A `*` is tried only after every concrete one (channels §6.1), so it
    // cannot make one unreachable however wide it is.
    assert_eq!(
        codes("fn f(a)\n\treturn 1\nend\nf := fn(rest*) 2\n"),
        Vec::<&str>::new()
    );
}

#[test]
fn a_closure_declaration_shadows_the_same_way() {
    assert_eq!(
        codes("fn f(a)\n\treturn 1\nend\nf := fn(a) 2\n"),
        vec!["unreachable-overload"]
    );
}

#[test]
fn reject_outside_a_function_is_an_error() {
    assert_eq!(codes("reject()\n"), vec!["reject-outside-function"]);
    assert_eq!(
        codes("parallel\n\treject() || y := 1\nend\n"),
        vec!["reject-outside-function"]
    );
    assert_eq!(codes("fn f(a)\n\treject()\nend\n"), Vec::<&str>::new());
}
