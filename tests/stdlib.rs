//! Standard library tests (spec/hydra_stdlib.md), and the two language
//! features its signatures needed: `&name` and `name := default` parameters.

use hydra::value::to_text;
use hydra::vm::{run_source, Options, RunResult};

/// One worker thread, so the interleaving is round-robin and reproducible:
/// these tests assert on the schedule itself. `tests/parallelism.rs` is where
/// the pool runs wide.
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

fn expr(src: &str) -> String {
    eval(&format!("x := {src}\n"), "x")
}

fn crash_of(src: &str) -> String {
    run(src).crash.map(|c| c.message).unwrap_or_else(|| "<no crash>".into())
}

// --- parameters -------------------------------------------------------------

#[test]
fn a_by_reference_parameter_must_be_passed_with_an_ampersand() {
    let src = "
fn bump(&box)
\tbox.n = box.n + 1
end
d := { :n : 0 }
bump(&d)
after := d.n
";
    assert_eq!(eval(src, "after"), "1");

    // Without the `&` the callee would append to a copy — a silent no-op, so
    // it crashes instead (§5.1).
    let src = "
fn bump(&box)
\tbox.n = box.n + 1
end
d := { :n : 0 }
bump(d)
";
    assert!(crash_of(src).contains("by reference"), "{}", crash_of(src));
}

#[test]
fn a_default_is_evaluated_in_the_functions_own_scope() {
    assert_eq!(eval("fn f(a, b = 10)\n\treturn a + b\nend\nx := f(1)\n", "x"), "11");
    assert_eq!(eval("fn f(a, b = 10)\n\treturn a + b\nend\nx := f(1, 2)\n", "x"), "3");
    // A later default may refer to an earlier parameter.
    assert_eq!(eval("fn f(a, b = a * 2)\n\treturn b\nend\nx := f(4)\n", "x"), "8");
    // Defaults are evaluated per call, not once.
    let src = "
n := 0
fn tick(v = 1)
\tn = n + v
end
tick()
tick()
tick(5)
";
    assert_eq!(eval(src, "n"), "7");
    // Closures take defaults too.
    assert_eq!(eval("f := fn(a = 3) a\nx := f()\n", "x"), "3");
}

#[test]
fn arguments_can_be_named() {
    assert_eq!(eval("fn f(a, b)\n\treturn a - b\nend\nx := f(b = 1, a = 5)\n", "x"), "4");
    assert_eq!(eval("fn f(a, b)\n\treturn a - b\nend\nx := f(5, b = 1)\n", "x"), "4");
    // Naming a later parameter leaves an earlier default in place, which a
    // positional call cannot do.
    let src = "
fn f(a, b = 10, c = 100)
\treturn a + b + c
end
x := f(1, c = 2)
";
    assert_eq!(eval(src, "x"), "13");
    // The builtin's parameter is `terminator`, because `end` is a keyword.
    assert_eq!(eval("x := print(\"hi\", terminator = \"\")\n", "x"), ":null");
    assert_eq!(eval("rows := []\nn := push(value = 1, list = &rows)\n", "n"), "1");
}

// --- resolution (§3) --------------------------------------------------------

#[test]
fn a_call_a_candidate_rejects_goes_to_the_next() {
    // Shadowing a function with one of a different shape does not hide the
    // original: the call picks the first candidate that accepts it.
    let src = "
fn f(a)
\treturn \"one\"
end
f := fn(a, b) \"two\"
one := f(1)
two := f(1, 2)
";
    assert_eq!(eval(src, "one"), "one");
    assert_eq!(eval(src, "two"), "two");
}

#[test]
fn argument_names_take_part_in_resolution() {
    let src = "
fn f(width)
\treturn \"by width\"
end
f := fn(height) \"by height\"
w := f(width = 1)
h := f(height = 1)
";
    assert_eq!(eval(src, "w"), "by width");
    assert_eq!(eval(src, "h"), "by height");
}

