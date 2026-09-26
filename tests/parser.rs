//! Parser tests (spec §3) and parallel-block transposition tests (spec §4).

use hydra::ast::*;
use hydra::parser::parse;

/// An s-expression view of an expression, so precedence is easy to assert on.
fn sexpr(e: &Expr) -> String {
    match e {
        Expr::Num { raw, .. } => raw.clone(),
        Expr::Str { parts, .. } => {
            if let [StrPart::Text(text)] = &parts[..] {
                format!("{text:?}")
            } else {
                let pieces: Vec<String> = parts
                    .iter()
                    .map(|p| match p {
                        StrPart::Text(text) => format!("{text:?}"),
                        StrPart::Expr(expr) => sexpr(expr),
                    })
                    .collect();
                format!("(str {})", pieces.join(" "))
            }
        }
        Expr::Sym(s) if !s.is_static() => {
            let pieces: Vec<String> = s
                .parts
                .iter()
                .map(|p| match p {
                    StrPart::Text(text) => format!("{text:?}"),
                    StrPart::Expr(expr) => sexpr(expr),
                })
                .collect();
            format!("(sym {})", pieces.join(" "))
        }
        Expr::Sym(s) => format!(":{}", s.name),
        Expr::List { items, .. } => {
            format!("(list {})", items.iter().map(sexpr).collect::<Vec<_>>().join(" "))
        }
        Expr::Dict { entries, .. } => format!(
            "(dict {})",
            entries.iter().map(|(k, v)| format!(":{} {}", k.name, sexpr(v))).collect::<Vec<_>>().join(" ")
        ),
        Expr::Name { name, .. } => name.clone(),
        Expr::Namespace { module, name, .. } => format!("{module}::{name}"),
        Expr::Key { obj, key, .. } => format!("(key {} :{})", sexpr(obj), key.name),
        Expr::Method { obj, module: None, name, .. } => format!("(dot {} {name})", sexpr(obj)),
        Expr::Method { obj, module: Some(module), name, .. } => {
            format!("(dot {} {module}::{name})", sexpr(obj))
        }
        Expr::Index { obj, index, .. } => format!("(index {} {})", sexpr(obj), sexpr(index)),
        Expr::Call { callee, args, .. } => {
            let args: Vec<String> = args
                .iter()
                .map(|a| match &a.name {
                    Some(name) => format!("{name}={}", sexpr(&a.value)),
                    None => sexpr(&a.value),
                })
                .collect();
            format!("(call {}{}{})", sexpr(callee), if args.is_empty() { "" } else { " " }, args.join(" "))
        }
        Expr::Unary { op, operand, .. } => format!("({op} {})", sexpr(operand)),
        Expr::Ref { target, .. } => format!("(& {})", sexpr(target)),
        Expr::Binary { op, left, right, .. } => {
            format!("({op} {} {})", sexpr(left), sexpr(right))
        }
        Expr::Closure(def) => {
            let params: Vec<String> = def
                .params
                .iter()
                .map(|p| {
                    let mark = if p.by_ref { "&" } else { "" };
                    match &p.default {
                        Some(d) => format!("{mark}{}:={}", p.name, sexpr(d)),
                        None => format!("{mark}{}", p.name),
                    }
                })
                .collect();
            match &def.body {
                ClosureBody::Expr(x) => format!("(fn ({}) {})", params.join(" "), sexpr(x)),
                ClosureBody::Block(b) => format!("(fn ({}) block[{}])", params.join(" "), b.len()),
            }
        }
    }
}

fn first_expr(src: &str) -> String {
    let program = parse(src, "t.hy").expect("parses");
    match &program.body[0] {
        Stmt::Expr { expr, .. } => sexpr(expr),
        Stmt::Decl { value, .. } => sexpr(value),
        Stmt::Assign { value, .. } => sexpr(value),
        other => panic!("expected an expression statement, got {other:?}"),
    }
}

