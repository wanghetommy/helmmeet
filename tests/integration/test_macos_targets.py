"""Build selection checks only: no compiler, downloads, app launch or user data."""
import os
from pathlib import Path
import shutil
import subprocess
import tempfile
import unittest
import json

ROOT = Path(__file__).resolve().parents[2]


class MacOSTargets(unittest.TestCase):
    def test_bundle_paths_are_target_specific_and_respect_output_root(self):
        script = ROOT / 'scripts/macos-target.sh'
        for target in ('', 'aarch64-apple-darwin', 'x86_64-apple-darwin'):
            for output in ('/private/tmp/build with spaces', 'relative output'):
                env = os.environ | {'TASK_ROOT': '/repo', 'TASK_TARGET': target, 'CARGO_TARGET_DIR': output}
                result = subprocess.run(['sh', '-c', '. "$1"; task_validate_target "$TASK_TARGET" && task_bundle_directory && printf "%s" "$TASK_BUNDLE"', 'test', str(script)], env=env, capture_output=True, text=True, check=True)
                base = Path(output) if output.startswith('/') else Path('/repo') / output
                self.assertEqual(result.stdout, str(base / target / 'release/bundle/macos/HelmMeet.app'))
        result = subprocess.run(['sh', '-c', '. "$1"; task_validate_target universal-apple-darwin', 'test', str(script)], capture_output=True)
        self.assertNotEqual(result.returncode, 0)

    def test_intel_policy_disables_host_cpu_extensions_and_metal(self):
        with tempfile.TemporaryDirectory() as directory:
            scripts = Path(directory) / 'scripts'; scripts.mkdir()
            wrapper = scripts / 'with-toolchain.sh'
            shutil.copyfile(ROOT / 'scripts/with-toolchain.sh', wrapper)
            env = {k: v for k, v in os.environ.items() if not k.startswith(('GGML_', 'CARGO_BUILD_TARGET', 'HELMMEET_BUILD_TARGET'))}
            env['HELMMEET_BUILD_TARGET'] = 'x86_64-apple-darwin'
            result = subprocess.run(['sh', str(wrapper), shutil.which('python3'), '-c', 'import os,json; print(json.dumps({k:v for k,v in os.environ.items() if k.startswith("GGML_")}))'], env=env, capture_output=True, text=True, check=True)
            flags = json.loads(result.stdout)
            for option in ('NATIVE', 'METAL', 'SSE42', 'AVX', 'AVX2', 'FMA', 'F16C', 'BMI2', 'AVX512', 'AMX_TILE'):
                self.assertEqual(flags['GGML_' + option], 'OFF')


if __name__ == '__main__':
    unittest.main()
