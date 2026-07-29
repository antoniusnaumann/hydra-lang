//! `check` tests (spec §11).
//!
//! The principle under test is as much what `check` *does not* say as what it
//! does: only what is guaranteed to crash.

use hydra::check::{check_program, CheckOptions};
use hydra::errors::{Report, Severity};
use hydra::parser::parse;

fn options() -> CheckOptions {
    CheckOptions { externs: Vec::new(), search_path: Vec::new() }
}

fn check(src: &str) -> Report {
    let program = parse(src, "tests/fixtures/t.hy").expect("parses");
    check_program(&program, &options())
}

fn codes(src: &str) -> Vec<&'static str> {
    check(src)
        .sorted()
        .into_iter()
        .filter(|d| d.severity == Severity::Error)
        .map(|d| d.code)
        .collect()
}

fn warnings(src: &str) -> Vec<&'static str> {
    check(src)
        .sorted()
        .into_iter()
        .filter(|d| d.severity == Severity::Warning)
        .map(|d| d.code)
        .collect()
}

fn message(src: &str) -> String {
    check(src).sorted().first().map(|d| d.message.clone()).unwrap_or_default()
}

// --- hard errors ------------------------------------------------------------

#[test]
fn assignment_to_a_name_with_no_binding() {
    assert_eq!(codes("x = 1\n"), vec!["assign-undeclared"]);
    assert_eq!(codes("x := 1\nx = 2\n"), Vec::<&str>::new());
    // The scope walk goes outward, so an outer binding is enough.
    assert_eq!(codes("x := 1\nif .true\n\tx = 2\nend\n"), Vec::<&str>::new());
    // But an inner one does not escape.
    assert_eq!(codes("if .true\n\ty := 1\nend\ny = 2\n"), vec!["assign-undeclared"]);
    // A compound assignment is an assignment and needs the same binding.
    assert_eq!(codes("x += 1\n"), vec!["assign-undeclared"]);
    assert_eq!(codes("x := 1\nx += 2\n"), Vec::<&str>::new());
}

#[test]
fn a_compound_assignment_reads_the_name_it_writes() {
    // `_hits = 1` writes a private and never reads it; `_hits += 1` reads it,
    // so it is used and the unused-private warning would be wrong (§11).
    assert_eq!(warnings("_hits := 0\n_hits = 1\n"), vec!["unused-private"]);
    assert_eq!(warnings("_hits := 0\n_hits += 1\n"), Vec::<&str>::new());
}

#[test]
fn a_name_declared_only_inside_a_trail_used_after_the_block() {
    // §11 calls this the best catch in the language, and §14 says `:=` inside
    // a trail is the mistake to look for.
    let src = "
parallel
\teu := 1 || us := 2
end
x := eu
";
    assert_eq!(codes(src), vec!["trail-local"]);
    assert!(message(src).contains("gone at the join") || message(src).contains("does not exist"));

    // Assigning to one is the same mistake.
    let src = "
parallel
\teu := 1 ||
end
eu = 2
";
    assert_eq!(codes(src), vec!["trail-local"]);

    // Declared above the block and assigned inside it is the correct shape.
    let src = "
eu := .null
parallel
\teu = 1 ||
end
x := eu
";
    assert_eq!(codes(src), Vec::<&str>::new());
}

#[test]
fn return_inside_a_trail() {
    let src = "
parallel
\treturn 1 || x := 2
end
";
    // The parser rejects nothing here; `check` is what reports it (§9.6).
    let program = parse(src, "t.hy").expect("parses");
    let report = check_program(&program, &options());
    assert_eq!(report.codes(), vec!["return-in-trail"]);

    // A function defined inside a trail may of course return.
    let src = "
parallel
\tf := fn(a) a + 1 ||
\tx := f(1)        ||
end
";
    assert_eq!(codes(src), Vec::<&str>::new());
}

#[test]
fn break_and_continue_labels() {
    assert_eq!(codes("for a in [1] as scan\n\tbreak scan\nend\n"), Vec::<&str>::new());
    assert_eq!(codes("for a in [1]\n\tbreak nope\nend\n"), vec!["unknown-label"]);
    assert_eq!(codes("for a in [1]\n\tcontinue nope\nend\n"), vec!["unknown-label"]);
    assert_eq!(codes("break\n"), vec!["break-outside-loop"]);
    assert_eq!(codes("continue\n"), vec!["continue-outside-loop"]);
    assert_eq!(codes("break trail\n"), vec!["break-outside-trail"]);

    // `break` with no label inside a trail ends the trail, which is allowed.
    assert_eq!(codes("parallel\n\tbreak || x := 1\nend\n"), Vec::<&str>::new());
    assert_eq!(codes("parallel\n\tbreak trail || x := 1\nend\n"), Vec::<&str>::new());
    // A block label is not a loop, so it cannot be continued.
    assert_eq!(
        codes("parallel as job\n\tcontinue job || x := 1\nend\n"),
        vec!["continue-block-label"]
    );
}

