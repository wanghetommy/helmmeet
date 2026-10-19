"""Dependency-cache safety tests; never build, download or access audio devices."""
import hashlib
import importlib.util
from pathlib import Path
import tarfile
import tempfile
import unittest
from unittest.mock import patch
import io

spec = importlib.util.spec_from_file_location('helmmeet_apm_build', Path(__file__).resolve().parents[2] / 'native/audio-processing/build.py')
builder = importlib.util.module_from_spec(spec)
spec.loader.exec_module(builder)


class ApmCacheSafety(unittest.TestCase):
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
