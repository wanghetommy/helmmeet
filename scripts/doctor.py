"""Read-only environment/model preflight. Does not grant capture or network access."""
import argparse
import hashlib
import json
import platform
import shutil
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument('--verify-model', action='store_true')
    args = parser.parse_args()
    print(f'Host: {platform.system()} {platform.machine()} {platform.mac_ver()[0]}')
    missing = []
    for tool in ('node', 'npm', 'python3', 'cargo', 'rustc', 'swift', 'cmake', 'xcode-select'):
        bundled = ROOT / ('.tools/python/bin' if tool == 'cmake' else '.tools/cargo/bin') / tool
        location = str(bundled) if tool in ('cargo', 'rustc', 'cmake') and bundled.is_file() else shutil.which(tool)
        print(f'{tool}: {location or "MISSING"}')
        if not location:
            missing.append(tool)
    config = ROOT / 'config/local.json'
    if not config.exists():
        print('Local model: NOT CONFIGURED (copy config/local.example.json)')
        print('Preflight incomplete; no capture/inference performed.')
        return 1
    try:
        settings = json.loads(config.read_text())['asr']
        model = Path(settings['model_path']).expanduser()
        if not model.is_absolute() or not model.is_file():
            raise ValueError('model_path must be an existing absolute file path')
        print(f'Local model: {model.name}, {model.stat().st_size:,} bytes')
        if args.verify_model:
            expected = settings.get('model_sha256')
            if not expected:
                raise ValueError('model_sha256 is not configured; cannot verify')
            digest = hashlib.sha256()
            with model.open('rb') as stream:
                for block in iter(lambda: stream.read(1024 * 1024), b''):
                    digest.update(block)
            actual = digest.hexdigest()
            print(f'SHA-256: {actual}')
            if actual != expected:
                raise ValueError('model SHA-256 does not match local configuration')
            print('Local fingerprint matches (not an independent provenance check).')
    except (OSError, ValueError, KeyError, TypeError) as error:
        print(f'Model check failed: {error}')
        return 1
    print('Missing tools: ' + (', '.join(missing) if missing else 'none'))
    print('No capture/inference performed. Native runtime and performance require separate tests.')
    return 1 if missing else 0


if __name__ == '__main__':
    raise SystemExit(main())