#[test]
fn precedence_is_pythons_not_cs() {
    // The point of the ordering: `flags | MASK == x` groups as
    // `(flags | MASK) == x` (§3).
    assert_eq!(first_expr("flags | MASK == x\n"), "(== (| flags MASK) x)");
    assert_eq!(first_expr("a or b and c\n"), "(or a (and b c))");
    assert_eq!(first_expr("not a == b\n"), "(not (== a b))");
    assert_eq!(first_expr("a + b * c\n"), "(+ a (* b c))");
    assert_eq!(first_expr("a * b + c\n"), "(+ (* a b) c)");
    assert_eq!(first_expr("a << b + c\n"), "(<< a (+ b c))");
    assert_eq!(first_expr("a & b ^ c | d\n"), "(| (^ (& a b) c) d)");
    assert_eq!(first_expr("-a * b\n"), "(* (- a) b)");
    assert_eq!(first_expr("~a + b\n"), "(+ (~ a) b)");
    assert_eq!(first_expr("a - b - c\n"), "(- (- a b) c)");
}

#[test]
fn postfix_binds_tightest() {
    assert_eq!(first_expr("-a.b\n"), "(- (key a :b))");
    assert_eq!(first_expr("f(x).y[0]\n"), "(index (key (call f x) :y) 0)");
    assert_eq!(first_expr("json::decode(body)\n"), "(call json::decode body)");
    assert_eq!(first_expr("m::n.k\n"), "(key m::n :k)");
}

#[test]
fn key_lookup_and_quoted_keys() {
    // `d.a` is sugar for `d[:a]`, and `headers."content-type"` for
    // `headers[:"content-type"]` (§2, §5).
    assert_eq!(first_expr("d.a\n"), "(key d :a)");
    assert_eq!(first_expr("headers.\"content-type\"\n"), "(key headers :content-type)");
    assert_eq!(first_expr("d[k]\n"), "(index d k)");
    assert_eq!(first_expr("{ :a : 5, :\"x-req-id\" : 17 }\n"), "(dict :a 5 :x-req-id 17)");
}

#[test]
fn references_need_lvalues() {
    assert_eq!(first_expr("f(&a)\n"), "(call f (& a))");
    assert_eq!(first_expr("x := &d.k\n"), "(& (key d :k))");
    assert_eq!(first_expr("x := [&a, &b]\n"), "(list (& a) (& b))");
    // §5.1: `&(a + b)` and `&f()` are errors.
    assert!(parse("x := &(a + b)\n", "t.hy").is_err());
    assert!(parse("x := &f()\n", "t.hy").is_err());
}

#[test]
fn closures_single_expression_versus_block() {
    // Decided by whether anything follows the `)` on the same line (§3).
    assert_eq!(first_expr("single := fn(a, b) a + b\n"), "(fn (a b) (+ a b))");
    assert_eq!(first_expr("multi := fn(c)\n\tx := c * c\n\treturn x - c\nend\n"), "(fn (c) block[2])");
}

#[test]
fn declaration_versus_assignment() {
    let program = parse("x := 1\nx = 2\nd.k = 3\nd[k] = 4\n", "t.hy").unwrap();
    assert!(matches!(&program.body[0], Stmt::Decl { names, .. } if names == &["x"]));
    assert!(matches!(&program.body[1], Stmt::Assign { .. }));
    assert!(matches!(&program.body[2], Stmt::Assign { .. }));
    assert!(matches!(&program.body[3], Stmt::Assign { .. }));

    // `decl = ident ":=" expr` — the left of `:=` is always a plain name.
    assert!(parse("d.k := 1\n", "t.hy").is_err());
    // The left of `=` must be an lvalue.
    assert!(parse("f() = 1\n", "t.hy").is_err());
    assert!(parse("1 = 2\n", "t.hy").is_err());
}

