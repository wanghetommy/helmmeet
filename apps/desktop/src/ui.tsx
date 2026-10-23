import type { ModelProfile } from './bridge';
import { useLayoutEffect, useRef, type ReactNode } from 'react';
export const labels: Record<string, string> = { ready: '准备中', recording: '录制中', paused: '已暂停', interrupted: '录制中断', finalizing: '整理中', completed: '已结束', needs_attention: '需要处理', draft: '待确认', confirmed: '已确认', incomplete: '覆盖不完整', stale: '需要重新确认', pending: '待确认', ignored: '已忽略', todo: '未开始', doing: '进行中', done: '已完成', decision: '决定', discussion: '讨论', risk: '风险', question: '待解决', suggestion: '建议', unanswered: '未明确', stage: '实时更新', minutes: '会议纪要', answer: '回答' };
export const time = (ms: number) => `${Math.floor(ms / 60000).toString().padStart(2, '0')}:${Math.floor(ms / 1000 % 60).toString().padStart(2, '0')}`;
export const modelName = (p: Pick<ModelProfile, 'display_name' | 'model'>) => p.display_name === '已迁移的文本模型' ? p.model : p.display_name;
export const typeName = (id: string, name?: string | null) => id === 'general' ? '通用会议' : name ?? '会议';
export function Icon({ name, size = 18 }: { name: string; size?: number }) {
  const paths: Record<string, ReactNode> = {
    chat: <><path d="M4 4h16v12H9l-5 4z" /><path d="M8 8h8m-8 4h6" /></>,
    meetings: <><rect x="3" y="4" width="18" height="17" rx="3" /><path d="M7 2v4m10-4v4M3 10h18m-13 5h3m3 0h2" /></>,
    library: <><path d="M4 4h6v16H4zm6 0h5v16h-5zm6 1 4-1 3 15-4 1z" /></>,
    settings: <><path d="m9 3-.5 2-2 1-2-.5-2 3 1.5 1.5v3L3 14.5l2 3 2-.5 2 1 .5 2h4l.5-2 2-1 2 .5 2-3-1.5-1.5v-3L20 8.5l-2-3-2 .5-2-1-.5-2z" /><circle cx="11.5" cy="11.5" r="3" /></>,
    plus: <path d="M12 5v14M5 12h14" />, back: <path d="m14 5-7 7 7 7" />, close: <path d="m6 6 12 12M18 6 6 18" />,
    search: <><circle cx="10.5" cy="10.5" r="6.5" /><path d="m16 16 5 5" /></>,
    more: <><circle cx="5" cy="12" r="1" /><circle cx="12" cy="12" r="1" /><circle cx="19" cy="12" r="1" /></>,
    mic: <><rect x="8" y="2" width="8" height="13" rx="4" /><path d="M5 10v2a7 7 0 0 0 14 0v-2M12 19v3m-4 0h8" /></>,
    play: <path d="m8 4 12 8-12 8z" />, pause: <><path d="M8 4v16m8-16v16" /></>, stop: <rect x="5" y="5" width="14" height="14" rx="2" />,
    document: <><path d="M6 3h8l4 4v14H6zM14 3v5h4M9 12h6m-6 4h6" /></>,
    chevron: <path d="m7 10 5 5 5-5" />, check: <path d="m4 12 5 5L20 6" />,
    export: <><path d="M12 15V3m-4 4 4-4 4 4M4 14v6h16v-6" /></>,
    info: <><circle cx="12" cy="12" r="9" /><path d="M12 11v6m0-11v1" /></>,
    panel: <><rect x="3" y="4" width="18" height="16" rx="2" /><path d="M8 4v16" /></>,
  };
  return <svg width={size} height={size} viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="1.65" strokeLinecap="round" strokeLinejoin="round" aria-hidden="true" focusable="false">{paths[name] ?? paths.document}</svg>;
}
export function Modal({ title, children, close, locked = false, alert = false, wide = false }: { title: string; children: ReactNode; close: () => void; locked?: boolean; alert?: boolean; wide?: boolean }) {
  const ref = useRef<HTMLDialogElement>(null);
  const opener = useRef(document.activeElement instanceof HTMLElement ? document.activeElement : null);
  useLayoutEffect(() => {
    const dialog = ref.current!; dialog.showModal();
    (dialog.querySelector('[data-initial-focus]') as HTMLElement | null)?.focus();
    return () => { dialog.close(); const target = opener.current; if (target?.isConnected) target.focus(); };
  }, []);
  return <dialog ref={ref} className={`modal ${wide ? 'modal-wide' : ''}`} role={alert ? 'alertdialog' : 'dialog'} aria-modal="true" aria-labelledby="modal-title" onCancel={e => { e.preventDefault(); if (!locked) close(); }} onClick={e => { if (e.target === e.currentTarget && !locked) close(); }}><div className="modal-inner"><header><h2 id="modal-title">{title}</h2><button type="button" className="icon-button" aria-label="关闭弹窗" disabled={locked} onClick={close}><Icon name="close" /></button></header>{children}</div></dialog>;
}
export function Empty({ icon = 'document', title, children, action }: { icon?: string; title: string; children?: ReactNode; action?: ReactNode }) {
  return <div className="empty-state"><div className="empty-symbol"><Icon name={icon} size={26} /></div><h2>{title}</h2>{children && <p>{children}</p>}{action}</div>;
}
