#!/usr/bin/env python3
"""Exercise marked PR description updates; nothing is sent to GitHub."""
import importlib.util
from pathlib import Path
import unittest

spec = importlib.util.spec_from_file_location('comment', Path(__file__).with_name('update-resource-impact.py'))
comment = importlib.util.module_from_spec(spec)
spec.loader.exec_module(comment)


class PullRequestBody(unittest.TestCase):
    def test_append_and_replace_keep_all_author_text(self):
        author = '# My change\n\nKeep every letter, space, and newline.  \n'
        generated = '## Resource impact: PASS\n\n| measured | 42 |'
        updated = comment.replace_section(author, generated)
        self.assertTrue(updated.startswith(author + '\n'))
        self.assertEqual(updated.count(comment.START), 1)
        self.assertEqual(updated.count(comment.END), 1)
        self.assertEqual(comment.replace_section(updated, generated), updated)
        suffix = '\n\n## Reviewer notes\nKeep these too.\n'
        replaced = comment.replace_section(updated + suffix, '## Resource impact: REGRESSION')
        self.assertEqual(replaced, author + '\n' + comment.START + '\n'
                         '## Resource impact: REGRESSION\n' + comment.END + suffix)

    def test_refuse_broken_or_ambiguous_markers_and_reports_that_include_them(self):
        for broken in (comment.START, comment.END, comment.END + comment.START,
                       comment.START + comment.START + comment.END):
            with self.subTest(broken=broken), self.assertRaises(ValueError):
                comment.replace_section(broken, 'report')
        with self.assertRaises(ValueError):
            comment.replace_section('author text', 'report ' + comment.END)

    def test_do_not_truncate_the_author_body_to_fit_github(self):
        almost_full = 'é' * (comment.LIMIT // 2)
        with self.assertRaises(ValueError):
            comment.replace_section(almost_full, 'report')

    def test_only_a_current_open_pr_has_its_description_patched_once(self):
        head, base = 'a' * 40, 'b' * 40
        pr = {'head': {'sha': head}, 'base': {'sha': base}, 'state': 'open', 'body': 'Original'}
        writes = []

        def get():
            return pr.copy()

        def patch(body):
            writes.append(body)
            pr['body'] = body

        self.assertTrue(comment.update(get, patch, head, base, 'measured'))
        self.assertFalse(comment.update(get, patch, head, base, 'measured'))
        self.assertFalse(comment.update(get, patch, 'c' * 40, base, 'stale head'))
        self.assertFalse(comment.update(get, patch, head, 'c' * 40, 'stale base'))
        pr['state'] = 'closed'
        self.assertFalse(comment.update(get, patch, head, base, 'closed'))
        self.assertEqual(writes, ['Original\n\n' + comment.START + '\nmeasured\n' + comment.END])

    def test_an_author_edit_detected_between_reads_is_not_overwritten(self):
        head = 'a' * 40
        pr = {'head': {'sha': head}, 'state': 'open'}
        snapshots = iter([{**pr, 'body': 'Original'}, {**pr, 'body': 'The author added a line'}])
        writes = []
        self.assertFalse(comment.update(lambda: next(snapshots), writes.append, head, None, 'measured'))
        self.assertEqual(writes, [])


if __name__ == '__main__':
    unittest.main(verbosity=2)
