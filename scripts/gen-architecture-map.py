#!/usr/bin/env python3
"""Build the Peersus architecture data and its offline viewer."""
import argparse
import base64
from collections import Counter, defaultdict
from datetime import datetime, timezone
from fnmatch import fnmatchcase
from functools import cache
import hashlib
import html
import json
from pathlib import Path
import re
import runpy
import subprocess
import sys
import tomllib

ROOT = Path(__file__).resolve().parents[1]
OUT = ROOT / 'target/architecture-map'
APP = 'crates/peersus-app/src/'
NET = 'crates/peersus-net/src/'
API = 'crates/peersus-api/src/'
UI = 'crates/peersus-ui/src/'
ARCH = 'docs/architecture.md'
MESSAGES = 'docs/api/service-messages.md'
HOST = APP + 'runtime/host_contract.rs'
MANIFESTS = 'docs/api/manifests.md'
FLOORPLAN_LICENSE = 'site/architecture/floorplan-MIT.txt'
FLOORPLAN_REV = '94f81b463b7617e06d873ac71323ce71644c8399'
SCHEMA = 'floorplan/graph@1'
PUBLIC_SCHEMA = 'peersus/architecture-view@1'
MAX_PUBLIC_BYTES = 2 * 1024 * 1024
FONT = 'crates/peersus-ui/assets/fonts/'

# Only identifiers, portable paths and reviewed explanations enter the artifact.
WORD = re.compile(r'[A-Za-z0-9_.:/ @+()=,\-\[\]"&!]+\Z')
PRIVATE = re.compile(
    r'(?i)(?:\b[a-z][a-z0-9+.-]*://|\b[a-z]:[\\/]|/(?:home|Users|var|tmp|run|etc|root|Volumes)/'
    r'|\b(?:\d{1,3}\.){3}\d{1,3}\b|\[(?:[0-9a-f]{0,4}:){2,}[0-9a-f:]*\]'
    r'|\b[\w.+-]+@[\w.-]+\.[a-z]{2,}\b|\b(?:localhost|[\w-]+\.(?:local|lan|internal))(?::\d+)?\b'
    r'|\badb-[\w-]+\b|(?:\w+\.)+\w+:[0-9]{2,5}\b)')
TOKEN = re.compile(r'//[^\n]*|/\*.*?\*/|(?:br|r)(?P<hash>\#*)".*?"(?P=hash)|"(?:\\.|[^"\\])*"|\w+|::|==|=>|[^\s]', re.S)


def read(path):
    candidate = ROOT / path
    if not candidate.resolve().is_relative_to(ROOT):
        raise ValueError('Input escapes repository')
    return candidate.read_text(encoding='utf-8')


def safe(value):
    if not isinstance(value, str) or len(value) > 320 or not WORD.fullmatch(value) or PRIVATE.search(value):
        raise ValueError('Unsafe architecture identifier or path')
    return value


def line_at(source, offset):
    return source.count('\n', 0, offset) + 1


def code_tokens(source):
    return [m for m in TOKEN.finditer(source) if not m.group().startswith(('//', '/*'))]


def closing(tokens, start):
    pairs = {'{': '}', '[': ']', '(': ')'}
    stack = []
    for index in range(start, len(tokens)):
        word = tokens[index].group()
        if word in pairs:
            stack.append(pairs[word])
        elif word in pairs.values():
            if not stack or stack.pop() != word:
                break
            if not stack:
                return index
    raise ValueError('Unbalanced source declaration')


def enum_variants(source, name, casing):
    match = re.search(r'\benum\s+' + re.escape(name) + r'\s*\{', source)
    if not match:
        raise ValueError(f'Missing enum {name}')
    tokens = code_tokens(source[match.end()-1:])
    end = closing(tokens, 0)
    i, result = 1, []
    while i < end:
        attrs = []
        while tokens[i].group() == '#':
            last = closing(tokens, i + 1)
            attrs.append(''.join(t.group() for t in tokens[i:last+1]))
            i = last + 1
        variant = tokens[i].group()
        if not re.fullmatch(r'[A-Z]\w*', variant):
            raise ValueError(f'Unsupported variant in {name}')
        rename = re.search(r'rename="([\w-]+)"', ''.join(attrs))
        wire = re.sub(r'(?<!^)(?=[A-Z])', '-' if casing == 'kebab-case' else '_', variant).lower()
        result.append((variant, rename[1] if rename else wire,
                       line_at(source, match.end()-1+tokens[i].start()), '#[serde(other)]' in attrs))
        i += 1
        if i < end and tokens[i].group() in ('{', '('):
            i = closing(tokens, i) + 1
        if i < end:
            if tokens[i].group() != ',':
                raise ValueError(f'Unsupported delimiter in {name}')
            i += 1
    return result


def named_predicates(source, names):
    tokens = code_tokens(source)
    visible = []
    i = 0
    while i < len(tokens):
        if [t.group() for t in tokens[i:i+7]] == ['#', '[', 'cfg', '(', 'test', ')', ']']:
            i += 7
            while i < len(tokens) and tokens[i].group() not in ('{', ';'):
                i += 1
            if i < len(tokens) and tokens[i].group() == '{':
                i = closing(tokens, i)
        else:
            visible.append(tokens[i])
        i += 1
    found = []
    for i, token in enumerate(visible):
        literal = token.group()
        if literal[1:-1] not in names or not literal.startswith('"'):
            continue
        before = [t.group() for t in visible[max(0, i-4):i]]
        after = [t.group() for t in visible[i+1:i+4]]
        # Dispatch by a concrete ID, not mere mention in labels, fixtures, logs or examples.
        if (before and before[-1] == '==') or (after and after[0] in ('==', '=>')):
            found.append((line_at(source, token.start()), literal[1:-1]))
    return sorted(set(found))


def forbidden_dependency(source, target, kind):
    if kind == 'dev':
        return False
    if source.startswith('extension:'):
        return target.startswith('peersus-') and target != 'peersus-api'
    if source == 'peersus-api':
        return target.startswith('peersus-')
    if source == 'peersus-core':
        return target.startswith('peersus-') and target != 'peersus-api'
    if source in ('peersus-app', 'peersus-net', 'peersus-plugin'):
        return target in ('peersus-ui', 'peersus-ui-android')
    return False


def git(*args):
    result = subprocess.run(['git', *args], cwd=ROOT, capture_output=True, check=False, timeout=10)
    if result.returncode:
        raise ValueError('Architecture evidence requires a Git checkout')
    return result.stdout.decode('utf-8')


def tracked_sources():
    return set(git('ls-files', '--cached', '--full-name', '-z').split('\0')) - {''}


def portable(path):
    return (isinstance(path, str) and '\\' not in path and ':' not in path
            and all(part not in ('', '.', '..') for part in path.split('/')))


def evidence_path(path):
    return re.fullmatch(r'(?:plugins|extension)/peersus-[a-z0-9-]+/'
                        r'(?:submission\.toml|wasm/(?:Cargo\.toml|plugin\.json|src/remote\.rs))', path)


class Graph:
    def __init__(self, extensions=None):
        self.nodes = {}
        self.edges = set()
        self.views = []
        self.violations = []
        self.texts = {}
        self.tracked = tracked_sources()
        self.extension_root = ROOT / 'plugins' if extensions is None else extensions.resolve()
        self.extension_prefix = 'plugins' if extensions is None else 'extension'
        if extensions is not None and (self.extension_root / 'extension').is_dir():
            self.extension_root /= 'extension'

    def extension(self, slug, relative):
        path = f'{self.extension_prefix}/peersus-{slug}/{relative}'
        source = self.extension_root.joinpath(*Path(path).parts[1:])
        if not evidence_path(path) or source.resolve() != source:
            raise ValueError('Unexpected extension evidence path')
        return path if source.is_file() else None

    def source_files(self, prefix):
        return sorted(path for path in self.tracked if path.startswith(prefix) and not (ROOT / path).is_symlink())

    def text(self, path):
        if path not in self.texts:
            if path.startswith(f'{self.extension_prefix}/') and evidence_path(path):
                parts = Path(path).parts[1:]
                source = self.extension_root.joinpath(*parts)
            elif path in self.tracked and portable(path):
                source = ROOT / path
            else:
                raise ValueError('Architecture evidence is not a tracked source')
            if source.resolve() != source:
                raise ValueError('Architecture evidence cannot follow symlinks')
            self.texts[path] = source.read_text(encoding='utf-8')
        return self.texts[path]

    def source(self, path, anchor):
        safe(path)
        text = self.text(path)
        if isinstance(anchor, str):
            candidates = [i for i, line in enumerate(text.splitlines(), 1) if anchor in line]
            if not candidates:
                raise ValueError(f'Missing source anchor in {path}: {anchor}')
            anchor = candidates[0]
        if not 1 <= anchor <= len(text.splitlines()):
            raise ValueError(f'Source line outside {path}')
        return {'path': path, 'line': anchor}

    def documentation(self, doc):
        doc_path, heading = doc
        titles = [line.lstrip('# ').rstrip() for line in self.text(doc_path).splitlines() if line.startswith('#')]
        title = next((t for t in titles if t == heading or t.startswith(heading + ' (')), None)
        if not title and not titles:
            raise ValueError(f'Missing documentation headings in {doc_path}')
        # A reference remains useful if a section was renamed in this revision.
        title = title or titles[0]
        slug = re.sub(r'[^\w\- ]', '', title.lower()).replace(' ', '-')
        return dict(path=doc_path, heading=title, slug=slug)

    def node(self, id_, label, kind, path, anchor=1, *, view, note='', doc=None, **facts):
        safe(id_)
        safe(label)
        if id_ in self.nodes:
            raise ValueError('Repeated node: ' + id_)
        node = dict(id=id_, label=label, kind=kind, view=view, note=note, source=self.source(path, anchor), **facts)
        if doc:
            node['doc'] = self.documentation(doc)
        self.nodes[id_] = node
        return id_

    def edge(self, src, dest, kind, detail=''):
        self.edges.add((src, dest, kind, detail))

    def violation(self, node, rule, message, source):
        self.violations.append(dict(node=node, rule=rule, message=message, source=source))

    def view(self, key, label, title, intro, how, lanes):
        """A lane is (name, node ids) or (name, node ids, one plain sentence saying what the group is)."""
        self.views.append(dict(id=key, label=label, title=title, intro=intro, how=how,
                               lanes=[dict(label=lane[0], nodes=list(lane[1]), summary=lane[2] if len(lane) > 2 else '')
                                      for lane in lanes]))

    def result(self):
        if any(a not in self.nodes or b not in self.nodes for a, b, _, _ in self.edges):
            raise ValueError('Graph contains an unresolved node reference')
        return dict(schema='peersus/architecture-map@2', views=self.views,
                    nodes=[self.nodes[id_] for id_ in sorted(self.nodes)],
                    edges=[{'from': a, 'to': b, 'kind': k, 'detail': d} for a, b, k, d in sorted(self.edges)],
                    violations=sorted(self.violations, key=lambda v: (v['source']['path'], v['source']['line'], v['message'])))


CRATE_NOTES = {
    'api': 'Defines the shared vocabulary: manifests, permissions, messages, page elements and canvas frames. It does not run them.',
    'core': 'Holds the domain model, preferences, commands and pairing rules without knowing a specific extension.',
    'net': 'Owns authenticated peer transport, offers, transfers and reconnect policy.',
    'app': 'Runs the single owner: commands, persistent state, local control and extension admission.',
    'ui': 'Draws owner snapshots, supplies generic controls and sends actions back to the owner.',
    'ui-android': 'Joins the shared interface and owner to the Android lifecycle and platform adapters.',
    'plugin': 'Routes calls and events and supervises extension workers.',
    'plugin-runtime': 'Runs the bounded WASM interpreter and the guest transport.',
    'plugin-android': 'Connects Android services and descriptors to the guest interpreter.',
    'plugin-canvas': 'Provides the drawing path for WASM workers with a canvas import.',
    'render': 'Validates guest display lists and rasterizes them inside the guest worker.',
    'store-fetch': 'Downloads bounded store/update data for the desktop owner; it does not decide trust.',
    'clipboard-platform': 'Adapts clipboard access to the system; it is not an extension.',
    'diagnostics': 'Provides tracing and diagnostics for the programs that use it.',
    'xtask': 'Dispatches the build, quality checks and packaging tools.',
}


def cargo_dependencies(manifest, workspace=None):
    for condition, target in [(None, manifest), *manifest.get('target', {}).items()]:
        for kind, key in [('normal', 'dependencies'), ('build', 'build-dependencies'), ('dev', 'dev-dependencies')]:
            for alias, spec in target.get(key, {}).items():
                spec = spec if isinstance(spec, dict) else {}
                if spec.get('workspace') and workspace is not None:
                    inherited = workspace[alias]
                    spec = {**(inherited if isinstance(inherited, dict) else {}), **spec}
                yield alias, spec.get('package', alias), kind, condition, spec.get('optional', False)


def cargo_targets(g, folder, manifest):
    package = manifest['package']
    name = package['name']
    targets = {}
    lib = manifest.get('lib')
    if lib is not None or package.get('autolib', True) and folder + '/src/lib.rs' in g.tracked:
        lib = lib or {}
        kinds = lib.get('crate-type', ['proc-macro' if lib.get('proc-macro') else 'lib'])
        targets[lib.get('name', name.replace('-', '_'))] = (lib.get('path', 'src/lib.rs'), kinds, lib)
    for bin_ in manifest.get('bin', []):
        title = bin_['name']
        standard = 'src/main.rs' if title == name else f'src/bin/{title}.rs'
        path = bin_.get('path', standard)
        if 'path' not in bin_ and f'{folder}/{path}' not in g.tracked:
            path = f'src/bin/{title}/main.rs'
        targets[title] = (path, ['bin'], bin_)
    if package.get('autobins', True):
        if folder + '/src/main.rs' in g.tracked:
            targets.setdefault(name, ('src/main.rs', ['bin'], {}))
        for path in g.source_files(folder + '/src/bin/'):
            relative = path.removeprefix(folder + '/')
            match = re.fullmatch(r'src/bin/([^/]+)(?:\.rs|/main\.rs)', relative)
            if match and all(entry[0] != relative for entry in targets.values()):
                targets.setdefault(match[1], (relative, ['bin'], {}))
    return sorted(targets.items())


def module_references(g, paths, source_root, cycles):
    """Run the shared lexical rules on tracked paths, not on every local file."""
    entries = [(path, path.removeprefix(source_root).split('/')) for path in paths]
    entries = [(path, parts) for path, parts in entries if path.endswith('.rs') and parts[0] != 'bin'
               and parts[-1] != 'tests.rs' and 'tests' not in parts[:-1]
               and parts not in (['lib.rs'], ['main.rs'])]
    modules = {parts[0].removesuffix('.rs') for _, parts in entries}
    edges = defaultdict(Counter)
    for path, parts in entries:
        module = parts[0].removesuffix('.rs')
        source = cycles['without_tests'](cycles['without_comments'](g.text(path)))
        refs = re.findall(r'\bcrate::(\w+)', source)
        for group in re.finditer(r'\bcrate::\{', source):
            refs += cycles['first_names'](source[group.end():cycles['closing'](source, group.end()) - 1])
        if len(parts) == 1:
            refs += re.findall(r'\bsuper::(\w+)', source)
        edges[module].update(other for other in refs if other in modules and other != module)
    return modules, edges


def code_view(g):
    workspace = tomllib.loads(g.text('Cargo.toml'))['workspace']
    locked = {p['name'] for p in tomllib.loads(g.text('Cargo.lock'))['package'] if 'source' not in p}
    scanner = 'scripts/module-cycles.py'
    g.text(scanner)  # Enforce source provenance before loading the shared lexical rules.
    cycles = runpy.run_path(str(ROOT / scanner))
    packages = {}
    manifests = {path.removesuffix('/Cargo.toml') for path in g.tracked if path.endswith('/Cargo.toml')}
    for pattern in workspace['members']:
        for folder in sorted(path for path in manifests if fnmatchcase(path, pattern)):
            manifest = f'{folder}/Cargo.toml'
            package = tomllib.loads(g.text(manifest))
            packages[package['package']['name']] = folder, manifest, package
    if packages.keys() - locked:
        raise ValueError('The tracked lockfile omits a declared workspace package')
    defaults = workspace.get('default-members', workspace['members'])
    deps = defaultdict(set)
    for name, (folder, manifest, p) in sorted(packages.items()):
        dependencies = list(cargo_dependencies(p, workspace.get('dependencies', {})))
        features = p.get('features', {})
        explicit = {feature.removeprefix('dep:') for values in features.values()
                    for feature in values if feature.startswith('dep:')}
        feature_names = set(features) | {alias for alias, _, _, _, optional in dependencies if optional and alias not in explicit}
        cid = 'crate:' + name
        g.node(cid, name, 'Crate', manifest, '[package]', view='code', doc=(ARCH, 'Workspace map'),
               note=CRATE_NOTES.get(name.removeprefix('peersus-'), 'A declared workspace package.'),
               features=sorted(map(safe, feature_names)), defaultMember=any(fnmatchcase(folder, pat) for pat in defaults))
        for alias, target, kind, condition, optional in dependencies:
            if target not in packages:
                continue
            qualifier = ', '.join(filter(None, [kind, condition, 'optional' if optional else '',
                                                 'as ' + alias if alias != target else '']))
            g.edge(cid, 'crate:' + target, 'depends on', safe(qualifier))
            if kind != 'dev':
                deps[name].add(target)
            if forbidden_dependency(name, target, kind):
                g.violation(cid, 'AGENTS.md:9–19', f'{name} depends on forbidden workspace crate {target}', g.source(manifest, 1))
        for title, (path, kinds, target_config) in cargo_targets(g, folder, p):
            target = g.node(f'target:{name}:{title}', title, 'Program' if 'bin' in kinds else 'Library', f'{folder}/{path}',
                            view='code', doc=(ARCH, 'Workspace map'), note='Cargo target: ' + ', '.join(kinds) + '.',
                            features=sorted(safe(x) for x in target_config.get('required-features', [])))
            g.edge(cid, target, 'builds')
        source_root = folder + '/src/'
        paths = g.source_files(source_root)
        modules, edges = module_references(g, paths, source_root, cycles)
        known = {}
        for path in paths:
            file = Path(path.removeprefix(source_root))
            parts = file.parts
            if file.suffix != '.rs' or any(p in ('tests', 'bin') for p in parts) or file.name == 'tests.rs' or file in (Path('lib.rs'), Path('main.rs')):
                continue
            stem = file.stem
            logical = (*parts[:-1], stem) if stem != 'mod' else parts[:-1]
            namepath = '::'.join(logical)
            # A companion folder can share its module root with foo.rs, not another Rust module.
            doc = (ARCH, 'The crates as Cargo declares them')
            if name == 'peersus-api' and re.search(r'^### `'+re.escape(stem)+r'`',g.text('docs/api/contract-index.md'),re.M):
                doc = ('docs/api/contract-index.md', f'`{stem}`')
            elif name == 'peersus-net' and logical[0] in ('streams','transport','handshake','file_session','transfer','network_host','announce'):
                doc = (ARCH, 'Transfers and relay configuration')
            elif name == 'peersus-app' and logical[0] in ('ipc','lifetime','local_pipe'):
                doc = ('docs/api/local-control.md','Connection and identity')
            elif name == 'peersus-ui' and logical[0] in ('canvas_view','follow','render_budget'):
                doc = (ARCH, 'Extension-rendered canvases')
            mid = g.node(f'module:{name}:{path}', namepath, 'Module file', path, view='code', doc=doc,
                         note='Source file under the crate module tree. Reference edges are lexical, not compiler resolution.')
            known[logical] = mid
        for logical, mid in sorted(known.items()):
            g.edge(known.get(logical[:-1], cid), mid, 'contains')
        for mod, links in sorted(edges.items()):
            for other, n in sorted(links.items()):
                if (mod,) in known and (other,) in known:
                    g.edge(known[(mod,)], known[(other,)], 'module references', f'{n} lexical references')
        for group in cycles['groups'](modules, edges):
            if group and (group[0],) in known:
                node = known[(group[0],)]
                g.violation(node, 'AGENTS.md §146–154 · module cycle check', 'Mutually dependent modules: ' + ', '.join(group), g.nodes[node]['source'])
    depths = {}
    def depth(name, trail=()):
        if name in trail:
            raise ValueError('Cycle in workspace declarations')
        if name not in depths:
            depths[name] = max((1 + depth(d, (*trail, name)) for d in deps[name]), default=0)
        return depths[name]
    lanes = defaultdict(list)
    for name in sorted(packages):
        lanes[depth(name)].append('crate:' + name)
    g.view('code', 'Code map', 'What depends on what?',
           'Start at the foundations. Open a crate to follow its Cargo targets, features, module files and dependency directions.',
           'Each card is a Rust crate. The first group depends on no other Peersus crate; every later group builds on the groups before it.',
           [('Foundations' if not band else f'Layer {band + 1}', lanes[band],
             'Crates that depend on no other Peersus crate.' if not band else 'Crates that build on the groups before this one.')
            for band in sorted(lanes)])


SERVICE_DOCS = {
    'peersus.displays':'Arranging desktops', 'peersus.input':'Input mode negotiation', 'peersus.config':'Caller readiness',
    'peersus.peers':'The paired devices, as one extension may know them', 'peersus.status':'What paired devices say of themselves',
    'peersus.system':'How this device is doing', 'peersus.canvas':'Extension-rendered canvases', 'peersus.notify':'Notifications an extension asks for',
    'peersus.language':"The person's language", 'peersus.pages':'Additional placements of declared pages',
    'peersus.actions':'Actions an extension takes back for now', 'peersus.drawings':'The drawings an extension keeps',
    'peersus.pictures':'The pictures an extension keeps', 'peersus.audio':'The sound an extension plays',
    'peersus.files':'Android file-selection and HTTPS host services', 'peersus.http':'Android file-selection and HTTPS host services',
    'peersus.location':'Android file-selection and HTTPS host services',
}


