"""Serve the production bundle with clearly named synthetic IPC fixtures for UI QA.
Never opens user data, audio devices, clipboard or a remote model. No browser control.
Build: npm run build. Run: python3 tests/integration/check_workspace_ui.py --port 1421
Open the printed local URL with the supported browser tool; see product validation doc.
"""
import argparse
from http.server import SimpleHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path
ROOT = Path(__file__).resolve().parents[2]
BOOT = r'''
window.isTauri=true;
const a='11111111-1111-4111-8111-111111111111',b='22222222-2222-4222-8222-222222222222';
const ref={segment_id:'s1',revision:2};
const fact={text:'合成会议正在评审音频采集方案。',kind:'discussion',section:'topic',evidence:[ref]};
const artifact=(id,type,version)=>({id,type,version,status:'draft',content:{summary:[{...fact,section:type==='minutes'?'minutes':'topic'}],actions:[]},coverage:{to_ms:60000,incomplete:false,profile_name:'本地假模型',config_version:1}});
const details={};let defaultNS='off';
for(const [id,title,state] of [[a,'合成会议 A','recording'],[b,'合成会议 B','completed']]) details[id]={id,title,state,enhancement:{available:true,mode:'low',echo_cancellation:true,aec_applied_ms:44000,aec_bypass_ms:16000,aec_status:{state:'applied',reason:'reference_valid',delay_ms:0},state:state==='completed'?'complete':'processing',processed_ms:60000,total_ms:60000,playable_through_ms:60000,error:null,generation:1,engine:'fixture-v1'},duration_ms:60000,created_at:'2026-10-18T00:00:00Z',last_error:null,version:1,consent_version:1,model_profile:null,sources:[],context:{},proposals:[],segments:[{id:'s1',start_ms:0,end_ms:30000,revision:2,text:'人工修订的测试文字。',state:'edited'},{id:'s2',start_ms:30000,end_ms:60000,revision:1,text:'第二段测试文字。',state:'final'}],artifacts:[artifact('summary','stage',3),artifact('minute2','minutes',2),artifact('minute1','minutes',1)],actions:[{id:'todo',task:'核对采集方案',version:1,owner_text:null,due_text:null,confirmation_state:'pending',execution_state:'todo'}],notes:[],jobs:[],analysis_mode:'local',auto_summary_minutes:0,assistant_id:'review',source_mask:1,transient:{},coverage:[],template:{id:'review',name:'方案评审',version:1,instructions:'目标与约束、方案权衡、风险和验证条件'},schedule:{enabled:false,mode:'realtime',version:1,failures:0,paused_reason:null},context_consent:true,conversation:[],legacy_answer_ids:[],summary_pending_segments:1};
const c='33333333-3333-4333-8333-333333333333';details[c]={...structuredClone(details[b]),id:c,title:'合成准备 C',state:'ready',duration_ms:0,segments:[],artifacts:[],enhancement:{...details[b].enhancement,mode:'off',echo_cancellation:false,aec_applied_ms:0,aec_bypass_ms:0,aec_status:{state:'waiting',reason:'waiting_for_audio',delay_ms:null},state:'queued',processed_ms:0,total_ms:0,playable_through_ms:0}};
details[a].jobs=[{id:'new-success',type:'stage',state:'succeeded',error:null},{id:'old-json-failure',type:'stage',state:'failed',error:'模型结构化结果无效'}];
details[b].jobs=[{id:'latest-json-failure',type:'final_map',state:'failed',error:'模型结构化结果无效'}];
const templatePolicy={goal:'合成目标：评估方案可行性。',criteria:'合成标准：目标、约束与风险证据。',qa_rules:'合成问答：先判断，再给验证建议。'};
for(const d of Object.values(details))d.template.policy=structuredClone(templatePolicy);
const templates=[{id:'general',name:'通用会议',version:1,instructions:'核对目标、结论和行动',policy:{...templatePolicy}},{id:'weekly',name:'周会',version:1,instructions:'进展、卡点与下周计划',policy:{...templatePolicy}},structuredClone(details[b].template)];
const assistantConversations={};let assistantPreferences={behavior:{persona:{personality:'合成性格：坦诚温和。',expression:'合成表达风格：短句；惯用语按需使用“关键在于……”。',thinking_models:'合成思维模型：假设与证据。',expertise:'合成知识与经验：项目管理与复盘实践。'},default_persona:{personality:'合成性格：坦诚温和。',expression:'合成表达风格：短句；惯用语按需使用“关键在于……”。',thinking_models:'合成思维模型：假设与证据。',expertise:'合成知识与经验：项目管理与复盘实践。'},version:1,persona_prompt:'合成测试人设：直接简洁回答。',default_persona_prompt:'合成默认人设。',simple_seconds:10,medium_seconds:20,complex_seconds:30,conclusion_first:true,one_question:true},voice_enabled:false,wake_word:'小舵同学',silence_ms:1200,speak_answers:true,collections:[],tts:{enabled:false,profile_id:null,config_version:null,model:'seed-tts-2.0-standard',speaker:'zh_female_gaolengyujie_uranus_bigtts',fallback_local:true}};let voiceState={state:'off',meeting_id:a,can_enable:true,reason:'disabled',message:'语音唤醒未开启'};
const assistantOpen=(mid,general)=>{mid=general?null:mid??Object.values(details).find(m=>['recording','paused','interrupted'].includes(m.state))?.id;const key=mid??'general';if(!assistantConversations[key])assistantConversations[key]={id:mid??'general-chat',meeting_id:mid??null,title:mid?details[mid].title:'与小舵聊一聊',turns:[],proposals:[]};const conv=assistantConversations[key];if(mid){conv.meeting_version=details[mid].version;conv.template={...structuredClone(details[mid].template),latest_version:templates.find(t=>t.id===details[mid].template.id)?.version??1};}return conv};
const preparationFixture=structuredClone(details[c]);
window.testCalls=[];
window.testAudioStarts=[];
const NativeAudioContext=window.AudioContext;
window.AudioContext=class extends NativeAudioContext {
 createBufferSource(){const node=super.createBufferSource();const start=node.start.bind(node);node.start=(at)=>{window.testAudioStarts.push({at,duration:node.buffer.duration});start(at);};return node;}
};
const profiles=[{profile_id:'p1',config_version:1,display_name:'合成本地模型',base_url:'http://127.0.0.1:11434/v1',model:'fixture',credential_valid:true,has_api_key:false,archived:false,is_local:true,latest_version:1,bound_meetings:3,context_tokens:32768,max_output_tokens:4096,timeout_seconds:10,last_test:null}];
let prefs={analysis_paused:false,default_source_mask:1,default_input_device:0,debug_mode:false};
const collections=[{id:'k1',name:'合成项目词库',kind:'terms',version:1,archived:false,term_count:2,characters:0,bound_meetings:0},{id:'k2',name:'合成项目背景',kind:'documents',version:1,archived:false,term_count:0,characters:32,bound_meetings:0}];
for(const d of Object.values(details)){d.paragraphs=d.segments.length?[{id:'p-'+d.segments[0].id,start_ms:d.segments[0].start_ms,end_ms:d.segments.at(-1).end_ms,text:d.segments.map(s=>s.text).join(''),segment_ids:d.segments.map(s=>s.id),members:d.segments.map(s=>({segment_id:s.id,revision:s.revision})),open:d.state==='recording',reason:'growing',boundary_source:'fixture'}]:[];d.model_profile=profiles[0];d.capture_epoch='epoch-'+d.id;d.knowledge=[];}
const capabilities=d=>({phase:d.state==='ready'?'preparation':['recording','paused','interrupted'].includes(d.state)?'live':'after',has_audio:d.duration_ms>0,has_confirmed_text:!!d.segments.length,has_minutes:!!d.artifacts.find(a=>a.type==='minutes'),has_actions:!!d.actions.length,model_ready:d.analysis_mode!=='transcript_only',can_analyze:d.state!=='ready'&&!!d.segments.length&&d.analysis_mode!=='transcript_only'&&!prefs.analysis_paused,analysis_paused:prefs.analysis_paused});
const syntheticCallbacks=new Map(),syntheticListeners=new Map();let syntheticCallbackId=0,syntheticListenerId=0;
window.emitSyntheticEvent=(name,payload)=>{for(const [id,row] of syntheticListeners)if(row.event===name)syntheticCallbacks.get(row.handler)?.({event:name,id,payload});};
window.__TAURI_INTERNALS__={transformCallback:fn=>{const id=++syntheticCallbackId;syntheticCallbacks.set(id,fn);return id;},unregisterCallback:id=>syntheticCallbacks.delete(id),invoke:async(cmd,args)=>{
 if(cmd==='plugin:event|listen'){const id=++syntheticListenerId;syntheticListeners.set(id,args);return id;}if(cmd==='plugin:event|unlisten'){syntheticListeners.delete(args.eventId);return;}

 if(cmd==='launch_request'){const pending=window.syntheticLaunchRequests??[];if(args?.acknowledge===pending[0]?.id)pending.shift();return structuredClone(pending[0]??null);}
 if(cmd!=='dispatch')throw Error('Unexpected outer IPC');
 const op=args.operation;testCalls.push(op);const d=details[op.meeting_id];let result;
 switch(op.kind){
 case 'list':result=Object.values(details);break;
 case 'detail':d.capabilities=capabilities(d);result=d;break;
 case 'settings':result={startup_warning:null,model_file:'fixture.bin',asr_configured:true,language:'zh',audio_enhancement_available:true,audio_enhancement_default:defaultNS,product:prefs,data_directory:'[隔离合成数据目录]',app_version:'0.1.0-test'};break;
 case 'set_audio_enhancement_default':defaultNS=op.mode;result={saved:true};break;
 case 'set_audio_enhancement':if(d.state!=='ready')throw Error('frozen');d.enhancement.mode=op.mode;if(op.echo_cancellation!==undefined)d.enhancement.echo_cancellation=op.echo_cancellation;result={saved:true};break;
 case 'export':result={exported:true};break;
 case 'model_profiles':result={profiles,default_profile_id:'p1'};break;
 case 'templates':result=templates;break;
 case 'save_template':{let t=templates.find(t=>t.id===op.id);if(t&&t.version!==op.expected_version)throw Error('模板已更新');const next={id:op.id??crypto.randomUUID(),version:(t?.version??0)+1,name:op.name,instructions:op.instructions,policy:op.policy};if(t)Object.assign(t,next);else templates.push(next);result=next;break;}
 case 'adopt_template':if(d.version!==op.expected_version)throw Error('会议版本已变化');d.template=structuredClone(templates.find(t=>t.id===d.template.id&&t.version===op.template_version));d.version++;result={adopted:true};break;
 case 'devices':result=[];break;
 case 'read_transcript_snapshot':result={meeting_id:d.id,title:d.title,source:op.source,timestamps:op.timestamps,through_ms:60000,hash:'fixture-hash',paragraphs:d.paragraphs.map(p=>({...p,text:p.segment_ids.map((id,i)=>op.source==='original'&&i===0?'原始识别文本':d.segments.find(s=>s.id===id).text).join('')})),text:op.source==='original'?'原始识别文本\n第二段测试文字。':'人工修订的测试文字。\n第二段测试文字。',segments:d.segments.map((s,i)=>({segment_id:s.id,start_ms:s.start_ms,revision:op.source==='original'?1:s.revision,text:op.source==='original'&&i===0?'原始识别文本':s.text}))};break;
 case 'playback':{const end=Math.min(op.start_ms+30000,d.duration_ms);result={start_ms:op.start_ms,end_ms:end,samples:Array.from({length:(end-op.start_ms)*16},(_,i)=>Math.round(2000*Math.sin(2*Math.PI*440*(op.start_ms*16+i)/16000)))};break;}
 case 'copy_transcript':result={meeting_id:d.id,title:d.title,through_ms:60000,segments:d.segments};break;
 case 'set_summary_schedule':Object.assign(d.schedule,{enabled:op.enabled,mode:op.mode,version:d.schedule.version+1});result={saved:true};break;
 case 'assistant_open':result=assistantOpen(op.meeting_id,op.general);break;
 case 'assistant_list':result=[...Object.values(details).filter(d=>d.state!=='ready').map(d=>({id:d.id,meeting_id:d.id,title:d.title})),...Object.values(assistantConversations).filter(c=>!c.meeting_id).map(c=>({id:c.id,meeting_id:null,title:c.title}))];break;
 case 'assistant_detail':result=Object.values(assistantConversations).find(c=>c.id===op.conversation_id);if(!result)throw Error('合成会话不存在');if(result.meeting_id)result=assistantOpen(result.meeting_id,false);break;
 case 'assistant_submit':{const c=Object.values(assistantConversations).find(c=>c.id===op.conversation_id);const id=crypto.randomUUID();c.turns.push({id,question:op.question,source:op.source,state:'succeeded',model_name:'合成假模型',policy:{answer_mode:op.mode??'auto',actual_mode:op.mode==='auto'?'analyze':op.mode,mode_label:({auto:'分析',analyze:'分析',clarify:'澄清',diverge:'发散',judge:'判断',converge:'收敛'})[op.mode??'auto'],meeting_template:structuredClone(c.template),persona_prompt:assistantPreferences.behavior.persona_prompt},answer:{blocks:[{kind:'suggestion',text:'建议先澄清目标的验收条件，再讨论行动。',sources:['method:duozhou-v1']}],evidence:[{source_id:'method:duozhou-v1',kind:'method',name:'小舵会议方法 v1',text:'目标需核对指标、基线和验收条件。'}]}});result={turn_id:id};break;}
 case 'assistant_clear':Object.values(assistantConversations).find(c=>c.id===op.conversation_id).turns=[];result={cleared:true};break;
 case 'assistant_cancel':result={cancelled:true};break;
 case 'assistant_settings':result=assistantPreferences;break;
 case 'save_assistant_settings':assistantPreferences=op.settings;voiceState=op.settings.voice_enabled?{state:'standby',meeting_id:a,wake_word:op.settings.wake_word,message:'待唤醒'}:{state:'off',meeting_id:a,can_enable:true,reason:'disabled',message:'语音唤醒未开启'};result=assistantPreferences;break;
 case 'assistant_voice_status':{const turns=Object.values(assistantConversations).flatMap(c=>c.turns.map(t=>({...t,conversation_id:c.id,meeting_id:c.meeting_id})));const latest=turns.filter(t=>['pending','running'].includes(t.state)).at(-1)??turns.at(-1);result={...voiceState,dialogue:voiceState.dialogue??(latest?{id:latest.id,conversation_id:latest.conversation_id,meeting_id:latest.meeting_id,state:latest.state,source:latest.source}:null)};break;}
 case 'assistant_voice_command':if(op.action==='listen')voiceState={state:'listening',meeting_id:a,message:'合成输入：请说话，停顿后自动提交'};else if(op.action==='cancel')voiceState={state:'standby',meeting_id:a,message:'合成输入已取消'};else if(op.action==='submit'){const c=assistantOpen(a);c.turns.push({id:crypto.randomUUID(),source:'voice',question:'合成语音问题：目前目标是什么？',state:'succeeded',answer:{blocks:[{kind:'suggestion',text:'建议先澄清验收条件。',sources:[]}],evidence:[]}});voiceState={state:'standby',meeting_id:a,message:'合成语音回合已写入聊天'};}result={accepted:true};break;
 case 'prepare_asr':await new Promise(resolve=>setTimeout(resolve,6000));result={ready:true};break;
 case 'delete':if(['recording','paused','interrupted'].includes(d.state))throw Error('请先结束会议');delete details[op.meeting_id];result={deleted:true};break;
 case 'record':if(op.action==='stop')throw Error('结束必须确认');d.state=op.action==='pause'?'paused':'recording';d.version++;if(op.action!=='pause')d.capture_epoch+='-new';d.capabilities=capabilities(d);result=d;break;
 case 'finish_meeting':if(op.expected_version!==d.version||op.capture_epoch!==d.capture_epoch)throw Error('录制会话已变化');d.state='finalizing';d.version++;d.capabilities=capabilities(d);result=d;break;
 case 'save_product_preferences':prefs={analysis_paused:op.analysis_paused,default_source_mask:op.default_source_mask,default_input_device:op.default_input_device,debug_mode:op.debug_mode};result={saved:true};break;
 case 'save_preparation':if(d.state!=='ready'||d.version!==op.expected_version)throw Error('准备版本已变化');Object.assign(d,{title:op.title,assistant_id:op.assistant_id,source_mask:op.source_mask,context:op.context,version:d.version+1});result={saved:true};break;
 case 'create':{const id=crypto.randomUUID();details[id]={...structuredClone(preparationFixture),id,title:op.title,assistant_id:op.assistant_id,actions:[],notes:[],state:'ready',version:1,analysis_mode:'local',model_profile:profiles[0]};result={id};break;}
 case 'follow_up':{const id=crypto.randomUUID();details[id]={...structuredClone(preparationFixture),id,title:d.title+' · 续会',parent_meeting_id:d.id,context:d.context,version:1};result={id};break;}
 case 'knowledge_reindex':result={rebuilt:true};break;
 case 'knowledge_list':result=collections.filter(k=>op.include_archived||!k.archived);break;
 case 'knowledge_get':{const k=collections.find(k=>k.id===op.set_id);result={...k,content:k.kind==='documents'?'合成项目背景，只用于隔离测试。':'',terms:k.kind==='terms'?[{id:'entity1',canonical:'王鹤',aliases:['王贺'],category:'人名'}]:[]};break;}
 case 'knowledge_save':{let k=collections.find(k=>k.id===op.collection.id);if(k){k.version++;k.name=op.collection.name;}else collections.push({id:crypto.randomUUID(),name:op.collection.name,kind:op.collection.kind,version:1,archived:false,term_count:op.collection.terms.length,characters:op.collection.content.length,bound_meetings:0});result={saved:true};break;}
 case 'knowledge_archive':collections.find(k=>k.id===op.set_id).archived=op.archived;result={saved:true};break;
 case 'bind_knowledge':d.knowledge=op.bindings.map(k=>({...k,...collections.find(s=>s.id===k.set_id),version:k.version,latest_version:1}));d.version++;result={saved:true};break;
 case 'switch_meeting_model':d.model_profile=profiles.find(p=>p.profile_id===op.profile_id);d.analysis_mode=op.enabled?'local':'transcript_only';d.version++;result={saved:true};break;
 case 'consent':d.analysis_mode=op.enabled?'local':'transcript_only';d.version++;result={saved:true};break;
 case 'preview_audio':result={levels:{microphone:0.03}};break;
 case 'confirm_artifact':d.artifacts.find(a=>a.id===op.artifact_id).status='confirmed';result={confirmed:true};break;
 case 'copy_artifact':result={copied:true};break;
 case 'note':d.notes.push({id:crypto.randomUUID(),time_ms:op.time_ms,text:op.text});result={saved:true};break;
 case 'evidence':result={text:'合成历史依据原文',revision:op.revision,start_ms:0};break;
 case 'preview_normalization':result={changes:[],conflicts:[],events:[],notice:'隔离测试中没有需要修订的词条'};break;

 case 'analyze':{const type=op.analysis_type==='stage'?'stage':op.analysis_type==='minutes'?'final_map':'qa';const id=crypto.randomUUID();d.jobs.unshift({id,type,state:'succeeded',error:null});if(op.analysis_type==='answer')d.conversation.push({job_id:id,question:op.question,scope:op.scope,artifact_id:null,state:'succeeded',error:null});else d.artifacts.unshift(artifact(id,op.analysis_type,4));result={job_id:id};break;}
 default:throw Error('Unexpected operation '+op.kind);
 }
 return structuredClone(result);
}};

'''

