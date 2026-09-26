"""Regression coverage for semantic-token → HTML offsets and escaping."""
from html.parser import HTMLParser
from pathlib import Path
import unittest

from highlight import Highlighter


class Markup(HTMLParser):
    def __init__(self, source):
        super().__init__()
        self.text = ''
        self.spans = []
        self.feed(source)

    def handle_data(self, data):
        self.text += data

    def handle_starttag(self, tag, attrs):
        if tag == 'span':
            self.spans.append(dict(attrs)['class'])


class HighlightTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.highlighter = Highlighter(Path(__file__).resolve().parent.parent)

    def test_unicode_multiline_and_html_are_preserved(self):
        source = 'text := "é <script> & 😀\\nline"\nprint text // café <b>\n'
        rendered = self.highlighter.render(source)
        self.assertEqual(Markup(rendered).text, source)
        self.assertNotIn('<script>', rendered)
        self.assertIn('syntax-comment', Markup(rendered).spans)

    def test_atom_colon_and_control_text_are_separate(self):
        rendered = self.highlighter.render('request := :return\nreturn 42')
        self.assertIn('<span class="syntax-punctuation-delimiter">:</span>', rendered)
        self.assertIn('<span class="syntax-keyword-control">return</span>', rendered)
        self.assertEqual(Markup(rendered).text, 'request := :return\nreturn 42')


if __name__ == '__main__':
    unittest.main()
