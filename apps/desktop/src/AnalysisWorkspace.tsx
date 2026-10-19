import { useLayoutEffect, useRef, useState, type ReactNode } from 'react';
import { call, meetingPhase, type Action, type Artifact, type Detail } from './bridge';
import { modelName } from './ui';
const labels: Record<string, string> = { stage: '会议总结', minutes: '会议纪要', draft: '草稿', confirmed: '已确认', incomplete: '覆盖不完整', stale: '原文已更新', pending: '排队', running: '生成中', succeeded: '已完成', failed: '失败', cancelled: '已取消', todo: '未开始', doing: '进行中', done: '已完成', ignored: '已忽略' };
const preferences = new Map<string, { summaryId: string; minutesTab: string; scrollTop?: number }>();
const quick: Record<string, string[]> = {
  weekly: ['哪些目标和进度尚未澄清？', '目前识别到哪些卡点和依赖？', '下期安排有哪些缺口？'],
  review: ['评审还缺哪些验证？', '方案的假设和风险是什么？', '通过条件明确了吗？'],
  decision: ['有哪些选项和判断标准？', '哪些决定其实尚未成立？', '执行条件有哪些未明确？'],
  retro: ['事实与归因有哪些混淆？', '有哪些可验证的改进实验？'],
  brainstorm: ['想法可以如何归类？', '哪些方向值得进一步验证？'],
};
export function AnalysisWorkspace({ detail, tab, setTab, question, setQuestion, scope, setScope, focusSegment, artifactView, configure, editAction, artifactId, setArtifactId, notify, fail }: {
  detail: Detail; tab: string; setTab: (t: string) => void; question: string; setQuestion: (t: string) => void; scope: string; setScope: (t: string) => void; focusSegment: string;
  artifactView: (a: Artifact) => ReactNode; configure: () => void; editAction: (a: Action, proposal?: string) => void;
  artifactId: string; setArtifactId: (s: string) => void; notify: (s: string) => void; fail: (s: string) => void;
}) {
  const questionRef = useRef(question); questionRef.current = question;
  const [pending, setPending] = useState<Record<string, boolean>>({});
  const [summaryId, setSummaryId] = useState(preferences.get(detail.id)?.summaryId ?? ''); const [minutesTab, setMinutesTab] = useState(preferences.get(detail.id)?.minutesTab ?? '纪要正文');
  preferences.set(detail.id, { ...preferences.get(detail.id), summaryId, minutesTab });
  const scroller = useRef<HTMLDivElement>(null);
  useLayoutEffect(() => { if (scroller.current) scroller.current.scrollTop = preferences.get(detail.id)?.scrollTop ?? 0; }, [detail.id]);
  const work = async (name: string, operation: Record<string, unknown>) => {
    if (pending[name] || (operation.kind === 'analyze' && !enabled)) return;
    setPending(p => ({ ...p, [name]: true }));
    try { await call({ ...operation, meeting_id: detail.id }); return true; } catch (e) { fail(String(e)); return false; } finally { setPending(p => ({ ...p, [name]: false })); }
  };
  const after = meetingPhase(detail) === 'after';
  const tabs = after ? ['会议纪要', '行动事项', '会议助手', ...(tab === '会议总结' ? ['会议总结'] : [])] : ['会议总结', '会议纪要', '会议助手'];
  const enabled = detail.capabilities?.can_analyze ?? (detail.analysis_mode !== 'transcript_only' && detail.state !== 'ready' && detail.segments.some(s => s.text.trim()));
  const modelReady = detail.capabilities?.model_ready ?? detail.analysis_mode !== 'transcript_only';
  const summaries = detail.artifacts.filter(a => a.type === 'stage');
  const summary = summaries.find(a => a.id === summaryId) ?? summaries[0];
  const minutes = detail.artifacts.filter(a => a.type === 'minutes');
  const minute = minutes.find(a => a.id === artifactId) ?? minutes[0];
  const jobType = tab === '会议总结' ? 'stage' : (tab === '会议纪要' || tab === '行动事项') ? 'final_map' : 'qa';
  const running = detail.jobs.filter(j => j.type === jobType && ['pending', 'running'].includes(j.state));
  const schedule = detail.schedule;
  const setSchedule = (on: boolean, mode: string) => work('schedule', { kind: 'set_summary_schedule', expected_version: schedule.version, enabled: on, mode });
  return <section className="assistant-panel"><div className="assistant-heading"><strong>{after ? '会后整理' : '会议助手'}</strong><div>{after && <button className="text-button" onClick={() => setTab('会议总结')}>会中总结</button>}<button className="text-button" onClick={configure}>{detail.model_profile ? modelName(detail.model_profile) : '选择模型'}</button></div></div>
    <div className="tabs" role="tablist" aria-label="会议分析">{tabs.map(t => <button id={`tab-${t}`} aria-controls={`panel-${t}`} role="tab" aria-selected={tab === t} tabIndex={tab === t ? 0 : -1} key={t} onKeyDown={e => { if (e.key === 'ArrowRight' || e.key === 'ArrowLeft') { const ts = tabs; const next = ts[(ts.indexOf(t) + (e.key === 'ArrowRight' ? 1 : ts.length - 1)) % ts.length]!; setTab(next); document.getElementById(`tab-${next}`)?.focus(); } }} onClick={() => setTab(t)}>{t}</button>)}</div>
    <div className="assistant-scroll" ref={scroller} onScroll={e => preferences.set(detail.id, { summaryId, minutesTab, scrollTop: e.currentTarget.scrollTop })}>
    {!modelReady && <div className="banner">配置文本模型后可生成纪要或提问。<button onClick={configure}>选择模型</button></div>}
    {detail.capabilities?.analysis_paused && <div className="banner">全局分析已暂停，可在设置中恢复。</div>}
    {!detail.segments.length && <p className="empty-note">已确认的转写文字出现后即可分析。</p>}
    {running.map(j => <div role="status" className="banner" key={j.id}>{labels[j.state]} · 录音与转写继续<button onClick={() => void work(j.id, { kind: 'cancel_job', job_id: j.id })}>取消本次</button></div>)}
    {detail.jobs.filter(j => j.type === jobType).slice(0, 1).filter(j => j.state === 'failed' && !pending.stage && !pending.minutes && !pending.answer).map(j => <details className="job-error" key={j.id}><summary>最近一次生成失败</summary><p>{j.error ?? '分析失败，请检查服务后手动重试。'}</p></details>)}
    <div id="panel-会议总结" role="tabpanel" aria-labelledby="tab-会议总结" hidden={tab !== '会议总结'}>
      <div className="summary-settings"><label className="inline-check"><input type="checkbox" checked={schedule.enabled} disabled={pending.schedule || ((!modelReady || detail.capabilities?.analysis_paused || after) && !schedule.enabled)} onChange={e => void setSchedule(e.target.checked, schedule.mode)} />自动总结</label>
        <select aria-label="自动总结频率" value={schedule.mode} disabled={pending.schedule} onChange={e => void setSchedule(schedule.enabled, e.target.value)}>{schedule.mode.startsWith('legacy') && <option value={schedule.mode}>沿用旧间隔 {schedule.mode === 'legacy_5' ? '5' : '10'} 分钟</option>}<option value="realtime">实时 · 60 字或 15 秒</option><option value="minute">1 分钟</option><option value="three_minutes">3 分钟</option></select>
        <button disabled={!enabled || pending.stage || running.length > 0} onClick={() => void work('stage', { kind: 'analyze', analysis_type: 'stage', question: '', scope: 'all' })}>立即总结</button></div>
      <details className="schedule-explanation"><summary>{detail.summary_pending_segments ? `${detail.summary_pending_segments} 段新内容待总结` : '暂无新内容'}</summary><p className="muted">{detail.summary_pending_segments ? `${detail.summary_pending_segments} 段新内容待处理 · ` : '暂无新内容 · '}等待本轮完成后重新计时；暂停时冻结。没有新增已确认文字不请求。实际更新还需等待模型生成。</p></details>
      {schedule.paused_reason && <div className="banner error">自动总结已暂停：{schedule.paused_reason}<button onClick={() => void setSchedule(true, schedule.mode)}>重新开启</button></div>}
      {summary ? <><small>已总结截至 {Math.floor(summary.coverage.to_ms / 60000)}:{String(Math.floor(summary.coverage.to_ms / 1000) % 60).padStart(2, '0')}{summary.coverage.incremental ? ' · 增量更新' : ''}</small>{artifactView(summary)}<details className="summary-history"><summary>更新历史（{summaries.length}）</summary><select aria-label="总结版本" value={summary.id} onChange={e => setSummaryId(e.target.value)}>{summaries.map(a => <option key={a.id} value={a.id}>v{a.version} · {labels[a.status] ?? a.status}{a.content.manual_text ? ' · 人工版' : ''}</option>)}</select><button onClick={() => setSummaryId('')}>查看最新</button></details></> : <div className="empty-note">当前主题、讨论脉络、关键进展和未决问题将在这里更新。</div>}
    </div>
    <div id={tab === '行动事项' ? 'panel-行动事项' : 'panel-会议纪要'} role="tabpanel" aria-labelledby={tab === '行动事项' ? 'tab-行动事项' : 'tab-会议纪要'} hidden={tab !== '会议纪要' && tab !== '行动事项'}>
      {tab === '会议纪要' && <><details><summary>本场类型与模板 v{detail.template?.version ?? '?'}</summary><p className="prewrap">{detail.template?.instructions ?? '历史模板来源缺失'}</p></details>
      <div className="button-row"><button disabled={!enabled || pending.minutes || running.length > 0} onClick={() => void work('minutes', { kind: 'analyze', analysis_type: 'minutes', question: '', scope: 'all' })}>{['completed', 'needs_attention'].includes(detail.state) ? '生成整场纪要草稿' : '生成截至当前的纪要草稿'}</button></div>
      </>}{!after && <div className="tabs" aria-label="纪要内容">{['纪要正文', '行动事项'].map(t => <button key={t} aria-pressed={minutesTab === t} onClick={() => setMinutesTab(t)}>{t}{t === '行动事项' ? ` ${detail.actions.length}` : ''}</button>)}</div>}
      {(after ? tab === '会议纪要' : minutesTab === '纪要正文') ? <>{minutes.length > 0 && <select aria-label="纪要版本" value={minute?.id ?? ''} onChange={e => setArtifactId(e.target.value)}>{minutes.map(a => <option value={a.id} key={a.id}>v{a.version} · {labels[a.status] ?? a.status}{a.content.manual_text ? ' · 人工版' : ''}</option>)}</select>}{minute ? artifactView(minute) : <div className="empty-note">按本场模板整理正式草稿。结束录制不会自动生成纪要。</div>}</> : <>{detail.actions.map(a => <article className="action-card" key={a.id}><div className="section-line"><strong>{a.task}</strong><span className="tag">{a.confirmation_state === 'pending' ? '待确认' : labels[a.confirmation_state] ?? '待确认'}</span></div><p>{a.owner_text || '负责人待确认'} · {a.due_text || '时间待确认'}</p><small>{labels[a.execution_state]}</small><button onClick={() => editAction(a)}>核对 / 编辑</button>{detail.proposals.filter(p => p.action_id === a.id).map(p => <div className="banner" key={p.id}>有新的字段建议<button onClick={() => editAction({ ...a, ...p.changes }, p.id)}>核对建议</button></div>)}</article>)}{!detail.actions.length && <p className="empty-note">共享行动候选将在分析后出现；不需要先生成纪要。</p>}</>}
    </div>
    <div id="panel-会议助手" role="tabpanel" aria-labelledby="tab-会议助手" hidden={tab !== '会议助手'}>
      <div className="quick-questions">{(quick[detail.assistant_id] ?? ['目前有哪些目标未澄清？', '识别到哪些卡点和风险？', '请 review 讨论中的遗漏与未决事项。']).map(q => <button key={q} onClick={() => setQuestion(q)}>{q}</button>)}</div>
      <p className="muted">{detail.context_consent ? '结合本场原文与最近 8 轮对话回答，可点击引用核对。' : '仅本次问题与所选原文，不携带历史对话。'}</p>
      <button disabled={pending.clear} onClick={() => void work('clear', { kind: 'clear_conversation' })}>开始新对话</button>
      {detail.legacy_answer_ids.length > 0 && <details><summary>旧版独立问答（{detail.legacy_answer_ids.length}）</summary>{detail.artifacts.filter(a => detail.legacy_answer_ids.includes(a.id)).slice().reverse().map(artifactView)}</details>}
      <div className="answers">{detail.conversation.map(turn => <div key={turn.job_id}><p className="asked">{turn.question} <small>· {turn.scope === 'all' ? '整场' : turn.scope === 'recent' ? '最近10分钟' : '选中片段'}</small></p>{turn.artifact_id ? (() => { const a = detail.artifacts.find(a => a.id === turn.artifact_id); return a && artifactView(a); })() : <p className="muted">{labels[turn.state] ?? turn.state}{turn.error ? `：${turn.error}` : ''}</p>}</div>)}</div>
      <form className="question-form" onSubmit={e => { e.preventDefault(); if (!enabled || pending.answer || !question.trim()) return; const q = question; void work('answer', { kind: 'analyze', analysis_type: 'answer', question: q, scope: scope === 'selection' ? `selection:${focusSegment}` : scope }).then(ok => { if (ok && questionRef.current === q) setQuestion(''); }); }}><select aria-label="问题范围" value={scope} onChange={e => setScope(e.target.value)}><option value="all">整场已确认内容</option><option value="recent">最近 10 分钟</option><option value="selection" disabled={!focusSegment}>选中的原文片段</option></select><textarea aria-label="会议问题" value={question} placeholder="提问或追问，请助手 review 本场讨论…" onChange={e => setQuestion(e.target.value)} /><button className="primary" disabled={!enabled || pending.answer || !question.trim()}>发送</button></form>
    </div>
    {tab === '会议总结' && summary && <button onClick={() => { void work('note', { kind: 'note', time_ms: Math.floor(summary.coverage.to_ms), text: (summary.content.summary ?? []).map(s => s.text).join('\n') }).then(ok => { if (ok) notify(`已另存「${detail.title}」总结笔记`); }); }}>另存为笔记</button>}
    <div className="button-row">{(tab === '会议总结' ? summary : tab === '会议纪要' ? minute : undefined) && <button onClick={() => { const a = tab === '会议总结' ? summary : minute; void work('copy', { kind: 'copy_artifact', artifact_id: a?.id }).then(ok => { if (ok) notify(`已复制「${detail.title}」${tab === '会议总结' ? '总结' : '纪要'}`); }); }}>复制{tab === '会议总结' ? '总结' : '纪要'}</button>}</div>
  </div></section>;
}
