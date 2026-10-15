"""C1 job-bound queue/execution budgets using the installed Cargo lease.

Only the installed holder owns the machine build lock. The RUNNER_TEMP guard
serializes this job's budget ledger; it never grants build admission.
"""
import fcntl
import json
import math
import os
from pathlib import Path
import re
import select
import signal
import stat
import subprocess
import sys
import time
import uuid

HOOK_ROOT = Path.home() / 'bin'
QUEUE_BUDGET = 900.0
EXEC_BUDGET = 1800.0
TERM_GRACE = 2.0
KILL_GRACE = 2.0
SIGNALS = (signal.SIGINT, signal.SIGTERM, signal.SIGHUP)


class GateError(Exception):
    def __init__(self, message, code=70):
        super().__init__(message)
        self.code = code


def fail(kind, message, code=70):
    raise GateError(f'GATE_E_{kind}: {message}', code)


def decode(data):
    def unique(pairs):
        value = {}
        for key, item in pairs:
            if key in value:
                raise ValueError('duplicate JSON key')
            value[key] = item
        return value
    return json.loads(data, object_pairs_hook=unique)


def number(value):
    return type(value) in (int, float) and math.isfinite(value) and value >= 0


def report(phase, value):
    # Observability must never unwind admission or safety cleanup.
    try:
        print('cargo-gate-budget: ' + json.dumps({
            'phase': phase, 'status': value['status'],
            'queue_total_seconds': round(value['queue_seconds'], 3),
            'execution_total_seconds': round(value['execution_seconds'], 3),
            'queue_remaining_seconds': round(max(0, QUEUE_BUDGET - value['queue_seconds']), 3),
            'execution_remaining_seconds': round(max(0, EXEC_BUDGET - value['execution_seconds']), 3),
        }, sort_keys=True), flush=True)
    except (OSError, ValueError):
        pass


def process(pid):
    result = subprocess.run(['ps', '-p', str(pid), '-o', 'ppid=', '-o', 'lstart=',
                             '-o', 'stat=', '-o', 'comm='], capture_output=True,
                            text=True, timeout=5, env=dict(os.environ, LC_ALL='C'))
    fields = result.stdout.strip().split(None, 7)
    if (result.returncode or len(fields) != 8 or not fields[0].isdigit() or
            fields[6].startswith('Z')):
        fail('STEP_WORKER', 'cannot establish live process identity')
    return {'pid': pid, 'start': ' '.join(fields[1:6]),
            'ppid': int(fields[0]), 'command': fields[7]}


def identity():
    names = ('GITHUB_REPOSITORY', 'GITHUB_WORKFLOW', 'GITHUB_JOB',
             'GITHUB_RUN_ID', 'GITHUB_RUN_ATTEMPT')
    job = {key: os.environ.get(key, '') for key in names}
    if (not re.fullmatch(r'[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+', job[names[0]]) or
            job[names[0]].rsplit('/', 1)[-1].lower() != 'tachi' or
            job[names[1]] != 'Full CI' or job[names[2]] != 'rust' or
            not all(re.fullmatch(r'[0-9]+', job[key]) for key in names[3:])):
        fail('STEP_JOB', 'command leases require the identified Full CI Rust job')
    pid = os.getpid()
    for _ in range(32):
        current = process(pid)
        if current['ppid'] <= 1:
            break
        pid = current['ppid']
        parent = process(pid)
        if Path(parent['command']).name == 'Runner.Worker':
            return {'job': job, 'worker': parent}
    fail('STEP_WORKER', 'Runner.Worker ancestor not established')


def installed():
    for action in ('start', 'end'):
        path = HOOK_ROOT / ('cargo-gate-ci-' + action)
        if not path.is_file() or not os.access(path, os.X_OK):
            fail('STEP_HOOK', 'installed hook unavailable')
    if not (HOOK_ROOT / 'cargo-gate-hold').is_file():
        fail('STEP_HOOK', 'installed snapshot helper unavailable')


