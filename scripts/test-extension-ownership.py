#!/usr/bin/env python3
"""Verify ownership against authenticated accounts and the current trusted base."""
import base64
import copy
import importlib.util
import json
from pathlib import Path
import unittest

spec = importlib.util.spec_from_file_location('ownership', Path(__file__).with_name('verify-extension-ownership.py'))
ownership = importlib.util.module_from_spec(spec)
spec.loader.exec_module(ownership)


def account(login, identifier):
    return {'login': login, 'id': identifier, 'type': 'User'}


def document(owner=10, contributors=(30,)):
    names = {10: 'owner', 20: 'maintainer', 30: 'contributor', 40: 'outsider'}
    return {'schema': 1, 'owner': {'login': names[owner], 'id': owner},
            'contributors': [{'login': names[value], 'id': value} for value in contributors]}


def blob(data):
    return {'type': 'file', 'encoding': 'base64', 'content': base64.b64encode(json.dumps(data).encode()).decode()}


class Ownership(unittest.TestCase):
    def fixture(self, existing=True, actor=10):
        names = {10: 'owner', 20: 'maintainer', 30: 'contributor', 40: 'outsider'}
        self.pr = {'number': 45, 'user': account(names[actor], actor)}
        self.submission = {'directory': 'extension/fly-me', 'id': 'fly-me'}
        self.commit = 'a' * 40
        self.reviews = []
        self.data = {
            'git/ref/heads/extension': {'object': {'sha': 'b' * 40}},
            'contents/extension?ref=' + 'b' * 40: [{'name': 'fly-me', 'type': 'dir'}] if existing else None,
            'contents/extension/fly-me?ref=' + 'b' * 40: [] if existing else None,
            'contents/extension/fly-me/ownership.json?ref=' + 'b' * 40: blob(document()) if existing else None,
            'contents/extension/fly-me/ownership.json?ref=' + self.commit: blob(document()),
            **{'users/' + login: account(login, identifier) for identifier, login in names.items()},
        }
        return self

    def check(self):
        return ownership.authorize(lambda path: copy.deepcopy(self.data[path]), self.pr,
                                   self.submission, self.commit, self.reviews)

    def approve(self, identifier):
        names = {10: 'owner', 20: 'maintainer', 30: 'contributor', 40: 'outsider'}
        self.reviews.append({'state': 'APPROVED', 'commit_id': self.commit,
                             'user': account(names[identifier], identifier)})

    def change(self, data):
        self.data['contents/extension/fly-me/ownership.json?ref=' + self.commit] = blob(data)

    def test_new_plugin_belongs_to_authenticated_submitter_not_display_author(self):
        self.fixture(existing=False)
        self.assertEqual(self.check()['owner']['id'], 10)
        self.pr['user'] = account('outsider', 40)
        with self.assertRaises(ValueError):
            self.check()
        self.approve(10)
        self.check()

    def test_owner_and_current_contributor_can_update_but_stranger_cannot(self):
        for actor in (10, 30):
            self.fixture(actor=actor).check()
        self.fixture(actor=40)
        with self.assertRaises(ValueError):
            self.check()
        self.approve(30)
        self.check()

    def test_only_owner_can_add_or_remove_contributors(self):
        for roster in ((), (30, 40)):
            self.fixture(actor=30).change(document(contributors=roster))
            with self.assertRaises(ValueError):
                self.check()
            self.approve(20)  # A store maintainer is not the plugin owner.
            with self.assertRaises(ValueError):
                self.check()
            self.approve(10)
            self.check()
        self.fixture().change(document(contributors=(30, 40)))
        self.check()

    def test_ownership_transfer_needs_existing_and_receiving_owner_consent(self):
        self.fixture().change(document(owner=40))
        with self.assertRaises(ValueError):
            self.check()
        self.approve(40)
        self.assertEqual(self.check()['owner']['id'], 40)
        self.fixture(actor=40).change(document(owner=40))
        with self.assertRaises(ValueError):
            self.check()
        self.approve(10)
        self.check()

    def test_self_declared_takeover_missing_registry_and_revoked_contributor_fail_closed(self):
        self.fixture(actor=40).change(document(owner=40, contributors=()))
        self.approve(40)
        with self.assertRaises(ValueError):
            self.check()
        self.fixture()
        self.data['contents/extension/fly-me/ownership.json?ref=' + 'b' * 40] = None
        with self.assertRaises(ValueError):
            self.check()
        self.fixture(actor=30)
        self.data['contents/extension/fly-me/ownership.json?ref=' + 'b' * 40] = blob(document(contributors=()))
        self.change(document(contributors=()))
        with self.assertRaises(ValueError):
            self.check()

    def test_forged_accounts_duplicate_roles_and_stale_or_bot_consent_fail(self):
        for change in ('forged-id', 'duplicate', 'stale', 'bot', 'dismissed'):
            with self.subTest(change=change):
                self.fixture(actor=40)
                self.approve(10)
                if change == 'forged-id':
                    data = document()
                    data['owner']['id'] = 40
                    self.change(data)
                elif change == 'duplicate':
                    self.change(document(contributors=(30, 30)))
                elif change == 'stale':
                    self.reviews[0]['commit_id'] = 'c' * 40
                elif change == 'bot':
                    self.reviews[0]['user']['type'] = 'Bot'
                else:
                    self.reviews[0]['state'] = 'DISMISSED'
                with self.assertRaises(ValueError):
                    self.check()

    def test_alternate_directory_cannot_claim_an_existing_stable_id(self):
        self.fixture(existing=False)
        self.data['contents/extension?ref=' + 'b' * 40] = [{'name': 'peersus-fly-me', 'type': 'dir'}]
        with self.assertRaises(ValueError):
            self.check()


if __name__ == '__main__':
    unittest.main(verbosity=2)
