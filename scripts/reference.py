#!/usr/bin/env python3
"""Offline reference generation/checks and immutable package-doc staging (#314).

Requires Python 3.11+ and a freshly built Tinox compiler. Human translations and
historical pages are inputs, never generation targets.
"""
import argparse
from collections import Counter
from html.parser import HTMLParser
import json
import os
import re
from pathlib import Path
import shutil
import subprocess
import tempfile
import tomllib

ROOT = Path(__file__).resolve().parent.parent
GUIDE_LIMIT = 8192  # UTF-8 bytes; also a conservative upper bound on token count.


def run(binary, *args, cwd=ROOT):
    result = subprocess.run([str(binary), *map(str, args)], cwd=cwd,
                            capture_output=True, text=True, timeout=120)
    if result.returncode:
        raise ValueError(f"{' '.join(map(str, args))}: {result.stderr}\n{result.stdout}")
    return result.stdout


class Inventory(HTMLParser):
    def __init__(self, text):
        super().__init__()
        self.sections = []
        self.ids = []
        self.modules = []
        self.links = []
        self.feed(text)

    def handle_starttag(self, tag, attrs):
        attrs = dict(attrs)
        ident = attrs.get('id', '')
        if ident:
            self.ids.append(ident)
        if 'mod-section' in attrs.get('class', '').split():
            self.sections.append(ident)
        if tag == 'a' and 'href' in attrs:
            href = attrs['href']
            self.links.append(href)
            if href.startswith('#mod-'):
                self.modules.append(href[1:])


DECL_START = re.compile(r'^(import\b|class\b|enum\b|interface\b|trait\b|abstract\b|namespace\b|@)')


def indent(text, prefix):
    return ''.join(prefix + line + '\n' if line.strip() else '\n' for line in text.splitlines())


def spec_candidates(block):
    """Ways to read a SPEC snippet as a program: as-is, as statements inside a
    method, as members of a class, or split into top-level declarations plus
    statements (imports + a class followed by the statements using it)."""
    wrap = 'class Main\n{\n    fnc main() -> Int32\n    {\n%s        return 0\n    }\n}\n'
    decls, stmts, chunk = [], [], None
    for line in block.splitlines():
        if chunk is not None:
            chunk.append(line)
            if line.startswith('}'):
                decls.append('\n'.join(chunk)); chunk = None
        elif DECL_START.match(line):
            if line.startswith('import'):
                decls.append(line)
            else:
                chunk = [line]
                if line.rstrip().endswith('}') and '{' in line:
                    decls.append(line); chunk = None
        else:
            stmts.append(line)
    if chunk is not None:
        decls.append('\n'.join(chunk))
    yield block
    yield wrap % indent(block, '        ')
    yield 'class Main\n{\n' + block + '\n}\n'
    yield '\n'.join(decls) + '\n' + wrap % indent('\n'.join(stmts), '        ')


def check_spec_snippets(root, binary):
    """Every ```tinox block in docs/SPEC.md must parse (#470): `tinox fmt`
    parses without type-checking, so illustrative names are fine but invalid
    syntax such as `val` is not."""
    text = (root / 'docs/SPEC.md').read_text()
    failures = []
    with tempfile.TemporaryDirectory() as tmp:
        probe = Path(tmp) / 'Snippet.tnx'
        for match in re.finditer(r'^```tinox\n(.*?)^```', text, re.S | re.M):
            for candidate in spec_candidates(match.group(1)):
                probe.write_text(candidate)
                if subprocess.run([str(binary), 'fmt', str(probe)], capture_output=True).returncode == 0:
                    break
            else:
                failures.append(text[:match.start()].count('\n') + 2)
    if failures:
        raise ValueError('docs/SPEC.md: tinox blocks that do not parse (first line of each): ' + ', '.join(map(str, failures)))


def check_inventories(root):
    inventories = []
    for name in ('docs.html', 'docs_en.html'):
        page = Inventory((root / name).read_text())
        duplicate_ids = [key for key, count in Counter(page.ids).items() if count > 1]
        if duplicate_ids:
            raise ValueError(f'{name}: duplicate anchors: {duplicate_ids}')
        missing = set(page.modules) - set(page.ids)
        unlisted = set(page.sections) - set(page.modules)
        if missing or unlisted:
            raise ValueError(f'{name}: missing anchors {sorted(missing)}, unlisted sections {sorted(unlisted)}')
        inventories.append((Counter(page.sections), Counter(page.modules)))
    if inventories[0] != inventories[1]:
        raise ValueError('German/English module sections or navigation inventories differ')
    return sorted(inventories[0][0])