def hook(action, *args):
    child = subprocess.Popen(['/bin/bash', str(HOOK_ROOT / ('cargo-gate-ci-' + action)), *args])
    try:
        code = child.wait()
    finally:
        # Stop only our unreaped hook child. Its own handler drains the pending
        # holder before exit; never leave a cancelled admission subprocess.
        handlers = {sig: signal.signal(sig, signal.SIG_IGN) for sig in SIGNALS}
        try:
            if child.poll() is None:
                child.terminate()
                try:
                    child.wait(timeout=5)
                except subprocess.TimeoutExpired:
                    child.kill()
                    child.wait()
        finally:
            for sig, handler in handlers.items():
                signal.signal(sig, handler)
    if code == 75 and action == 'start':
        fail('QUEUE_TIMEOUT', 'infra_blocked: cumulative job queue budget exhausted', 75)
    if code:
        fail('STEP_HOOK', 'installed hook refused')


def snapshot(expected):
    result = subprocess.run([sys.executable, str(HOOK_ROOT / 'cargo-gate-hold'), 'snapshot'],
                            capture_output=True, text=True, timeout=10)
    try:
        value = decode(result.stdout)
    except ValueError:
        fail('STEP_SNAPSHOT', 'installed lease snapshot unavailable')
    now = time.monotonic()
    if (result.returncode or not isinstance(value, dict) or type(value.get('protocol')) is not int or value['protocol'] != 1 or
            value.get('worker') != expected['worker'] or value.get('job') != expected['job'] or
            not number(value.get('queue_seconds')) or
            not number(value.get('acquired_monotonic')) or value['acquired_monotonic'] > now):
        fail('STEP_SNAPSHOT', 'installed lease identity or timing invalid')
    return value


class Budget:
    def __init__(self, expected):
        root = Path(os.environ.get('RUNNER_TEMP', ''))
        if not root.is_absolute():
            fail('STEP_BUDGET', 'RUNNER_TEMP must be an existing absolute directory')
        info = root.lstat()
        if not stat.S_ISDIR(info.st_mode) or info.st_uid != os.getuid():
            fail('STEP_BUDGET', 'unsafe RUNNER_TEMP directory')
        self.path = root / 'cargo-gate-step-budget.json'
        self.expected = expected
        self.previous = None
        self.fd = os.open(root / 'cargo-gate-step-budget.lock',
                          os.O_RDWR | os.O_CREAT | os.O_NOFOLLOW | os.O_NONBLOCK, 0o600)
        try:
            self.safe_file(os.fstat(self.fd))
            fcntl.flock(self.fd, fcntl.LOCK_EX | fcntl.LOCK_NB)
        except BaseException:
            os.close(self.fd)
            raise

    @staticmethod
    def safe_file(info):
        if (not stat.S_ISREG(info.st_mode) or info.st_nlink != 1 or
                stat.S_IMODE(info.st_mode) != 0o600 or info.st_uid != os.getuid()):
            fail('STEP_BUDGET', 'budget files must be owned regular single-link mode-0600 files')

    def close(self):
        os.close(self.fd)

    def read(self):
        fd = os.open(self.path, os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK)
        try:
            self.safe_file(os.fstat(fd))
            data = os.read(fd, 16385)
            if len(data) > 16384:
                fail('STEP_BUDGET', 'oversized budget record')
            return data
        finally:
            os.close(fd)

    def load(self):
        try:
            self.previous = self.read()
            value = decode(self.previous)
        except (OSError, ValueError):
            fail('STEP_BUDGET', 'budget missing or malformed; release must initialize once')
        if (not isinstance(value, dict) or set(value) !=
                {'protocol', 'identity', 'status', 'queue_seconds', 'execution_seconds'} or
                type(value['protocol']) is not int or value['protocol'] != 1 or value['identity'] != self.expected or
                value['status'] != 'ready' or not number(value['queue_seconds']) or
                not number(value['execution_seconds'])):
            fail('STEP_BUDGET', 'budget identity/state invalid or prior phase unfinished')
        return value

    def save(self, value, initial=False):
        data = (json.dumps(value, sort_keys=True) + '\n').encode()
        temporary = self.path.with_name('.cargo-gate-budget.' + uuid.uuid4().hex + '.tmp')
        fd = os.open(temporary, os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_NOFOLLOW, 0o600)
        try:
            with os.fdopen(fd, 'wb', closefd=False) as stream:
                stream.write(data)
                stream.flush()
                os.fsync(fd)
            if initial:
                os.link(temporary, self.path)
            else:
                if self.read() != self.previous:
                    fail('STEP_BUDGET', 'budget changed during phase')
                os.replace(temporary, self.path)
            self.previous = data
        finally:
            os.close(fd)
            if temporary.exists():
                temporary.unlink()