def host_members(g):
    modules = [HOST, APP+'platform_services.rs', APP+'platform_services/location.rs', APP+'page_offers.rs',
               APP+'status_service.rs', APP+'display_service.rs', APP+'page_frames.rs', APP+'drawing_layers.rs',
               APP+'transfer_events.rs', APP+'system_readings.rs', API+'canvas.rs', API+'elements.rs', API+'extension_data.rs',
               API+'pictures.rs', API+'audio.rs', API+'notification.rs']
    constants = {}
    for path in modules:
        prefix = Path(path).stem
        for m in re.finditer(r'^pub(?:\(crate\))? const (\w+):\s*&str\s*=\s*"([^"\n]+)"', g.text(path), re.M):
            constants[prefix+'::'+m[1]] = m[2]
            if path == HOST or path.endswith('/platform_services.rs'):
                constants[m[1]] = m[2]
    sys_path = APP+'system_readings.rs'
    for m in re.finditer(r'const (\w+): Part = Part \{(.*?)\n\};', g.text(sys_path), re.S):
        cap = re.search(r'\bcapability:\s*"([^"]+)"', m[2])
        if cap:
            constants['system_readings::'+m[1]+'.capability'] = cap[1]

    def value(expr):
        expr = expr.strip()
        if expr.startswith('"'):
            return safe(json.loads(expr))
        parts = expr.removeprefix('crate::').split('::')
        if parts[0] == 'status':
            parts[0] = 'status_service'
        key = '::'.join(parts[-2:])
        if key in constants:
            return safe(constants[key])
        # Page frames re-exports the shared canvas service constants.
        if parts[0] == 'page_frames' and 'canvas::'+parts[-1] in constants:
            return safe(constants['canvas::'+parts[-1]])
        raise ValueError('Unresolved host declaration symbol: '+expr)

    permissions = g.text(APP+'runtime/permissions.rs').split('fn needs_no_device', 1)[1].split('.contains(', 1)[0]
    scopes = set()
    for line in permissions.splitlines():
        # Each path segment consumes one identifier; do not let a qualifier match
        # the same prefix in two ways or let whitespace span subsequent lines.
        token = re.match(r'\w+(?:::\w+)*(?:\.capability)?,', line.lstrip())
        if token:
            scopes.add(value(token[0][:-1]))
    source = g.text(HOST)
    start = source.index('pub(crate) fn exports_for(')
    direct, loop = source[start:].split('    for (service, description) in [', 1)
    atom = r'"[\w.-]+"|(?:\w+::)*\w+'
    conversion = r'\.(?:into|to_owned)\(\)'
    pair = re.compile(r'\(\s*('+atom+r')'+conversion+r',\s*(?:Member\s*\{\s*capability:\s*(None|Some\(('+atom+r')'+conversion+r'\))|guarded\(('+atom+r'),)', re.S)
    offers = defaultdict(list)
    for block in re.finditer(r'exports\.push\(Export \{(.*?)\n        \}\);', direct, re.S):
        text = block[1]
        svc = value(re.search(r'\bservice:\s*('+atom+r')'+conversion,text)[1])
        version = int(re.search(r'\bversion:\s*(\d+)',text)[1])
        methods, events = text.split('events:',1)
        for kind, section, offset in [('method', methods, 0), ('event', events, len(methods)+len('events:'))]:
            matches = list(pair.finditer(section))
            declarations = section.count('Member {') + section.count('guarded(')
            if len(matches) != declarations and 'system_readings::PARTS' not in section:
                raise ValueError(f'Unsupported member shape in {svc}')
            for match in matches:
                member = value(match[1]); cap = value(match[3] or match[4]) if match[3] or match[4] else None
                offers[(svc,version)].append((kind,member,cap,HOST,line_at(source,start+block.start(1)+offset+match.start(1))))
        if 'system_readings::PARTS' in methods:
            table = re.search(r'\bPARTS:[^=]*= \[(.*?)\];',g.text(sys_path),re.S)[1]
            for part in re.findall(r'&(\w+)',table):
                item = re.search(r'const '+part+r': Part = Part \{(.*?)\n\};',g.text(sys_path),re.S)
                member = re.search(r'\bmethod:\s*"([^"]+)"',item[1])[1]
                offers[(svc,version)].append(('method',safe(member),value('system_readings::'+part+'.capability'),sys_path,line_at(g.text(sys_path),item.start())))
    if len(offers) != direct.count('exports.push(Export {'):
        raise ValueError('A direct host export had no extracted members')
    services = {value(m) for m in re.findall(r'\(\s*('+atom+r'),\s*(?:\w+::)*\w+,?\s*\)',loop.split('] {',1)[0])}
    path = APP+'platform_services.rs'
    platform = g.text(path)
    for table, kind in [('METHODS','method'),('EVENTS','event'),('RAISED','event')]:
        section = re.search(r'\b'+table+r': [^=]*= &\[(.*?)\];',platform,re.S)
        rows = list(re.finditer(r'\(\s*('+atom+r'),\s*('+atom+r'),\s*('+atom+r'),\s*"(?:\\.|[^"\\])*"\s*,?\s*\)',section[1],re.S))
        expected = len(re.findall(r'\(\s*(?:'+atom+r'),',section[1]))
        if len(rows) != expected:
            raise ValueError('Unsupported platform service table row')
        version = int(re.search(r'\bversion:\s*(\d+)',loop)[1])
        for row in rows:
            svc = value(row[1])
            if svc not in services:
                raise ValueError('Platform service is not in the host offer loop')
            offers[(svc,version)].append((kind,value(row[2]),value(row[3]),path,line_at(platform,section.start(1)+row.start())))
    if not services <= {key[0] for key in offers}:
        raise ValueError('Missing platform host service')
    return offers, scopes


def broker(g):
    offers, scopes = host_members(g)
    host = g.node('participant:host','Application host','Participant',HOST,'fn host_manifest',view='broker',
                  doc=(ARCH,'Extensions and backend independence'),note='Projects services to exact importers. A declaration is not a grant.')
    bus = g.node('participant:broker','Service broker','Participant','crates/peersus-plugin/src/hub.rs',view='broker',
                 doc=(ARCH,'Work per broker call'),note='Checks provider, version, member, allowed consumer and current permission before delivering work.')
    g.edge(host,bus,'registers and routes through')
    catalog = {}
    extensions = []
    imports = []
    policy_names = set()
    def service(provider, owner, name, version, path, line, members, consumers=None):
        sid = f'service:{provider}:{name}:{version}'
        doc = (MESSAGES,SERVICE_DOCS[name]) if name in SERVICE_DOCS else (MANIFESTS,'Identity and declarations')
        g.node(sid,name,'Service',path,line, view='broker',doc=doc,version=version,provider=provider,
               note='Host export, offered only to matching importers.' if provider=='host' else 'Extension declares this version and its consumer allowlist.',
               consumers=['Matching importers'] if consumers is None else [safe(v) for v in sorted(consumers)])
        g.edge(owner,sid,'exports')
        for kind, member, cap, where, at in sorted(members):
            scope = ('Caller only' if provider=='host' else 'No separate grant') if cap is None else (
                'Once on this device' if provider=='host' and cap in scopes else 'Per paired device')
            mid = g.node(f'member:{provider}:{name}:{version}:{kind}:{member}',member,kind.title(),where,at,view='broker',doc=doc,
                         provider=provider,version=version,permission=cap,scope=scope,
                         note=f'{kind.title()} on {name}. A declared route does not reveal any current grant.')
            g.edge(sid,mid,'defines')
            catalog[(provider,name,version,kind,member)] = (mid,cap,consumers)
        return sid
    for (name,version), members in sorted(offers.items()):
        service('host',host,name,version,members[0][3],members[0][4],members)
    for slug in sorted(name.lower() for name in EXTENSION_NOTES):
        path = g.extension(slug, 'wasm/plugin.json')
        if path is None:
            extensions.append(g.node('extension:'+slug, slug.title(), 'Extension', MANIFESTS, '## Identity and declarations',
                                     view='broker', doc=(MANIFESTS,'Identity and declarations'),
                                     note='No manifest in this source snapshot. Contract details are not shown.'))
            continue
        base = path.removesuffix('wasm/plugin.json')
        recipe = tomllib.loads(g.text(base+'submission.toml'))
        if recipe['id'] != slug:
            raise ValueError('Unexpected first-party extension ID')
        m = json.loads(g.text(path))
        eid = g.node('extension:'+slug, slug.title(), 'Extension', path, '"package"', view='broker',
                     doc=(MANIFESTS,'Identity and declarations'), note='Declared in source; not proof of installation, admission or publication.',
                     platforms=[safe(v) for v in sorted(m['platforms'])], execution=safe(m['execution']['kind']))
        extensions.append(eid)
        policy_names.add('peersus-'+slug)
        def anchor(key):
            return g.source(path, json.dumps(key))['line']
        for export in m['exports']:
            name, ver = safe(export['service']), export['version']
            policy_names.add(name)
            members = [(k[:-1],safe(member),safe(a['capability']) if a.get('capability') else None,path,anchor(member))
                       for k in ('methods','events') for member,a in export[k].items()]
            service(slug,eid,name,ver,path,anchor(name),members,export.get('consumers',[]))
        for i in m['imports']:
            for k in ('methods','events'):
                for member in i[k]:
                    imports.append((eid,slug,(i['provider'],i['service'],i['version'],k[:-1],member), path, anchor(i['service'])))
        capabilities = m.get('capabilities',{})
        for name in sorted(capabilities):
            pid = g.node(f'capability:{slug}:{name}',safe(name),'Permission',path,anchor(name),view='broker',
                         doc=(MANIFESTS,'Identity and declarations'),note='Extension-owned capability; its declaration does not grant it.')
            g.edge(eid,pid,'declares')
        for family in ('pages','actions','settings'):
            for entry in m.get('contributions',{}).get(family,[]):
                id_ = safe(entry['id'])
                item = g.node(f'{family}:{slug}:{id_}',id_,family[:-1].title(),path,anchor(id_),view='broker',
                              doc=(MANIFESTS,'Identity and declarations'),note='Declared by the extension; the host renders or dispatches it generically.')
                if entry.get('capability') in capabilities:
                    g.edge(item,f'capability:{slug}:{entry["capability"]}','requires')
                if family == 'pages' and entry.get('surface'):
                    g.nodes[item]['surface'] = safe(entry['surface'])
                if entry.get('platforms'):
                    g.nodes[item]['platforms'] = [safe(v) for v in entry['platforms']]
                g.edge(eid,item,'contributes')
                ref = entry.get('source') or entry.get('target')
                if isinstance(ref,dict) and 'service' in ref:
                    for key, (mid, _, _) in catalog.items():
                        if key[0:2] == (slug, ref['service']) and key[3:] == ('method',ref['method']):
                            g.edge(item,mid,'dispatches to')
                            g.edge(host,mid,'may invoke','Declared contribution; current admission is separate')
        cargo=base+'wasm/Cargo.toml'
        manifest=tomllib.loads(g.text(cargo))
        for table in [manifest, *manifest.get('target',{}).values()]:
            for key, kind in [('dependencies','normal'),('build-dependencies','build'),('dev-dependencies','dev')]:
                for name, spec in table.get(key,{}).items():
                    dep = spec.get('package',name) if isinstance(spec,dict) else name
                    if 'crate:'+dep in g.nodes:
                        g.edge(eid,'crate:'+dep,'depends on',kind)
                    if forbidden_dependency(eid,dep,kind):
                        g.violation(eid,'AGENTS.md:19',f'{slug} depends on forbidden workspace crate {dep}',g.source(cargo,name))
    host_requests = Counter(owner for owner, _, key, _, _ in imports if key[0] == 'host' and key in catalog)
    for owner, total in sorted(host_requests.items()):
        g.edge(owner,host,'asks the host',f'{total} matching method and event declarations')
    for owner,slug,key,path,line in sorted(imports):
        offer = catalog.get(key)
        if not offer:
            identity = ':'.join(map(str,key))
            unknown = 'unresolved:'+identity
            if unknown not in g.nodes:
                g.node(unknown,key[-1],'Unresolved import',path,line,view='broker',doc=(MANIFESTS,'Identity and declarations'),
                       note='No local export matches this provider, service, version and member.')
            g.edge(owner,unknown,'unresolved import')
            continue
        member,cap,consumers = offer
        allowed = consumers is None or slug in consumers
        blocked = not allowed or (consumers is not None and cap is not None)
        reason = ('Consumer is not on the allowlist' if not allowed else 'A guarded extension method cannot be called by another extension' if blocked
                  else 'Permission depends on current grant' if cap else '')
        g.edge(owner,member,'blocked by contract' if blocked else ('subscribes to' if key[3]=='event' else 'imports'),reason)
    for (provider,_,_,kind,member), (id_,cap,consumers) in sorted(catalog.items()):
        if provider != 'host' and kind == 'method' and member == 'receive' and 'host' in consumers:
            g.edge(host,id_,'may route paired stream','Peer and service admission still apply')
    for crate in ('peersus-app','peersus-core','peersus-ui','peersus-ui-android','peersus-plugin','peersus-net'):
        for relative in g.source_files(f'crates/{crate}/src/'):
            path = Path(relative)
            if path.suffix != '.rs' or 'tests' in path.parts or path.name=='tests.rs' or path.stem.endswith(('_tests','_pipeline')):
                continue
            text=g.text(relative)
            # Work in files that mention a concrete service, avoiding unrelated production text.
            qualified = {name for name in policy_names if '"'+name+'"' in text}
            if qualified:
                for at,name in named_predicates(text,qualified):
                    g.violation('crate:'+crate,'AGENTS.md:69,105',f'Host predicate names concrete extension declaration {name}',g.source(relative,at))
    contract=[]
    enums=[('Operation','message.rs','kebab-case'),('Delivery','message.rs','kebab-case'),('Outcome','message.rs','kebab-case'),
           ('Node','elements.rs','snake_case'),('Event','elements.rs','snake_case'),('Execution','manifest.rs','snake_case'),
           ('Surface','contributions.rs','snake_case')]
    for enum,file,casing in enums:
        path=API+file
        concept=g.node('contract:'+enum,enum,'Wire type',path,'enum '+enum,view='broker',
                       doc=('docs/api/contract-index.md',f'`{Path(file).stem}`'),note='Shared contract data; the host and guest interpret it at their boundary.')
        contract.append(concept)
        for _,wire,at,_ in enum_variants(g.text(path),enum,casing):
            item=g.node(f'variant:{enum}:{wire}',wire,'Variant',path,at,view='broker',doc=('docs/api/contract-index.md',f'`{Path(file).stem}`'))
            g.edge(concept,item,'contains')
    g.view('broker','Broker','Who may call what?',
           'Follow an import to the exact versioned method or event. The map shows declarations, not installed extensions or live grants.',
           'Start with the host and the broker, then open an extension to see what it exports and imports. A declaration is not a grant.',
           [('Routing', [host,bus],'How the host and the broker decide who may call what.'),
            ('Six local extensions',extensions,'The first-party extensions of this working copy.'),
            ('Wire vocabulary',contract,'The shared message types the host and every extension agree on.')])


def system(g):
    def role(id_, label, kind, path, anchor, note, heading='The owner, its windows, and recovery (desktop)'):
        return g.node('role:'+id_,label,kind,path,anchor,view='system',note=note,doc=(ARCH,heading))
    desktop = [
        role('desktop-window','Desktop window','Process',UI+'link/session.rs','fn start_owner',
             'Renders snapshots, keeps an owner lease and starts the owner if one is missing.'),
        role('desktop-owner','Desktop owner','Process',APP+'bin/peersus.rs','fn main',
             'Owns the network, persistent state and extension broker. The window does not own those decisions.'),
        role('desktop-wasm','WASM worker','Process','crates/peersus-plugin/src/module.rs','wasm: directory.join',
             'Interprets bounded extension code without ambient device access.','Extensions and backend independence'),
        role('desktop-canvas','WASM canvas worker','Process','crates/peersus-plugin/src/module.rs','&self.wasm_canvas',
             'Selected by an exact canvas-open import; rasterizes the guest scene and sends finished pixels to the window.','Extension-rendered canvases'),
        role('desktop-native','Native helper','Process','xtask/src/main.rs','\"native-host\" |',
             'Loads an admitted native library separately. This is process isolation, not the WASM sandbox.','Extensions and backend independence'),
        role('desktop-store','Store fetch helper','Process',APP+'store_runtime.rs','Command::new(helper)',
             'Downloads bounded bytes; the owner verifies trust and authorizes installation.','Trust and installation'),
        role('desktop-session','Desktop session manager','System role',APP+'service.rs','ExecStart=',
             'May start the owner without a window; operating systems supply distinct activation mechanisms.','Native desktop session startup'),
    ]
    android_manifest = 'android/app/src/main/AndroidManifest.xml'
    android = [
        role('android-app','Android app process','Process',android_manifest,'android:name=\".MainActivity\"',
             'The activity, shared window and owner live here; they are logical roles, not separate desktop processes.','Android window lifecycle'),
        role('android-wasm','Android isolated WASM worker','Process',android_manifest,'android:name=\".WasmPluginService\"',
             'The owner binds an OS-managed isolated worker; the worker runs WASM and can rasterize its own canvas.','Extension-rendered canvases'),
        role('android-store','Android store worker','Process',android_manifest,'android:name=\".StoreFetchService\"',
             'Separate non-exported service under the app UID, not the isolated WASM worker.','Trust and installation'),
        role('android-service','Android transfer service','Logical role',android_manifest,'android:name=\".TransferService\"',
             'Maintains the connected-device lifecycle; the owner is not spawned as a desktop executable.','Android window lifecycle'),
    ]
    android_native = role('android-native','Community native worker','Process',
                          'android/app/src/community/java/dev/peersus/app/NativePluginService.java',
                          'class NativePluginService',
                          'Only the community build permits this native worker. It runs with the application UID.',
                          'Extensions and backend independence')
    android.append(android_native)
    network = [
        role('remote-owner','Paired peer owner','Peer role',NET+'network_host.rs','Request::File { offer } =>',
             'The other device applies its own rules and user decisions. The map shows no actual peers.','Transfers and relay configuration'),
        role('relay','Optional relay','Transport role',NET+'relay_policy.rs','use ',
             'A fallback path for encrypted transport; it is not the remote application owner.','Transfers and relay configuration'),
        role('invitation','Invitation admission','Wire type',NET+'handshake.rs','pub enum Purpose',
             'The device that offered the code chooses the admissible purpose. A files-only invitation does not pair devices.','Transfers and relay configuration'),
        role('streams','Paired stream requests','Wire type',NET+'streams.rs','pub enum Request',
             'Typed bidirectional stream preambles route file traffic and extensions independently.','Transfers and relay configuration'),
        role('deadline','Offer deadline notice','Side channel',NET+'announce.rs','pub struct Announce',
             'An optional unidirectional announcement reports the receiving side’s acceptance deadline.','Transfers and relay configuration'),
    ]
    for path, enum, parent, prefix in [(NET+'streams.rs','Request',network[3],'stream'),(NET+'handshake.rs','Purpose',network[2],'purpose')]:
        for _, wire, at, fallback in enum_variants(g.text(path),enum,'kebab-case'):
            item=g.node(prefix+':'+wire,wire,'Wire variant',path,at,view='system',doc=(ARCH,'Transfers and relay configuration'),
                        note='Receive-only fallback for an unfamiliar stream kind; never sent.' if fallback else 'Declared wire discriminator; routing does not itself grant admission.')
            g.edge(parent,item,'defines')
    g.edge(desktop[0],desktop[1],'starts if absent')
    g.edge(desktop[0],desktop[1],'local control','Commands and snapshots; window holds a lease')
    g.edge(desktop[6],desktop[1],'may activate')
    for child in desktop[2:6]:
        g.edge(desktop[1],child,'launches')
    standalone = role('desktop-standalone','Standalone extension process','Process','crates/peersus-plugin/src/service.rs',
                      'fn process_launcher','A separately launched executable, not a library in the native helper and not a WASM sandbox.',
                      'Extensions and backend independence')
    desktop.append(standalone)
    g.edge(desktop[1],standalone,'may supervise')
    g.edge(desktop[4],desktop[0],'canvas pixels','Owner authorizes the claim; the worker delivers the raster')
    g.edge(android[0],android[1],'binds isolated worker')
    g.edge(android[0],android[2],'binds store service')
    g.edge(android[0],android_native,'may bind in Community')
    g.edge(android[3],android[0],'supports owner lifecycle')
    g.edge(android[1],android[0],'canvas pixels','Validated frame becomes available to the shared window')
    for owner in (desktop[1],android[0]):
        g.edge(owner,network[0],'authenticated peer session')
        g.edge(owner,network[1],'may relay through')
    for item in network[2:]:
        g.edge(network[0],item,'exchanges')
    for owner, name, binary in [(desktop[0],'peersus-ui','peersus-ui'),(desktop[1],'peersus-app','peersus'),
                                (desktop[2],'peersus-plugin-runtime','peersus-wasm-host'),(desktop[3],'peersus-plugin-canvas','peersus-wasm-canvas-host'),
                                (desktop[5],'peersus-store-fetch','peersus-store-fetch'),(android[0],'peersus-ui-android','peersus_android')]:
        g.edge(owner,f'target:{name}:{binary}','implemented by')
    g.edge(android[1],'crate:peersus-plugin-android','implemented by')
    g.view('system','Where things run','What runs where?',
           'Follow a local window into its owner, then across a worker or device boundary. These are architecture roles, not live devices.',
           'Each card is a program or role. Read the groups from top to bottom: a computer, an Android phone, then what two devices share.',
           [('Desktop',desktop,'The programs on a computer: the window, the owner behind it and the helpers it starts.'),
            ('Android',android,'The same roles inside one app, plus Android’s own isolated workers.'),
            ('Between devices',network,'What two paired devices exchange, and the optional relay.')])


