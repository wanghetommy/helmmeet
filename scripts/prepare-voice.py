"""Explicit download bootstrap, with SHA checks; builds never fetch implicitly."""
import pathlib, json, hashlib, subprocess, argparse, shutil, tarfile
root=pathlib.Path(__file__).resolve().parents[1]
p=argparse.ArgumentParser();p.add_argument('--download',action='store_true');a=p.parse_args()
lock=json.loads((root/'native/voice-assistant/dependencies.lock.json').read_text());cache=root/'.tools/voice';downloads=cache/'downloads';downloads.mkdir(parents=True,exist_ok=True)
for kind in ('library','kws','vad'):
 spec=lock[kind];path=downloads/spec['archive']
 if not path.exists() or path.stat().st_size==0:
  if not a.download:raise SystemExit('离线语音依赖未就绪；请运行 python3 scripts/prepare-voice.py --download')
  url='https://api.github.com/repos/k2-fsa/sherpa-onnx/releases/assets/'+str(spec['asset_id']) if 'asset_id' in spec else 'https://github.com/k2-fsa/sherpa-onnx/releases/download/kws-models/'+spec['archive']
  subprocess.run(['curl','--fail','--location','--continue-at','-','--connect-timeout','20','--max-time','300','-H','Accept: application/octet-stream',url,'-o',str(path)+'.download'],check=True)
  pathlib.Path(str(path)+'.download').replace(path)
 if hashlib.sha256(path.read_bytes()).hexdigest()!=spec['sha256']:raise SystemExit(kind+' SHA256不匹配')
 if kind!='vad':
  destination=cache/('vendor' if kind=='library' else 'models');destination.mkdir(exist_ok=True)
  with tarfile.open(path) as tar:tar.extractall(destination,filter='data')
models=cache/'models'/lock['kws']['archive'].removesuffix('.tar.bz2');bundle=cache/'bundle';bundle.mkdir(exist_ok=True)
for name,spec in lock['kws']['files'].items():
 source=models/spec['source']
 if hashlib.sha256(source.read_bytes()).hexdigest()!=spec['sha256']:raise SystemExit('模型文件SHA256不匹配')
 shutil.copyfile(source,bundle/name)
shutil.copyfile(downloads/lock['vad']['archive'],bundle/'silero_vad.onnx')
shutil.copyfile(root/'native/voice-assistant/dependencies.lock.json',bundle/'manifest.json')
print('Prepared offline KWS + VAD resources')
