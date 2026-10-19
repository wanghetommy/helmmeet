import { Component, useState, type ReactNode } from 'react';
import { call, type Detail, type Meeting } from './bridge';

type Target=Pick<Detail,'id'|'title'|'version'|'capture_epoch'>;
function RecoveryPage(){
 const [target,setTarget]=useState<Target>();const [busy,setBusy]=useState(false);const [message,setMessage]=useState('');
 const prepare=async()=>{setBusy(true);setMessage('');try{const rows=await call<Meeting[]>({kind:'list'});const active=rows.find(m=>['recording','paused','interrupted'].includes(m.state));if(!active){setMessage('当前没有正在录制的会议，可从应用菜单退出。');return;}const d=await call<Detail>({kind:'detail',meeting_id:active.id});setTarget({id:d.id,title:d.title,version:d.version,capture_epoch:d.capture_epoch});}catch{setMessage('暂时无法读取录制状态，请重试。');}finally{setBusy(false);}};
 const finish=async()=>{if(!target)return;setBusy(true);try{await call({kind:'finish_meeting',meeting_id:target.id,expected_version:target.version,capture_epoch:target.capture_epoch??''});setTarget(undefined);setMessage('录制已结束，已有音频已保存。可重新加载界面，或从应用菜单退出。');}catch{setTarget(undefined);setMessage('录制状态已变化或尚未结束，请重新核对后再试。');}finally{setBusy(false);}};
 return <main className="page-pad" role="alert"><section className="panel"><h1>界面显示异常</h1><p>录音可能仍在后台保存。重新加载界面不会停止录音。</p><div className="button-row"><button className="primary" onClick={()=>window.location.reload()}>重新加载界面</button><button disabled={busy} onClick={()=>void prepare()}>检查并结束录制…</button></div>{target&&<div role="group" aria-label="确认结束录制"><p>确认结束「{target.title}」？</p><div className="button-row"><button disabled={busy} onClick={()=>setTarget(undefined)}>继续录制</button><button className="danger" disabled={busy} onClick={()=>void finish()}>确认结束录制</button></div></div>}{message&&<p role="status">{message}</p>}</section></main>;
}
// A render failure must leave an independent path to seal recording and exit.
// Do not log model text, transcript, or raw error messages from failed rendering.
export class RenderRecovery extends Component<{children:ReactNode},{failed:boolean}>{
 state={failed:false};
 static getDerivedStateFromError(){return {failed:true};}
 render(){return this.state.failed?<RecoveryPage/>:this.props.children;}
}
