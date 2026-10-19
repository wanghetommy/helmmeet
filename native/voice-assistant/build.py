"""Offline, per-target static wrapper build. Archives are verified by prepare-voice."""
import argparse, pathlib, subprocess, hashlib, json
root=pathlib.Path(__file__).resolve().parents[2]
p=argparse.ArgumentParser();p.add_argument('--target',required=True);a=p.parse_args()
lock=json.loads((root/'native/voice-assistant/dependencies.lock.json').read_text())
for spec in [lock['library']]:
 archive=root/'.tools/voice/downloads'/spec['archive']
 if not archive.is_file() or hashlib.sha256(archive.read_bytes()).hexdigest()!=spec['sha256']: raise SystemExit('Voice dependency cache missing or corrupt; run python3 scripts/prepare-voice.py --download')
if hashlib.sha256((root/'native/voice-assistant/include/sherpa-onnx-c-api.h').read_bytes()).hexdigest()!=lock['header_sha256']:raise SystemExit('Pinned C API header changed')
arch={'aarch64-apple-darwin':'arm64','x86_64-apple-darwin':'x86_64'}[a.target]
source=root/'native/voice-assistant';out=root/'.tools/voice/targets'/a.target;out.mkdir(parents=True,exist_ok=True)
subprocess.run(['clang++','-std=c++17','-O2','-fvisibility=hidden','-arch',arch,'-mmacosx-version-min=14.0','-I'+str(source/'include'),'-c',str(source/'src/voice.cpp'),'-o',str(out/'voice.o')],check=True)
subprocess.run(['ar','rcs',str(out/'libhelmmeet_voice.a'),str(out/'voice.o')],check=True)
