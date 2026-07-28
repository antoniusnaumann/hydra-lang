//! Concurrency tests (spec §9).
//!
//! Scheduling is round-robin over one statement boundary at a time, so an
//! interleaving is reproducible and a test can assert on the exact order.

use hydra::value::to_text;
use hydra::vm::{run_source, Options, RunResult};

fn opts() -> Options {
    Options { search_path: Vec::new(), ..Options::default() }
}

fn run_with(src: &str, options: Options) -> RunResult {
    run_source(src, "t.hy", options).expect("compiles")
}

fn run(src: &str) -> RunResult {
    run_with(src, opts())
}

fn eval(src: &str, name: &str) -> String {
    let result = run(src);
    if let Some(crash) = &result.crash {
        panic!("unexpected crash: {crash}");
    }
    let cell = result.root_scope.lookup(name).unwrap_or_else(|| panic!("no binding `{name}`"));
    let value = cell.borrow().clone();
    to_text(&value)
}

// --- parallel (§9.3) --------------------------------------------------------

#[test]
fn every_trail_runs_and_the_block_joins() {
    let src = "
a := 0
b := 0
c := 0
parallel
\ta = 1 || b = 2 || c = 3
end
total := a + b + c
";
    assert_eq!(eval(src, "total"), "6");
}

#[test]
fn rows_are_cosmetic_there_is_no_barrier() {
    // Trail 1 finishes its only statement while trail 0 still has two to go,
    // and nothing waits for it (§9.3).
    let src = "
log := \"\"
parallel
\tlog = log + \"1\" || log = log + \"a\"
\tlog = log + \"2\" ||
\tlog = log + \"3\" ||
end
";
    assert_eq!(eval(src, "log"), "1a23");
}

#[test]
fn a_trail_reads_and_writes_its_parent_scope() {
    let src = "
n := 10
parallel
\tn = n + 1 || m := n
end
";
    assert_eq!(eval(src, "n"), "11");
}

#[test]
fn declarations_inside_a_trail_are_trail_local() {
    // §6: everything a trail declares with `:=` is gone at the join. This is
    // `check`'s single most valuable diagnostic, and at runtime it crashes.
    let src = "
parallel
\tlocal := 5 || other := 6
end
x := local
";
    let result = run(src);
    let crash = result.crash.expect("reading a trail-local after the block crashes");
    assert!(crash.message.contains("`local` is not declared"), "{}", crash.message);
}

#[test]
fn parallel_for_makes_one_trail_per_element() {
    let src = "
total := 0
parallel for n in [1, 2, 3, 4]
\ttotal = total + n
end
";
    assert_eq!(eval(src, "total"), "10");
}

#[test]
fn parallel_while_spawns_a_trail_per_iteration() {
    let src = "
n := 0
hits := 0
parallel while n < 3
\thits = hits + 1
\tn = n + 1
end
";
    // The condition is evaluated in the parent while the trails it already
    // spawned are running, so how many get spawned is genuinely racy — see
    // QUESTIONS.md §14. What is guaranteed: the block joins, every spawned
    // trail ran its body, and the loop ended.
    let result = run(src);
    assert!(result.crash.is_none());
    let read =
        |name: &str| to_text(&result.root_scope.lookup(name).expect("binding").borrow().clone());
    assert_eq!(read("hits"), read("n"), "every trail that ran did both statements");
    assert!(read("n").parse::<f64>().unwrap() >= 3.0, "the condition stopped holding");
}

#[test]
fn alive_is_true_in_a_live_trail() {
    let src = "
seen := .null
parallel
\tseen = alive() ||
end
";
    assert_eq!(eval(src, "seen"), ".true");
}

// --- race (§9.4) ------------------------------------------------------------

#[test]
fn race_is_decided_by_the_first_completion() {
    let src = "
winner := .null
race
\twinner = \"short\" || slow := 1
\t                  || slow = 2
\t                  || winner = \"long\"
end
";
    assert_eq!(eval(src, "winner"), "short");
}

#[test]
fn a_losing_trail_may_never_run_at_all() {
    // §9.1: a trail may still be unscheduled when a race is decided.
    let src = "
log := \"\"
race
\tlog = log + \"w\" || log = log + \"l\"
end
";
    assert_eq!(eval(src, "log"), "w");
}

#[test]
fn nothing_records_the_winner() {
    // §9.4: if you need to know, write it down as the trail's last statement.
    let src = "
who := .null
race
\twho = \"a\" || who = \"b\"
end
";
    assert_eq!(eval(src, "who"), "a");
}

// --- cancellation (§9.5) ----------------------------------------------------

