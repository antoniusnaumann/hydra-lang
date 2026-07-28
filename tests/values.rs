//! Value-model tests that Hydra code cannot reach on its own (spec §2, §5).

use hydra::value::{interned_count, num_to_text, sym, to_int32, to_uint32};

#[test]
fn symbols_are_interned_and_compared_by_identity() {
    assert_eq!(sym("ok"), sym("ok"));
    assert_ne!(sym("ok"), sym("nope"));
    assert_eq!(sym("ok").ptr(), sym("ok").ptr());
}

#[test]
fn the_intern_table_is_collectable() {
    // §2: input data can mint symbols, so the table must be collectable or a
    // program that decodes in a loop grows without bound. There is no way to
    // mint one from Hydra yet (QUESTIONS.md §3a), so this tests the mechanism
    // the runtime will need.
    let before = interned_count();
    {
        let held = sym("a-name-nothing-else-uses");
        assert_eq!(held.name(), "a-name-nothing-else-uses");
        assert_eq!(interned_count(), before + 1);
    }
    assert_eq!(interned_count(), before, "the entry is gone once nothing holds the symbol");
}

#[test]
fn to_int32_follows_javascript() {
    // §2: NaN and the infinities convert to 0; values outside range wrap.
    assert_eq!(to_int32(f64::NAN), 0);
    assert_eq!(to_int32(f64::INFINITY), 0);
    assert_eq!(to_int32(f64::NEG_INFINITY), 0);
    assert_eq!(to_int32(2147483648.0), -2147483648); // 2^31 becomes -2^31
    assert_eq!(to_int32(4294967296.0), 0);
    assert_eq!(to_int32(4294967297.0), 1);
    assert_eq!(to_int32(-1.0), -1);
    assert_eq!(to_int32(3.9), 3); // truncation is toward zero
    assert_eq!(to_int32(-3.9), -3);
    assert_eq!(to_uint32(-1.0), 4294967295);
}

#[test]
fn numbers_render_like_javascript() {
    assert_eq!(num_to_text(3.0), "3");
    assert_eq!(num_to_text(3.5), "3.5");
    assert_eq!(num_to_text(-0.0), "0");
    assert_eq!(num_to_text(f64::NAN), "NaN");
    assert_eq!(num_to_text(f64::INFINITY), "Infinity");
    assert_eq!(num_to_text(f64::NEG_INFINITY), "-Infinity");
}
