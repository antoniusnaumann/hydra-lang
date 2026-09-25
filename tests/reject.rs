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

#[test]
fn what_a_rejecting_candidate_wrote_does_not_reach_the_next_one() {
    // Holding the arguments for the fall-back is free — copy-on-write means a
    // handle, not a copy — but the next candidate has to be handed what the
    // caller wrote (§5.1). So the retained arguments are marked shared, and a
    // write in the candidate that rejects splits a copy of its own.
    let src = "
fn take(d)
	return d.n
end
fn take(d)
	d.n = 99
	reject()
end
answered := take({ :n : 1 })
";
    assert_eq!(eval(src, "answered"), "1");

    let src = "
fn eat(list)
	return len(list)
end
fn eat(list)
	push(&list, 2)
	reject()
end
answered := eat([1])
";
    assert_eq!(eval(src, "answered"), "1");

    // And the caller's own value is untouched either way.
    let src = "
fn take(d)
	return d.n
end
fn take(d)
	d.n = 99
	reject()
end
box := { :n : 1 }
answered := take(box)
kept := box.n
";
    assert_eq!(eval(src, "answered"), "1");
    assert_eq!(eval(src, "kept"), "1");
}

// --- when nobody takes it -----------------------------------------------------

#[test]
fn every_refusal_is_printed_when_the_rejection_goes_unhandled() {
    // Nothing took the call, so it answers with the rejection, and a statement
    // at the top level has nowhere to hand that (§8.1).
    let src = "
fn pick(n)
\treject(\"the first wants something else\")
end
fn pick(n)
\treject()
end
pick(1)
";
    let crash = crash_of(src);
    assert!(crash.contains("unhandled rejection"), "{crash}");
    assert!(crash.contains("no `pick` took 1 argument(s)"), "{crash}");
    // The one that ran last is nearest the call, so it is listed first.
    assert!(crash.contains("pick(n) rejected it\n"), "{crash}");
    assert!(crash.contains("pick(n) rejected it: the first wants something else"), "{crash}");
}

#[test]
fn a_lone_function_that_rejects_says_so() {
    let crash = crash_of("fn only(n)\n\treject(\"nothing behind me\")\nend\nonly(1)\n");
    assert!(crash.contains("unhandled rejection"), "{crash}");
    assert!(crash.contains("only(n) rejected it: nothing behind me"), "{crash}");
}

#[test]
fn a_rejection_nothing_takes_is_the_answer_of_the_call() {
    // `reject(msg)` is `return [:reject, msg]`: consumed, it is a value like any
    // other, and the last candidate tried is the one whose answer it is.
    let src = "
fn pick(n)
\treject(\"the first wants something else\")
end
fn pick(n)
\treject()
end
x := pick(1)
why := x[1]
";
    assert_eq!(eval(src, "x"), "[:reject, the first wants something else]");
    assert_eq!(eval(src, "why"), "the first wants something else");
}

#[test]
fn a_rejection_at_the_top_level_has_nowhere_to_go() {
    let crash = crash_of("reject(\"nope\")\n");
    assert!(crash.contains("unhandled rejection: nope"), "{crash}");
    assert!(crash.contains("top level"), "{crash}");
    // A trail of a top-level block is still the top level.
    let crash = crash_of("parallel\n\treject(\"x\") || y := 1\nend\n");
    assert!(crash.contains("unhandled rejection: x"), "{crash}");
    // A trail cannot return from its function (§9.6), so it cannot hand back
    // the function's call either.
    let crash = crash_of("fn f(a)\n\tparallel\n\t\treject() || y := 1\n\tend\nend\nf(1)\n");
    assert!(crash.contains("cannot return from its function"), "{crash}");
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
fn writing_before_rejecting_is_worth_a_warning() {
    // The copy is what the write costs, and the call throws it away (§3).
    let warned = |src: &str| codes(src).contains(&"write-before-reject");
    assert!(warned("fn f(d)\n\td.n = 1\n\treject()\nend\n"));
    // Looking before writing costs nothing at all.
    assert!(!warned("fn f(d)\n\tif d.n > 3\n\t\treject()\n\tend\n\td.n = 1\nend\n"));
    // A counter of its own copies nothing, so nothing is said about it.
    assert!(!warned("fn f(d)\n\tn := 0\n\tn = n + 1\n\tif n > 0\n\t\treject()\n\tend\nend\n"));
    // A local taken from an argument shares its buffer, so writing it splits
    // the same node.
    assert!(warned("fn f(d)\n\tlocal := d\n\tlocal.n = 1\n\treject()\nend\n"));
    // Through a reference it is not a copy but an effect: the write reached the
    // caller's own value and stays there.
    assert!(warned("fn f(&box)\n\tbox.n = 1\n\treject()\nend\n"));
}

#[test]
fn reject_outside_a_function_is_an_error() {
    assert_eq!(codes("reject()\n"), vec!["reject-outside-function"]);
    assert_eq!(codes(":reject\n"), vec!["reject-outside-function"]);
    assert_eq!(
        codes("parallel\n\treject() || y := 1\nend\n"),
        vec!["reject-outside-function"]
    );
    assert_eq!(codes("fn f(a)\n\treject()\nend\n"), Vec::<&str>::new());
    // Consumed, it is only a value (§8.1).
    assert_eq!(codes("x := reject()\nprint(x)\n"), Vec::<&str>::new());
}

#[test]
fn a_trail_cannot_reject_for_its_function() {
    assert_eq!(
        codes("fn f(a)\n\tparallel\n\t\treject() || print(a)\n\tend\nend\n"),
        vec!["reject-in-trail"]
    );
}

#[test]
fn a_shadow_can_reject_through_a_helper() {
    let shadowed = |src: &str| codes(src).contains(&"unreachable-overload");
    let earlier = "fn f(a)\n\treturn 1\nend\n";
    // `return :reject` is `reject()` spelled out.
    assert!(!shadowed(&format!("{earlier}fn f(a)\n\tif a\n\t\treturn :reject\n\tend\n\treturn 2\nend\n")));
    // A helper's `:reject`, unconsumed, is the shadow's own (§8.1) — through
    // any number of helpers.
    let helpers = "fn deny(c)\n\tif c\n\t\treturn :reject, \"no\"\n\tend\nend\nfn guard(c)\n\tdeny(c)\nend\n";
    assert!(!shadowed(&format!("{helpers}{earlier}fn f(a)\n\tguard(a)\n\treturn 2\nend\n")));
    assert!(!shadowed(&format!("{helpers}{earlier}fn f(a)\n\treturn guard(a)\nend\n")));
    // Consumed, it goes nowhere, and the shadow never rejects.
    assert!(shadowed(&format!("{helpers}{earlier}fn f(a)\n\t_ = guard(a)\n\treturn 2\nend\n")));
    // A call the file cannot see into might reject, and an error has to allow
    // for that.
    assert!(!shadowed(&format!("{earlier}fn f(a)\n\ta()\n\treturn 2\nend\n")));
    // A builtin never does.
    assert!(shadowed(&format!("{earlier}fn f(a)\n\tprint(a)\n\treturn 2\nend\n")));
}

#[test]
fn writing_before_a_helper_rejects_is_worth_a_warning_too() {
    let warned = |src: &str| codes(src).contains(&"write-before-reject");
    let helper = "fn deny(c)\n\tif c\n\t\treturn :reject\n\tend\nend\n";
    assert!(warned(&format!("{helper}fn f(d)\n\td.n = 1\n\tdeny(d.n > 0)\nend\n")));
    assert!(warned("fn f(d)\n\td.n = 1\n\treturn :reject\nend\n"));
    // A call the file cannot see into is not enough for a warning.
    assert!(!warned("fn f(d, g)\n\td.n = 1\n\tg()\nend\n"));
}

#[test]
fn reject_constructs_one_tagged_list() {
    assert_eq!(eval("x := reject(\"why\")\n", "x"), "[:reject, why]");
    assert_eq!(eval("x := reject(msg = \"why\")\n", "x"), "[:reject, why]");
    assert_eq!(eval("x := reject()\n", "x"), "[:reject, :null]");
    assert_eq!(eval("x := ::reject(42)\n", "x"), "[:reject, 42]");
    // Defining it in Hydra has the same value semantics as the builtin.
    assert_eq!(eval("fn reject(msg)\nreturn [:reject, msg]\nend\nx := reject(\"why\")\n", "x"), "[:reject, why]");
}

#[test]
fn a_returned_or_unconsumed_tagged_list_retries_the_call() {
    for rejection in ["return [:reject, \"try the fallback\"]", "[:reject, \"try the fallback\"]", "return reject(\"try the fallback\")"] {
        let source = format!("fn pick(n)\nreturn 42\nend\nfn pick(n)\n{rejection}\nend\nx := pick(1)\n");
        assert_eq!(eval(&source, "x"), "42");
        assert!(!codes(&source).contains(&"unreachable-overload"));
    }
}

#[test]
fn the_tagged_list_shape_is_exact_and_can_be_consumed() {
    for expression in ["[]", "[:reject]", "[:reject, 1, 2]", "[:other, \"message\"]"] {
        assert_eq!(eval(&format!("fn f()\nreturn {expression}\nend\nx := f()\n"), "x"), expression.replace('"', ""));
    }
    let source = "fn f()\nx := [:reject, \"kept as data\"]\n_ = reject(\"discarded\")\nreturn x\nend\nx := f()\n";
    assert_eq!(eval(source, "x"), "[:reject, kept as data]");
}

#[test]
fn nested_helpers_preserve_all_refusals_without_duplicate_payloads() {
    let source = r#"
fn inner(n)
    return [:reject, "inner-first"]
end
fn inner(n)
    reject("inner-second")
end
fn outer(n)
    reject("outer-fallback")
end
fn outer(n)
    inner(n)
end
outer(1)
"#;
    let crash = crash_of(source);
    for message in ["inner-first", "inner-second", "outer-fallback"] {
        assert_eq!(crash.matches(message).count(), 1, "{crash}");
    }
    assert!(crash.find("inner-second").unwrap() < crash.find("inner-first").unwrap());
    assert!(crash.find("inner-first").unwrap() < crash.find("outer-fallback").unwrap());
}

#[test]
fn consumed_refusals_are_not_attached_to_a_new_rejection() {
    for consume in ["x := old()", "_ = old()", "x := [old()]"] {
        let source = format!("fn old()\nreject(\"unrelated-message\")\nend\nfn current()\n{consume}\nreturn [:reject, \"current-message\"]\nend\ncurrent()\n");
        let crash = crash_of(&source);
        assert!(crash.contains("current-message"), "{crash}");
        assert!(!crash.contains("unrelated-message"), "{crash}");
    }
}

#[test]
fn standard_handler_renders_tagged_payloads_and_checker_recognizes_them() {
    let crash = crash_of("[:reject, { :reason : 42 }]\n");
    assert!(crash.contains("unhandled rejection: { :reason : 42 }"), "{crash}");
    assert_eq!(codes("[:reject, \"no\"]\n"), vec!["reject-outside-function"]);
    assert_eq!(codes("x := [:reject, \"data\"]\n"), Vec::<&str>::new());
    assert!(codes("fn f(d)\nd.x = 1\nreturn [:reject, \"no\"]\nend\n").contains(&"write-before-reject"));
}

#[test]
fn a_consumed_boolean_operand_does_not_supply_refusal_messages() {
    let source = "fn old()\nreject(\"unrelated-message\")\nend\nfn current()\nreturn old() and :reject\nend\ncurrent()\n";
    let crash = crash_of(source);
    assert!(crash.contains("unhandled rejection"), "{crash}");
    assert!(!crash.contains("unrelated-message"), "{crash}");
}
