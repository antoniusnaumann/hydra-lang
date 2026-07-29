//! Formatter tests (spec §12).

use hydra::format::format_source;

fn fmt(src: &str) -> String {
    format_source(src, "t.hy").expect("formats")
}

/// Formatting formatted source changes nothing (rule 6).
fn fmt_idempotent(src: &str) -> String {
    let once = fmt(src);
    let twice = format_source(&once, "t.hy").expect("formats");
    assert_eq!(once, twice, "formatting is not idempotent");
    once
}

#[test]
fn indentation_is_one_tab_per_level_and_never_spaces() {
    let src = "if a\nx := 1\nif b\ny := 2\nend\nend\n";
    assert_eq!(fmt_idempotent(src), "if a\n\tx := 1\n\tif b\n\t\ty := 2\n\tend\nend\n");

    // Existing indentation is cosmetic and gets replaced, spaces included.
    let src = "if a\n        x := 1\nend\n";
    assert_eq!(fmt_idempotent(src), "if a\n\tx := 1\nend\n");
}

#[test]
fn spacing_around_operators_and_after_commas() {
    assert_eq!(fmt_idempotent("x:=1+2*3\n"), "x := 1 + 2 * 3\n");
    assert_eq!(fmt_idempotent("f( a,b )\n"), "f(a, b)\n");
    assert_eq!(fmt_idempotent("x := [ 1,2,3 ]\n"), "x := [1, 2, 3]\n");
    assert_eq!(fmt_idempotent("x := a[ i ]\n"), "x := a[i]\n");
    assert_eq!(fmt_idempotent("x := a . b\n"), "x := a.b\n");
    assert_eq!(fmt_idempotent("x := json :: decode(b)\n"), "x := json::decode(b)\n");
    // A leading `::` is the builtin namespace and stays tight too (§7).
    assert_eq!(fmt_idempotent(":: push(& rows,1)\n"), "::push(&rows, 1)\n");
    assert_eq!(fmt_idempotent("x := a==b\n"), "x := a == b\n");
    assert_eq!(fmt_idempotent("x := not a and b\n"), "x := not a and b\n");
    assert_eq!(fmt_idempotent("x := a>>>b\n"), "x := a >>> b\n");
    // A compound assignment is one operator and gets the spacing of one.
    assert_eq!(fmt_idempotent("x+=1\n"), "x += 1\n");
    assert_eq!(fmt_idempotent("x>>>=1\n"), "x >>>= 1\n");
    assert_eq!(fmt_idempotent("d . k*=2\n"), "d.k *= 2\n");
    // And what follows it is still an operand, so a minus there is a prefix.
    assert_eq!(fmt_idempotent("x += -1\n"), "x += -1\n");
}

#[test]
fn no_space_between_a_reference_and_its_lvalue() {
    // `&a`, never `& a` (rule 2).
    assert_eq!(fmt_idempotent("f(& a)\n"), "f(&a)\n");
    assert_eq!(fmt_idempotent("x := & a\n"), "x := &a\n");
    assert_eq!(fmt_idempotent("d := { .x : & a }\n"), "d := { .x : &a }\n");
    assert_eq!(fmt_idempotent("list := [& a, & b]\n"), "list := [&a, &b]\n");
    // Binary `&` keeps its spaces.
    assert_eq!(fmt_idempotent("x := flags&MASK\n"), "x := flags & MASK\n");
    // As does a prefix minus after a binary one.
    assert_eq!(fmt_idempotent("x := a - -b\n"), "x := a - -b\n");
    assert_eq!(fmt_idempotent("x := -a\n"), "x := -a\n");
    assert_eq!(fmt_idempotent("x := ~mask\n"), "x := ~mask\n");
}

#[test]
fn dict_literals_normalise() {
    // Rule 3a: spaces inside the braces and around the colon.
    assert_eq!(fmt_idempotent("d := {.a:1,.b:2}\n"), "d := { .a : 1, .b : 2 }\n");
    assert_eq!(fmt_idempotent("d := {   }\n"), "d := {}\n");
    // A quoted symbol whose content is a valid identifier is rewritten bare.
    assert_eq!(fmt_idempotent("d := { .\"name\" : 1 }\n"), "d := { .name : 1 }\n");
    // One that is not stays quoted.
    assert_eq!(
        fmt_idempotent("d := { .\"x-req-id\" : 1 }\n"),
        "d := { .\"x-req-id\" : 1 }\n"
    );
    assert_eq!(fmt_idempotent("x := headers.\"content-type\"\n"), "x := headers.\"content-type\"\n");
    assert_eq!(fmt_idempotent("x := headers.\"name\"\n"), "x := headers.name\n");
}

#[test]
fn compound_keywords_normalise_to_one_space() {
    assert_eq!(
        fmt_idempotent("if a\nelse    if b\nend\n"),
        "if a\nelse if b\nend\n"
    );
    assert_eq!(
        fmt_idempotent("parallel\tfor r in x\nf(r)\nend\n"),
        "parallel for r in x\n\tf(r)\nend\n"
    );
}

