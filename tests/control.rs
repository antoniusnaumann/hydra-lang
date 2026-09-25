//! Loop handlers receive only unconsumed values in their own function.
use hydra::value::to_text;
use hydra::vm::{run_source, Options, RunResult};

fn run(src: &str, threads: usize) -> RunResult {
    run_source(src, "control.hy", Options { threads, step_budget: 1, ..Options::default() }).unwrap()
}
fn value(result: &RunResult, name: &str) -> String {
    assert!(result.crash.is_none(), "{:?}", result.crash);
    to_text(&result.root_scope.lookup(name).unwrap().read().unwrap())
}

#[test]
fn helpers_explicitly_return_control_to_the_receiving_loop() {
    let result = run(r#"
fn break_if(condition)
    if condition
        return :break
    end
end
fn continue_if(condition)
    if condition
        return continue()
    end
end
sum := 0
for n in [1, 2, 3, 4, 5]
    continue_if(n == 2)
    break_if(n == 4)
    sum += n
end
"#, 1);
    assert_eq!(value(&result, "sum"), "4");
}

#[test]
fn consuming_or_forwarding_control_is_ordinary_value_flow() {
    let result = run(r#"
fn signal()
    return :break
end
fn relay()
    return signal()
end
x := relay()
y := continue()
items := [break(), continue()]
hits := 0
for n in [1, 2]
    held := signal()
    _ = held
    hits += 1
    relay()
    hits += 100
end
"#, 1);
    assert_eq!(value(&result, "x"), ":break");
    assert_eq!(value(&result, "y"), ":continue");
    assert_eq!(value(&result, "items"), "[:break, :continue]");
    assert_eq!(value(&result, "hits"), "1");
}

#[test]
fn unconsumed_control_never_implicitly_returns_from_a_function() {
    for body in [":break", ":continue", "break()", "continue()"] {
        let source = format!("fn helper()\n{body}\nend\nfor n in [1]\nhelper()\nend\n");
        let result = run(&source, 1);
        let message = result.crash.unwrap().message;
        assert!(message.contains("no enclosing loop in this function"), "{message}");
    }
}

#[test]
fn nested_loop_handlers_unwind_scopes_and_iterators() {
    let result = run(r#"
hits := 0
for outer in [1, 2, 3]
    for inner in [1, 2, 3]
        if inner == 1
            continue()
        end
        if inner == 2
            hits += 1
            break()
        end
    end
    hits += 10
end
n := 0
while n < 5
    n += 1
    if n < 3
        :continue
    end
    :break
end
"#, 1);
    assert_eq!(value(&result, "hits"), "33");
    assert_eq!(value(&result, "n"), "3");
}

#[test]
fn a_helper_handles_its_own_loop_before_returning() {
    let result = run(r#"
fn helper()
    for x in [1, 2]
        break()
    end
    return 7
end
sum := 0
for n in [1, 2]
    sum += helper()
end
"#, 1);
    assert_eq!(value(&result, "sum"), "14");
}

#[test]
fn explicit_return_bypasses_a_local_loop() {
    let result = run("fn helper()\nwhile :true\nreturn :break\nend\nend\nx := helper()\n", 1);
    assert_eq!(value(&result, "x"), ":break");
}

#[test]
fn control_builtins_are_shadowable_and_have_no_arguments() {
    let result = run("fn break()\nreturn 42\nend\nx := break()\ny := ::break()\nz := ::continue()\n", 1);
    assert_eq!(value(&result, "x"), "42");
    assert_eq!(value(&result, "y"), ":break");
    assert_eq!(value(&result, "z"), ":continue");
    assert!(run("break(1)\n", 1).crash.unwrap().message.contains("accepts"));
}

#[test]
fn parallel_continue_only_ends_its_iteration() {
    for threads in [1, 4] {
        let result = run("hits := 0\nparallel for n in [1, 2, 3, 4]\nif n == 2\ncontinue()\nend\nhits += n\nend\n", threads);
        assert_eq!(value(&result, "hits"), "8");
    }
}

#[test]
fn parallel_break_stops_spawning_and_cancels_siblings() {
    for threads in [1, 4] {
        let result = run(r#"
fn stop()
    return :break
end
hits := 0
parallel while :true
    stop()
    hits += 1
end
after := 42
"#, threads);
        assert_eq!(value(&result, "hits"), "0");
        assert_eq!(value(&result, "after"), "42");
    }
}

#[test]
fn parallel_break_wakes_siblings_waiting_on_channels() {
    for threads in [1, 4] {
        let result = run(r#"
parallel for n in [0, 1, 2, 3]
    if n == 0
        break()
    end
    _ = receive()
end
after := :done
"#, threads);
        assert_eq!(value(&result, "after"), ":done");
    }
}

#[test]
fn local_loop_control_does_not_end_a_parallel_iteration() {
    let result = run("hits := 0\nparallel for n in [1, 2, 3]\nwhile :true\nbreak()\nend\nhits += n\nend\n", 4);
    assert_eq!(value(&result, "hits"), "6");
}
