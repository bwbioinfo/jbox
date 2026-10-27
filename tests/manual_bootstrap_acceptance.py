#!/usr/bin/env python3
"""Accept the production bootstrap policy using original four-repository snapshots."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import re
import shlex
import subprocess
import tempfile
import time

REPO = Path(__file__).resolve().parents[1]
ROOT = HOME = DATA = CLI = None
SOURCE_ROOT = None
env = None
log = None
SPECS = {
    'mpgi-iac': ('c025ad0c2320270e90305b46b9c37dd279af1982', '/workspace/iac'),
    'nanite': ('e9af9ccdccdf603e214e47713c7e455d9a2d183f', '/workspace/nanite'),
    'heliXplore': ('f25bb62584899bed85c2bac68cceb60e655bc56c', '/workspace/heliXplore'),
    'porectl': ('a438f5dafc473cf0512dde091d17d3aaa7a503b1', '/workspace/porectl'),
}
owned = set()
expected = {}
original = {}

def note(text):
    line = f'{time.strftime("%Y-%m-%dT%H:%M:%S%z")} {text}'
    print(line, flush=True)
    print(line, file=log, flush=True)

def run(args, cwd=None, expected_code=0, input=None, timeout=900):
    note('RUN ' + ' '.join(map(str, args)))
    start = time.monotonic()
    result = subprocess.run(list(map(str, args)), cwd=cwd, env=env, input=input, text=True,
                            stdout=subprocess.PIPE, stderr=subprocess.STDOUT, timeout=timeout)
    print(result.stdout, end='', flush=True)
    print(result.stdout, end='', file=log, flush=True)
    note(f'EXIT {result.returncode} elapsed={time.monotonic() - start:.3f}s')
    owned.update(re.findall(r'sandbox branch jbox/([a-z]+-[a-z]+-[0-9a-f]+)/', result.stdout))
    if expected_code is not None:
        assert result.returncode == expected_code, result.stdout
    return result.stdout

def cli(*args, **kw):
    return run([CLI, *args], cwd=ROOT / 'mpgi-iac', **kw)

def states():
    return list((DATA / 'jbox/sessions').glob('*/session.json'))

def assert_cleanup():
    assert not states(), states()
    for session in owned:
        assert not run(['docker', 'ps', '-a', '--filter', f'name=^/jbox-{session}$', '--format', '{{.Names}}']).strip()
    for name in SPECS:
        listing = run(['git', '-C', ROOT / name, 'worktree', 'list', '--porcelain'])
        assert listing.count('worktree ') == 1, listing

def verify_export(session, label):
    commands = ['set -eu', 'test ! -e /var/run/docker.sock', 'test "$(id -u)" = ' + str(os.getuid())]
    export_code = 'import json,hashlib; rows=[json.loads(line) for line in open("/tmp/bootstrap-acceptance.jsonl") if line.strip()]; ids=sorted(row["id"] for row in rows if "id" in row); print("IDS="+str(len(ids))+":"+hashlib.sha256("\\n".join(ids).encode()).hexdigest())'
    for name, (_, mount) in SPECS.items():
        commands.extend([f'cd {mount}', f'printf "WORKSPACE={name} "; pwd'])
        if name in expected:
            commands.extend([
                'bd export --include-memories > /tmp/bootstrap-acceptance.jsonl',
                'python3 -c ' + shlex.quote(export_code),
            ])
    commands.extend(['printf "PUBLIC_SHELL_PASS\\n"', 'exit'])
    output = cli('shell', session, input='\n'.join(commands)+'\n', timeout=240)
    assert 'PUBLIC_SHELL_PASS' in output
    for name, (_, mount) in SPECS.items():
        assert f'WORKSPACE={name} {mount}' in output
    for name, ids in expected.items():
        digest = hashlib.sha256('\n'.join(sorted(ids)).encode()).hexdigest()
        assert f'IDS={len(ids)}:{digest}' in output, (name, len(ids), digest)
        note(f'PASS {label} {name}: exact count={len(ids)} ID_SHA256={digest}')

def config(timeout=None):
    text = '''version=1
[resources]
cpus=2
memory="4G"
disk="10G"
ttl="1h"
[workspace]
mount="/workspace/iac"
[network]
internet=true
host=false
lan=false
[jcode]
persistent_credentials=false
skills=[]
[git]
network=false
credentials="none"
[git.author]
inherit_host=false
name="Bootstrap acceptance"
email="acceptance@example.invalid"
'''
    if timeout is not None:
        text += f'\n[beads]\nbootstrap_timeout_seconds={timeout}\n'
    for name, (_, mount) in SPECS.items():
        if name != 'mpgi-iac':
            text += f'\n[[repos]]\npath="../{name}"\nmount="{mount}"\n'
    (ROOT / 'mpgi-iac/.jbox.toml').write_text(text)
    run(['git', '-C', ROOT / 'mpgi-iac', 'add', '.jbox.toml'])
    run(['git', '-c', 'core.hooksPath=/dev/null', '-c', 'user.name=Acceptance', '-c', 'user.email=acceptance@example.invalid', '-C', ROOT / 'mpgi-iac', 'commit', '-m', f'Isolate production bootstrap acceptance policy={timeout or "default"}'])

def workflow():
    for name, (commit, _) in SPECS.items():
        source = SOURCE_ROOT / name
        run(['git', '-c', 'core.hooksPath=/dev/null', 'clone', '--no-local', '--no-checkout', source, ROOT / name])
        run(['git', '-c', 'core.hooksPath=/dev/null', '-C', ROOT / name, 'checkout', '--detach', commit])
        snapshot = ROOT / name / '.beads/issues.jsonl'
        if snapshot.is_file():
            expected[name] = {row['id'] for row in map(json.loads, snapshot.read_text().splitlines()) if 'id' in row}
            note(f'INPUT {name} commit={commit} records={len(expected[name])} bytes={snapshot.stat().st_size}')
    assert {name: len(ids) for name, ids in expected.items()} == {'mpgi-iac': 274, 'nanite': 544, 'heliXplore': 96}
    for invalid in (0, 3601):
        config(invalid)
        rejected = cli('run', '--new', '--no-attach', expected_code=1, timeout=30)
        assert 'beads.bootstrap_timeout_seconds' in rejected and '1 and 3600' in rejected
        assert 'Starting Kata guest' not in rejected
        assert_cleanup()
        note(f'PASS invalid public timeout policy={invalid} rejected before guest startup')
    # A deliberately too-small, but valid policy must fail closed through the real public path.
    config(1)
    negative = cli('run', '--new', '--no-attach', expected_code=1, timeout=1200)
    assert 'Beads bootstrap failed' in negative and 'budget=1s' in negative and 'phase=init' in negative
    assert 'status=124' in negative or 'status=137' in negative
    assert_cleanup()
    note('PASS configured 1s bound fails before SSH, reports actual phase, and cleans all owned startup resources')
    config()
    cli('run', '--new', '--no-attach', timeout=1200)
    assert len(states()) == 1
    state = json.loads(states()[0].read_text())
    session = state['id']
    owned.add(session)
    inspected = json.loads(run(['docker', 'inspect', state['container_name']]))[0]
    assert inspected['HostConfig']['Runtime'] == 'kata'
    assert inspected['HostConfig']['ReadonlyRootfs'] is True
    assert inspected['HostConfig']['Privileged'] is False
    assert all(Path(mount['Source']).resolve().is_relative_to(ROOT.resolve()) for mount in inspected['Mounts'])
    environment = dict(item.split('=', 1) for item in inspected['Config']['Env'] if '=' in item)
    assert environment['JBOX_BEADS_BOOTSTRAP_TIMEOUT_SECONDS'] == '180'
    assert environment['JBOX_BEADS_WORKSPACE_COUNT'] == str(len(expected))
    logs = run(['docker', 'logs', '--timestamps', state['container_name']])
    for name in expected:
        mount = SPECS[name][1]
        assert f'workspace={mount} phase=bootstrap END' in logs
    assert 'budget=180s' in logs
    verify_export(session, 'fresh')
    cli('status', session)
    cli('diff', session)
    initial_container = inspected['Id']
    cli('run', '--no-attach')
    assert json.loads(run(['docker', 'inspect', state['container_name']]))[0]['Id'] == initial_container
    cli('stop', session)
    assert json.loads(states()[0].read_text())['state'].lower() == 'stopped'
    cli('run', '--no-attach', timeout=1000)
    assert json.loads(states()[0].read_text())['id'] == session
    verify_export(session, 'retained')
    run(['docker', 'logs', '--timestamps', state['container_name']])
    cli('stop', session)
    cli('clean', session)
    assert_cleanup()
    note('Public workflow checks passed')


PORTABLE_FILES = ('issues.jsonl', 'metadata.json', 'config.yaml')


def source_manifest(source):
    def git(*args):
        return subprocess.check_output(['git', '-C', str(source), *args], text=True)
    files = {}
    for name in PORTABLE_FILES:
        path = source / '.beads' / name
        files[name] = hashlib.sha256(path.read_bytes()).hexdigest() if path.exists() else None
    return {'head': git('rev-parse', 'HEAD').strip(),
            'status': git('status', '--porcelain'), 'files': files}


def capture_manifests(source_root):
    manifests, errors = {}, []
    for name in SPECS:
        try:
            manifests[name] = source_manifest(source_root / name)
        except Exception as exc:
            errors.append(f'{name}: manifest capture failed: {type(exc).__name__}: {exc}')
    return manifests, errors


def compare_manifests(before, after):
    errors = []
    for name in SPECS:
        if name not in before or name not in after:
            errors.append(f'{name}: missing before or after manifest')
            continue
        for field in ('head', 'status'):
            if before[name][field] != after[name][field]:
                errors.append(f'{name}: {field} changed: {before[name][field]!r} -> {after[name][field]!r}')
        for filename in PORTABLE_FILES:
            if before[name]['files'][filename] != after[name]['files'][filename]:
                errors.append(f'{name}: .beads/{filename} changed: {before[name]["files"][filename]!r} -> {after[name]["files"][filename]!r}')
    return errors


def make_result(workflow_error, guard_errors, cleanup_errors):
    return {'workflow_pass': workflow_error is None, 'workflow_error': workflow_error,
            'guard_errors': guard_errors, 'cleanup_errors': cleanup_errors,
            'overall_pass': workflow_error is None and not guard_errors and not cleanup_errors}


def main(argv=None):
    global ROOT, HOME, DATA, CLI, SOURCE_ROOT, env, log
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--source-root', type=Path, required=True)
    parser.add_argument('--scratch-root', type=Path, required=True)
    parser.add_argument('--cli', type=Path, default=REPO / 'target/release/jbox')
    args = parser.parse_args(argv)
    SOURCE_ROOT, scratch, CLI = args.source_root.resolve(), args.scratch_root.resolve(), args.cli.resolve()
    # A scratch directory anywhere inside a live repository contaminates status.
    for source in [REPO, *(SOURCE_ROOT / name for name in SPECS)]:
        if scratch.is_relative_to(source.resolve()):
            parser.error(f'scratch root must be outside repository {source}')
    scratch.mkdir(parents=True, exist_ok=True)
    ROOT = Path(tempfile.mkdtemp(prefix='bp.', dir=scratch)).resolve()
    HOME, DATA = ROOT / 'h', ROOT / 'd'
    HOME.mkdir()
    DATA.mkdir()
    env = os.environ.copy()
    env.update(HOME=str(HOME), XDG_DATA_HOME=str(DATA), XDG_CACHE_HOME=str(ROOT / 'c'),
               XDG_CONFIG_HOME=str(ROOT / 'x'), GIT_CONFIG_NOSYSTEM='1', GIT_CONFIG_GLOBAL='/dev/null')
    for key in ('SSH_AUTH_SOCK', 'SSH_AGENT_PID', 'JCODE_HOME', 'GITHUB_TOKEN', 'GH_TOKEN', 'OPENAI_API_KEY', 'ANTHROPIC_API_KEY'):
        env.pop(key, None)
    log = (ROOT / 'acceptance.log').open('w', buffering=1)
    note(f'ROOT={ROOT}; actual production CLI={CLI}; no wrappers or live host DB mounts')
    before, guard_errors = capture_manifests(SOURCE_ROOT)
    (ROOT / 'source_manifest_before.json').write_text(json.dumps(before, indent=2) + '\n')
    workflow_error = None
    cleanup_errors = []
    try:
        if guard_errors:
            raise RuntimeError('Initial source manifest capture failed')
        workflow()
    except Exception as exc:
        workflow_error = f'{type(exc).__name__}: {exc}'
        note(f'WORKFLOW FAILED: {workflow_error}')
    finally:
        for path in states():
            try:
                session = json.loads(path.read_text())['id']
                cli('stop', session, expected_code=None)
                cli('clean', session, expected_code=0)
            except Exception as exc:
                cleanup_errors.append(f'{path}: {type(exc).__name__}: {exc}')
        try:
            assert_cleanup()
        except Exception as exc:
            cleanup_errors.append(f'cleanup verification: {type(exc).__name__}: {exc}')
        after, after_errors = capture_manifests(SOURCE_ROOT)
        guard_errors.extend(after_errors)
        guard_errors.extend(compare_manifests(before, after))
        (ROOT / 'source_manifest_after.json').write_text(json.dumps(after, indent=2) + '\n')
        result = make_result(workflow_error, guard_errors, cleanup_errors)
        (ROOT / 'overall_result.json').write_text(json.dumps(result, indent=2) + '\n')
        for error in guard_errors + cleanup_errors:
            note(f'GUARD/CLEANUP FAILED: {error}')
        if result['overall_pass']:
            note('OVERALL_ACCEPTANCE_PASS')
        log.close()
    return 0 if result['overall_pass'] else 1


if __name__ == '__main__':
    raise SystemExit(main())
