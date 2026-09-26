use hydra::value::to_text;
use hydra::vm::{run_source, Options, RunResult};
use std::process::Command;

fn run(source: &str) -> RunResult {
    run_source(
        source,
        "list-cli-test.hy",
        Options {
            threads: 1,
            step_budget: 1,
            script_args: vec!["--name".into(), "Hydra".into()],
            search_path: vec![],
            ..Options::default()
        },
    )
    .expect("parses")
}
fn read(r: &RunResult, name: &str) -> String {
    assert!(r.ok(), "{:?}", r.crash);
    to_text(
        &r.root_scope
            .lookup(name)
            .unwrap_or_else(|| panic!("missing {name}"))
            .read()
            .unwrap(),
    )
}
fn eval(expr: &str) -> String {
    read(&run(&format!("use list\nx := {expr}\n")), "x")
}

#[test]
fn list_transformations_and_rust_style_chains() {
    for (expr, expected) in [
        (
            "list::range(5).list::map(fn(x) x * 2).list::filter(fn(x) x > 3)",
            "[4, 6, 8]",
        ),
        (
            "list::filter_map([:null, :false, 0, 2], fn(x) x)",
            "[:false, 0, 2]",
        ),
        ("list::flat_map([1, 2], fn(x) [x, x])", "[1, 1, 2, 2]"),
        ("list::flatten([[1], [], [2, 3]])", "[1, 2, 3]"),
        ("list::chain([1], [2,3])", "[1, 2, 3]"),
        ("list::zip([1,2,3], [4,5])", "[[1, 4], [2, 5]]"),
        ("list::enumerate([:a, :b])", "[[0, :a], [1, :b]]"),
        ("list::rev([1,2,3])", "[3, 2, 1]"),
        ("list::take([1,2,3], 2)", "[1, 2]"),
        ("list::skip([1,2,3], 2)", "[3]"),
        ("list::step_by([1,2,3,4,5], 2)", "[1, 3, 5]"),
        ("list::take_while([1,2,3,1], fn(x) x < 3)", "[1, 2]"),
        ("list::skip_while([1,2,3,1], fn(x) x < 3)", "[3, 1]"),
        (
            "list::map_while([1, :false, :null, 4], fn(x) x)",
            "[1, :false]",
        ),
        ("list::range(5, -1, -2)", "[5, 3, 1]"),
        ("list::repeat(:a, 3)", "[:a, :a, :a]"),
    ] {
        assert_eq!(eval(expr), expected, "{expr}");
    }
}

#[test]
fn list_reductions_searches_and_short_circuiting() {
    for (expr, expected) in [
        ("list::fold([1,2,3], 10, fn(a,b) a+b)", "16"),
        ("list::reduce([1,2,3], fn(a,b) a+b)", "6"),
        ("list::reduce([], fn(a,b) a+b)", ":null"),
        ("list::sum([])", "0"),
        ("list::product([])", "1"),
        ("list::sum([1,2,3])", "6"),
        ("list::product([2,3,4])", "24"),
        ("list::min([3,1,2])", "1"),
        ("list::max([3,1,2])", "3"),
        ("list::min([])", ":null"),
        ("list::max([])", ":null"),
        ("list::first([])", ":null"),
        ("list::last([1,2])", "2"),
        ("list::nth([1,2], 2)", ":null"),
        ("list::count([1,2])", "2"),
        ("list::find([1,2,3], fn(x) x > 1)", "2"),
        ("list::find_map([:null,:false,2], fn(x) x)", ":false"),
        ("list::position([1,2,1], fn(x) x==1)", "0"),
        ("list::rposition([1,2,1], fn(x) x==1)", "2"),
        ("list::any([], fn(x) x)", ":false"),
        ("list::all([], fn(x) x)", ":true"),
    ] {
        assert_eq!(eval(expr), expected, "{expr}");
    }
    let r=run("use list\nseen := []\nfn predicate(x)\n _ = push(&seen, x)\n return x > 1\nend\n\nx := list::any([1,2,3], predicate)\n");
    assert_eq!(read(&r, "seen"), "[1, 2]");
    let r=run("use list\na, b := list::partition([1,2,3,4], fn(x) x % 2 == 0)\nc, d := list::unzip([[1,:a],[2,:b]])\n");
    for (n, e) in [
        ("a", "[2, 4]"),
        ("b", "[1, 3]"),
        ("c", "[1, 2]"),
        ("d", "[:a, :b]"),
    ] {
        assert_eq!(read(&r, n), e);
    }
}