#[test]
fn an_in_flight_call_runs_to_the_end_and_its_result_is_discarded() {
    // The whole of §9.5 in one program: the loser's call finishes (its effect
    // lands), the pending assignment does not happen, and no further statement
    // of that trail runs.
    let src = "
log := \"\"
result := .null
fn work()
\tlog = log + \"effect\"
\treturn \"value\"
end
race
\tresult = work()     || log = log + \"w\"
\tlog = log + \"never\" ||
end
";
    let result = run(src);
    assert!(result.crash.is_none());
    let read = |name: &str| {
        to_text(&result.root_scope.lookup(name).expect("binding").borrow().clone())
    };
    assert_eq!(read("log"), "weffect", "the call completed but the trail stopped after it");
    assert_eq!(read("result"), ".null", "the pending assignment was discarded");
}

#[test]
fn alive_goes_false_inside_a_cancelled_trail() {
    let src = "
status := .null
fn check()
\tstatus = alive()
\treturn 1
end
race
\tr := check() || w := 1
end
";
    assert_eq!(eval(src, "status"), ".false");
}

#[test]
fn a_crash_in_a_dead_trail_is_isolated() {
    let src = "
after := 0
fn boom()
\tx := missing_name
\treturn 1
end
race
\tr := boom() || w := 1
end
after = 1
";
    let result = run_with(src, Options { report_dead_crashes: false, ..opts() });
    assert!(result.crash.is_none(), "the program continues past a dead trail's crash");
    assert_eq!(result.dead_crashes.len(), 1);
    let after = result.root_scope.lookup("after").expect("binding").borrow().clone();
    assert_eq!(to_text(&after), "1");
}

#[test]
fn a_dead_trail_crash_is_fatal_under_strict_mode() {
    let src = "
fn boom()
\tx := missing_name
\treturn 1
end
race
\tr := boom() || w := 1
end
";
    let result =
        run_with(src, Options { strict: true, report_dead_crashes: false, ..opts() });
    assert!(result.crash.is_some(), "strict mode makes it fatal so tests fail on it");
}

#[test]
fn a_live_crash_stops_every_sibling() {
    // §8: mark every sibling cancelled, let each finish its in-flight
    // statement, then exit non-zero.
    let src = "
log := \"\"
parallel
\tboom := missing_name || log = log + \"a\"
\t                     || log = log + \"b\"
end
";
    let result = run(src);
    assert!(result.crash.is_some());
    let log = result.root_scope.lookup("log").expect("binding").borrow().clone();
    // The sibling's first statement had already been queued behind the crash;
    // whatever it managed, it stopped before running everything.
    assert!(to_text(&log).len() < 2, "siblings stopped early, got {:?}", to_text(&log));
}

// --- control flow inside trails (§9.6) --------------------------------------

#[test]
fn break_ends_the_current_trail() {
    let src = "
log := \"\"
parallel
\tlog = log + \"a\" || log = log + \"x\"
\tbreak           ||
\tlog = log + \"b\" ||
end
";
    assert_eq!(eval(src, "log"), "ax");
}

#[test]
fn break_trail_reaches_out_of_a_loop() {
    let src = "
log := \"\"
parallel
\tfor n in [1, 2, 3] || log = log + \"x\"
\t\tlog = log + \"n\" ||
\t\tbreak trail      ||
\tend                ||
end
";
    assert_eq!(eval(src, "log"), "xn");
}

#[test]
fn break_with_a_label_still_targets_the_loop() {
    let src = "
log := \"\"
parallel
\tfor n in [1, 2, 3] as scan || log = log + \"x\"
\t\tlog = log + \"n\"         ||
\t\tbreak scan               ||
\tend                        ||
\tlog = log + \"after\"       ||
end
";
    assert_eq!(eval(src, "log"), "xnafter");
}

#[test]
fn return_inside_a_trail_is_rejected() {
    // §9.6: `check` rejects it; the compiler will not emit it either.
    let src = "
parallel
\treturn 1 || x := 2
end
";
    let err = run_source(src, "t.hy", opts()).unwrap_err();
    assert!(err.message.contains("`return` inside a trail"), "{}", err.message);
}

#[test]
fn a_larger_step_budget_still_produces_the_same_result() {
    // Scheduling is nondeterministic by design; only the join is guaranteed.
    let src = "
a := 0
b := 0
parallel
\ta = 1 || b = 2
end
total := a + b
";
    let result = run_with(src, Options { step_budget: 64, ..opts() });
    let total = result.root_scope.lookup("total").expect("binding").borrow().clone();
    assert_eq!(to_text(&total), "3");
}

#[test]
fn a_nested_block_opened_by_a_called_function_joins_too() {
    // A trail can suspend inside a `parallel` a called function opened.
    let src = "
log := \"\"
fn fan()
\tparallel
\t\tlog = log + \"i\" || log = log + \"j\"
\tend
\treturn 1
end
parallel
\tr := fan() || log = log + \"o\"
end
";
    let out = eval(src, "log");
    assert_eq!(out.len(), 3, "every trail ran exactly once, got {out:?}");
    assert!(out.contains('i') && out.contains('j') && out.contains('o'), "{out:?}");
}
