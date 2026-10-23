import { useEffect, useState } from 'react';
import { call, type Detail, type TranscriptSnapshot } from './bridge';
const preferences = new Map<string, { source: string; timestamps: boolean }>();
export function TranscriptOriginal({ detail, search, notify, fail }: { detail: Detail; search: string; notify: (s: string) => void; fail: (s: string) => void }) {
  const [source, setSource] = useState(preferences.get(detail.id)?.source ?? 'current');
  const [timestamps, setTimestamps] = useState(preferences.get(detail.id)?.timestamps ?? true);
  const [snapshot, setSnapshot] = useState<TranscriptSnapshot>();
  const [copying, setCopying] = useState(false);
  preferences.set(detail.id, { source, timestamps });
  const revisions = detail.segments.map(s => `${s.id}:${s.revision}`).join(',');
  useEffect(() => {
    let live = true;
    void call<TranscriptSnapshot>({ kind: 'read_transcript_snapshot', meeting_id: detail.id, source, timestamps }).then(s => { if (live) setSnapshot(s); }).catch(e => { if (live) fail(String(e)); });
    return () => { live = false; };
  }, [detail.id, revisions, source, timestamps, fail]);
  const copy = async () => {
    setCopying(true);
    try {
      const frozen = await call<TranscriptSnapshot>({ kind: 'read_transcript_snapshot', meeting_id: detail.id, source, timestamps });
      const result = await call<TranscriptSnapshot>({ kind: 'copy_transcript', meeting_id: frozen.meeting_id, source: frozen.source, timestamps: frozen.timestamps, expected_hash: frozen.hash });
      notify(`已复制「${result.title}」${source === 'current' ? '当前修订版' : '原始识别版'}，截至 ${Math.floor(result.through_ms / 60000)}:${String(Math.floor(result.through_ms / 1000) % 60).padStart(2, '0')}（全文，含 ${result.paragraphs?.length??result.segments.length} 段发言）`);
    } catch (e) { fail(String(e)); } finally { setCopying(false); }
  };
  return <div className="original-view"><div className="button-row"><select aria-label="原稿来源" value={source} onChange={e => { setSnapshot(undefined); setSource(e.target.value); }}><option value="current">当前修订版 · 含人工修订</option><option value="original">原始识别版</option></select><label className="inline-check"><input type="checkbox" checked={timestamps} onChange={e => setTimestamps(e.target.checked)} />时间戳</label><button disabled={copying || !snapshot?.text.trim()} onClick={() => void copy()}>{copying ? '复制中…' : '复制全文'}</button></div><small>仅已确认转写，搜索不影响复制全文。{source === 'original' ? '人工创建且无 ASR 原版的片段不在此版中。' : '包含人工修订；原始识别版本仍保留。'}</small><div className="original-text" aria-label="对话原稿">{snapshot ? (snapshot.paragraphs??snapshot.segments.map(s=>({...s,id:s.segment_id}))).filter(s => s.text.toLowerCase().includes(search.toLowerCase())).map(s => <p key={s.id}>{timestamps && <small>[{String(Math.floor(s.start_ms / 60000)).padStart(2, '0')}:{String(Math.floor(s.start_ms / 1000) % 60).padStart(2, '0')}] </small>}{s.text}</p>) : <p role="status">读取原稿…</p>}{snapshot && !snapshot.text.trim() && <p className="empty-note">暂无此来源的已确认文字</p>}</div></div>;
}
