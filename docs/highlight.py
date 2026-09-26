"""Static HTML from Hydra's own semantic tokens; no browser-side lexer."""
from html import escape
import json
from pathlib import Path
import subprocess
import tempfile


class Highlighter:
    def __init__(self, root):
        build = subprocess.run(
            ['cargo', 'build', '--locked', '--bin', 'hydra', '--message-format=json'],
            cwd=root, check=True, capture_output=True, text=True,
        )
        self.binary = next(
            item['executable'] for line in build.stdout.splitlines()
            if (item := json.loads(line)).get('executable')
            and item.get('target', {}).get('name') == 'hydra'
        )

    def render(self, source):
        # Parsing is separate: semantic_tokens deliberately tolerates incomplete code.
        subprocess.run([self.binary, 'fmt', '-'], input=source, text=True,
                       check=True, stdout=subprocess.DEVNULL)
        with tempfile.TemporaryDirectory(prefix='hydra-docs-') as directory:
            path = Path(directory) / 'example.hy'
            path.write_text(source, encoding='utf-8')
            tokens = subprocess.run([self.binary, 'tokens', str(path)],
                                    check=True, capture_output=True, text=True).stdout
        offsets, offset = [], 0
        for line in source.splitlines(keepends=True):
            offsets.append(offset)
            offset += len(line)
        pieces, cursor = [], 0
        for record in tokens.splitlines():
            position, length, scope = record.split()
            line, column = map(int, position.split(':'))
            start = offsets[line - 1] + column - 1
            end = start + int(length)
            if not cursor <= start <= end <= len(source):
                raise ValueError(f'Invalid semantic token: {record}')
            pieces.append(escape(source[cursor:start]))
            pieces.append(f'<span class="syntax-{escape(scope.replace(".", "-"))}">{escape(source[start:end])}</span>')
            cursor = end
        pieces.append(escape(source[cursor:]))
        return ''.join(pieces)
