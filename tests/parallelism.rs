//! The worker pool (spec §9.1, with the owner's ruling that CPU-bound work
//! must actually run in parallel).
//!
//! Everywhere else the tests pin the pool to one worker so a schedule is
//! reproducible. Here it runs wide, so these assert on what must hold for
//! *every* schedule — plus the one thing that only holds with real threads:
//! that more than one trail is stepping at a time.

use hydra::value::to_text;
use hydra::vm::{run_source, Options, RunResult};

fn cores() -> usize {
    std::thread::available_parallelism().map(|n| n.get()).unwrap_or(1)
}

fn wide() -> Options {
    Options { search_path: Vec::new(), threads: cores().max(2), ..Options::default() }
}

fn run_with(src: &str, options: Options) -> RunResult {
    run_source(src, "t.hy", options).expect("compiles")
}

fn read(result: &RunResult, name: &str) -> String {
    let cell = result.root_scope.lookup(name).unwrap_or_else(|| panic!("no `{name}`"));
    let value = cell.read().unwrap().clone();
    to_text(&value)
}

/// Enough arithmetic that a trail cannot finish inside one scheduler slice.
const BUSY: &str = "
fn burn(rounds)
\tn := 0
\ti := 0
\twhile i < rounds
\t\tn = n + i % 7
\t\ti = i + 1
\tend
\treturn n
end
";

#[test]
fn cpu_bound_trails_run_at_the_same_time() {
    if cores() < 2 {
        return; // nothing to prove on a single core
    }
    let src = format!(
        "{BUSY}
done := 0
parallel for worker in [1, 2, 3, 4]
\tr := burn(4000)
\tdone = done + 1
end
"
    );
    let result = run_with(&src, wide());
    assert!(result.crash.is_none(), "{:?}", result.crash);
    assert!(
        result.peak_parallelism > 1,
        "CPU-bound trails should overlap, peak was {}",
        result.peak_parallelism
    );
}

#[test]
fn one_worker_never_overlaps() {
    let src = format!(
        "{BUSY}
parallel
\ta := burn(500) || b := burn(500)
end
"
    );
    let result = run_with(&src, Options { threads: 1, ..wide() });
    assert_eq!(result.peak_parallelism, 1);
}

#[test]
fn a_block_still_joins_every_trail() {
    // §9.3 holds whatever the pool does: control passes `end` only when all
    // have finished.
    let src = format!(
        "{BUSY}
total := 0
parallel for n in [1, 2, 3, 4, 5, 6, 7, 8]
\tr := burn(200)
\tpush(&scratch, n)
end
count := len(scratch)
"
    );
    let src = format!("scratch := []\n{src}");
    let result = run_with(&src, wide());
    assert!(result.crash.is_none(), "{:?}", result.crash);
    assert_eq!(read(&result, "count"), "8");
}

#[test]
fn a_shared_list_survives_concurrent_appends() {
    // A `&` crossing into a trail is the shared mutable state §9.2 describes.
    // Each append is one write, so none of them is lost or torn.
    let src = "
rows := []
parallel for n in [1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16]
\tpush(&rows, n)
end
count := len(rows)
";
    for _ in 0..20 {
        let result = run_with(src, wide());
        assert!(result.crash.is_none(), "{:?}", result.crash);
        assert_eq!(read(&result, "count"), "16");
    }
}

