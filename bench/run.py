#!/usr/bin/env python3
"""Compare Hydra against CPython on synthetic and practical workloads.

Each case is a pair of programs, `cases/<name>.hy` and `cases/<name>.py`, that
do the same work and print the same result. This script builds the release
interpreter, generates deterministic input files, runs both programs several
times, checks that their outputs agree, and reports wall time and peak memory.

    python3 bench/run.py                    # everything, default sizes
    python3 bench/run.py --quick            # smaller inputs, one run each
    python3 bench/run.py -k list -k csv     # only cases whose name matches
    python3 bench/run.py --scale 2          # twice the work per case
    python3 bench/run.py --json out.json    # also write raw results
"""

from __future__ import annotations

import argparse
import json
import math
import os
import random
import shutil
import statistics
import subprocess
import sys
import tempfile
import time
from dataclasses import dataclass, field
from pathlib import Path
from typing import Callable

BENCH = Path(__file__).resolve().parent
ROOT = BENCH.parent
CASES = BENCH / "cases"


@dataclass
class Case:
    name: str
    kind: str  # "baseline", "synthetic" or "practical"
    what: str
    # Arguments for both programs, given the scale and the fixture directory.
    args: Callable[[float, Path], list[str]]
    # A short label for the amount of work, shown in the table.
    size: Callable[[float], str] = lambda scale: ""


def count(base: int) -> Callable[[float], int]:
    return lambda scale: max(1, round(base * scale))


def counted(base: int) -> tuple[Callable, Callable]:
    """A case whose only argument is an iteration count that scales linearly."""
    n = count(base)
    return (lambda scale, data: [str(n(scale))]), (lambda scale: f"n={n(scale):,}")


def fib_n(scale: float) -> int:
    # fib(n) does ~1.618^n calls, so scaling the work means adding to n.
    return max(1, 25 + round(math.log(scale) / math.log(1.618)))


def matmul_n(scale: float) -> int:
    # n^3 multiply-adds.
    return max(2, round(70 * scale ** (1 / 3)))


def fixture(name: str) -> Callable[[float, Path], list[str]]:
    return lambda scale, data: [str(data / name)]


def fixture_size(name: str) -> Callable[[float], str]:
    return lambda scale: f"{FIXTURE_LINES[name](scale):,} {FIXTURE_UNITS[name]}"


FIXTURE_LINES = {
    "text.txt": count(200_000),
    "data.csv": count(200_000),
    "records.json": count(40_000),
    "logs.txt": count(200_000),
    "edges.txt": count(200_000),
    "config.ini": count(20_000),
    "events.csv": count(300_000),
}
FIXTURE_UNITS = {
    "text.txt": "lines",
    "data.csv": "rows",
    "records.json": "records",
    "logs.txt": "requests",
    "edges.txt": "edges",
    "config.ini": "sections",
    "events.csv": "events",
}

