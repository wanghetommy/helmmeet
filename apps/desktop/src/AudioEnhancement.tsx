import { call, type Detail, type Settings } from './bridge';
const modes = <><option value="off">关闭降噪</option><option value="low">轻度降噪</option><option value="moderate">中度降噪</option></>;
type Perform = (work: () => Promise<unknown>) => Promise<void>;
export function AudioEnhancementDefault({ settings, busy, perform, reload }: { settings: Settings | undefined; busy: boolean; perform: Perform; reload: () => Promise<void> }) {
  return <section className="panel"><h2>语音增强 · 实验版</h2><label>新会议默认降噪<select aria-label="新会议默认降噪" disabled={busy || !settings?.audio_enhancement_available} value={settings?.audio_enhancement_default ?? 'off'} onChange={e => { const mode = e.target.value; void perform(async () => { await call({ kind: 'set_audio_enhancement_default', mode }); await reload(); }); }}>{modes}</select></label><p>仅处理麦克风，电脑声音保持原声。新会议可单独调整；开始录制后强度固定。转写仍使用原声。回声消除在本场双音源设置中按需开启；耳机无需开启。</p>{settings && !settings.audio_enhancement_available && <p role="status">此构建未包含增强组件。</p>}</section>;
}
export function MeetingEnhancement({ detail, source, busy, perform }: { detail: Detail; source: number; busy: boolean; perform: Perform }) {
  const e = detail.enhancement;
  if (!e) return null;
  const configurable = detail.state === 'ready';
  return <div className="enhancement-strip">
    {configurable ? <label>本场麦克风降噪<select aria-label="本场麦克风降噪" value={e.mode} disabled={busy || source === 2 || !e.available} onChange={ev => { const mode = ev.target.value; void perform(() => call({ kind: 'set_audio_enhancement', meeting_id: detail.id, mode })); }}>{modes}</select></label> : <span>麦克风：{{ off: e.echo_cancellation ? '未开降噪' : '原声', low: '轻度降噪', moderate: '中度降噪' }[e.mode] ?? e.mode}</span>}
    {configurable && <label className="inline-check"><input aria-label="外放回声消除" type="checkbox" checked={e.echo_cancellation ?? false} disabled={busy || !e.available || (source !== 3 && !e.echo_cancellation)} onChange={ev => { const enabled = ev.target.checked; void perform(() => call({ kind: 'set_audio_enhancement', meeting_id: detail.id, mode: e.mode, echo_cancellation: enabled })); }} />外放回声消除（实验版，需双音源）</label>}
    {configurable && e.echo_cancellation && <small>确认会议声音从默认扬声器外放。仅支持内置或USB输出；蓝牙、其他应用单独指定不同输出时请关闭。开始后参数固定。</small>}
    {!configurable && e.echo_cancellation && <small role="status">AEC：{({applied:'参考有效，已处理',warming:'校准中，暂用降噪／原声',silent:'当前无播放信号',bypass:'参考不可用，已旁路',waiting:'等待音轨'} as Record<string,string>)[e.aec_status?.state] ?? '等待参考'}{e.aec_status?.state === 'bypass' && `（${({ reference_missing: '系统音轨缺段', reference_unreadable: '系统音轨不可读', route_unsupported: '输出设备暂不支持', route_unobserved: '路由切换或未确认', timing_unavailable: '缺少精确时间信息' } as Record<string,string>)[e.aec_status.reason] ?? '参考不足'}）`}{e.aec_status?.delay_ms != null ? ` · APM内部延迟 ${e.aec_status.delay_ms}ms` : ''} · AEC已处理 {(e.aec_applied_ms / 1000).toFixed(1)} 秒{e.aec_bypass_ms > 0 ? ` · 校准／旁路 ${(e.aec_bypass_ms / 1000).toFixed(1)} 秒` : ''}</small>}
    {source === 2 ? <small>仅电脑声音，无需麦克风降噪</small> : (e.mode !== 'off' || e.echo_cancellation) && <small role="status">{e.state === 'failed' ? '增强失败，原声保留' : e.state === 'complete' ? '增强完成' : configurable ? '开始后生成独立增强音轨' : '增强处理中'}{!configurable && ` · 已处理 ${(e.processed_ms / 1000).toFixed(1)} / ${(e.total_ms / 1000).toFixed(1)} 秒`}</small>}
    {(e.mode !== 'off' || e.echo_cancellation) && !e.available && <span role="alert">此构建无法继续处理，请切换原声或使用含增强组件的版本</span>}
    {e.error && <span role="alert">{e.error}</span>}{e.state === 'failed' && e.available && <button disabled={busy} onClick={() => void perform(() => call({ kind: 'retry_audio_enhancement', meeting_id: detail.id }))}>重试增强</button>}
  </div>;
}
