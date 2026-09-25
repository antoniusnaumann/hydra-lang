//! Tagged process-control values are data until a statement leaves them unconsumed.
use hydra::value::to_text;
use hydra::vm::{run_source, Options, RunResult};

fn run(source: &str, threads: usize) -> RunResult {
    run_source(source, "termination.hy", Options { threads, step_budget: 1, report_dead_crashes: false, ..Options::default() }).unwrap()
}
fn value(result: &RunResult, name: &str) -> String {
    to_text(&result.root_scope.lookup(name).unwrap().read().unwrap())
}

#[test]
fn builtins_only_construct_lists_when_consumed() {
    let result = run("a := exit(7)\nb := panic(\"why\")\nc := exit()\nd := ::exit(code = 12)\ne := ::panic(msg = \"oops\")\nafter := 42\n", 1);
    assert!(result.ok());
    assert_eq!(result.exit_code, None);
    assert_eq!(value(&result, "a"), "[:exit, 7]");
    assert_eq!(value(&result, "b"), "[:panic, why]");
    assert_eq!(value(&result, "c"), "[:exit, 0]");
    assert_eq!(value(&result, "d"), "[:exit, 12]");
    assert_eq!(value(&result, "e"), "[:panic, oops]");
    assert_eq!(value(&result, "after"), "42");
}

#[test]
fn exit_returns_the_status_to_the_host_without_crashing() {
    for expression in ["exit(23)", "[:exit, 23]"] {
        let result = run(&format!("before := 1\n\n{expression}\nafter := 2\n"), 1);
        assert_eq!(result.exit_code, Some(23));
        assert!(result.crash.is_none());
        assert!(!result.ok());
        assert!(result.root_scope.lookup("after").is_none());
    }
    let result = run("exit()\nafter := 1\n", 1);
    assert_eq!(result.exit_code, Some(0));
    assert!(result.ok());
    assert!(result.root_scope.lookup("after").is_none());
}

#[test]
fn panic_reports_the_message_at_the_handling_statement() {
    for expression in ["panic(\"broken\")", "[:panic, \"broken\"]"] {
        let result = run(&format!("fn helper()\n{expression}\nend\nhelper()\nafter := 1\n"), 1);
        let crash = result.crash.unwrap();
        assert_eq!(crash.message, "panic: broken");
        assert_eq!(crash.site.pos.line, 2);
        assert_eq!(crash.trace[0].pos.line, 4);
        assert_eq!(result.exit_code, None);
        assert!(result.root_scope.lookup("after").is_none());
    }
}

#[test]
fn returned_control_is_consumed_or_handled_by_the_caller() {
    for (tag, payload) in [("exit", "7"), ("panic", "\"broken\"")] {
        let helper = format!("fn helper()\nreturn [:{tag}, {payload}]\nend\n");
        let consumed = run(&format!("{helper}x := helper()\nafter := 42\n"), 1);
        assert!(consumed.ok());
        assert_eq!(value(&consumed, "after"), "42");
        let handled = run(&format!("{helper}helper()\nafter := 42\n"), 1);
        assert!(!handled.ok());
        assert!(handled.root_scope.lookup("after").is_none());
    }
}

#[test]
fn exit_validates_the_code_when_handled_not_when_constructed() {
    for code in ["-1", "256", "1.5", "0 / 0", "1 / 0", "\"7\"", ":null"] {
        assert!(run(&format!("x := exit({code})\n"), 1).ok());
        let result = run(&format!("exit({code})\n"), 1);
        assert!(result.crash.unwrap().message.contains("integer from 0 through 255"));
        assert_eq!(result.exit_code, None);
    }
    assert_eq!(run("exit(255)\n", 1).exit_code, Some(255));
}

#[test]
fn panic_renders_any_message_value() {
    let result = run("panic({ :reason : 42 })\n", 1);
    assert_eq!(result.crash.unwrap().message, "panic: { :reason : 42 }");
    assert!(run("panic()\n", 1).crash.unwrap().message.contains("accepts"));
}

#[test]
fn handlers_require_the_exact_two_element_shape() {
    let result = run("fn f()\nx := [:exit]\nx\n\ny := [:panic, 1, 2]\ny\nend\nf()\nafter := 42\n", 1);
    assert!(result.ok());
    assert_eq!(value(&result, "after"), "42");
}

#[test]
fn termination_never_retries_an_overload() {
    for signal in ["exit(7)", "panic(\"stop\")"] {
        let result = run(&format!("reached := 0\nfn f()\nreached = 1\nend\nfn f()\n{signal}\nend\nf()\n"), 1);
        assert_eq!(value(&result, "reached"), "0");
        assert!(!result.ok());
    }
}

#[test]
fn builtin_names_can_be_shadowed_and_qualified() {
    let result = run("fn exit(code)\nreturn code + 1\nend\nfn panic(msg)\nreturn msg\nend\na := exit(7)\nb := panic(\"ok\")\nc := ::exit(9)\nd := ::panic(\"data\")\n", 1);
    assert!(result.ok());
    assert_eq!(value(&result, "a"), "8");
    assert_eq!(value(&result, "b"), "ok");
    assert_eq!(value(&result, "c"), "[:exit, 9]");
    assert_eq!(value(&result, "d"), "[:panic, data]");
}

#[test]
fn global_termination_stops_in_flight_calls_and_parked_siblings() {
    for threads in [1, 4] {
        for signal in ["exit(7)", "panic(\"stop all\")"] {
            let source = format!("fn busy()\nwhile :true\nend\nend\nparallel\nbusy() || {signal} || _ = receive()\nend\nafter := 1\n");
            let result = run(&source, threads);
            assert!(!result.ok());
            assert!(result.root_scope.lookup("after").is_none());
        }
    }
}

#[test]
fn a_cancelled_trail_cannot_exit_the_live_program() {
    // The second trail wins while the first is inside its helper.
    let source = "fn work()\nx := 1\nexit(7)\nend\nrace\nwork() || winner := 1\nend\nafter := 42\n";
    let result = run(source, 1);
    assert!(result.ok(), "{result:?}");
    assert_eq!(result.exit_code, None);
    assert_eq!(value(&result, "after"), "42");
}

#[test]
fn panic_in_a_cancelled_trail_obeys_crash_isolation() {
    let source = "fn work()\nx := 1\npanic(\"dead panic\")\nend\nrace\nwork() || winner := 1\nend\nafter := 42\n";
    let result = run(source, 1);
    assert!(result.ok(), "{result:?}");
    assert_eq!(result.dead_crashes.len(), 1);
    assert_eq!(result.dead_crashes[0].message, "panic: dead panic");
    assert_eq!(value(&result, "after"), "42");
    let result = run_source(source, "termination.hy", Options { threads: 1, step_budget: 1, strict: true, report_dead_crashes: false, ..Options::default() }).unwrap();
    assert_eq!(result.crash.unwrap().message, "panic: dead panic");
}
