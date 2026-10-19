"""Dependency-cache safety tests; never build, download or access audio devices."""
import hashlib
import importlib.util
from pathlib import Path
import tarfile
import tempfile
import unittest
from unittest.mock import patch
import io
import contextlib
import json

spec = importlib.util.spec_from_file_location('helmmeet_apm_build', Path(__file__).resolve().parents[2] / 'native/audio-processing/build.py')
builder = importlib.util.module_from_spec(spec)
spec.loader.exec_module(builder)


class ApmCacheSafety(unittest.TestCase):
    def test_cross_build_keeps_host_tools_and_target_libraries_separate(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            python = root / '.tools/python/bin/python3'
            python.parent.mkdir(parents=True); python.touch()
            calls = []
            lock = json.loads((builder.HERE / 'dependencies.lock.json').read_text())

            def run(args, **kwargs):
                calls.append(([str(a) for a in args], kwargs))
                if 'setup' in args:
                    Path(args[args.index('setup') + 1]).mkdir(parents=True, exist_ok=True)
                return json.dumps(lock['tools']) if '-c' in args else ''

            with patch.object(builder, 'ROOT', root), patch.object(builder, 'CACHE', root / 'cache'), \
                 patch.object(builder.sys, 'platform', 'darwin'), patch.object(builder, 'native_target', return_value='aarch64-apple-darwin'), \
                 patch.object(builder, 'prepare_source', side_effect=lambda item, _: root / item['directory']), \
                 patch.object(builder, 'run', side_effect=run), contextlib.redirect_stdout(io.StringIO()):
                for target in ('x86_64-apple-darwin', 'aarch64-apple-darwin'):
                    with patch.object(builder.sys, 'argv', ['build.py', '--target', target, '--upstream-only']):
                        builder.main()
            cmake = [a for a, _ in calls if '-S' in a]
            self.assertIn('-DCMAKE_OSX_ARCHITECTURES=x86_64', cmake[0])
            self.assertIn('-DCMAKE_OSX_ARCHITECTURES=arm64', cmake[1])
            self.assertNotEqual(next(a for a in cmake[0] if a.startswith('-DCMAKE_INSTALL_PREFIX=')),
                                next(a for a in cmake[1] if a.startswith('-DCMAKE_INSTALL_PREFIX=')))
            setups = [(a, kw['env']) for a, kw in calls if 'setup' in a]
            self.assertTrue(any('x86_64.ini' in a for a in setups[0][0]))
            for _, env in setups:
                self.assertEqual(env['PKG_CONFIG'], str(root / 'cache/host/arm64/bin/pkgconf'))
            configures = [(a, kw['env']) for a, kw in calls if a[0].endswith('/configure')]
            self.assertTrue(all('-arch arm64' in env['CFLAGS'] for _, env in configures))
            for target in lock['targets']:
                self.assertTrue((root / 'cache/targets' / target / 'build/apm/.helmmeet-config').is_file())

    def test_missing_offline_source_never_downloads(self):
        with tempfile.TemporaryDirectory() as directory, patch.object(builder, 'CACHE', Path(directory)), patch.object(builder, 'run') as run:
            item = dict(archive='fixture.tar.gz', directory='fixture', sha256='0'*64)
            with self.assertRaisesRegex(RuntimeError, 'rerun with --download'):
                builder.prepare_source(item, False)
            run.assert_not_called()

    def test_corrupted_archive_is_rejected_before_extraction(self):
        with tempfile.TemporaryDirectory() as directory, patch.object(builder, 'CACHE', Path(directory)):
            archive = Path(directory) / 'downloads/fixture.tar.gz'
            archive.parent.mkdir(); archive.write_bytes(b'corrupt')
            with self.assertRaisesRegex(RuntimeError, 'Checksum mismatch'):
                builder.prepare_source(dict(archive=archive.name, directory='fixture', sha256='0'*64), False)
            self.assertFalse((Path(directory) / 'src').exists())

    def test_archive_cannot_escape_dependency_root(self):
        with tempfile.TemporaryDirectory() as directory, patch.object(builder, 'CACHE', Path(directory)):
            archive = Path(directory) / 'downloads/fixture.tar.gz'; archive.parent.mkdir()
            with tarfile.open(archive, 'w:gz') as package:
                info = tarfile.TarInfo('fixture/../../escape');info.size = 4
                package.addfile(info, io.BytesIO(b'data'))
            item = dict(archive=archive.name, directory='fixture', sha256=hashlib.sha256(archive.read_bytes()).hexdigest())
            with self.assertRaises(tarfile.FilterError):
                builder.prepare_source(item, False)
            self.assertFalse((Path(directory) / 'escape').exists())

    def test_dependency_paths_cannot_select_user_directories(self):
        with self.assertRaisesRegex(RuntimeError, 'Invalid dependency cache path'):
            builder.prepare_source(dict(archive='fixture.tar.gz', directory='../user-data'), False)
