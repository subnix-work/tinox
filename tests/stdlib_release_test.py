"""Release archive boundaries and registry failure handling."""
import importlib.util
import io
from pathlib import Path
import sys
import tarfile
import tempfile
import unittest
import urllib.error
from unittest.mock import patch

sys.path.insert(0, str(Path(__file__).resolve().parents[1] / 'scripts'))
SPEC = importlib.util.spec_from_file_location('stdlib_release', Path(__file__).resolve().parents[1] / 'scripts/publish-stdlib.py')
release = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(release)


class ReleaseChecks(unittest.TestCase):
    def test_archive_contains_only_manifest_and_module_sources_for_both_tiers(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            for directory in ['crates/tinox-core/packages', 'crates/tinox-core-ext']:
                manifest = root / directory / 'demo/tinox.toml'
                manifest.parent.mkdir(parents=True)
                manifest.write_text('[package]\ncompiler = "2.8.0"\nname = "demo"\nversion = "1.1.0"\n')
                package = dict(name='demo', manifest=str(manifest.relative_to(root)))
                source = release.reference.package_source(root, package)
                source.mkdir(parents=True)
                (source / 'Demo.tnx').write_text('class Demo {}\n')
                (source / 'old.tar.gz').write_bytes(b'not source')
                for excluded in ['tests', 'examples']:
                    path = manifest.parent / excluded / 'Main.tnx'
                    path.parent.mkdir()
                    path.write_text('class Main {}\n')
                with tarfile.open(fileobj=io.BytesIO(release.archive(root, package))) as tar:
                    self.assertEqual(set(tar.getnames()), {'tinox.toml', 'tinox/core/demo/Demo.tnx'})
                    self.assertEqual(tar.extractfile('tinox.toml').read(), manifest.read_bytes())

    def test_missing_registry_version_is_reported_without_credentials(self):
        error = urllib.error.HTTPError('https://example.invalid', 404, 'missing', {}, None)
        with patch.object(release.urllib.request, 'urlopen', side_effect=error):
            with self.assertRaisesRegex(ValueError, r'^GET https://example.invalid: HTTP 404$'):
                release.request('https://example.invalid', key='secret-must-not-appear')


if __name__ == '__main__':
    unittest.main()