def page_path(root, group, artifact, version):
    # Coordinates become directory names, never arbitrary filesystem paths.
    for value in (group, artifact, version):
        if not value or not all(c.isascii() and (c.isalnum() or c in '.-_') for c in value) or value in ('.', '..'):
            raise ValueError(f'invalid package coordinate: {value!r}')
    return root / 'docs' / group.replace('.', '-') / artifact / version.replace('.', '-') / 'docs.html'


def packages(root):
    result = []
    for manifest in sorted((root / 'crates/tinox-core-ext').glob('*/tinox.toml')):
        data = tomllib.loads(manifest.read_text())
        package = data['package']
        group = package.get('group', 'tinox.core')
        page = page_path(root, group, package['name'], package['version'])
        result.append(dict(group=group, name=package['name'], version=package['version'],
                           manifest=str(manifest.relative_to(root)),
                           docs=str(page.relative_to(root)), dependencies=data.get('dependencies', [])))
    return result


def check_links(page, text=None):
    for href in Inventory(page.read_text() if text is None else text).links:
        target = href.split('#', 1)[0]
        if target and not target.startswith(('https:', 'http:', 'mailto:', 'data:')):
            # normpath: the page's own directory may not exist yet while staging
            if not Path(os.path.normpath(page.parent / target)).is_file():
                raise ValueError(f'{page}: broken dependency/document link: {href}')


def check_packages(root, inventory, require_pages=True):
    for package in inventory:
        page = root / package['docs']
        if require_pages and not page.is_file():
            raise ValueError(f'missing version docs: {page}; run make docs-stage')
        for dependency in package['dependencies']:
            target = page_path(root, dependency['group'], dependency['artifactId'], dependency['version'])
            if not target.is_file():
                raise ValueError(f"{package['name']}: missing dependency docs: {target}")
        if page.is_file():
            check_links(page)


def canonical_examples(root, binary):
    examples = json.loads((root / 'docs/guide/examples.json').read_text())
    listed = {entry['source'] for entry in examples}
    actual = {str(p.relative_to(root / 'docs/guide/examples'))
              for p in (root / 'docs/guide/examples').rglob('*.tnx')}
    if listed != actual:
        raise ValueError('canonical example manifest must list every .tnx example exactly once')
    if len(listed) != len(examples):
        raise ValueError('duplicate canonical example')
    with tempfile.TemporaryDirectory(prefix='tinox-guide-') as temp:
        for index, example in enumerate(examples):
            source = root / 'docs/guide/examples' / example['source']
            run(binary, 'check', source)
            exe = Path(temp) / f'example-{index}'
            run(binary, 'build', source, '-o', exe)
            result = subprocess.run([str(exe)], cwd=temp, capture_output=True, text=True, timeout=10)
            if result.returncode or result.stdout != example['stdout']:
                raise ValueError(f'{source}: expected {example["stdout"]!r}, got {result.stdout!r}; {result.stderr}')
            if example['tests']:
                output = run(binary, 'test', source, cwd=Path(temp))
                if '1 passed, 0 failed' not in output:
                    raise ValueError(f'{source}: @Test did not run: {output}')
    return examples


def json_text(value):
    return json.dumps(value, indent=2, ensure_ascii=False, sort_keys=True) + '\n'