def group_drained(anchor, allow_dead=False):
    # Do not poll/wait/reap the anchor before inspection, even after SIGKILL:
    # an unreaped child (including a zombie) pins this PID and therefore PGID.
    if anchor.returncode is not None:
        fail('STEP_GROUP', 'anchor identity already reaped')
    result = subprocess.run(['ps', '-axo', 'pid=,pgid=,stat='], capture_output=True,
                            text=True, timeout=5, env=dict(os.environ, LC_ALL='C'))
    if result.returncode or not result.stdout.strip():
        fail('STEP_GROUP', 'process inspection unavailable; retaining job lease')
    seen, live = False, False
    for line in result.stdout.splitlines():
        fields = line.split()
        if len(fields) != 3 or not all(x.isdigit() for x in fields[:2]):
            fail('STEP_GROUP', 'malformed process inspection; retaining job lease')
        pid, pgid = map(int, fields[:2])
        if pgid == anchor.pid:
            if pid == anchor.pid:
                seen = allow_dead or not fields[2].startswith('Z')
            elif not fields[2].startswith('Z'):
                live = True
    if not seen:
        fail('STEP_GROUP', 'anchor identity unproven; retaining job lease')
    return not live


def drain(anchor):
    group_drained(anchor, allow_dead=True)  # Establish pinned group before signalling.
    for sig, grace in ((signal.SIGTERM, TERM_GRACE), (signal.SIGKILL, KILL_GRACE)):
        os.killpg(anchor.pid, sig)
        deadline = time.monotonic() + grace
        while True:
            if group_drained(anchor, allow_dead=True):
                return
            if time.monotonic() >= deadline:
                break
            time.sleep(.05)
    fail('STEP_GROUP', 'owned group did not drain; retaining job lease')


def anchor_main(fd, argv):
    # A caught (not SIG_IGN) disposition resets to default in the exec'd command.
    # The anchor survives TERM so it can pin the PGID during bounded cleanup.
    for sig in SIGNALS:
        signal.signal(sig, lambda *_: None)
    try:
        code = subprocess.call(argv, stdin=subprocess.DEVNULL, close_fds=True)
    except OSError:
        code = 70
    os.write(fd, (str(code) + '\n').encode())
    os.close(fd)
    sys.stdin.buffer.read(1)
    return code if code >= 0 else 128 - code


def release(budget):
    lease = snapshot(budget.expected)
    value = {'protocol': 1, 'identity': budget.expected, 'status': 'initializing',
             'queue_seconds': lease['queue_seconds'],
             'execution_seconds': time.monotonic() - lease['acquired_monotonic']}
    budget.save(value, initial=True)  # Refuse duplicate initialization before releasing.
    hook('end')
    value['execution_seconds'] = time.monotonic() - lease['acquired_monotonic']
    value['status'] = 'ready'
    budget.save(value)
    check_remaining(budget, value)
    report('initialized', value)
    return 0


def check_remaining(budget, value):
    if value['queue_seconds'] >= QUEUE_BUDGET:
        value['status'] = 'queue_timeout'
        budget.save(value)
        report('terminal', value)
        fail('QUEUE_TIMEOUT', 'infra_blocked: cumulative job queue budget exhausted', 75)
    if value['execution_seconds'] >= EXEC_BUDGET:
        value['status'] = 'execution_timeout'
        budget.save(value)
        report('terminal', value)
        fail('EXEC_TIMEOUT', 'cumulative admitted execution budget exhausted', 124)


