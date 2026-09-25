//! Editor support tests (spec §13).

use hydra::editor::{semantic_tokens, theme_json, tmlanguage_json, CLASSES};

fn classes(src: &str) -> Vec<(u32, u32, u32, &'static str)> {
    semantic_tokens(src, "t.hy")
        .expect("lexes")
        .into_iter()
        .map(|t| (t.line, t.col, t.len, t.class))
        .collect()
}

fn class_at(src: &str, line: u32, col: u32) -> &'static str {
    classes(src)
        .into_iter()
        .find(|(l, c, _, _)| *l == line && *c == col)
        .map(|(_, _, _, class)| class)
        .unwrap_or("<none>")
}

#[test]
fn a_compound_keyword_is_coloured_as_one_unit() {
    // §13: `parallel`, `race` and every compound built on them are one unit.
    // The lexer already made it one token, so this is free here and is the
    // thing a regex grammar has to work for.
    let found = classes("parallel for r in x\nend\n");
    assert_eq!(found[0], (1, 1, 12, "keyword.concurrency"));
    assert_eq!(class_at("race while c\nend\n", 1, 1), "keyword.concurrency");
    assert_eq!(class_at("parallel\n\ta = 1 ||\nend\n", 1, 1), "keyword.concurrency");

    // Extra internal whitespace is still one token, and its source extent is
    // what it spanned, not what it normalises to.
    let found = classes("parallel   for r in x\nend\n");
    assert_eq!(found[0], (1, 1, 14, "keyword.concurrency"));
}

#[test]
fn the_trail_separator_has_its_own_class() {
    assert_eq!(class_at("parallel\n\ta = 1 || b = 2\nend\n", 2, 8), "punctuation.trail");
}

#[test]
fn keywords_split_between_control_and_other() {
    assert_eq!(class_at("if a\nend\n", 1, 1), "keyword.control");
    assert_eq!(class_at("for a in b\nend\n", 1, 7), "keyword.control");
    assert_eq!(class_at("if a\nelse if b\nend\n", 2, 1), "keyword.control");
    assert_eq!(class_at("fn f()\nend\n", 1, 1), "keyword.other");
    assert_eq!(class_at("use json\n", 1, 1), "keyword.other");
    assert_eq!(class_at("x := a and b\n", 1, 8), "keyword.other");
    assert_eq!(class_at("for a in b as scan\nend\n", 1, 12), "keyword.other");
}

#[test]
fn identifiers_are_classified_by_context() {
    assert_eq!(class_at("x := plain\n", 1, 6), "variable");
    assert_eq!(class_at("x := called()\n", 1, 6), "entity.function");
    assert_eq!(class_at("x := d.field\n", 1, 8), "variable.property");
    assert_eq!(class_at("x := json::decode\n", 1, 6), "entity.namespace");
    assert_eq!(class_at("x := ALL_CAPS\n", 1, 6), "constant");
    // ALL_CAPS is convention only (§2), but §13 gives it a colour.
    assert_eq!(class_at("x := Mixed_Caps\n", 1, 6), "variable");
}

#[test]
fn literals_and_comments() {
    assert_eq!(class_at("x := :null\n", 1, 7), "constant");
    assert_eq!(class_at("break()\n", 1, 1), "entity.function");
    assert_eq!(class_at("x := :null\n", 1, 6), "punctuation.delimiter");
    assert_eq!(class_at("x := :\"x-req-id\"\n", 1, 6), "punctuation.delimiter");
    assert_eq!(class_at("x := \"text\"\n", 1, 6), "string");
    assert_eq!(class_at("x := 3.0\n", 1, 6), "constant.numeric");
    assert_eq!(class_at("x := 1 // why\n", 1, 8), "comment");
    // A comment marker inside a string is not a comment.
    assert_eq!(class_at("x := \"// not\"\n", 1, 6), "string");
}

#[test]
fn every_class_in_the_spec_table_is_produced() {
    let src = "// note\n\
               use json\n\
               REGIONS := [\"eu\"]\n\
               status := :null\n\
               parallel for r in REGIONS\n\
               \tjson::decode(r.body) || wait(60)\n\
               end\n";
    let produced: Vec<&str> = classes(src).into_iter().map(|(_, _, _, c)| c).collect();
    for (class, _, _) in CLASSES {
        assert!(produced.contains(class), "no {class} in {produced:?}");
    }
}

#[test]
fn the_grammar_matches_compound_keywords_before_their_heads() {
    let grammar = tmlanguage_json();
    let compound = grammar.find("parallel[ \\\\t]+for").expect("a compound rule");
    let plain = grammar.find("(parallel|race)").expect("a plain rule");
    assert!(compound < plain, "a regex grammar has to try the compound form first");
    assert!(grammar.contains("\"scopeName\": \"source.hydra\""));
    assert!(grammar.contains("\"fileTypes\": [\"hy\"]"));
}

#[test]
fn the_theme_carries_the_reference_colours() {
    let theme = theme_json();
    // §13's table, as used in the mock-ups.
    assert!(theme.contains("#ff8a65"));
    assert!(theme.contains("#c586c0"));
    assert!(theme.contains("#4ec9b0"));
    assert!(theme.contains("\"fontStyle\": \"italic\""));
}

#[test]
fn control_handlers_use_returns_colour_but_keep_atom_delimiters() {
    let expected = class_at("return 1\n", 1, 1);
    for name in ["exit", "panic", "reject"] {
        assert_eq!(class_at(&format!("{name}(1)\n"), 1, 1), expected);
        assert_eq!(class_at(&format!(":{name}\n"), 1, 1), "punctuation.delimiter");
        assert_eq!(class_at(&format!(":{name}\n"), 1, 2), expected);
        assert_eq!(class_at(&format!(":\"{name}\"\n"), 1, 2), expected);
        assert_eq!(class_at(&format!("::{name}(1)\n"), 1, 3), expected);
        assert_eq!(class_at(&format!(":{name}-later\n"), 1, 2), "constant");
        assert_eq!(class_at(&format!("module::{name}(1)\n"), 1, 9), "entity.function");
    }
}