def walkthrough(g):
    steps = [
        ('pick','Choose a file and a paired peer','Sender window',UI+'send_page.rs','.send_picked(',
         'The person selects what to send and to whom. The guest broker is not involved in this native transfer.'),
        ('handoff','Queue the selected handle','Sender owner',APP+'runtime.rs','Command::QueueHeld {',
         'The owner admits the handoff and queues the held file for this destination. Queued does not mean received.'),
        ('offer','Send the typed offer','Peer connection',NET+'file_session.rs','pub async fn send(',
         'The authenticated session opens a file stream carrying an offer. The payload waits for acceptance.'),
        ('decision','Ask the receiver','Receiver window',UI+'receive_page.rs','fn accept(',
         'The receiver can accept and choose where it lands, decline or let the offer time out. That is separate from pairing.'),
        ('refused','Stop before sending data','Receiver decision',NET+'file_session.rs','let Some(directory) = answered',
         'Refusal, cancellation or an expired decision stops this offer before payload transfer.'),
        ('bytes','Send the accepted payload','Peer connection',NET+'file_session.rs','source.send(&mut tx',
         'After Accepted, bytes travel to the receiving owner. A local write is not confirmation of delivery.'),
        ('verify','Verify and publish the file','Receiver owner',NET+'file_session.rs','transfer::receive(&mut rx',
         'The receiver stages the file, checks its digest and publishes it without overwriting an existing file.'),
        ('failed','Report that it could not be kept','Receiver owner',NET+'file_session.rs','Err(error) => {',
         'A write or verification failure after consent is not the same thing as a declined offer.'),
        ('complete','Confirm verified arrival','Sender owner',NET+'file_session.rs','Ok(Reply::Complete)',
         'Only the completion reply confirms verified arrival. The sender can now report success.'),
    ]
    ids = []
    for slug,title,lane,path,anchor,note in steps:
        ids.append(g.node('send:'+slug,title,'Trace step',path,anchor,view='send',note=note,
                          doc=(ARCH,'Transfers and relay configuration'),lane=lane))
    for lhs,rhs,kind in [(0,1,'hands off'),(1,2,'offers'),(2,3,'asks'),(3,4,'if declined or expired'),(3,5,'if accepted'),
                         (5,6,'stages bytes'),(6,7,'if verification or write fails'),(6,8,'if committed and acknowledged')]:
        g.edge(ids[lhs],ids[rhs],kind)
    for i,role in enumerate(['desktop-window','desktop-owner','remote-owner','remote-owner','remote-owner','remote-owner','remote-owner','remote-owner','desktop-owner']):
        g.edge(ids[i],'role:'+role,'runs through')
    g.view('send','File journey','Follow one file',
           'Walk the ordinary paired-file path. The two exits show what changes before consent and after bytes start moving.',
           'Read the cards from top to bottom. The line between two cards names what moves the file on; an exit card shows where it stops instead.',
           [('From the sender',ids[:3],'The sender picks a file and a peer, and the owner offers it.'),
            ('At the receiver',ids[3:6],'The receiver decides. Nothing moves before it accepts.'),
            ('Verify and finish',ids[6:],'The receiver checks the file; only its reply confirms arrival.')])


def delivery(g):
    rows = [
        ('dev','Development build','dev.sh','scripts/dev.py','Builds desktop or Android development artifacts; does not itself install.'),
        ('deploy','Development install','scripts/dev.py','deploy_selected(args.devices','Builds fresh, installs and restarts on selected targets, then updates eligible installed extensions.'),
        ('build','Release preparation','build.sh','exec cargo xtask','Prepares local release artifacts without signing, publishing or deploying.'),
        ('check','Quality gate','check.sh','exec cargo xtask','Enters the strict checks, separate from build approval.'),
        ('xtask','Task dispatcher','xtask/src/main.rs','fn task(','Dispatches desktop builds, Android builds and metadata, compilation, lint, test and audit checks.'),
        ('android','Android artifacts','xtask/src/main.rs','\"android\" =>','Builds release Android variants; Gradle produces packages and bundles.'),
        ('artifacts','Unsigned distribution artifacts','.github/workflows/artifacts.yml','workflow_dispatch:','Manually prepares artifacts; this workflow does not create a release.'),
        ('release','Development release','.github/workflows/release.yml','workflow_dispatch:','On explicit request builds the development channel, signs it with the development update key and the fixed Android key, and publishes one prerelease.'),
        ('submit','Extension source submission','docs/extension-store.md','**Submit.**','Each extension comes through a source submission, separate from application main.'),
        ('validate','Review and validate extension','.github/workflows/extension-validate.yml','name:','Review readiness, untrusted build and independent output validation are separate.'),
        ('approve','Protected approval','.github/workflows/extension-approve.yml','name:','Verifies the reviewed inputs before signing and staging. Approval is not publication.'),
        ('publish','Publish approved archive','docs/extension-store.md','**Publish.**','A separate authorized release publishes exactly the approved archive and records.'),
        ('install','Verify and install extension','docs/extension-store.md','**Install.**','The owner verifies provenance and asks for agreement; new grants are not automatic.'),
        ('preview','Stage a website preview','.github/workflows/pages.yml','python3 site/build.py --output','Stages files in a new output directory. The preview workflow uploads an artifact, not a deployment.'),
    ]
    ids = {}
    for key,label,path,anchor,note in rows:
        docs = ('docs/extension-store.md','The chain') if key in ('submit','validate','approve','publish','install') else (ARCH,'Reading and validating changes')
        ids[key]=g.node('delivery:'+key,label,'Stage',path,anchor,view='delivery',note=note,doc=docs)
    for a,b,relation in [('dev','xtask','delegates'),('deploy','dev','requires fresh build'),('build','xtask','delegates'),('check','xtask','delegates'),
                         ('xtask','android','builds'),('artifacts','build','uses release task'),('artifacts','android','uses Android release task'),
                         ('release','xtask','uses development tasks'),
                         ('submit','validate','checked by'),('validate','approve','provides reviewed evidence'),('approve','publish','stages, does not publish'),
                         ('publish','install','supplies verified packages')]:
        g.edge(ids[a],ids[b],relation)
    g.view('delivery','Delivery','From checkout to device',
           'Local builds, installation and publication are different actions. Follow an entry point through the work it actually starts.',
           'Each card is one stage. The arrow in the details panel says what starts the next stage; building, installing and publishing never imply each other.',
           [('Local commands',[ids[i] for i in ('dev','deploy','build','check','xtask','android')],
             'What you run in a checkout: builds, installs and the quality gate.'),
            ('Review and distribution',[ids[i] for i in ('artifacts','submit','validate','approve','publish','install')],
             'How an extension gets from a source submission to a verified install.'),
            ('Website staging',[ids['preview']],'How the site is staged for a preview.')])


CRATE_CARDS = {
    'api': ('Extension contract', 'Defines how the host and extensions talk.'),
    'app': ('Application owner', 'Runs the local state, transfers, and extensions.'),
    'clipboard-platform': ('System clipboard', 'Connects to the platform’s system clipboard.'),
    'core': ('Rules and state', 'Defines shared rules, preferences, and commands.'),
    'diagnostics': ('Diagnostics', 'Records what the app and its helpers report.'),
    'net': ('Peer transport', 'Connects paired devices and transfers their files.'),
    'plugin': ('Extension broker', 'Checks and routes requests to extensions.'),
    'plugin-android': ('Android worker bridge', 'Hands Android’s worker channels to the interpreter.'),
    'plugin-canvas': ('Canvas worker', 'Draws extension canvases inside the worker.'),
    'plugin-runtime': ('Extension interpreter', 'Runs a bounded interpreter in the worker.'),
    'render': ('Canvas rasterizer', 'Turns an extension’s drawing instructions into pixels.'),
    'store-fetch': ('Store downloader', 'Downloads store bytes without deciding what to trust.'),
    'ui': ('Shared window', 'Shows the owner’s state and forwards actions.'),
    'ui-android': ('Android entry point', 'Connects the shared window and owner to Android.'),
    'xtask': ('Build tools', 'Runs the commands that build and check Peersus.'),
}
MODULE_NOTES = {
    UI+'link.rs': 'Passes window actions to the owner and reads the state it sends back.',
    UI+'link/session.rs': 'Finds an existing desktop owner or starts one for the window.',
    UI+'send_page.rs': 'Lets the person choose what to send and which paired device should get it.',
    UI+'receive_page.rs': 'Lets the person accept or decline an offer and pick where to save it.',
    UI+'canvas_view.rs': 'Displays finished extension pixels and forwards the person’s input.',
    UI+'agreement_flow.rs': 'Controls how the window presents the licence and notice before installation.',
    UI+'view.rs': 'Decides what the shared window should show from the state it has received.',
    APP+'runtime.rs': 'Applies commands, runs the application, and publishes its state.',
    APP+'runtime/host_contract.rs': 'Offers host services only to extensions that declare they use them.',
    APP+'runtime/permissions.rs': 'Checks which permissions a caller has before admitting work.',
    APP+'runtime/transfers.rs': 'Coordinates file transfers and extension data between paired devices.',
    APP+'runtime/plugin_calls.rs': 'Routes replies and ends calls that are no longer allowed to continue.',
    APP+'runtime/packages.rs': 'Stages, confirms, replaces, and removes extension packages.',
    APP+'store_runtime.rs': 'Asks a separate helper to fetch bytes without giving it trust decisions.',
    APP+'store_service.rs': 'Checks Store approvals and matches downloaded archives to their records.',
    APP+'input_follow.rs': 'Moves a local pointer to an arranged device and returns it when the visit ends.',
    APP+'desktop_capture.rs': 'Receives pointer and key input from the desktop’s capture portal.',
    APP+'desktop_input.rs': 'Replays approved input through the desktop’s native input service.',
    APP+'lifetime.rs': 'Tracks when the owner can stop after the last window leaves.',
    APP+'ipc.rs': 'Carries desktop window and command-line requests to the application owner.',
    APP+'binary_metadata.rs': 'Reads the identity and description from a program file without running it.',
    APP+'board.rs': 'Keeps the result of an action so the window can show it beside the button.',
    APP+'catalog.rs': 'Checks what the maintainer’s approval record proves about an extension package.',
    APP+'cli.rs': 'Turns command-line options into the same validated commands an app window sends.',
    APP+'clipboard_handles.rs': 'Lets an extension exchange clipboard text through the platform’s clipboard adapter.',
    APP+'displays.rs': 'Shares a description of the desktop displays with paired devices for arranging screen edges.',
    APP+'in_flight.rs': 'Tracks calls the owner has made to extensions and is still waiting on.',
    APP+'instance.rs': 'Keeps one owner and a local place for programs run by the same person to reach it.',
    APP+'page_frames.rs': 'Admits a mounted window to receive an extension’s finished pictures.',
    APP+'parked.rs': 'Holds work from another device until the corresponding extension is ready to collect it.',
    APP+'permits.rs': 'Tracks what an otherwise hidden device was opened for and how long that lasts.',
    APP+'platform_calls.rs': 'Keeps unfinished platform work with the owner rather than blocking the dispatch thread.',
    APP+'platform_services.rs': 'Defines the platform operations the owner can offer to sandboxed extensions.',
    APP+'platform_services/snapshot.rs': 'Works with a file the person selected rather than a path supplied by an extension.',
    APP+'plugin_manager.rs': 'Installs, lists, and removes the files that make up an extension package.',
    APP+'plugin_state.rs': 'Checks the extension’s folder to tell what state its installation is in.',
    APP+'private_store.rs': 'Keeps local development distribution separate from production Store approvals.',
    APP+'receive.rs': 'Carries what a paired device sent to the extension that was asked to take it.',
    APP+'running.rs': 'Tracks extension transfers that are already carrying data.',
    APP+'runtime/admission.rs': 'Checks whether another device’s request may reach an extension before keeping it.',
    APP+'runtime/refusals.rs': 'Remembers when a paired device has declined an extension request and when to try again.',
    APP+'statuses.rs': 'Tracks the statuses extensions define and which of them are active on this device.',
    APP+'store_trust.rs': 'Defines which Store approvals the owner trusts and stops it going back to older catalog records.',
    APP+'update_install.rs': 'Replaces the application files when it was installed as a plain folder of programs.',
    NET+'network_host.rs': 'Admits connections and routes stream requests to the right handler.',
    NET+'file_session.rs': 'Exchanges a file offer, waits for acceptance, and carries the file and its result.',
    NET+'transfer.rs': 'Stages a received file and checks its bytes before making it available.',
    NET+'streams.rs': 'Names the different conversations two paired devices can open.',
    NET+'pairing_tasks.rs': 'Runs pairing in the background and remembers a peer only after mutual consent.',
    NET+'ceremony.rs': 'Exchanges sealed choices before either person’s number can be revealed.',
    NET+'handshake.rs': 'Checks an invitation and admits only the purpose promised by its code.',
    NET+'relay_policy.rs': 'Chooses or replaces a relay when a route is needed.',
    NET+'announce.rs': 'Carries an optional deadline for the receiver’s choice on a file offer.',
    'crates/peersus-clipboard-platform/src/owner.rs': 'Runs system clipboard operations on a single owning thread.',
    'crates/peersus-clipboard-platform/src/watch.rs': 'Listens for clipboard changes from the Linux desktop.',
    'crates/peersus-core/src/config.rs': 'Holds preferences that are shared by every way of opening Peersus.',
    'crates/peersus-core/src/model.rs': 'Defines the commands and state shared by the owner and its windows.',
    'crates/peersus-core/src/pairing.rs': 'Checks that both people independently picked the same number.',
    API+'manifest.rs': 'Describes what an extension offers, asks for, and may do if allowed.',
    API+'message.rs': 'Defines the calls, replies, and events the broker understands.',
    API+'elements.rs': 'Describes the controls an extension can put on its pages.',
    API+'canvas.rs': 'Defines the format for a guest’s drawing instructions and finished pixels.',
    API+'platform.rs': 'Describes the device in a portable format without giving extensions platform permissions.',
    'crates/peersus-plugin/src/module.rs': 'Chooses a trusted launcher suited to the extension’s execution kind.',
    'crates/peersus-plugin/src/hub.rs': 'Matches a request to its provider, version, member, and permission.',
    'crates/peersus-ui-android/src/plugins.rs': 'Asks Android to bind a helper service and hands it the worker channels.',
}
EXTENSION_NOTES = {
    'Clipboard': 'Shares clipboard items with paired devices that are allowed to receive them.',
    'Control': 'Lets a person use one device’s input to control another with permission.',
    'Location': 'Shows device locations when the user has granted permission.',
    'Monitor': 'Shows approved information about how a paired device is running.',
    'Test': 'Exercises the extension interfaces from a guest rather than from the host.',
    'Upload': 'Lets a person select a file and send it to an approved web destination.',
}

# Positions in this list are the three rows of the overview, not the hierarchy of the code.
OVERVIEW = [
    ('role:desktop-window', 'Desktop window', 'Shows the app and sends actions to the owner.', 'ui'),
    ('role:desktop-owner', 'Desktop owner', 'Keeps local state and runs transfers.', 'service'),
    ('role:remote-owner', 'Other paired device', 'Decides what to accept and keeps its own state.', 'ui'),
    ('role:desktop-store', 'Store fetch helper', 'Downloads store bytes for the owner to check.', 'worker'),
    ('desktop-workers', 'Extension workers', 'Run desktop extensions separately from the owner.', 'worker'),
    ('role:relay', 'Optional relay', 'Carries sealed traffic when a direct route fails.', 'external'),
    ('role:android-app', 'Android app', 'Runs the window and owner together.', 'ui'),
    ('role:android-wasm', 'Isolated Android worker', 'Runs extensions in an isolated Android service.', 'worker'),
    ('extensions', 'Six extensions', 'Add optional features without changing Peersus.', 'feature'),
]
SEND_CARDS = {
    'pick': ('Choose a file', 'Pick a file and the paired device it should go to.', 'ui'),
    'handoff': ('Queue the file', 'The sender’s owner takes the file handle and queues the work.', 'service'),
    'offer': ('Send the offer', 'The owners open an authenticated stream carrying a file offer.', 'data'),
    'decision': ('Ask the receiver', 'The other person chooses whether and where to save the file.', 'ui'),
    'refused': ('Stop before sending', 'Refusal or expiry ends the offer before the file moves.', 'feature'),
    'bytes': ('Send the bytes', 'File data travels only after the receiver accepts.', 'data'),
    'verify': ('Verify and save', 'The receiving owner checks the digest before keeping the file.', 'service'),
    'failed': ('Report a write failure', 'The sender is told that the received bytes could not be kept.', 'feature'),
    'complete': ('Confirm arrival', 'The receiving owner acknowledges that it verified and saved the file.', 'service'),
}
DELIVERY_CARDS = {
    'dev': ('Development build', 'Builds development artifacts without installing them.'),
    'deploy': ('Development install', 'Builds, installs, and restarts on selected targets.'),
    'build': ('Release build', 'Prepares local release files without publishing them.'),
    'check': ('Quality gate', 'Runs the project’s checks before a change ships.'),
    'xtask': ('Task dispatcher', 'Runs the matching build, packaging, or check task.'),
    'android': ('Android artifacts', 'Builds the Android release packages and bundles.'),
    'artifacts': ('Unsigned artifacts', 'Produces distribution files without creating a release.'),
    'submit': ('Submit source', 'Proposes the extension on its separate branch.'),
    'validate': ('Validate extension', 'Builds untrusted source and checks its output independently.'),
    'approve': ('Protected approval', 'Checks reviewed evidence before signing and staging.'),
    'publish': ('Publish archive', 'Publishes the exact approved package and records.'),
    'install': ('Install package', 'Checks trust and consent without granting permissions.'),
    'preview': ('Site preview', 'Stages a website artifact without deploying it.'),
}


def guided_flows(g):
    """The standalone subjects the inventory does not already walk through."""
    packages = APP+'runtime/packages.rs'
    pair = 'crates/peersus-core/src/pairing.rs'
    follow, transfers, replay = APP+'input_follow.rs', APP+'runtime/transfers.rs', APP+'desktop_input.rs'
    leased = 'fn hold_outbound_visit' in g.text(transfers)
    capture = (follow, 'async fn capture_turn' if leased else 'pub async fn follow')
    carrier = g.extension('control', 'wasm/src/remote.rs')
    dispatch = (carrier, 'pub fn handle<') if carrier else (follow, 'if let Some(plugin) = &route.carrier' if leased else 'let Some(plugin) = carrier else')
    visit = (transfers, 'fn hold_outbound_visit') if leased else (follow, 'let send = |route:')
    replay_anchor = 'fn dispatch_leased' if leased else 'pub fn dispatch(&self, call: Call)'
    release = (transfers, 'fn close_inbound_visits') if leased else (follow, 'Captured::Left =>')
    return [
        ('control-visit', 'A Control visit', 'Follow a deliberate desktop crossing until input is handed back.',
         (ARCH, 'Input carried across screen edges'), [
             ('Cross an edge', 'ui', *capture, 'Captured input',
              'The Linux owner captures pointer and key input when a person crosses an eligible edge.'),
             ('Hand input to Control', 'feature', *dispatch, 'Visit request',
              'The declared carrier forwards input without the owner knowing any extension by name.'),
             ('Open the visit', 'service', *visit, 'Permitted input samples',
              'The owners hold a paired visit stream open while the separate data stream can change.' if leased else
              'The owner hands input to the declared carrier, in order, for the visit to the other device.'),
             ('Replay approved input', 'worker', replay, replay_anchor, 'End of visit',
              'The other owner checks permission before its input adapter replays the operation.'),
             ('Give control back', 'feature', *release, None,
              'Leaving, disconnecting or losing permission closes the visit and releases the input it still holds.'),
         ]),
        ('extension-install', 'Install an extension', 'Follow Store bytes through verification, consent, and registration.',
         (ARCH, 'Trust and installation'), [
             ('Choose Install', 'ui', packages, 'fn start_store_install', 'Approved package request',
              'The owner starts with the signed entry for the selected extension.'),
             ('Fetch the archive', 'worker', packages, 'bytes: fetcher.fetch(', 'Bounded archive bytes',
              'A separate helper downloads the selected archive under a size limit.'),
             ('Check the package', 'service', APP+'store_service.rs', 'pub fn finish_install(', 'Verified staged package',
              'The owner checks the archive against its signed record and the package declarations.'),
             ('Read licence and notice', 'ui', packages, 'fn confirm_verified', 'Agreement confirmation',
              'If this licence and notice were not already accepted, the owner waits for the person’s choice.'),
             ('Install without grants', 'service', packages, 'fn confirm_install', None,
              'A first installation records acceptance and installs the package without granting permissions.'),
         ]),
        ('pairing', 'Pair two devices', 'Follow an authenticated invitation and the choices that establish trust.',
         ('AGENTS.md', 'Contractul de asociere'), [
             ('Use an invitation', 'ui', NET+'handshake.rs', 'pub async fn join', 'Authenticated invitation',
              'The invited device authenticates the connection and uses an invitation for pairing.'),
             ('Choose the same number', 'feature', NET+'pairing_tasks.rs', 'pub fn choose(', 'Local choice',
              'Both people pick one of the same three numbers, shown in a different order.'),
             ('Exchange sealed choices', 'service', pair, 'pub fn receive_commitment', 'Sealed commitments',
              'Both sides commit to their choices before either learns the other’s number.'),
             ('Check the numbers', 'data', pair, 'pub fn reveal(', 'Matching proofs',
              'Both sides reveal their choices, and pairing succeeds only if both proofs are valid and the numbers match.'),
             ('Remember the peer', 'service', NET+'pairing_tasks.rs', '=> peers.remember(', None,
              'Each side saves the other device only after mutual consent completes successfully.'),
         ]),
    ]


