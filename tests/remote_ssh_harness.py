"""Opt-in real OpenSSH harness: python3 -I -B tests/remote_ssh_harness.py.

Requires a runnable local sshd, ssh-keygen, Git, Python 3.9+. Generates all
keys/config/repositories under a temporary directory. Never edits ~/.ssh.
This is same-machine coverage, not Mac-to-Linux acceptance.
"""
import getpass
import importlib.util
import json
import os
from pathlib import Path
import shutil
import socket
import subprocess
import tempfile
import time

spec = importlib.util.spec_from_file_location('fixtures', Path(__file__).with_name('remote_probe_tests.py'))
fixtures = importlib.util.module_from_spec(spec)
spec.loader.exec_module(fixtures)


def run(args, **kwargs):
    return subprocess.run(args, env=dict(PATH='/usr/bin:/bin'), stdout=subprocess.PIPE,
                          stderr=subprocess.PIPE, timeout=15, **kwargs)


def main():
    sshd = shutil.which('sshd') or '/usr/sbin/sshd'
    with tempfile.TemporaryDirectory(prefix='weave-sshd-') as temporary:
        root = Path(temporary).resolve()
        for name in ['host', 'client', 'wrong']:
            result = run(['ssh-keygen', '-q', '-t', 'ed25519', '-N', '', '-f', str(root / name)])
            assert result.returncode == 0, result.stderr
        with socket.socket() as sock:
            sock.bind(('127.0.0.1', 0))
            port = sock.getsockname()[1]
        server_config = root / 'sshd_config'
        server_config.write_text(f'''ListenAddress 127.0.0.1
Port {port}
HostKey {root}/host
PidFile {root}/pid
AuthorizedKeysFile {root}/client.pub
StrictModes no
PasswordAuthentication no
KbdInteractiveAuthentication no
UsePAM no
LogLevel ERROR
''')
        log = root / 'server.log'
        with log.open('wb') as errors:
            server = subprocess.Popen([sshd, '-D', '-e', '-f', str(server_config)],
                                      env=dict(PATH='/usr/bin:/bin'), stdout=subprocess.DEVNULL,
                                      stderr=errors)
            try:
                for _ in range(50):
                    if server.poll() is not None:
                        raise RuntimeError('Local sshd unavailable: ' + log.read_text())
                    try:
                        with socket.create_connection(('127.0.0.1', port), timeout=0.1):
                            break
                    except OSError:
                        time.sleep(0.1)
                known = root / 'known_hosts'
                known.write_text('fixture ' + (root / 'host.pub').read_text())
                client = root / 'ssh_config'
                client.write_text(f'''Host fixture
 HostName 127.0.0.1
 HostKeyAlias fixture
 Port {port}
 User {getpass.getuser()}
 IdentityFile {root}/client
 IdentitiesOnly yes
 UserKnownHostsFile {known}
 GlobalKnownHostsFile /dev/null
''')
                repos = root / 'repositories'
                repos.mkdir()
                repo = repos / 'space repo'
                repo.mkdir()
                fixtures.git(repo, 'init', '-b', 'trunk')
                (repo / 'file').write_text('base\n')
                fixtures.git(repo, 'add', '.')
                fixtures.git(repo, 'commit', '-m', 'base')
                fixtures.git(repo, 'worktree', 'add', '-b', 'topic', str(repos / 'linked'))
                before = fixtures.snapshot(repos)
                token = 'acceptance-fixture'
                request = dict(roots=[str(repos)], operation='discover', max_depth=4, token=token)
                source = ('import json\nREQUEST = json.loads(bytes.fromhex("' +
                          json.dumps(request).encode().hex() + '").decode())\n' +
                          (Path(__file__).parents[1] / 'src/remote_probe.py').read_text()).encode()
                # Match production trust/isolation options; -F selects only the
                # generated fixture configuration rather than the user's config.
                args = ['ssh', '-F', str(client), '-T', '-oBatchMode=yes',
                        '-oStrictHostKeyChecking=yes', '-oUpdateHostKeys=no',
                        '-oVerifyHostKeyDNS=no', '-oKnownHostsCommand=none',
                        '-oConnectTimeout=10', '-oConnectionAttempts=1',
                        '-oClearAllForwardings=yes', '-oForwardAgent=no', '-oForwardX11=no',
                        '-oTunnel=no', '-oPermitLocalCommand=no', '-oRemoteCommand=none',
                        '-oRequestTTY=no', '-oControlMaster=no', '-oControlPath=none',
                        '-oControlPersist=no', '-oServerAliveInterval=5', '-oServerAliveCountMax=1',
                        '--', 'fixture', 'exec env -i PATH=/usr/bin:/bin LC_ALL=C python3 -I -B -']
                output = run(args, input=source)
                assert output.returncode == 0, output.stderr
                begin = ('\nWEAVE_REMOTE_' + token + '_BEGIN\n').encode()
                end = ('\nWEAVE_REMOTE_' + token + '_END\n').encode()
                result = json.loads(output.stdout.split(begin)[1].split(end)[0])
                assert result['state'] == 'complete', result
                assert len(result['repositories']) == 2, result
                assert fixtures.snapshot(repos) == before, 'Remote fixture mutated'
                known_before = known.read_bytes()
                known.write_bytes(b'')
                assert run(args, input=source).returncode == 255, 'Unknown key accepted'
                assert known.read_bytes() == b'', 'known_hosts was modified'
                known.write_text('fixture ' + (root / 'wrong.pub').read_text())
                changed_before = known.read_bytes()
                assert run(args, input=source).returncode == 255, 'Changed key accepted'
                assert known.read_bytes() == changed_before, 'Changed key was replaced'
                known.write_bytes(known_before)
                client.write_text(client.read_text().replace(str(root / 'client\n'), str(root / 'wrong\n')))
                assert run(args, input=source).returncode == 255, 'Wrong identity accepted'
                assert fixtures.snapshot(repos) == before, 'Refusal mutated fixture'
                print('PASS: real SSH discovery; unknown/changed host keys; authentication refusal; repository byte/mode/mtime preservation')
            finally:
                server.terminate()
                server.wait(timeout=5)


if __name__ == '__main__':
    main()