#[test]
fn key_read_on_a_dict_literal_that_provably_lacks_the_key() {
    assert_eq!(codes("d := { .a : 1 }\nx := d.b\n"), vec!["missing-key"]);
    assert_eq!(codes("x := { .a : 1 }.b\n"), vec!["missing-key"]);
    assert_eq!(codes("d := { .a : 1 }\nx := d[.b]\n"), vec!["missing-key"]);
    assert_eq!(codes("d := { .a : 1 }\nx := d.a\n"), Vec::<&str>::new());

    // Writing creates, so a later read is fine — and any write to the name at
    // all makes the key set unknowable, which is the safe direction.
    assert_eq!(codes("d := { .a : 1 }\nd.b = 2\nx := d.b\n"), Vec::<&str>::new());
    assert_eq!(codes("d := { .a : 1 }\nd = { .b : 2 }\nx := d.b\n"), Vec::<&str>::new());
    // A `&` can hand the dict to something that adds keys.
    assert_eq!(
        codes("fn f(v)\nend\nd := { .a : 1 }\nf(&d)\nx := d.b\n"),
        Vec::<&str>::new()
    );
    // A computed key says nothing.
    assert_eq!(codes("d := { .a : 1 }\nk := .b\nx := d[k]\n"), Vec::<&str>::new());
}

#[test]
fn passing_a_dict_by_value_cannot_change_it() {
    // Value semantics are what make the key analysis sound: `f(d)` gets a copy
    // (§5.1), so the literal's key set still holds afterwards.
    assert_eq!(
        codes("fn f(v)\nend\nd := { .a : 1 }\nf(d)\nx := d.b\n"),
        vec!["missing-key"]
    );
}

#[test]
fn duplicate_key_in_one_dict_literal() {
    assert_eq!(codes("d := { .a : 1, .a : 2 }\n"), vec!["duplicate-key"]);
    assert_eq!(codes("d := { .a : 1, .\"a\" : 2 }\n"), vec!["duplicate-key"]);
    assert_eq!(codes("d := { .a : 1, .b : 2 }\n"), Vec::<&str>::new());
}

#[test]
fn private_names_through_a_namespace() {
    assert_eq!(codes("use json\nx := json::_key\n"), vec!["private-through-namespace"]);
    assert_eq!(codes("use json\nx := json::decode(\"b\")\n"), Vec::<&str>::new());
    assert_eq!(codes("use json\nx := json::nope\n"), vec!["unknown-export"]);
    assert_eq!(codes("x := nope::thing\n"), vec!["unknown-module"]);
}

#[test]
fn a_call_no_candidate_accepts() {
    assert_eq!(codes("fn f(a, b)\nend\nf(1)\n"), vec!["no-matching-call"]);
    assert_eq!(codes("fn f(a, b)\nend\nf(1, 2)\n"), Vec::<&str>::new());
    assert_eq!(codes("g := fn(a) a\ng()\n"), vec!["no-matching-call"]);
    assert_eq!(codes("use json\njson::decode()\n"), vec!["no-matching-call"]);
    assert_eq!(codes("use json\ndecode()\n"), vec!["no-matching-call"]);
    // A name that is reassigned might hold anything by then.
    assert_eq!(codes("fn f(a)\nend\nf := fn(a, b) a\nf(1, 2)\n"), Vec::<&str>::new());
}

#[test]
fn named_arguments_are_matched_against_the_signature() {
    assert_eq!(codes("fn f(a, b)\nend\nf(1, b = 2)\n"), Vec::<&str>::new());
    assert_eq!(codes("fn f(a, b)\nend\nf(b = 2, a = 1)\n"), Vec::<&str>::new());
    // A name the function does not have, or one already filled positionally.
    assert_eq!(codes("fn f(a, b)\nend\nf(1, nope = 2)\n"), vec!["no-matching-call"]);
    assert_eq!(codes("fn f(a, b)\nend\nf(1, a = 2)\n"), vec!["no-matching-call"]);
    // A default may be skipped by naming a later parameter.
    assert_eq!(codes("fn f(a, b = 1, c = 2)\nend\nf(1, c = 3)\n"), Vec::<&str>::new());
}

