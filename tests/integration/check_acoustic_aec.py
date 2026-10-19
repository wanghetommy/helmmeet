"""Opt-in real speaker/microphone check; never part of verify or CI.

The operator must first arrange a quiet room and a human double-talk participant.
Audio and the isolated database stay in a fresh local directory. No model calls.
"""
import argparse
import array
import json
import os
from pathlib import Path
import subprocess
import time
import wave

RATE = 16000
NEAR_TEXT = '这是我在本机麦克风说的话。项目目标是提高会议效率，今天要确认负责人、交付日期和风险。我会保持正常音量，也会轻声说完这句话。'
FAR_TEXT = '这里是远端会议发言。我们正在讨论产品方案和开发计划。第一项是确认会议目标，第二项是评估技术风险，第三项是明确验收标准。请保留发言人的语音细节和每句话的结尾。'


def read_pcm(path):
    with wave.open(str(path), 'rb') as wav:
        assert (wav.getnchannels(), wav.getsampwidth(), wav.getframerate()) == (1, 2, RATE)
        samples = array.array('h', wav.readframes(wav.getnframes()))
    return samples


def write_pcm(path, samples):
    with wave.open(str(path), 'wb') as wav:
        wav.setparams((1, 2, RATE, 0, 'NONE', 'not compressed'))
        wav.writeframes(samples.tobytes())


def prepare(folder):
    folder.mkdir(mode=0o700, parents=True, exist_ok=False)
    def speech(name, text):
        source, dest = folder / f'{name}.aiff', folder / f'{name}.wav'
        subprocess.run(['/usr/bin/say', '-v', 'Tingting', '-r', '190', '-o', str(source), text], check=True)
        subprocess.run(['/usr/bin/afconvert', '-f', 'WAVE', '-d', 'LEI16@16000', '-c', '1', str(source), str(dest)], check=True)
        return read_pcm(dest)
    far = speech('far', FAR_TEXT)
    near_cue = speech('near-cue', '接下来请你单独说话，按屏幕上的句子读。')
    both_cue = speech('both-cue', '现在开始双讲，请你继续说话，我也同时说话。')
    end_cue = speech('end-cue', '测试结束，谢谢，请停止说话。')
    track = array.array('h', [0]) * (65 * RATE)
    def place(samples, start, end, repeat=False):
        count = (end - start) * RATE
        if repeat:
            samples = (samples + array.array('h', [0]) * (RATE // 3)) * (count // len(samples) + 2)
        count = min(count, len(samples))
        track[start * RATE:start * RATE + count] = samples[:count]
    place(far, 4, 25, True)
    place(near_cue, 25, 29)
    place(both_cue, 41, 45)
    place(far, 45, 61, True)
    place(end_cue, 61, 65)
    write_pcm(folder / 'speaker-script.wav', track)
    plan = {'sample_rate': RATE, 'seconds': 68, 'near_text': NEAR_TEXT,
            'phases_seconds_in_playback': {'ambient': [0, 3], 'far_only': [8, 24],
                                         'near_only': [30, 40], 'double_talk': [47, 60]},
            'human_participation': 'Operator must confirm actual participation after capture',
            'stimulus': 'Local macOS TTS; acoustic microphone capture is real, far-end voice is synthesized'}
    (folder / 'plan.json').write_text(json.dumps(plan, ensure_ascii=False, indent=2))
    print(f'Prepared {folder}; microphone has NOT started.', flush=True)
    print(f'近端朗读：{NEAR_TEXT}', flush=True)


def capture(args):
    folder = args.folder.resolve()
    root = folder / 'capture'
    if root.exists():
        raise RuntimeError('Refusing to overwrite existing capture; prepare a new folder')
    env = os.environ.copy()
    for key in ('HELMMEET_DEV_CONFIG', 'HELMMEET_TEST_LLM', 'HELMMEET_DATA_ROOT'):
        env.pop(key, None)
    env.update(HELMMEET_ACOUSTIC_ROOT=str(root), HELMMEET_ACOUSTIC_SECONDS='68')
    binary = args.app.resolve() / 'Contents/MacOS/helmmeet-desktop'
    log = folder / 'capture-process.log'
    player = None
    with log.open('w') as output:
        process = subprocess.Popen([str(binary), '--check-acoustic-aec'], env=env, stdout=output, stderr=subprocess.STDOUT)
        try:
            deadline = time.monotonic() + 30
            while not (root / 'ready.json').exists():
                if process.poll() is not None or time.monotonic() >= deadline:
                    raise RuntimeError('Capture did not start; see local capture-process.log')
                time.sleep(.05)
            print('REAL microphone + system capture started; playing local speaker test.', flush=True)
            player = subprocess.Popen(['/usr/bin/afplay', str(folder / 'speaker-script.wav')])
            player.wait(timeout=75)
            code = process.wait(timeout=40)
            if code:
                raise RuntimeError(f'Capture failed ({code}); raw data retained, see {log}')
            print(f'Capture complete. Local comparison files: {root}', flush=True)
        finally:
            if player is not None and player.poll() is None:
                player.terminate()
                player.wait(timeout=5)
            if process.poll() is None:
                if root.is_dir():
                    (root / 'stop').touch()
                try:
                    process.wait(timeout=40)
                except subprocess.TimeoutExpired:
                    # Only this diagnostic child; normal capture first receives stop above.
                    process.kill()
                    process.wait()
                    raise RuntimeError('Native diagnostic did not stop; isolated raw files retained')


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('operation', choices=['prepare', 'capture'])
    parser.add_argument('--folder', type=Path, required=True)
    parser.add_argument('--app', type=Path, default=Path('target/release/bundle/macos/HelmMeet.app'))
    args = parser.parse_args()
    if args.operation == 'prepare':
        prepare(args.folder)
    else:
        capture(args)


if __name__ == '__main__':
    main()