#[test]
fn a_compound_assignment_carries_its_operator() {
    let program = parse("x += 1\nx >>>= 2\nd.k *= 3\nx = 4\n", "t.hy").unwrap();
    let op_of = |i: usize| match &program.body[i] {
        Stmt::Assign { op, .. } => *op,
        other => panic!("expected an assignment, got {other:?}"),
    };
    assert_eq!(op_of(0), Some("+"));
    assert_eq!(op_of(1), Some(">>>"));
    assert_eq!(op_of(2), Some("*"));
    // A plain `=` carries none: it does not read what it writes.
    assert_eq!(op_of(3), None);

    // The target rule is the one `=` has.
    assert!(parse("f() += 1\n", "t.hy").is_err());
    assert!(parse("1 += 2\n", "t.hy").is_err());
    // Assignment is a statement (§3), so a compound one is not an argument
    // either — `f(a += 1)` is not a named argument with a strange name.
    assert!(parse("f(a += 1)\n", "t.hy").is_err());
}

#[test]
fn if_else_if_chain() {
    let program = parse("if a\n\tx = 1\nelse if b\n\tx = 2\nelse\n\tx = 3\nend\n", "t.hy").unwrap();
    let Stmt::If { branches, .. } = &program.body[0] else { panic!("expected if") };
    assert_eq!(branches.len(), 3);
    assert_eq!(branches[0].keyword, "if");
    assert_eq!(branches[1].keyword, "else if");
    assert_eq!(branches[2].keyword, "else");
    assert!(branches[2].cond.is_none());
}

#[test]
fn else_then_nested_if_needs_its_own_end() {
    // `else` at end of line followed by `if` on the next line is an else block
    // containing a nested if (§2), so it needs two `end`s.
    assert!(parse("if a\nelse\nif b\nend\nend\n", "t.hy").is_ok());
    assert!(parse("if a\nelse\nif b\nend\n", "t.hy").is_err());
}

#[test]
fn control_atoms_and_unreserved_builtin_names() {
    let program = parse("for e in list\n\t:continue\n\t:break\nend\n", "t.hy").unwrap();
    let Stmt::For { body, .. } = &program.body[0] else { panic!("expected for") };
    assert!(matches!(&body[0], Stmt::Expr { expr: Expr::Sym(s), .. } if s.name == "continue"));
    assert!(matches!(&body[1], Stmt::Expr { expr: Expr::Sym(s), .. } if s.name == "break"));
    assert_eq!(first_expr("break outer\n"), "(call break outer)");
    assert_eq!(first_expr("break trail\n"), "(call break trail)");
    assert_eq!(first_expr("continue outer\n"), "(call continue outer)");
    assert!(parse("fn break()\nreturn :break\nend\nbreak()\n", "t.hy").is_ok());
}

// --- parallel blocks (§4) ---------------------------------------------------

fn trail_sexprs(src: &str) -> Vec<Vec<String>> {
    let program = parse(src, "t.hy").expect("parses");
    let Stmt::Parallel { trails, .. } = &program.body[0] else { panic!("expected a parallel block") };
    trails
        .iter()
        .map(|t| {
            t.body
                .iter()
                .map(|s| match s {
                    Stmt::Expr { expr, .. } => sexpr(expr),
                    Stmt::Assign { targets, value, .. } => {
                        let targets: Vec<String> = targets.iter().map(sexpr).collect();
                        format!("(= {} {})", targets.join(" "), sexpr(value))
                    }
                    Stmt::Decl { names, value, .. } => {
                        format!("(:= {} {})", names.join(" "), sexpr(value))
                    }
                    Stmt::If { branches, .. } => format!("(if branches[{}])", branches.len()),
                    other => format!("{other:?}"),
                })
                .collect()
        })
        .collect()
}

#[test]
fn transposes_column_wise() {
    // Cell k of every row, in row order, is trail k (§4 step 4).
    let src = "parallel\n\
               \teu = warm(\"eu\") || us = warm(\"us\") || ap = warm(\"ap\")\n\
               \tsmoke(eu)         || smoke(us)         || smoke(ap)\n\
               end\n";
    assert_eq!(
        trail_sexprs(src),
        vec![
            vec!["(= eu (call warm \"eu\"))".to_string(), "(call smoke eu)".to_string()],
            vec!["(= us (call warm \"us\"))".to_string(), "(call smoke us)".to_string()],
            vec!["(= ap (call warm \"ap\"))".to_string(), "(call smoke ap)".to_string()],
        ]
    );
}

