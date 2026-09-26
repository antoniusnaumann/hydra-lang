#!/usr/bin/env python3
"""Build the static reference: python3 docs/build.py."""
import argparse
from collections import defaultdict
from html.parser import HTMLParser
from urllib.parse import urlsplit
from html import escape
import json
from pathlib import Path
import re
import tomllib

from highlight import Highlighter
from content import ATOM_GROUPS, BUILTINS, CONTROL_ROWS, FS, LANGUAGE, MODULES

HERE = Path(__file__).resolve().parent
ROOT = HERE.parent
VERSION = tomllib.loads((ROOT / 'Cargo.toml').read_text())['package']['version']
SEARCH = []
OUTPUT = {}
HIGHLIGHTER = None
PAGES = [('index', 'Overview'), ('language', 'Language'), ('builtins', 'Builtins'), ('atoms', 'Atoms & handlers')] + [(name, name) for name in MODULES]


def inline(text):
    """Only inline code is supported; reference copy stays deliberately small."""
    return re.sub(r'`([^`]+)`', lambda m: '<code>' + m[1] + '</code>', escape(text))


def slug(text):
    return text.lower().replace(' ', '-')


def search(page, anchor, label, kind, description):
    SEARCH.append(dict(url=f'{page}.html' + (f'#{anchor}' if anchor else ''), label=label, kind=kind, description=description.replace('`', '')))


def table(headers, rows):
    return '<div class="table-wrap"><table><thead><tr>' + ''.join(f'<th scope="col">{escape(h)}</th>' for h in headers) + '</tr></thead><tbody>' + ''.join('<tr>' + ''.join(f'<td>{cell}</td>' for cell in row) + '</tr>' for row in rows) + '</tbody></table></div>'


def code(text):
    return f'<pre><code>{HIGHLIGHTER.render(text)}</code></pre>'


def heading(level, anchor, title):
    return f'<h{level} id="{anchor}">{escape(title)}<a class="anchor" href="#{anchor}" aria-label="Link to {escape(title)}">#</a></h{level}>'


def registry():
    # Read the literal info! records, not a second copy of the signatures.
    source = (ROOT / 'src/value.rs').read_text()
    string = r'"((?:\\.|[^"\\])*)"'
    pattern = r'info!\(\s*(None|[A-Z]+)\s*,\s*' + string + r'\s*,\s*' + string
    found = defaultdict(lambda: defaultdict(list))
    for module, name, signature in re.findall(pattern, source):
        signature = json.loads('"' + signature + '"')
        found['builtins' if module == 'None' else module.lower()][name].append(signature)
    for module, entries in [('builtins', BUILTINS)] + [(m, data[1]) for m, data in MODULES.items()]:
        if set(found[module]) != set(entries):
            raise SystemExit(f'Reference names differ from {module} registry: {set(found[module]) ^ set(entries)}')
    expected_modules = set(MODULES)
    module_block = re.search(r'pub const BUILTIN_MODULES:.*?= &\[(.*?)\];', source, re.S)[1]
    if set(re.findall(r'"([^"]+)"', module_block)) != expected_modules or set(found) != expected_modules | {'builtins'}:
        raise SystemExit('Standard modules need reference coverage')
    # Fail closed if a future native is missed by the literal reader.
    native_block = re.search(r'pub const NATIVES:.*?= &\[(.*?)\];', source, re.S)[1]
    assert len(re.findall(r'Native::\w+', native_block)) == sum(len(v) for group in found.values() for v in group.values()), 'A native signature needs documentation'
    return found


def page(name, title, description, body, sections):
    active = lambda key: ' aria-current="page"' if name == key else ''
    links = ''.join(f'<a href="{key}.html"{active(key)}>{escape(label)}</a>' for key, label in PAGES[:4])
    module_links = ''.join(f'<a href="{key}.html"{active(key)}><code>{key}</code></a>' for key in MODULES)
    local = ''.join(f'<a href="#{anchor}">{escape(label)}</a>' for anchor, label in sections)
    search(name, '', title, 'Page', description)
    html = f'''<!doctype html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<meta name="color-scheme" content="light dark">
<meta name="description" content="{escape(description, quote=True)}">
<title>{escape(title)} · Hydra</title>
<link rel="stylesheet" href="style.css">
<script src="search-index.js" defer></script>
<script src="search.js" defer></script>
</head>
<body>
<a class="skip" href="#main">Skip to content</a>
<header><div class="header-inner">
<a class="brand" href="index.html">hydra <span>{escape(VERSION)}</span></a>
<span class="header-label">Reference</span>
<div class="search">
<label class="sr-only" for="search-input">Search the reference</label>
<input id="search-input" type="search" placeholder="Search reference" autocomplete="off" spellcheck="false" aria-controls="search-results" aria-expanded="false">
<kbd aria-hidden="true">/</kbd>
<div id="search-panel" hidden><p id="search-status" role="status" aria-live="polite"></p><ul id="search-results"></ul></div>
</div>
</div></header>
<div class="layout">
<aside><nav aria-label="Reference navigation">{links}
<p class="nav-label">Standard modules</p>{module_links}
</nav><nav class="on-page" aria-label="On this page"><p class="nav-label">On this page</p>{local}</nav></aside>
<main id="main"><div class="page-title"><h1>{escape(title)}</h1></div>
<p class="intro">{inline(description)}</p>
{body}
<footer>Hydra {escape(VERSION)} · Reference</footer>
</main></div>
</body></html>
'''
    OUTPUT[f'{name}.html'] = html