def dependency_layers(cards, pairs):
    """The cover graph of the dependency order, with mutual dependencies kept in one layer.

    A shortcut is removed only if another path preserves reachability. An irreducible
    edge may still span layers; the viewer passes it through only the inter-layer gutters.
    """
    outgoing = {node: set() for node in cards}
    for a, b in pairs:
        outgoing[a].add(b)

    def reachable(start):
        seen, pending = set(), [start]
        while pending:
            node = pending.pop()
            if node not in seen:
                seen.add(node)
                pending.extend(outgoing[node] - seen)
        return seen

    closure = {node: reachable(node) for node in cards}
    order = {node: i for i, node in enumerate(cards)}
    component = {
        node: min((other for other in closure[node] if node in closure[other]), key=order.__getitem__)
        for node in cards
    }
    condensed = {root: set() for root in component.values()}
    for a, b in pairs:
        start, end = component[a], component[b]
        if start != end:
            condensed[start].add(end)
    cover = {(a, b) for a, targets in condensed.items() for b in targets
             if not any(b in closure[via] for via in targets - {b})}
    reduced = [i for i, (a, b) in enumerate(pairs) if (component[a], component[b]) in cover]
    feedback = [i for i, (a, b) in enumerate(pairs) if component[a] == component[b]]

    @cache
    def distance(root):
        return max((1 + distance(other) for other in condensed[root]), default=0)

    far = max((distance(root) for root in condensed), default=0)
    ranks = {node: far - distance(component[node]) for node in cards}
    layers = [[node for node in cards if ranks[node] == rank] for rank in range(far + 1)]
    previous = defaultdict(list)
    for i in reduced:
        a, b = pairs[i]
        previous[b].append(a)
    positions = {}
    for layer in layers:
        def order_in_layer(node):
            parents = previous[node]
            return (sum(positions[p] for p in parents)/len(parents), order[node]) if parents else (1, order[node])
        layer.sort(key=order_in_layer)
        positions.update((node, (i+.5)/len(layer)) for i, node in enumerate(layer))
    return layers, reduced, feedback


def compile_diagrams(nodes, links, boards):
    """Project Floorplan's hierarchy once, including exact edges for later inspection."""
    items = {'n:'+node['id']: node for node in nodes}
    parent = {key: 'n:'+node['parent'] if node['parent'] else 'root' for key, node in items.items()}
    children = defaultdict(list)
    for key, owner in parent.items():
        children[owner].append(key)

    @cache
    def chain(item):
        return (item,) if item == 'root' else (item, *chain(parent[item]))

    diagram = {}
    ambient = [i for i, link in enumerate(links) if 'board' not in link and not link['structural']]
    explicit = defaultdict(list)
    for i, link in enumerate(links):
        if 'board' in link:
            explicit[link['board']].append(i)
    for container in dict.fromkeys((*boards, *children)):
        direct = container in boards
        visible = boards[container]['cards'] if direct else children[container]
        cards, ancestors = set(visible), set(chain(container))
        related = {}
        grouped = defaultdict(list)

        def project(item, link):
            for step in chain(item):
                if step in cards:
                    return step, None
                if step == container:
                    return None, None
            if item in ancestors:
                return None, None
            if link['labels'][0] == 'Depends on' and items[item].get('evidenceKind') == 'Crate':
                return None, item  # the dependency belongs to the crate, not its enclosing system
            lineage = chain(item)
            nearest = next(i for i, step in enumerate(lineage) if step in ancestors)
            return None, lineage[nearest - 1]

        def connector(side, item):
            key = f'x:{side}:{item}'
            if key in related or len(related) < 3:
                related[key] = item
                return key
            return None

        for i in explicit[container] or ambient:
            link = links[i]
            if direct:
                a, b = link['a'], link['b']
                if a not in cards or b not in cards:
                    continue
            else:
                a, outside_a = project(link['a'], link)
                b, outside_b = project(link['b'], link)
                if a and outside_b:
                    b = connector('out', outside_b)
                elif b and outside_a:
                    a = connector('in', outside_a)
            if a and b and a != b:
                grouped[a, b, link['k']].append(i)
        plan = dict(cards=visible, connectors=list(related))
        if len(grouped) > 15:
            # A normal and a target-specific declaration can connect the same pair.
            # Draw them once; each declaration still has its own record in the inspector.
            merged = defaultdict(list)
            for (a, b, _kind), records in grouped.items():
                merged[a, b].extend(records)
            layers, reduced, feedback = dependency_layers([*visible, *related], list(merged))
            plan.update(layers=layers, reduced=reduced, feedback=feedback)
            plan['edges'] = [(a, b, sorted(records)) for (a, b), records in merged.items()]
        else:
            plan['edges'] = [(a, b, records) for (a, b, _kind), records in grouped.items()]
        diagram[container] = plan
    return diagram


def floorplan(g, inventory):
    """Keep the inventory intact and project it into the offline floorplan/graph@1 model."""
    nodes, parents = {}, {}
    custom = {key: (label, summary, kind) for key, label, summary, kind in OVERVIEW}
    custom.update({'send:'+key: val for key, val in SEND_CARDS.items()})
    custom.update({'delivery:'+key: (*val, 'script') for key, val in DELIVERY_CARDS.items()})
    custom.update({
        'participant:host': ('Application services', 'Offers shared services to extensions that declare they use them.', 'service'),
        'participant:broker': ('Service broker', 'Checks who is calling and what they may ask for.', 'service'),
    })
    kinds = {'Crate': 'library', 'Program': 'worker', 'Library': 'library', 'Extension': 'feature',
             'Service': 'service', 'Setting': 'config', 'Page': 'ui', 'Permission': 'config',
             'Process': 'worker', 'Stage': 'script', 'Unresolved import': 'config', 'System role': 'infra',
             'Transport role': 'external', 'Side channel': 'data'}
    fact_keys = ('version', 'provider', 'permission', 'scope', 'consumers', 'platforms', 'execution',
                 'surface', 'features', 'defaultMember', 'lane')
    for node in inventory['nodes']:
        key, title, kind = node['id'], node['label'], node['kind']
        summary = node['note'] or 'One named option in its shared declaration.'
        if kind == 'Crate':
            title, summary = CRATE_CARDS.get(title.removeprefix('peersus-'), (title, summary))
        elif kind == 'Extension' and 'execution' in node:
            summary = EXTENSION_NOTES[title]
        elif kind == 'Module file':
            summary = MODULE_NOTES.get(node['source']['path']) or (
                f'Contains the {title.rsplit("::", 1)[-1].replace("_", " ")} code in this crate.')
        elif kind in ('Program', 'Library'):
            summary = ('Builds an executable for this crate.' if kind == 'Program'
                       else 'Builds the library that other parts use from this crate.')
        elif kind in ('Method', 'Event'):
            service = key.split(':')[2]
            summary = (f'Asks {service} to handle this operation.' if kind == 'Method'
                       else f'Tells subscribers when this {service} event occurs.')
        title, summary, floor_kind = custom.get(key, (title, summary, kinds.get(kind, 'feature')))
        facts = {field: node[field] for field in fact_keys if field in node}
        if title != node['label']:
            facts['identifier'] = node['label']
        nodes[key] = dict(label=title, kind=floor_kind, summary=summary, source=node['source'], doc=node['doc'],
                          inventoryId=key, facts=facts, evidenceKind=kind,
                          details=(node['note'] if kind != 'Module file' and node['note'] != summary else ''))

    def section(key, title, kind, summary, example, parent=None):
        nodes[key] = dict(label=title, kind=kind, summary=summary, source=nodes[example]['source'], doc=nodes[example]['doc'])
        if parent:
            parents[key] = parent

    for key, example in [('desktop-workers', 'role:desktop-wasm'), ('extensions', 'participant:broker')]:
        title, summary, kind = custom[key]
        section(key, title, kind, summary, example)
    section('wire-contract', 'Extension messages', 'library', 'Names the messages every extension and host understand.',
            'contract:Operation', 'extensions')
    section('relay-policy', 'Choose a relay', 'config', 'Orders fallback routes without changing which peers a person trusts.',
            'module:peersus-net:'+NET+'relay_policy.rs', 'role:relay')
    section('relay-settings', 'Relay settings', 'config', 'Defines how automatic and ordered fallback routes are configured.',
            'module:peersus-core:crates/peersus-core/src/relay.rs', 'role:relay')
    section('code', 'Code', 'library', 'Follow dependencies between the Rust crates.', 'crate:peersus-core')
    section('send', 'File journey', 'feature', 'Follow one file from selection to verified arrival.', 'send:pick')
    section('delivery', 'Delivery', 'script', 'Trace local builds, staging and extension publication.', 'delivery:dev')
    if any(n['kind'] == 'Unresolved import' for n in inventory['nodes']):
        section('unresolved-imports', 'Unmatched imports', 'config', 'Lists declarations with no matching local provider.',
                'participant:broker', 'participant:broker')
    for edge in inventory['edges']:
        a, b, rel = edge['from'], edge['to'], edge['kind']
        if rel in ('contains', 'defines', 'exports', 'contributes', 'declares') or (
                rel == 'builds' and b.startswith('target:')):
            parents[b] = a
    parents.update({
        'role:desktop-session': 'role:desktop-window', 'role:desktop-wasm': 'desktop-workers',
        'role:desktop-canvas': 'desktop-workers', 'role:desktop-native': 'desktop-workers',
        'role:desktop-standalone': 'desktop-workers', 'role:android-store': 'role:android-app',
        'role:android-service': 'role:android-app', 'role:android-native': 'role:android-app',
        'role:invitation': 'role:remote-owner', 'role:streams': 'role:remote-owner',
        'role:deadline': 'role:remote-owner', 'participant:host': 'extensions', 'participant:broker': 'extensions',
    })
    crate_owners = {
        'ui': 'role:desktop-window', 'api': 'wire-contract', 'plugin-runtime': 'desktop-workers',
        'plugin-canvas': 'desktop-workers', 'render': 'desktop-workers', 'store-fetch': 'role:desktop-store',
        'ui-android': 'role:android-app', 'plugin-android': 'role:android-wasm', 'xtask': 'delivery',
    }
    for key in nodes:
        family, sep, rest = key.partition(':')
        if not sep:
            continue
        owner = {'extension': 'extensions', 'contract': 'wire-contract', 'send': 'send', 'delivery': 'delivery',
                 'unresolved': 'unresolved-imports'}.get(family)
        if family == 'crate':
            owner = crate_owners.get(rest.removeprefix('peersus-'), 'role:desktop-owner')
        if owner:
            parents[key] = owner

    # Large crate folders are grouped by their actual source paths, not by guessed responsibilities.
    module_children = defaultdict(list)
    for key, owner in parents.items():
        if key.startswith('module:'):
            module_children[owner].append(key)
    for owner, members in sorted(module_children.items()):
        if len(members) <= 12:
            continue
        groups = defaultdict(list)
        for key in members:
            path = nodes[key]['source']['path'].split('/src/', 1)[-1]
            folder = path.rpartition('/')[0]
            groups[folder or 'src'].append(key)
        for folder, keys in sorted(groups.items()):
            keys.sort(key=lambda k: nodes[k]['source']['path'])
            for offset in range(0, len(keys), 12):
                chunk = keys[offset:offset+12]
                title = 'Top-level files' if folder == 'src' else folder.replace('_', ' ').capitalize()
                if len(keys) > 12:
                    title += f' · {offset+1}–{offset+len(chunk)}'
                key = f'files:{owner}:{folder}:{offset}'
                section(key, title, 'library', 'Groups source files from the same part of this crate.', chunk[0], owner)
                parents.update((member, key) for member in chunk)

    root = [row[0] for row in OVERVIEW]
    extra = [('root', root[a], root[b], kind, label, short) for a, b, kind, label, short in (
        (0, 1, 'rpc', 'Window actions and state snapshots', 'Actions'),
        (1, 2, 'data', 'Encrypted peer session', 'Session'),
        (1, 3, 'rpc', 'Download request and bounded store bytes', 'Store bytes'),
        (1, 4, 'rpc', 'Extension calls, events and replies', 'Calls'),
        (1, 5, 'config', 'Encrypted fallback traffic', 'Relay'),
        (5, 2, 'config', 'Encrypted fallback traffic', 'Fallback'),
        (6, 7, 'rpc', 'Worker channels and canvas frames', 'Frames'),
        (6, 2, 'data', 'Encrypted peer session', 'Peer data'),
        (4, 8, 'call', 'Declared extension entry points', 'Guest module'),
        (7, 8, 'call', 'Android-compatible extension code', 'Code'),
    )]
    extra.extend(('send', 'send:'+a, 'send:'+b, kind, label, label) for a, b, kind, label in (
        ('pick', 'handoff', 'call', 'Selected file handle'), ('handoff', 'offer', 'call', 'File offer'),
        ('offer', 'decision', 'data', 'Offer and file details'), ('decision', 'refused', 'config', 'Decline or timeout'),
        ('decision', 'bytes', 'call', 'Acceptance'), ('bytes', 'verify', 'data', 'File bytes and digest'),
        ('verify', 'failed', 'config', 'Write or verification error'), ('verify', 'complete', 'data', 'Completion reply'),
    ))
    extra.extend(('delivery', 'delivery:'+a, 'delivery:'+b, kind, label, caption) for a, b, kind, label, caption in (
        ('dev', 'xtask', 'call', 'Development build request', 'Build request'),
        ('deploy', 'dev', 'call', 'Fresh build request for selected targets', 'Fresh build'),
        ('build', 'xtask', 'call', 'Release build request', 'Release build'),
        ('check', 'xtask', 'call', 'Quality checks request', 'Checks'),
        ('xtask', 'android', 'call', 'Android build request', 'Android build'),
        ('artifacts', 'build', 'call', 'Distribution build job', 'Build job'),
        ('artifacts', 'android', 'call', 'Android build job', 'Android job'),
        ('submit', 'validate', 'config', 'Source and reviewed recipe', 'Submission'),
        ('validate', 'approve', 'config', 'Validated archives and review evidence', 'Review'),
        ('approve', 'publish', 'config', 'Signed and staged approval record', 'Approval'),
        ('publish', 'install', 'data', 'Published archive and approval record', 'Package'),
    ))
    views = {v['id']: v for v in inventory['views']}
    cards = lambda v: [key for lane in views[v]['lanes'] for key in lane['nodes']]
    broker_cards = ['participant:host', 'participant:broker', 'wire-contract',
                    *(key for key in cards('broker') if key.startswith('extension:'))]
    extra.extend(('extensions', guest, 'participant:host', 'rpc', 'Declared calls and event subscriptions', 'Calls')
                 for guest in broker_cards[3:] if 'execution' in g.nodes[guest])
    extra.extend([
        ('extensions', 'participant:host', 'participant:broker', 'rpc', 'Versioned requests and replies', 'Requests'),
        ('extensions', 'participant:broker', 'wire-contract', 'import', 'Shared service message formats', 'Formats'),
    ])
    boards = {
        'root': dict(cards=root, mobileCards=[root[i] for i in (0, 1, 3, 4, 2, 5, 6, 7, 8)],
                     read='Start at either app, then follow what crosses a process or device boundary.', groups=[
            # The panel groups roles by ownership while the board order follows its connections.
            ('Desktop', [root[i] for i in (0, 1, 3, 4)]), ('Android', [root[6], root[7]]),
            ('Across devices', [root[2], root[5]]), ('Optional features', [root[8]])]),
        'code': dict(cards=cards('code'), read='Follow dependencies layer by layer; select a crate to see every link.'),
        'extensions': dict(cards=broker_cards, title='Broker', read='Follow a declared call to the broker, then open an extension to see its contract.',
                           groups=[('Host and contract', broker_cards[:3]), ('Six extensions', broker_cards[3:])]),
        'send': dict(cards=['send:'+s for s in ('pick', 'handoff', 'offer', 'bytes', 'decision', 'refused',
                                                'verify', 'complete', 'failed')],
                     read='Follow the offer to the receiver; dashed branches show where the transfer stops.'),
        'delivery': dict(cards=cards('delivery'), columns=4,
                         read='Follow a command or approval step; building, installing and publishing are separate.'),
    }
    guides = guided_flows(g)
    boards['root'].update(kinds={'call': 'Extension code', 'config': 'Relay fallback'},
                          brief='System overview', title='Peersus', inspector='How Peersus fits together',
                          stats=[(len(cards('code')), 'Rust crates'), (len(broker_cards)-3, 'Extensions'),
                                 (1+len(guides), 'Guided flows')],
                          groupCaption={'Encrypted peer session': 'Session'},
                          captionPriority=['Relay', 'Guest module', 'Session'])
    boards['code'].update(adjacentLayers=True, showViolations=True, fillLayer='crate:xtask')
    boards['extensions'].update(kinds={'rpc': 'Broker calls'}, badge=f'{len(broker_cards)-3} total',
                                priorityTo='participant:host', priorityFrom=broker_cards[3:])
    boards['send']['kinds'] = {'call': 'Handoff and consent', 'config': 'Exit branch'}
    boards['delivery']['kinds'] = {'call': 'Commands and jobs', 'config': 'Review and approval', 'data': 'Artifacts'}
    for board in ('code', 'delivery'):
        boards[board]['groups'] = [(lane['label'], lane['nodes']) for lane in views[board]['lanes']]
    flows = [('send', 'Send a file', 'Follow an ordinary paired transfer, from the picker to verified arrival.',
              ['send:'+s for s in ('pick', 'handoff', 'offer', 'decision', 'bytes', 'verify', 'complete')])]
    for key, title, intro, doc, steps in guides:
        refs = []
        for i, (label, kind, path, anchor, edge, note) in enumerate(steps, 1):
            step_id = f'{key}:{i}'
            nodes[step_id] = dict(label=label, kind=kind, summary=note, source=g.source(path, anchor),
                                  doc=g.documentation(doc), evidenceKind='Trace step')
            parents[step_id] = key
            refs.append(step_id)
            if edge:
                extra.append((key, step_id, f'{key}:{i+1}', 'call', edge, edge))
        section(key, title, 'feature', intro, refs[0])
        boards[key] = dict(cards=refs, read=intro)
        flows.append((key, title, intro, refs))

    ids = {key: re.sub(r'[^A-Za-z0-9_.-]', '-', key) for key in nodes}
    reverse_ids = {ident: key for key, ident in ids.items()}
    if len(ids) != len(reverse_ids) or not set(parents.values()) <= nodes.keys():
        raise ValueError('Floorplan hierarchy has an ambiguous or missing identifier')

    def ref(key):
        return 'root' if key == 'root' else 'n:' + ids[key]

    # One file record per source even when many facts refer to different lines in it.
    priority = {'Module file': 0, 'Program': 1, 'Library': 1}
    files = {}
    for key, n in sorted(nodes.items(), key=lambda kv: (priority.get(kv[1].get('evidenceKind'), 2), kv[0])):
        p = n['source']['path']
        if p not in files:
            files[p] = dict(p=p, n=ids[key], r='.', l=Path(p).suffix[1:] or 'source', loc=len(g.text(p).splitlines()),
                            name=n['label'], d=n['summary'])
    stats = defaultdict(Counter)
    lang_lines = defaultdict(Counter)
    for file in files.values():
        owner, seen = reverse_ids[file['n']], set()
        while owner:
            if owner in seen:
                raise ValueError('Floorplan hierarchy contains a cycle')
            seen.add(owner)
            stats[owner].update(files=1, loc=file['loc'])
            lang_lines[owner][file['l']] += file['loc']
            owner = parents.get(owner)
    projected = [dict(n, id=ids[key], parent=ids[parents[key]] if key in parents else None,
                      paths=[n['source']['path']], tags=[], packages=[], hosts=[],
                      stats=dict(files=stats[key]['files'], loc=stats[key]['loc'], langs=dict(lang_lines[key]),
                                 repos=['.'] if stats[key]['files'] else []))
                 for key, n in nodes.items()]
    relations = {
        'depends on': 'import', 'module references': 'import', 'imports': 'rpc', 'asks the host': 'rpc',
        'subscribes to': 'event', 'authenticated peer session': 'data', 'canvas pixels': 'data', 'local control': 'rpc',
        'may relay through': 'config', 'binds store service': 'rpc', 'binds isolated worker': 'rpc',
        'blocked by contract': 'config', 'unresolved import': 'config', 'requires': 'config',
        'dispatches to': 'rpc', 'may invoke': 'rpc', 'may route paired stream': 'rpc', 'registers and routes through': 'rpc',
    }
    structural = {'contains', 'contributes', 'defines', 'declares', 'exports', 'implemented by', 'runs through'}

    def link(a, b, kind, label, detail='', **fields):
        return dict(a=ref(a), b=ref(b), k=kind, w=1, labels=[label], desc=detail, **fields)

    links = [link(e['from'], e['to'], ('config' if e['kind'] == 'depends on' and not e['detail'].startswith('normal')
                                     else relations.get(e['kind'], 'call')),
                  e['kind'].capitalize(), e['detail'], structural=e['kind'] in structural)
             for e in inventory['edges']]
    links.extend(link(a, b, kind, label, board=ref(board), caption=caption)
                 for board, a, b, kind, label, caption in extra)
    for board in boards.values():
        board['cards'] = [ref(key) for key in board['cards']]
        if 'mobileCards' in board:
            board['mobileCards'] = [ref(key) for key in board['mobileCards']]
        if 'groups' in board:
            board['groups'] = [(title, [ref(key) for key in keys]) for title, keys in board['groups']]
        for key in ('fillLayer', 'priorityTo'):
            if key in board:
                board[key] = ref(board[key])
        if 'priorityFrom' in board:
            board['priorityFrom'] = [ref(key) for key in board['priorityFrom']]
    tour = [
        (root[0], 'Begin at the window', 'The window shows the owner’s state and passes your actions back to it.'),
        (root[1], 'Find the owner', 'The owner keeps the network, saved state and permission decisions.'),
        (root[2], 'Cross to another device', 'The receiving device makes its own decisions; the relay is only a fallback path.'),
        (root[7], 'Look at the Android worker', 'The app houses its window and owner; a separate service runs extension code.'),
        (root[8], 'Read an extension contract', 'The extension supplies its own feature; the broker checks its declarations and permissions.'),
    ]
    return dict(schema=SCHEMA,
                meta=dict(root=None, rootName='Peersus', audience='everyone', agent='none', repos=[],
                          counts=dict(files=len(files), loc=sum(f['loc'] for f in files.values()), unassigned=0)),
                project=dict(name='Peersus', summary='Peersus moves files between paired devices and waits for the receiver to accept.',
                             entrypoints=[nodes[k]['source']['path'] for k in (root[0], root[1], root[6])]),
                nodes=projected, files=[files[path] for path in sorted(files)], routes=[], links=links,
                flows=[dict(id=key, title=title, description=description,
                            steps=[dict(ref=ref(k), note=nodes[k].get('details') or nodes[k]['summary']) for k in steps])
                       for key, title, description, steps in flows],
                tour=[dict(ref=ref(key), title=title, note=note) for key, title, note in tour],
                peersus=dict(boards={ref(key): val for key, val in boards.items()},
                             diagrams=compile_diagrams(projected, links, {ref(key): val for key, val in boards.items()}),
                             views=[(ref(key), label) for key, label in (
                                 ('root', 'Overview'), ('code', 'Code'), ('extensions', 'Broker'),
                                 ('send', 'File journey'), ('delivery', 'Delivery'))],
                             tourAction={'title': 'Then follow a file', 'flow': flows[0][0]},
                             legend={'rpc': 'Local calls', 'data': 'Peer data', 'call': 'Steps and calls',
                                     'config': 'Conditional', 'import': 'Code dependencies', 'event': 'Events'},
                             counts=dict(crates=len(cards('code')), extensions=len(EXTENSION_NOTES), flows=len(flows)),
                             upstream=FLOORPLAN_REV, violations=inventory['violations']))


