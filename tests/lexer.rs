//! Lexer tests (spec §1, §2).

use hydra::lexer::{static_text, text_piece, tokenize, StrPiece, Tok};

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
fn a_compound_assignment_is_one_operator() {
    // Each one is longer than both the operator it carries and the `=` it ends
    // with, so longest match has to reach it first.
    for (src, spelling) in [
        ("a += b", "+="),
        ("a -= b", "-="),
        ("a *= b", "*="),
        ("a /= b", "/="),
        ("a %= b", "%="),
        ("a |= b", "|="),
        ("a &= b", "&="),
        ("a ^= b", "^="),
        ("a <<= b", "<<="),
        ("a >>= b", ">>="),
        ("a >>>= b", ">>>="),
    ] {
        assert_eq!(ops(src), vec![spelling], "for {src:?}");
    }
    // And the operators they are built from still lex as themselves.
    assert_eq!(ops("a >= b"), vec![">="]);
    assert_eq!(ops("a <= b"), vec!["<="]);
    assert_eq!(ops("a = -b"), vec!["=", "-"]);
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
    // Colons introduce atoms; dots always introduce key lookups.
    assert_eq!(kinds(":null")[0], Tok::Sym { parts: vec![text_piece("null")], quoted: false });
    assert_eq!(kinds("d.a")[1], Tok::Op("."));
    assert_eq!(kinds("f().a")[3], Tok::Op("."));
    assert_eq!(kinds("[1].a")[3], Tok::Op("."));
    let ks = kinds("{ :a : 1 }.a");
    assert_eq!(ks[ks.len() - 5], Tok::Op("}"));
    assert_eq!(ks[ks.len() - 4], Tok::Op("."));
    assert_eq!(kinds("x := :true")[2], Tok::Sym { parts: vec![text_piece("true")], quoted: false });
    assert_eq!(kinds("not :false")[1], Tok::Sym { parts: vec![text_piece("false")], quoted: false });
}

#[test]
fn a_symbols_name_may_contain_a_hyphen() {
    // Subtracting one symbol from another is nonsense, so `:x-req-id` can only
    // have been meant as one name (§2).
    assert_eq!(
        kinds(":x-req-id")[0],
        Tok::Sym { parts: vec![text_piece("x-req-id")], quoted: false }
    );
    assert_eq!(kinds(":a-1")[0], Tok::Sym { parts: vec![text_piece("a-1")], quoted: false });
    // A space ends the name; a trailing operator character does not.
    assert_eq!(kinds(":a - b")[0], Tok::Sym { parts: vec![text_piece("a")], quoted: false });
    assert_eq!(kinds(":a - b")[1], Tok::Op("-"));
    assert_eq!(kinds(":a-")[0], Tok::Sym { parts: vec![text_piece("a-")], quoted: false });

    // A *key lookup* is not a symbol literal: there the thing left of the `-`
    // is a value, and subtracting from it is perfectly sensible.
    let ks = kinds("d.total-1");
    assert_eq!(ks[1], Tok::Op("."));
    assert_eq!(ks[2], Tok::Ident("total".into()));
    assert_eq!(ks[3], Tok::Op("-"));
}