#[test]
fn a_builtin_is_the_last_candidate() {
    // A local that rejects the call falls through to the builtin.
    let src = "
len := fn(a, b) \"local\"
theirs := len(1, 2)
builtin := len(\"abc\")
";
    assert_eq!(eval(src, "theirs"), "local");
    assert_eq!(eval(src, "builtin"), "3");
}

#[test]
fn only_when_no_candidate_accepts_is_it_a_crash() {
    let crash = crash_of("fn f(a)\nend\nf := fn(a, b) a\nx := f(1, 2, 3)\n");
    assert!(crash.contains("no `f` accepts"), "{crash}");
    // The message lists what was tried.
    assert!(crash.contains("f(a, b)") && crash.contains("f(a)"), "{crash}");
}

#[test]
fn a_missing_reference_is_reported_not_resolved_around() {
    // `&` is not part of accepting a call, so a missing one crashes with its
    // own message instead of quietly selecting some other function.
    let src = "
fn push(target, image)
\treturn \"module-ish\"
end
rows := []
x := ::push(rows, 1)
";
    assert!(crash_of(src).contains("by reference"), "{}", crash_of(src));
}

#[test]
fn resolution_reaches_past_an_import_to_an_earlier_one() {
    // `use` keeps every candidate: the most recent wins a read, and a call it
    // rejects falls through to the earlier module.
    let src = "
use http as *
use shadows as *
theirs := push(\"host\", \"img\")
fetched := fetch(\"url\")
";
    let result = run_source(src, "tests/fixtures/main.hy", Options { threads: 1, step_budget: 1, ..Options::default() }).expect("compiles");
    assert!(result.crash.is_none(), "{:?}", result.crash);
    let read =
        |name: &str| to_text(&result.root_scope.lookup(name).expect("binding").read().unwrap().clone());
    assert_eq!(read("theirs"), "pushed img to host");
    assert_eq!(read("fetched"), "fetched url");
}

#[test]
fn a_default_widens_what_a_function_accepts() {
    assert_eq!(eval("fn f(a, b = 1)\n\treturn b\nend\nx := f(1)\n", "x"), "1");
    assert_eq!(eval("fn f(a, b = 1)\n\treturn b\nend\nx := f(1, 2)\n", "x"), "2");
    // Outside that range nothing accepts the call.
    assert!(crash_of("fn f(a, b = 1)\nend\nf()\n").contains("no `f` accepts"));
    assert!(crash_of("fn f(a, b = 1)\nend\nf(1, 2, 3)\n").contains("no `f` accepts"));
}

// --- the builtins -----------------------------------------------------------

#[test]
fn builtins_are_shadowable_global_names() {
    // Looked up after the scope chain, so they are not reserved words.
    assert_eq!(eval("len := fn(a) 99\nx := len([1, 2])\n", "x"), "99");
    // And they are ordinary values.
    assert_eq!(eval("f := len\nx := f([1, 2, 3])\n", "x"), "3");
}

#[test]
fn len_counts_elements_keys_and_characters() {
    assert_eq!(expr("len([1, 2, 3])"), "3");
    assert_eq!(expr("len([])"), "0");
    assert_eq!(expr("len({ :a : 1, :b : 2 })"), "2");
    assert_eq!(expr("len(\"hello\")"), "5");
    // Characters, not bytes: source is UTF-8 (§1).
    assert_eq!(expr("len(\"héllo\")"), "5");
    assert!(crash_of("x := len(3)\n").contains("counts a list"));
    assert!(crash_of("x := len(:sym)\n").contains("counts a list"));
}

#[test]
fn has_answers_without_crashing() {
    assert_eq!(expr("has({ :a : 1 }, :a)"), ":true");
    assert_eq!(expr("has({ :a : 1 }, :b)"), ":false");
    assert_eq!(expr("has([1, 2], 1)"), ":true");
    assert_eq!(expr("has([1, 2], 2)"), ":false");
    // The negative-index rule applies first.
    assert_eq!(expr("has([1, 2], 0 - 1)"), ":true");
    assert_eq!(expr("has([], 0 - 1)"), ":false");
    // Asking about the wrong kind of thing is a bug, not a `:false`.
    assert!(crash_of("x := has([1], :a)\n").contains("has no key"));
    assert!(crash_of("x := has(3, :a)\n").contains("cannot index"));
}

#[test]
fn get_returns_the_fallback_and_does_not_create() {
    assert_eq!(expr("get({ :a : 1 }, :a, 0)"), "1");
    assert_eq!(expr("get({ :a : 1 }, :b, 0)"), "0");
    assert_eq!(expr("get([10, 20], 1, 0)"), "20");
    assert_eq!(expr("get([10, 20], 5, :missing)"), ":missing");

    // Reading does not create the key (§5).
    let src = "d := { :a : 1 }\nseen := get(d, :b, 0)\nstill := has(d, :b)\n";
    assert_eq!(eval(src, "still"), ":false");

    // The result copies, like every other read (§5.1).
    let src = "
d := {}
first := get(d, :k, [])
second := get(d, :k, [])
shared := first === second
";
    assert_eq!(eval(src, "shared"), ":false");
}

#[test]
fn push_appends_through_a_reference_and_answers_the_new_length() {
    let src = "
rows := []
first := push(&rows, \"a\")
second := push(&rows, \"b\")
total := len(rows)
";
    assert_eq!(eval(src, "first"), "1");
    assert_eq!(eval(src, "second"), "2");
    assert_eq!(eval(src, "total"), "2");
    assert_eq!(eval(src, "rows"), "[a, b]");
}

#[test]
fn push_without_a_reference_crashes_rather_than_doing_nothing() {
    // The whole point of the signature: appending to a copy would look like
    // working code (§5.1).
    let crash = crash_of("rows := []\npush(rows, 1)\n");
    assert!(crash.contains("by reference"), "{crash}");
    assert!(crash.contains("copy"), "{crash}");
}

#[test]
fn push_copies_the_value_in_unless_it_is_a_reference() {
    let src = "
rows := []
item := { :n : 1 }
push(&rows, item)
item.n = 2
kept := rows[0].n
";
    assert_eq!(eval(src, "kept"), "1");

    let src = "
rows := []
item := { :n : 1 }
push(&rows, &item)
item.n = 2
seen := rows[0].n
";
    assert_eq!(eval(src, "seen"), "2");
}

#[test]
fn push_reaches_a_list_nested_in_a_structure() {
    let src = "
state := { :rows : [] }
push(&state.rows, \"x\")
n := len(state.rows)
";
    assert_eq!(eval(src, "n"), "1");
}

#[test]
fn push_splits_a_shared_list_first() {
    // Value semantics still hold: appending through a reference must not be
    // visible in a copy taken earlier (§5.1).
    let src = "
rows := [1]
snapshot := rows
push(&rows, 2)
snapshot_len := len(snapshot)
rows_len := len(rows)
";
    assert_eq!(eval(src, "snapshot_len"), "1");
    assert_eq!(eval(src, "rows_len"), "2");
}

#[test]
fn push_from_inside_a_trail() {
    // The `&` crossing into a trail is the shared mutable state §9.2 warns
    // about, and it is the only way trails can build one list together.
    let src = "
rows := []
parallel
\tpush(&rows, \"a\") || push(&rows, \"b\")
end
n := len(rows)
";
    assert_eq!(eval(src, "n"), "2");
}

#[test]
fn print_writes_the_text_form_and_returns_null() {
    // stdout is not captured here; `tests/cli.rs` checks what it writes.
    assert_eq!(eval("x := print(\"hi\")\n", "x"), ":null");
    assert_eq!(eval("x := print(\"hi\", \"\")\n", "x"), ":null");
}

// --- qualified builtins (§7) ------------------------------------------------

#[test]
fn a_builtin_is_reachable_through_a_leading_namespace_selector() {
    // Qualified syntax wins: `::name` is the language's own namespace.
    assert_eq!(eval("rows := []\n\n::push(&rows, 1)\nn := ::len(rows)\n", "n"), "1");

    // Past a local shadow.
    let src = "
len := fn(a) \"shadowed\"
rows := [1, 2]
theirs := len(rows)
ours := ::len(rows)
";
    assert_eq!(eval(src, "theirs"), "shadowed");
    assert_eq!(eval(src, "ours"), "2");

    // It is a value like any other.
    assert_eq!(eval("f := ::len\nx := f(\"abc\")\n", "x"), "3");
}

#[test]
fn a_builtin_is_not_a_variable() {
    assert!(run_source("::len = 1\n", "t.hy", opts()).is_err());
    assert!(run_source("x := &::len\n", "t.hy", opts()).is_err());
    assert!(crash_of("x := ::nope()\n").contains("no builtin named `nope`"));
}

#[test]
fn a_module_export_shadows_a_builtin_and_the_qualified_form_gets_past_it() {
    // `shadows.hy` exports its own `push`, which is §14's "push an image to a
    // host" rather than the list append.
    let src = "
use shadows as *
theirs := push(\"host\", \"img\")
rows := []
mine := ::push(&rows, 1)
";
    let result = run_source(src, "tests/fixtures/main.hy", Options { threads: 1, step_budget: 1, ..Options::default() }).expect("compiles");
    assert!(result.crash.is_none(), "{:?}", result.crash);
    let read =
        |name: &str| to_text(&result.root_scope.lookup(name).expect("binding").read().unwrap().clone());
    assert_eq!(read("theirs"), "pushed img to host");
    assert_eq!(read("mine"), "1");
}
