import { useAssistantIdentity } from './AssistantIdentity';
import { useLayoutEffect, useRef, useState, type ReactNode } from 'react';
import { call, meetingPhase, type Action, type Artifact, type Detail } from './bridge';
import { modelName } from './ui';

const labels: Record<string, string> = { stage: '实时更新', minutes: '会议纪要', draft: '待确认', confirmed: '已确认', incomplete: '覆盖不完整', stale: '需要重新确认', pending: '排队', running: '生成中', succeeded: '已完成', failed: '失败', cancelled: '已取消', todo: '未开始', doing: '进行中', done: '已完成', ignored: '已忽略' };
const preferences = new Map<string, { scrollTop?: number }>();

function statusText(artifact?: Artifact) {
  if (!artifact) return '待生成';
  if (artifact.status === 'stale') return '需要重新确认';
  if (artifact.status === 'incomplete') return '覆盖不完整';
  if (artifact.type === 'stage') return '实时更新';
  if (artifact.status === 'confirmed') return '已确认';
  return '待确认';
}

export function AnalysisWorkspace({ detail, tab, setTab, artifactView, configure, editAction, artifactId, setArtifactId, notify, fail }: {
  detail: Detail; tab: string; setTab: (t: string) => void; question: string; setQuestion: (t: string) => void;
  artifactView: (a: Artifact) => ReactNode; configure: () => void; editAction: (a: Action, proposal?: string) => void;
  artifactId: string; setArtifactId: (s: string) => void; notify: (s: string) => void; fail: (s: string) => void;
}) {
  const { name: assistantName } = useAssistantIdentity();
  const [supersededFailures, setSupersededFailures] = useState<string[]>([]);
  const [pending, setPending] = useState<Record<string, boolean>>({});
  const [historyId, setHistoryId] = useState('');
  preferences.set(detail.id, { ...preferences.get(detail.id) });
  const scroller = useRef<HTMLDivElement>(null);
  useLayoutEffect(() => { if (scroller.current) scroller.current.scrollTop = preferences.get(detail.id)?.scrollTop ?? 0; }, [detail.id]);
  const after = meetingPhase(detail) === 'after';
  const activeTab = tab === '会议总结' ? '会议纪要' : tab;
  const tabs = ['会议纪要', '行动事项'];
  const enabled = detail.capabilities?.can_analyze ?? (detail.analysis_mode !== 'transcript_only' && detail.state !== 'ready' && detail.segments.some(s => s.text.trim()));
  const modelReady = detail.capabilities?.model_ready ?? detail.analysis_mode !== 'transcript_only';
  const summaries = detail.artifacts.filter(a => a.type === 'stage');
  const minutes = detail.artifacts.filter(a => a.type === 'minutes');
  const latestSummary = summaries[0];
  const latestMinute = minutes.find(a => a.id === artifactId) ?? minutes[0];
  const preferred = after && latestMinute ? latestMinute : latestSummary;
  const history = [...summaries.map(a => ({ ...a, lane: '实时更新' })), ...minutes.map(a => ({ ...a, lane: '会后整理' }))].sort((a, b) => b.coverage.to_ms - a.coverage.to_ms || b.version - a.version);
  const selected = history.find(a => a.id === historyId) ?? preferred;
  const selectedLane = selected?.type === 'stage' ? '实时更新' : selected ? '会后整理' : '';
  const primaryJobType = after ? 'final_map' : 'stage';
  const running = detail.jobs.filter(j => j.type === primaryJobType && ['pending', 'running'].includes(j.state));
  const schedule = detail.schedule;
  const work = async (name: string, operation: Record<string, unknown>) => {
    if (pending[name] || (operation.kind === 'analyze' && !enabled)) return;
    setPending(p => ({ ...p, [name]: true }));
    try { await call({ ...operation, meeting_id: detail.id }); if (operation.kind === 'analyze') { const type = operation.analysis_type === 'stage' ? 'stage' : operation.analysis_type === 'minutes' ? 'final_map' : 'qa'; const old = detail.jobs.find(j => j.type === type && j.state === 'failed'); if (old) setSupersededFailures(ids => [...ids, old.id]); } return true; } catch (e) { fail(String(e)); return false; } finally { setPending(p => ({ ...p, [name]: false })); }
  };
  const setSchedule = (on: boolean, mode: string) => work('schedule', { kind: 'set_summary_schedule', expected_version: schedule.version, enabled: on, mode });
  const failureJobs = detail.jobs.filter(j => j.type === primaryJobType).slice(0, 1).filter(j => j.state === 'failed' && !supersededFailures.includes(j.id) && !pending.stage && !pending.minutes && !pending.answer);
  const actionList = <>{detail.actions.map(a => <article className="action-card" key={a.id}><div className="section-line"><strong>{a.task}</strong><span className="tag">{a.confirmation_state === 'pending' ? '待确认' : labels[a.confirmation_state] ?? '待确认'}</span></div><p>{a.owner_text || '负责人待确认'} · {a.due_text || '时间待确认'}</p><small>{labels[a.execution_state]}</small><button onClick={() => editAction(a)}>核对 / 编辑</button>{detail.proposals.filter(p => p.action_id === a.id).map(p => <div className="banner" key={p.id}>有新的字段建议<button onClick={() => editAction({ ...a, ...p.changes }, p.id)}>核对建议</button></div>)}</article>)}{!detail.actions.length && <p className="empty-note">共享行动候选将在分析后出现；不需要先生成会议纪要。</p>}</>;
  return <section className="assistant-panel"><div className="assistant-heading"><div><strong>会议纪要</strong><span className={`tag ${selected?.status ?? 'pending'}`}>· {statusText(selected)}</span></div><button className="text-button" onClick={configure}>{detail.model_profile ? modelName(detail.model_profile) : '选择模型'}</button></div>
    <div className="tabs" role="tablist" aria-label="会议内容">{tabs.map(t => <button id={`tab-${t}`} aria-controls={`panel-${t}`} role="tab" aria-selected={activeTab === t} tabIndex={activeTab === t ? 0 : -1} key={t} onKeyDown={e => { if (e.key === 'ArrowRight' || e.key === 'ArrowLeft') { const next = tabs[(tabs.indexOf(t) + (e.key === 'ArrowRight' ? 1 : tabs.length - 1)) % tabs.length]!; setTab(next); document.getElementById(`tab-${next}`)?.focus(); } }} onClick={() => setTab(t)}>{t}{t === '行动事项' ? ` ${detail.actions.length}` : ''}</button>)}</div>
    <div className="assistant-scroll" ref={scroller} onScroll={e => preferences.set(detail.id, { scrollTop: e.currentTarget.scrollTop })}>
    {!modelReady && <div className="banner">配置文本模型后可更新会议纪要或提问。<button onClick={configure}>选择模型</button></div>}
    {detail.capabilities?.analysis_paused && <div className="banner">全局分析已暂停，可在设置中恢复。</div>}
    {!detail.segments.length && <p className="empty-note">已确认的转写文字出现后即可更新会议纪要。</p>}
    {running.map(j => <div role="status" className="banner" key={j.id}>{labels[j.state]} · 录音与转写继续<button onClick={() => void work(j.id, { kind: 'cancel_job', job_id: j.id })}>取消本次</button></div>)}
    {failureJobs.map(j => <details className="job-error" key={j.id}><summary>最近一次纪要更新失败</summary><p>{j.error ?? '分析失败，请检查服务后手动重试。'}</p></details>)}
    {activeTab === '会议纪要' ? <div id="panel-会议纪要" role="tabpanel" aria-labelledby="tab-会议纪要">
      <div className="summary-settings"><label className="inline-check"><input type="checkbox" checked={schedule.enabled} disabled={pending.schedule || ((!modelReady || detail.capabilities?.analysis_paused || after) && !schedule.enabled)} onChange={e => void setSchedule(e.target.checked, schedule.mode)} />实时更新会议纪要（同步更新{assistantName}记忆）</label><label className="inline-check"><input type="checkbox" checked={detail.calibration?.enabled ?? false} disabled={pending.calibration || !modelReady || detail.analysis_mode === 'transcript_only'} onChange={e => void work('calibration', { kind: 'set_calibration', expected_version: detail.version, enabled: e.target.checked, mode: detail.calibration?.mode ?? 'standard' })} />实时语音校准</label><select aria-label="语音校准频率" value={detail.calibration?.mode ?? 'standard'} disabled={pending.calibration || !detail.calibration?.enabled} onChange={e => void work('calibration', { kind: 'set_calibration', expected_version: detail.version, enabled: true, mode: e.target.value })}><option value="low_latency">较快 · 400字</option><option value="standard">标准 · 800字</option><option value="high_accuracy">稳健 · 1200字</option></select><button disabled={!detail.calibration?.enabled || !modelReady || pending.calibrationRun} onClick={() => void work('calibrationRun', { kind: 'run_calibration', expected_version: detail.version })}>{pending.calibrationRun ? '校准排队中…' : '立即校准全部未处理内容'}</button>
        <select aria-label="会议纪要更新频率" value={schedule.mode} disabled={pending.schedule} onChange={e => void setSchedule(schedule.enabled, e.target.value)}>{schedule.mode.startsWith('legacy') && <option value={schedule.mode}>沿用旧间隔 {schedule.mode === 'legacy_5' ? '5' : '10'} 分钟</option>}<option value="realtime">实时 · 30秒且20字，或100字</option><option value="three_minutes">3分钟 · 至少200字</option><option value="economy">节省调用</option></select>
        <button disabled={!enabled || after || pending.stage || running.length > 0} onClick={() => void work('stage', { kind: 'analyze', analysis_type: 'stage', question: '', scope: 'all' })}>立即更新纪要</button></div>
      <details className="schedule-explanation"><summary>{detail.summary_pending_segments ? `${detail.summary_pending_segments} 段新内容待更新` : '暂无新内容'}</summary><p className="muted">{detail.summary_pending_segments ? `${detail.summary_pending_segments} 段新内容待处理 · ` : '暂无新内容 · '}实时更新在满足字数或时间条件后生成；暂停时冻结。实际更新还需等待转写、排队和模型生成。</p></details>{detail.calibration?.enabled && <p className="muted">{detail.calibration.run?.state === 'running' ? `校准处理中 ${detail.calibration.run.completed}/${detail.calibration.run.total} 段 · ` : ''}按模型预算动态分批；已校准 {detail.calibration.succeeded} 段{detail.calibration.needs_review ? `，${detail.calibration.needs_review} 段待确认` : ''}{detail.calibration.pending ? `，${detail.calibration.pending} 段排队中` : ''}{detail.calibration.failed ? `，${detail.calibration.failed} 段失败` : ''}。原始识别文本始终保留。</p>}
      {schedule.migration_notice && <small className="muted">{schedule.migration_notice}</small>}
      {detail.transient.summary_state && <p role="status" className="muted">{detail.transient.summary_state} · {detail.transient.summary_pending_chars ?? 0}字待处理</p>}
      {(detail.summary_pending_bytes ?? 0) >= 50 * 1024 && <div role="status" className="banner">会议纪要与助手记忆正在追赶积压，原文已保存在本机，将分批处理。</div>}
      {schedule.paused_reason && <div className="banner error">实时更新已暂停：{schedule.paused_reason}<button onClick={() => void setSchedule(true, schedule.mode)}>重新开启</button></div>}
      <div className="minutes-actions"><button disabled={!enabled || pending.minutes || running.length > 0} onClick={() => void work('minutes', { kind: 'analyze', analysis_type: 'minutes', question: '', scope: 'all' })}>{after ? '生成整场纪要草稿' : '生成截至当前的纪要草稿'}</button></div>
      {detail.template && <details><summary>本场类型与模板 v{detail.template.version}</summary><p className="prewrap">{detail.template.instructions}</p></details>}
      {selected && <small className="muted">{selectedLane} · 依据截至 {Math.floor(selected.coverage.to_ms / 60000)}:{String(Math.floor(selected.coverage.to_ms / 1000) % 60).padStart(2, '0')}{selected.coverage.incremental ? ' · 增量更新' : ''}</small>}
      {selected ? artifactView(selected) : <div className="empty-note">会议纪要会随着实时记录更新；会后可生成待确认草稿。</div>}
      {history.length > 0 && <details className="summary-history"><summary>更新历史（{history.length}）</summary><select aria-label="会议纪要版本" value={selected?.id ?? ''} onChange={e => { const a = history.find(x => x.id === e.target.value); setHistoryId(e.target.value); if (a?.type === 'minutes') setArtifactId(a.id); }}><option value="">查看最新</option>{history.map(a => <option key={a.id} value={a.id}>v{a.version} · {a.lane} · {statusText(a)}</option>)}</select><button onClick={() => setHistoryId('')}>查看最新</button></details>}
      <div className="button-row">{selected && <button onClick={() => { void work('copy', { kind: 'copy_artifact', artifact_id: selected.id }).then(ok => { if (ok) notify(`已复制「${detail.title}」会议纪要`); }); }}>复制会议纪要</button>}</div>
    </div> : <div id="panel-行动事项" role="tabpanel" aria-labelledby="tab-行动事项">{actionList}</div>}
    </div></section>;
}
