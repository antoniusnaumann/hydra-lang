//! Lexer tests (spec §1, §2).

use hydra::lexer::{tokenize, Tok};

fn kinds(src: &str) -> Vec<Tok> {
    tokenize(src, "t.hy").expect("lexes").tokens.into_iter().map(|t| t.kind).collect()
}

fn ops(src: &str) -> Vec<String> {
    tokenize(src, "t.hy")
        .expect("lexes")
        .tokens
        .iter()
        .filter_map(|t| match &t.kind {
            Tok::Op(o) => Some(o.to_string()),
            _ => None,
        })
        .collect()
}

#[test]
fn longest_match_wins() {
    // `===` before `==` before `=`, `!==` before `!=`, `>>>` before `>>`,
    // `::` and `:=` before `:`, `||` before `|` (§2).
    assert_eq!(ops("a === b"), vec!["==="]);
    assert_eq!(ops("a == b"), vec!["=="]);
    assert_eq!(ops("a = b"), vec!["="]);
    assert_eq!(ops("a !== b"), vec!["!=="]);
    assert_eq!(ops("a != b"), vec!["!="]);
    assert_eq!(ops("a >>> b"), vec![">>>"]);
    assert_eq!(ops("a >> b"), vec![">>"]);
    assert_eq!(ops("a > b"), vec![">"]);
    assert_eq!(ops("a := b"), vec![":="]);
    assert_eq!(ops("a::b"), vec!["::"]);
    assert_eq!(ops("a || b"), vec!["||"]);
    assert_eq!(ops("a | b"), vec!["|"]);
}

#[test]
fn compound_keywords_are_one_token() {
    for (src, spelling) in [
        ("else if", "else if"),
        ("else\tif", "else if"),
        ("else   if", "else if"),
        ("parallel for", "parallel for"),
        ("parallel while", "parallel while"),
        ("race for", "race for"),
        ("race while", "race while"),
    ] {
        let ks = kinds(src);
        assert_eq!(ks[0], Tok::Kw(spelling), "for {src:?}");
        assert!(matches!(ks[1], Tok::Newline), "for {src:?}: {ks:?}");
    }
}

#[test]
fn compound_keywords_never_span_a_newline() {
    // `else` at end of line followed by `if` is an else block containing a
    // nested if, which needs its own `end` (§2).
    let ks = kinds("else\nif");
    assert_eq!(ks[0], Tok::Kw("else"));
    assert_eq!(ks[1], Tok::Newline);
    assert_eq!(ks[2], Tok::Kw("if"));
}

#[test]
fn compound_head_followed_by_something_else_stays_simple() {
    let ks = kinds("else iffy");
    assert_eq!(ks[0], Tok::Kw("else"));
    assert_eq!(ks[1], Tok::Ident("iffy".into()));

    let ks = kinds("parallel");
    assert_eq!(ks[0], Tok::Kw("parallel"));

    // No gap at all is not a compound keyword either.
    let ks = kinds("elseif");
    assert_eq!(ks[0], Tok::Ident("elseif".into()));
}

#[test]
fn symbol_versus_key_lookup() {
    // A dot in leading position starts a symbol; a dot after an expression,
    // a `)`, a `]`, a `}` or a literal is a lookup (§2).
    assert_eq!(kinds(".null")[0], Tok::Sym { name: "null".into(), quoted: false });
    assert_eq!(kinds("d.a")[1], Tok::Op("."));
    assert_eq!(kinds("f().a")[3], Tok::Op("."));
    assert_eq!(kinds("[1].a")[3], Tok::Op("."));
    let ks = kinds("{ .a : 1 }.a");
    assert_eq!(ks[ks.len() - 5], Tok::Op("}"));
    assert_eq!(ks[ks.len() - 4], Tok::Op("."));
    assert_eq!(kinds("x := .true")[2], Tok::Sym { name: "true".into(), quoted: false });
    assert_eq!(kinds("not .false")[1], Tok::Sym { name: "false".into(), quoted: false });
}

#[test]
fn quoted_symbols() {
    assert_eq!(
        kinds(".\"content-type\"")[0],
        Tok::Sym { name: "content-type".into(), quoted: true }
    );
    // `headers."content-type"` is `headers[."content-type"]` (§2): the lexer
    // produces a lookup dot followed by a string, and the parser turns the pair
    // into a key.
    let ks = kinds("headers.\"content-type\"");
    assert_eq!(ks[1], Tok::Op("."));
    assert!(matches!(&ks[2], Tok::Str { value, .. } if value == "content-type"));
}

#[test]
fn numbers_and_key_lookup_on_them() {
    assert_eq!(kinds("42")[0], Tok::Num { value: 42.0, raw: "42".into() });
    assert_eq!(kinds("3.0")[0], Tok::Num { value: 3.0, raw: "3.0".into() });
    assert_eq!(kinds("1e3")[0], Tok::Num { value: 1000.0, raw: "1e3".into() });
    assert_eq!(kinds("1.5e-3")[0], Tok::Num { value: 0.0015, raw: "1.5e-3".into() });
    // A fraction needs a digit after the dot, so `3.a` stays a lookup.
    let ks = kinds("3.a");
    assert_eq!(ks[0], Tok::Num { value: 3.0, raw: "3".into() });
    assert_eq!(ks[1], Tok::Op("."));
}

#[test]
fn strings_have_escapes_but_no_interpolation() {
    let ks = kinds(r#""hi \"there\"\n""#);
    assert_eq!(ks[0], Tok::Str { value: "hi \"there\"\n".into(), raw: r#"hi \"there\"\n"#.into() });

    // A string is one token even when it contains what would otherwise be
    // syntax, which is why the row splitter works on tokens.
    let ks = kinds("\"a || b end\"");
    assert!(matches!(&ks[0], Tok::Str { value, .. } if value == "a || b end"));
    assert_eq!(ks.len(), 3); // string, newline, eof
}

#[test]
fn comments_run_to_end_of_line() {
    let lexed = tokenize("x := 1 // trailing\n// whole line\ny := 2\n", "t.hy").unwrap();
    assert_eq!(lexed.comments[&1], (8, "// trailing".to_string()));
    assert_eq!(lexed.comments[&2], (1, "// whole line".to_string()));
    assert!(!lexed.comments.contains_key(&3));
}

#[test]
fn unterminated_string_is_an_error() {
    assert!(tokenize("x := \"abc\n", "t.hy").is_err());
    assert!(tokenize("x := \"abc", "t.hy").is_err());
    assert!(tokenize(r#"x := "\q""#, "t.hy").is_err());
}

#[test]
fn no_bang_operator() {
    // There is no `!` operator and no `&&`; logic is and / or / not (§2).
    assert!(tokenize("!a", "t.hy").is_err());
    assert_eq!(ops("a && b"), vec!["&", "&"]);
}