#[test]
fn quoted_symbols() {
    assert_eq!(
        kinds(":\"content-type\"")[0],
        Tok::Sym { parts: vec![text_piece("content-type")], quoted: true }
    );
    // `headers."content-type"` is `headers[:"content-type"]` (§2): the lexer
    // produces a lookup dot followed by a string, and the parser turns the pair
    // into a key.
    let ks = kinds("headers.\"content-type\"");
    assert_eq!(ks[1], Tok::Op("."));
    assert!(matches!(&ks[2], Tok::Str { parts } if static_text(parts).as_deref() == Some("content-type")));
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
fn strings_have_escapes() {
    let ks = kinds(r#""hi \"there\"\n""#);
    assert_eq!(
        ks[0],
        Tok::Str {
            parts: vec![StrPiece::Text {
                value: "hi \"there\"\n".into(),
                raw: r#"hi \"there\"\n"#.into()
            }]
        }
    );

    // A string is one token even when it contains what would otherwise be
    // syntax, which is why the row splitter works on tokens.
    let ks = kinds("\"a || b end\"");
    assert!(matches!(&ks[0], Tok::Str { parts } if static_text(parts).as_deref() == Some("a || b end")));
    assert_eq!(ks.len(), 3); // string, newline, eof
}

#[test]
fn interpolation_is_lexed_recursively() {
    // §1: the expression is lexed and parsed recursively, so it may contain
    // parentheses, calls, and further strings with their own interpolations.
    let ks = kinds(r#""hi \(name), \(a + b)""#);
    let Tok::Str { parts } = &ks[0] else { panic!("a string, got {:?}", ks[0]) };
    assert_eq!(parts.len(), 4);
    assert!(matches!(&parts[0], StrPiece::Text { value, .. } if value == "hi "));
    let StrPiece::Expr { tokens, .. } = &parts[1] else { panic!("an interpolation") };
    assert_eq!(tokens[0].kind, Tok::Ident("name".into()));
    let StrPiece::Expr { tokens, .. } = &parts[3] else { panic!("an interpolation") };
    assert_eq!(tokens.len(), 4); // a + b, then eof

    // A string inside an interpolation, with its own interpolation.
    let ks = kinds(r#""outer \(f("inner \(x)")) done""#);
    let Tok::Str { parts } = &ks[0] else { panic!("a string") };
    assert_eq!(parts.len(), 3);
    assert!(static_text(parts).is_none());

    // Parentheses inside the expression do not end it early.
    let ks = kinds(r#""\(f(g(1), 2))""#);
    let Tok::Str { parts } = &ks[0] else { panic!("a string") };
    assert_eq!(parts.len(), 1);
}

#[test]
fn a_quoted_symbol_may_interpolate() {
    // §2: which gives dynamic symbol construction without a `sym(str)` builtin.
    let ks = kinds(r#":"\(prefix)-id""#);
    let Tok::Sym { parts, quoted } = &ks[0] else { panic!("a symbol, got {:?}", ks[0]) };
    assert!(*quoted);
    assert_eq!(parts.len(), 2);
    assert!(static_text(parts).is_none());
}

#[test]
fn an_unterminated_interpolation_is_an_error() {
    assert!(tokenize(r#"x := "\(a""#, "t.hy").is_err());
    assert!(tokenize("x := \"\\(a\nb)\"\n", "t.hy").is_err());
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

#[test]
fn atoms_consume_punctuation_until_a_delimiter() {
    for name in ["some-other-prop+interesting_added_info", "a-", "a.b", "a!@#$%=<>?~&^", "a/b", "a|b", "a\\b", "aé"] {
        let src = format!(":{name}");
        let lexed = tokenize(&src, "t.hy").unwrap();
        assert_eq!(lexed.tokens[0].static_str().as_deref(), Some(name));
        assert_eq!(lexed.tokens[0].len, src.chars().count() as u32);
        assert_eq!(lexed.tokens.len(), 3);
        assert!(hydra::lexer::is_symbol_name(name));
    }
    for delimiter in [" ", "\t", "\n", "\r\n", "(", ")", "[", "]", "{", "}", ",", ":", "\"text\"", "// comment", "||"] {
        let src = format!(":atom+info{delimiter}");
        assert_eq!(tokenize(&src, "t.hy").unwrap().tokens[0].static_str().as_deref(), Some("atom+info"), "{src:?}");
    }
    for name in ["a b", "a:b", "a,b", "a//b", "a||b", "a(b", "a\"b"] {
        assert!(!hydra::lexer::is_symbol_name(name), "{name:?}");
    }
}

#[test]
fn colon_atoms_leave_assignment_namespace_and_dict_separators_intact() {
    assert_eq!(ops("x := ::len([])"), vec![":=", "::", "(", "[", "]", ")"]);
    let lexed = tokenize("{:name:\"Hydra\",:status::ready}", "t.hy").unwrap();
    let names: Vec<String> = lexed.tokens.iter().filter_map(|t| match &t.kind {
        Tok::Sym { .. } => t.static_str(),
        _ => None,
    }).collect();
    assert_eq!(names, ["name", "status", "ready"]);
    assert_eq!(ops("{:status::ready}"), vec!["{", ":", "}"]);
}