def reference(name, title, intro, entries, signatures, note):
    sections, body = [], note
    groups = dict.fromkeys(v[0] for v in entries.values())
    for group in groups:
        anchor = "section-" + slug(group)
        sections.append((anchor, group))
        body += heading(2, anchor, group)
        for function, (category, result, description) in entries.items():
            if category != group:
                continue
            sections.append((function, function))
            prefix = name + '::' if name in MODULES else ''
            function_sigs = signatures[function]
            first, *rest = function_sigs
            body += f'<article class="function" id="{function}"><h3><code>{escape(prefix + first)}</code><a class="anchor" href="#{function}" aria-label="Link to {prefix + function}">#</a></h3>'
            body += ''.join(f'<div class="overload"><code>{escape(prefix + sig)}</code></div>' for sig in rest)
            body += f'<div class="result">→ <code>{escape(result)}</code></div><p>{inline(description)}</p></article>'
            search(name, function, prefix + function, 'Function', first + ' — ' + description)
    page(name, title, intro, body, sections)


def validate_links():
    class Links(HTMLParser):
        def __init__(self, text):
            super().__init__()
            self.ids, self.links = set(), []
            self.feed(text)

        def handle_starttag(self, tag, attrs):
            attrs = dict(attrs)
            if 'id' in attrs:
                if attrs['id'] in self.ids:
                    raise ValueError(f"Duplicate anchor: {attrs['id']}")
                self.ids.add(attrs['id'])
            for key in ('href', 'src'):
                if key in attrs:
                    self.links.append(attrs[key])

    pages = {name: Links(text) for name, text in OUTPUT.items() if name.endswith('.html')}
    for name, page in pages.items():
        for link in page.links:
            target = urlsplit(link)
            if target.scheme or target.netloc:
                continue
            path = target.path or name
            if path not in OUTPUT and not (HERE / path).is_file():
                raise ValueError(f'{name}: missing {link}')
            if target.fragment and (path not in pages or target.fragment not in pages[path].ids):
                raise ValueError(f'{name}: missing anchor {link}')


