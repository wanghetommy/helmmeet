#!/usr/bin/env python3
"""Build AP02 in .tools only. Network is opt-in; verify archives before extraction."""
import argparse
import fcntl
import hashlib
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tarfile

ROOT = Path(__file__).resolve().parents[2]
HERE = Path(__file__).resolve().parent
CACHE = ROOT / '.tools/audio-processing'
PREFIX = CACHE / 'prefix'


def run(args, *, cwd=ROOT, env=None):
    result = subprocess.run([str(x) for x in args], cwd=cwd, env=env,
                            stdout=subprocess.PIPE, stderr=subprocess.STDOUT, text=True)
    if result.returncode:
        raise RuntimeError(f'Command failed: {args[0]}\n{result.stdout[-7000:]}')
    return result.stdout


def digest(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


def prepare_source(item, download):
    if any(Path(item[key]).name != item[key] or item[key] in ('.', '..') for key in ('archive', 'directory')):
        raise RuntimeError('Invalid dependency cache path')
    archive = CACHE / 'downloads' / item['archive']
    archive.parent.mkdir(parents=True, exist_ok=True)
    if not archive.exists():
        if not download:
            raise RuntimeError(f'Missing {archive.name}; rerun with --download to populate verified cache')
        temporary = archive.with_suffix('.download')
        run(['/usr/bin/curl', '--fail', '--location', '--proto', '=https', '--tlsv1.2',
             '--connect-timeout', '20', '--max-time', '180', '--retry', '2', '-o', temporary, item['url']])
        if digest(temporary) != item['sha256']:
            temporary.unlink()
            raise RuntimeError(f'Checksum mismatch: {archive.name}')
        temporary.rename(archive)
    if digest(archive) != item['sha256']:
        raise RuntimeError(f'Checksum mismatch: {archive.name}')
    source = CACHE / 'src' / item['directory']
    stamp = source / '.helmmeet-archive-sha256'
    if not stamp.exists() or stamp.read_text().strip() != item['sha256']:
        if source.exists():
            # Only our generated, dependency-specific cache directory.
            shutil.rmtree(source)
        source.parent.mkdir(parents=True, exist_ok=True)
        with tarfile.open(archive) as package:
            for member in package.getmembers():
                parts = Path(member.name).parts
                if not parts or parts[0] != item['directory']:
                    raise RuntimeError('Archive has an unexpected root')
            package.extractall(source.parent, filter='data')
        stamp.write_text(item['sha256'] + '\n')
    return source


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--download', action='store_true')
    parser.add_argument('--jobs', type=int, default=6)
    parser.add_argument('--upstream-only', action='store_true')
    args = parser.parse_args()
    if sys.platform != 'darwin' or not 1 <= args.jobs <= 32:
        parser.error('This locked build targets macOS arm64; jobs must be 1..32')
    lock = json.loads((HERE / 'dependencies.lock.json').read_text())
    python = ROOT / '.tools/python/bin/python3'
    if not python.exists():
        raise RuntimeError('Run npm run bootstrap first (project-local Python/toolchain)')
    versions = json.loads(run([python, '-c', 'import importlib.metadata as m,json; print(json.dumps({x:m.version(x) for x in ["meson","ninja","cmake"]}))']))
    if versions != lock['tools']:
        raise RuntimeError('Install pinned tools in .tools/python using build-requirements.txt')
    sources = {item['name']: prepare_source(item, args.download) for item in lock['sources']}
    env = os.environ.copy()
    env.update(PATH=f'{PREFIX}/bin:{ROOT}/.tools/python/bin:' + env['PATH'],
               MACOSX_DEPLOYMENT_TARGET='14.0', PKG_CONFIG=str(PREFIX / 'bin/pkgconf'),
               PKG_CONFIG_LIBDIR=str(PREFIX / 'lib/pkgconfig'),
               PKG_CONFIG_ALLOW_SYSTEM_CFLAGS='1', PKG_CONFIG_ALLOW_SYSTEM_LIBS='1')
    build = CACHE / 'build'
    build.mkdir(exist_ok=True)
    if not (PREFIX / 'bin/pkgconf').exists():
        directory = build / 'pkgconf'
        directory.mkdir(exist_ok=True)
        tool_env = env | {'CFLAGS': '-O2 -arch arm64 -mmacosx-version-min=14.0'}
        run([sources['pkgconf'] / 'configure', f'--prefix={PREFIX}', '--disable-shared', '--enable-static'], cwd=directory, env=tool_env)
        run(['make', f'-j{args.jobs}'], cwd=directory, env=tool_env)
        run(['make', 'install'], cwd=directory, env=tool_env)
    cmake = ROOT / '.tools/python/bin/cmake'
    run([cmake, '-S', sources['abseil-cpp'], '-B', build / 'abseil', '-G', 'Ninja',
         f'-DCMAKE_INSTALL_PREFIX={PREFIX}', '-DCMAKE_BUILD_TYPE=Release', '-DCMAKE_CXX_STANDARD=17',
         '-DCMAKE_OSX_ARCHITECTURES=arm64', '-DCMAKE_OSX_DEPLOYMENT_TARGET=14.0',
         '-DCMAKE_POSITION_INDEPENDENT_CODE=ON', '-DABSL_BUILD_TESTING=OFF', '-DABSL_ENABLE_INSTALL=ON',
         '-DABSL_PROPAGATE_CXX_STD=ON', '-DBUILD_SHARED_LIBS=OFF'], env=env)
    run([cmake, '--build', build / 'abseil', '--parallel', str(args.jobs)], env=env)
    run([cmake, '--install', build / 'abseil'], env=env)
    meson = ROOT / '.tools/python/bin/meson'
    directory = build / 'apm-arm64'
    signature = digest(HERE / 'build-support/arm64.ini') + digest(HERE / 'dependencies.lock.json')
    stamp = directory / '.helmmeet-config'
    configured = (directory / 'build.ninja').exists()
    setup = (['--reconfigure'] if stamp.exists() and stamp.read_text() == signature else ['--wipe']) if configured else []
    run([meson, 'setup', *setup, directory, sources['webrtc-audio-processing'],
         f'--cross-file={HERE}/build-support/arm64.ini', f'--prefix={PREFIX}', '--libdir=lib',
         '--buildtype=release', '--default-library=static', '--wrap-mode=nofallback',
         '-Db_staticpic=true', '-Db_ndebug=true'], env=env)
    stamp.write_text(signature)
    run([meson, 'compile', '-C', directory, '-j', str(args.jobs)], env=env)
    if args.upstream_only:
        print('Locked upstream static build complete')
        return
    output = CACHE / 'lib'
    output.mkdir(exist_ok=True)
    wrapper = output / 'helmmeet_apm.o'
    flags = ['-arch', 'arm64', '-mmacosx-version-min=14.0', '-std=c++17', '-O3', '-DNDEBUG',
             '-fvisibility=hidden', '-DWEBRTC_MAC', '-DWEBRTC_POSIX',
             f'-I{HERE}/include', f'-I{sources["webrtc-audio-processing"]}/webrtc', f'-I{PREFIX}/include']
    run(['xcrun', 'clang++', *flags, '-c', HERE / 'src/helmmeet_apm.cpp', '-o', wrapper], env=env)
    archives = sorted(directory.rglob('*.a')) + sorted((PREFIX / 'lib').glob('libabsl_*.a'))
    bundle = output / 'libhelmmeet_apm.a'
    pending_bundle = output / 'libhelmmeet_apm.pending.a'
    run(['xcrun', 'libtool', '-static', '-o', pending_bundle, wrapper, *archives], env=env)
    pending_bundle.replace(bundle)
    probe = CACHE / 'bin/apm-probe'
    probe.parent.mkdir(exist_ok=True)
    pending_probe = probe.with_suffix('.pending')
    run(['xcrun', 'clang++', *flags, '-Wall', '-Wextra', '-Werror', HERE / 'tests/probe.cpp', bundle,
         '-framework', 'Foundation', '-framework', 'CoreFoundation', '-Wl,-dead_strip', '-o', pending_probe], env=env)
    pending_probe.replace(probe)
    # Required redistribution notices are in source control and in feature builds.
    sizes = {'target': lock['target'], 'apm_archives_bytes': sum(p.stat().st_size for p in directory.rglob('*.a')),
             'abseil_archives_bytes': sum(p.stat().st_size for p in (PREFIX / 'lib').glob('libabsl_*.a')),
             'bundle_bytes': bundle.stat().st_size, 'probe_bytes': probe.stat().st_size,
             'bundle_sha256': digest(bundle), 'dependencies_lock_sha256': digest(HERE / 'dependencies.lock.json')}
    (CACHE / 'build-manifest.json').write_text(json.dumps(sizes, indent=2) + '\n')
    print(json.dumps(sizes, indent=2))


if __name__ == '__main__':
    try:
        CACHE.mkdir(parents=True, exist_ok=True)
        with (CACHE / '.build.lock').open('a') as build_lock:
            fcntl.flock(build_lock, fcntl.LOCK_EX)
            main()
    except (RuntimeError, OSError, subprocess.SubprocessError) as error:
        raise SystemExit(str(error))
