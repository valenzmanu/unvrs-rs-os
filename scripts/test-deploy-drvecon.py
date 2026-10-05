#!/usr/bin/env python3
"""Check wrapper control flow without invoking a real deployment or kernel.

usage: test-deploy-drvecon.py [wrapper]   (default deploy-drvecon.sh)"""
import json
import os
from pathlib import Path
import subprocess
import shutil
import sys
import tempfile

name = sys.argv[1] if len(sys.argv) > 1 else 'deploy-drvecon.sh'

source = Path(__file__).resolve().parent.parent
with tempfile.TemporaryDirectory(prefix='drvecon-deploy-') as directory:
    # resolved: the wrapper names its repo with pwd -P (macOS /var is /private/var)
    home = Path(directory).resolve()
    repo = home / 'checkout'
    subprocess.run(['git', 'clone', '--shared', '--no-checkout', str(source), str(repo)],
                   check=True, capture_output=True)
    script = repo / 'scripts' / name
    script.parent.mkdir()
    shutil.copy2(source / 'scripts' / name, script)
    if name != 'deploy-drvecon.sh':
        shutil.copy2(source / 'scripts/deploy-drvecon.sh', script.parent / 'deploy-drvecon.sh')
    commit = subprocess.check_output(['git', '-C', str(repo), 'rev-parse', 'HEAD'], text=True).strip()
    binary = home / 'bin/unvrs'
    binary.parent.mkdir()
    fake_client = '''#!/usr/bin/env python3
import json, os, sys
from pathlib import Path
log = Path(os.environ['UNVRS_HOME']) / 'calls.jsonl'
calls = [json.loads(line) for line in log.read_text().splitlines()] if log.exists() else []
with log.open('a') as file:
    file.write(json.dumps({'client': Path(sys.argv[0]).parent.name, 'args': sys.argv[1:]}) + '\\n')
mode = os.environ['DEPLOY_CHECK_MODE']
verb = sys.argv[1]
if verb == 'deploy' and Path(sys.argv[0]).parent.name == 'bin': sys.exit('installed client has no deploy subcommand')
if verb == 'deploy' and mode == 'deploy_failure': sys.exit(1)
if verb == 'rollback' and mode == 'rollback_failure': sys.exit(1)
if verb == 'doctor':
    first = not any(call['args'][0] == 'doctor' for call in calls)
    if mode == 'restored_doctor_failure' or (first and mode != 'success'): sys.exit(1)
'''
    builders = [repo / 'target/debug/unvrs', repo / 'target/release/unvrs']
    for client in [binary, *builders]:
        client.parent.mkdir(parents=True, exist_ok=True)
        client.write_text(fake_client)
        client.chmod(0o755)
    log = home / 'calls.jsonl'
    for builder in builders:
        for mode, expected in [
            ('success', ['deploy', 'doctor']),
            ('deploy_failure', ['deploy']),
            ('doctor_failure', ['deploy', 'doctor', 'rollback', 'doctor']),
            ('rollback_failure', ['deploy', 'doctor', 'rollback']),
            ('restored_doctor_failure', ['deploy', 'doctor', 'rollback', 'doctor']),
        ]:
            log.unlink(missing_ok=True)
            env = {**os.environ, 'UNVRS_HOME': str(home), 'UNVRS_DRIVEN_PID': '', 'DEPLOY_CHECK_MODE': mode}
            result = subprocess.run([str(script), commit], env=env, capture_output=True, text=True)
            calls = [json.loads(line) for line in log.read_text().splitlines()]
            assert [call['args'][0] for call in calls] == expected, (mode, calls)
            assert calls[0] == {'client': builder.parent.name, 'args': [
                'deploy', commit, '--repo', str(repo), '--drain-secs', '900']
                + (['--force-requeue'] if name == 'deploy-crew.sh' else [])}
            assert all(call['client'] == 'bin' for call in calls[1:]), calls
            assert result.returncode == (0 if mode == 'success' else 1), (mode, result.stderr)
            print(f'{builder.parent.name} {mode}: passed')
        builder.unlink()
    for label, args, driven in [
        ('missing worker binary', [commit], ''),
        ('driven refusal', [commit], '157'),
        ('missing commit', [], ''),
        ('invalid commit', ['--not-a-commit'], ''),
    ]:
        log.unlink(missing_ok=True)
        env = {**os.environ, 'UNVRS_HOME': str(home), 'UNVRS_DRIVEN_PID': driven, 'DEPLOY_CHECK_MODE': 'success'}
        result = subprocess.run([str(script), *args], env=env, capture_output=True, text=True)
        assert result.returncode != 0 and not log.exists(), (label, result.stderr)
        print(f'{label}: passed')