#[test]
fn cells_may_be_empty_but_separators_may_not_be_omitted() {
    let src = "parallel\n\ta = 1 || b = 2\n\t      || c = 3\n\td = 4 ||\nend\n";
    assert_eq!(
        trail_sexprs(src),
        vec![
            vec!["(= a 1)".to_string(), "(= d 4)".to_string()],
            vec!["(= b 2)".to_string(), "(= c 3)".to_string()],
        ]
    );
}

#[test]
fn rows_must_agree_on_separator_count() {
    // This is what makes the block's own `end` unambiguous (§4).
    let src = "parallel\n\ta = 1 || b = 2\n\tc = 3\nend\n";
    let err = parse(src, "t.hy").unwrap_err();
    assert!(err.message.contains("same number of `||`"), "{}", err.message);
}

#[test]
fn a_cell_may_hold_a_block_spanning_rows() {
    let src = "parallel\n\
               \tif ok    || x = 1\n\
               \t\ty = 2  ||\n\
               \tend      ||\n\
               end\n";
    let trails = trail_sexprs(src);
    assert_eq!(trails[0], vec!["(if branches[1])".to_string()]);
    assert_eq!(trails[1], vec!["(= x 1)".to_string()]);
}

#[test]
fn end_of_block_is_the_line_with_no_separators() {
    // A row whose only content is a cell-internal `end` still carries its
    // separators, so it cannot be mistaken for the block terminator (§4).
    let src = "parallel\n\tif a || if b\n\tend  || end\nend\n";
    let trails = trail_sexprs(src);
    assert_eq!(trails.len(), 2);
    assert_eq!(trails[0], vec!["(if branches[1])".to_string()]);
}

#[test]
fn a_string_containing_a_separator_does_not_split() {
    let src = "parallel\n\ta = \"x || y\" || b = 2\nend\n";
    let trails = trail_sexprs(src);
    assert_eq!(trails.len(), 2);
    assert_eq!(trails[0], vec!["(= a \"x || y\")".to_string()]);
}

#[test]
fn blank_and_comment_only_lines_are_not_rows() {
    let src = "parallel\n\ta = 1 || b = 2\n\n\t// a note\n\tc = 3 || d = 4\nend\n";
    let trails = trail_sexprs(src);
    assert_eq!(trails[0], vec!["(= a 1)".to_string(), "(= c 3)".to_string()]);
}

#[test]
fn a_parallel_block_may_not_be_written_inside_a_cell() {
    // §4 [P]: nest by calling a function that opens the inner block.
    let src = "parallel\n\tparallel || b = 2\n\tend      ||\nend\n";
    let err = parse(src, "t.hy").unwrap_err();
    assert!(err.message.contains("call a function"), "{}", err.message);
}

#[test]
fn compound_keyword_may_not_be_split_across_a_cell_boundary() {
    let src = "parallel\n\tif a || else || if b\n\tend  ||      || end\nend\n";
    let err = parse(src, "t.hy").unwrap_err();
    assert!(err.message.contains("may not be split"), "{}", err.message);
}

#[test]
fn parallel_for_and_race_for() {
    let program = parse("parallel for r in REGIONS as job\n\twarm(r)\nend\n", "t.hy").unwrap();
    let Stmt::ParallelFor { kind, var, label, .. } = &program.body[0] else { panic!() };
    assert_eq!(*kind, BlockKind::Parallel);
    assert_eq!(var, "r");
    assert_eq!(label.as_deref(), Some("job"));

    let program = parse("race for r in REGIONS\n\tprobe(r)\nend\n", "t.hy").unwrap();
    assert!(matches!(&program.body[0], Stmt::ParallelFor { kind: BlockKind::Race, .. }));

    let program = parse("race while more()\n\tstep()\nend\n", "t.hy").unwrap();
    assert!(matches!(&program.body[0], Stmt::ParallelWhile { kind: BlockKind::Race, .. }));
}

