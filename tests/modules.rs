//! Module tests (spec §7).
//!
//! `use name` does two separable things: it *executes* the file's toplevel,
//! skipped if that file is already in scope, and it *binds* the module, which
//! always runs. What binding means is what the three forms differ in: `use fs`
//! registers the qualifier alone, `use fs as *` binds the names unqualified as
//! well, and `use fs as filesystem` puts the qualifier under that name.

use hydra::value::to_text;
use hydra::vm::{run_source, Options, RunResult};

/// Run with the fixture directory as the importing file's own directory, so
/// `use` resolves the way §7 says: same directory first.
fn run(src: &str) -> RunResult {
    run_source(
        src,
        "tests/fixtures/main.hy",
        Options { search_path: Vec::new(), threads: 1, step_budget: 1, ..Options::default() },
    )
    .expect("compiles")
}

fn eval(src: &str, name: &str) -> String {
    let result = run(src);
    if let Some(crash) = &result.crash {
        panic!("unexpected crash: {crash}");
    }
    to_text(&result.root_scope.lookup(name).unwrap_or_else(|| panic!("no `{name}`")).read().unwrap().clone())
}

#[test]
fn most_recent_use_wins_and_namespaces_disambiguate() {
    // The spec's own example: both modules export `decode`.
    let src = "
use http as *
use json as *
unqualified := decode(\"body\")
explicit := http::decode(\"body\")
also := json::decode(\"body\")
";
    assert_eq!(eval(src, "unqualified"), "json:body");
    assert_eq!(eval(src, "explicit"), "http:body");
    assert_eq!(eval(src, "also"), "json:body");
}

#[test]
fn source_order_decides_even_when_the_other_module_comes_second() {
    let src = "
use json as *
use http as *
unqualified := decode(\"body\")
";
    assert_eq!(eval(src, "unqualified"), "http:body");
}

#[test]
fn private_names_are_neither_exported_nor_reachable() {
    let result = run("use json as *\nx := _key\n");
    assert!(result.crash.expect("crash").message.contains("`_key` is not declared"));

    let result = run("use json\nx := json::_key\n");
    assert!(result.crash.expect("crash").message.contains("private"));

    let result = run("use json\nx := json::_hidden()\n");
    assert!(result.crash.expect("crash").message.contains("private"));
}

#[test]
fn a_files_toplevel_executes_exactly_once_per_program() {
    // `bump` increments a counter that lives in `base`. Two `use`s of `bump`
    // must leave it at 1: the second binds without executing (§7).
    let src = "
use bump
use bump
use base as *
count := ticks
";
    assert_eq!(eval(src, "count"), "1");
}

#[test]
fn binding_runs_again_even_when_execution_is_skipped() {
    // `use json` after `use http` rebinds `decode` even though `json` may
    // already have been executed transitively.
    let src = "
use json as *
use http as *
use json as *
which := decode(\"b\")
";
    assert_eq!(eval(src, "which"), "json:b");
}

#[test]
fn circular_imports_resolve_to_whatever_is_bound_so_far() {
    let src = "
use circ_a as *
use circ_b as *
a := a_val
b := b_val
";
    assert_eq!(eval(src, "a"), "1");
    assert_eq!(eval(src, "b"), "2");
}

#[test]
fn a_module_does_not_see_the_importers_names() {
    // Imports are per importing file (QUESTIONS.md §10).
    let src = "
use needs_x as *
x := 1
seen := peek()
";
    let result = run(src);
    assert!(result.crash.expect("crash").message.contains("`x` is not declared"));
}

#[test]
fn selecting_from_a_module_that_was_never_used_crashes() {
    let result = run("x := nope::thing\n");
    assert!(result.crash.expect("crash").message.contains("no module `nope`"));
}

#[test]
fn a_missing_module_file_crashes_with_the_path_it_looked_in() {
    let result = run("use no_such_module\n");
    let crash = result.crash.expect("crash");
    assert!(crash.message.contains("cannot find module `no_such_module`"), "{}", crash.message);
}

#[test]
fn an_imported_name_shares_the_modules_storage() {
    // `mod::x` and an unqualified `x` are the same variable (QUESTIONS.md §10).
    let src = "
use base as *
ticks = 7
seen := base::ticks
";
    assert_eq!(eval(src, "seen"), "7");
}

// --- the three forms of `use` (§7) ------------------------------------------

#[test]
fn a_plain_use_brings_a_module_in_for_qualified_calling_only() {
    assert_eq!(eval("use json\nx := json::decode(\"b\")\n", "x"), "json:b");
    // Nothing of it is reachable bare, so an import cannot capture a name.
    let result = run("use json\nx := decode(\"b\")\n");
    assert!(result.crash.expect("crash").message.contains("`decode` is not declared"));
}

#[test]
fn a_star_import_binds_the_names_and_keeps_the_qualifier() {
    // Two star imports that collide need a way to say which one is meant.
    let src = "
use http as *
use json as *
bare := decode(\"b\")
theirs := http::decode(\"b\")
";
    assert_eq!(eval(src, "bare"), "json:b");
    assert_eq!(eval(src, "theirs"), "http:b");
}

#[test]
fn an_alias_replaces_the_qualifier_rather_than_adding_one() {
    assert_eq!(eval("use json as codec\nx := codec::decode(\"b\")\n", "x"), "json:b");
    let result = run("use json as codec\nx := json::decode(\"b\")\n");
    assert!(result.crash.expect("crash").message.contains("no module `json`"));
    // And an alias that renames nothing is a mistake worth catching.
    let error = run_source("use json as json\n", "tests/fixtures/main.hy", Options::default())
        .expect_err("rejected")
        .to_string();
    assert!(error.contains("the name it already has"), "{error}");
}

#[test]
fn a_qualifier_works_through_the_dot() {
    // `x.mod::f(…)` is exactly `mod::f(x, …)` (§5.2, §7).
    let src = "
use json
through := \"b\".json::decode()
written := json::decode(\"b\")
";
    assert_eq!(eval(src, "through"), "json:b");
    assert_eq!(eval(src, "written"), "json:b");
}