GEOMETRY = r'''const G_EPS = .015;
const G_CLEARANCE = 10;
const G_HAIRPIN_CLEARANCE = 24;
const G_FRAME_CLEARANCE = 28;
const G_PORT_SPACING = 14;
const gNear = (a,b) => Math.abs(a-b) <= G_EPS;
const gEqual = (a,b) => gNear(a[0],b[0]) && gNear(a[1],b[1]);
const gRange = (a,b,c,d) => Math.min(b,d)-Math.max(a,c);
const gBounds = ({x,y,w,h}) => [[x,x+w],[y,y+h]];
const gBoxOverlap = (a,b) => gRange(a.x,a.x+a.w,b.x,b.x+b.w)>G_EPS &&
  gRange(a.y,a.y+a.h,b.y,b.y+b.h)>G_EPS;
function gSegment(p,q) {
  const axis=gNear(p[1],q[1])?0:gNear(p[0],q[0])?1:-1;
  if(axis<0)return {axis};
  return {axis,on:p[1-axis],lo:Math.min(p[axis],q[axis]),hi:Math.max(p[axis],q[axis]),p,q};
}
function gSegments(wires) {
  return wires.flatMap(({id,points,color,radius=0})=>points.slice(1).map((q,i)=>({...gSegment(points[i],q),id,color,radius})));
}
function gHits(s,r,pad=0) {
  const ranges=gBounds(r), [a,b]=ranges[s.axis], [c,d]=ranges[1-s.axis];
  return gRange(s.lo,s.hi,a-pad,b+pad)>=-G_EPS && s.on>=c-pad-G_EPS && s.on<=d+pad+G_EPS;
}
function gParallel(s,t,distance=G_CLEARANCE) {
  return s.axis===t.axis && gRange(s.lo,s.hi,t.lo,t.hi)>G_EPS && Math.abs(s.on-t.on)<distance-G_EPS;
}
function gInFrame(p,r,radius=0) {
  const inset=G_FRAME_CLEARANCE+radius-G_EPS;
  return gBounds(r).every(([lo,hi],i)=>p[i]>=lo+inset&&p[i]<=hi-inset);
}
function checkGeometry({cards,labels,marks,wires,ports,gutter,frame}) {
  const problems=[], segments=gSegments(wires), box=new Map(cards.map(c=>[c.id,c]));
  if(!cards.length)return ['missing cards'];
  const range=[0,1].map(axis=>[Math.min(...cards.map(c=>gBounds(c)[axis][0]))-gutter,
    Math.max(...cards.map(c=>gBounds(c)[axis][1]))+gutter]);
  const out=p=>p.some((value,d)=>value<range[d][0]-G_EPS || value>range[d][1]+G_EPS);
  const encloses=(s,p)=>gNear(s.on,p[1-s.axis]) && p[s.axis]>=s.lo-G_EPS && p[s.axis]<=s.hi+G_EPS;
  for(const [i,card] of cards.entries())for(const next of cards.slice(i+1))
    if(gBoxOverlap(card,next))problems.push(`cards overlap: ${card.id}, ${next.id}`);
  for(const s of segments){
    if(s.axis<0){problems.push(`non-orthogonal segment: ${s.id}`);continue;}
    if(out(s.p)||out(s.q))problems.push(`gutter bounds: ${s.id}`);
    if(frame&&(!gInFrame(s.p,frame,s.radius)||!gInFrame(s.q,frame,s.radius)))problems.push(`frame clearance: ${s.id}`);
    for(const r of cards){
      const [[a,b],[c,d]]=s.axis===0?gBounds(r):gBounds(r).reverse();
      if(gHits(s,r)){
        const u=Math.max(s.lo,a),v=Math.min(s.hi,b);
        const touch=s.axis===0?[u,s.on]:[s.on,u];
        const owns=point=>ports.some(p=>p.card===r.id&&gEqual(p.p,point)&&(!p.owner||p.owner.includes(s.id)));
        if(u<v-G_EPS || ![s.p,s.q].some(p=>gEqual(p,touch)&&owns(p)))problems.push(`card contact: ${s.id}, ${r.id}`);
      }else if(gRange(s.lo,s.hi,a,b)>G_EPS && Math.min(Math.abs(s.on-c),Math.abs(s.on-d))<G_CLEARANCE-G_EPS)
        problems.push(`card clearance: ${s.id}, ${r.id}`);
    }
    for(const r of labels)if(r.owner!==s.id&&gHits(s,r))problems.push(`label crosses: ${r.id}, ${s.id}`);
    for(const r of marks)if(gHits(s,r))problems.push(`layer crosses: ${r.id}, ${s.id}`);
  }
  const orth=segments.filter(s=>s.axis>=0);
  for(const [i,s] of orth.entries())for(const t of orth.slice(i+1)){
    if(gParallel(s,t))problems.push(`parallel clearance: ${s.id}, ${t.id}`);
    else if(s.color&&s.color===t.color&&gParallel(s,t,G_HAIRPIN_CLEARANCE))
      problems.push(`hairpin clearance: ${s.id}, ${t.id}`);
  }
  for(const [i,r] of labels.entries()){
    for(const next of labels.slice(i+1))if(gBoxOverlap(r,next))problems.push(`label overlap: ${r.id}, ${next.id}`);
    for(const c of cards)if(gBoxOverlap(r,c))problems.push(`label touches card: ${r.id}, ${c.id}`);
    for(const c of marks)if(gBoxOverlap(r,c))problems.push(`label touches layer: ${r.id}, ${c.id}`);
  }
  const sides=new Map();
  for(const p of ports){
    if(out(p.p))problems.push(`gutter bounds: port ${p.card||p.side}`);
    if(frame&&!gInFrame(p.p,frame))problems.push(`frame clearance: port ${p.card||p.side}`);
    if(!p.card)continue;
    const r=box.get(p.card), axis='lr'.includes(p.side)?1:0, side='rb'.includes(p.side)?1:0;
    if(!r||!gNear(p.p[1-axis],gBounds(r)[1-axis][side])||!orth.some(s=>encloses(s,p.p)))
      problems.push(`port is detached: ${p.card}/${p.side}`);
    if(!r)continue;
    const [lo,hi]=gBounds(r)[axis], id=`${p.card}/${p.side}`;
    if(!sides.has(id))sides.set(id,{other:[],center:(lo+hi)/2});
    sides.get(id).other.push(p.p[axis]);
  }
  for(const [id,{other,center}] of sides){
    other.sort((a,b)=>a-b);
    for(let i=1;i<other.length;i++)if(other[i]-other[i-1]<G_PORT_SPACING-G_EPS)problems.push(`port spacing: ${id}`);
    if(!gNear((other[0]+other.at(-1))/2,center))problems.push(`port alignment: ${id}`);
  }
  return [...new Set(problems)];
}
'''


SKIN = r'''/* Everything outside the map uses unmodified site/style.css. */
#diagram, #palette {
  --mono: ui-monospace, "SF Mono", Menlo, Consolas, monospace;
  --k-rpc: var(--accent); --k-data: var(--blue); --k-config: var(--muted);
  --k-event: var(--blue); --k-call: var(--ink); --k-import: var(--blue);
}
#main.document { padding-block-start: 1.125rem; }
#map-source:not([hidden]) { display: block; margin-block-end: .5rem; font-size: .75rem; line-height: 1.5; letter-spacing: normal; }
#tools { position: relative; align-items: center; margin-block: 0 .625rem; }
#tools .store-search { flex: 0 1 21rem; min-width: min(100%, 15rem); }
#tools .button:not(.primary), #palette .button:not(.primary) { border-color: var(--line); color: var(--ink); }
#tools .button:not(.primary):hover, #palette .button:not(.primary):hover { border-color: var(--muted); }
#tools .store-search input { cursor: text; }
#tabs { justify-content: flex-start; flex-wrap: wrap; border-block: 1px solid var(--line); }
#tabs a[aria-current=page] { color: var(--accent); }
#crumbs { display: flex; flex-wrap: wrap; gap: .2rem .55rem; margin-block: .65rem; font-size: .875rem; color: var(--muted); }
#crumbs[hidden] { display: none; }
#diagram .map-layout { display: grid; grid-template-columns: minmax(0, 1fr) minmax(15rem, 23%); gap: 1rem; align-items: start; margin-top: .875rem; }
#diagram .map-layout.no-panel { grid-template-columns: minmax(0, 1fr); }
#diagram .map-frame, #panel { min-width: 0; border: 1px solid var(--line); border-radius: 1rem; }
#diagram .map-frame { overflow: hidden; background: var(--paper); }
#diagram .legend { display: flex; flex-wrap: wrap; align-items: center; gap: .4rem 1rem; padding: .75rem 1rem; border-bottom: 1px solid var(--line); color: var(--muted); font-size: .8125rem; line-height: 1.45; }
#diagram .legend-heading { display: inline-flex; flex-wrap: wrap; align-items: baseline; gap: .25rem .625rem; }
#diagram .legend-heading h2 { margin: 0; font-size: .9375rem; line-height: 1.35; letter-spacing: normal; color: var(--ink); }
#diagram .legend-heading strong { padding-inline-start: .625rem; border-inline-start: 1px solid var(--line); }
#diagram .legend-keys { display: flex; flex: 1 1 21rem; flex-wrap: wrap; align-items: center; min-width: 0; gap: .35rem .875rem; }
#diagram .legend strong { font: 700 .6875rem/1.5 var(--mono); letter-spacing: .08em; text-transform: uppercase; color: var(--ink); }
#diagram .legend span { display: inline-flex; align-items: center; gap: .35rem; }
#diagram .legend .legend-note { flex-basis: 100%; }
#diagram .legend i { display: block; width: 1rem; border-top: 2px solid var(--wire); }
#diagram .legend .k-config i { border-top-style: dashed; }
#diagram .legend .k-event i { border-top-style: dotted; }
#diagram .k-rpc{--wire:var(--k-rpc)} #diagram .k-data{--wire:var(--k-data)} #diagram .k-config{--wire:var(--k-config)}
#diagram .k-event{--wire:var(--k-event)} #diagram .k-call{--wire:var(--k-call)} #diagram .k-import{--wire:var(--k-import)}
#stage { position: relative; overflow: auto; outline-offset: -3px; scrollbar-width: thin; scrollbar-color: var(--line) var(--paper); height: var(--stage-height, 30rem); }
#diagram svg.i { width: 1rem; height: 1rem; fill: none; stroke: currentColor; stroke-width: 1.75; stroke-linecap: round; stroke-linejoin: round; flex: none; }
#svg { display:block; touch-action:auto; }
#zoom { display:flex; align-items:center; flex-wrap:wrap; gap: .3rem; padding:.5rem .75rem; background:var(--paper); border-top:1px solid var(--line); }
#zoom .button { min-height:2.75rem; width:2.75rem; padding:0; color:var(--ink); background:transparent; }
#zoom .button:hover { background:var(--panel); }
#zoom .button:disabled { opacity:.5; cursor:default; }
#scale { color:var(--muted); font:.6875rem var(--mono); min-width:4ch; margin-inline-start:.45rem; }
#zoom .hint { margin-inline-start:auto; color:var(--muted); font-size:.75rem; }
#panel { padding:1.5rem; background:var(--panel); }
#panel:focus { outline-offset: -3px; }
#panel .panel-top { display:none; }
#panel .sec { padding:1rem 0; border-top:1px solid var(--line); }
#panel .panel-top + .sec { padding-top:0; border-top:0; }
#panel .link-list summary { padding:.1rem 0; font-size:.875rem; }
#panel .link-list[open] summary { margin-block-end:.5rem; }
#panel .ph, #panel .group-h, #panel .evidence dt { color:var(--muted); font:700 .6875rem/1.5 var(--mono); letter-spacing:.08em; text-transform:uppercase; }
#panel .pt { margin:.4rem 0 .5rem; font-size:1.375rem; line-height:1.2; letter-spacing:-.02em; overflow-wrap:anywhere; text-wrap:pretty; }
#panel .pd { margin:.35rem 0 0; color:var(--muted); font-size:.9375rem; line-height:1.5; overflow-wrap:anywhere; }
#panel h3 { font-size:1rem; line-height:1.3; margin:0 0 .5rem; }
#panel .stats { display:grid; grid-template-columns:repeat(3,minmax(0,1fr)); gap:.375rem; margin-top:1rem; }
#panel .stats div { display:grid; align-content:start; gap:.2rem; min-width:0; padding:.5rem .32rem; border:1px solid var(--line); border-radius:.75rem; background:var(--paper); }
#panel .stats b { font-size:1.125rem; line-height:1.15; }
#panel .stats span { font-size:.6875rem; line-height:1.28; color:var(--muted); }
#panel .group-h { margin:.65rem 0 .18rem; }
#panel .group-h:first-of-type { margin-top:0; }
#diagram .row, #palette .row { width:100%; display:flex; align-items:flex-start; gap:.5rem; text-align:left; padding:.42rem .25rem; border:0; border-radius:.375rem; background:transparent; color:var(--ink); min-width:0; min-height:2.25rem; }
#diagram .row:focus-visible, #palette .row:focus-visible { outline-offset:-2px; }
#diagram .row:hover, #palette .row:hover { background:var(--paper); color:var(--accent); }
#diagram .row .dot, #palette .row .dot { margin:.5rem 0 0; width:.375rem; height:.375rem; flex:none; }
#diagram .rt, #palette .rt { min-width:0; flex:1; display:grid; gap:.1rem; }
#diagram .rn, #palette .rn { font-size:.875rem; font-weight:700; line-height:1.3; overflow-wrap:anywhere; }
#diagram .rd, #diagram .rc, #palette .rd, #palette .rc { color:var(--muted); font-size:.75rem; line-height:1.4; overflow-wrap:anywhere; }
#diagram .rc, #palette .rc { text-align:right; max-width:8ch; }
#panel .evidence { display:grid; gap:.6rem; margin:0; }
#panel .evidence dd { margin:0; font-size:.875rem; line-height:1.45; overflow-wrap:anywhere; }
#panel .evidence code { font:.72rem/1.5 var(--mono); color:var(--muted); }
#panel .evidence strong { display:block; }
#panel .panel-actions { display:flex; flex-wrap:wrap; gap:.5rem; margin-top:1rem; }
#diagram .map-card, .map-card-measure .map-card { background:var(--panel); border:1px solid var(--line); border-radius:1rem; width:100%; height:100%; display:grid; grid-template:auto 1fr auto/minmax(0,1fr); align-items:start; gap:.5rem; padding:1.25rem; position:relative; color:var(--ink); overflow-wrap:anywhere; }
#diagram .fo-pad { width:100%; height:100%; }
#diagram .node foreignObject { overflow:hidden; }
#diagram .map-card .head, .map-card-measure .head { display:flex; align-items:center; gap:.5rem; min-width:0; }
#diagram .map-card .ico, .map-card-measure .ico { display:grid; place-items:center; width:1.375rem; height:1.375rem; flex:none; color:var(--muted); }
#diagram .n-ui .ico, #diagram .n-service .ico { color:var(--accent); }
#diagram .map-card .ico svg, .map-card-measure .ico svg { width:1.1875rem; height:1.1875rem; fill:none; stroke:currentColor; stroke-width:1.8; stroke-linecap:round; stroke-linejoin:round; }
#diagram .map-card .title, .map-card-measure .title { font-size:.9375rem; font-weight:700; line-height:1.25; min-width:0; }
#diagram .map-card .desc, .map-card-measure .desc { font-size:.8125rem; line-height:1.4; color:var(--muted); }
#diagram .map-card .foot, .map-card-measure .foot { display:flex; align-items:center; justify-content:space-between; gap:.4rem; min-height:1.34rem; }
#diagram .map-card .meta, .map-card-measure .meta { font:700 .625rem/1.2 var(--mono); color:var(--muted); border:1px solid var(--line); border-radius:99rem; padding:.15rem .37rem; background:var(--paper); white-space:nowrap; }
#diagram .card-hit, .map-card-measure .card-hit { position:absolute; inset:0; width:100%; height:100%; border:0; border-radius:inherit; background:none; }
#diagram .card-hit:focus-visible { outline-offset:-3px; }
#diagram .map-card:hover { border-color:var(--muted); }
#diagram .node.sel .map-card { border-color:var(--accent); }
#diagram .map-card .drill, .map-card-measure .drill { z-index:1; display:grid; place-items:center; min-height:2.125rem; min-width:2.125rem; margin:-.3rem -.2rem -.35rem 0; padding:0; background:var(--paper); color:var(--muted); border:1px solid var(--line); border-radius:.3125rem; }
#diagram .map-card .drill:hover { color:var(--accent); border-color:var(--accent); }
#diagram .map-card.external, .map-card-measure .map-card.external { border-style:dashed; }
.map-card-measure { position:fixed; top:0; left:0; visibility:hidden; pointer-events:none; }
.map-card-measure .map-card { height:auto; }
#diagram .edge { color:var(--wire,var(--muted)); }
#diagram .edge .trace { fill:none; stroke:currentColor; stroke-opacity:.76; stroke-linejoin:round; stroke-width:1.6; }
#diagram .edge.k-config .trace { stroke-dasharray:5 5; }
#diagram .edge.k-event .trace { stroke-dasharray:2 4; }
#diagram .edge .trace.trunk { stroke-width:2.3; }
#diagram .port { fill:var(--paper); stroke:currentColor; stroke-width:1.4; }
#diagram .port.target { fill:currentColor; }
#diagram .edge .hit { fill:none; stroke:transparent; stroke-width:12; cursor:pointer; }
#diagram .edge:is(.hl,:hover,:focus-visible) .trace { stroke-opacity:1; stroke-width:2.35; }
#diagram svg.dim .node:not(.hl) .map-card { opacity:.55; }
#diagram svg.dim .edge:not(.hl) { opacity:.22; }
#diagram .wire-label, #diagram .layer-number { fill:var(--muted); font:700 .625rem var(--mono); text-anchor:middle; dominant-baseline:central; pointer-events:none; }
#diagram .layer-number { text-anchor:start; }
#diagram .wire-leader { fill:none; stroke:currentColor; stroke-width:1; pointer-events:none; }
#diagram .trunk-badge { fill:var(--paper); stroke:currentColor; stroke-width:1; }
#diagram .trunk-count { fill:currentColor; }
#flowmenu, #palette { border:1px solid var(--line); border-radius:.75rem; background:var(--panel); color:var(--ink); }
#flowmenu { position:absolute; z-index:3; top:calc(100% + .375rem); left:0; width:min(23rem, 100%); padding:.375rem; }
#flowmenu .row { padding:.55rem .7rem; }
#flowbar { display:grid; grid-template-columns:minmax(0,1fr) auto; gap:.75rem; align-items:center; padding:1rem; border-bottom:1px solid var(--line); background:var(--panel); }
#flowbar .fk { font:700 .6875rem/1.5 var(--mono); color:var(--accent); letter-spacing:.05em; text-transform:uppercase; }
#flowbar .controls { display:flex; gap:.35rem; }
#flowbar .controls .icon { width:3.125rem; padding:0; }
#flowbar p { margin:0; font-size:.9375rem; line-height:1.45; color:var(--muted); }
#palette { width:min(38rem,calc(100vw - 2rem)); max-height:calc(100svh - 2rem); padding:1.25rem; overflow:auto; }
#palette::backdrop { background:rgb(0 0 0 / .55); }
#palette .prompt { display:flex; gap:.5rem; }
#palette .store-search { min-width:0; }
#palette #results { margin-top:.5rem; }
#palette .search-caption { margin:0; padding:.65rem .2rem; font-size:.875rem; color:var(--muted); }
@media (max-width: 53rem) {
  #diagram .map-layout { grid-template-columns:minmax(0,1fr); }
  #panel .panel-top { display:flex; justify-content:flex-end; margin-bottom:1rem; }
  #zoom .hint { display:none; }
  #diagram .legend { font-size:.75rem; }
  #flowbar { grid-template-columns:1fr; }
  #flowbar .controls { justify-content:flex-end; }
}
@media (max-width: 35rem) {
  #tools { gap: .375rem; }
  #tools .store-search { flex: 1 1 7rem; min-width: min(100%, 7rem); }
  #tools .store-search input { padding-inline: .55rem; font-size: .875rem; }
  #search-btn::-webkit-search-cancel-button { display: none; }
  #tools .button { padding-inline: .45rem; white-space: nowrap; }
}
'''


