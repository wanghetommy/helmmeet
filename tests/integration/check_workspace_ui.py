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
window.testCalls=[];
window.testAudioStarts=[];
const NativeAudioContext=window.AudioContext;
window.AudioContext=class extends NativeAudioContext {
 createBufferSource(){const node=super.createBufferSource();const start=node.start.bind(node);node.start=(at)=>{window.testAudioStarts.push({at,duration:node.buffer.duration});start(at);};return node;}
};
const profiles=[{profile_id:'p1',config_version:1,display_name:'合成本地模型',base_url:'http://127.0.0.1:11434/v1',model:'fixture',credential_valid:true,has_api_key:false,archived:false,is_local:true,latest_version:1,bound_meetings:3,context_tokens:32768,max_output_tokens:4096,timeout_seconds:10,last_test:null}];
let prefs={analysis_paused:false,default_source_mask:1,default_input_device:0};
const collections=[{id:'k1',name:'合成项目词库',kind:'terms',version:1,archived:false,term_count:2,characters:0,bound_meetings:0},{id:'k2',name:'合成项目背景',kind:'documents',version:1,archived:false,term_count:0,characters:32,bound_meetings:0}];
for(const d of Object.values(details)){d.model_profile=profiles[0];d.capture_epoch='epoch-'+d.id;d.knowledge=[];}
const capabilities=d=>({phase:d.state==='ready'?'preparation':['recording','paused','interrupted'].includes(d.state)?'live':'after',has_audio:d.duration_ms>0,has_confirmed_text:!!d.segments.length,has_minutes:!!d.artifacts.find(a=>a.type==='minutes'),has_actions:!!d.actions.length,model_ready:d.analysis_mode!=='transcript_only',can_analyze:d.state!=='ready'&&!!d.segments.length&&d.analysis_mode!=='transcript_only'&&!prefs.analysis_paused,analysis_paused:prefs.analysis_paused});
window.__TAURI_INTERNALS__={transformCallback:()=>1,unregisterCallback:()=>{},invoke:async(cmd,args)=>{
 if(cmd==='plugin:event|listen')return 1;if(cmd==='plugin:event|unlisten')return;

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
 case 'templates':result=[{id:'general',name:'通用会议',version:1,instructions:'核对目标、结论和行动'},details[b].template];break;
 case 'devices':result=[];break;
 case 'read_transcript_snapshot':result={meeting_id:d.id,title:d.title,source:op.source,timestamps:op.timestamps,through_ms:60000,hash:'fixture-hash',text:op.source==='original'?'原始识别文本\n第二段测试文字。':'人工修订的测试文字。\n第二段测试文字。',segments:d.segments.map((s,i)=>({segment_id:s.id,start_ms:s.start_ms,revision:op.source==='original'?1:s.revision,text:op.source==='original'&&i===0?'原始识别文本':s.text}))};break;
 case 'playback':{const end=Math.min(op.start_ms+30000,d.duration_ms);result={start_ms:op.start_ms,end_ms:end,samples:Array.from({length:(end-op.start_ms)*16},(_,i)=>Math.round(2000*Math.sin(2*Math.PI*440*(op.start_ms*16+i)/16000)))};break;}
 case 'copy_transcript':result={meeting_id:d.id,title:d.title,through_ms:60000,segments:d.segments};break;
 case 'set_summary_schedule':Object.assign(d.schedule,{enabled:op.enabled,mode:op.mode,version:d.schedule.version+1});result={saved:true};break;
 case 'record':if(op.action==='stop')throw Error('结束必须确认');d.state=op.action==='pause'?'paused':'recording';d.version++;if(op.action!=='pause')d.capture_epoch+='-new';d.capabilities=capabilities(d);result=d;break;
 case 'finish_meeting':if(op.expected_version!==d.version||op.capture_epoch!==d.capture_epoch)throw Error('录制会话已变化');d.state='finalizing';d.version++;d.capabilities=capabilities(d);result=d;break;
 case 'save_product_preferences':prefs={analysis_paused:op.analysis_paused,default_source_mask:op.default_source_mask,default_input_device:op.default_input_device};result={saved:true};break;
 case 'save_preparation':if(d.state!=='ready'||d.version!==op.expected_version)throw Error('准备版本已变化');Object.assign(d,{title:op.title,assistant_id:op.assistant_id,source_mask:op.source_mask,context:op.context,version:d.version+1});result={saved:true};break;
 case 'create':{const id=crypto.randomUUID();details[id]={...structuredClone(details[c]),id,title:op.title,assistant_id:op.assistant_id,actions:[],notes:[],state:'ready',version:1,analysis_mode:'local',model_profile:profiles[0]};result={id};break;}
 case 'follow_up':{const id=crypto.randomUUID();details[id]={...structuredClone(details[c]),id,title:d.title+' · 续会',parent_meeting_id:d.id,context:d.context,version:1};result={id};break;}
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
    args=parser.parse_args()
    dist=ROOT/'apps/desktop/dist'
    class Handler(SimpleHTTPRequestHandler):
        def __init__(self,*a,**kw):super().__init__(*a,directory=str(dist),**kw)
        def do_GET(self):
            if self.path in ('/','/?test=product'):
                body=(dist/'index.html').read_text().replace('<head>','<head><script>'+BOOT+'</script>').encode();self.send_response(200);self.send_header('Content-Type','text/html; charset=utf-8');self.send_header('Content-Length',str(len(body)));self.end_headers();self.wfile.write(body)
            else:super().do_GET()
        def log_message(self,*_):pass
    server=ThreadingHTTPServer(('127.0.0.1',args.port),Handler)
    print(f'Isolated synthetic product QA: http://127.0.0.1:{server.server_port}/',flush=True)
    try:server.serve_forever()
    except KeyboardInterrupt:pass
    finally:server.server_close()
if __name__=='__main__':main()
