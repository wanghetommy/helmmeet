export type DialogueStatus={id:string;conversation_id:string;meeting_id:string|null;state:string;source:string};
export type VoiceStatus={state:string;wake_preview?:{id:number;state:string;speaker:string;cached?:boolean;error?:string|null};wake_ack?:{state:string;speaker?:string;available:boolean;refreshing:boolean;error?:string|null};can_enable?:boolean;reason?:string;meeting_id?:string;turn_id?:string;message?:string;question?:string;wake_word?:string;input_level?:number;audio_playing?:boolean;continuing?:boolean;outcome?:string;updated_at_ms?:number;dialogue?:DialogueStatus|null;interruption?:{available:boolean;reason:string}|null;speech?:{provider:'api'|'local'|'none';error?:string|null}|null};
export type AssistantDisplay={kind:string;title:string;detail:string;icon:string;motion:'none'|'breathe'|'input'|'thinking'|'output';level?:number};
export function speechDescription(value:VoiceStatus){return value.speech?.provider==='api'?'火山 TTS':value.speech?.provider==='local'?(value.speech.error?'本机声音（API 失败后回退）':'本机声音'):'未启动播报';}
const display=(kind:string,title:string,detail:string,icon:string,motion:AssistantDisplay['motion']='none'):AssistantDisplay=>({kind,title,detail,icon,motion});
export function assistantDisplay(value:VoiceStatus,name:string,completedTurn?:string,now=Date.now()):AssistantDisplay {
 const interrupt=value.interruption?.available===false?'播放参考不足，请点击“打断并提问”':`可再次说“${name}”或点击打断并提问`;
 if(value.state==='listening')return {...display('listening','正在听你讲话','请说出问题，停顿后自动提交','mic','input'),level:Number.isFinite(value.input_level)?Math.max(0,Math.min(1,value.input_level!)):0};
 if(value.state==='testing_wake')return display('testing','正在测试唤醒词',`请说“${name}”，本次只检查唤醒`,'ear','breathe');
 if(value.state==='transcribing')return display('thinking','正在思考','正在理解问题，可打断并重新提问','thought','thinking');
 if(value.state==='thinking')return display('thinking','正在思考','正在组织回答，可打断并重新提问','thought','thinking');
 if(value.state==='synthesizing'||value.state==='speaking'&&value.audio_playing===false)return value.continuing
  ?display('answering','正在回答（语音）','正在准备下一段语音，文字已可查看','speaker')
  :display('preparing','正在思考（语音）','正在准备语音，文字已可查看','thought-audio','thinking');
 if(value.state==='speaking')return display('answering','正在回答（语音）',`${speechDescription(value)} · ${interrupt}`,'speaker','output');
 const dialogue=value.dialogue;
 if(dialogue&&['pending','running'].includes(dialogue.state))return display('thinking','正在思考',dialogue.state==='pending'?'正在等待回答，可取消':'正在组织回答，可取消','thought','thinking');
 if(['answered','spoken'].includes(value.outcome??'')&&value.updated_at_ms!=null&&now>=value.updated_at_ms&&now-value.updated_at_ms<1800)return display('completed',value.outcome==='spoken'?'回答完毕':'已回答','完整文字已保留在聊天中','chat-check');
 if(!['stopped','speech_failed'].includes(value.outcome??'')&&completedTurn&&dialogue?.id===completedTurn&&dialogue.state==='succeeded')return display('completed','已回答','完整回答已保留在聊天中','chat-check');
 if(value.state==='error')return display('error','语音暂不可用',value.message??'请重试或使用文字提问','alert');
 if(value.state==='standby')return display('hearing','正在听会',value.message&&value.message!=='待唤醒'?value.message:`正在监听，说“${name}”提问`,'ear','breathe');
 const inactive:Record<string,[string,string]>={disabled:['语音未开启','开启后可在会议中唤醒提问'],meeting_paused:['听会已暂停','继续会议后恢复监听'],microphone_disabled:['麦克风已关闭','恢复会议麦克风后可继续听会'],locked:['听会已暂停','锁屏期间停止语音监听'],globally_paused:['听会已暂停','全局模型分析已暂停'],no_microphone:['暂无可用麦克风','会议录制且麦克风开启时可听会'],status_unavailable:['状态暂不可用','正在重新获取状态，可继续查看聊天']};
 const text=inactive[value.reason??'']??['语音未开启','会议录制且麦克风开启时可听会'];
 return display('inactive',text[0]!,text[1]!,'mic-off');
}
