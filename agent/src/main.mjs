// JSONL stdio only. No credentials, model endpoints, filesystem discovery or network.
import readline from 'node:readline';
import { createAgentSession, createExtensionRuntime, ModelRuntime, SessionManager, SettingsManager } from '@earendil-works/pi-coding-agent';
import { createAssistantMessageEventStream } from '@earendil-works/pi-ai/utils/event-stream';
import { buildPersona, TOOL_DEFS } from './persona.mjs';

const send = value => process.stdout.write(JSON.stringify(value)+'\n');
const input = readline.createInterface({input:process.stdin,crlfDelay:Infinity});
const waiting = new Map();
let start, started=false, modelCalls=0;
let activeSession, forceFinal=false, maxModelCalls=6;
const initial = new Promise(resolve=>{start=resolve;});
input.on('line',line=>{
  if(line.length>2_000_000){process.exitCode=2; input.close(); return;}
  let m;try{m=JSON.parse(line);}catch{return;}
  if(m.type==='start'&&!started){started=true;start(m);}
  else if(m.type==='reply'&&waiting.has(m.id)){const p=waiting.get(m.id);waiting.delete(m.id);m.error?p.reject(Error('宿主操作失败')):p.resolve(m.result);}
});
input.on('close',()=>{for(const p of waiting.values())p.reject(Error('宿主已断开')); if(!started)process.exit(0);});
function rpc(type,payload,signal){return new Promise((resolve,reject)=>{
  if(signal?.aborted)return reject(Error('已取消'));
  const id=crypto.randomUUID(); const onAbort=()=>{waiting.delete(id);reject(Error('已取消'));};
  signal?.addEventListener('abort',onAbort,{once:true});
  waiting.set(id,{resolve:v=>{signal?.removeEventListener('abort',onAbort);resolve(v);},reject:e=>{signal?.removeEventListener('abort',onAbort);reject(e);}});
  send({type,id,...payload});
});}
const textOf = c=>typeof c==='string'?c:(c??[]).filter(x=>x.type==='text').map(x=>x.text).join('\n');
function safeFailure(error){
  const raw=error instanceof Error?error.message:String(error??'');
  if(/HTTP (401|403)/.test(raw))return '模型凭据或权限被拒绝（HTTP '+raw.match(/HTTP (401|403)/)[1]+'）';
  if(/HTTP 429/.test(raw))return '模型服务限流（HTTP 429），请稍后重试';
  if(/HTTP 5\d\d/.test(raw))return '模型服务暂时不可用（'+raw.match(/HTTP 5\d\d/)[0]+'）';
  if(/超时|连接失败|网络|socket|ECONN|ETIMEDOUT/i.test(raw))return '模型服务连接失败或超时';
  if(/工具调用|tool|function/i.test(raw))return '模型工具调用失败：请确认该模型支持 OpenAI 兼容工具调用';
  if(/JSON|正文|结构|引用|校验/i.test(raw))return '模型返回格式或引用校验失败';
  if(/上下文|token|预算|100KB/i.test(raw))return '模型上下文容量不足，请缩小问题范围或提高模型上下文配置';
  if(/取消|中断/i.test(raw))return '本轮模型请求已取消';
  return 'Agent运行失败，请检查模型配置和工具能力';
}
export function toOpenAI(context){
  const messages=[], declarations=new Map();
  for(const m of context.messages){
    if(m.role==='system'||m.role==='developer'){
      for(const t of m.toolsAdded??[])declarations.set(t.name,{type:'function',function:{name:t.name,description:t.description,parameters:t.parameters}});
      for(const t of m.toolsRemoved??[])declarations.delete(t.name);
      const text=[textOf(m.content??''),...Object.values(m.sections??{}).filter(v=>typeof v==='string')].filter(Boolean).join('\n'); if(text)messages.push({role:'system',content:text});
    }else if(m.role==='user')messages.push({role:'user',content:textOf(m.content)});
    else if(m.role==='assistant'){
      const calls=m.content.filter(c=>c.type==='toolCall').map(c=>({id:c.id,type:'function',function:{name:c.name,arguments:JSON.stringify(c.arguments)}}));
      messages.push({role:'assistant',content:textOf(m.content)||null,...(calls.length?{tool_calls:calls}:{})});
    }else if(m.role==='toolResult')messages.push({role:'tool',tool_call_id:m.toolCallId,content:textOf(m.content)});
  }
  return {messages,tools:[...declarations.values()]};
}
function stream(model,context,options){
  const s=createAssistantMessageEventStream();
  const empty={role:'assistant',content:[],api:'openai-completions',provider:'helmmeet',model:model.id,timestamp:Date.now(),usage:{input:0,output:0,cacheRead:0,cacheWrite:0,totalTokens:0,cost:{input:0,output:0,cacheRead:0,cacheWrite:0,total:0}},stopReason:'stop'};
  queueMicrotask(async()=>{
    try{
      const payload=toOpenAI(context);
      const compaction=payload.tools.length===0;
      modelCalls++;
      // A late automatic compaction must not consume the final answer/repair slots.
      if(modelCalls>=maxModelCalls-2)activeSession?.setAutoCompactionEnabled(false);
      // Reserve the last two calls for an answer and its single structure repair.
      const finalAnswer=!compaction&&(forceFinal||modelCalls>=maxModelCalls-1);
      if(finalAnswer){
        payload.tools=[];
        payload.messages.push({role:'user',content:'本轮取证预算已到收尾阶段，停止调用工具。仅依据已经实际读取的内容输出最终合规JSON；若仍有未读页或预算耗尽，明确说明覆盖不完整，不能声称检查了整场。无足够依据时说明缺口，不补造事实或草稿执行结果。'});
      }
      const result=await rpc('model_request',{...payload,purpose:compaction?'compaction':finalAnswer?'final_answer':'answer'},options?.signal);
      const content=[];
      if(result.content)content.push({type:'text',text:result.content});
      for(const c of result.tool_calls??[])content.push({type:'toolCall',id:c.id,name:c.function.name,arguments:JSON.parse(c.function.arguments)});
      const u=result._usage??{};
      const usage={...empty.usage,input:u.prompt_tokens??0,output:u.completion_tokens??0,totalTokens:u.total_tokens??0,cacheRead:u.prompt_tokens_details?.cached_tokens??0};
      const m={...empty,content,usage,stopReason:result._finish_reason==='length'?'length':result.tool_calls?.length?'toolUse':'stop'};
      s.push({type:'start',partial:{...empty}});
      s.push({type:'done',reason:m.stopReason,message:m});s.end();
    }catch(error){const reason=safeFailure(error);const m={...empty,stopReason:'error',errorMessage:reason};s.push({type:'error',reason:'error',error:m});s.end();}
  });return s;
}
async function run(request){
  maxModelCalls=Math.max(1,Math.min(6,Number.isInteger(request.model_budget)?request.model_budget:6));
  const resources={getExtensions:()=>({extensions:[],errors:[],runtime:createExtensionRuntime()}),getSkills:()=>({skills:[],diagnostics:[]}),getPrompts:()=>({prompts:[],diagnostics:[]}),getThemes:()=>({themes:[],diagnostics:[]}),getAgentsFiles:()=>({agentsFiles:[]}),getSystemPrompt:()=>buildPersona(request.assistant_name),getSystemPromptSource:()=>undefined,getAppendSystemPrompt:()=>[],getAppendSystemPromptSources:()=>[],extendResources:()=>{},reload:async()=>{}};
  const runtime=await ModelRuntime.create({credentials:{read:async()=>undefined,list:async()=>[],modify:async()=>{throw Error("凭据由宿主管理");},delete:async()=>{}},modelsPath:null,allowModelNetwork:false,refreshOnCreate:false});
  runtime.registerProvider('helmmeet',{api:'openai-completions',apiKey:'local-broker-no-secret',baseUrl:'http://127.0.0.1/disabled',streamSimple:stream,models:[{id:'bound-model',name:'用户绑定模型',input:['text'],reasoning:false,contextWindow:request.context_tokens,maxTokens:request.max_tokens,cost:{input:0,output:0,cacheRead:0,cacheWrite:0}}]});
  const tools=TOOL_DEFS.map(([name,description,properties])=>({name,label:name,description,parameters:{type:'object',properties,additionalProperties:false},execute:async(toolCallId,args,signal)=>{const result=await rpc('tool_request',{name,args,tool_call_id:toolCallId},signal);if(result.retrieval_exhausted||result.budget_exhausted)forceFinal=true;return {content:[{type:'text',text:JSON.stringify(result)}],details:{}};}}));
  const manager=SessionManager.inMemory(process.cwd());
  let firstHistory;
  for(const m of request.history??[]){const entry=manager.appendMessage(m.role==='user'?{role:'user',content:m.text,timestamp:Date.now()}:{role:'assistant',content:[{type:'text',text:m.text}],api:'openai-completions',provider:'helmmeet',model:'bound-model',timestamp:Date.now(),usage:{input:0,output:0,cacheRead:0,cacheWrite:0,totalTokens:0,cost:{input:0,output:0,cacheRead:0,cacheWrite:0,total:0}},stopReason:'stop'});firstHistory??=entry;}
  if(request.checkpoint?.summary)manager.appendCompaction(request.checkpoint.summary,firstHistory??null,request.checkpoint.tokens_before??0);
  const boundary=request.compact_only?manager.appendMessage({role:'user',content:'宿主保留此边界。后续问题尚未发送，不要重构或添加新的事实。',timestamp:Date.now()}):null;
  const usable=request.context_tokens-request.max_tokens-Math.max(1024,Math.ceil(request.context_tokens*.1));
  const {session}=await createAgentSession({cwd:process.cwd(),agentDir:process.cwd(),modelRuntime:runtime,model:runtime.getModel('helmmeet','bound-model'),thinkingLevel:'off',resourceLoader:resources,tools:tools.map(t=>t.name),customTools:tools,sessionManager:manager,settingsManager:SettingsManager.inMemory({compaction:{enabled:true,reserveTokens:request.context_tokens-Math.floor(usable*.7),keepRecentTokens:request.compact_only?1:Math.max(256,Math.min(4096,Math.floor(usable*.25)))},retry:{enabled:false,maxRetries:0},cacheWarming:{mode:'off'}})});
  activeSession=session;
  if(maxModelCalls<=2&&!request.compact_only)session.setAutoCompactionEnabled(false);
  try{
    if(session.getActiveToolNames().some(n=>!tools.some(t=>t.name===n)))throw Error('工具边界错误');
    send({type:'ready',engine:'pi-coding-agent/1.3.0',tools:session.getActiveToolNames()});
    if(request.compact_only){
      const compact=await session.compact('请用中文压缩会议助手较早对话。保留当前目标、明确约束、已澄清条件、未决追问、已接受记录及来源ID；区分建议与事实、过去决定与后来的改口。不要输出内部推理、工具步骤或编造信息，历史回答不是会议事实。仅概括已有内容，简洁，控制在1200中文字以内。');
      if(compact.firstKeptEntryId!==boundary)throw Error('压缩边界未覆盖完整历史对');
      send({type:'compacted',summary:compact.summary,tokens_before:compact.tokensBefore,usage:compact.usage??{estimated:true}});return;
    }
    const scope={kind:request.scope?.kind,meeting_id:request.scope?.meeting_id,scope:request.scope?.scope};
    await session.prompt(JSON.stringify({question:request.question,scope}));
    for(let attempt=0;attempt<2;attempt++){
      const text=session.getLastAssistantText();
      const verdict=await rpc('validate',{text});
      if(verdict.ok){send({type:'result',text});return;}
      // Once the host reports a validation error, the next turn is a repair
      // turn: disable tools/compaction and require the model to emit only the
      // final JSON object. This prevents a repair prompt from opening another
      // unbounded tool loop and repeatedly landing in the same generic error.
      forceFinal=true;
      if(attempt===0)await session.prompt('本轮回答未通过宿主结构/引用校验：'+String(verdict.error??'格式无效')+'。现在进入最终修复回合，只输出一个JSON对象，顶层必须是blocks数组；sources只能使用本轮实际返回的完整source_id字符串，不能引用未读取或与助手播报重叠的原文。建议或无法确认的内容使用suggestion/unanswered且sources为空；不要调用工具、不要输出Markdown或解释文字。');
      else {
        // Never turn a second validation miss into a user-visible generic
        // failure. Return a safe, explicitly incomplete answer; the host can
        // persist it and the user can retry without losing the conversation.
        send({type:'result',text:JSON.stringify({blocks:[{kind:'unanswered',text:'我暂时无法把本轮回答可靠绑定到已读取的原文依据；原文仍保留，请重试或缩小问题范围。',sources:[]}]})});
        return;
      }
    }
    throw Error('回答校验失败');
  }finally{session.dispose();}
}
try{await run(await initial);}catch(error){send({type:'failure',error:safeFailure(error)});process.exitCode=1;}
input.close();
