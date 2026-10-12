"""CLI permission summaries, closed input, and terminal echo restoration; no network."""
import errno
import fcntl
import os
from pathlib import Path
import pty
import select
import subprocess
import sys
import termios
import time

binary = str(Path(sys.argv[1]).resolve())
env = {key: value for key, value in os.environ.items() if key not in (
    'CONTROL_PLANE_TUNNEL_ID', 'CONTROL_PLANE_API_KEY', 'CONTROL_PLANE_BASE_URL',
    'RIVET_ALLOW_UNSAFE_CONTROL_PLANE',
)}
env['NO_COLOR'] = '1'
flags = ['--allow-command', 'shell=/bin/sh', '--allow-subcommand', 'shell=-c']

for network in (False, True):
    result = subprocess.run([binary, 'session', *flags, *(['--network'] if network else [])],
                            input='', text=True, capture_output=True, env=env, timeout=5)
    assert result.returncode != 0
    assert 'shell →' in result.stdout and '(-c)' in result.stdout, result.stdout
    assert 'Commands: none' not in result.stdout
    assert f'Child network: {"enabled" if network else "disabled"}' in result.stdout
    assert 'CONTROL_PLANE_TUNNEL_ID is required' in result.stderr, result.stderr

for data in ('7\n', '7\ncustom\n/bin/sh\n'):
    result = subprocess.run([binary, 'session'], input=data, text=True,
                            capture_output=True, env=env, timeout=5)
    assert result.returncode != 0 and 'input closed' in result.stderr, result.stderr
    assert len(result.stdout) < 4096


def terminal_case(key_prompt, action):
    master, slave = pty.openpty()
    os.set_blocking(master, False)
    # macOS revokes the slave when its controlling session exits; inspect via master.
    original = termios.tcgetattr(master)
    original_flags = fcntl.fcntl(slave, fcntl.F_GETFL)
    case_env = dict(env)
    if key_prompt:
        case_env['CONTROL_PLANE_TUNNEL_ID'] = 'tunnel_0123456789abcdefghijklmnopqrstuv'
    prompt = b'Runtime API key (input hidden):' if key_prompt else b'Tunnel ID (input hidden):'

    def controlling_terminal():
        os.setsid()
        fcntl.ioctl(0, termios.TIOCSCTTY, 0)

    process = None
    output = b''
    try:
        process = subprocess.Popen([binary, 'session', *flags], stdin=slave, stdout=slave,
                                   stderr=slave, env=case_env, preexec_fn=controlling_terminal)
        deadline = time.monotonic() + 5
        while prompt not in output:
            assert time.monotonic() < deadline, output.decode(errors='replace')
            assert process.poll() is None, output.decode(errors='replace')
            if select.select([master], [], [], 0.1)[0]:
                try:
                    output += os.read(master, 4096)
                except BlockingIOError:
                    pass
        assert not (termios.tcgetattr(master)[3] & termios.ECHO)
        if action == b'partial-interrupt':
            os.write(master, b'dummy-partial-input\x04')
            time.sleep(0.1)
            os.write(master, b'\x03')
        elif action == b"\x03":
            os.write(master, b'\x03')
        else:
            os.write(master, action)
        deadline = time.monotonic() + 5
        while process.poll() is None or select.select([master], [], [], 0)[0]:
            assert time.monotonic() < deadline, output.decode(errors='replace')
            if not select.select([master], [], [], 0.05)[0]:
                continue
            try:
                chunk = os.read(master, 4096)
            except OSError as error:
                if error.errno == errno.EAGAIN:
                    continue
                if error.errno == errno.EIO:
                    break
                raise
            if not chunk:
                break
            output += chunk
        process.wait(timeout=5)
        assert termios.tcgetattr(master) == original, 'terminal settings were not restored'
        assert process.returncode == 1, process.returncode
        assert fcntl.fcntl(slave, fcntl.F_GETFL) & os.O_NONBLOCK == original_flags & os.O_NONBLOCK, 'stdin blocking mode was not restored'
        expected = {b'\x03': b'interrupted', b'\x04': b'input closed',
                    b'partial-interrupt': b'interrupted', b'invalid-ui-test\n': b'invalid tunnel ID format'}[action]
        assert b'dummy-partial-input' not in output
        assert b'invalid-ui-test' not in output
        assert expected in output, output.decode(errors='replace')
    finally:
        termios.tcsetattr(master, termios.TCSANOW, original)
        os.close(master)
        os.close(slave)
        if process is not None and process.poll() is None:
            process.kill()
            process.wait(timeout=5)


for key_prompt in (False, True):
    for action in (b'\x03', b'\x04', b'partial-interrupt'):
        terminal_case(key_prompt, action)
terminal_case(False, b'invalid-ui-test\n')
print('CLI UI: permission summaries, EOF, and 7 terminal input/cancellation cases passed')
