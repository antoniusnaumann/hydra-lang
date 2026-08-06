//! Calling through a dot (spec §5.2): a field call where the receiver has one,
//! and otherwise the receiver as the first argument.

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

#[test]
fn a_receiver_with_no_field_becomes_the_first_argument() {
    let src = "
text := \"hello\".len()
list := [1, 2, 3].len()
d := { .region : \"eu\" }
asked := d.has(.region)
missing := d.get(.retries, 3)
";
    assert_eq!(eval(src, "text"), "5");
    assert_eq!(eval(src, "list"), "3");
    assert_eq!(eval(src, "asked"), ".true");
    assert_eq!(eval(src, "missing"), "3");
}

#[test]
fn a_field_holding_a_closure_wins_and_is_not_passed_the_receiver() {
    let src = "
fn greet(who)
\treturn \"free \\(who)\"
end
obj := { .greet : fn(who) \"field \\(who)\" }
answered := obj.greet(\"eu\")
";
    assert_eq!(eval(src, "answered"), "field eu");
}

#[test]
fn a_field_that_is_not_callable_falls_through_to_the_function() {
    // A number named `count` is not what `x.count()` meant (§5.2).
    let src = "
fn count(thing, extra)
\treturn len(thing) + extra
end
obj := { .count : 3, .other : 1 }
answered := obj.count(10)
";
    assert_eq!(eval(src, "answered"), "12");
}

#[test]
fn calls_chain_through_the_dot() {
    let src = "
fn twice(n)
\treturn n * 2
end
answered := \"abcd\".len().twice()
";
    assert_eq!(eval(src, "answered"), "8");
}

#[test]
fn the_receiver_is_passed_exactly_as_written() {
    // §5.1 stands: the caller marks a reference, never the callee. So a
    // parameter that needs one needs it here too.
    let src = "
rows := []
n := (&rows).push(7)
";
    assert_eq!(eval(src, "rows"), "[7]");
    assert_eq!(eval(src, "n"), "1");

    let crash = crash_of("rows := []\nrows.push(7)\n");
    assert!(crash.contains("takes `list` by reference"), "{crash}");
}

#[test]
fn a_bare_dot_is_still_a_key_read() {
    // Nothing is bound or partially applied by writing the dot without a call.
    let crash = crash_of("d := { .a : 1 }\nx := d.len\n");
    assert!(crash.contains("no key .len"), "{crash}");
}

#[test]
fn neither_a_field_nor_a_function_says_so() {
    let crash = crash_of("d := { .a : 1 }\nx := d.nope()\n");
    assert!(crash.contains("no field `.nope` and no function `nope`"), "{crash}");
}

#[test]
fn a_quoted_key_is_not_a_function_name() {
    // `d."x-y"(…)` is an ordinary field call and crashes when it is missing.
    let crash = crash_of("d := { .a : 1 }\nx := d.\"not a name\"()\n");
    assert!(crash.contains("no key"), "{crash}");
}

#[test]
fn resolution_by_shape_still_applies_to_the_free_call() {
    // The receiver takes the first slot, and the rest is §3 unchanged.
    let src = "
fn describe(thing)
\treturn \"one\"
end
describe := fn(thing, detail) \"two\"
short := \"x\".describe()
long := \"x\".describe(\"detail\")
";
    assert_eq!(eval(src, "short"), "one");
    assert_eq!(eval(src, "long"), "two");
}
