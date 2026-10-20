# Benchmarks against Python

```sh
python3 bench/run.py            # build the release interpreter, run every case 3 times
python3 bench/run.py --quick    # a tenth of the work, one run each
python3 bench/run.py --list     # the cases and their sizes
python3 bench/run.py -k list -k csv --scale 2 --repeat 5 --json results.json
```

Each case is a pair, `cases/<name>.hy` and `cases/<name>.py`, written the same
way in both languages and printing the same result. The runner checks that the
outputs agree (numbers within floating-point tolerance), so a case that gets
faster by computing something else fails. The exit status is nonzero on any
mismatch or crash.

Reported times are the median wall-clock time per process, startup included;
`startup` measures that cost on its own. Memory is each process's peak RSS.
Ratios above 1x mean Hydra is slower.

**Synthetic:** integer loops, recursive calls, list append, iteration, indexed
read-modify-write, `list` pipelines, sorting, string building, dict counting,
and nested-list matrix multiply.

**Practical:** inputs are generated deterministically into `bench/.data/`
(ignored by git) and reused while their size is unchanged: a Zipf-distributed
text file (`read_lines`, `grep`, `word_count`), a CSV file (`csv_aggregate`),
and a JSON array of records (`json_records`). `write_lines` formats, writes, and
rereads a file. Python reads files line by line, as it idiomatically would;
Hydra uses `fs::lines`.

Sizes are chosen so the full suite finishes in a few minutes. `--scale`
multiplies the work in every case; `fib_recursive` and `matmul` adjust their
parameter so the work, not the parameter, scales. `list_iter` cannot exceed
scale 1 because `list::range` refuses more than 1,000,000 elements.

To add a case, write both programs, then add a `Case` entry in `run.py`.