def artifacts(root, binary):
    metadata = json.loads(run(binary, 'reference'))
    modules = check_inventories(root)
    inventory = packages(root)
    check_packages(root, inventory)
    examples = canonical_examples(root, binary)
    guide = (root / 'docs/guide/guide.md.in').read_text()
    guide = guide.replace('{{compiler_version}}', metadata['compiler_version'])
    snippets = '\n\n'.join(f"`{entry['source']}` (checked stdout: {entry['stdout'].strip()!r}):\n\n```tinox\n"
                            + (root / 'docs/guide/examples' / entry['source']).read_text() + '```'
                            for entry in examples)
    guide = guide.replace('{{examples}}', snippets)
    selected = {'Getter', 'Setter', 'Test'}
    annotations = ['| Annotation | Targets | Arguments |', '| --- | --- | --- |']
    for annotation in metadata['annotations']:
        if annotation['name'] in selected:
            annotations.append(f"| @{annotation['name']} | {', '.join(annotation['targets'])} | {annotation['min_args']}–{annotation['max_args']} |")
    guide = guide.replace('{{annotations}}', '\n'.join(annotations))
    size = len(guide.encode())
    if size > GUIDE_LIMIT:
        raise ValueError(f'AI guide exceeds {GUIDE_LIMIT} bytes: {size}')
    return {
        'docs/current/reference.json': json_text(metadata),
        'docs/current/packages.json': json_text(dict(compiler_version=metadata['compiler_version'],
                                                    packages=inventory, module_sections=modules)),
        'docs/AI_GUIDE.md': guide,
        'docs/current/guide-size.json': json_text(dict(utf8_bytes=size, words=len(guide.split()),
                                                     byte_limit=GUIDE_LIMIT,
                                                     compiler_version=metadata['compiler_version'])),
    }


def stage_package(root, binary, package):
    destination = root / package['docs']
    # Never regenerate, replace, or supplement an existing version directory.
    if destination.parent.exists():
        if not destination.is_file():
            raise ValueError(f'incomplete version directory: {destination.parent}')
        check_links(destination)
        return False
    manifest = root / package['manifest']
    source = manifest.parent / 'tinox/core' / package['name']
    examples = root / 'docs/tinox-core' / package['name'] / 'examples'
    if not source.is_dir():
        raise ValueError(f'missing package source: {source}')
    # Missing editable examples must not silently disappear from the next page.
    historical = list(destination.parent.parent.glob('*/docs.html'))
    if any('<pre><code>' in page.read_text() for page in historical):
        if not examples.is_dir() or not list(examples.glob('*.tnx')):
            raise ValueError(f'{package["name"]}: restore published example sources first')
    with tempfile.TemporaryDirectory(prefix='tinox-doc-stage-') as temp:
        stage = Path(temp)
        shutil.copyfile(manifest, stage / 'tinox.toml')
        shutil.copytree(source, stage / 'src')
        if examples.is_dir():
            shutil.copytree(examples, stage / 'examples')
        output = stage / 'docs.html'
        run(binary, 'doc', '--out', output, cwd=stage)
        if not output.is_file():
            raise ValueError(f'{package["name"]}: tinox doc produced no page')
        check_links(destination, output.read_text())
        reference = json.loads(run(binary, 'reference', source))
        # Exclusive creation refuses a racing attempt to write the same version.
        destination.parent.mkdir(parents=True, exist_ok=False)
        shutil.copyfile(output, destination)
        (destination.parent / 'reference.json').write_text(json_text(reference))
    return True


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('command', choices=['generate', 'check', 'stage', 'verify-release'])
    parser.add_argument('--tinox', type=Path, default=ROOT / 'target/release/tinox')
    args = parser.parse_args()
    binary = args.tinox.resolve()
    try:
        if args.command in ('stage', 'verify-release'):
            inventory = packages(ROOT)
            # Stage dependencies first: their pages must already exist. Existing
            # versions are validated and skipped, including historical content.
            pending = inventory[:]
            while pending:
                ready = [p for p in pending if all(page_path(ROOT, d['group'], d['artifactId'], d['version']).is_file()
                                                  for d in p['dependencies'])]
                if not ready:
                    raise ValueError('missing dependency pages or dependency cycle: ' + ', '.join(p['name'] for p in pending))
                for package in ready:
                    if args.command == 'stage':
                        if stage_package(ROOT, binary, package):
                            print(f"staged {package['docs']}")
                    elif not (ROOT / package['docs']).is_file():
                        raise ValueError(f"missing release docs: {package['docs']}; run make docs-stage")
                    pending.remove(package)
            check_inventories(ROOT)
            check_packages(ROOT, inventory)
        else:
            for name, content in artifacts(ROOT, binary).items():
                path = ROOT / name
                if args.command == 'generate':
                    path.parent.mkdir(parents=True, exist_ok=True)
                    path.write_text(content)
                elif not path.is_file() or path.read_text() != content:
                    raise ValueError(f'{name} is stale; run make docs-generate')
            if args.command == 'check':
                check_spec_snippets(ROOT, binary)
        print(f'reference {args.command}: OK')
    except (ValueError, OSError, subprocess.SubprocessError) as error:
        parser.exit(1, f'error: {error}\n')


if __name__ == '__main__':
    main()
