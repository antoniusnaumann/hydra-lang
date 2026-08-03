//! Interpreter tests (spec §5–§8).
//!
//! There is no standard library, so a program cannot print anything (see
//! QUESTIONS.md §1). Tests therefore run a program and inspect the module
//! scope it leaves behind.

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

/// Run a program and read a toplevel binding back as text.
fn eval(src: &str, name: &str) -> String {
    let result = run(src);
    if let Some(crash) = &result.crash {
        panic!("unexpected crash: {crash}");
    }
    let cell = result.root_scope.lookup(name).unwrap_or_else(|| panic!("no binding `{name}`"));
    let value = cell.read().unwrap().clone();
    to_text(&value)
}

/// Evaluate a single expression by binding it.
fn expr(src: &str) -> String {
    eval(&format!("x := {src}\n"), "x")
}

fn crash_of(src: &str) -> String {
    let result = run(src);
    result.crash.map(|c| c.message).unwrap_or_else(|| "<no crash>".into())
}

// --- values -----------------------------------------------------------------

#[test]
fn numbers_are_doubles() {
    assert_eq!(expr("1 + 2"), "3");
    assert_eq!(expr("3.0"), "3");
    assert_eq!(expr("7 / 2"), "3.5");
    assert_eq!(expr("7 % 3"), "1");
    assert_eq!(expr("-7 % 3"), "-1"); // sign of the dividend, like C and JS
    assert_eq!(expr("1 / 0"), "Infinity");
    assert_eq!(expr("0 / 0"), "NaN");
}

#[test]
fn bitwise_is_32_bit_javascript() {
    // §2: operands go through ToInt32, the operation runs on 32-bit integers,
    // and the result widens back to a double.
    assert_eq!(expr("6 & 3"), "2");
    assert_eq!(expr("6 | 3"), "7");
    assert_eq!(expr("6 ^ 3"), "5");
    assert_eq!(expr("~0"), "-1");
    assert_eq!(expr("1 << 31"), "-2147483648"); // 2^31 becomes -2^31
    assert_eq!(expr("-1 >> 1"), "-1");
    assert_eq!(expr("-1 >>> 1"), "2147483647");
    // The unsigned shift is the one operator that can exceed 2^31 - 1.
    assert_eq!(expr("-1 >>> 0"), "4294967295");
    // NaN and the infinities convert to 0.
    assert_eq!(expr("(0 / 0) | 0"), "0");
    assert_eq!(expr("(1 / 0) | 0"), "0");
    assert_eq!(expr("(0 - 1 / 0) | 0"), "0");
    // Shift counts are taken modulo 32.
    assert_eq!(expr("1 << 33"), "2");
    // Truncation is toward zero.
    assert_eq!(expr("(0 - 3.7) | 0"), "-3");
    assert_eq!(expr("3.7 | 0"), "3");
}

#[test]
fn strings_concatenate_with_plus_but_never_convert() {
    assert_eq!(expr("\"hi \" + \"there\""), "hi there");
    // `+` joins two strings or adds two numbers. A string and a non-string is
    // a bad operand, not a silent conversion: interpolation is how a value is
    // rendered (QUESTIONS.md §3).
    assert!(crash_of("x := \"n = \" + 3\n").contains("interpolate"));
    assert!(crash_of("x := 3 + \" apples\"\n").contains("interpolate"));
    assert!(crash_of("x := \"tag \" + .ok\n").contains("interpolate"));
}

#[test]
fn interpolation_renders_any_expression() {
    assert_eq!(eval("name := \"world\"\nx := \"hi \\(name)\"\n", "x"), "hi world");
    assert_eq!(expr("\"\\(1 + 2) apples\""), "3 apples");
    assert_eq!(expr("\"tag \\(.ok)\""), "tag .ok");
    assert_eq!(expr("\"\\([1, 2])\""), "[1, 2]");
    assert_eq!(expr("\"\\({ .a : 1 })\""), "{ .a : 1 }");
    // Nested: an interpolation may contain a string with its own.
    assert_eq!(eval("n := 2\nx := \"a \\(\"b \\(n)\")\"\n", "x"), "a b 2");
    // §14's line, now that it works again.
    assert_eq!(eval("down := 1\nx := \"\\(3 - down)/3 regions live\"\n", "x"), "2/3 regions live");
}