#[test]
fn line_breaks_never_move() {
    // Rule 4: a newline terminates a statement, so the formatter must not join
    // or split statement lines — including blank ones.
    let src = "x := 1\n\n\ny := 2\n";
    assert_eq!(fmt_idempotent(src), "x := 1\n\n\ny := 2\n");
}

#[test]
fn comments_stay_on_their_line() {
    let src = "// a note\nx := 1    // trailing\n\t// indented note\ny := 2\n";
    assert_eq!(fmt_idempotent(src), "// a note\nx := 1 // trailing\n// indented note\ny := 2\n");

    // A comment inside a block is indented with the block.
    let src = "if a\n// why\nx := 1\nend\n";
    assert_eq!(fmt_idempotent(src), "if a\n\t// why\n\tx := 1\nend\n");
}

#[test]
fn closures_open_a_block_only_when_multi_line() {
    // §3: decided by whether anything follows the `)` on the same line.
    let src = "single := fn(a, b) a + b\nmulti := fn(c)\nx := c * c\nreturn x - c\nend\n";
    assert_eq!(
        fmt_idempotent(src),
        "single := fn(a, b) a + b\nmulti := fn(c)\n\tx := c * c\n\treturn x - c\nend\n"
    );
}

#[test]
fn parallel_cells_are_padded_to_the_widest_in_their_column() {
    let src = "parallel\neu = warm(\"eu\") || us = w(\"us\") || ap = warmer(\"ap\")\nsmoke(eu) || smoke(us) || smoke(ap)\nend\n";
    assert_eq!(
        fmt_idempotent(src),
        "parallel\n\
         \teu = warm(\"eu\") || us = w(\"us\") || ap = warmer(\"ap\")\n\
         \tsmoke(eu)       || smoke(us)    || smoke(ap)\n\
         end\n"
    );
}

#[test]
fn every_row_keeps_a_separator_for_every_column() {
    // Rule 5: empty cells included — which is also what makes the block's own
    // `end` unambiguous (§4).
    let src = "parallel\na = 1||b = 2\nc = 3||\nend\n";
    assert_eq!(fmt_idempotent(src), "parallel\n\ta = 1 || b = 2\n\tc = 3 ||\nend\n");
}

#[test]
fn a_cell_holding_a_block_keeps_its_rows() {
    let src = "parallel\nif ok||x = 1\ny = 2||\nend||\nend\n";
    assert_eq!(
        fmt_idempotent(src),
        "parallel\n\tif ok || x = 1\n\ty = 2 ||\n\tend   ||\nend\n"
    );
}

#[test]
fn interpolations_normalise_like_ordinary_expressions() {
    // §12 rule 3a: `\(a + b)`, never `\( a+b )`.
    assert_eq!(fmt_idempotent("x := \"n \\( a+b )\"\n"), "x := \"n \\(a + b)\"\n");
    assert_eq!(fmt_idempotent("x := \"\\(f( 1,2 ))\"\n"), "x := \"\\(f(1, 2))\"\n");
    assert_eq!(fmt_idempotent("x := .\"\\( prefix )-id\"\n"), "x := .\"\\(prefix)-id\"\n");
    // A string nested inside an interpolation normalises too.
    assert_eq!(
        fmt_idempotent("x := \"a \\(f(\"b \\( c )\"))\"\n"),
        "x := \"a \\(f(\"b \\(c)\"))\"\n"
    );
    // An interpolated symbol can never be rewritten bare.
    assert_eq!(fmt_idempotent("d := { .\"\\(k)\" : 1 }\n"), "d := { .\"\\(k)\" : 1 }\n");
}

#[test]
fn a_string_that_looks_like_syntax_is_left_alone() {
    let src = "x := \"a || b end // not a comment\"\n";
    assert_eq!(fmt_idempotent(src), "x := \"a || b end // not a comment\"\n");
}

#[test]
fn escapes_survive_a_format_pass() {
    let src = "x := \"quote \\\" tab \\t done\"\n";
    assert_eq!(fmt_idempotent(src), "x := \"quote \\\" tab \\t done\"\n");
}

#[test]
fn the_reference_program_formats_and_stays_formatted() {
    let src = std::fs::read_to_string("examples/deploy.hy").expect("example");
    let once = format_source(&src, "examples/deploy.hy").expect("formats");
    let twice = format_source(&once, "examples/deploy.hy").expect("formats");
    assert_eq!(once, twice);
    // Its `parallel` block comes out aligned.
    assert!(once.contains("\teu = warm(\"eu\", img) || us = warm(\"us\", img) || ap = warm(\"ap\", img)\n"));
    assert!(once.contains("\tsmoke(eu)            || smoke(us)            || smoke(ap)\n"));
}