#[test]
fn positions_point_at_the_original_source() {
    // §4 step 6: every error message inside a parallel block depends on this.
    let src = "parallel\n\ta = 1 || b = 2\n\tc = 3 || d = 4\nend\n";
    let program = parse(src, "t.hy").unwrap();
    let Stmt::Parallel { trails, .. } = &program.body[0] else { panic!() };
    assert_eq!(trails[1].body[0].pos().line, 2);
    assert_eq!(trails[1].body[1].pos().line, 3);
    // The second column starts after the `||`, not at column 1.
    assert!(trails[1].body[0].pos().col > 8);
}

#[test]
fn reference_program_parses() {
    let src = std::fs::read_to_string("examples/deploy.hy").expect("example exists");
    parse(&src, "examples/deploy.hy").expect("the spec's reference program parses");
}

#[test]
fn parameters_may_be_by_reference_or_defaulted() {
    // `&name` requires the call to pass a reference; `name := expr` gives it a
    // default evaluated in the function's own scope.
    assert_eq!(first_expr("f := fn(&list, value) value\n"), "(fn (&list value) value)");
    assert_eq!(first_expr("f := fn(a, b = 1) a\n"), "(fn (a b:=1) a)");
    assert_eq!(first_expr("f := fn(a = \"x\") a\n"), "(fn (a:=\"x\") a)");

    // A default is a value, so it cannot also be a reference.
    assert!(parse("f := fn(&a = 1) a\n", "t.hy").is_err());
    // Defaults come after the parameters without them.
    assert!(parse("f := fn(a = 1, b) a\n", "t.hy").is_err());
    assert!(parse("f := fn(a, a) a\n", "t.hy").is_err());
}

#[test]
fn arguments_may_be_named() {
    assert_eq!(first_expr("x := f(1, width = 2)\n"), "(call f 1 width=2)");
    assert_eq!(first_expr("x := f(a = 1, b = 2)\n"), "(call f a=1 b=2)");
    // `name = value` is unambiguous: assignment is a statement, not an
    // expression, so it cannot appear in an argument anyway (§3).
    assert_eq!(first_expr("x := f(a == 1)\n"), "(call f (== a 1))");
    // Named arguments come last, and each parameter is named once.
    assert!(parse("x := f(a = 1, 2)\n", "t.hy").is_err());
    assert!(parse("x := f(a = 1, a = 2)\n", "t.hy").is_err());
    // A keyword is not a name — which is why the builtin's parameter is
    // `terminator` and not `end` (hydra_stdlib.md §2).
    let err = parse("x := f(end = 1)\n", "t.hy").unwrap_err();
    assert!(err.message.contains("`end` is a keyword"), "{}", err.message);
    let err = parse("f := fn(end) end\n", "t.hy").unwrap_err();
    assert!(err.message.contains("`end` is a keyword"), "{}", err.message);
}

#[test]
fn expressions_aggressively_continue_across_single_newlines() {
    for (source, expected) in [
        ("foo\n.bar\n.baz", "(key (key foo :bar) :baz)"),
        ("foo\n.bar.baz", "(key (key foo :bar) :baz)"),
        ("32\n-a", "(- 32 a)"),
        ("32\n-a\n* b\n+ c", "(+ (- 32 (* a b)) c)"),
        ("a +\nb *\nc", "(+ a (* b c))"),
        ("a\nand\nnot b\nor c", "(or (and a (not b)) c)"),
        ("a\n| b\n^ c\n& d\n<< e", "(| a (^ b (& c (<< d e))))"),
        ("a\n<= b\n== c", "(== (<= a b) c)"),
        ("f\n(1,\n2)\n[0]", "(index (call f 1 2) 0)"),
        ("obj\n.method\n(1)", "(call (dot obj method) 1)"),
        ("obj\n.mod\n::call\n(1)", "(call (dot obj mod::call) 1)"),
        ("json\n::decode\n(body)", "(call json::decode body)"),
        ("obj\n.total-1", "(- (key obj :total) 1)"),
        ("obj\n.\"a-b\"", "(key obj :a-b)"),
        ("x\n:=\n32\n- a", "(- 32 a)"),
        ("x\n+=\n2", "2"),
        ("[\n1,\n2\n]", "(list 1 2)"),
        ("{\n:a+b:\n1,\n:c:\n2\n}", "(dict :a+b 1 :c 2)"),
        ("f(\nx\n=\n1\n)", "(call f x=1)"),
    ] {
        let program = parse(source, "t.hy").unwrap_or_else(|e| panic!("{source:?}: {e}"));
        assert_eq!(program.body.len(), 1, "{source:?}");
        assert_eq!(first_expr(source), expected, "{source:?}");
        let formatted = hydra::format::format_source(source, "t.hy").unwrap();
        assert_eq!(first_expr(&formatted), expected, "formatted {source:?}");
    }
}

