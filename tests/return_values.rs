//! Return requests are data until a statement leaves them unconsumed.
use hydra::value::to_text;
use hydra::vm::{run_source, Options, RunResult};

fn run(src: &str) -> RunResult {
    run_source(src, "return.hy", Options { threads: 1, ..Options::default() }).expect("parses and compiles")
}
fn value(result: &RunResult, name: &str) -> String {
    assert!(result.ok(), "{:?}", result.crash);
    to_text(&result.root_scope.lookup(name).unwrap().read().unwrap())
}

#[test]
fn return_constructs_data_and_supports_zero_or_multiple_values() {
    let result = run("a := return(1, 2)\nb := return()\nc := return\nd := c 3\nfn empty()\nreturn\npanic \"unreachable\"\nend\nfn pair()\nreturn 1, 2\nend\nx := empty()\ny, z := pair()\n");
    assert_eq!(value(&result, "a"), "[:return, 1, 2]");
    assert_eq!(value(&result, "b"), "[:return]");
    assert_eq!(value(&result, "d"), "[:return, 3]");
    assert_eq!(value(&result, "x"), ":null");
    assert_eq!(value(&result, "y"), "1");
    assert_eq!(value(&result, "z"), "2");
}

#[test]
fn helpers_forward_return_requests_for_the_callers_function() {
    let result = run("fn return_if(condition, value)\nif condition\nreturn return(value)\nend\nend\nfn choose()\nreturn_if :true, 42\nreturn 0\nend\nx := choose()\nfn keep()\nrequest := return(7)\n_ = request\nreturn 9\nend\ny := keep()\n");
    assert_eq!(value(&result, "x"), "42");
    assert_eq!(value(&result, "y"), "9");
}

#[test]
fn direct_return_values_unwind_nested_loops_and_empty_atoms_return_null() {
    let result = run("fn f()\nfor n in [1, 2]\nwhile :true\n\n[:return, n, 8]\nend\nend\nend\na, b := f()\nfn empty()\n:return\nend\nx := empty()\n");
    assert_eq!(value(&result, "a"), "1");
    assert_eq!(value(&result, "b"), "8");
    assert_eq!(value(&result, "x"), ":null");
}

#[test]
fn return_is_shadowable_and_the_builtin_namespace_stays_available() {
    let result = run("fn return(x)\n::return(x + 1)\nend\nx := return 4\ny := ::return 6\n");
    assert_eq!(value(&result, "x"), "5");
    assert_eq!(value(&result, "y"), "[:return, 6]");
}

#[test]
fn return_requests_are_unhandled_outside_a_function_and_inside_trails() {
    for source in ["return 1", ":return", "[:return, 1]", "parallel\nreturn 1 ||\nend", "fn f()\nparallel\n:return ||\nend\nend\nf()"] {
        assert!(run(source).crash.unwrap().message.contains("unhandled :return"), "{source}");
    }
    let result = run("fn f()\nreturn 8\nend\nx := 0\nparallel\nx = f() || _ = return(9)\nend\n");
    assert_eq!(value(&result, "x"), "8");
}

#[test]
fn returning_a_stored_request_copies_its_payload_before_mutation() {
    let result = run("request := return([1])\nfn f()\nrequest\nend\nx := f()\nx[0] = 9\noriginal := request[1][0]\n");
    assert_eq!(value(&result, "original"), "1");
    assert_eq!(value(&result, "x"), "[9]");
}
