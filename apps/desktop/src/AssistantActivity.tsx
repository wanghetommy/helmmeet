import {createContext,useCallback,useContext,useEffect,useMemo,useRef,useState,type CSSProperties,type ReactNode} from 'react';
import {call,desktop} from './bridge';
import {assistantDisplay,type AssistantDisplay,type VoiceStatus} from './assistantStatus';
import {useAssistantIdentity} from './AssistantIdentity';
import {Icon} from './ui';
type Actions={refresh:()=>Promise<void>;trackTurn:(id:string)=>void};
const ActionsContext=createContext<Actions|null>(null);
const ActivityContext=createContext<{voice:VoiceStatus;display:AssistantDisplay;refresh:()=>Promise<void>;trackTurn:(id:string)=>void}|null>(null);
export function AssistantActivityProvider({children}:{children:ReactNode}){
 const {name}=useAssistantIdentity();const [voice,setVoice]=useState<VoiceStatus>({state:'off'});const [completed,setCompleted]=useState<string>();const [,expire]=useState(0);
 const mounted=useRef(false),serial=useRef(0),tracked=useRef(new Set<string>()),prior=useRef<VoiceStatus|undefined>(undefined);const completedAt=useRef(0),lastSuccess=useRef(Date.now());
 const refresh=useCallback(async()=>{if(!desktop)return;const ticket=++serial.current;
  try{const next=await call<VoiceStatus>({kind:'assistant_voice_status'});if(!mounted.current||ticket!==serial.current)return;
   if(tracked.current.size>64)tracked.current.clear();
   const old=prior.current?.dialogue;if(old&&['pending','running'].includes(old.state))tracked.current.add(old.id);
   const turn=next.dialogue;if(turn&&['pending','running'].includes(turn.state))tracked.current.add(turn.id);
   if(turn&&!['pending','running'].includes(turn.state)){const observed=tracked.current.delete(turn.id);if(turn.state==='succeeded'&&observed){setCompleted(turn.id);completedAt.current=Date.now();}}
   if(next.state==='listening'||next.state==='error'||next.reason==='meeting_paused'||next.reason==='microphone_disabled'){setCompleted(undefined);tracked.current.clear();}
   if(JSON.stringify(prior.current)!==JSON.stringify(next))setVoice(next);prior.current=next;lastSuccess.current=Date.now();
  }catch{if(mounted.current&&ticket===serial.current&&Date.now()-lastSuccess.current>3000){prior.current=undefined;setVoice({state:'off',reason:'status_unavailable'});setCompleted(undefined);}}
 },[]);
 useEffect(()=>{mounted.current=true;let stopped=false;let timer:ReturnType<typeof setTimeout>;const poll=async()=>{await refresh();if(!stopped)timer=setTimeout(poll,250);};void poll();return()=>{stopped=true;mounted.current=false;serial.current++;clearTimeout(timer);};},[refresh]);
 useEffect(()=>{if(!completed)return;const timer=setTimeout(()=>setCompleted(undefined),Math.max(0,1800-(Date.now()-completedAt.current)));return()=>clearTimeout(timer);},[completed]);
 useEffect(()=>{if(!voice.outcome||!voice.updated_at_ms)return;const remaining=voice.updated_at_ms+1800-Date.now();if(remaining<=0)return;const timer=setTimeout(()=>expire(v=>v+1),remaining);return()=>clearTimeout(timer);},[voice.outcome,voice.updated_at_ms]);
 const trackTurn=useCallback((id:string)=>{tracked.current.add(id);setCompleted(undefined);void refresh();},[refresh]);
 const actions=useMemo(()=>({refresh,trackTurn}),[refresh,trackTurn]);
 return <ActionsContext.Provider value={actions}><ActivityContext.Provider value={{voice,display:assistantDisplay(voice,name,completed),refresh,trackTurn}}>{children}</ActivityContext.Provider></ActionsContext.Provider>;
}
export function useAssistantActions(){const value=useContext(ActionsContext);if(!value)throw Error('Missing assistant activity provider');return value;}
export function useAssistantActivity(){const value=useContext(ActivityContext);if(!value)throw Error('Missing assistant activity provider');return value;}
export function AssistantStatusIcon({value,size=18}:{value:AssistantDisplay;size?:number}){
 return <span aria-hidden="true" className={`assistant-status-icon motion-${value.motion}`} style={{'--input-level':value.level??0} as CSSProperties}><Icon name={value.icon} size={size}/>{value.motion==='thinking'&&<span className="assistant-thinking-dots"><i/><i/><i/></span>}{value.motion==='input'&&<span className="assistant-input-meter"><i/><i/><i/></span>}</span>;
}
export function AssistantStatusLine(){const {display}=useAssistantActivity();return <div className={`assistant-status-line status-${display.kind}`} role="status" aria-live="polite"><AssistantStatusIcon value={display}/><span><strong>{display.title}</strong><small>{display.detail}</small></span></div>;}

export function AssistantVoiceInput({disabled,start,fail}:{disabled:boolean;start:()=>Promise<void>;fail:(message:string)=>void}){
 const {voice,refresh}=useAssistantActivity();const [busy,setBusy]=useState(false);
 const command=async(action:string)=>{setBusy(true);try{await call({kind:'assistant_voice_command',action});await refresh();}catch(e){fail(String(e));}finally{setBusy(false);}};
 if(voice.state==='error')return <button type="button" className="quiet" disabled={disabled||busy} onClick={()=>void command('cancel')}>重试语音</button>;
 if(voice.state==='listening')return <><button type="button" className="quiet" disabled={disabled||busy} onClick={()=>void command('cancel')}>取消提问</button><button type="button" disabled={disabled||busy} onClick={()=>void command('submit')}>提交问题</button></>;
 const interrupt=['thinking','transcribing','synthesizing','speaking'].includes(voice.state);
 return <button type="button" className="quiet voice-input-button" disabled={disabled||busy||voice.state==='off'} title={voice.state==='off'?'请先在助手设置开启语音唤醒':interrupt?'停止当前回答，接收新问题':'开始说出你的问题'} onClick={()=>void start()}><Icon name="mic" size={14}/>{interrupt?'打断并提问':'语音输入'}</button>;
}
