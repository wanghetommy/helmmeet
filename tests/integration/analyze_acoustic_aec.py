"""Local analysis of an explicitly authorized acoustic capture (requires local numpy).

Does not record, play, transcribe or send audio. Double-talk energy is not ERLE or
near-end preservation: a human must compare the paired, equal-gain listening clips.
"""
import argparse
import json
import math
from pathlib import Path
import sqlite3
import wave

import numpy as np


def pcm(path):
    with wave.open(str(path), 'rb') as w:
        assert (w.getnchannels(), w.getsampwidth(), w.getframerate()) == (1, 2, 16000)
        return np.frombuffer(w.readframes(w.getnframes()), dtype='<i2').astype(np.float64) / 32768


def wav(path, samples):
    with wave.open(str(path), 'wb') as w:
        w.setparams((1, 2, 16000, 0, 'NONE', 'not compressed'))
        w.writeframes(np.rint(np.clip(samples, -1, 32767 / 32768) * 32768).astype('<i2').tobytes())


def power(x):
    return float(np.mean(x * x)) if len(x) else 0.


def db(value):
    return 10 * math.log10(max(value, 1e-15))


def analyze(folder, variant='capture'):
    root = folder / variant
    participation = json.loads((folder / 'participation.json').read_text()) if (folder / 'participation.json').exists() else {}
    plan = json.loads((folder / 'plan.json').read_text())
    raw, out, ref, stimulus = [pcm(p) for p in [root / 'microphone-raw.wav', root / 'microphone-aec.wav', root / 'system-reference.wav', folder / 'speaker-script.wav']]
    assert len(raw) == len(out) == len(ref)
    # Find the SCK reference's actual shift relative to the known playback, rather
    # than assuming process spawn equals first audible sample. Search only +/-2s.
    a, b = ref[:28 * 16000:8], stimulus[:28 * 16000:8]
    n = 1 << (len(a) + len(b) - 1).bit_length()
    corr = np.fft.irfft(np.fft.rfft(a, n) * np.conj(np.fft.rfft(b, n)), n)
    lags = np.arange(-4000, 4001)
    lag = int(lags[np.argmax(corr[lags % n])]) * 8
    # Refine the decimated estimate to single-sample precision. An 8-sample
    # rounding error alone can lower speech correlation substantially.
    candidates = range(lag - 8, lag + 9)
    def score(shift):
        ia, ib = max(0, shift), max(0, -shift)
        length = min(len(ref) - ia, len(stimulus) - ib, 24 * 16000)
        return float(np.dot(ref[ia:ia + length], stimulus[ib:ib + length]))
    lag = max(candidates, key=score)
    start_a, start_b = max(0, lag), max(0, -lag)
    count = min(len(ref) - start_a, len(stimulus) - start_b, 24 * 16000)
    matched_a, matched_b = ref[start_a:start_a + count], stimulus[start_b:start_b + count]
    coefficient = float(np.dot(matched_a, matched_b) / max(1e-15, np.linalg.norm(matched_a) * np.linalg.norm(matched_b)))
    valid = np.zeros(len(raw), dtype=bool)
    if (root / 'app.sqlite-wal').exists() and (root / 'app.sqlite-wal').stat().st_size > 0:
        raise RuntimeError('Stop and close the diagnostic before analysis; WAL must be checkpointed')
    # The diagnostic has exited and checkpointed. immutable avoids creating SHM
    # files while inspecting a sealed, read-only snapshot in a restricted sandbox.
    with sqlite3.connect(f'file:{root / "app.sqlite"}?mode=ro&immutable=1', uri=True) as conn:
        intervals = conn.execute('SELECT start_sample,end_sample,state,reason,delay_ms FROM aec_intervals ORDER BY start_sample').fetchall()
        for lo, hi, state, _, _ in intervals:
            if state == 'applied':
                valid[lo:hi] = True
        gaps = conn.execute("SELECT kind,start_ms,end_ms,state,reason FROM coverage_intervals WHERE kind LIKE 'capture_%' AND state!='covered'").fetchall()
        routes = conn.execute('SELECT device,transport,sample_rate,supported,min(start_sample),max(end_sample) FROM audio_output_routes GROUP BY device,transport,sample_rate,supported').fetchall()
        sources = conn.execute('SELECT source,state,device,reason FROM recording_sources').fetchall()
        no_models = {table: conn.execute(f'SELECT count(*) FROM {table}').fetchone()[0] for table in ['asr_chunks', 'analysis_jobs', 'transcript_segments']}
    phases = {}
    for name, (lo, hi) in plan['phases_seconds_in_playback'].items():
        lo, hi = max(0, lo * 16000 + lag), min(len(raw), hi * 16000 + lag)
        r, o, s = raw[lo:hi], out[lo:hi], ref[lo:hi]
        values = {'start_s': lo / 16000, 'end_s': hi / 16000,
                  'raw_rms_dbfs': db(power(r)), 'aec_rms_dbfs': db(power(o)),
                  'system_rms_dbfs': db(power(s)), 'applied_fraction': float(np.mean(valid[lo:hi])),
                  'raw_clipped_samples': int(np.sum(np.abs(r) >= .999)),
                  'aec_clipped_samples': int(np.sum(np.abs(o) >= .999))}
        if name == 'far_only':
            # Evaluate applied 20ms blocks with excited reference. A caller must
            # confirm that the human stayed silent in this designated window.
            excited = np.zeros(len(r), dtype=bool)
            for i in range(0, len(r) - 319, 320):
                if power(s[i:i + 320]) > 1e-5 and valid[lo + i:lo + i + 320].all():
                    excited[i:i + 320] = True
            values['evaluated_seconds'] = float(np.sum(excited) / 16000)
            eligible = excited.any() and coefficient >= .9 and participation.get('far_only_silent') is True
            values['echo_window_reduction_db'] = db(power(r[excited])) - db(power(o[excited])) if eligible else None
            values['measurement_eligible'] = bool(eligible)
            values['interpretation'] = 'Far-only microphone energy reduction, includes ambient noise; not double-talk ERLE.'
        if name in ('far_only', 'near_only', 'double_talk'):
            wav(root / f'{name}-raw.wav', r)
            wav(root / f'{name}-aec.wav', o)
            wav(root / f'{name}-AB.wav', np.concatenate([r, np.zeros(16000), o]))
            values['listening'] = 'AB clip = raw, 1s silence, AEC; identical gain, no loudness normalization'
            values['human_quality_verdict'] = participation.get('listening_verdict', 'pending listening and participant confirmation')
        phases[name] = values
    report = {'duration_s': len(raw) / 16000, 'playback_shift_ms': lag / 16,
              'stimulus_reference_correlation': coefficient, 'phases': phases,
              'capture_noncovered_intervals': gaps, 'routes': routes, 'sources': sources,
              'model_work_counts': no_models, 'aec_intervals': intervals, 'participation': participation,
              'limits': ['Far-end speech is local TTS played through real speakers.',
                         'Human speaking and listening require participant confirmation.',
                         'Double-talk total energy is not a near-end gain or intelligibility metric.',
                         'Two-hour stability explicitly deferred by user.']}
    (root / 'quality.json').write_text(json.dumps(report, ensure_ascii=False, indent=2))
    print(json.dumps({k: v for k, v in report.items() if k != 'aec_intervals'}, ensure_ascii=False, indent=2))


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--folder', type=Path, required=True)
    parser.add_argument('--variant', choices=['capture', 'replay'], default='capture')
    args = parser.parse_args()
    analyze(args.folder.resolve(), args.variant)
