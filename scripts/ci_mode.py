"""Reuse full CI checks only for a version-only release above a fully tested main commit."""
import argparse
import json
import os
import re
import subprocess


VERSION = re.compile(r'^version = "(\d+\.\d+\.\d+)"$', re.MULTILINE)
RELEASE_FILES = {'Cargo.toml', 'Cargo.lock', 'fuzz/Cargo.lock',
                 '.release-please-manifest.json', 'CHANGELOG.md'}
FULL_JOBS = {'dependencies', 'fuzz', 'build (macos-latest)',
             'build (ubuntu-latest)', 'cross', 'release-artifacts'}


def package_version(text):
    package = re.search(r'^\[package\]\n(.*?)(?=^\[|\Z)', text, re.MULTILINE | re.DOTALL)
    match = VERSION.search(package[1]) if package else None
    return match[1] if match else None


def normalize_manifest(text, version):
    package = re.search(r'^\[package\]\n(.*?)(?=^\[|\Z)', text, re.MULTILINE | re.DOTALL)
    if not package or not package_version(text) == version:
        raise ValueError('unexpected package version')
    start, end = package.span(1)
    return text[:start] + VERSION.sub('version = "<release>"', package[1], count=1) + text[end:]


def normalize_lock(text, version):
    blocks = text.split('[[package]]')
    found = 0
    for index, block in enumerate(blocks[1:], 1):
        if re.search(r'^name = "rivet"$', block, re.MULTILINE):
            match = VERSION.search(block)
            if not match or match[1] != version:
                raise ValueError('unexpected lockfile version')
            blocks[index] = VERSION.sub('version = "<release>"', block, count=1)
            found += 1
    if found != 1:
        raise ValueError('expected one local rivet package')
    return '[[package]]'.join(blocks)


def version_only(before, after):
    """Compare complete file contents, ignoring only explicitly permitted version fields."""
    changed = {path for path in before.keys() | after.keys() if before.get(path) != after.get(path)}
    if not {'Cargo.toml', 'Cargo.lock', '.release-please-manifest.json'} <= changed <= RELEASE_FILES:
        return False
    if any(path not in before or path not in after for path in changed):
        return False
    old, new = package_version(before['Cargo.toml']), package_version(after['Cargo.toml'])
    if not old or not new or tuple(map(int, new.split('.'))) <= tuple(map(int, old.split('.'))):
        return False
    try:
        if normalize_manifest(before['Cargo.toml'], old) != normalize_manifest(after['Cargo.toml'], new):
            return False
        for path in ('Cargo.lock', 'fuzz/Cargo.lock'):
            if path in changed and normalize_lock(before[path], old) != normalize_lock(after[path], new):
                return False
        previous = json.loads(before['.release-please-manifest.json'])
        current = json.loads(after['.release-please-manifest.json'])
        if previous.get('.') != old or current.get('.') != new:
            return False
        previous['.'] = current['.'] = '<release>'
        return previous == current
    except (ValueError, TypeError, AttributeError):
        return False


def git(*args):
    return subprocess.check_output(['git', *args]).decode()


def release_diff(base):
    if git('diff', '--summary', base, 'HEAD').strip():
        return False
    paths = set(git('diff', '--name-only', base, 'HEAD').splitlines())
    if not paths <= RELEASE_FILES:
        return False
    before, after = {}, {}
    for path in paths:
        before[path] = git('show', f'{base}:{path}')
        after[path] = git('show', f'HEAD:{path}')
    return version_only(before, after)


def full_run(run, jobs, sha):
    return (run.get('head_sha') == sha and run.get('head_branch') == 'main'
            and run.get('event') == 'push' and run.get('status') == 'completed'
            and run.get('conclusion') == 'success'
            and FULL_JOBS <= {job['name'] for job in jobs
                              if job.get('status') == 'completed' and job.get('conclusion') == 'success'})


def api(path):
    return json.loads(subprocess.check_output(['gh', 'api', path]))


def baseline_run(repo, sha):
    runs = api(f'repos/{repo}/actions/workflows/ci.yml/runs?head_sha={sha}&per_page=100')
    for run in runs['workflow_runs']:
        if (run.get('event') != 'push' or run.get('status') != 'completed'
                or run.get('conclusion') != 'success'):
            continue
        jobs = api(f'repos/{repo}/actions/runs/{run["id"]}/jobs?per_page=100')['jobs']
        if full_run(run, jobs, sha):
            return str(run['id'])
    return ''


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--base', required=True)
    parser.add_argument('--repo', required=True)
    parser.add_argument('--event', required=True)
    args = parser.parse_args()
    run_id = ''
    # Manual runs remain a way to force all checks.
    if args.event in ('push', 'pull_request'):
        try:
            if release_diff(args.base):
                run_id = baseline_run(args.repo, args.base)
        except (subprocess.CalledProcessError, OSError, ValueError, KeyError):
            print('Could not verify the baseline; running full CI.')
    release_only = bool(run_id)
    print(f'CI mode: {"release build and smoke checks" if release_only else "full checks"}')
    if release_only:
        print(f'Reusing full checks from main CI run {run_id} at {args.base}.')
    with open(os.environ['GITHUB_OUTPUT'], 'a') as output:
        output.write(f'release_only={str(release_only).lower()}\n')
        output.write(f'baseline_run_id={run_id}\n')
    with open(os.environ['GITHUB_STEP_SUMMARY'], 'a') as summary:
        summary.write(f'CI mode: **{"release build and smoke checks" if release_only else "full checks"}**.\n')
        if release_only:
            summary.write(f'Full checks: [{run_id}](https://github.com/{args.repo}/actions/runs/{run_id}), commit `{args.base}`. Only release version fields and metadata changed.\n')


if __name__ == '__main__':
    main()