def main():
    global HIGHLIGHTER
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--check', action='store_true', help='validate without writing; fail if generated files are stale')
    args = parser.parse_args()
    SEARCH.clear()
    OUTPUT.clear()
    signatures = registry()
    HIGHLIGHTER = Highlighter(ROOT)
    directory = table(['Reference', 'Contents'], [
        ('<a href="language.html">Language</a>', 'Calls, continuation, values, functions, and trails.'),
        ('<a href="builtins.html">Builtins</a>', f'{len(BUILTINS)} global functions. No import required.'),
        ('<a href="atoms.html">Atoms &amp; handlers</a>', 'Control values, modes, and filesystem reasons.'),
        *[(f'<a href="{name}.html"><code>{name}</code></a>', data[0]) for name, data in MODULES.items()],
    ])
    body = heading(2, 'reference', 'Reference') + directory
    body += heading(2, 'example', 'A small example') + code('use fs\n\ntext, reason := fs::read "notes.txt", ""\nif reason != :null\n\tprint "No notes yet"\nend\n\nprint text')
    body += heading(2, 'control', 'Functions construct. Handlers act.')
    body += '<p><code>return(42)</code> constructs <code>[:return, 42]</code>. Assign it and it stays data. Leave it unconsumed in a function and that function returns <code>42</code>.</p>'
    body += code('request := return(42)  // data\n\nfn answer()\n\trequest             // handled here\nend\n\nprint answer()')
    body += '<p>The same rule connects <code>break</code>, <code>continue</code>, <code>reject</code>, <code>exit</code>, and <code>panic</code> to their control values. <a href="atoms.html#control">See the mapping →</a></p>'
    body += heading(2, 'notation', 'Notation')
    body += '<p><code>xs*</code> collects positional arguments. A standalone <code>*</code> makes following parameters named-only. <code>&amp;x</code> passes a reference. The arrow after a signature describes its result; it is not Hydra syntax.</p>'
    page('index', 'Hydra reference', 'A compact guide to the implemented language and standard library.', body, [('reference', 'Reference'), ('example', 'Example'), ('control', 'Control values'), ('notation', 'Notation')])

    note = '<p class="note">Ordinary, shadowable functions. <code>f := print</code> keeps the function; <code>::print</code> selects the builtin explicitly. <a href="language.html#calls">Call syntax →</a></p>'
    note += '<p>Control functions only construct values. Consumed results stay data; unconsumed results reach a handler. Loop and return requests crash when no local handler exists. <a href="atoms.html#control">Handlers →</a></p>'
    reference('builtins', 'Builtins', 'Global functions, available without an import.', BUILTINS, signatures['builtins'], note)

    note = '<p class="note"><code>use fs</code>, then <code>fs::read(...)</code>. Alternatively, <code>use fs as files</code> or <code>use fs as *</code>.</p>'
    note += '<p><code>size</code>, <code>modified</code>, <code>read</code>, <code>lines</code>, and <code>list</code> return <code>value, reason</code>. Success has reason <code>:null</code>. Their fallback overloads return <code>fallback, reason</code> on failure; calls without a fallback crash. A single binding keeps only the value. <a href="atoms.html#fs-reasons">Failure reasons →</a></p>'
    reference('fs', 'fs', 'Filesystem paths, inspection, reading, and writing.', FS, signatures['fs'], note)

    for name, (summary, entries, usage, example) in MODULES.items():
        if name == 'fs':
            continue
        note = '<p class="note">' + inline(f'use {name}, then `{name}::name(...)`. Imports support aliases and `as *`.') + '</p>'
        note += '<p>' + inline(usage) + '</p>'
        if name in ('io', 'time', 'http'):
            note += '<p>Waiting calls run off the scheduler so other trails can progress. Normal trail cancellation lets an in-flight call finish. Each waiting call currently uses an OS helper thread.</p>'
        if example:
            note += code(example)
        reference(name, name, summary, entries, signatures[name], note)

    body = heading(2, 'control', 'Control values')
    rows = []
    for function, value, scope, effect in CONTROL_ROWS:
        name = function.split('(')[0]
        rows.append((f'<a href="builtins.html#{name}"><code>{escape(function)}</code></a>', f'<code>{escape(value)}</code>', inline(scope + '. ' + effect)))
        search('atoms', 'control', ':' + name, 'Control atom', value + ' — ' + effect)
    body += table(['Constructor', 'Value', 'When unconsumed'], rows)
    body += '<p>Bare <code>:return</code> also returns <code>:null</code>; bare <code>:reject</code> also rejects. Bare <code>:exit</code> and <code>:panic</code> have no control effect: their handlers require two-element lists.</p>'
    body += '<p>Loop and return handlers belong to the current function. A helper must explicitly return a request for its caller to handle. An unconsumed return request in a trail has no function handler. <a href="language.html#handlers">Example →</a></p>'
    sections = [('control', 'Control values')]
    for anchor, title, items in ATOM_GROUPS:
        sections.append((anchor, title))
        body += heading(2, anchor, title)
        body += table(['Atom', 'Meaning'], [(f'<code id="{slug(atom[1:])}">{escape(atom)}</code>', inline(meaning)) for atom, meaning in items])
        for atom, meaning in items:
            search('atoms', slug(atom[1:]), atom, 'Atom', meaning)
    page('atoms', 'Atoms & handlers', 'Atoms are ordinary values. Functions and handlers recognize particular names and list shapes.', body, sections)

    body, sections = '', []
    for anchor, title, description, example in LANGUAGE:
        sections.append((anchor, title))
        body += heading(2, anchor, title) + f'<p>{inline(description)}</p>'
        if example:
            body += code(example)
        search('language', anchor, title, 'Language', description)
    page('language', 'Language', 'The rules you need to read and write Hydra.', body, sections)
    OUTPUT['search-index.js'] = 'window.HYDRA_SEARCH = ' + json.dumps(SEARCH, ensure_ascii=False, separators=(',', ':')) + ';\n'
    validate_links()
    stale = []
    for name, text in OUTPUT.items():
        path = HERE / name
        if args.check:
            if not path.exists() or path.read_text(encoding='utf-8') != text:
                stale.append(name)
        else:
            path.write_text(text, encoding='utf-8')
    if stale:
        raise SystemExit('Stale reference: ' + ', '.join(stale) + '. Run python3 docs/build.py')
    print(f'Built {len(PAGES)} pages, {len(BUILTINS) + sum(len(data[1]) for data in MODULES.values())} functions, {len(SEARCH)} search entries.')


if __name__ == '__main__':
    main()
