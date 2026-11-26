#!/usr/bin/env python3
"""Exercise architecture extraction and output without relying on a pre-generated map."""
import contextlib
import io
from html.parser import HTMLParser
import json
import os
from pathlib import Path
import re
import runpy
import subprocess
import sys
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[1]
SCRIPT = ROOT / 'scripts/gen-architecture-map.py'
OUT = ROOT / 'target/architecture-map'
EXTENSIONS = Path(os.environ['PEERSUS_ARCHITECTURE_TEST_EXTENSIONS']) if 'PEERSUS_ARCHITECTURE_TEST_EXTENSIONS' in os.environ else None


class ArchitectureMap(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        OUT.mkdir(parents=True, exist_ok=True)
        scratch = tempfile.TemporaryDirectory(dir=OUT)
        cls.addClassCleanup(scratch.cleanup)
        cls.output_dir = Path(scratch.name)
        generate = runpy.run_path(str(SCRIPT))['output']
        generate.__globals__['OUT'] = cls.output_dir
        with contextlib.redirect_stdout(io.StringIO()):
            if generate(check=False, extensions=EXTENSIONS) != 0:
                raise AssertionError('Fixture generation failed')

    def data(self):
        return json.loads((self.output_dir / 'map.json').read_bytes())

    def test_views_resolve_real_source_and_documentation(self):
        data = self.data()
        self.assertEqual([v['id'] for v in data['views']], ['system', 'code', 'broker', 'send', 'delivery'])
        nodes = {n['id']: n for n in data['nodes']}
        self.assertEqual(len(nodes), len(data['nodes']))
        for v in data['views']:
            for lane in v['lanes']:
                self.assertTrue(set(lane['nodes']) <= nodes.keys())
        for e in data['edges']:
            self.assertIn(e['from'], nodes)
            self.assertIn(e['to'], nodes)
        for n in nodes.values():
            with self.subTest(node=n['id']):
                source = n['source']
                self.assertFalse(Path(source['path']).is_absolute())
                self.assertGreaterEqual(source['line'], 1)
                if source['path'].startswith('extension/') and EXTENSIONS is not None:
                    base = EXTENSIONS if (EXTENSIONS / 'extension').is_dir() else EXTENSIONS.parent
                else:
                    base = ROOT
                path = base / source['path']
                self.assertTrue(path.is_file())
                if n.get('doc'):
                    doc = n['doc']
                    self.assertIn(doc['heading'], (ROOT / doc['path']).read_text())
        self.assertIn('stream:plugin', nodes)
        streams = (ROOT / 'crates/peersus-net/src/streams.rs').read_text()
        self.assertEqual('stream:plugin-visit' in nodes, bool(re.search(r'^\s+PluginVisit\s*\{', streams, re.M)))
        self.assertIn('stream:unknown', nodes)
        self.assertIn('role:android-wasm', nodes)
        self.assertIn('role:desktop-canvas', nodes)
        self.assertIn('target:peersus-plugin-runtime:peersus-wasm-host', nodes)

    def test_broker_resolves_exact_versions_and_permission_scope(self):
        nodes = {n['id']: n for n in self.data()['nodes']}
        self.assertEqual({n['label'] for n in nodes.values() if n['kind'] == 'Extension'},
                         {'Clipboard', 'Control', 'Location', 'Monitor', 'Test', 'Upload'})
        method = nodes['member:host:peersus.peers:1:method:list']
        self.assertEqual(method['permission'], 'peers.watch')
        self.assertEqual(method['scope'], 'Per paired device')
        self.assertEqual(nodes['member:host:peersus.config:1:method:get']['scope'], 'Caller only')
        if nodes['extension:location'].get('execution'):
            self.assertIn('member:location:location.position:1:method:receive', nodes)
        else:
            self.assertIn('No manifest', nodes['extension:location']['note'])
            self.assertNotIn('member:location:location.position:1:method:receive', nodes)

    def test_floorplan_projection_preserves_evidence_and_relationships(self):
        graph = json.loads((self.output_dir / 'graph.json').read_bytes())
        self.assertEqual(graph['schema'], 'floorplan/graph@1')
        items = {'n:'+node['id']: node for node in graph['nodes']}
        self.assertEqual(len(items), len(graph['nodes']))
        by_inventory = {n['inventoryId']: n for n in items.values() if 'inventoryId' in n}
        inventory = self.data()
        for node in inventory['nodes']:
            mapped = by_inventory[node['id']]
            self.assertEqual(mapped['source'], node['source'])
            self.assertEqual(mapped['doc'], node['doc'])
        self.assertEqual(
            {f['id'] for f in graph['flows']}, {'send', 'control-visit', 'extension-install', 'pairing'})
        for group in [flow['steps'] for flow in graph['flows']] + [graph['tour']]:
            self.assertGreaterEqual(len(group), 3)
            for step in group:
                self.assertIn(step['ref'], items)
        root = graph['peersus']['boards']['root']
        self.assertEqual(len(root['cards']), 9)
        self.assertEqual(len(set(root['cards'])), 9)
        for ref in root['cards']:
            self.assertIsNone(items[ref]['parent'])
        for view, _label in graph['peersus']['views']:
            board = graph['peersus']['boards'][view]
            self.assertTrue(board['read'])
            self.assertLessEqual(set(board['cards']), items.keys())
        extracted = {(n['inventoryId'], ref) for ref, n in items.items() if 'inventoryId' in n}
        origin = dict(extracted)
        expected = {(origin[e['from']], origin[e['to']], e['kind'].capitalize(), e['detail']) for e in inventory['edges']}
        actual = {(e['a'], e['b'], e['labels'][0], e['desc']) for e in graph['links'] if 'board' not in e}
        self.assertEqual(actual, expected)
        for edge in graph['links']:
            self.assertIn(edge['a'], items)
            self.assertIn(edge['b'], items)
            self.assertTrue(edge['labels'])
        refs = {f['p']: f['n'] for f in graph['files']}
        for file in graph['project']['entrypoints']:
            self.assertIn('n:'+refs[file], items)

    def test_code_default_preserves_reachability_without_redundant_links(self):
        graph = json.loads((self.output_dir / 'graph.json').read_bytes())
        plan = graph['peersus']['diagrams']['n:code']
        links = graph['links']
        full = {(a, b) for a, b, _ in plan['edges']}
        reduced = {tuple(plan['edges'][i][:2]) for i in plan['reduced']}
        ranks = {node: i for i, layer in enumerate(plan['layers']) for node in layer}
        self.assertEqual(set(ranks), set(graph['peersus']['boards']['n:code']['cards']))
        declared = [i for i, link in enumerate(links) if not link.get('structural') and not link.get('board')
                    and link['a'] in ranks and link['b'] in ranks]
        self.assertEqual(sorted(i for _, _, records in plan['edges'] for i in records), declared)
        self.assertTrue(all(links[i]['a'] == a and links[i]['b'] == b
                            for a, b, records in plan['edges'] for i in records))
        self.assertTrue(reduced < full)

        def reaches(start, pairs):
            seen, queue = set(), [start]
            while queue:
                node = queue.pop()
                for a, b in pairs:
                    if a == node and b not in seen:
                        seen.add(b)
                        queue.append(b)
            return seen

        for node in ranks:
            self.assertEqual(reaches(node, full), reaches(node, reduced))
        for a, b in reduced:
            self.assertLess(ranks[a], ranks[b])
            self.assertNotIn(b, reaches(a, reduced - {(a, b)}))
        for ref in ('n:extensions', 'n:delivery'):
            self.assertNotIn('reduced', graph['peersus']['diagrams'][ref])

    def test_reduction_keeps_shortcuts_between_unequal_length_layers_and_identifies_cycles(self):
        layers, kept, feedback = runpy.run_path(str(SCRIPT))['dependency_layers'](
            ['app', 'first', 'next', 'base', 'side', 'mutual'],
            [('app', 'first'), ('first', 'next'), ('next', 'base'), ('app', 'base'), ('app', 'side'),
             ('side', 'mutual'), ('mutual', 'side')])
        rank = {node: i for i, layer in enumerate(layers) for node in layer}
        self.assertEqual(kept, [0, 1, 2, 4])
        self.assertEqual(feedback, [5, 6])
        self.assertEqual(rank['side'], rank['mutual'])
        self.assertGreater(rank['side'] - rank['app'], 1)

    def test_dense_drilldowns_keep_mutual_links_off_the_default_canvas(self):
        graph = json.loads((self.output_dir / 'graph.json').read_bytes())
        diagrams = graph['peersus']['diagrams']
        dense = {ref: plan for ref, plan in diagrams.items() if 'layers' in plan}
        self.assertIn('n:crate-peersus-app', dense)
        for ref, plan in dense.items():
            with self.subTest(ref=ref):
                ranks = {node: i for i, layer in enumerate(plan['layers']) for node in layer}
                self.assertEqual(set(ranks), set(plan['cards']) | set(plan['connectors']))
                for i, (a, b, sources) in enumerate(plan['edges']):
                    self.assertLessEqual(ranks[a], ranks[b])
                    self.assertEqual(i in plan['feedback'], ranks[a] == ranks[b])
                    self.assertTrue(all(0 <= index < len(graph['links']) for index in sources))
                self.assertFalse(set(plan['feedback']) & set(plan['reduced']))

    def test_geometry_detects_card_contact_clearance_and_parallel_conflicts(self):
        core = runpy.run_path(str(SCRIPT))['GEOMETRY']
        checks = r'''
const assert = require('node:assert/strict');
const cards = [{id:'a',x:20,y:20,w:100,h:80}, {id:'b',x:190,y:20,w:100,h:80}];
const wire = (id,points) => ({id,points});
const good = {cards, labels:[{id:'caption',owner:'ab',x:139,y:36,w:34,h:15}], marks:[],
  wires:[wire('ab',[[120,60],[190,60]])],
  ports:[{card:'a',side:'r',p:[120,60]}, {card:'b',side:'l',p:[190,60]}], gutter:70};
assert.deepEqual(checkGeometry(good), []);
const check = (extra, word) => assert(checkGeometry({...good,...extra}).some(v=>v.startsWith(word)),word);
check({wires:[wire('ab',[[120,60],[202,60]])]}, 'card contact');
check({wires:[wire('ab',[[39,109],[90,109]])]}, 'card clearance');
check({wires:[...good.wires,wire('cd',[[135,68],[174,68]])]}, 'parallel clearance');
check({wires:[...good.wires,wire('cd',[[156,33],[156,55]])]}, 'label crosses');
check({labels:[...good.labels,{id:'other',owner:'other',x:140,y:35,w:20,h:12}]}, 'label overlap');
check({marks:[{id:'layer',x:149,y:53,w:11,h:12}]}, 'layer crosses');
check({ports:[...good.ports,{card:'a',side:'r',p:[120,68]}]}, 'port spacing');
check({wires:[...good.wires,wire('cd',[[370,60],[378,60]])]}, 'gutter bounds');
check({wires:[{...wire('ab',[[127,55],[174,55]]),color:'blue'},
              {...wire('cd',[[131,68],[175,68]]),color:'blue'}]}, 'hairpin clearance');
check({frame:{x:0,y:0,w:316,h:80},wires:[wire('ab',[[130,58],[165,58]])]}, 'frame clearance');
const frame={x:0,y:0,w:316,h:87};
const limit={...good,frame,ports:[],wires:[wire('ab',[[130,58],[165,58]])]};
assert.deepEqual(checkGeometry(limit), []);
check({...limit,wires:limit.wires.map(w=>({...w,radius:1.25}))}, 'frame clearance');
const pair=[wire('ab',[[127,55],[174,55]]),wire('cd',[[131,79],[175,79]])].map(w=>({...w,color:'blue'}));
assert.deepEqual(checkGeometry({...good,wires:pair,ports:[]}), []);
'''
        result = subprocess.run(['node', '-e', core + checks], capture_output=True, text=True, check=False, timeout=10)
        self.assertEqual(result.returncode, 0, result.stderr)

    def test_page_carries_exact_site_stylesheet_and_chrome(self):
        text = (self.output_dir / 'index.html').read_text()
        match = re.search(r'<style id="site-style">(.*?)</style>', text, re.S)
        self.assertIsNotNone(match)
        self.assertEqual(match[1], (ROOT / 'site/style.css').read_text())
        self.assertIn('<main class="document store" id="main">', text)
        self.assertEqual(re.findall(r'<h1\b[^>]*>', text), ['<h1 class="visually-hidden">'])
        self.assertRegex(text, r'<header id="legend"[^>]*>\s*<div[^>]*><h2 id="board-title"')
        self.assertRegex(text, r'<footer><span class="brand"[^>]*><span class="brand-line"><img class="mark\b')
        self.assertRegex(text, r'<header class="site-header"><span class="brand"[^>]*><span class="brand-line"><img class="mark\b')

    def test_offline_chrome_does_not_link_unbuilt_checkout_pages(self):
        text = (self.output_dir / 'index.html').read_text()
        header = re.search(r'<header class="site-header">(.*?)</header>', text, re.S)[1]
        footer = re.search(r'<footer>(.*?)</footer>', text, re.S)[1]
        self.assertNotIn('href=', header)
        self.assertEqual(re.findall(r'href="([^"]+)"', footer), ['licenses/quicksand-OFL-1.1.txt'])

    def test_data_only_reads_only_covered_repository_inputs_and_writes_no_viewer(self):
        workflow = ROOT / '.github/workflows/architecture-map.yml'
        triggers = workflow.read_text().split('\non:\n', 1)[1].split('\n\n', 1)[0]
        # Every publication of main is a new root commit GitHub cannot diff, so a `paths` filter would skip the map
        # without a trace: the map follows every push to main, which covers whatever the generator reads.
        self.assertIn('  push:\n    branches: [main]\n', triggers)
        self.assertNotIn('    paths:', triggers)
        invocation = '''
import pathlib, runpy, sys
root = pathlib.Path.cwd()
script, *args = sys.argv[1:]
opened = set()
def audit(event, values):
    if event != "open" or not isinstance(values[0], str):
        return
    p = pathlib.Path(values[0]).absolute()
    if p.is_relative_to(root):
        name = p.relative_to(root).as_posix()
        if not name.startswith(("plugins/", "extension/", ".git/")):
            opened.add(name)
sys.addaudithook(audit)
sys.argv = [script, *args]
try:
    runpy.run_path(script, run_name="__main__")
finally:
    print("READS", *sorted(opened), sep="\\n")
'''
        with tempfile.TemporaryDirectory() as scratch:
            base = Path(scratch)
            args = [sys.executable, '-c', invocation, str(SCRIPT), '--data-only', '--commit', '0'*40,
                    '--commit-date', '2026-01-02T03:04:05+00:00']
            if EXTENSIONS is not None:
                args += ['--extensions-root', str(EXTENSIONS)]
            for folder in ('first', 'second'):
                result = subprocess.run([*args, '--output', str(base / folder)], cwd=ROOT, capture_output=True, text=True, timeout=30)
                self.assertEqual(result.returncode, 0, result.stderr)
                reads = result.stdout.split('\nREADS\n', 1)[1].splitlines()
                self.assertIn('Cargo.toml', reads)
                self.assertIn('docs/architecture.md', reads)
                self.assertFalse([p for p in reads if p.startswith('site/')])
                self.assertEqual({p.name for p in (base / folder).iterdir()}, {'map.json'})
            self.assertEqual((base / 'first/map.json').read_bytes(), (base / 'second/map.json').read_bytes())
            data = json.loads((base / 'first/map.json').read_bytes())
            self.assertEqual(data['source'], {'commit': '0'*40, 'date': '2026-01-02T03:04:05Z'})

    def test_privacy_check_rejects_untracked_anchors_and_private_values(self):
        generator = runpy.run_path(str(SCRIPT))
        data = json.loads(generator['publication'](EXTENSIONS))
        validate = generator['verify_publication']
        node = data['nodes'][0]
        with self.assertRaisesRegex(ValueError, 'non-public source'):
            validate({**data, 'nodes': [{**node, 'source': {'path': 'target/synthetic.rs', 'line': 1}}]})
        with self.assertRaisesRegex(ValueError, 'Private'):
            validate({**data, 'nodes': [{**node, 'summary': 'probe @ 192.0.2.10'}]})

    def test_html_is_self_contained_and_matches_floorplan_json(self):
        text = (self.output_dir / 'index.html').read_text()
        match = re.search(r'<script type="application/json" id="map-data">(.*?)</script>', text, re.S)
        self.assertIsNotNone(match)
        self.assertEqual(json.loads(match.group(1)), json.loads((self.output_dir / 'graph.json').read_bytes()))
        self.assertNotRegex(text, r'https?://|file://|/(?:home|Users)/|\b(?:\d{1,3}\.){3}\d{1,3}\b')
        self.assertNotRegex(text, r'<(?:script|img|link)\b[^>]*\b(?:src|href)=["\'](?!data:)')
        self.assertNotIn('fetch(', text)

    def test_embedded_third_party_components_have_their_licences(self):
        class Assets(HTMLParser):
            def __init__(self):
                super().__init__()
                self.notices = set()
                self.unlicensed = []
                self.current_notice = None

            def handle_starttag(self, tag, attrs):
                attrs = dict(attrs)
                if tag not in ('script', 'style'):
                    return
                self.current_notice = notice = attrs.get('data-license')
                if notice:
                    self.notices.add(notice)
                if tag == 'script' and attrs.get('type') != 'application/json' and not notice:
                    self.unlicensed.append('executable script')

            def handle_data(self, data):
                if 'data:font/' in data and self.current_notice != 'licenses/quicksand-OFL-1.1.txt':
                    self.unlicensed.append('embedded font')

        assets = Assets()
        assets.feed((self.output_dir / 'index.html').read_text())
        self.assertEqual(assets.unlicensed, [])
        self.assertLessEqual({'licenses/floorplan-MIT.txt', 'licenses/quicksand-OFL-1.1.txt'}, assets.notices)
        on_disk = {f.relative_to(self.output_dir).as_posix() for f in (self.output_dir / 'licenses').iterdir() if f.is_file()}
        self.assertEqual(on_disk, assets.notices)
        self.assertEqual((self.output_dir / 'licenses/floorplan-MIT.txt').read_bytes(),
                         (ROOT / 'site/architecture/floorplan-MIT.txt').read_bytes())
        self.assertEqual((self.output_dir / 'licenses/quicksand-OFL-1.1.txt').read_bytes(),
                         (ROOT / 'crates/peersus-ui/assets/fonts/OFL.txt').read_bytes())

    def test_check_reports_stale_and_missing_files_without_creating_or_replacing_them(self):
        mod = runpy.run_path(str(SCRIPT))
        output = mod['output']
        with tempfile.TemporaryDirectory(dir=OUT) as scratch, contextlib.redirect_stderr(io.StringIO()) as report:
            directory = Path(scratch)
            output.__globals__['OUT'] = directory
            (directory / 'map.json').write_bytes(b'unrelated stale bytes')
            self.assertEqual(output(check=True, extensions=EXTENSIONS), 1)
            self.assertEqual((directory / 'map.json').read_bytes(), b'unrelated stale bytes')
            self.assertFalse((directory / 'index.html').exists())
            for name in ('map.json', 'index.html', 'graph.json', 'licenses/floorplan-MIT.txt',
                         'licenses/quicksand-OFL-1.1.txt'):
                self.assertIn(name, report.getvalue())

    def test_rules_reject_guest_runtime_dependency_and_real_host_predicate(self):
        mod = runpy.run_path(str(SCRIPT))
        self.assertTrue(mod['forbidden_dependency']('extension:demo', 'peersus-net', 'normal'))
        self.assertFalse(mod['forbidden_dependency']('extension:demo', 'peersus-api', 'normal'))
        self.assertFalse(mod['forbidden_dependency']('extension:demo', 'serde', 'normal'))
        source = ('// if service == "example.run" {}\n'
                  'fn test() { let label = "example.run"; }\n'
                  'fn dispatch(service: &str) { if service == "example.run" {} }\n'
                  '#[cfg(test)]\nmod tests { fn f() { if service == "example.run" {} } }\n')
        self.assertEqual(mod['named_predicates'](source, {'example.run'}), [(3, 'example.run')])

    def test_serdes_unknown_and_overridden_names_are_not_inferred_from_case(self):
        mod = runpy.run_path(str(SCRIPT))
        source = 'pub enum X { HelloWorld { name: String }, #[serde(rename="exact")] Choice, #[serde(other)] Unknown }'
        variants = mod['enum_variants'](source, 'X', 'kebab-case')
        self.assertEqual([(name, wire, fallback) for name, wire, _line, fallback in variants],
                         [('HelloWorld', 'hello-world', False), ('Choice', 'exact', False), ('Unknown', 'unknown', True)])


if __name__ == '__main__':
    unittest.main()