#[test]
fn a_name_with_several_candidates_says_nothing() {
    // A call one candidate rejects goes to the next (§3), so no single
    // signature is guaranteed — which is exactly §11's principle.
    assert_eq!(codes("len := fn(a, b) a\nx := len(1)\n"), Vec::<&str>::new());
    assert_eq!(codes("use shadows\nx := push(1, 2, 3)\n"), Vec::<&str>::new());
}

#[test]
fn references_must_be_rooted_at_a_name() {
    // The parser already refuses `&f()`; `check` covers the rest of §5.1.
    assert!(parse("x := &f()\n", "t.hy").is_err());
    assert!(parse("x := &(a + b)\n", "t.hy").is_err());
    assert_eq!(codes("a := 1\nx := &a\n"), Vec::<&str>::new());
}

#[test]
fn a_compound_keyword_split_across_lines() {
    // `parallel` on one line and `for` on the next quietly means something
    // else: a block whose single trail holds a loop (§2, §11). It is caught in
    // the parser, because it never reaches a shape `check` could see.
    for src in [
        "parallel\nfor r in [1, 2]\n\tf(r)\nend\nend\n",
        "race\nwhile c\n\tf()\nend\nend\n",
    ] {
        let err = parse(src, "t.hy").unwrap_err();
        assert!(err.message.contains("may not be split across"), "{}", err.message);
    }
}

#[test]
fn undeclared_names_are_reported_when_resolution_is_exact() {
    assert_eq!(codes("x := missing\n"), vec!["undeclared-name"]);
    assert_eq!(codes("missing()\n"), vec!["undeclared-name"]);
    // `alive()` is the one primitive (§9.5).
    assert_eq!(codes("x := alive()\n"), Vec::<&str>::new());
    // A name the host supplies is not undeclared.
    let program = parse("print(\"hi\")\n", "t.hy").unwrap();
    let report = check_program(
        &program,
        &CheckOptions { externs: vec!["print".into()], search_path: Vec::new() },
    );
    assert!(report.codes().is_empty());
}

#[test]
fn an_unresolvable_module_switches_name_resolution_off() {
    // Guessing would break the "only what is guaranteed" principle (§11).
    let src = "use no_such_module\nx = 1\ny := whatever\n";
    let report = check(src);
    assert!(report.errors().next().is_none(), "{:?}", report.codes());
    assert!(report.codes().contains(&"unresolved-module"));
}

#[test]
fn forward_references_between_functions_are_fine() {
    // `a` is only called after both declarations have run.
    assert_eq!(codes("fn a()\n\treturn b()\nend\nfn b()\n\treturn 1\nend\nx := a()\n"), Vec::<&str>::new());
}

// --- warnings ---------------------------------------------------------------

#[test]
fn a_name_two_used_modules_both_export() {
    // Silent shadowing is the failure mode that reaches production (§11).
    assert_eq!(warnings("use http\nuse json\n"), vec!["ambiguous-import"]);
    assert!(warnings("use json\n").is_empty());
}

#[test]
fn else_followed_by_a_lone_nested_if() {
    let src = "if a\n\tx := 1\nelse\n\tif b\n\t\tx := 2\n\tend\nend\n";
    assert!(warnings(src).contains(&"else-then-if"), "{:?}", warnings(src));
    // `else if` is one keyword and warns about nothing.
    let src = "if a\n\tx := 1\nelse if b\n\tx := 2\nend\n";
    assert!(!warnings(src).contains(&"else-then-if"));
}

#[test]
fn a_side_effect_inside_a_racing_trail() {
    let src = "race\n\tsend() || other()\nend\n";
    let warned: Vec<_> = warnings(src).into_iter().filter(|c| *c == "effect-in-race").collect();
    assert_eq!(warned.len(), 2);
    // A `parallel` block runs every trail, so nothing to warn about there.
    let src = "parallel\n\tsend() || other()\nend\n";
    assert!(!warnings(src).contains(&"effect-in-race"));
}

#[test]
fn an_unused_private() {
    assert_eq!(warnings("_helper := 1\n"), vec!["unused-private"]);
    assert!(warnings("_helper := 1\nx := _helper\n").is_empty());
}