def run(budget, argv):
    if not argv:
        fail('STEP_COMMAND', 'missing command')
    value = budget.load()
    check_remaining(budget, value)
    value['status'] = 'waiting'
    budget.save(value)
    lease, anchor, read_fd, write_fd = None, None, None, None
    released = False
    execution_before = value['execution_seconds']
    try:
        hook('start', '--queue-seconds', str(QUEUE_BUDGET - value['queue_seconds']))
        lease = snapshot(budget.expected)
        value['queue_seconds'] += lease['queue_seconds']
        value['status'] = 'running'
        budget.save(value)
        report('admitted', dict(value, execution_seconds=execution_before + time.monotonic() - lease['acquired_monotonic']))
        if value['queue_seconds'] >= QUEUE_BUDGET:
            fail('QUEUE_TIMEOUT', 'infra_blocked: cumulative job queue budget exhausted', 75)
        deadline = lease['acquired_monotonic'] + EXEC_BUDGET - value['execution_seconds']
        if time.monotonic() >= deadline:
            fail('EXEC_TIMEOUT', 'cumulative admitted execution budget exhausted', 124)
        read_fd, write_fd = os.pipe()
        anchor = subprocess.Popen([sys.executable, __file__, '_anchor', str(write_fd), *argv],
                                  stdin=subprocess.PIPE, pass_fds=(write_fd,),
                                  start_new_session=True, close_fds=True)
        os.close(write_fd)
        write_fd = None
        data = b''
        while b'\n' not in data:
            remaining = deadline - time.monotonic()
            if remaining <= 0:
                fail('EXEC_TIMEOUT', 'cumulative admitted execution budget exhausted', 124)
            ready, _, _ = select.select([read_fd], [], [], min(.1, remaining))
            if ready:
                part = os.read(read_fd, 32)
                if not part:
                    fail('STEP_COMMAND', 'command result unavailable')
                data += part
                if len(data) > 32:
                    fail('STEP_COMMAND', 'malformed command result')
        code = int(data.strip())
        if not group_drained(anchor):
            fail('STEP_GROUP', 'descendants remain; retaining job lease')
        hook('end')
        released = True
        value['execution_seconds'] = execution_before + time.monotonic() - lease['acquired_monotonic']
        value['status'] = 'ready'
        budget.save(value)
        try:
            check_remaining(budget, value)
        except GateError as exhausted:
            # A command that already failed before its deadline keeps that
            # result; the terminal ledger still prevents any later admission.
            if code == 0 or exhausted.code not in (75, 124):
                raise
        report('completed', value)
        return code if code >= 0 else 128 - code
    except BaseException as error:
        # A second cancellation must not interrupt cleanup of our own children.
        handlers = {sig: signal.signal(sig, signal.SIG_IGN) for sig in SIGNALS}
        try:
            code = error.code if isinstance(error, GateError) else 70
            value['status'] = {75: 'queue_timeout', 124: 'execution_timeout'}.get(code, 'interrupted')
            if anchor is not None:
                drain(anchor)
            # Timeouts release only after proven drain; other uncertain failures
            # retain their lease for runner cleanup, even if our children drained.
            if not released and ((lease is not None and code in (75, 124)) or lease is None):
                hook('end')
                released = True
            if code == 75:
                value['queue_seconds'] = max(QUEUE_BUDGET, value['queue_seconds'])
            if lease is not None:
                value['execution_seconds'] = execution_before + time.monotonic() - lease['acquired_monotonic']
            budget.save(value)
        except (GateError, OSError, subprocess.SubprocessError) as cleanup_error:
            print(f'GATE_E_STEP_CLEANUP: {cleanup_error}; retaining unfinished budget/lease', file=sys.stderr)
        finally:
            report('terminal', value)
            for sig, handler in handlers.items():
                signal.signal(sig, handler)
        raise
    finally:
        for fd in (read_fd, write_fd):
            if fd is not None:
                os.close(fd)
        if anchor is not None:
            anchor.stdin.close()
            # On an unproven drain leave the anchor unreaped/alive as necessary;
            # the existing holder retains build custody until runner cleanup.
            try:
                anchor.wait(timeout=2)
            except subprocess.TimeoutExpired:
                pass


def main():
    args = sys.argv[1:]
    if args[:1] == ['_anchor']:
        return anchor_main(int(args[1]), args[2:])
    def interrupted(*_):
        fail('STEP_INTERRUPTED', 'step cancelled; future phases refuse unfinished budget')
    for sig in SIGNALS:
        signal.signal(sig, interrupted)
    installed()
    budget = Budget(identity())
    try:
        if args == ['release']:
            return release(budget)
        if args[:2] != ['run', '--']:
            fail('STEP_COMMAND', 'expected release or run -- COMMAND')
        return run(budget, args[2:])
    finally:
        budget.close()


if __name__ == '__main__':
    try:
        sys.exit(main())
    except (GateError, OSError, ValueError, subprocess.SubprocessError) as error:
        print(str(error) if isinstance(error, GateError) else 'GATE_E_STEP_RUNTIME', file=sys.stderr)
        sys.exit(error.code if isinstance(error, GateError) else 70)