#[test]
fn a_symbol_can_be_built_by_interpolation() {
    // §2: dynamic symbol construction without a separate `sym(str)` builtin.
    let src = "
prefix := \"x\"
key := .\"\\(prefix)-id\"
d := { .\"\\(prefix)-id\" : 7 }
seen := d[key]
same := key === .\"x-id\"
";
    assert_eq!(eval(src, "seen"), "7");
    assert_eq!(eval(src, "same"), ".true");
    // A name may contain `-`, so the built symbol renders bare (§2).
    assert_eq!(eval(src, "key"), ".x-id");
}

#[test]
fn truthiness_is_only_about_null_and_false() {
    // Everything else is truthy, including 0, "" and [] (§5).
    for (src, expected) in [
        ("0", "yes"),
        ("\"\"", "yes"),
        ("[]", "yes"),
        (".true", "yes"),
        (".whatever", "yes"),
        (".null", "no"),
        (".false", "no"),
    ] {
        let program = format!("x := \"no\"\nif {src}\n\tx = \"yes\"\nend\n");
        assert_eq!(eval(&program, "x"), expected, "for {src}");
    }
}

#[test]
fn dicts_are_symbol_keyed_and_dot_is_sugar() {
    assert_eq!(expr("{ .a : 5 }.a"), "5");
    assert_eq!(expr("{ .a : 5 }[.a]"), "5");
    assert_eq!(expr("{ .\"x-req-id\" : 17 }.\"x-req-id\""), "17");
    assert_eq!(eval("k := .a\nd := { .a : 5 }\nx := d[k]\n", "x"), "5");
}

#[test]
fn reading_a_missing_key_crashes_but_writing_creates() {
    assert!(crash_of("d := { .a : 1 }\nx := d.b\n").contains("no key .b"));
    assert_eq!(eval("d := { .a : 1 }\nd.b = 2\nx := d.b\n", "x"), "2");
    assert_eq!(eval("d := {}\nd.k = 1\nx := d\n", "x"), "{ .k : 1 }");
}

#[test]
fn list_indexing() {
    // QUESTIONS.md §2: 0-based, whole numbers, out of range crashes.
    assert_eq!(expr("[10, 20, 30][0]"), "10");
    assert_eq!(expr("[10, 20, 30][2]"), "30");
    // A negative index counts from the end (§15.2, ruled on by the owner).
    assert_eq!(expr("[10, 20, 30][0 - 1]"), "30");
    assert_eq!(expr("[10, 20, 30][0 - 3]"), "10");
    assert_eq!(eval("a := [1, 2]\na[0 - 1] = 9\nx := a[1]\n", "x"), "9");
    assert!(crash_of("x := [1, 2][2]\n").contains("out of range"));
    assert!(crash_of("x := [1, 2][0 - 3]\n").contains("out of range"));
    assert!(crash_of("x := [1, 2][0.5]\n").contains("whole number"));
}

// --- value semantics (§5.1) -------------------------------------------------

#[test]
fn assignment_copies() {
    let src = "a := { .x : 1 }\nb := a\nb.x = 2\nsame := a.x\n";
    assert_eq!(eval(src, "same"), "1");
    let src = "a := [1, 2]\nb := a\nb[0] = 9\nsame := a[0]\n";
    assert_eq!(eval(src, "same"), "1");
}

#[test]
fn a_callee_cannot_touch_the_callers_value() {
    let src = "
fn f(d)
\td.x = 99
end
a := { .x : 1 }
f(a)
kept := a.x
";
    assert_eq!(eval(src, "kept"), "1");
}

#[test]
fn insertion_copies() {
    let src = "a := { .x : 1 }\nd := { .held : a }\na.x = 2\nheld := d.held.x\n";
    assert_eq!(eval(src, "held"), "1");
}

#[test]
fn identity_is_the_cow_buffer() {
    // §5.1, exactly the example in the spec.
    let src = "
a := { .x : 0 }
b := a
before := b === a
b.x = 1
after := b === a
c := &a
aliased := c === a
";
    assert_eq!(eval(src, "before"), ".true");
    assert_eq!(eval(src, "after"), ".false");
    assert_eq!(eval(src, "aliased"), ".true");
}

#[test]
fn identity_on_values_without_identity_is_value_equality() {
    assert_eq!(expr("\"a\" === \"a\""), ".true");
    assert_eq!(expr("1 === 1"), ".true");
    assert_eq!(expr(".ok === .ok"), ".true");
    assert_eq!(expr("(0 / 0) === (0 / 0)"), ".false"); // NaN is never equal
    assert_eq!(expr("(0 / 0) == (0 / 0)"), ".false");
    assert_eq!(expr("[1] === [1]"), ".false");
    assert_eq!(expr("[1] == [1]"), ".true");
}

#[test]
fn deep_equality_walks_structures() {
    assert_eq!(expr("{ .a : [1, 2] } == { .a : [1, 2] }"), ".true");
    assert_eq!(expr("{ .a : [1, 2] } == { .a : [1, 3] }"), ".false");
    assert_eq!(expr("{ .a : 1, .b : 2 } == { .b : 2, .a : 1 }"), ".true");
    assert_eq!(expr("[1, 2] != [1, 2, 3]"), ".true");
}

#[test]
fn a_structure_containing_nan_equals_itself() {
    // The documented consequence of trying identity first (§5).
    let src = "a := [0 / 0]\nself_equal := a == a\ncopy := a\ncopy_equal := copy == a\n";
    assert_eq!(eval(src, "self_equal"), ".true");
    assert_eq!(eval(src, "copy_equal"), ".true");
}

#[test]
fn cyclic_structures_compare_without_looping() {
    let src = "
a := { .next : .null }
a.next = &a
b := { .next : .null }
b.next = &b
same := a == b
";
    assert_eq!(eval(src, "same"), ".true");
}

// --- references (§5.1) ------------------------------------------------------

#[test]
fn a_reference_opts_out_of_copying() {
    let src = "
fn bump(d)
\td.x = d.x + 1
end
a := { .x : 1 }
bump(&a)
after := a.x
";
    assert_eq!(eval(src, "after"), "2");
}

#[test]
fn a_reference_writes_through_for_plain_values() {
    // QUESTIONS.md §6: `x = 5` where x holds a reference writes to the target.
    let src = "
fn set(x)
\tx = 5
end
a := 1
set(&a)
after := a
";
    assert_eq!(eval(src, "after"), "5");
}

#[test]
fn references_into_containers_and_out_of_them() {
    let src = "d := { .x : 1 }\nr := &d.x\nr = 7\nafter := d.x\n";
    assert_eq!(eval(src, "after"), "7");

    let src = "a := 1\nheld := { .r : &a }\na = 2\nseen := held.r\n";
    assert_eq!(eval(src, "seen"), "2");

    let src = "a := 1\nlist := [&a]\na = 3\nseen := list[0]\n";
    assert_eq!(eval(src, "seen"), "3");
}

#[test]
fn reading_a_reference_derefs_then_copies() {
    // You must write `&` again to keep aliasing (QUESTIONS.md §6).
    let src = "a := { .x : 1 }\nr := &a\nb := r\nb.x = 9\nkept := a.x\n";
    assert_eq!(eval(src, "kept"), "1");
}

// --- scope and binding (§6) -------------------------------------------------

#[test]
fn declaration_shadows_and_assignment_searches_outward() {
    let src = "
x := 1
if .true
\tx = 2
end
outer := x
";
    assert_eq!(eval(src, "outer"), "2");

    let src = "
x := 1
if .true
\tx := 5
end
outer := x
";
    assert_eq!(eval(src, "outer"), "1");
}

#[test]
fn assigning_to_an_undeclared_name_crashes() {
    assert!(crash_of("x = 1\n").contains("not declared"));
    // A compound assignment is an assignment: it reads the binding it writes,
    // so there is even less for it to do without one.
    assert!(crash_of("x += 1\n").contains("not declared"));
}

#[test]
fn a_compound_assignment_applies_its_operator() {
    let src = "
n := 10
n += 5
n -= 3
n *= 4
n /= 6
n %= 5
";
    assert_eq!(eval(src, "n"), "3");

    // Every one of them is the binary operator of §3's table, with §2's
    // semantics unchanged — `>>>` still widens back through an unsigned shift.
    assert_eq!(eval("b := 1\nb <<= 4\n", "b"), "16");
    assert_eq!(eval("b := 16\nb >>= 2\n", "b"), "4");
    assert_eq!(eval("b := 0 - 8\nb >>>= 28\n", "b"), "15");
    assert_eq!(eval("b := 4\nb |= 3\n", "b"), "7");
    assert_eq!(eval("b := 6\nb &= 3\n", "b"), "2");
    assert_eq!(eval("b := 6\nb ^= 3\n", "b"), "5");
    // `+` joins two strings, so `+=` does too (QUESTIONS.md §4).
    assert_eq!(eval("s := \"ab\"\ns += \"cd\"\n", "s"), "abcd");
    assert!(crash_of("s := \"n = \"\ns += 3\n").contains("interpolate"));
}

#[test]
fn a_compound_assignment_reaches_keys_and_elements() {
    assert_eq!(eval("d := { .n : 1 }\nd.n += 41\n", "d"), "{ .n : 42 }");
    assert_eq!(eval("l := [1, 2, 3]\nl[0] += 100\nl[-1] *= 2\n", "l"), "[101, 2, 6]");
    assert_eq!(eval("d := { .l : [1] }\nd.l[0] -= 1\n", "d"), "{ .l : [0] }");

    // Writing a missing key creates it (§5), but this one reads it first, and
    // reading a missing key crashes — the same crash `d.b = d.b + 1` raises.
    assert!(crash_of("d := { .a : 1 }\nd.b += 1\n").contains("no key .b"));
    assert!(crash_of("l := [1]\nl[3] += 1\n").contains("out of range"));
}

#[test]
fn a_compound_assignment_names_its_place_once() {
    // `a[next()] += 1` is `a[i] = a[i] + 1`, not the text of it: the index is
    // evaluated once, so `next` is called once and the update lands in the
    // element it chose.
    let src = "
calls := 0
fn next()
\tcalls = calls + 1
\treturn 0
end
a := [10, 20]
a[next()] += 5
";
    assert_eq!(eval(src, "calls"), "1");
    assert_eq!(eval(src, "a"), "[15, 20]");
}

#[test]
fn a_compound_assignment_writes_through_a_reference() {
    // A `&` in the binding is written *through*, exactly as `=` is
    // (QUESTIONS.md §6), and the copy a plain read makes is not updated.
    let src = "
a := 1
fn bump(x)
\tx += 41
end
bump(&a)
";
    assert_eq!(eval(src, "a"), "42");

    let src = "
a := 1
fn bump(x)
\tx += 41
end
bump(a)
";
    assert_eq!(eval(src, "a"), "1");
}

#[test]
fn a_compound_assignment_splits_a_shared_container_first() {
    // Copy-on-write is invisible to it: `b` was a copy, so updating `b` leaves
    // `a` alone (§5.1).
    let src = "
a := [1, 2]
b := a
b[0] += 100
";
    assert_eq!(eval(src, "a"), "[1, 2]");
    assert_eq!(eval(src, "b"), "[101, 2]");
}

#[test]
fn closures_capture_the_scope_chain() {
    let src = "
counter := 0
fn bump()
\tcounter = counter + 1
end
bump()
bump()
total := counter
";
    assert_eq!(eval(src, "total"), "2");
}

#[test]
fn shadowing_leaves_an_old_closure_with_the_old_binding() {
    // `:=` on an existing name is a fresh binding, so closures that captured
    // the old one keep the old one (§6).
    let src = "
x := 1
fn peek()
\treturn x
end
x := 2
old := peek()
new := x
";
    assert_eq!(eval(src, "old"), "1");
    assert_eq!(eval(src, "new"), "2");
}

#[test]
fn functions_and_closures() {
    let src = "
fn add(a, b)
\treturn a + b
end
single := fn(a, b) a * b
multi := fn(c)
\tx := c * c
\treturn x - c
end
p := add(2, 3)
q := single(2, 3)
r := multi(4)
";
    assert_eq!(eval(src, "p"), "5");
    assert_eq!(eval(src, "q"), "6");
    assert_eq!(eval(src, "r"), "12");
}

#[test]
fn a_call_no_candidate_accepts_crashes() {
    let crash = crash_of("fn f(a)\nend\nf(1, 2)\n");
    assert!(crash.contains("no `f` accepts"), "{crash}");
    assert!(crash.contains("f(a)"), "the crash names what it tried: {crash}");
}

#[test]
fn a_function_without_return_yields_null() {
    assert_eq!(eval("fn f()\nend\nx := f()\n", "x"), ".null");
}

// --- control flow -----------------------------------------------------------

#[test]
fn loops_and_labels() {
    let src = "
total := 0
for n in [1, 2, 3, 4]
\tif n == 3
\t\tcontinue
\tend
\tif n == 4
\t\tbreak
\tend
\ttotal = total + n
end
";
    assert_eq!(eval(src, "total"), "3");

    let src = "
hits := 0
for a in [1, 2] as outer
\tfor b in [1, 2]
\t\thits = hits + 1
\t\tbreak outer
\tend
end
";
    assert_eq!(eval(src, "hits"), "1");
}

#[test]
fn while_loops() {
    let src = "
n := 0
while n < 5
\tn = n + 1
end
";
    assert_eq!(eval(src, "n"), "5");
}

#[test]
fn else_if_chain_runs_one_branch() {
    let src = "
x := 0
if .false
\tx = 1
else if .true
\tx = 2
else
\tx = 3
end
";
    assert_eq!(eval(src, "x"), "2");
}

#[test]
fn and_or_short_circuit_and_keep_the_operand() {
    assert_eq!(expr(".null or \"fallback\""), "fallback");
    assert_eq!(expr("\"kept\" or \"other\""), "kept");
    assert_eq!(expr(".false and \"unreached\""), ".false");
    assert_eq!(expr("\"a\" and \"b\""), "b");
    assert_eq!(expr("not .null"), ".true");
    assert_eq!(expr("not 0"), ".false");
}

#[test]
fn alive_is_true_outside_any_trail() {
    assert_eq!(eval("x := alive()\n", "x"), ".true");
}

#[test]
fn a_crash_stops_the_program() {
    let result = run("a := 1\nb := missing_name\nc := 2\n");
    assert!(result.crash.is_some());
    assert!(result.root_scope.lookup("c").is_none());
}