#[test]
fn a_reference_crossing_into_a_trail() {
    // The only way to share mutable data between trails (§9.2).
    let src = "a := { .x : 1 }\nparallel\n\tf(&a) || g()\nend\n";
    assert!(warnings(src).contains(&"ref-into-trail"), "{:?}", warnings(src));
}

#[test]
fn the_reference_program_reports_no_errors() {
    // §14's program imports `fmt`, `http` and `json`, which do not exist, so
    // name resolution switches off and the placeholders it calls cannot be
    // called guaranteed-crashes. What is left is warnings.
    let src = std::fs::read_to_string("examples/deploy.hy").expect("example");
    let program = parse(&src, "examples/deploy.hy").expect("parses");
    let report = check_program(&program, &options());
    let errors: Vec<_> = report.errors().collect();
    assert!(errors.is_empty(), "the spec's own program should check clean: {errors:?}");
    assert!(report.codes().contains(&"unresolved-module"));
}

#[test]
fn externs_stand_in_for_the_missing_standard_library() {
    // With every module resolvable, a placeholder call *is* a guaranteed
    // crash — unless the host is known to supply it (QUESTIONS.md §1).
    // `print` is a builtin now, so the placeholder here is one that is not.
    let src = "use json\nbody := read_file(\"x\")\nprint(json::decode(body))\n";
    let program = parse(src, "tests/fixtures/t.hy").expect("parses");
    assert_eq!(check_program(&program, &options()).codes(), vec!["undeclared-name"]);

    let with_extern =
        CheckOptions { externs: vec!["read_file".into()], search_path: Vec::new() };
    assert!(check_program(&program, &with_extern).codes().is_empty());
}

#[test]
fn a_by_reference_parameter_passed_by_value() {
    // §5.1 makes this a guaranteed crash, and before it was one it was a
    // silent no-op: the callee appends to a copy.
    let src = "rows := []\npush(rows, 1)\n";
    assert_eq!(codes(src), vec!["missing-reference"]);
    assert_eq!(codes("rows := []\npush(&rows, 1)\n"), Vec::<&str>::new());

    // The same rule for a user function that declares one.
    let src = "fn bump(&box)\n\tbox.n = 1\nend\nd := { .n : 0 }\nbump(d)\n";
    assert_eq!(codes(src), vec!["missing-reference"]);
    let src = "fn bump(&box)\n\tbox.n = 1\nend\nd := { .n : 0 }\nbump(&d)\n";
    assert_eq!(codes(src), Vec::<&str>::new());
}

#[test]
fn builtins_are_known_names_with_known_signatures() {
    assert_eq!(codes("print(\"hi\")\n"), Vec::<&str>::new());
    assert_eq!(codes("print(\"hi\", \"\")\n"), Vec::<&str>::new());
    assert_eq!(codes("print(\"hi\", terminator = \"\")\n"), Vec::<&str>::new());
    assert_eq!(codes("print()\n"), vec!["no-matching-call"]);
    assert_eq!(codes("print(\"a\", \"b\", \"c\")\n"), vec!["no-matching-call"]);
    assert_eq!(codes("x := len([1], 2)\n"), vec!["no-matching-call"]);
    assert_eq!(codes("x := get(d, .k)\n"), vec!["no-matching-call", "undeclared-name"]);
}

#[test]
fn an_unresolvable_module_also_silences_the_builtin_signatures() {
    // That module might export a `push` of its own, so nothing about the
    // unqualified name is guaranteed.
    let src = "use no_such_module\nrows := []\npush(rows, 1)\n";
    let report = check(src);
    assert!(report.errors().next().is_none(), "{:?}", report.codes());
}

#[test]
fn a_module_export_that_shadows_a_builtin_is_worth_a_second_look() {
    // The same failure mode as two modules exporting one name (§11): it still
    // returns *something*, so it reaches production.
    let src = "use shadows\nx := push(\"host\", \"img\")\n";
    assert!(warnings(src).contains(&"shadowed-builtin"), "{:?}", warnings(src));
    assert!(check(src).errors().next().is_none());
    // Qualifying says which one is meant, and the builtin's signature is known
    // even here, so the missing `&` is still caught.
    assert_eq!(codes("use shadows\nrows := []\nx := ::push(rows, 1)\n"), vec!["missing-reference"]);
    assert_eq!(codes("use shadows\nrows := []\nx := ::push(&rows, 1)\n"), Vec::<&str>::new());
}

#[test]
fn a_qualified_builtin_that_does_not_exist() {
    assert_eq!(codes("x := ::nope()\n"), vec!["unknown-builtin"]);
}