#[test]
fn blank_lines_stop_continuation() {
    for source in ["32\n\n-a", "32\n \t\n-a", "foo\n\n:bar", "f\n\n(1)", "a\n\n[0]"] {
        assert_eq!(parse(source, "t.hy").unwrap().body.len(), 2, "{source:?}");
    }
    assert_eq!(first_expr("32\n\n-a"), "32");
    let program = parse("32\n\n-a", "t.hy").unwrap();
    let Stmt::Expr { expr, .. } = &program.body[1] else { panic!("expression") };
    assert_eq!(sexpr(expr), "(- a)");
    for source in ["a +\n\nb", "a\n\n+ b", "f(\n\n1)", "{:a:\n\n1}"] {
        assert!(parse(source, "t.hy").is_err(), "{source:?}");
    }
    assert_eq!(parse("foo\nbar\nreject()", "t.hy").unwrap().body.len(), 3);
}

#[test]
fn atom_boundaries_preserve_collections_and_trails() {
    assert_eq!(first_expr(":some-other-prop+interesting_added_info"), ":some-other-prop+interesting_added_info");
    assert_eq!(first_expr("[:a+b,:c*d,:e.f]"), "(list :a+b :c*d :e.f)");
    assert_eq!(first_expr("{:a+b::c*d,:e/f::g=?!}"), "(dict :a+b :c*d :e/f :g=?!)");
    let program = parse("parallel\n:a+b||:c*d\nend", "t.hy").unwrap();
    let Stmt::Parallel { trails, .. } = &program.body[0] else { panic!("parallel") };
    assert_eq!(trails.len(), 2);
}

#[test]
fn continuation_is_per_parallel_column_and_preserves_positions() {
    let program = parse("parallel\nfoo || 32\n.bar || -a\n\n:baz || -b\nend", "t.hy").unwrap();
    let Stmt::Parallel { trails, .. } = &program.body[0] else { panic!("parallel") };
    for trail in trails { assert_eq!(trail.body.len(), 2); }
    let Stmt::Expr { expr, .. } = &trails[0].body[0] else { panic!("expression") };
    assert_eq!(sexpr(expr), "(key foo :bar)");
    assert_eq!(expr.pos().line, 3);
    let Stmt::Expr { expr, .. } = &trails[1].body[0] else { panic!("expression") };
    assert_eq!(sexpr(expr), "(- 32 a)");
    let program = parse("parallel\nfoo || 1\n || 2\n:bar || 3\nend", "t.hy").unwrap();
    let Stmt::Parallel { trails, .. } = &program.body[0] else { panic!("parallel") };
    assert_eq!(trails[0].body.len(), 2);
}

