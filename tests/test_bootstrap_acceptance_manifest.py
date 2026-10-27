import importlib.util
from pathlib import Path
import subprocess
import tempfile
import unittest
from unittest import mock

spec = importlib.util.spec_from_file_location('bootstrap_acceptance', Path(__file__).with_name('manual_bootstrap_acceptance.py'))
driver = importlib.util.module_from_spec(spec)
spec.loader.exec_module(driver)


class ManifestTests(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self.tmp.cleanup)
        self.root = Path(self.tmp.name)
        for name in driver.SPECS:
            repo = self.root / name
            repo.mkdir()
            subprocess.run(['git', 'init', '-q', str(repo)], check=True)
            subprocess.run(['git', '-C', str(repo), '-c', 'user.name=Test', '-c', 'user.email=test@example.invalid', 'commit', '--allow-empty', '-qm', 'initial'], check=True)

    def test_status_and_all_sources_checked(self):
        before, errors = driver.capture_manifests(self.root)
        self.assertFalse(errors)
        (self.root / 'mpgi-iac' / 'new').write_text('changed')
        (self.root / 'nanite' / 'new').write_text('changed')
        after, errors = driver.capture_manifests(self.root)
        self.assertFalse(errors)
        self.assertEqual(set(after), set(driver.SPECS))
        differences = driver.compare_manifests(before, after)
        self.assertTrue(any('mpgi-iac: status changed' in item for item in differences))
        self.assertTrue(any('nanite: status changed' in item for item in differences))

    def test_portable_file_addition_and_removal(self):
        beads = self.root / 'mpgi-iac' / '.beads'
        beads.mkdir()
        (beads / 'issues.jsonl').write_text('data')
        before, _ = driver.capture_manifests(self.root)
        (beads / 'issues.jsonl').unlink()
        (beads / 'metadata.json').write_text('new')
        after, _ = driver.capture_manifests(self.root)
        differences = driver.compare_manifests(before, after)
        self.assertTrue(any('issues.jsonl changed' in item for item in differences))
        self.assertTrue(any('metadata.json changed' in item for item in differences))

    def test_capture_continues_after_error(self):
        with mock.patch.object(driver, 'source_manifest', side_effect=[ValueError('broken'), {}, {}, {}]) as capture:
            manifests, errors = driver.capture_manifests(self.root)
        self.assertEqual(capture.call_count, 4)
        self.assertEqual(len(manifests), 3)
        self.assertIn('broken', errors[0])

    def test_result_distinguishes_workflow_guard_and_cleanup(self):
        result = driver.make_result('workflow failure', ['source changed'], ['cleanup failed'])
        self.assertFalse(result['workflow_pass'])
        self.assertFalse(result['overall_pass'])
        self.assertEqual(result['workflow_error'], 'workflow failure')
        self.assertEqual(result['guard_errors'], ['source changed'])
        self.assertEqual(result['cleanup_errors'], ['cleanup failed'])
        self.assertTrue(driver.make_result(None, [], [])['overall_pass'])
        self.assertFalse(driver.make_result(None, ['changed'], [])['overall_pass'])


if __name__ == '__main__':
    unittest.main()
