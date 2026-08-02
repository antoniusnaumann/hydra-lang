//! Auto-channels, and the two language features underneath them: variadic
//! parameters and multiple return values (`spec/hydra_channels.md`).

use hydra::value::to_text;
use hydra::vm::{run_source, Options, RunResult};

/// One worker thread, so the interleaving is round-robin and reproducible.
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

// --- variadic parameters (§6.1) ---------------------------------------------

#[test]
fn a_variadic_collects_what_is_left_into_a_list() {
    let src = "
fn log(prefix, values*)
\treturn values
end
none := log(\"a\")
some := log(\"a\", 1, 2)
";
    assert_eq!(eval(src, "none"), "[]");
    assert_eq!(eval(src, "some"), "[1, 2]");
}

#[test]
fn everything_after_the_star_can_only_be_named() {
    let src = "
fn log(prefix, values*, sep = \"-\")
\treturn \"\\(prefix)\\(sep)\\(len(values))\"
end
plain := log(\"a\", 1, 2)
named := log(\"a\", 1, 2, sep = \"+\")
";
    assert_eq!(eval(src, "plain"), "a-2");
    assert_eq!(eval(src, "named"), "a+2");
}

#[test]
fn a_bare_star_closes_the_positional_list_and_collects_nothing() {
    let src = "
fn retry(host, *, attempts = 3)
\treturn attempts
end
default := retry(\"eu\")
named := retry(\"eu\", attempts = 5)
";
    assert_eq!(eval(src, "default"), "3");
    assert_eq!(eval(src, "named"), "5");
    assert!(
        crash_of("fn retry(host, *, attempts = 3)\n\treturn attempts\nend\nretry(\"eu\", 5)\n")
            .contains("no `retry` accepts"),
        "a bare `*` takes no positional argument"
    );
}

#[test]
fn a_variadic_cannot_be_filled_by_name() {
    // Naming it would put one value where a list belongs.
    let crash = crash_of("fn log(values*)\n\treturn values\nend\nlog(values = 1)\n");
    assert!(crash.contains("no `log` accepts"), "{crash}");
}

#[test]
fn a_concrete_arity_beats_a_variadic_even_when_the_variadic_is_nearer() {
    // §6.1: a `*` accepts everything positional, so trying it last is what
    // keeps a narrower candidate reachable.
    let src = "
fn f(a)
\treturn \"one\"
end
f := fn(rest*) \"many\"
narrow := f(1)
wide := f(1, 2)
";
    assert_eq!(eval(src, "narrow"), "one");
    assert_eq!(eval(src, "wide"), "many");
}

// --- multiple return values (§6.2) ------------------------------------------

#[test]
fn a_function_may_answer_with_several_values_and_the_extras_are_dropped() {
    let src = "
fn parse(text)
\treturn text, len(text)
end
only := parse(\"hello\")
value, size := parse(\"hello\")
";
    assert_eq!(eval(src, "only"), "hello");
    assert_eq!(eval(src, "value"), "hello");
    assert_eq!(eval(src, "size"), "5");
}

#[test]
fn several_values_land_in_an_assignment_too() {
    let src = "
fn two()
\treturn 1, 2
end
a := 0
d := { .k : 0 }
a, d.k = two()
key := d.k
";
    assert_eq!(eval(src, "a"), "1");
    assert_eq!(eval(src, "key"), "2");
}

#[test]
fn naming_more_values_than_arrive_is_a_crash() {
    let crash = crash_of("fn two()\n\treturn 1, 2\nend\na, b, c := two()\n");
    assert!(crash.contains("answers with 2 values, but 3 were named"), "{crash}");
}

// --- the channel calls (§1–§5) ----------------------------------------------

#[test]
fn a_value_reaches_a_sibling_and_says_which_one_sent_it() {
    let src = "
got := .null
from := .null
parallel
\tsend(\"ready\") || got, from = receive()
end
";
    assert_eq!(eval(src, "got"), "ready");
    assert_eq!(eval(src, "from"), "0");
}

#[test]
fn an_index_addresses_one_sibling() {
    let src = "
one := .null
two := .null
parallel
\tsend(\"x\", 2) || one = receive(0) || two = receive(0)
end
";
    // Trail 0 addressed trail 2, so trail 1 waits for a value that never comes
    // and hears the channel close instead (§1).
    assert_eq!(eval(src, "two"), "x");
    assert_eq!(eval(src, "one"), ".null");
}

#[test]
fn receive_answers_null_on_both_when_no_sender_is_left() {
    // The one answer a sender cannot fake: a real send always arrives with a
    // real index behind it (§1).
    let src = "
value := .true
channel := .true
parallel
\tsend(.null) || value, channel = receive()
\t            || value, channel = receive()
end
";
    assert_eq!(eval(src, "value"), ".null");
    assert_eq!(eval(src, "channel"), ".null");
}

#[test]
fn send_says_when_nobody_is_left_to_receive() {
    let src = "
first := .null
second := .null
parallel
\tfirst = send(1)   || taken := receive()
\tsecond = send(2)  ||
end
";
    assert_eq!(eval(src, "first"), ".true");
    assert_eq!(eval(src, "second"), ".false");
}

#[test]
fn a_worker_loop_ends_when_the_producer_does() {
    // §4's example, which is the shape this whole feature exists for.
    let src = "
ran := 0
parallel
\tfor job in [1, 2, 3] || while alive()
\tsend(job)            || work, ch := receive()
\tend                  || if ch == .null
\t                     || break
\t                     || end
\t                     || ran = ran + work
\t                     || end
end
";
    assert_eq!(eval(src, "ran"), "6");
}

#[test]
fn detach_buffers_and_does_not_wait() {
    let src = "
sent := .null
later := .null
parallel
\tsent = send(7, mode = .detach) || later = receive()
end
";
    assert_eq!(eval(src, "sent"), ".true");
    assert_eq!(eval(src, "later"), "7");
}

#[test]
fn broadcast_reaches_every_eligible_trail() {
    let src = "
a := .null
b := .null
parallel
\tsend(9, mode = .broadcast) || a = receive() || b = receive()
end
";
    assert_eq!(eval(src, "a"), "9");
    assert_eq!(eval(src, "b"), "9");
}

#[test]
fn channel_answers_a_trails_own_index() {
    let src = "
here := .null
there := .null
parallel
\there = channel() || there = channel()
end
";
    assert_eq!(eval(src, "here"), "0");
    assert_eq!(eval(src, "there"), "1");
}

// --- what cannot happen (§6.5–§6.7) -----------------------------------------

#[test]
fn every_trail_waiting_is_a_crash_and_not_a_hang() {
    let crash = crash_of("parallel\n\tv := receive() || w := receive()\nend\n");
    assert!(crash.contains("every trail in this block is waiting"), "{crash}");
}

#[test]
fn an_index_that_cannot_exist_crashes() {
    let crash = crash_of("parallel\n\tsend(1, 2) || v := receive()\nend\n");
    assert!(crash.contains("no trail 2: this block has 2 trails"), "{crash}");
}

#[test]
fn a_trail_cannot_name_itself() {
    let crash = crash_of("parallel\n\tsend(1, 0) || v := receive()\nend\n");
    assert!(crash.contains("not to itself"), "{crash}");
}

#[test]
fn a_cancelled_trail_is_woken_and_then_runs_nothing() {
    // The race is decided while the other trail is parked in `receive`. It
    // wakes with the closed answer, and its next statement never runs (§6.5).
    let src = "
reached := .false
race
\tx := 1 || v, ch := receive()
\t       || reached = .true
end
";
    let result = run(src);
    assert!(result.crash.is_none(), "{:?}", result.crash);
    assert_eq!(
        to_text(&result.root_scope.lookup("reached").expect("binding").read().unwrap().clone()),
        ".false"
    );
}

#[test]
fn the_channel_calls_do_not_reach_outside_a_block() {
    let crash = crash_of("v := receive()\n");
    assert!(crash.contains("there is no trail here"), "{crash}");
}
