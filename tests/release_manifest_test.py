"""Release manifest generation and verification (#558)."""
import base64
import io
import json
from pathlib import Path
import sys
import tarfile
import tempfile
import unittest

sys.path.insert(0, str(Path(__file__).resolve().parents[1] / 'scripts'))
import release_manifest as rm  # noqa: E402


def package(name, version='1.0.0', deps=()):
    return dict(group='tinox.core', name=name, version=version, manifest=f'm/{name}/tinox.toml',
                docs=f'docs/{name}.html', dependencies=[dict(group='tinox.core', artifactId=d, version='1.0.0') for d in deps])


def manifest(**changes):
    base = dict(schema=1, stack=dict(name='s', database='postgres', members=['a']),
                compiler=dict(version='2.9.1', runtime_c_sha256='r', runtime_h_sha256='h', cargo_lock_sha256='c',
                              builder_apt_packages=[['x']], source=dict(commit=None, tree_clean=None)),
                packages=[dict(group='tinox.core', name='a', version='1.0.0', content_sha256='aaa', files=1,
                               docs='docs/a.html', docs_sha256='ddd', dependencies=[])])
    base.update(changes)
    return json.loads(json.dumps(base))


class StackAndDiff(unittest.TestCase):
    def test_stack_orders_dependencies_first_and_detects_cycles(self):
        inventory = [package('app', deps=['lib']), package('lib', deps=['base']), package('base')]
        names = [p['name'] for p in rm.stack_members(None, inventory, dict(packages=['app']))]
        self.assertEqual(names, ['base', 'lib', 'app'])
        cyclic = [package('a', deps=['b']), package('b', deps=['a'])]
        with self.assertRaisesRegex(ValueError, 'cycle'):
            rm.stack_members(None, cyclic, dict(packages=['a']))
        with self.assertRaisesRegex(ValueError, 'unknown package'):
            rm.stack_members(None, inventory, dict(packages=['missing']))

    def test_identical_manifests_have_no_problems(self):
        self.assertEqual(rm.diff(manifest(), manifest(), strict=True), [])

    def test_changed_content_without_version_bump_is_actionable(self):
        changed = manifest()
        changed['packages'][0]['content_sha256'] = 'bbb'
        problems = rm.diff(manifest(), changed)
        self.assertEqual(len(problems), 1)
        self.assertIn('without a version bump', problems[0])
        self.assertIn('make docs-stage', problems[0])

    def test_version_bump_dependency_and_membership_changes_are_reported(self):
        newer = manifest()
        newer['packages'][0]['version'] = '1.0.1'
        self.assertIn('regenerate the manifest after publishing', rm.diff(manifest(), newer)[0])
        pinned = manifest()
        pinned['packages'][0]['dependencies'] = [dict(name='b', version='1.0.0')]
        self.assertIn('dependency pins changed', rm.diff(manifest(), pinned)[0])
        extra = manifest()
        extra['packages'].append(dict(extra['packages'][0], name='b'))
        self.assertIn('joined the stack', rm.diff(manifest(), extra)[0])
        self.assertIn('no longer in the stack', rm.diff(extra, manifest())[0])

    def test_stale_compiler_and_runtime_fail_visibly(self):
        other = manifest()
        other['compiler']['version'] = '2.10.0'
        self.assertIn('does not match the manifest', rm.diff(manifest(), other)[0])
        runtime = manifest()
        runtime['compiler']['runtime_c_sha256'] = 'changed'
        self.assertEqual(rm.diff(manifest(), runtime), [])  # identity only compared with --strict
        self.assertIn('runtime_c_sha256', rm.diff(manifest(), runtime, strict=True)[0])
        schema = manifest(schema=2)
        self.assertIn('schema', rm.diff(schema, manifest())[0])

    def test_content_hash_ignores_archive_timestamps_but_not_content(self):
        def tar_bytes(content, mtime):
            out = io.BytesIO()
            with tarfile.open(fileobj=out, mode='w:gz') as tar:
                info = tarfile.TarInfo('tinox.toml')
                info.size, info.mtime = len(content), mtime
                tar.addfile(info, io.BytesIO(content))
            return out.getvalue()
        first = rm.content_hash(rm.archive_contents(tar_bytes(b'x', 1)))
        self.assertEqual(first, rm.content_hash(rm.archive_contents(tar_bytes(b'x', 99999))))
        self.assertNotEqual(first, rm.content_hash(rm.archive_contents(tar_bytes(b'y', 1))))


class Registry(unittest.TestCase):
    def archive_for(self, content):
        out = io.BytesIO()
        with tarfile.open(fileobj=out, mode='w:gz') as tar:
            info = tarfile.TarInfo('tinox.toml')
            info.size = len(content)
            tar.addfile(info, io.BytesIO(content))
        return out.getvalue()

    def test_registry_mismatch_and_missing_versions_are_reported(self):
        good = self.archive_for(b'x')
        m = manifest()
        m['packages'][0]['content_sha256'] = rm.content_hash(rm.archive_contents(good))
        ok = lambda url: (200, dict(contentBase64=base64.b64encode(good).decode()))
        self.assertEqual(rm.verify_registry(m, 'https://c', fetch=ok), [])
        bad = lambda url: (200, dict(contentBase64=base64.b64encode(self.archive_for(b'other')).decode()))
        self.assertIn('differs from the manifest', rm.verify_registry(m, 'https://c', fetch=bad)[0])

        def missing(url):
            raise ValueError('HTTP 404')
        self.assertIn('publish it first', rm.verify_registry(m, 'https://c', fetch=missing)[0])


class RepositoryManifest(unittest.TestCase):
    def test_committed_manifest_matches_the_tree(self):
        actual = rm.build_manifest()
        path = rm.manifest_path(rm.ROOT, actual)
        self.assertTrue(path.is_file(), f'{path} missing: run make release-manifest')
        problems = rm.diff(json.loads(path.read_text()), actual)
        self.assertEqual(problems, [], '\n'.join(problems))


if __name__ == '__main__':
    unittest.main()
