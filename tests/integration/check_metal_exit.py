"""Opt-in packaged AppKit/Metal teardown regression; no recording or model HTTP."""
import argparse
import os
from pathlib import Path
import sqlite3
import subprocess
import tempfile
import time


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--model', required=True, type=Path)
    parser.add_argument('--app', type=Path, default=Path('target/release/bundle/macos/HelmMeet.app'))
    args = parser.parse_args()
    binary = args.app.resolve() / 'Contents/MacOS/helmmeet-desktop'
    model = args.model.resolve(strict=True)
    if not binary.is_file():
        parser.error('Build the app before running this check')
    with tempfile.TemporaryDirectory(prefix='helmmeet-metal-exit-') as test_dir:
        for phase in ('idle', 'busy', 'loading'):
            root = Path(test_dir) / phase
            env = os.environ.copy()
            for key in ('HELMMEET_DEV_CONFIG', 'HELMMEET_TEST_LLM',
                        'GGML_BACKTRACE_LLDB', 'GGML_METAL_NO_RESIDENCY'):
                env.pop(key, None)
            env.update(HELMMEET_DATA_ROOT=str(root), HELMMEET_TEST_MODEL=str(model),
                       HELMMEET_EXIT_PHASE=phase)
            began = time.monotonic()
            process = subprocess.Popen([str(binary), '--check-asr-exit'], env=env,
                                       stdout=subprocess.PIPE, stderr=subprocess.STDOUT, text=True)
            try:
                output, _ = process.communicate(timeout=120)
            except subprocess.TimeoutExpired:
                # Only this isolated synthetic child; never target an existing app.
                process.kill()
                output, _ = process.communicate()
                raise AssertionError(f'{phase}: native Quit timed out; {output[-1500:]}')
            assert process.returncode == 0, f'{phase}: exit={process.returncode}; {output[-1500:]}'
            assert f'ASR exit probe: {phase};' in output, f'{phase}: target state not reached'
            assert 'workers joined before final exit' in output, f'{phase}: native Quit skipped cleanup'
            assert 'GGML_ASSERT' not in output and 'backtrace' not in output.lower(), output[-1500:]
            with sqlite3.connect(f'file:{root / "app.sqlite"}?mode=ro', uri=True) as db:
                covered = db.execute('SELECT count(*) FROM asr_chunks').fetchone()[0]
                assert covered == int(phase == 'idle'), f'{phase}: incorrect coverage after cancellation'
                assert db.execute("SELECT count(*) FROM audio_chunks WHERE status='sealed'").fetchone()[0] == 1
                assert db.execute('SELECT count(*) FROM analysis_jobs').fetchone()[0] == 0
            print(f'{phase}: repeated AppKit Quit -> joined workers -> exit 0; '
                  f'coverage={covered}, sealed synthetic audio preserved; '
                  f'{time.monotonic() - began:.2f}s', flush=True)


if __name__ == '__main__':
    main()