VIEWER = r'''function mountArchitecture(G, signal, saved) {
'use strict';
const $ = selector => document.querySelector(selector);
const svg = $('#svg'), vp = $('#viewport'), stage = $('#stage'), panel = $('#panel');
const NS = svg.namespaceURI;
const boardSpecs = G.peersus.boards;
const mobile = matchMedia('(max-width: 53rem)');
const viewLabels = new Map(G.peersus.views);
const kinds = ['rpc','data','call','config','import','event'];
function titleFor(kind, container=state.container) {
  const overrides=boardSpecs[container]?.kinds;
  return overrides&&Object.hasOwn(overrides,kind)?overrides[kind]:
    (Object.hasOwn(G.peersus.legend||{},kind)?G.peersus.legend[kind]:'Other connections');
}
const clamp = (value, lo, hi) => Math.max(lo, Math.min(value, hi));
const plural = (n, one) => `${n} ${one}${n === 1 ? '' : 's'}`;
const children = nodes => nodes.flat(Infinity).filter(n => n != null && n !== false);
function dom(tag, attrs = {}, ...parts) {
  const node=document.createElement(tag);
  for(const [name,value] of Object.entries(attrs))node.setAttribute(name,value);
  node.append(...children(parts));
  return node;
}
const div=(className,...content)=>dom('div',{class:className},...content);
const span=(className,...content)=>dom('span',{class:className},...content);
const button=(className,attrs,...content)=>dom('button',{type:'button',class:className,...attrs},...content);
const para=(content)=>dom('p',{class:'pd'},content);
/*__FP_PRIMITIVES__*/
const glyphs=new Map();
function icon(kind,small=false) {
  const key=Object.hasOwn(P,kind)?kind:'file';
  if(!glyphs.has(key)) {
    // P is the bundled, fixed Floorplan icon table. No document data becomes markup.
    const parsed=new DOMParser().parseFromString(`<svg xmlns="${NS}" viewBox="0 0 24 24">${P[key]}</svg>`,'image/svg+xml');
    glyphs.set(key,document.importNode(parsed.documentElement,true));
  }
  const svg=glyphs.get(key).cloneNode(true);
  if(small)svg.setAttribute('class','i');
  return svg;
}
const iconI=kind=>icon(kind,true);
const nodes = new Map(G.nodes.map(n => [n.id, n]));
const parentOf = new Map(), kids = new Map();
for (const n of G.nodes) {
  const ref = 'n:' + n.id, parent = n.parent ? 'n:' + n.parent : 'root';
  parentOf.set(ref, parent);
  if (!kids.has(parent)) kids.set(parent, []);
  kids.get(parent).push(ref);
}
const visibleKids = item => boardSpecs[item]?.cards || kids.get(item) || [];
/*__FP_CHAIN__*/
const connEntity = id => id.replace(/^x:(in|out):/, '');
const nameOf = item => info(item).name;
function info(item) {
  if (item === 'root') return { name:G.project.name, desc:G.project.summary, kind:'library' };
  const n = nodes.get(connEntity(item).slice(2));
  return { name:n.label, desc:n.summary, kind:n.kind, n };
}
const drillable = item => visibleKids(connEntity(item)).length > 0;
const state = { container:'root', selected:null, hover:null, view:null, layout:null, flow:null, guide:false, zoom:1, base:1 };
function buildView(container) {
  const {cards,connectors,edges:refs,layers,reduced,feedback}=G.peersus.diagrams[container]||{cards:[],connectors:[],edges:[]};
  const edges=refs.map(([a,b,records],i)=>{
    const links=records.map(idx=>G.links[idx]);
    return {id:'e'+i,a,b,links,k:links.find(l=>l.k==='import')?.k||links[0].k};
  });
  return {container,visible:cards,connectors:connectors.map(id=>({id})),edges,layers,feedback,
    defaultEdges:layers?reduced.map(idx=>edges[idx]):edges};
}
/*__GEOMETRY__*/
/*__FP_LCA__*/

function makeCard(ref) {
  const {name,desc,kind,n} = info(ref), isRef = ref.startsWith('x:');
  const count = visibleKids(connEntity(ref)).length;
  const chip = isRef ? 'Related' : boardSpecs[ref]?.badge || (count ? plural(count, 'part') :
    n?.evidenceKind === 'Trace step' || state.flow ? 'Step' : n?.evidenceKind === 'Crate' ? 'Crate' : 'Source');
  return div(`map-card card n-${kind}${kind === 'external' ? ' external' : ''}`,
    div('head',dom('span',{class:'ico','aria-hidden':'true'},icon(kind)),span('title',name)),
    div('desc',desc),div('foot',span('meta',chip),
      (drillable(ref)||isRef)&&button('drill',{'aria-label':`Open ${name}`},iconI('drill'))),
    button('card-hit',{'aria-label':`Inspect ${name}`,'aria-pressed':'false'}));
}

// Card dimensions come from a real site tile at the same width, not from guessed text metrics.
function measureCards(ids,width) {
  const sample=Object.assign(document.createElement('div'),{className:'map-card-measure',inert:true});
  sample.style.width=`${width}px`;
  sample.append(...ids.map(makeCard));document.body.append(sample);
  const height=Math.max(139,...Array.from(sample.children,c=>Math.ceil(c.getBoundingClientRect().height)));
  sample.remove();return height;
}
function cardGrid(view,available) {
  const candidates=[...view.visible,...view.connectors.map(c=>c.id)];
  const compact=available<700, columns=Math.min(candidates.length||1,available<450?1:compact?2:3);
  const margin=columns===1?46:28, padY=46, gx=columns===1?50:69, gy=67, overhead=2*margin+(columns-1)*gx;
  const w=Math.floor(Math.min(226,(available-overhead)/columns)),h=measureCards(candidates,w);
  const layers=view.layers?.map(layer=>[...layer]);
  const id=boardSpecs[view.container]?.fillLayer;
  if(layers&&id&&columns===3){
    // A redundant long link can leave a free grid cell in the preceding visual band.
    const rank=layers.findIndex(l=>l.length>columns);
    if(rank>0&&layers[rank].includes(id)&&layers[rank-1].length<columns){
      layers[rank]=layers[rank].filter(key=>key!==id);layers[rank-1].push(id);
    }
  }
  const grid=layers || [compact?boardSpecs[view.container]?.mobileCards||candidates:candidates];
  const cards=[], marks=[];let row=0;
  for(const [rank,band] of grid.entries()){
    if(!band.length)continue;
    if(layers)marks.push({id:'layer:'+rank,text:String(rank+1).padStart(2,'0'),x:7,y:padY+row*(h+gy)+Math.floor(h/2)-7,w:15,h:14});
    for(let i=0;i<band.length;i+=columns){
      const batch=band.slice(i,i+columns), shift=layers?Math.floor((columns-batch.length)*(w+gx)/2):0;
      for(const [col,id] of batch.entries()) cards.push({id,rank,row,col,x:margin+shift+col*(w+gx),y:padY+row*(h+gy),w,h,width:w,height:h});
      row++;
    }
  }
  const width=overhead+columns*w,height=2*padY+row*h+Math.max(0,row-1)*gy;
  return {width,height,cards,marks,layers,columns,gutter:Math.max(gx,gy),route:makeRouter(cards,marks,width,height)};
}

const directions={l:[-1,0],r:[1,0],t:[0,-1],b:[0,1]};
function endpoint(c,side) {
  const [dx,dy]=directions[side], p=[c.x+Math.floor(c.w/2),c.y+Math.floor(c.h/2)];
  // Use an exact centre even when a measured dimension is odd.
  p[0]=dx?c.x+(dx>0?c.w:0):c.x+c.w/2;
  p[1]=dy?c.y+(dy>0?c.h:0):c.y+c.h/2;
  return {card:c.id,side,p,q:[p[0]+16*dx,p[1]+16*dy]};
}
function heap() {
  const queue=[];
  function push(item){let i=queue.push(item)-1;while(i){const p=(i-1)>>1;if(queue[p][0]<=item[0])break;queue[i]=queue[p];i=p;}queue[i]=item;}
  function pop(){const top=queue[0],end=queue.pop();if(queue.length){let i=0;while(2*i+1<queue.length){let a=2*i+1;if(a+1<queue.length&&queue[a+1][0]<queue[a][0])a++;if(queue[a][0]>=end[0])break;queue[i]=queue[a];i=a;}queue[i]=end;}return top;}
  return {push,pop,get length(){return queue.length;}};
}
function makeRouter(cards,marks,width,height) {
  const byId=new Map(cards.map(c=>[c.id,c]));
  const tips=new Map(cards.map(c=>[c.id,Object.keys(directions).map(side=>endpoint(c,side))]));
  const inset=G_FRAME_CLEARANCE+2; // Keep clearance to the painted stroke, not just its centreline.
  function coords(axis,span){
    const source=[inset,span-inset];
    for(const group of tips.values())for(const t of group)source.push(t.q[axis]);
    for(const mark of marks){const [a,b]=gBounds(mark)[axis];source.push(a-12,b+12);}
    const unique=[...new Set(source.filter(x=>x>=inset&&x<=span-inset))].sort((a,b)=>a-b), order=[];
    for(let i=1;i<unique.length;i++){
      const a=unique[i-1], b=unique[i], n=Math.ceil((b-a)/18);
      order.push(a);for(let i=1;i<n;i++)order.push(a+(b-a)*i/n);
    }
    order.push(unique.at(-1));return order;
  }
  const xs=coords(0,width),ys=coords(1,height),nx=xs.length,ny=ys.length;
  const points=ys.flatMap(y=>xs.map(x=>[x,y])), graph=points.map(()=>[]);
  const obstacles=[...cards,...marks];
  const clear=(s,skip)=>!obstacles.some(r=>r.id!==skip&&gHits(s,r,10.25));
  function connect(i,j) {
    const s=gSegment(points[i],points[j]);if(!clear(s))return;
    graph[i].push({j,s});graph[j].push({j:i,s});
  }
  points.forEach((_,i)=>{
    if(i%nx<nx-1)connect(i,i+1);
    if(i<nx*(ny-1))connect(i,i+nx);
  });
  const at=p=>{
    const y=ys.indexOf(p[1]),x=xs.indexOf(p[0]);
    return x<0||y<0?-1:y*nx+x;
  };
  const taxi=(p,q)=>Math.abs(p[0]-q[0])+Math.abs(p[1]-q[1]);
  return function route(edges, {forward=false, compact=false}={}) {
    const used=[], routes=[], cardinal=new Map();
    const palette=getComputedStyle($('#diagram'));
    const colors=new Map(kinds.map(k=>[k,palette.getPropertyValue(`--k-${k}`).trim()]));
    const muted=palette.getPropertyValue('--muted').trim(), color=kind=>colors.get(kind)??muted;
    function check(edge,s) {
      let cost=0;
      for(const t of used){
        const distance=forward||color(edge.k)===color(t.edge.k)?G_HAIRPIN_CLEARANCE:G_CLEARANCE;
        if(gParallel(s,t,distance)){
          if(!gNear(s.on,t.on)||!forward&&edge.k!==t.edge.k||
             !(edge.a===t.edge.a||edge.b===t.edge.b||edge.a===t.edge.b||edge.b===t.edge.a))return -1;
          cost+=1; // coalesce only a genuinely shared stem; prefer open space elsewhere
        }else if(s.axis!==t.axis&&gRange(s.lo,s.hi,t.on,t.on)>=-G_EPS&&
                t.lo+G_EPS<s.on&&s.on<t.hi-G_EPS)cost+=29;
      }
      return cost;
    }
    function solve(edge){
      const a=byId.get(edge.a),b=byId.get(edge.b);
      if(!a||!b)return;
      const down=forward&&a.rank<b.rank;
      function ends(card,side){
        return tips.get(card.id).filter(p=>(!side||p.side===side)&&at(p.q)>=0&&
          clear(gSegment(p.p,p.q),card.id)&&check(edge,gSegment(p.p,p.q))>=0);
      }
      const starts=ends(a,down?'b':null), endsAt=ends(b,down?'t':null);
      if(!starts.length||!endsAt.length)return;
      const direct=Math.min(...starts.flatMap(s=>endsAt.map(t=>taxi(s.q,t.q))));
      const budget=compact?Math.max(direct*1.53+45,192):Infinity;
      const toNode=new Map(endsAt.map(t=>[at(t.q),t]));
      const best=new Map(), prev=new Map(), queue=heap();
      const estimate=p=>Math.min(...endsAt.map(t=>taxi(p,t.q)));
      for(const p of starts){
        const axis='lr'.includes(p.side)?0:1, i=at(p.q), key=i*2+axis;
        const distance=16+check(edge,gSegment(p.p,p.q));
        best.set(key,distance);prev.set(key,p);queue.push([distance+estimate(p.q),distance,i,axis,16]);
      }
      let found;
      while(queue.length){
        const [,cost,i,axis,length]=queue.pop(),key=i*2+axis;
        if(cost!==best.get(key))continue;
        const target=toNode.get(i);
        if(target){found={key,target};break;}
        const p=points[i];
        for(const {j,s} of graph[i]){
          const q=points[j];if(down&&q[1]<p[1])continue;
          const l=length+(s.hi-s.lo);if(l+estimate(q)>budget)continue;
          const traffic=check(edge,s);if(traffic<0)continue;
          const next=cost+s.hi-s.lo+traffic+(axis!==s.axis?24:0),k=j*2+s.axis;
          if(next>=(best.get(k)??Infinity))continue;
          best.set(k,next);prev.set(k,key);queue.push([next+estimate(q),next,j,s.axis,l]);
        }
      }
      if(!found)return;
      const pointsBack=[], {target}=found;let key=found.key;
      while(typeof key==='number'){pointsBack.push(points[Math.floor(key/2)]);key=prev.get(key);}
      const chain=[key.p,...pointsBack.reverse(),target.p];
      const path=chain.filter((p,i)=>i===0||i===chain.length-1||gSegment(chain[i-1],p).axis!==gSegment(p,chain[i+1]).axis);
      return {edge,points:path,ports:[key,target]};
    }
    for(const edge of edges){
      const line=solve(edge);if(!line)continue;
      routes.push(line);
      used.push(...gSegments([{id:edge.id,points:line.points}]).map(s=>({...s,edge})));
      for(const port of line.ports){
        const key=`${port.card}:${port.side}`;
        if(!cardinal.has(key))cardinal.set(key,{...port,related:[]});
        cardinal.get(key).related.push(edge);
      }
    }
    return {routes,ports:[...cardinal.values()]};
  };
}

// One visible segment per coincident run, carrying only the real edges that use it.
function mergeWires(routes) {
  const channel=new Map(), groups=new Map();
  for(const {edge,points} of routes)for(const s of gSegments([{id:edge.id,points}])){
    const key=`${s.axis}:${s.on}`;
    if(!channel.has(key))channel.set(key,[]);
    channel.get(key).push({...s,edge});
  }
  for(const lane of channel.values()){
    const {axis,on}=lane[0], cuts=[...new Set(lane.flatMap(s=>[s.lo,s.hi]))].sort((a,b)=>a-b);
    for(let i=1;i<cuts.length;i++){
      const lo=cuts[i-1],hi=cuts[i];
      const members=[...new Set(lane.filter(s=>s.lo<=lo&&s.hi>=hi).map(s=>s.edge))];
      if(!members.length)continue;
      const key=members.map(e=>e.id).sort().join('/');
      if(!groups.has(key))groups.set(key,{key,members,segments:[]});
      const p=axis===0?[lo,on]:[on,lo],q=axis===0?[hi,on]:[on,hi];
      const segments=groups.get(key).segments,last=segments.at(-1);
      if(last&&last.axis===axis&&gEqual(last.q,p))segments[segments.length-1]=gSegment(last.p,q);
      else segments.push(gSegment(p,q));
    }
  }
  return [...groups.values()];
}

const measure=document.createElement('canvas').getContext('2d');
function routeLabels(res, groups, caption) {
  // Count shared corridors. Every text rectangle (including its leader) is tested
  // against real stroke sections, cards and already placed annotations.
  measure.font=`700 10px ${getComputedStyle($('#diagram')).getPropertyValue('--mono')}`;
  const slots=[...res.cards,...res.marks], lines=groups.flatMap(g=>g.segments);
  const ranked=groups.slice().sort((a,b)=>caption(b).priority-caption(a).priority);
  const labels=new Map();
  for(const g of ranked){
    const {text,count}=caption(g);if(!text)continue;
    const w=Math.ceil(measure.measureText(text).width)+(count?12:5), h=count?18:17;
    const segments=g.segments.slice().sort((a,b)=>(b.hi-b.lo)-(a.hi-a.lo));
    for(const s of segments){
      if(s.hi-s.lo<12)continue;
      for(const t of [.5,.35,.65,.23,.77])for(const d of [-1,1]){
        const u=s.lo+(s.hi-s.lo)*t,v=s.on, middle=s.axis===0?[u,v]:[v,u];
        const x=middle[0]+(s.axis===0?0:d*(w/2+8)),y=middle[1]+(s.axis===0?d*(h/2+8):0);
        const rect={x:x-w/2,y:y-h/2,w,h};
        if(rect.x<3||rect.y<3||rect.x+w>res.width-3||rect.y+h>res.height-3)continue;
        if(slots.some(r=>gBoxOverlap(r,rect))||lines.some(l=>gHits(l,rect,1.5)))continue;
        const end=s.axis===0?[x,v+d*7]:[v+d*7,y], leader=gSegment(middle,end);
        if(!gInFrame(end,{x:0,y:0,w:res.width,h:res.height},.5))continue;
        const leadBox={x:Math.min(middle[0],end[0]),y:Math.min(middle[1],end[1]),w:Math.abs(middle[0]-end[0]),h:Math.abs(middle[1]-end[1])};
        if(res.cards.some(r=>gHits(leader,r,10.25))||slots.some(r=>gHits(leader,r,1))||
           lines.some(l=>l!==s&&(gHits(l,leadBox,1)||gParallel(l,leader))))continue;
        lines.push(leader);slots.push(rect);labels.set(g,{rect,x,y,leader,text,count});break;
      }
      if(labels.has(g))break;
    }
  }
  return labels;
}

function makeScene(view,res,touching=[]) {
  const byId=new Map(res.cards.map(c=>[c.id,c]));
  const span=e=>{
    const a=byId.get(e.a),b=byId.get(e.b);
    return Math.abs(a.x-b.x)+Math.abs(a.y-b.y);
  };
  const config=boardSpecs[view.container];
  const priority=e=>e.b===config?.priorityTo&&config.priorityFrom?.includes(e.a)===true;
  // Let longest priority routes reserve shared corridors; leave other edges short.
  const ordered=[...view.defaultEdges].sort((a,b)=>Number(priority(b))-Number(priority(a))||
    (priority(a)&&priority(b)?span(b)-span(a):span(a)-span(b)));
  // Draw local relationships first rather than stretching long wires around the outside.
  // Keep the exact records available in the inspector and in the focused neighborhood.
  const shortened=config?.adjacentLayers?ordered.filter(e=>byId.get(e.b).rank-byId.get(e.a).rank<=1):ordered;
  const primary=config?shortened:shortened.slice(0,9);
  const active=new Set(touching.map(e=>e.id));
  const edges=[...new Map([...touching.slice().sort((a,b)=>span(a)-span(b)),...primary].map(e=>[e.id,e])).values()];
  const {routes,ports}=res.route(edges,{forward:!!res.layers,compact:!touching.length&&!config?.priorityTo});
  const wires=mergeWires(routes);
  const singleCaption=config&&res.columns>1;
  const captions=routeLabels(res,wires,g=>{
    const names=g.members[0].links[0];
    if(g.members.length>1){
      const groupName=singleCaption&&g.members.every(e=>e.links[0].labels[0]===names.labels[0])&&
        Object.hasOwn(config.groupCaption||{},names.labels[0])&&config.groupCaption[names.labels[0]];
      return {text:groupName?`${groupName} ×${g.members.length}`:String(g.members.length),count:true,priority:10+g.members.length};
    }
    const label=singleCaption?(names.caption||''):'';
    return {text:label,count:false,priority:config?.captionPriority?.includes(label)?3:1};
  });
  return {ports,wires,captions,count:routes.length,active};
}

function edgeTitle(e) {
  return `${nameOf(e.a)} → ${nameOf(e.b)}: ${[...new Set(e.links.flatMap(l=>l.labels))].join('; ')}`;
}
const drawSegment=({p,q})=>`M${p[0]},${p[1]}L${q[0]},${q[1]}`;
function drawWires(scene,res) {
  res.traceLayer.replaceChildren();res.portLayer.replaceChildren();
  const rendered=new Map();
  for(const wire of scene.wires){
    const {key,members,segments}=wire, lit=members.some(e=>scene.active.has(e.id));
    const linked=members.length>1, kind=members.every(e=>e.k===members[0].k)?members[0].k:'import';
    const title=linked?`${members.length} connections share this line`:edgeTitle(members[0]);
    const node=el('g',{class:`edge k-${kind}${lit?' hl':''}`,tabindex:0,role:'button','aria-label':title,'data-members':key},res.traceLayer);
    el('title',{},node).textContent=linked?members.map(edgeTitle).join(' · '):title;
    const d=segments.map(drawSegment).join(' ');
    el('path',{class:`trace${linked?' trunk':''}`,d,'data-wire':key},node);
    el('path',{class:'hit',d},node);
    const caption=scene.captions.get(wire);
    if(caption){
      const box=el('g',{'data-wire-label':key,'aria-hidden':true},node);
      el('path',{class:'wire-leader',d:drawSegment(caption.leader),'data-wire':key},node);
      if(caption.count){
        const {x,y,w,h}=caption.rect;el('rect',{class:'trunk-badge',x,y,width:w,height:h,rx:9},box);
      }
      el('text',{class:`wire-label${caption.count?' trunk-count':''}`,x:caption.x,y:caption.y},box).textContent=caption.text;
    }
    const choose=()=>select(linked?{edges:members}:{edge:members[0].id});
    node.onclick=ev=>{ev.stopPropagation();choose();};
    node.onkeydown=ev=>{if(ev.key==='Enter'||ev.key===' '){ev.preventDefault();choose();}};
    rendered.set(key,members);
  }
  for(const {p,card,side,related} of scene.ports){
    const out=related.some(e=>e.a===card), lit=related.some(e=>scene.active.has(e.id));
    const owners=[...rendered].filter(([,edges])=>edges.some(e=>related.includes(e))).map(([id])=>id);
    const group=el('g',{class:`edge k-${related[0].k}${lit?' hl':''}`,'aria-hidden':true},res.portLayer);
    el('circle',{class:out?'port':'port target',cx:p[0],cy:p[1],r:related.length>1?3.45:2.8,
      'data-port-card':card,'data-port-side':side,'data-port-owner':owners.join(' ')},group);
  }
  svg.dataset.linksShown=scene.count;
}
function draw(view,res) {
  vp.replaceChildren();
  res.traceLayer=el('g',{class:'edges'},vp);
  for(const mark of res.marks)el('text',{class:'layer-number','data-layer-label':mark.id,
    x:mark.x,y:mark.y+mark.h/2,'aria-hidden':true},vp).textContent=mark.text;
  const cards=el('g',{},vp);res.portLayer=el('g',{class:'edges'},vp);
  state.chipEls=new Map();
  for(const c of res.cards){
    const group=el('g',{class:'node','data-ref':c.id,transform:`translate(${c.x},${c.y})`},cards);
    const card=el('foreignObject',{width:c.w,height:c.h},group);
    card.append(div('fo-pad',makeCard(c.id)));
    card.querySelector('.card-hit').onclick=ev=>{ev.stopPropagation();select(c.id);};
    card.querySelector('.drill')?.addEventListener('click',ev=>{ev.stopPropagation();open(c.id);});
    group.ondblclick=ev=>{if(!ev.target.closest('.drill')&&drillable(c.id))open(c.id);};
    if(res.base.count<view.edges.length){
      const hover=ref=>{if(state.hover!==ref){state.hover=ref;refreshSelection();}};
      group.onpointerenter=ev=>{if(ev.pointerType!=='touch')hover(c.id);};
      group.onpointerleave=ev=>{if(ev.pointerType!=='touch'&&state.hover===c.id)hover(null);};
      group.onfocusin=ev=>{if(ev.target.matches(':focus-visible'))hover(c.id);};
      group.onfocusout=ev=>{if(!group.contains(ev.relatedTarget)&&state.hover===c.id)hover(null);};
    }
    state.chipEls.set(c.id,group);
  }
  svg.dataset.gutter=res.gutter;
}
function refreshSelection() {
  const {layout:res,view,selected,hover,flow}=state;
  const ref=hover||(typeof selected==='string'?selected:null);
  const from=flow?.flow.steps[flow.i].ref, to=flow?.flow.steps[flow.i+1]?.ref;
  const active=ref?view.edges.filter(e=>e.a===ref||e.b===ref):selected?.edges||view.edges.filter(e=>e.id===selected?.edge||e.a===from&&e.b===to);
  const neighbours=new Set(active.flatMap(e=>[e.a,e.b]));
  for(const [id,group] of state.chipEls){
    group.classList.toggle('sel',id===(ref||from));
    group.classList.toggle('hl',id===ref||id===from||id===to||neighbours.has(id));
    group.querySelector('.card-hit').setAttribute('aria-pressed',String(id===selected));
  }
  svg.classList.toggle('dim',Boolean(ref||from||active.length));
  const key=active.map(e=>e.id).join(' ');
  if(key!==res.sceneKey){
    if(res.traceLayer.contains(document.activeElement))stage.focus({preventScroll:true});
    res.sceneKey=key;
    drawWires(active.length?makeScene(view,res,active):res.base,res);
  }
}

function paintZoom() {
  const res = state.layout;
  if (!res) return;
  const width = Math.max(stage.clientWidth,Math.ceil(res.width*state.zoom)), height = Math.max(stage.clientHeight,Math.ceil(res.height*state.zoom));
  const x = Math.max(0,(width-res.width*state.zoom)/2), y = Math.max(0,(height-res.height*state.zoom)/2);
  svg.setAttribute('width',width); svg.setAttribute('height',height);
  vp.setAttribute('transform',`translate(${x},${y}) scale(${state.zoom})`);
  $('#scale').textContent = `${Math.round(state.zoom*100)}%`;
  $('#z-out').disabled = state.zoom<=.3;
  $('#z-in').disabled = state.zoom>=2.4;
}
function zoomAt(factor,cx=stage.clientWidth/2,cy=stage.clientHeight/2) {
  const before = vp.getCTM(), x=(stage.scrollLeft+cx-before.e)/state.zoom, y=(stage.scrollTop+cy-before.f)/state.zoom;
  state.zoom = clamp(state.zoom*factor,.3,2.4); paintZoom();
  const after = vp.getCTM();
  stage.scrollTo({left:x*state.zoom+after.e-cx,top:y*state.zoom+after.f-cy});
}
function fit(all = true) {
  if (!state.layout) return;
  state.zoom = all ? Math.min(state.base,stage.clientWidth/state.layout.width,stage.clientHeight/state.layout.height) : state.base;
  paintZoom(); stage.scrollTo(0,0);
}
let drag;
svg.addEventListener('pointerdown', ev => {
  if (ev.pointerType==='touch'||ev.button!==0||ev.target.closest('.node,.edge')) return;
  drag = {x:ev.clientX,y:ev.clientY,left:stage.scrollLeft,top:stage.scrollTop,id:ev.pointerId};
  svg.setPointerCapture(ev.pointerId);
},{signal});
svg.addEventListener('pointermove', ev => {
  if (drag?.id===ev.pointerId) stage.scrollTo(drag.left+drag.x-ev.clientX,drag.top+drag.y-ev.clientY);
},{signal});
svg.addEventListener('lostpointercapture', () => { drag=undefined; },{signal});
stage.addEventListener('wheel',ev => {
  if (!(ev.ctrlKey||ev.metaKey)) return;
  ev.preventDefault(); const r=stage.getBoundingClientRect(); zoomAt(Math.exp(-ev.deltaY*.008),ev.clientX-r.left,ev.clientY-r.top);
},{passive:false,signal});
$('#z-in').onclick=()=>zoomAt(1.25); $('#z-out').onclick=()=>zoomAt(.8); $('#z-fit').onclick=()=>fit();

function hash(container, sel) {
  const query=new URLSearchParams();
  if (container!=='root') query.set('v',container);
  if (typeof sel==='string') query.set('s',connEntity(sel));
  return '#'+(query.toString()||'overview');
}
function readHash() {
  const query=new URLSearchParams(location.hash.slice(1)), v=query.get('v'), s=query.get('s');
  return {container:parentOf.has(v)?v:'root',selected:parentOf.has(s)?s:null};
}
function navigate(container, selected=null, push=true, keepFlow=false) {
  const changed=container!==state.container;
  state.container=container; state.selected=selected; state.hover=null; state.guide=false;
  if (!keepFlow) state.flow=null;
  if (push && location.hash!==hash(container,selected)) history.pushState(null,'',hash(container,selected));
  if (changed || !state.layout) render();
  else { renderFlowBar(); refreshSelection(); renderPanel(); }
}
window.addEventListener('popstate',()=>{ const q=readHash(); navigate(q.container,q.selected,false); },{signal});
function reveal(ref) {
  const item=connEntity(ref);
  navigate(visibleKids(state.container).includes(item)?state.container:parentOf.get(item)||'root',item);
  const card=state.chipEls.get(item);
  if (card) {
    const r=card.getBoundingClientRect(), frame=stage.getBoundingClientRect();
    if (r.left<frame.left||r.right>frame.right||r.top<frame.top||r.bottom>frame.bottom)
      stage.scrollBy((r.left+r.right-frame.left-frame.right)/2,(r.top+r.bottom-frame.top-frame.bottom)/2);
  }
  if (mobile.matches) setPanel(true,true);
}
function open(ref) {
  if (ref.startsWith('x:')) { reveal(ref); return; }
  if (!drillable(ref)) { select(ref); return; }
  if (mobile.matches) setPanel(false);
  navigate(ref);
  $('#board-title').focus({preventScroll:true});
}
function select(selected) {
  state.flow=null; state.guide=false; state.selected=selected;
  if (location.hash!==hash(state.container,selected))
    history.pushState(null,'',hash(state.container,selected));
  renderFlowBar(); refreshSelection(); renderPanel(); setPanel(true,mobile.matches);
}
function setPanel(on,jump=false) {
  const toggle=$('#btn-panel'); panel.hidden=!on; $('.map-layout').classList.toggle('no-panel',!on);
  toggle.setAttribute('aria-expanded',String(on));
  if (on&&jump) { panel.scrollIntoView({block:'nearest'}); panel.focus({preventScroll:true}); }
}
$('#btn-panel').onclick=()=>setPanel(panel.hidden,panel.hidden&&mobile.matches);

function renderContext() {
  const container=state.container;
  const path=chain(container).slice().reverse();
  const section=G.peersus.views.find(([id])=>id!=='root'&&path.includes(id))?.[0] ??
    (path.slice(1).some(id=>boardSpecs[id])?null:'root');
  const crumbName=id=>viewLabels.get(id)||boardSpecs[id]?.title||nameOf(id);
  const crumbs=$('#crumbs');
  crumbs.hidden=path.length<=2;
  crumbs.replaceChildren(...(crumbs.hidden?[]:[
    ...path.slice(0,-1).flatMap(id=>[crumb(id,crumbName(id)),dom('span',{'aria-hidden':'true'},'/')]),
    dom('span',{},crumbName(container)),
  ]));
  const {edges,feedback,layers}=state.view, shown=state.layout.base.count;
  const used=new Set(edges.map(e=>e.k)), types=[...new Set([...kinds,...used])].filter(k=>used.has(k));
  const swatches=types.map(k=>span(`k-${k}`,dom('i',{'aria-hidden':'true'}),titleFor(k)));
  if(!swatches.length)swatches.push(dom('span',{},'No connections between these cards; select a card to see its source.'));
  if(edges.length>shown)swatches.push(span('legend-note',
    `${shown} of ${edges.length} direct links · ${feedback?.length?'Mutual links are shown on selection':'Select a card to trace its connections'}${layers?' · Read top to bottom':''}`));
  $('#legend-keys').replaceChildren(...swatches);
  for(const a of $('#tabs').children){
    if(a.dataset.crumb===section)a.setAttribute('aria-current','page');
    else a.removeAttribute('aria-current');
  }
}
function render() {
  const view=buildView(state.container), available=$('.map-frame').clientWidth;
  const res=cardGrid(view,available);
  res.base=makeScene(view,res);
  state.view=view;state.layout=res;state.hover=null;
  draw(view,res);renderContext();renderFlowBar();renderPanel();
  state.base=Math.min(1,available/res.width);
  const height=Math.max(240,Math.ceil(res.height*state.base));
  $('.map-layout').style.setProperty('--stage-height',`${height}px`);
  fit(false);refreshSelection();
}

const crumb=(ref,label)=>dom('a',{href:hash(ref),'data-crumb':ref},label);
const row=(label,desc,ref,extra='',action='item')=>button('row',{[`data-${action}`]:ref},
  span('dot'),span('rt',span('rn',label),desc&&span('rd',desc)),extra&&span('rc',extra));
const itemRow=(ref,more=false)=>{const i=info(ref); return row(i.name,more?i.desc:'',ref);};
const sec=(title,body,kind='')=>children([body]).length?div(`sec ${kind}`,dom('h3',{},title),body):null;
const header=(tag,title,desc)=>[div('ph',tag),dom('h2',{class:'pt'},title),desc&&para(desc)];
const tile=(n,unit)=>dom('div',{},dom('b',{},n.toLocaleString('en-US')),dom('span',{},unit));
const disclosure=(title,rows)=>rows.length?dom('details',{class:'sec link-list'},
  dom('summary',{},`${title} · ${rows.length}`),rows):null;
const description=(title,...content)=>dom('div',{},dom('dt',{},title),dom('dd',{},...content));
const evidence=(rows)=>dom('dl',{class:'evidence'},rows);
function summaryPanel() {
  const i=info(state.container), config=boardSpecs[state.container];
  const grouped=state.layout.layers?.map((refs,i)=>[`Layer ${i+1}`,refs])||config?.groups||[['On this board',state.view.visible]];
  const groups=grouped.map(([name,refs])=>[div('group-h',name),refs.map(ref=>itemRow(ref))]);
  const links=state.view.edges.map(e=>row(nameOf(e.a)+' → '+nameOf(e.b),e.links[0]?.labels[0],e.id,'','edge'));
  return [
    div('sec',header(config?.brief||'Current view',config?.inspector||i.name,i.desc),
      config?.stats?.length&&div('stats',config.stats.map(([n,unit])=>tile(n,unit)))),
    sec(state.container==='root'?'Systems':'Parts',groups,'groups'),
    disclosure('What crosses each line',links),
  ];
}
function guidePanel() {
  const action=G.peersus.tourAction, flow=G.flows.find(f=>f.id===action?.flow);
  return [
    div('sec',header('Reading order','Start here','Short stops before the first full walkthrough.')),
    sec('Read in order',G.tour.map((s,i)=>row(`${i+1}. ${s.title}`,s.note,s.ref))),
    flow&&sec(action.title,row(flow.title,flow.description,flow.id,'','start-flow')),
  ];
}
function itemPanel(ref) {
  const real=connEntity(ref), i=info(real), n=i.n;
  const kids=visibleKids(real);
  const bits=Object.entries(n.facts||{}).filter(([,v])=>v!==null && v!=='' && (!Array.isArray(v)||v.length));
  const facts=bits.map(([k,v])=>description(k.replace(/([A-Z])/g,' $1'),Array.isArray(v)?v.join(', '):String(v)));
  const touching=G.links.filter(l => !l.structural && (l.a===real||l.b===real) && (!l.board||l.board===state.container));
  const connects=touching.map(l=>row(`${nameOf(l.a)} → ${nameOf(l.b)}`,`${l.labels.join('; ')}${l.desc?' · '+l.desc:''}`,l.a===real?l.b:l.a));
  const openable=drillable(real)&&state.container!==real;
  return [
    div('sec',header(n.evidenceKind||'Part',i.name,i.desc),n.details&&para(n.details),
      openable&&div('panel-actions',button('button primary',{'data-open':real},'Open inside ',iconI('drill')))),
    sec('Source & documentation',evidence([
      description('Source anchor',dom('code',{},`${n.source.path}:${n.source.line}`)),
      n.doc&&description('Documentation',dom('strong',{},n.doc.heading),dom('code',{},`${n.doc.path}#${n.doc.slug}`)),
    ])),
    facts.length?sec('Declarations',evidence(facts)):null,
    sec('Inside',kids.map(id=>itemRow(id,true))),
    disclosure('Connections',connects),
  ];
}
function edgePanel(e) {
  return [
    div('sec',header(titleFor(e.k),nameOf(e.a)+' → '+nameOf(e.b),'')),
    sec('What travels on this line',e.links.map(l=>para(`${l.labels.join('; ')}${l.desc?' · '+l.desc:''}`))),
    sec('Connected parts',[itemRow(connEntity(e.a),true),itemRow(connEntity(e.b),true)]),
  ];
}
function renderPanel() {
  const sel=state.selected, edge=sel?.edge&&state.view.edges.find(e=>e.id===sel.edge);
  const body=state.guide?guidePanel():sel?.edges?sel.edges.map(edgePanel):edge?edgePanel(edge):
    typeof sel==='string'?itemPanel(sel):boardSpecs[state.container]?summaryPanel():itemPanel(state.container);
  const flags=G.peersus.violations.length&&boardSpecs[state.container]?.showViolations&&!sel&&!state.guide?
    sec('Source rule flags',G.peersus.violations.map(v=>para(`${v.message} · ${v.source.path}:${v.source.line}`))):null;
  panel.replaceChildren(...children([
    div('panel-top',button('button secondary',{'data-close-panel':''},'Close details')),body,flags,
  ]));
  panel.scrollTop=0;
}
panel.onclick=ev=>{
  const b=ev.target.closest('button'); if (!b) return;
  if (b.hasAttribute('data-close-panel')) {setPanel(false);$('#btn-panel').focus();return;}
  if (b.dataset.item) reveal(b.dataset.item);
  if (b.dataset.open) open(b.dataset.open);
  if (b.dataset.edge) select({edge:b.dataset.edge});
  if (b.dataset.startFlow) startFlow(b.dataset.startFlow);
};

const menu=$('#flowmenu'), flowButton=$('#flows-btn');
menu.replaceChildren(...G.flows.map(f=>row(f.title,f.description,f.id,plural(f.steps.length,'step'),'start-flow')));
function toggleMenu(on) {menu.hidden=!on;flowButton.setAttribute('aria-expanded',String(on));}
flowButton.onclick=()=>{ toggleMenu(menu.hidden); if(!menu.hidden) menu.querySelector('button')?.focus(); };
menu.onclick=ev=>{const b=ev.target.closest('[data-start-flow]');if(b)startFlow(b.dataset.startFlow);};
document.addEventListener('click',ev=>{
  const a=ev.target.closest('a[data-crumb]');
  if(a&&!ev.metaKey&&!ev.ctrlKey&&!ev.shiftKey&&!ev.altKey){ev.preventDefault();navigate(a.dataset.crumb);}
  if(!ev.target.closest('#flowmenu,#flows-btn'))toggleMenu(false);
},{signal});
$('#start-here').onclick=()=>{state.guide=true;renderPanel();setPanel(true,mobile.matches);if(!mobile.matches)panel.focus({preventScroll:true});};
function startFlow(id) {
  const flow=G.flows.find(f=>f.id===id); if(!flow)return;
  toggleMenu(false); if(mobile.matches)setPanel(false);
  state.flow={flow,i:0}; flowStep(0);
}
function flowStep(i) {
  if(!state.flow)return;
  const {flow}=state.flow;
  if(i>=flow.steps.length){state.flow=null;renderFlowBar();refreshSelection();return;}
  state.flow.i=clamp(i,0,flow.steps.length-1);
  const a=flow.steps[state.flow.i].ref,b=flow.steps[state.flow.i+1]?.ref;
  navigate(b?lcaContainer(a,b):parentOf.get(a),null,true,true);
}
function renderFlowBar() {
  const container=state.container;
  const title=container==='root'?viewLabels.get(container):boardSpecs[container]?.title;
  $('#board-title').textContent=state.flow?.flow.title||title||nameOf(container);
  const bar=$('#flowbar');bar.hidden=!state.flow;
  if(!state.flow){bar.replaceChildren();return;}
  const {flow,i}=state.flow;
  bar.replaceChildren(
    dom('div',{},div('fk',`Step ${i+1} / ${flow.steps.length}`),
      dom('strong',{},nameOf(flow.steps[i].ref)),dom('p',{},flow.steps[i].note)),
    div('controls',button('button secondary icon',{'data-direction':'back','aria-label':'Previous step',
      ...(i===0?{disabled:''}:{})},iconI('left')),
      button('button primary',{'data-direction':'next'},i+1===flow.steps.length?'Done':'Next step'),
      button('button secondary icon',{'data-direction':'close','aria-label':'Close walkthrough'},iconI('x'))));
  for(const b of bar.querySelectorAll('button')) b.onclick=()=>{
    const next=b.dataset.direction==='next'?i+1:b.dataset.direction==='back'?i-1:flow.steps.length;
    flowStep(next); bar.querySelector(`[data-direction="${b.dataset.direction}"]:enabled`)?.focus({preventScroll:true});
  };
}

const search=$('#search'), dialog=$('#palette'), results=$('#results');
const index=G.nodes.map(n=>({id:'n:'+n.id,name:n.label,desc:n.summary,path:`${n.inventoryId||n.id} ${n.source.path}`,kind:n.evidenceKind||n.kind}));
const searchCaption=text=>dom('p',{class:'search-caption'},text);
function runSearch() {
  const terms=search.value.trim().toLowerCase().split(/\s+/).filter(Boolean);
  if(!terms.length){results.replaceChildren(searchCaption('Try a part name, service, permission or repository path.'));return;}
  let scored=[];
  /*__FP_SEARCH__*/
  const rows=scored.sort((a,b)=>b[0]-a[0]).slice(0,24).map(([,it])=>row(it.name,it.desc,it.id,it.kind));
  results.replaceChildren(...(rows.length?rows:[searchCaption('No matching parts. Try a shorter name or a different path.')]));
}
function openPalette() {if(dialog.open)return;dialog.showModal();runSearch();search.focus();}
$('#search-btn').onclick=openPalette;
$('#search-btn').onkeydown=ev=>{if(ev.key==='Enter'||ev.key===' '){ev.preventDefault();openPalette();}};
$('#close-search').onclick=()=>dialog.close();search.oninput=runSearch;
function pickResult(b){if(b){dialog.close();reveal(b.dataset.item);}}
results.onclick=ev=>pickResult(ev.target.closest('button'));
search.onkeydown=ev=>{
  if(ev.key==='Enter'){ev.preventDefault();pickResult(results.querySelector('button'));}
  if(ev.key==='ArrowDown'){ev.preventDefault();results.querySelector('button')?.focus();}
};
results.onkeydown=ev=>{
  if(ev.key==='ArrowUp'){ev.preventDefault();(ev.target.previousElementSibling||search).focus();}
  if(ev.key==='ArrowDown'){ev.preventDefault();ev.target.nextElementSibling?.focus();}
};
window.addEventListener('keydown',ev=>{
  if(dialog.open||(ev.target.matches('input,textarea')&&ev.target.id!=='search-btn'))return;
  if(ev.key==='/'||(ev.key==='k'&&(ev.metaKey||ev.ctrlKey))){ev.preventDefault();openPalette();}
  else if(ev.key==='Escape'&&!menu.hidden){toggleMenu(false);flowButton.focus();}
  else if(ev.key==='Escape'&&state.flow)flowStep(state.flow.flow.steps.length);
  else if(ev.key==='Escape'&&!panel.hidden&&mobile.matches){setPanel(false);$('#btn-panel').focus();}
  else if(ev.key==='Escape'&&(state.selected||state.hover)){state.hover=null;select(null);}
  else if(ev.key==='ArrowRight'&&state.flow)flowStep(state.flow.i+1);
  else if(ev.key==='ArrowLeft'&&state.flow)flowStep(state.flow.i-1);
  else if(ev.key==='f'&&!ev.ctrlKey&&!ev.metaKey)fit();
},{signal});
$('#tabs').replaceChildren(...G.peersus.views.map(([ref,label])=>crumb(ref,label)));
if(dialog.open)runSearch();
const initial=readHash(); state.container=initial.container; state.selected=initial.selected;
state.guide=saved?.guide??false;
const flow=G.flows.find(row=>row.id===saved?.flow);
if(flow)state.flow={flow,i:Math.min(saved.step,flow.steps.length-1)};
setPanel(saved?.panel??(!mobile.matches||!!initial.selected));
const frame=$('.map-frame');
let lastWidth;
function refresh(){
  if(frame.clientWidth && frame.clientWidth!==lastWidth) {lastWidth=frame.clientWidth;render();}
}
const observer=new ResizeObserver(refresh);
signal.addEventListener('abort',()=>observer.disconnect(),{once:true});
const ready=document.fonts.ready.then(()=>{if(!signal.aborted){refresh();observer.observe(frame);}});
return {ready,save:()=>({guide:state.guide,panel:!panel.hidden,flow:state.flow?.flow.id,step:state.flow?.i})};
}
'''