CASES_LIST: list[Case] = [
    Case("startup", "baseline", "Start the interpreter and print one number", lambda s, d: []),
    Case("loop_arith", "synthetic", "Integer arithmetic in a while loop", *counted(2_000_000)),
    Case("fib_recursive", "synthetic", "Naive recursive Fibonacci: function calls",
         lambda s, d: [str(fib_n(s))], lambda s: f"fib({fib_n(s)})"),
    Case("list_push", "synthetic", "Append to a list", *counted(1_000_000)),
    # list::range refuses more than 1,000,000 elements, so this cannot scale past 1.
    Case("list_iter", "synthetic", "Iterate a list with for-in and sum a filter", *counted(1_000_000)),
    Case("list_index", "synthetic", "Read-modify-write list elements by index", *counted(20_000)),
    Case("list_pipeline", "synthetic", "map/filter/sum over a range", *counted(300_000)),
    Case("list_sort", "synthetic", "Generate pseudo-random numbers, then sort", *counted(300_000)),
    Case("string_build", "synthetic", "Grow a string with += and interpolation", *counted(30_000)),
    Case("dict_count", "synthetic", "Count occurrences under 1,000 dynamic keys", *counted(100_000)),
    Case("matmul", "synthetic", "Dense matrix multiply with nested lists",
         lambda s, d: [str(matmul_n(s))], lambda s: f"{matmul_n(s)}x{matmul_n(s)}"),
    Case("read_lines", "practical", "Read a large text file and walk its lines",
         fixture("text.txt"), fixture_size("text.txt")),
    Case("grep", "practical", "Count lines containing a substring",
         fixture("text.txt"), fixture_size("text.txt")),
    # Reads the whole file but counts only the first lines: Hydra's dict lookup
    # is linear in its size, so the full file would take minutes.
    Case("word_count", "practical", "Split lines into words and count frequencies",
         lambda s, d: [str(d / "text.txt"), str(count(3_000)(s))],
         lambda s: f"{count(3_000)(s):,} lines"),
    Case("csv_aggregate", "practical", "Parse CSV rows and sum a column per category",
         fixture("data.csv"), fixture_size("data.csv")),
    Case("json_records", "practical", "Parse a JSON array, aggregate fields, re-encode",
         fixture("records.json"), fixture_size("records.json")),
    Case("write_lines", "practical", "Format lines, write a file, read it back",
         lambda s, d: [str(count(300_000)(s)), str(d / "out.txt")],
         lambda s: f"{count(300_000)(s):,} lines"),
    Case("log_stats", "practical", "Access log: requests per status, bytes per path, slowest path",
         fixture("logs.txt"), fixture_size("logs.txt")),
    Case("sessions", "practical", "Sort requests by client and time, then split into sessions",
         fixture("logs.txt"), fixture_size("logs.txt")),
    Case("top_words", "practical", "Word frequencies, then the ten most common by sorting",
         fixture("text.txt"), fixture_size("text.txt")),
    Case("json_groups", "practical", "Group JSON records by a nested field and aggregate",
         fixture("records.json"), fixture_size("records.json")),
    Case("report", "practical", "Render matching JSON records as a text table",
         fixture("records.json"), fixture_size("records.json")),
    Case("ini_parse", "practical", "Parse an INI file into sections and typed values",
         fixture("config.ini"), fixture_size("config.ini")),
    Case("graph_bfs", "practical", "Read an edge list, build adjacency lists, breadth-first search",
         fixture("edges.txt"), fixture_size("edges.txt")),
    Case("inventory", "practical", "Apply order events to per-item records through helper functions",
         fixture("events.csv"), fixture_size("events.csv")),
]


# --- fixtures ----------------------------------------------------------------

SYLLABLES = ["ka", "lo", "mi", "ne", "ru", "ta", "shi", "po", "ve", "do", "an", "el", "or", "un", "is"]
CATEGORIES = ["books", "music", "garden", "tools", "toys", "food", "sports", "office"]


def vocabulary(rng: random.Random, size: int) -> list[str]:
    words: set[str] = set()
    while len(words) < size:
        words.add("".join(rng.choice(SYLLABLES) for _ in range(rng.randint(1, 4))))
    return sorted(words)


def write_text(path: Path, lines: int) -> None:
    rng = random.Random(1)
    words = vocabulary(rng, 20_000)
    # Zipf-like weights, so a few words are very common, as in real text.
    weights = [1 / (rank + 1) for rank in range(len(words))]
    with path.open("w", encoding="utf-8") as f:
        for _ in range(lines):
            line = rng.choices(words, weights, k=rng.randint(4, 16))
            if rng.random() < 0.03:
                line.insert(rng.randrange(len(line) + 1), "error")
            f.write(" ".join(line) + "\n")


def write_csv(path: Path, rows: int) -> None:
    rng = random.Random(2)
    with path.open("w", encoding="utf-8") as f:
        f.write("id,category,amount,flag,note\n")
        for i in range(rows):
            note = "".join(rng.choice(SYLLABLES) for _ in range(rng.randint(2, 6)))
            f.write(f"{i},{rng.choice(CATEGORIES)},{rng.randint(1, 50_000)},{int(rng.random() < 0.2)},{note}\n")


def write_json(path: Path, records: int) -> None:
    rng = random.Random(3)
    data = [
        {
            "id": i,
            "name": "".join(rng.choice(SYLLABLES) for _ in range(rng.randint(2, 5))),
            "score": rng.randint(0, 1000),
            "active": rng.random() < 0.6,
            "tags": rng.sample(CATEGORIES, rng.randint(0, 4)),
            "owner": {"id": rng.randint(1, 500), "team": rng.choice(CATEGORIES)},
        }
        for i in range(records)
    ]
    path.write_text(json.dumps(data, separators=(",", ":")), encoding="utf-8")


