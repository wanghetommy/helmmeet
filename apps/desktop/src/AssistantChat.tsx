import { useEffect, useRef, useState } from 'react';
import { call, desktop } from './bridge';
import { Empty, time } from './ui';
import { readAssistantAnswer } from './assistantAnswer';
type Turn = {id:string;question:string;source:string;state:string;error?:string;model_name?:string;answer?:unknown};
type Conversation = {id:string;meeting_id:string|null;title:string;turns:Turn[];proposals:{id:string;turn_id:string;state:string;payload:{action:string;title:string;text:string};result?:{meeting_id?:string}}[]};
type Row = {id:string;meeting_id:string|null;title:string};
const drafts=new Map<string,string>();
const states:Record<string,string>={pending:'正在排队',running:'小舵正在检索和思考…',failed:'本次回答未完成',cancelled:'已取消'};
const kinds:Record<string,string>={meeting_fact:'会议事实',knowledge:'资料依据',suggestion:'小舵建议',unanswered:'待澄清',legacy:'历史回答'};
export function AssistantChat({meetingId,embedded=false,fail,onMeeting,voiceTurn,scope="all",setScope,focusSegment,activeMeetingId}:{meetingId?:string;embedded?:boolean;fail:(e:string)=>void;onMeeting?:(id:string)=>void;voiceTurn?:number;scope?:string;setScope?:(s:string)=>void;focusSegment?:string;activeMeetingId?:string}){
  const [conversation,setConversation]=useState<Conversation>(); const [rows,setRows]=useState<Row[]>([]);const [question,setQuestion]=useState('');const [busy,setBusy]=useState(false);const [loading,setLoading]=useState(desktop);
  const current=useRef('');const serial=useRef(0);const mounted=useRef(true);const scroller=useRef<HTMLDivElement>(null);const follow=useRef(true);
  const show=(d:Conversation)=>{current.current=d.id;setConversation(d);setQuestion(drafts.get(d.id)??'');};
  const draft=(q:string)=>{if(current.current)drafts.set(current.current,q);setQuestion(q);};
  const load=async(id=current.current)=>{if(!id||!desktop)return;const d=await call<Conversation>({kind:'assistant_detail',conversation_id:id});if(mounted.current&&id===current.current)setConversation(d);};
  const open=async(general=false,id?:string)=>{
    if(!desktop)return;const token=++serial.current;setLoading(true);
    try{const d=await call<Conversation>({kind:'assistant_open',meeting_id:id??meetingId??null,general});if(token===serial.current&&mounted.current){show(d);follow.current=true;}setRows(await call<Row[]>({kind:'assistant_list'}));}catch(e){fail(String(e));}finally{if(token===serial.current&&mounted.current)setLoading(false);}
  };
  useEffect(()=>{mounted.current=true;void open();const timer=setInterval(()=>void load().catch(e=>fail(String(e))),900);return()=>{mounted.current=false;serial.current++;clearInterval(timer);};},[meetingId]);
  useEffect(()=>{if(voiceTurn)void load();},[voiceTurn]);
  const last=conversation?.turns.at(-1);const pending=conversation?.turns.find(t=>['pending','running'].includes(t.state));
  useEffect(()=>{if(follow.current&&scroller.current)scroller.current.scrollTop=scroller.current.scrollHeight;},[conversation?.id,last?.id,last?.state]);
  const send=async()=>{if(busy||pending||!conversation||!question.trim())return;const q=question.trim(),cid=conversation.id;setBusy(true);
    try{await call({kind:'assistant_submit',conversation_id:cid,question:q,source:'text',utterance_id:crypto.randomUUID(),scope:scope==='selection'?`selection:${focusSegment??''}`:scope});if(cid===current.current){draft('');follow.current=true;await load(cid);}}catch(e){fail(String(e));}finally{setBusy(false);}
  };
  const action=async(operation:Record<string,unknown>)=>{setBusy(true);try{const result=await call<{meeting_id?:string}>(operation);await load();if(result.meeting_id)onMeeting?.(result.meeting_id);}catch(e){fail(String(e));}finally{setBusy(false);}};
  return <section className={`duozhou-chat ${embedded?'embedded-chat':'page-pad'}`} aria-label="小舵聊天会话">
    <header className="duozhou-heading"><div><p className="eyebrow">Duozhou Meeting Assistant</p><h1>{embedded?'小舵会议专家':'小舵助手'}</h1><p className="muted">{conversation?.meeting_id?'当前上下文：':'通用会话 · '}{conversation?.title??'会议与管理问题，和小舵聊一聊'}</p></div>{!embedded&&<div className="button-row"><button disabled={busy} onClick={()=>void open(true)}>通用聊天</button><select aria-label="切换小舵会话" value={conversation?.id??''} onChange={e=>{const row=rows.find(r=>r.id===e.target.value);if(row)void (async()=>{const d=await call<Conversation>(row.meeting_id?{kind:'assistant_open',meeting_id:row.meeting_id}:{kind:'assistant_detail',conversation_id:row.id});show(d);})().catch(e=>fail(String(e)));}}><option value="" disabled>历史会话</option>{rows.map(r=><option key={r.id} value={r.id}>{r.meeting_id?'会议 · ':''}{r.title}</option>)}</select></div>}</header>
    {!embedded&&activeMeetingId&&conversation?.meeting_id!==activeMeetingId&&<div className="banner"><span>有会议正在进行。当前会话保持原范围。</span><button onClick={()=>void open(false,activeMeetingId)}>切换当前会议</button></div>}
    {embedded&&setScope&&<div className="duozhou-range"><label>提问范围<select aria-label="提问范围" value={scope} onChange={e=>setScope(e.target.value)}><option value="all">本场全部原文</option><option value="recent">最近 10 分钟</option><option value="selection" disabled={!focusSegment}>选中段落</option></select></label></div>}
    <div className="duozhou-messages" ref={scroller} onScroll={e=>{const el=e.currentTarget;follow.current=el.scrollHeight-el.scrollTop-el.clientHeight<80;}} aria-live="polite">
      {!desktop?<Empty title="请在桌面应用中与小舵聊天">浏览器预览不调用模型。</Empty>:loading&&!conversation?<p role="status">正在打开会话…</p>:!conversation?.turns.length?<Empty icon="chat" title="把讨论中的问题交给小舵">{conversation?.meeting_id?'直接问本场目标、卡点、风险或下一步。':'可以一起准备会议、设计议程，或讨论管理方法。'}</Empty>:conversation.turns.map(t=>{const answer=readAssistantAnswer(t.answer);return <div className="duozhou-turn" key={t.id}>
        <div className="duozhou-user"><small>你{t.source==='voice'?' · 语音':''}</small><p className="prewrap">{t.question}</p></div>
        <div className="duozhou-answer"><strong>小舵</strong>{answer?.blocks.map((b,i)=><div className="duozhou-block" key={i}><small className={`tag ${b.kind==='suggestion'?'suggested':''}`}>{kinds[b.kind]??b.kind}</small><p className="prewrap">{b.text}</p>{b.sources.map((ref,n)=>{const s=answer?.evidence.find(s=>s.source_id===ref);return s?<details className="duozhou-citation" key={n}><summary>{s.kind==='meeting'?`会议原文 ${time(s.start_ms??0)} · r${s.revision}`:s.name??'资料依据'}{s.version?` · v${s.version}`:''}{s.stale?' · 原文已更新':''}{s.assistant_playback_overlap?' · 含助手播报重叠':''}</summary><p className="prewrap">{s.text}</p>{s.meeting_id&&onMeeting&&<button onClick={()=>onMeeting(s.meeting_id!)}>打开来源会议</button>}</details>:null;})}</div>)}{!answer&&<p className={t.state==='failed'?'error-text':'muted'}>{t.error??(t.state==='succeeded'?'回答显示格式异常，可重试或开始新话题。':states[t.state]??t.state)}</p>}{answer?.coverage&&answer.coverage.transcript_total>0&&<p className="micro">本轮取到 {answer.coverage.transcript_returned} / {answer.coverage.transcript_total} 段原文{answer.coverage.through_ms!=null?` · 转写截至 ${time(answer.coverage.through_ms)}`:''}{answer.coverage.transcript_returned<answer.coverage.transcript_total?' · 未完整读取全场':''}</p>}{t.model_name&&<small className="muted">{t.model_name}</small>}
          {conversation.proposals.filter(p=>p.turn_id===t.id&&t.state==='succeeded').map(p=><div className="duozhou-proposal" key={p.id}><strong>{p.payload.action==='prepare_meeting'?'会议准备草稿':'笔记草稿'} · {p.payload.title}</strong><p className="prewrap">{p.payload.text}</p><button disabled={busy||p.state!=='pending'} onClick={()=>void action({kind:'assistant_accept',proposal_id:p.id})}>{p.state==='accepted'?'已接受':p.payload.action==='prepare_meeting'?'接受并打开准备页':'接受并记为笔记'}</button></div>)}
        </div>
      </div>;})}
    </div>
    <div className="duozhou-quick">{(conversation?.meeting_id?['目前哪些目标尚未澄清？','有哪些卡点和风险？','用5Why帮我分析当前问题。']:['帮我设计一次方案评审议程。','怎样让周会形成明确行动？','帮我准备一个项目周会。']).map(q=><button disabled={busy||Boolean(pending)} key={q} onClick={()=>draft(q)}>{q}</button>)}</div>
    <form className="duozhou-compose" onSubmit={e=>{e.preventDefault();void send();}}><textarea aria-label="小舵问题" placeholder={conversation?.meeting_id?'围绕本场会议提问…':'和小舵聊一聊…'} value={question} maxLength={4000} onChange={e=>draft(e.target.value)} onKeyDown={e=>{if((e.ctrlKey||e.metaKey)&&e.key==='Enter'){e.preventDefault();void send();}}}/><div className="section-line"><button type="button" className="quiet" disabled={!conversation||busy} onClick={()=>void action({kind:'assistant_clear',conversation_id:conversation?.id})}>开始新话题</button><span className="micro">⌘ / Ctrl + Enter 发送</span>{pending?<button type="button" onClick={()=>void action({kind:'assistant_cancel',turn_id:pending.id})}>取消回答</button>:<button className="primary" disabled={!desktop||!conversation||busy||!question.trim()}>发送</button>}</div></form>
  </section>;
}
