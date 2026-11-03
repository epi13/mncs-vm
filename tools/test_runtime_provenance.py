"""Receipt freshness regressions using real files, Git and the provider protocol."""
import json
import shutil
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[1] / 'python'))
from mncs_vm_client import digest, inspect_runtime, sha

class RuntimeProvenanceTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name) / 'vm'
        (self.root / 'src').mkdir(parents=True)
        self.source = self.root / 'src/main.rs'
        self.source.write_text('fn main() {}\n')
        self.git('init', '-q')
        self.git('add', '.')
        self.commit()
        inputs = {str(self.source): sha(self.source)}
        tools = {}
        for name in ['cargo', 'rustc']:
            path = Path(shutil.which(name)).resolve()
            tools[name] = {'configured_path': str(path), 'resolved_path': str(path), 'sha256': sha(path)}
        revision = self.git('rev-parse', 'HEAD')
        self.receipt = {'schema_version': 'mncs.vm-build-receipt/1',
            'source_input_roots': [str(self.root / 'src'), str(self.root / '.cargo/config.toml')],
            'source_inputs': inputs, 'source_inputs_identity': digest(inputs),
            'source_revisions': {'mncs-vm': revision},
            'dependency_closure': [{'repository': 'mncs-vm', 'checkout': str(self.root), 'revision': revision}],
            'dirty_content_identity': digest({}), 'dirty_input_count': 0,
            'build_configuration': {'toolchain_executables': tools}}
        self.executable = Path(self.temporary.name) / 'fixture-provider'
        self.write_provider()

    def git(self, *args):
        return subprocess.check_output(['git', '-C', str(self.root), *args], text=True).strip()

    def commit(self):
        self.git('-c', 'user.name=Fixture', '-c', 'user.email=fixture@example.invalid', 'commit', '-qm', 'fixture')

    def write_provider(self):
        value = {'schema_version': 'mncs.vm.runtime-provider/1',
                 'build_origin': {'identity': 'sha256:' + digest(self.receipt), 'receipt': self.receipt}}
        self.executable.write_text('#!' + sys.executable + '\nprint(' + repr(json.dumps(value)) + ')\n')
        self.executable.chmod(0o700)

    def origin(self):
        return inspect_runtime(self.executable)['build_origin']

    def test_metadata_commit_preserves_content_freshness_and_original_provenance(self):
        original = self.origin()
        self.assertEqual(original['status'], 'matches-embedded-inputs')
        (self.root / 'README.md').write_text('metadata only\n')
        self.git('add', 'README.md'); self.commit()
        current = self.origin()
        self.assertEqual(current['status'], 'matches-embedded-inputs')
        self.assertEqual(current['receipt_identity'], original['receipt_identity'])
        self.assertNotEqual(current['current_source_revisions'], current['source_revisions'])

    def test_changed_source_still_invalidates(self):
        self.source.write_text('fn main() { panic!("changed"); }\n')
        current = self.origin()
        self.assertEqual(current['status'], 'stale-inputs')
        self.assertIn(str(self.source), current['mismatches'])

    def test_new_untracked_source_input_invalidates(self):
        (self.root / 'src/new.rs').write_text('pub fn added() {}\n')
        self.assertIn('source-input-set', self.origin()['mismatches'])

    def test_new_previously_absent_configuration_invalidates(self):
        path = self.root / '.cargo/config.toml'; path.parent.mkdir()
        path.write_text('[build]\njobs = 2\n')
        self.assertIn('source-input-set', self.origin()['mismatches'])

    def test_legacy_receipt_remains_conservative(self):
        self.receipt.pop('source_input_roots'); self.write_provider()
        (self.root / 'README.md').write_text('metadata\n')
        self.git('add', 'README.md'); self.commit()
        self.assertIn('source-revision:mncs-vm', self.origin()['mismatches'])

if __name__ == '__main__':
    unittest.main()
