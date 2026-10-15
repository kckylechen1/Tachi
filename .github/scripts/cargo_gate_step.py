"""C1 command leases through the installed, job-bound Cargo hook protocol.

The job-start hook remains the fallback for old workflows. This helper never
installs hooks or bootstraps a lock. An anchor retains its process-group identity
until the command's descendants are checked; uncertainty leaves the job lease
with its existing holder for runner cleanup/completion.
"""
import os
from pathlib import Path
import select
import signal
import subprocess
import sys

HOOK_ROOT = Path.home() / 'bin'


class GateError(Exception):
    pass


def hook(action):
    path = HOOK_ROOT / ('cargo-gate-ci-' + action)
    if not path.is_file() or not os.access(path, os.X_OK):
        raise GateError('GATE_E_STEP_HOOK: installed hook unavailable')
    code = subprocess.call(['/bin/bash', str(path)])
    if code:
        raise GateError('GATE_E_STEP_HOOK: installed hook refused')


def group_drained(anchor):
    # The unreaped live anchor prevents reuse of this PGID during inspection.
    if anchor.poll() is not None:
        raise GateError('GATE_E_STEP_GROUP: anchor exited before inspection')
    result = subprocess.run(['ps', '-axo', 'pid=,pgid=,stat='], capture_output=True,
                            text=True, timeout=5, env=dict(os.environ, LC_ALL='C'))
    if result.returncode or not result.stdout.strip():
        raise GateError('GATE_E_STEP_GROUP: process inspection unavailable')
    seen = False
    for line in result.stdout.splitlines():
        fields = line.split()
        if len(fields) != 3 or not all(x.isdigit() for x in fields[:2]):
            raise GateError('GATE_E_STEP_GROUP: malformed process inspection')
        pid, pgid = map(int, fields[:2])
        if pgid == anchor.pid:
            if pid == anchor.pid:
                seen = not fields[2].startswith('Z')
            elif not fields[2].startswith('Z'):
                return False
    if not seen:
        raise GateError('GATE_E_STEP_GROUP: live anchor identity unproven')
    return True


def anchor_main(fd, argv):
    try:
        code = subprocess.call(argv, stdin=subprocess.DEVNULL, close_fds=True)
    except OSError:
        code = 70
    os.write(fd, (str(code) + '\n').encode())
    os.close(fd)
    sys.stdin.buffer.read(1)
    return code if code >= 0 else 128 - code


def run(argv):
    if not argv:
        raise GateError('GATE_E_STEP_COMMAND: missing command')
    # Probe both entry points before admitting any command.
    for action in ('start', 'end'):
        path = HOOK_ROOT / ('cargo-gate-ci-' + action)
        if not path.is_file() or not os.access(path, os.X_OK):
            raise GateError('GATE_E_STEP_HOOK: installed hook unavailable')
    hook('start')
    read_fd, write_fd = os.pipe()
    anchor = None
    try:
        anchor = subprocess.Popen([sys.executable, __file__, '_anchor', str(write_fd), *argv],
                                  stdin=subprocess.PIPE, pass_fds=(write_fd,),
                                  start_new_session=True, close_fds=True)
        os.close(write_fd)
        write_fd = None
        data = b''
        while b'\n' not in data:
            ready, _, _ = select.select([read_fd], [], [], .2)
            if ready:
                part = os.read(read_fd, 32)
                if not part:
                    raise GateError('GATE_E_STEP_COMMAND: command result unavailable')
                data += part
                if len(data) > 32:
                    raise GateError('GATE_E_STEP_COMMAND: malformed command result')
            elif anchor.poll() is not None:
                raise GateError('GATE_E_STEP_COMMAND: anchor exited without result')
        code = int(data.strip())
        if not group_drained(anchor):
            raise GateError('GATE_E_STEP_GROUP: descendants remain; retaining job lease')
        hook('end')
        return code if code >= 0 else 128 - code
    finally:
        os.close(read_fd)
        if write_fd is not None:
            os.close(write_fd)
        if anchor is not None:
            anchor.stdin.close()
            # No foreign process or holder is signalled. On interruption the
            # command may still run; the holder keeps custody until job cleanup.
            try:
                anchor.wait(timeout=2)
            except subprocess.TimeoutExpired:
                pass


def main():
    args = sys.argv[1:]
    if args[:1] == ['_anchor']:
        return anchor_main(int(args[1]), args[2:])
    if (os.environ.get('GITHUB_REPOSITORY', '').rsplit('/', 1)[-1].lower() != 'tachi' or
            os.environ.get('GITHUB_WORKFLOW') != 'Full CI' or os.environ.get('GITHUB_JOB') != 'rust'):
        raise GateError('GATE_E_STEP_JOB: command leases require the Full CI Rust job')
    def interrupted(*_):
        raise GateError('GATE_E_STEP_INTERRUPTED: retaining job lease for cleanup')
    for sig in (signal.SIGINT, signal.SIGTERM, signal.SIGHUP):
        signal.signal(sig, interrupted)
    if args == ['release']:
        hook('end')
        return 0
    if args[:1] != ['run'] or args[1:2] != ['--']:
        raise GateError('GATE_E_STEP_COMMAND: expected release or run -- COMMAND')
    return run(args[2:])


if __name__ == '__main__':
    try:
        sys.exit(main())
    except (GateError, OSError, ValueError, subprocess.SubprocessError) as error:
        print(str(error) if isinstance(error, GateError) else 'GATE_E_STEP_RUNTIME', file=sys.stderr)
        sys.exit(70)
