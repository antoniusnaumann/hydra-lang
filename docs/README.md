# Compact reference

Open `index.html` directly, or serve this directory. The site works offline and
has no browser dependencies. Regeneration requires Python 3.11+ and Rust/Cargo.

From the repository, run `direnv allow` once, then `docs`. It rebuilds the
reference, serves it on loopback, and opens your browser. Ctrl-C stops it.
Port 8765 is preferred; an occupied port automatically falls back to a free one.
Use `docs --port 9000` or `docs --no-open` when needed.

Without direnv, run `./bin/docs`. Build/check commands remain available:

```sh
python3 docs/build.py
python3 docs/build.py --check
python3 -m http.server 8765 --directory docs
```

Edit `content.py` for descriptions; `build.py` for page structure; `style.css`
for presentation. Function signatures come from `src/value.rs`. The builder
checks that every registered builtin has a description. Commit the generated
HTML and search index alongside their sources.

`--check` does not write files. CI runs it on every push and pull request: stale
output, missing builtin descriptions, invalid examples, and broken local links
fail the check. Examples are parsed, not executed (some intentionally refer to
illustrative names). Highlighting comes from a freshly built compiler's
`hydra tokens` output, including separate atom delimiters; there is no second
grammar or browser highlighting dependency.

Behavioral prose still needs review when semantics change. The repository's
`AGENTS.md` makes reference updates part of language changes. Run the highlighting
adapter tests with `python3 -m unittest discover -s docs -p 'test_*.py'`.

Search supports `/`, arrow keys, Enter, and Escape. Navigation and all reference
content remain available without JavaScript. Appearance follows the system’s
light or dark preference; narrow windows use a compact top navigation.