METHODS = ["GET"] * 7 + ["POST"] * 2 + ["PUT", "DELETE"]
STATUSES = [200] * 14 + [201, 204, 301, 304, 304, 400, 403, 404, 404, 500]


def write_logs(path: Path, requests: int) -> None:
    rng = random.Random(4)
    clients = [f"10.{rng.randint(0, 255)}.{rng.randint(0, 255)}.{rng.randint(1, 254)}" for _ in range(3_000)]
    paths = [f"/api/{rng.choice(CATEGORIES)}/{i}" for i in range(400)]
    t = 1_700_000_000
    with path.open("w", encoding="utf-8") as f:
        for _ in range(requests):
            t += rng.randint(0, 3)
            f.write(f"{t} {rng.choice(clients)} {rng.choice(METHODS)} {rng.choice(paths)} "
                    f"{rng.choice(STATUSES)} {rng.randint(0, 50_000)} {rng.randint(1, 2_000)}\n")


def write_edges(path: Path, edges: int) -> None:
    rng = random.Random(5)
    nodes = max(2, edges // 4)
    with path.open("w", encoding="utf-8") as f:
        f.write(f"{nodes} {edges}\n")
        for _ in range(edges):
            f.write(f"{rng.randrange(nodes)} {rng.randrange(nodes)}\n")


def write_ini(path: Path, sections: int) -> None:
    rng = random.Random(6)
    with path.open("w", encoding="utf-8") as f:
        f.write("; generated configuration\n")
        for i in range(sections):
            f.write(f"\n[service-{i}]\n")
            if rng.random() < 0.3:
                f.write("# owned by the platform team\n")
            f.write(f"name = {''.join(rng.choice(SYLLABLES) for _ in range(3))}\n")
            f.write(f"port = {rng.randint(1024, 65535)}\n")
            f.write(f"enabled = {rng.choice(['true', 'false'])}\n")
            f.write(f"  timeout   =   {rng.randint(1, 120)}  \n")
            f.write(f"url = http://{rng.choice(CATEGORIES)}.local/path?a=b\n")
            for k in range(rng.randint(0, 4)):
                f.write(f"extra{k} = {rng.choice(SYLLABLES)}\n")


def write_events(path: Path, events: int) -> None:
    rng = random.Random(7)
    kinds = ["order"] * 6 + ["restock"] * 3 + ["cancel"]
    with path.open("w", encoding="utf-8") as f:
        f.write("kind,sku,qty\n")
        for _ in range(events):
            f.write(f"{rng.choice(kinds)},sku-{rng.randrange(2_000)},{rng.randint(1, 20)}\n")


WRITERS = {
    "text.txt": write_text,
    "data.csv": write_csv,
    "records.json": write_json,
    "logs.txt": write_logs,
    "edges.txt": write_edges,
    "config.ini": write_ini,
    "events.csv": write_events,
}


def prepare_fixtures(data: Path, scale: float, needed: set[str]) -> None:
    data.mkdir(parents=True, exist_ok=True)
    for name in sorted(needed):
        size = FIXTURE_LINES[name](scale)
        # Regenerate only when the size changed; generation is deterministic.
        stamp = data / f"{name}.size"
        path = data / name
        if path.exists() and stamp.exists() and stamp.read_text() == str(size):
            continue
        started = time.perf_counter()
        WRITERS[name](path, size)
        stamp.write_text(str(size))
        mb = path.stat().st_size / 1e6
        print(f"  generated {name}: {size:,} {FIXTURE_UNITS[name]}, {mb:.1f} MB "
              f"({time.perf_counter() - started:.1f}s)", file=sys.stderr)


# --- running -----------------------------------------------------------------

@dataclass
class Run:
    seconds: float
    rss: int  # peak resident set size in bytes
    output: str
    error: str
    code: int


@dataclass
class Result:
    case: Case
    size: str
    hydra: list[Run] = field(default_factory=list)
    python: list[Run] = field(default_factory=list)
    skipped: str = ""

    def median(self, runs: list[Run]) -> float:
        return statistics.median(r.seconds for r in runs)

    def ok(self) -> bool:
        return all(r.code == 0 for r in self.hydra + self.python) and agree(
            self.hydra[0].output, self.python[0].output
        )


def run_once(command: list[str], timeout: float) -> Run:
    with tempfile.TemporaryFile() as out, tempfile.TemporaryFile() as err:
        started = time.perf_counter()
        process = subprocess.Popen(command, stdout=out, stderr=err, cwd=ROOT)
        deadline = started + timeout
        # wait4 reports this child's own peak memory, not the maximum over all
        # children the way getrusage(RUSAGE_CHILDREN) would.
        while True:
            pid, status, usage = os.wait4(process.pid, os.WNOHANG)
            if pid:
                break
            if time.perf_counter() > deadline:
                process.kill()
                _, status, usage = os.wait4(process.pid, 0)
                elapsed = time.perf_counter() - started
                return Run(elapsed, 0, "", f"timed out after {timeout:.0f}s", -1)
            time.sleep(0.0005)
        elapsed = time.perf_counter() - started
        process.returncode = os.waitstatus_to_exitcode(status)
        # macOS reports bytes, Linux kibibytes.
        rss = usage.ru_maxrss if sys.platform == "darwin" else usage.ru_maxrss * 1024
        out.seek(0)
        err.seek(0)
        return Run(elapsed, rss, out.read().decode(errors="replace").strip(),
                   err.read().decode(errors="replace").strip(), process.returncode)


def agree(left: str, right: str) -> bool:
    """Outputs match token by token, numbers within floating-point tolerance."""
    a, b = left.split(), right.split()
    if len(a) != len(b):
        return False
    for x, y in zip(a, b):
        if x == y:
            continue
        try:
            if not math.isclose(float(x), float(y), rel_tol=1e-9, abs_tol=1e-9):
                return False
        except ValueError:
            return False
    return True


# --- reporting ---------------------------------------------------------------

def seconds(value: float) -> str:
    return f"{value * 1000:.0f} ms" if value < 1 else f"{value:.2f} s"


def megabytes(value: int) -> str:
    return f"{value / 1e6:.0f} MB" if value else "-"


def ratio(result: Result) -> float:
    return result.median(result.hydra) / result.median(result.python)


def table(rows: list[list[str]], right: set[int]) -> str:
    widths = [max(len(row[i]) for row in rows) for i in range(len(rows[0]))]
    lines = []
    for index, row in enumerate(rows):
        cells = [cell.rjust(widths[i]) if i in right else cell.ljust(widths[i]) for i, cell in enumerate(row)]
        lines.append("  ".join(cells).rstrip())
        if index == 0:
            lines.append("  ".join("-" * w for w in widths))
    return "\n".join(lines)


def report(results: list[Result]) -> str:
    rows = [["case", "kind", "size", "hydra", "python", "hydra/python", "hydra mem", "python mem", "check"]]
    for r in results:
        if r.skipped:
            rows.append([r.case.name, r.case.kind, r.size, "-", "-", "-", "-", "-", r.skipped])
            continue
        check = "ok" if r.ok() else "MISMATCH"
        rows.append([
            r.case.name, r.case.kind, r.size,
            seconds(r.median(r.hydra)), seconds(r.median(r.python)), f"{ratio(r):.1f}x",
            megabytes(max(x.rss for x in r.hydra)), megabytes(max(x.rss for x in r.python)), check,
        ])
    out = [table(rows, right={3, 4, 5, 6, 7})]
    for kind in ("synthetic", "practical"):
        ratios = [ratio(r) for r in results if r.case.kind == kind and not r.skipped and r.ok()]
        if ratios:
            mean = math.exp(statistics.fmean(math.log(x) for x in ratios))
            out.append(f"geometric mean hydra/python, {kind}: {mean:.1f}x over {len(ratios)} cases")
    out.append("Times are median wall-clock per process, startup included; >1x means Hydra is slower.")
    return "\n".join(out)


def mismatches(results: list[Result]) -> str:
    out = []
    for r in results:
        if r.skipped or r.ok():
            continue
        out.append(f"\n{r.case.name}:")
        for label, runs in (("hydra", r.hydra), ("python", r.python)):
            run = runs[0]
            text = run.output if run.code == 0 else f"exit {run.code}: {run.error}"
            out.append(f"  {label}: {text[:500]}")
    return "\n".join(out)


# --- main --------------------------------------------------------------------

def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("-k", "--filter", action="append", default=[],
                        help="run only cases whose name contains this; repeatable")
    parser.add_argument("--scale", type=float, default=1.0, help="multiply the work in every case (default 1)")
    parser.add_argument("--repeat", type=int, default=3, help="runs per program; the median is reported (default 3)")
    parser.add_argument("--quick", action="store_true", help="shorthand for --scale 0.1 --repeat 1")
    parser.add_argument("--timeout", type=float, default=300, help="seconds before a run is abandoned (default 300)")
    parser.add_argument("--python", default=sys.executable, help="Python interpreter to compare against")
    parser.add_argument("--hydra", help="Hydra binary; by default build and use target/release/hydra")
    parser.add_argument("--no-build", action="store_true", help="do not run cargo build --release first")
    parser.add_argument("--data", type=Path, default=BENCH / ".data", help="where generated inputs are kept")
    parser.add_argument("--json", type=Path, help="also write raw results here")
    parser.add_argument("--list", action="store_true", help="list the cases and exit")
    args = parser.parse_args()

    if args.quick:
        args.scale, args.repeat = 0.1, 1
    if args.scale <= 0 or args.repeat < 1:
        parser.error("--scale must be positive and --repeat at least 1")

    selected = [c for c in CASES_LIST if not args.filter or any(f in c.name for f in args.filter)]
    if args.list:
        print(table([["case", "kind", "size", "what"]] +
                    [[c.name, c.kind, c.size(args.scale), c.what] for c in selected], right=set()))
        return 0
    if not selected:
        parser.error("no case matches the filter")

    hydra = args.hydra
    if not hydra:
        if not args.no_build:
            print("building release interpreter…", file=sys.stderr)
            subprocess.run(["cargo", "build", "--release", "--quiet"], cwd=ROOT, check=True)
        hydra = str(ROOT / "target" / "release" / "hydra")
    if not Path(hydra).exists() and not shutil.which(hydra):
        parser.error(f"no Hydra binary at {hydra}")

    needed = {name for c in selected for name in FIXTURE_LINES if str(args.data / name) in c.args(args.scale, args.data)}
    prepare_fixtures(args.data, args.scale, needed)

    version = subprocess.run([args.python, "--version"], capture_output=True, text=True).stdout.strip()
    print(f"hydra: {hydra}\npython: {version} ({args.python})\n"
          f"scale {args.scale:g}, {args.repeat} run(s) each\n", file=sys.stderr)

    # The first exec of a freshly built binary pays for loading and code
    # signing checks; keep that out of the first case.
    for warm in ([hydra, "run", str(CASES / "startup.hy")], [args.python, str(CASES / "startup.py")]):
        run_once(warm, args.timeout)

    results: list[Result] = []
    for case in selected:
        hy, py = CASES / f"{case.name}.hy", CASES / f"{case.name}.py"
        result = Result(case, case.size(args.scale))
        results.append(result)
        if not hy.exists() or not py.exists():
            result.skipped = "missing program"
            continue
        extra = case.args(args.scale, args.data)
        print(f"  {case.name:<16} {result.size}", end="", flush=True, file=sys.stderr)
        for _ in range(args.repeat):
            result.hydra.append(run_once([hydra, "run", str(hy), "--", *extra], args.timeout))
            result.python.append(run_once([args.python, str(py), *extra], args.timeout))
            if result.hydra[-1].code or result.python[-1].code:
                break
        state = "ok" if result.ok() else "MISMATCH"
        print(f"  {seconds(result.median(result.hydra))} vs {seconds(result.median(result.python))}  {state}",
              file=sys.stderr)

    print()
    print(report(results))
    detail = mismatches(results)
    if detail:
        print("\nmismatched or failed cases:" + detail)

    if args.json:
        args.json.write_text(json.dumps({
            "hydra": hydra, "python": version, "scale": args.scale, "repeat": args.repeat,
            "results": [
                {
                    "case": r.case.name, "kind": r.case.kind, "size": r.size, "skipped": r.skipped,
                    "ok": not r.skipped and r.ok(),
                    "hydra": [vars(x) for x in r.hydra], "python": [vars(x) for x in r.python],
                }
                for r in results
            ],
        }, indent=2))

    return 0 if all(r.skipped or r.ok() for r in results) else 1


if __name__ == "__main__":
    sys.exit(main())
