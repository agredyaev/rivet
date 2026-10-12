"""Release-only CI must never admit source, dependency, or workflow changes."""
import copy
import importlib.util
import json
import os
from pathlib import Path
import subprocess
import tempfile
import unittest
from unittest.mock import patch

spec = importlib.util.spec_from_file_location('ci_mode', Path(__file__).resolve().parents[1] / 'scripts/ci_mode.py')
mode = importlib.util.module_from_spec(spec)
spec.loader.exec_module(mode)


def files(version):
    return {
        'Cargo.toml': f'[package]\nname = "rivet"\nversion = "{version}"\nedition = "2024"\n\n[dependencies]\nbytes = "=1.0.0"\n',
        'Cargo.lock': f'version = 4\n\n[[package]]\nname = "rivet"\nversion = "{version}"\ndependencies = ["bytes"]\n\n[[package]]\nname = "bytes"\nversion = "1.0.0"\nchecksum = "abc"\n',
        '.release-please-manifest.json': json.dumps({'.': version}),
        'CHANGELOG.md': f'# {version}\n',
    }


class ReleaseMode(unittest.TestCase):
    def test_version_metadata_only(self):
        self.assertTrue(mode.version_only(files('0.3.0'), files('0.4.0')))
        before, after = files('0.3.0'), files('0.4.0')
        before['fuzz/Cargo.lock'], after['fuzz/Cargo.lock'] = before['Cargo.lock'], after['Cargo.lock']
        self.assertTrue(mode.version_only(before, after))

    def test_non_release_changes_force_full_checks(self):
        changes = [
            ('Cargo.toml', lambda text: text.replace('=1.0.0', '=1.1.0')),
            ('Cargo.toml', lambda text: text.replace('2024', '2021')),
            ('Cargo.lock', lambda text: text.replace('checksum = "abc"', 'checksum = "def"')),
            ('Cargo.lock', lambda text: text.replace('name = "bytes"\nversion = "1.0.0"', 'name = "bytes"\nversion = "1.1.0"')),
            ('Cargo.lock', lambda text: text.replace('["bytes"]', '[]')),
            ('.release-please-manifest.json', lambda text: '{".": "0.4.0", "other": "1.0.0"}'),
            ('src/main.rs', lambda text: 'modified code'),
            ('.github/workflows/ci.yml', lambda text: 'modified checks'),
            ('fuzz/Cargo.lock', lambda text: 'modified fuzz dependencies'),
        ]
        for path, change in changes:
            with self.subTest(path=path):
                after = files('0.4.0')
                after[path] = change(after.get(path, ''))
                self.assertFalse(mode.version_only(files('0.3.0'), after))

    def test_inconsistent_missing_and_backward_versions_are_rejected(self):
        before = files('0.3.0')
        for version in ('0.3.0', '0.2.0', 'garbage'):
            self.assertFalse(mode.version_only(before, files(version)))
        for path in ('Cargo.toml', 'Cargo.lock', '.release-please-manifest.json'):
            after = files('0.4.0')
            after[path] = before[path]
            self.assertFalse(mode.version_only(before, after))
            del after[path]
            self.assertFalse(mode.version_only(before, after))

    def test_baseline_requires_full_success_on_exact_main_commit(self):
        run = {'id': 123, 'head_sha': 'base', 'head_branch': 'main', 'event': 'push',
               'status': 'completed', 'conclusion': 'success'}
        jobs = [{'name': name, 'status': 'completed', 'conclusion': 'success'} for name in mode.FULL_JOBS]
        self.assertTrue(mode.full_run(run, jobs, 'base'))
        for field, value in [('head_sha', 'other'), ('head_branch', 'feature'), ('event', 'pull_request'),
                             ('event', 'workflow_dispatch'), ('status', 'in_progress'), ('conclusion', 'failure')]:
            self.assertFalse(mode.full_run(dict(run, **{field: value}), jobs, 'base'))
        for index in range(len(jobs)):
            incomplete = copy.deepcopy(jobs)
            incomplete[index]['conclusion'] = 'skipped'
            self.assertFalse(mode.full_run(run, incomplete, 'base'))

    def test_real_git_diff_and_api_failure_fallback(self):
        with tempfile.TemporaryDirectory() as directory:
            original = os.getcwd()
            os.chdir(directory)
            try:
                def git(*args):
                    return subprocess.check_output(['git', *args], stderr=subprocess.DEVNULL).decode().strip()
                git('init', '-q')
                git('config', 'user.name', 'CI test')
                git('config', 'user.email', 'ci@example.invalid')
                def commit(version):
                    for name, value in files(version).items():
                        Path(name).write_text(value)
                    git('add', '.')
                    git('commit', '-qm', version)
                commit('0.3.0')
                base = git('rev-parse', 'HEAD')
                commit('0.4.0')
                self.assertTrue(mode.release_diff(base))
                output, summary = Path('output'), Path('summary')
                env = {'GITHUB_OUTPUT': str(output), 'GITHUB_STEP_SUMMARY': str(summary)}
                def run(event):
                    with patch.dict(os.environ, env), patch('sys.argv', ['ci_mode', '--base', base, '--repo', 'owner/repo', '--event', event]):
                        mode.main()
                successful_run = {'id': 123, 'head_sha': base, 'head_branch': 'main', 'event': 'push',
                                  'status': 'completed', 'conclusion': 'success'}
                successful_jobs = [{'name': name, 'status': 'completed', 'conclusion': 'success'} for name in mode.FULL_JOBS]
                with patch.object(mode, 'api', side_effect=[{'workflow_runs': [successful_run]}, {'jobs': successful_jobs}]):
                    run('push')
                self.assertIn('release_only=true', output.read_text())
                output.write_text('')
                with patch.object(mode, 'api', side_effect=subprocess.CalledProcessError(1, 'gh')):
                    run('push')
                self.assertIn('release_only=false', output.read_text())
                output.write_text('')
                with patch.object(mode, 'api', return_value={'workflow_runs': []}):
                    run('pull_request')
                self.assertIn('release_only=false', output.read_text())
                with patch.object(mode, 'api') as api:
                    run('workflow_dispatch')
                    api.assert_not_called()
                Path('Cargo.toml').chmod(0o755)
                git('add', 'Cargo.toml')
                git('commit', '-qm', 'mode changed')
                self.assertFalse(mode.release_diff(base))
                Path('src').mkdir()
                Path('src/main.rs').write_text('changed')
                git('add', 'src/main.rs')
                git('commit', '-qm', 'source changed')
                self.assertFalse(mode.release_diff(base))
            finally:
                os.chdir(original)


if __name__ == '__main__':
    unittest.main()