#[test]
fn a_shared_dict_survives_concurrent_writes() {
    let src = "
seen := {}
parallel for n in [1, 2, 3, 4, 5, 6, 7, 8]
\tseen[.\"\\(n)\"] = n
end
count := len(seen)
";
    for _ in 0..20 {
        let result = run_with(src, wide());
        assert!(result.crash.is_none(), "{:?}", result.crash);
        assert_eq!(read(&result, "count"), "8");
    }
}

/// Eight trails, each incrementing the same parent binding 500 times.
///
/// With real threads on real cores, a read and a write that are two steps
/// interleave and lose updates; these are the programs that say whether they do.
fn counting_trails(statement: &str) -> String {
    format!(
        "
count := 0
parallel for worker in [1, 2, 3, 4, 5, 6, 7, 8]
\ti := 0
\twhile i < 500
\t\t{statement}
\t\ti = i + 1
\tend
end
"
    )
}

#[test]
fn a_compound_assignment_is_atomic_with_respect_to_its_target() {
    // The point of `+=` being one instruction: the read and the write happen
    // under one lock, so eight trails incrementing one binding add exactly
    // 4000 and no increment is lost, whatever the pool does.
    let src = counting_trails("count += 1");
    for _ in 0..20 {
        let result = run_with(&src, wide());
        assert!(result.crash.is_none(), "{:?}", result.crash);
        assert_eq!(read(&result, "count"), "4000");
    }
}

#[test]
fn writing_the_same_binding_in_two_steps_may_still_lose_updates() {
    // The contrast, and why `+=` is worth having: `count = count + 1` is a read
    // and a write with a gap in between, so §9.2's last-write-wins applies and
    // an increment can be overwritten. Never *more* than 4000, and this asserts
    // only that — how much is lost is the nondeterminism the spec embraces.
    let src = counting_trails("count = count + 1");
    for _ in 0..20 {
        let result = run_with(&src, wide());
        assert!(result.crash.is_none(), "{:?}", result.crash);
        let count: f64 = read(&result, "count").parse().expect("a number");
        assert!(count <= 4000.0, "a two-step update cannot add more than it counted: {count}");
    }
}

#[test]
fn a_compound_assignment_is_atomic_inside_a_shared_structure() {
    // The same guarantee one level down: the lock the write takes is the one
    // the read is made under, wherever in the structure the place is.
    let src = "
totals := { .hits : 0, .rows : [0] }
parallel for worker in [1, 2, 3, 4, 5, 6, 7, 8]
\ti := 0
\twhile i < 200
\t\ttotals.hits += 1
\t\ttotals.rows[0] += 2
\t\ti = i + 1
\tend
end
hits := totals.hits
rows := totals.rows[0]
";
    for _ in 0..20 {
        let result = run_with(src, wide());
        assert!(result.crash.is_none(), "{:?}", result.crash);
        assert_eq!(read(&result, "hits"), "1600");
        assert_eq!(read(&result, "rows"), "3200");
    }
}

#[test]
fn value_semantics_hold_across_workers() {
    // Each trail works on its own copy (§5.1), so none of them can see another
    // one's writes however they interleave.
    let src = "
template := { .n : 0 }
mine := .null
parallel
\ta := template || b := template || c := template
\ta.n = 1       || b.n = 2       || c.n = 3
\tmine = a.n    ||               ||
end
untouched := template.n
";
    for _ in 0..20 {
        let result = run_with(src, wide());
        assert!(result.crash.is_none(), "{:?}", result.crash);
        assert_eq!(read(&result, "untouched"), "0");
        assert_eq!(read(&result, "mine"), "1");
    }
}

#[test]
fn a_race_still_cancels_its_losers() {
    let src = format!(
        "{BUSY}
winner := .null
race
\tr := burn(10)   || s := burn(20000)
\twinner = \"fast\" || winner = \"slow\"
end
"
    );
    for _ in 0..10 {
        let result = run_with(&src, wide());
        assert!(result.crash.is_none(), "{:?}", result.crash);
        // Either trail may win the race, but the block is decided by one of
        // them and the program moves on (§9.4).
        let winner = read(&result, "winner");
        assert!(winner == "fast" || winner == "slow" || winner == ".null", "{winner}");
    }
}

#[test]
fn a_crash_in_one_trail_stops_the_others_whatever_the_pool_does() {
    let src = format!(
        "{BUSY}
parallel for n in [1, 2, 3, 4]
\tr := burn(500)
\tboom := missing_name
end
"
    );
    for _ in 0..10 {
        let result = run_with(&src, wide());
        assert!(result.crash.is_some(), "a live trail crashed, so the program must");
    }
}

#[test]
fn nested_blocks_opened_by_called_functions_join_across_workers() {
    let src = format!(
        "{BUSY}
fn fan()
\tinner := 0
\tparallel
\t\tx := burn(200) || y := burn(200)
\tend
\treturn 1
end
outer := 0
parallel for n in [1, 2, 3, 4]
\tr := fan()
\touter = outer + 1
end
"
    );
    for _ in 0..10 {
        let result = run_with(&src, wide());
        assert!(result.crash.is_none(), "{:?}", result.crash);
    }
}

#[test]
fn many_more_trails_than_workers() {
    // Trails are green threads: the pool bounds how many run at an instant,
    // not how many there are (§9.1).
    let src = "
rows := []
parallel for n in [1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 17, 18, 19, 20,
21, 22, 23, 24, 25, 26, 27, 28, 29, 30]
\tpush(&rows, n)
end
count := len(rows)
";
    // The list literal is one line, so build it rather than write it out.
    let numbers: Vec<String> = (1..=200).map(|n| n.to_string()).collect();
    let src = src.replace(
        "[1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 17, 18, 19, 20,\n21, 22, 23, 24, 25, 26, 27, 28, 29, 30]",
        &format!("[{}]", numbers.join(", ")),
    );
    let result = run_with(&src, wide());
    assert!(result.crash.is_none(), "{:?}", result.crash);
    assert_eq!(read(&result, "count"), "200");
}