PAGE = r'''<!doctype html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<meta name="color-scheme" content="light dark">
<meta name="referrer" content="no-referrer">
<meta http-equiv="Content-Security-Policy" content="__CSP__">
<title>__TITLE__</title>
__ASSETS__
</head>
<body>
<a class="skip" href="#main">Skip to content</a><header class="site-header"></header>
<main class="document store" id="main">
  <h1 class="visually-hidden">Peersus architecture map</h1>
  <div id="diagram">
    <div class="store-toolbar" id="tools" aria-label="Map tools">
      <label class="store-search"><span class="visually-hidden">Search the architecture</span><input id="search-btn" type="search" placeholder="Search map" readonly aria-haspopup="dialog" aria-controls="palette"></label>
      <button type="button" class="button primary" id="start-here">Start here</button>
      <button type="button" class="button secondary" id="flows-btn" aria-expanded="false" aria-controls="flowmenu">Flows</button>
      <button type="button" class="button secondary" id="btn-panel" aria-expanded="true" aria-controls="panel">Details</button>
      <div id="flowmenu" aria-label="Guided flows" hidden></div>
    </div>
    <nav id="tabs" aria-label="Architecture views"></nav>
    <div id="crumbs" aria-label="Location in map" hidden></div>
    <noscript><p class="notice">Enable JavaScript to open the map, or read __DATA_FALLBACK__.</p></noscript>
    <div class="map-layout">
      <section class="map-frame" aria-labelledby="board-title">
        <header id="legend" class="legend">
          <div class="legend-heading"><h2 id="board-title" tabindex="-1">Architecture</h2><strong>Connections</strong></div>
          <div id="legend-keys" class="legend-keys" role="group" aria-label="Connection legend"></div>
        </header>
        <section id="flowbar" aria-label="Current walkthrough" hidden></section>
        <div id="stage" tabindex="0" aria-label="Architecture diagram, scroll to pan">
          <svg id="svg" aria-label="Architecture cards and connections"><g id="viewport"></g></svg>
        </div>
        __FP_ZOOM__
      </section>
      <aside id="panel" aria-label="Map details" tabindex="-1"></aside>
    </div>
  </div>
</main><footer></footer>
<dialog id="palette" class="store" aria-label="Search the architecture"><div class="prompt"><label class="store-search"><span class="visually-hidden">Search by name, service or source path</span><input id="search" type="search" placeholder="Name, service or source path…" autocomplete="off" spellcheck="false"></label><button type="button" class="button secondary" id="close-search">Close</button></div><div id="results" aria-live="polite"></div></dialog>
__OFFLINE_SCRIPTS__
</body>
</html>
'''