#[test]
fn comments_are_transparent_but_blank_lines_are_not() {
    assert_eq!(first_expr("foo // receiver\n// explanation\n.bar"), "(key foo :bar)");
    assert_eq!(first_expr("a +\n// operand\nb"), "(+ a b)");
    assert_eq!(first_expr("foo\n.\nbar"), "(key foo :bar)");
    assert_eq!(first_expr("foo.\nbar"), "(key foo :bar)");
    assert_eq!(parse("foo\n// explanation\n \n:bar", "t.hy").unwrap().body.len(), 2);
    let program = parse("parallel\nfoo || 32\n// explanation\n.bar || -a\nend", "t.hy").unwrap();
    let Stmt::Parallel { trails, .. } = &program.body[0] else { panic!("parallel") };
    for trail in trails { assert_eq!(trail.body.len(), 1); }
}

#[test]
fn every_binary_operator_and_assignment_continues() {
    for op in ["or", "and", "==", "!=", "===", "!==", "<", ">", "<=", ">=", "|", "^", "&", "<<", ">>", ">>>", "+", "-", "*", "/", "%"] {
        for source in [format!("a\n{op} b"), format!("a {op}\nb")] {
            assert_eq!(first_expr(&source), format!("({op} a b)"), "{source:?}");
        }
    }
    for op in std::iter::once("=").chain(hydra::lexer::COMPOUND_ASSIGN.iter().map(|(op, _)| *op)) {
        for source in [format!("a\n{op} b"), format!("a {op}\nb")] {
            let program = parse(&source, "t.hy").unwrap();
            assert_eq!(program.body.len(), 1);
            assert!(matches!(&program.body[0], Stmt::Assign { .. }), "{source:?}");
        }
    }
}

#[test]
fn colon_atoms_need_no_blank_line_and_dots_require_receivers() {
    assert_eq!(parse("foo\n:bar\n:baz\n", "t.hy").unwrap().body.len(), 3);
    assert_eq!(parse("if :true\n:break\nend\n", "t.hy").unwrap().body.len(), 1);
    for source in [".atom", "foo\n\n.bar", "32\n\n+2"] {
        assert!(parse(source, "t.hy").is_err(), "{source}");
    }
    assert!(parse("{\n:key\n: :value\n}", "t.hy").is_ok());
}

#[test]
fn calls_without_parentheses_are_outermost_and_comma_separated() {
    for source in ["add 1, 2", "x := add 1, 2", "x = add 1, 2", "x += add 1, 2"] {
        assert_eq!(first_expr(source), "(call add 1 2)");
    }
    assert_eq!(first_expr("f g(1), width = 2"), "(call f (call g 1) width=2)");
    assert_eq!(first_expr("f :ready"), "(call f :ready)");
    assert_eq!(first_expr("f { :name : :value }"), "(call f (dict :name :value))");
    assert_eq!(first_expr("x.f 1"), "(call (dot x f) 1)");
    assert_eq!(first_expr("x.mod::f 1"), "(call (dot x mod::f) 1)");
    assert_eq!(first_expr("&x.push 1"), "(call (dot (& x) push) 1)");
    assert_eq!(first_expr("f 1,\n2 +\n3"), "(call f 1 (+ 2 3))");
    for source in ["f g 1", "x := f g 1", "f(g 1)", "[f 1]", "if f 1\nend", "f 1 2"] {
        assert!(parse(source, "t.hy").is_err(), "{source}");
    }
    assert_eq!(first_expr("f"), "f");
    assert_eq!(first_expr("x := f"), "f");
    assert_eq!(first_expr("return"), "(call return)");
    assert_eq!(first_expr("x := return"), "return");
}

#[test]
fn whitespace_distinguishes_unary_arguments_and_indexing() {
    for (source, expected) in [
        ("f -1", "(call f (- 1))"),
        ("f - 1", "(- f 1)"),
        ("f-1", "(- f 1)"),
        ("f [1]", "(call f (list 1))"),
        ("f[1]", "(index f 1)"),
        ("f\n[1]", "(index f 1)"),
        ("f\n-1", "(- f 1)"),
        ("f &x", "(call f (& x))"),
        ("f & x", "(& f x)"),
        ("f -\n1", "(- f 1)"),
        ("f ~x", "(call f (~ x))"),
        ("f not x", "(call f (not x))"),
    ] {
        assert_eq!(first_expr(source), expected, "{source}");
    }
}