#[test]
fn itertools_operations_preserve_order_and_shapes() {
    for (expr, expected) in [
        ("list::chunks([1,2,3,4,5], 2)", "[[1, 2], [3, 4], [5]]"),
        ("list::windows([1,2,3], 2)", "[[1, 2], [2, 3]]"),
        ("list::intersperse([1,2,3], 0)", "[1, 0, 2, 0, 3]"),
        ("list::interleave([1,2,3],[4])", "[1, 4, 2, 3]"),
        (
            "list::cartesian_product([1,2],[:a,:b])",
            "[[1, :a], [1, :b], [2, :a], [2, :b]]",
        ),
        ("list::combinations([1,2,3], 2)", "[[1, 2], [1, 3], [2, 3]]"),
        (
            "list::permutations([1,2,3], 2)",
            "[[1, 2], [1, 3], [2, 1], [2, 3], [3, 1], [3, 2]]",
        ),
        ("list::combinations([], 0)", "[[]]"),
        ("list::permutations([], 0)", "[[]]"),
        ("list::combinations([1], 2)", "[]"),
        ("list::unique([1,2,1,3,2])", "[1, 2, 3]"),
        ("list::dedup([1,1,2,1])", "[1, 2, 1]"),
        ("list::counts([:a,:b,:a])", "[[:a, 2], [:b, 1]]"),
        ("list::unique_by([1,3,2,4], fn(x) x%2)", "[1, 2]"),
        ("list::dedup_by_key([1,3,2,1], fn(x) x%2)", "[1, 2, 1]"),
        ("list::dedup_by([1,2,4,5], fn(a,b) b-a==1)", "[1, 4]"),
        (
            "list::group_by([1,2,3,4], fn(x) x%2)",
            "[[1, [1, 3]], [0, [2, 4]]]",
        ),
        (
            "list::chunk_by([1,3,2,1], fn(x) x%2)",
            "[[1, [1, 3]], [0, [2]], [1, [1]]]",
        ),
        (r#"list::join([1,:ready,"x"], ",")"#, "1,:ready,x"),
    ] {
        assert_eq!(eval(expr), expected, "{expr}");
    }
}

#[test]
fn list_sorts_and_keyed_operations_are_stable() {
    assert_eq!(eval("list::sorted([\"z\",\"a\",\"β\"])"), "[a, z, β]");
    assert_eq!(eval("list::sorted_by([3,1,2], fn(a,b) b-a)"), "[3, 2, 1]");
    let r=run("use list\nitems := [{ :k : 2, :v : :a }, { :k : 1, :v : :b }, { :k : 2, :v : :c }]\nsorted := list::sorted_by_key(items, fn(x) x.k)\nminimum := list::min_by_key(items, fn(x) x.k)\nmaximum := list::max_by_key(items, fn(x) x.k)\nby := list::min_by([3,1,2], fn(a,b) a-b)\n");
    assert_eq!(
        read(&r, "sorted"),
        "[{ :k : 1, :v : :b }, { :k : 2, :v : :a }, { :k : 2, :v : :c }]"
    );
    assert_eq!(read(&r, "minimum"), "{ :k : 1, :v : :b }");
    assert_eq!(read(&r, "maximum"), "{ :k : 2, :v : :c }");
    assert_eq!(read(&r, "by"), "1");
    assert_eq!(eval("list::max_by([3,1,2], fn(a,b) a-b)"), "3");
}

#[test]
fn callbacks_suspend_nest_consume_control_and_keep_value_semantics() {
    let r=run("use list\nuse time\nseen := []\nfn transform(x)\n time::sleep(0.001)\n _ = push(&seen, x)\n return list::map([x], fn(y) y+1)\nend\n\nx := list::map([1,2], transform)\ncontrol := list::map([1,2], fn(x) :break)\n_ = list::for_each([1], fn(x) :return)\ninspected := list::inspect([3,4], fn(x) push(&seen,x))\na := [[1]]\nb := list::repeat(a, 2)\nb[0][0][0] = 9\n");
    assert_eq!(read(&r, "x"), "[[2], [3]]");
    assert_eq!(read(&r, "seen"), "[1, 2, 3, 4]");
    assert_eq!(read(&r, "control"), "[:break, :break]");
    assert_eq!(read(&r, "inspected"), "[3, 4]");
    assert_eq!(read(&r, "a"), "[[1]]");
    assert_eq!(read(&r, "b"), "[[[9]], [[1]]]");
}

#[test]
fn invalid_list_inputs_and_callbacks_crash_cleanly() {
    for expr in [
        "list::map([], 42)",
        "list::map(42, fn(x) x)",
        "list::flat_map([1], fn(x) x)",
        "list::chunks([1], 0)",
        "list::step_by([1], 0)",
        "list::range(0,10,0)",
        "list::range(1000001)",
        "list::take([1],-1)",
        "list::unzip([[1]])",
        "list::sorted([1,\"a\"])",
        "list::map([1], fn(x) 1.missing)",
    ] {
        let r = run(&format!("use list\nx := {expr}"));
        assert!(r.crash.is_some(), "{expr}");
    }
}

const CLI_BASE: &str = r#"use cli
p := cli::parser(prog = "demo", description = "A useful tool", epilog = "Done.")
cli::add_argument(&p, "-v", "--verbose", action = :count)
cli::add_argument(&p, "--enabled", action = :store_true)
cli::add_argument(&p, "--cache", action = :store_false)
cli::add_argument(&p, "-n", "--number", type = :int, default = "2")
cli::add_argument(&p, "--ratio", type = :float, default = 0.5)
cli::add_argument(&p, "--mode", choices = ["fast", "safe"], default = "safe")
cli::add_argument(&p, "-I", action = :append)
cli::add_argument(&p, "files", nargs = "*")
"#;

#[test]
fn cli_options_positionals_actions_and_types() {
    let r=run(&format!("{CLI_BASE}\na := cli::parse_args(p, [\"-vv\", \"--enabled\", \"--cache\", \"-n3\", \"--ratio=-1.5\", \"--mode\", \"fast\", \"-Ia\", \"-I\", \"b\", \"input\", \"--\", \"-file\"])\nx := a.verbose\ny := a.enabled\nz := a.cache\nn := a.number\nf := a.ratio\nm := a.mode\ni := a.I\nfiles := a.files\n"));
    for (n, e) in [
        ("x", "2"),
        ("y", ":true"),
        ("z", ":false"),
        ("n", "3"),
        ("f", "-1.5"),
        ("m", "fast"),
        ("i", "[a, b]"),
        ("files", "[input, -file]"),
    ] {
        assert_eq!(read(&r, n), e);
    }
    let r = run(&format!(
        "{CLI_BASE}\na := cli::parse_args(p, [])\nn := a.number\ni := a.I\nfiles := a.files\n"
    ));
    assert_eq!(read(&r, "n"), "2");
    assert_eq!(read(&r, "i"), "[]");
    assert_eq!(read(&r, "files"), "[]");
}

#[test]
fn cli_nargs_defaults_and_negative_positionals() {
    let r = run(r#"use cli
p := cli::parser()
cli::add_argument(&p, "values", type = :int, nargs = "*")
cli::add_argument(&p, "output")
cli::add_argument(&p, "--pair", type = :int, nargs = 2)
cli::add_argument(&p, "--maybe", nargs = "?", const = "present", default = "absent")
a := cli::parse_args(p, ["--pair", "1", "2", "--maybe", "--", "-1", "-2", "file"])
v := a.values
out := a.output
pair := a.pair
maybe := a.maybe
"#);
    assert_eq!(read(&r, "v"), "[-1, -2]");
    assert_eq!(read(&r, "out"), "file");
    assert_eq!(read(&r, "pair"), "[1, 2]");
    assert_eq!(read(&r, "maybe"), "present");
}

#[test]
fn cli_try_parse_is_non_exiting_and_parse_known_keeps_unknowns() {
    for argv in [
        "[\"--mode\",\"wrong\"]",
        "[\"--number\",\"no\"]",
        "[\"--wat\"]",
        "[\"--number\"]",
    ] {
        let r = run(&format!(
            "{CLI_BASE}\na, reason, message := cli::try_parse_args(p, {argv})\nafter := 42\n"
        ));
        assert_eq!(read(&r, "reason"), ":invalid");
        assert!(read(&r, "message").contains("demo: error:"));
        assert_eq!(read(&r, "after"), "42");
    }
    let r = run(&format!(
        "{CLI_BASE}\na, reason, message := cli::try_parse_args(p, [\"--help\"])\n"
    ));
    assert_eq!(read(&r, "reason"), ":help");
    let help = read(&r, "message");
    for text in [
        "usage: demo",
        "A useful tool",
        "--number",
        "choices: fast, safe",
        "Done.",
    ] {
        assert!(help.contains(text), "{help}");
    }
    let r=run(&format!("{CLI_BASE}\na, extra := cli::parse_known_args(p, [\"--unknown=1\",\"--enabled\"])\nx := a.enabled\n"));
    assert_eq!(read(&r, "extra"), "[--unknown=1]");
    assert_eq!(read(&r, "x"), ":true");
}

#[test]
fn cli_subcommands_and_parser_copies() {
    let r = run(r#"use cli
root := cli::parser(prog = "tool")
child := cli::parser(description = "Build things")
cli::add_argument(&child, "--release", action = :store_true)
cli::add_argument(&child, "target")
cli::add_subparser(&root, "build", child, help = "Build a target")
a := cli::parse_args(root, ["build", "--release", "app"])
command := a.command
release := a.release
target := a.target
b, reason, message := cli::try_parse_args(root, ["build", "--help"])
copy := root
cli::add_argument(&copy, "--extra")
original := cli::format_help(root)
changed := cli::format_help(copy)
"#);
    assert_eq!(read(&r, "command"), "build");
    assert_eq!(read(&r, "release"), ":true");
    assert_eq!(read(&r, "target"), "app");
    assert_eq!(read(&r, "reason"), ":help");
    assert!(read(&r, "message").contains("usage: tool build"));
    assert!(!read(&r, "original").contains("--extra"));
    assert!(read(&r, "changed").contains("--extra"));
}

#[test]
fn cli_defaults_to_host_arguments_and_rejects_bad_schemas() {
    let r=run("use cli\np := cli::parser()\ncli::add_argument(&p, \"--name\", required = :true)\na := cli::parse_args(p)\nname := a.name\n");
    assert_eq!(read(&r, "name"), "Hydra");
    for line in [
        "cli::add_argument(&p, \"--help\")",
        "cli::add_argument(&p, \"--x\", type = :unknown)",
        "cli::add_argument(&p, \"--x\", nargs = 0)",
        "cli::add_argument(&p, \"x\", \"y\")",
    ] {
        assert!(
            run(&format!("use cli\np := cli::parser()\n{line}"))
                .crash
                .is_some(),
            "{line}"
        );
    }
}

#[test]
fn cli_help_and_errors_exit_from_real_scripts() {
    let path = std::env::temp_dir().join(format!("hydra-cli-exit-{}.hy", std::process::id()));
    std::fs::write(&path,"use cli\np := cli::parser(prog = \"demo\")\ncli::add_argument(&p, \"--number\", type = :int, required = :true)\na := cli::parse_args(p)\nprint a.number\n").unwrap();
    for (args, code, needle, stderr) in [
        (vec!["--help"], 0, "usage: demo", false),
        (vec![], 2, "required", true),
        (vec!["--number", "bad"], 2, "invalid int", true),
        (vec!["--number", "7"], 0, "7\n", false),
    ] {
        let out = Command::new(env!("CARGO_BIN_EXE_hydra"))
            .arg("run")
            .arg(&path)
            .arg("--")
            .args(args)
            .output()
            .unwrap();
        assert_eq!(out.status.code(), Some(code));
        let text = String::from_utf8(if stderr { out.stderr } else { out.stdout }).unwrap();
        assert!(text.contains(needle), "{text}");
    }
    std::fs::remove_file(path).unwrap();
}

#[test]
fn cli_constants_versions_and_child_errors() {
    let r = run(r#"use cli
p := cli::parser(prog = "tool")
cli::add_argument(&p, "--fast", action = :store_const, const = :fast)
cli::add_argument(&p, "--tag", action = :append_const, const = :tag)
cli::add_argument(&p, "--version", action = :version, version = "tool 1.0")
a := cli::parse_args(p, ["--fast", "--tag", "--tag"])
fast := a.fast
tags := a.tag
_, reason, version := cli::try_parse_args(p, ["--version"])
child := cli::parser()
cli::add_argument(&child, "target")
cli::add_subparser(&p, "build", child, required = :false)
empty := cli::parse_args(p, [])
command := empty.command
_, child_reason, message := cli::try_parse_args(p, ["build"])
"#);
    assert_eq!(read(&r, "fast"), ":fast");
    assert_eq!(read(&r, "tags"), "[:tag, :tag]");
    assert_eq!(read(&r, "reason"), ":help");
    assert_eq!(read(&r, "version"), "tool 1.0\n");
    assert_eq!(read(&r, "command"), ":null");
    assert_eq!(read(&r, "child_reason"), ":invalid");
    assert!(read(&r, "message").contains("tool build: error:"));
}

#[test]
fn list_callbacks_can_communicate_with_sibling_trails() {
    // With one worker, a callback must suspend the task rather than block the VM.
    let r = run(r#"use list
values := []
parallel
    values = list::map([1, 2], fn(x) receive()) || _ = send(10)
                                               || _ = send(20)
end
"#);
    assert_eq!(read(&r, "values"), "[10, 20]");
}

#[test]
fn documented_examples_execute() {
    for (file, args, expected) in [
        ("examples/list_pipeline.hy", vec![], "[16, 25, 36, 49]"),
        (
            "examples/cli_greet.hy",
            vec!["Hydra", "-n", "2", "-v"],
            "Printed 2 greetings",
        ),
    ] {
        let out = Command::new(env!("CARGO_BIN_EXE_hydra"))
            .arg("run")
            .arg(file)
            .arg("--")
            .args(args)
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        assert!(String::from_utf8_lossy(&out.stdout).contains(expected));
    }
}

#[test]
fn cli_optional_constants_are_typed_and_attached_values_preserved() {
    let r = run(r#"use cli
p := cli::parser()
cli::add_argument(&p, "--level", nargs = "?", type = :int, const = "3")
cli::add_argument(&p, "-s")
a := cli::parse_args(p, ["--level", "-s==value"])
level := a.level
value := a.s
"#);
    assert_eq!(read(&r, "level"), "3");
    assert_eq!(read(&r, "value"), "=value");
}