def between(text, first, last):
    """Extract a delimited part of the pinned template, failing on upstream drift."""
    try:
        start = text.index(first)
        return text[start:text.index(last, start + len(first))]
    except ValueError as error:
        raise ValueError(f'Floorplan template boundary changed: {first}') from error


def data_uri(kind, data):
    return f'data:{kind};base64,' + base64.b64encode(data).decode('ascii')


def json_text(data):
    text = json.dumps(data, ensure_ascii=True, separators=(',', ':'), sort_keys=True)
    if PRIVATE.search(text):
        raise ValueError('Private or non-local address found in generated data')
    return text


def digest(text):
    return base64.b64encode(hashlib.sha256(text.encode()).digest()).decode('ascii')


def offline_chrome(page):
    site = runpy.run_path(str(ROOT / 'site/build.py'))
    page = site['chrome']('offline-map.html', page, None)
    for name in ('peersus-mark.svg', 'peersus-mark-light.svg'):
        mark = data_uri('image/svg+xml', read(f'site/brand/{name}').encode())
        page = page.replace(f'src="brand/{name}"', f'src="{mark}"')
    page = re.sub(r'<small>[^<]*<a href="[^"\n]+">SIL Open Font Licence</a></small>',
                  '<small>Offline source map · Floorplan (MIT) · Typeface Quicksand, '
                  '<a href="licenses/quicksand-OFL-1.1.txt">SIL Open Font Licence</a></small>', page, count=1)
    header = re.search(r'(<header class="site-header">)(<a class="brand"[^>]*>.*?</a>)'
                       r'<nav[^>]*>.*?(<button id="theme"[^>]*>.*?</button>)</nav></header>', page, re.S)
    footer = re.search(r'<footer>(<a class="brand"[^>]*>.*?</a>).*?(<small>.*?</small>)</footer>', page, re.S)
    if not header or not footer:
        raise ValueError('Unexpected staged site chrome')

    def brand(anchor):
        content = anchor.split('>', 1)[1].removesuffix('</a>')
        return f'<span class="brand" role="img" aria-label="Peersus">{content}</span>'

    offline_header = f'{header[1]}{brand(header[2])}<nav aria-label="Theme">{header[3]}</nav></header>'
    offline_footer = f'<footer>{brand(footer[1])}{footer[2]}</footer>'
    return page.replace(header[0], offline_header, 1).replace(footer[0], offline_footer, 1)


def offline_theme():
    text = between(read('site/app.js'), '// The theme button is in the header', '// Supporters who chose')
    return 'const pages=[];\n' + text + 'for(const init of pages)init(new AbortController().signal);\n'


def viewer_assets(site_root):
    """The public and offline viewers use the same renderer and pinned MIT fragments."""
    upstream = json.loads((site_root / 'architecture/floorplan-parts.json').read_bytes())
    if upstream['revision'] != FLOORPLAN_REV:
        raise ValueError(f'Expected the Floorplan viewer fragments from {FLOORPLAN_REV}')
    upstream_parts = {
        'FP_PRIMITIVES': upstream['primitives'],
        'FP_CHAIN': upstream['chain'],
        'GEOMETRY': GEOMETRY,
        'FP_LCA': upstream['lca'],
        'FP_SEARCH': 'for (const it of index) {\n' + upstream['search']
        .replace('s + it.w * 2 - n.length * 0.02', 's - n.length * 0.02'),
    }
    js = VIEWER
    for key, part in upstream_parts.items():
        token = f'/*__{key}__*/'
        if js.count(token) != 1:
            raise ValueError(f'Missing or duplicated viewer seam: {key}')
        js = js.replace(token, part)
    zoom = upstream['zoom']
    zoom = zoom.replace(' class="glass"', ' aria-label="Zoom controls"').replace('<button ', '<button type="button" class="button secondary icon" ')
    zoom = zoom.replace('  </div>', '    <output id="scale" aria-label="Map scale"></output><span class="hint">Scroll or drag to pan · Select a line for details</span>\n  </div>')
    return js, zoom


def stage_assets(site_root, policy):
    js, zoom = viewer_assets(site_root)
    client = (site_root / 'architecture/site-client.js').read_text(encoding='utf-8')
    js = ('(() => {\n"use strict";\n'
          f'const PUBLIC_SCHEMA = {json.dumps(PUBLIC_SCHEMA)}, MAX_PUBLIC_BYTES = {MAX_PUBLIC_BYTES};\n'
          + js + client + '\n})();\n')
    head = ('<link rel="icon" href="brand/peersus.svg" type="image/svg+xml">\n'
            '<link rel="stylesheet" href="style.css">\n<link rel="stylesheet" href="architecture.css">\n'
            '<script src="app.js" defer></script>\n<script src="architecture.js" defer></script>')
    page = (PAGE.replace('__TITLE__', 'Architecture — Peersus').replace('__CSP__', html.escape(policy, quote=True))
            .replace('__ASSETS__', head).replace('__FP_ZOOM__', zoom)
            .replace('__DATA_FALLBACK__', '<a href="architecture/map.json">the machine-readable snapshot</a>')
            .replace('__OFFLINE_SCRIPTS__', ''))
    return {'architecture.html': page.encode(), 'architecture.css': SKIN.encode(), 'architecture.js': js.encode(),
            'architecture/map.json': publication()}


def render_floorplan(inventory, graph):
    js, zoom = viewer_assets(ROOT / 'site')
    js = ('(() => {\n' + js + offline_theme() +
          'mountArchitecture(JSON.parse(document.getElementById("map-data").textContent),new AbortController().signal);\n})();\n')
    if re.search(r'</script|<!--', js, re.I):
        raise ValueError('Unsafe inline viewer script')
    site_style = read('site/style.css')
    font = data_uri('font/ttf', (ROOT / (FONT + 'Quicksand[wght].ttf')).read_bytes())
    # The literal stylesheet is unchanged. Resolve only its one local font before the body is parsed.
    font_source = re.findall(r'@font-face\{[^{}]*?src:url\("([^"\n]+)"\)', site_style)
    if font_source != ['fonts/Quicksand.ttf'] or len(re.findall(r'url\(', site_style)) != 1:
        raise ValueError('Unexpected resource reference in site/style.css')
    initialise_font = ('{const rules=document.getElementById("site-style").sheet.cssRules;'
                       'for(const rule of rules)if(rule.type===CSSRule.FONT_FACE_RULE)'
                       'rule.style.setProperty("src",rule.style.getPropertyValue("src").replace('
                       f'{json.dumps(font_source[0])},{json.dumps(font)}));' + '}')
    app_mark = read(UI.removesuffix('src/') + 'assets/peersus-icon.svg')
    csp = (f"default-src 'none'; script-src 'sha256-{digest(js)}' 'sha256-{digest(initialise_font)}'; "
           f"style-src 'sha256-{digest(site_style)}' 'sha256-{digest(SKIN)}'; font-src data:; img-src data:; "
           "connect-src 'none'; object-src 'none'; base-uri 'none'; form-action 'none'")
    licence = (ROOT / FLOORPLAN_LICENSE).read_bytes()
    if hashlib.sha256(licence).hexdigest() != '65a06a9dab105cd5a2b713a41a020a442bd431e6e19631182ed3889fd1672faf':
        raise ValueError('The pinned Floorplan licence has changed')
    head = (f'<link rel="icon" href="{data_uri("image/svg+xml", app_mark.encode())}" type="image/svg+xml">\n'
            f'<style id="site-style">{site_style}</style>\n<style>{SKIN}</style>\n'
            f'<script data-license="licenses/quicksand-OFL-1.1.txt">{initialise_font}</script>')
    page = (offline_chrome(PAGE).replace('__TITLE__', 'Peersus').replace('__CSP__', html.escape(csp, quote=True))
            .replace('__FP_ZOOM__', zoom).replace('__ASSETS__', head)
            .replace('__DATA_FALLBACK__', 'the companion graph in <code>graph.json</code>'))
    graph_text = json_text(graph)
    embedded = graph_text.replace('<', r'\u003c').replace('>', r'\u003e').replace('&', r'\u0026')
    scripts = (f'<script type="application/json" id="map-data">{embedded}</script>\n'
               f'<script data-license="licenses/floorplan-MIT.txt">{js}</script>')
    page = page.replace('__OFFLINE_SCRIPTS__', scripts)
    # Retain the whole notice even when index.html is shared without its companion files.
    page = page.replace('<!doctype html>\n', '<!doctype html>\n<!-- Floorplan viewer, ' + FLOORPLAN_REV + '\n' +
                        licence.decode('utf-8') + '\n-->\n', 1)
    return {'map.json': (json_text(inventory)+'\n').encode(), 'index.html': page.encode(),
            'graph.json': (graph_text+'\n').encode(), 'licenses/floorplan-MIT.txt': licence,
            'licenses/quicksand-OFL-1.1.txt': (ROOT / (FONT+'OFL.txt')).read_bytes()}



def generate(extensions=None):
    graph = Graph(extensions)
    for build in (code_view, broker, system, walkthrough, delivery):
        build(graph)
    order = ['system', 'code', 'broker', 'send', 'delivery']
    graph.views.sort(key=lambda v: order.index(v['id']))
    data = graph.result()
    projection = floorplan(graph, data)
    return data, projection


def source_revision(commit=None, commit_date=None):
    if commit is None and commit_date is None:
        commit, commit_date = git('show', '-s', '--format=%H%n%cI', 'HEAD').splitlines()
    if not isinstance(commit, str) or not re.fullmatch(r'[0-9a-f]{40}', commit):
        raise ValueError('Source commit must be a full lowercase Git object ID')
    value = datetime.fromisoformat(commit_date.replace('Z', '+00:00')) if isinstance(commit_date, str) else None
    if value is None or value.tzinfo is None or value.microsecond:
        raise ValueError('Source commit date must be ISO 8601 with a time zone')
    return dict(commit=commit, date=value.astimezone(timezone.utc).isoformat(timespec='seconds').replace('+00:00', 'Z'))


def published_paths(graph):
    paths = {v['source']['path'] for v in graph['peersus']['violations']}
    for node in graph['nodes']:
        paths.add(node['source']['path'])
        if node['doc']:
            paths.add(node['doc']['path'])
    return paths


def verify_publication(graph, tracked=None):
    if graph['schema'] != PUBLIC_SCHEMA or set(graph['source']) != {'commit', 'date'}:
        raise ValueError('Unknown architecture data schema or source')
    if source_revision(graph['source']['commit'], graph['source']['date']) != graph['source']:
        raise ValueError('Expected the canonical source date')
    text = json_text(graph) + '\n'
    if len(text) > MAX_PUBLIC_BYTES:
        raise ValueError('Architecture data exceeds the viewer limit')
    tracked = tracked_sources() if tracked is None else tracked
    for path in published_paths(graph):
        if not portable(path) or not (path in tracked or evidence_path(path)):
            raise ValueError('Architecture data refers to non-public source')
    return text.encode()


def publication(extensions=None, commit=None, commit_date=None):
    _, graph = generate(extensions)
    keep = lambda row, keys: {key: row[key] for key in keys.split() if key in row}
    graph = {
        'schema': PUBLIC_SCHEMA,
        'source': source_revision(commit, commit_date),
        'project': keep(graph['project'], 'name summary'),
        'nodes': [keep(n, 'id parent label kind summary source doc inventoryId facts details evidenceKind') for n in graph['nodes']],
        'links': [keep(e, 'a b k labels desc caption board structural') for e in graph['links']],
        'flows': graph['flows'], 'tour': graph['tour'],
        'peersus': keep(graph['peersus'], 'boards diagrams views tourAction legend violations'),
    }
    return verify_publication(graph)


def output_data(directory, extensions=None, commit=None, commit_date=None):
    payload = publication(extensions, commit, commit_date)
    path = directory / 'map.json'
    if directory.is_symlink() or path.is_symlink():
        raise ValueError('Refusing a symlinked architecture output')
    directory.mkdir(parents=True, exist_ok=True)
    if set(directory.iterdir()) - {path}:
        raise ValueError('Data-only output directory may contain only map.json')
    if not path.is_file() or path.read_bytes() != payload:
        path.write_bytes(payload)
    print(f'Architecture data: {len(payload)} bytes in map.json')
    return 0


def output(check, extensions=None):
    out = OUT
    if out.is_symlink() or not out.resolve().is_relative_to(ROOT):
        raise ValueError('Output directory must remain inside the checkout')
    data, projection = generate(extensions)
    expected = render_floorplan(data, projection)
    stale = []
    if (out / 'licenses').is_symlink():
        raise ValueError('Refusing a symlinked output: licenses')
    for name, content in expected.items():
        path = out / name
        if path.is_symlink():
            raise ValueError('Refusing a symlinked output: '+name)
        if not path.is_file() or path.read_bytes() != content:
            stale.append(name)
    if check and stale:
        print('Stale architecture map: '+', '.join(stale)+'; run python3 scripts/gen-architecture-map.py', file=sys.stderr)
        return 1
    if not check and stale:
        out.mkdir(parents=True, exist_ok=True)
        for name in stale:
            path = out / name
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_bytes(expected[name])
    print(f"Architecture map: {len(data['nodes'])} nodes, {len(data['edges'])} edges, {len(data['violations'])} rule flags")
    for v in data['violations']:
        print(f"  {v['source']['path']}:{v['source']['line']} [{v['rule']}] {v['message']}")
    return 0


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    mode = parser.add_mutually_exclusive_group()
    mode.add_argument('--check', action='store_true', help='Check offline generated bytes without writing files')
    mode.add_argument('--data-only', action='store_true', help='Write the versioned map.json publication')
    mode.add_argument('--check-data', type=Path, metavar='MAP_JSON', help='Check a generated publication for privacy and provenance')
    parser.add_argument('--output', type=Path, help='Directory for --data-only')
    parser.add_argument('--extensions-root', type=Path, help='Extension branch checkout or its extension/ directory; default: local plugins/')
    parser.add_argument('--commit', help='Full source commit (defaults to checked-out Git HEAD)')
    parser.add_argument('--commit-date', help='Source commit time (ISO 8601 including the time zone)')
    args = parser.parse_args()
    if args.data_only != (args.output is not None):
        parser.error('--data-only requires --output and vice versa')
    if (args.commit is None) != (args.commit_date is None) or args.commit is not None and not args.data_only:
        parser.error('--commit and --commit-date must be used together with --data-only')
    try:
        if args.check_data is not None:
            if args.check_data.stat().st_size > MAX_PUBLIC_BYTES:
                raise ValueError('Architecture data exceeds the viewer limit')
            verify_publication(json.loads(args.check_data.read_bytes()))
            print('Architecture publication privacy and provenance: passed')
            return 0
        if args.data_only:
            return output_data(args.output, args.extensions_root, args.commit, args.commit_date)
        return output(args.check, args.extensions_root)
    except FileNotFoundError as e:
        print('Architecture map: required input is missing: '+(Path(e.filename).name if e.filename else 'unknown file'), file=sys.stderr)
    except (ValueError, KeyError, IndexError, TypeError, subprocess.TimeoutExpired) as e:
        print('Architecture map: '+str(e), file=sys.stderr)
    return 1


if __name__ == '__main__':
    raise SystemExit(main())
