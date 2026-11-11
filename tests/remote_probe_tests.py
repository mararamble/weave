"""Read-only helper tests; invoked by cargo test with an isolated environment."""
import hashlib
import errno
import importlib.util
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest

spec = importlib.util.spec_from_file_location('remote_probe', Path(__file__).parents[1] / 'src/remote_probe.py')
probe = importlib.util.module_from_spec(spec)
spec.loader.exec_module(probe)


def git(path, *args):
    env = dict(PATH='/usr/bin:/bin', HOME=str(path), GIT_CONFIG_NOSYSTEM='1',
               GIT_CONFIG_GLOBAL='/dev/null', GIT_OPTIONAL_LOCKS='0',
               GIT_AUTHOR_NAME='Fixture', GIT_AUTHOR_EMAIL='fixture@example.invalid',
               GIT_COMMITTER_NAME='Fixture', GIT_COMMITTER_EMAIL='fixture@example.invalid')
    subprocess.run(['git', '-c', 'core.hooksPath=/dev/null', '-c', 'core.fsmonitor=false',
                    '-C', str(path), *args], env=env, check=True, stdout=subprocess.PIPE,
                   stderr=subprocess.PIPE)


def snapshot(root):
    result = {}
    for current, dirs, files in os.walk(root, followlinks=False):
        for name in dirs + files:
            path = Path(current) / name
            info = path.lstat()
            data = os.readlink(path) if path.is_symlink() else (path.read_bytes() if path.is_file() else b'')
            if isinstance(data, str):
                data = os.fsencode(data)
            result[os.fsencode(path)] = (info.st_mode, info.st_size, info.st_mtime_ns, hashlib.sha256(data).hexdigest())
    return result


class ProbeTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix='weave-remote-')
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name).resolve()

    def discover(self, depth=4):
        return probe.probe(dict(roots=[str(self.root)], operation='discover', max_depth=depth))

    def repository(self, name='repo'):
        path = self.root / name
        path.mkdir()
        git(path, 'init', '-b', 'trunk')
        return path

    def test_normal_bare_linked_detached_packed_unborn_and_no_mutation(self):
        repo = self.repository('repo space;$(ignored)')
        (repo / 'file').write_text('base\n')
        git(repo, 'add', '.')
        git(repo, 'commit', '-m', 'base')
        linked = self.root / 'linked space'
        git(repo, 'worktree', 'add', '-b', 'topic', str(linked))
        detached = self.root / 'detached'
        git(repo, 'worktree', 'add', '--detach', str(detached))
        git(repo, 'pack-refs', '--all')
        bare = self.root / 'bare.git'
        bare.mkdir()
        git(bare, 'init', '--bare', '-b', 'trunk')
        unborn = self.repository('unborn')
        # These settings would execute commands if a careless status call ran.
        git(repo, 'config', 'core.fsmonitor', 'touch should-never-exist')
        git(repo, 'config', 'core.hooksPath', str(self.root / 'hostile-hooks'))
        (linked / 'file').write_text('dirty\n')
        (linked / 'untracked').write_bytes(b'private content never collected')
        before = snapshot(self.root)
        result = self.discover()
        self.assertEqual(snapshot(self.root), before)
        self.assertEqual(result['state'], 'complete', result['issues'])
        records = {r['path']: r for r in result['repositories']}
        self.assertEqual(set(records), {str(p) for p in [repo, linked, detached, bare, unborn]})
        self.assertEqual(records[str(repo)]['branch'], 'trunk')
        self.assertEqual(records[str(linked)]['kind'], 'linked')
        self.assertIsNone(records[str(detached)]['branch'])
        self.assertTrue(records[str(bare)]['unborn'])
        self.assertEqual(records[str(bare)]['kind'], 'bare')
        self.assertTrue(records[str(unborn)]['unborn'])

    def test_non_utf8_newline_path_bytes_are_exact(self):
        raw = os.fsencode(self.root) + b'/odd-\xff\nname'
        try:
            os.mkdir(raw)
        except OSError as error:
            if error.errno == errno.EILSEQ:
                self.skipTest("Filesystem rejects non-UTF-8 names (APFS); byte validation covered in Rust")
            raise
        git(os.fsdecode(raw), 'init', '-b', 'trunk')
        result = self.discover()
        self.assertEqual(result['state'], 'complete')
        record = result['repositories'][0]
        self.assertEqual(record['path_hex'], raw.hex())
        self.assertTrue(record['paths_lossy'])

    def test_newline_path_bytes_are_exact(self):
        repo = self.repository('line\nbreak')
        result = self.discover()
        self.assertEqual(result['state'], 'complete')
        self.assertEqual(result['repositories'][0]['path_hex'], os.fsencode(repo).hex())

    def test_symlinks_escape_missing_roots_and_depth_are_partial(self):
        with tempfile.TemporaryDirectory() as other:
            (self.root / 'symlink').symlink_to(other, target_is_directory=True)
            result = self.discover()
            self.assertEqual(result['state'], 'partial')
            self.assertEqual(result['repositories'], [])
            self.assertEqual(result['issues'][0]['reason'], 'symlink_skipped')
        nested = self.root / 'nested'
        nested.mkdir()
        result = self.discover(0)
        self.assertIn('depth_limit', [i['reason'] for i in result['issues']])
        result = probe.probe(dict(roots=[str(self.root / 'missing')], operation='discover', max_depth=4))
        self.assertEqual(result['state'], 'partial')

    def test_external_metadata_and_malformed_head_are_partial(self):
        repo = self.repository()
        (repo / '.git/HEAD').write_text('not a head\n')
        result = self.discover()
        self.assertEqual(result['state'], 'partial')
        self.assertIn('malformed_head', [i['reason'] for i in result['issues']])
        separate = self.root / 'separate'
        separate.mkdir()
        (separate / '.git').write_text('gitdir: /outside/permitted/roots\n')
        result = self.discover()
        self.assertIn('metadata_outside_roots', [i['reason'] for i in result['issues']])

    def test_metadata_symlink_fifo_encoding_and_limits_refuse_safely(self):
        repo = self.repository()
        head = repo / '.git/HEAD'
        head.unlink()
        head.symlink_to('/etc/passwd')
        self.assertEqual(self.discover()['state'], 'partial')
        head.unlink()
        os.mkfifo(head)
        self.assertEqual(self.discover()['state'], 'partial')
        head.unlink()
        head.write_bytes(b'ref: refs/heads/\xff\n')
        self.assertIn('unsupported_metadata_encoding', [i['reason'] for i in self.discover()['issues']])
        head.write_bytes(b'x' * (probe.MAX_FILE + 1))
        self.assertIn('metadata_size_limit', [i['reason'] for i in self.discover()['issues']])

    def test_limits_and_permission_denial_report_partial(self):
        self.repository()
        limit = probe.MAX_DIRS
        try:
            probe.MAX_DIRS = 1
            self.assertEqual(self.discover()['state'], 'partial')
        finally:
            probe.MAX_DIRS = limit
        private = self.root / 'private'
        private.mkdir()
        private.chmod(0)
        try:
            if os.geteuid() != 0:
                self.assertIn('permission_denied', [i['reason'] for i in self.discover()['issues']])
        finally:
            private.chmod(0o700)


if __name__ == '__main__':
    unittest.main()
