# Reference maintenance

When changing user-visible syntax, builtin or module behavior, control atoms,
handlers, or formatting rules, update the concise reference in `docs/content.py`
in the same change. Keep examples representative of the new behavior. Update
related executable regression tests when semantics change; prose cannot be
validated automatically.

Run `python3 docs/build.py` and commit the generated pages and search index with
their sources. Run `python3 docs/build.py --check` before finishing. It builds the
current compiler, parses all examples, generates highlighting from its semantic
tokens, checks builtin coverage and links, and rejects stale output. Do not edit
generated HTML directly or maintain a separate syntax-highlighting grammar.