def main():
    parser=argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--port',type=int,default=1421)
    parser.add_argument('--scenario',choices=['normal','tts-voices','reading-paragraphs','assistant-focus','assistant-status','assistant-interruption','voice-lifecycle','render-recovery','minutes-overlap','launch-cold','launch-active','launch-edit'],default='normal')
    args=parser.parse_args()
    dist=ROOT/'apps/desktop/dist'
    scenario = ""
    if args.scenario in ('launch-cold', 'launch-active'):
        scenario = "window.syntheticLaunchRequests=[{id:'launch-fixture-1',title:'链接合成周会 A&B',meeting_type:'weekly'}];"
        if args.scenario == 'launch-cold':
            scenario += "details[a].state='completed';details[a].capabilities=capabilities(details[a]);"
    elif args.scenario == 'launch-edit':
        scenario = """
details[a].state='completed';details[a].capabilities=capabilities(details[a]);let syntheticLaunchId=0;
document.addEventListener('DOMContentLoaded',()=>{const b=document.createElement('button');b.textContent='发送合成新建请求';b.onclick=()=>{(window.syntheticLaunchRequests??=[]).push({id:'warm-'+(++syntheticLaunchId),title:'网页合成新会议',meeting_type:'weekly'});window.emitSyntheticEvent('launch-link',null);};document.body.prepend(b);});
"""
    elif args.scenario == 'tts-voices':
        scenario = """
assistantPreferences.tts.enabled=true;assistantPreferences.tts.profile_id='p1';assistantPreferences.tts.config_version=1;
voiceState.wake_ack={state:'ready',speaker:assistantPreferences.tts.speaker,available:true,refreshing:false,error:null};
const ttsInvoke=window.__TAURI_INTERNALS__.invoke;
const prepareSyntheticAck=()=>{const speaker=assistantPreferences.tts.speaker;voiceState.wake_ack={state:'preparing',speaker,available:false,refreshing:true,error:null};setTimeout(()=>{voiceState.wake_ack={state:'ready',speaker,available:true,refreshing:false,error:null}},700)};
window.__TAURI_INTERNALS__.invoke=async(cmd,args)=>{
 const op=args?.operation;
 if(cmd==='dispatch'&&op.kind==='preview_wake_ack'){
  if(!op.tts){if(op.expected_id==null||voiceState.wake_preview?.id===op.expected_id){voiceState.wake_preview={...voiceState.wake_preview,state:'cancelled'};}return {accepted:true};}
  const id=op.selection_order;const cached=['zh_female_xiaohe_uranus_bigtts','zh_female_kefunvsheng_uranus_bigtts'].includes(op.tts.speaker);
  voiceState.wake_preview={id,speaker:op.tts.speaker,state:cached?'playing':'preparing',cached};
  const play=()=>{if(voiceState.wake_preview?.id!==id||voiceState.wake_preview.state==='cancelled')return;voiceState.wake_preview.state='playing';setTimeout(()=>{if(voiceState.wake_preview?.id===id&&voiceState.wake_preview.state==='playing')voiceState.wake_preview.state='done'},2400)};
  if(cached)play();else setTimeout(play,1600);return {accepted:true,id};
 }
 if(cmd==='dispatch'&&op.kind==='assistant_voice_command'&&op.action==='refresh_ack'){prepareSyntheticAck();return {accepted:true};}
 const result=await ttsInvoke(cmd,args);
 if(cmd==='dispatch'&&op.kind==='save_assistant_settings')prepareSyntheticAck();
 return result;
};
document.addEventListener('DOMContentLoaded',()=>{const button=document.createElement('button');button.textContent='合成回应失败';button.style='position:fixed;left:12px;bottom:100px;z-index:100';button.onclick=()=>{if(voiceState.wake_preview)voiceState.wake_preview={...voiceState.wake_preview,state:'failed',error:'合成测试：音色不可用'};voiceState.wake_ack={state:'failed',speaker:assistantPreferences.tts.speaker,available:false,refreshing:false,error:'合成测试：音色不可用'}};document.body.append(button)});
"""
    elif args.scenario == 'reading-paragraphs':
        scenario = """
const readingBlocks=[
'我们先看本周营销方案的目标。本次讨论需要明确目标客户、投放渠道和预算安排，同时把当前已有的数据与还需要验证的假设分开。初步建议先测试两个渠道，分别观察咨询量、有效线索和转化成本，再根据同一周期的数据判断是否继续投入，避免只凭曝光量就扩大预算。',
'预算方面，可以先保留一部分作为测试费用，剩余资金等首轮结果出来后再决定分配。每个渠道需要使用相同的统计口径，并明确数据的记录时间和复盘方式。如果其中一个渠道咨询很多但实际转化较少，应先核对客户匹配度和沟通流程，再讨论是否调整内容或停止投放。',
'下一步先整理现有客户反馈和渠道数据，再补齐测试方案中的验收条件。本次讨论尚未确认具体负责人和截止日期，因此暂时只记录待讨论事项。后续会议可以围绕测试结果比较不同方案的收益、风险和执行成本，形成有证据的选择，并明确哪些问题需要继续观察。'];
const rawBlocks=readingBlocks.map(t=>t.replaceAll('营销','营削'));
const p=details[a].paragraphs[0];p.text=readingBlocks.join('');p.raw_text=rawBlocks.join('');p.reading_blocks=readingBlocks;p.raw_reading_blocks=rawBlocks;p.calibration={status:'needs_review',changes:[]};
details[a].segments[0].text=rawBlocks[0];details[a].segments[1].text=rawBlocks.slice(1).join('');
details[a].calibration={enabled:true,mode:'standard',pending:0,succeeded:0,needs_review:1,failed:0};
const readingInvoke=window.__TAURI_INTERNALS__.invoke;
window.__TAURI_INTERNALS__.invoke=async(cmd,args)=>{
 const op=args?.operation;
 if(cmd==='dispatch'&&['read_transcript_snapshot','copy_transcript'].includes(op.kind)){
  const blocks=op.source==='original'?rawBlocks:readingBlocks;
  return {meeting_id:a,title:details[a].title,source:op.source,timestamps:op.timestamps,through_ms:60000,hash:'reading-fixture-hash',segments:details[a].segments,paragraphs:[{...p,text:blocks.join(''),reading_blocks:blocks}],text:(op.timestamps?'[00:00] ':'')+blocks.join('\\n\\n')};
 }
 if(cmd==='dispatch'&&op.kind==='confirm_calibration'){p.calibration.status='succeeded';details[a].calibration.needs_review=0;details[a].calibration.succeeded=1;return {confirmed:true,paragraph_id:p.id};}
 return readingInvoke(cmd,args);
};
"""
    elif args.scenario == 'assistant-focus':
        scenario = """
assistantPreferences.voice_enabled=true;
const focusStates=[{state:'standby',message:'待唤醒'},{state:'listening',input_level:.4},{state:'thinking'},{state:'synthesizing'},{state:'speaking',audio_playing:true,speech:{provider:'api'}}];
let focusPhase=0,focusLosses=0;
voiceState={meeting_id:a,...focusStates[0]};
document.addEventListener('DOMContentLoaded',()=>{
 const report=document.createElement('output');report.setAttribute('aria-label','合成输入焦点验收');report.style='position:fixed;left:12px;bottom:100px;z-index:100;background:white;border:1px solid #aaa;padding:8px;font-size:12px;max-width:190px';document.body.append(report);
 document.addEventListener('focusout',event=>{if(event.target.matches?.('.duozhou-compose textarea'))focusLosses++;});
 setInterval(()=>{voiceState={meeting_id:a,...focusStates[++focusPhase%focusStates.length]};report.textContent=`合成焦点回归：工作区刷新${testCalls.filter(c=>c.kind==='list').length}次；会话刷新${testCalls.filter(c=>c.kind==='assistant_detail').length}次；输入离焦${focusLosses}次；当前焦点${document.activeElement?.getAttribute('aria-label')??document.activeElement?.tagName}`;},600);
});
"""
    elif args.scenario == 'assistant-status':
        scenario = """
assistantPreferences.voice_enabled=true;
voiceState={state:'standby',meeting_id:a,message:'待唤醒'};
const statusChat=assistantOpen(a);
const statusTurn={id:'status-fixture',conversation_id:statusChat.id,meeting_id:a,state:'running',source:'text'};
document.addEventListener('DOMContentLoaded',()=>{
 const panel=document.createElement('div');panel.style='position:fixed;left:12px;bottom:120px;z-index:100;background:white;border:1px solid #aaa;padding:8px;font-size:12px;max-width:180px';
 const label=document.createElement('label');label.textContent='合成状态验收';const select=document.createElement('select');select.setAttribute('aria-label','合成助手状态');
 const states={
 '听会':{state:'standby',message:'待唤醒'},
 '听讲话':{state:'listening',input_level:.65},
 '识别问题':{state:'transcribing'},
 '思考':{state:'thinking',turn_id:'status-fixture'},
 '语音准备':{state:'synthesizing'},
 '本机合成':{state:'speaking',audio_playing:false},
 '语音回答':{state:'speaking',audio_playing:true,speech:{provider:'api'}},
 '分段间隙':{state:'synthesizing',continuing:true},
 '回答完毕':{state:'standby',outcome:'spoken'},
 '文字思考':{state:'off',reason:'disabled',dialogue:statusTurn},
 '文字完成':{state:'off',reason:'disabled',dialogue:{...statusTurn,state:'succeeded'}},
 '暂停':{state:'off',reason:'meeting_paused'},
 '关闭麦克风':{state:'off',reason:'microphone_disabled'},
 '异常':{state:'error',message:'合成故障：模型服务暂不可用，请重试'}
 };
 for(const key of Object.keys(states)){const option=document.createElement('option');option.textContent=key;option.value=key;select.append(option);}
 select.onchange=()=>{voiceState={meeting_id:a,...structuredClone(states[select.value]),updated_at_ms:Date.now()};statusChat.turns=['synthesizing','speaking'].includes(voiceState.state)?[{id:'complete-synthetic-answer',question:'合成问题：下一步如何推进？',source:'voice',state:'succeeded',answer:{blocks:[{kind:'suggestion',text:'我建议先确认验收标准，再比较两个方案的风险。',sources:[]}],evidence:[]}}]:[];};label.append(select);panel.append(label);document.body.append(panel);
});
"""
    elif args.scenario == 'assistant-interruption':
        scenario = """
assistantPreferences.voice_enabled=true;
voiceState={state:'thinking',meeting_id:a,turn_id:'thinking-fixture',message:'正在回答 · 可唤醒打断'};
const syntheticChat=assistantOpen(a);
syntheticChat.turns=[{id:'completed-fixture',source:'voice',question:'合成已完成问题',state:'succeeded',speech_interrupted:true,answer:{blocks:[{kind:'suggestion',text:'合成已完成回答全文保留。',sources:[]}],evidence:[]}},{id:'thinking-fixture',source:'voice',question:'合成正在思考的问题',state:'running',answer:null}];
"""
    elif args.scenario == 'voice-lifecycle':
        scenario = """
const syntheticChat=assistantOpen(a);
syntheticChat.turns=[
{id:'old-failed',source:'voice',question:'合成失败问题',state:'failed',error:'合成服务失败',answer:{evidence:null}},
{id:'old-cancelled',source:'voice',question:'合成取消问题',state:'cancelled',answer:null},
{id:'old-success',source:'voice',question:'合成已完成问题',state:'succeeded',answer:{blocks:[{kind:'suggestion',text:'合成建议仍可阅读。',sources:[]}],evidence:[]}},
{id:'pending-voice',source:'voice',question:'合成正在处理的问题',state:'pending',answer:null}];
let syntheticPolls=0;
const syntheticInvoke=window.__TAURI_INTERNALS__.invoke;
window.__TAURI_INTERNALS__.invoke=async(cmd,args)=>{if(cmd==='dispatch'&&args.operation.kind==='assistant_detail'){syntheticPolls++;const t=syntheticChat.turns.at(-1);if(syntheticPolls>5){t.state='failed';t.error='合成超时，界面保持可操作';t.answer={evidence:null};}else if(syntheticPolls>2)t.state='running';}return syntheticInvoke(cmd,args)};
"""
    elif args.scenario == 'minutes-overlap':
        scenario = "for(const art of details[b].artifacts.filter(x=>x.type==='minutes')){art.status='incomplete';art.coverage.incomplete=true;art.coverage.assistant_playback_overlap_segments=10;}details[b].jobs=[{id:'synthetic-minute-success',type:'final_map',state:'succeeded',error:null}];"
    elif args.scenario == 'render-recovery':
        scenario = "details[a].segments[0].text={synthetic_render_fault:true};"

    class Handler(SimpleHTTPRequestHandler):
        def __init__(self,*a,**kw):super().__init__(*a,directory=str(dist),**kw)
        def do_GET(self):
            if self.path in ('/','/?test=product'):
                body=(dist/'index.html').read_text().replace('<head>','<head><script>'+BOOT+scenario+'</script>').encode();self.send_response(200);self.send_header('Content-Type','text/html; charset=utf-8');self.send_header('Content-Length',str(len(body)));self.end_headers();self.wfile.write(body)
            else:super().do_GET()
        def log_message(self,*_):pass
    server=ThreadingHTTPServer(('127.0.0.1',args.port),Handler)
    print(f'Isolated synthetic product QA: http://127.0.0.1:{server.server_port}/',flush=True)
    try:server.serve_forever()
    except KeyboardInterrupt:pass
    finally:server.server_close()
if __name__=='__main__':main()
