"""Failure-path checks for documentation publication and bilingual drift."""
import importlib.util
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch

SPEC = importlib.util.spec_from_file_location('reference', Path(__file__).resolve().parents[1] / 'scripts/reference.py')
reference = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(reference)


class ReferenceChecks(unittest.TestCase):
    def test_inventory_rejects_translation_drift_and_dangling_navigation(self):
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            good = '<a href="#mod-a">A</a><div class="mod-section" id="mod-a"></div>'
            (root / 'docs.html').write_text(good)
            (root / 'docs_en.html').write_text(good)
            self.assertEqual(reference.check_inventories(root), ['mod-a'])
            (root / 'docs_en.html').write_text(good.replace('mod-a', 'mod-b'))
            with self.assertRaisesRegex(ValueError, 'inventories differ'):
                reference.check_inventories(root)
            (root / 'docs_en.html').write_text(good.replace('id="mod-a"', 'id="mod-b"'))
            with self.assertRaisesRegex(ValueError, 'missing anchors'):
                reference.check_inventories(root)

    def test_existing_versions_are_never_rewritten(self):
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            page = root / 'docs/tinox-core/demo/1-0-0/docs.html'
            page.parent.mkdir(parents=True)
            content = '<p>Immutable historical explanation</p>'
            page.write_text(content)
            package = dict(docs=str(page.relative_to(root)))
            with patch.object(reference, 'run', side_effect=AssertionError('must not generate')):
                self.assertFalse(reference.stage_package(root, Path('unused'), package))
            self.assertEqual(page.read_text(), content)
            self.assertEqual(list(page.parent.iterdir()), [page])

    def test_broken_dependency_link_blocks_release(self):
        with tempfile.TemporaryDirectory() as temp:
            page = Path(temp) / 'docs.html'
            page.write_text('<a href="../../absent/1-0-0/docs.html">dependency</a>')
            with self.assertRaisesRegex(ValueError, 'broken dependency'):
                reference.check_links(page)

    def test_new_version_is_staged_with_source_metadata_once(self):
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            manifest = root / 'crates/tinox-core-ext/demo/tinox.toml'
            manifest.parent.mkdir(parents=True)
            manifest.write_text('[package]\ncompiler = \"2.8.0\"\nname = "demo"\nversion = "1.0.1"\n')
            source = manifest.parent / 'tinox/core/demo'
            source.mkdir(parents=True)
            (source / 'Demo.tnx').write_text('class Demo {}')
            package = dict(name='demo', manifest=str(manifest.relative_to(root)),
                           docs='docs/tinox-core/demo/1-0-1/docs.html')

            def fake_run(binary, *args, cwd=None):
                if args[0] == 'doc':
                    self.assertTrue((cwd / 'src/Demo.tnx').is_file())
                    self.assertEqual((cwd / 'tinox.toml').read_text(), manifest.read_text())
                    Path(args[2]).write_text('<p>Generated API</p>')
                    return ''
                return '{"schema_version": 1, "sources": {"Demo.tnx": []}}'

            with patch.object(reference, 'run', side_effect=fake_run):
                self.assertTrue(reference.stage_package(root, Path('unused'), package))
                self.assertFalse(reference.stage_package(root, Path('unused'), package))
            self.assertTrue((root / package['docs']).is_file())
            self.assertTrue((root / package['docs']).with_name('reference.json').is_file())

    def test_invalid_coordinates_and_incomplete_versions_fail(self):
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            with self.assertRaises(ValueError):
                reference.page_path(root, '../group', 'demo', '1.0.0')
            page = root / 'docs/tinox-core/demo/1-0-0/docs.html'
            page.parent.mkdir(parents=True)
            with self.assertRaisesRegex(ValueError, 'incomplete version'):
                reference.stage_package(root, Path('unused'), dict(docs=str(page.relative_to(root))))


if __name__ == '__main__':
    unittest.main()
