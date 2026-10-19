// JSONL stdio only. No credentials, model endpoints, filesystem discovery or network.
import readline from 'node:readline';
import { createAgentSession, createExtensionRuntime, ModelRuntime, SessionManager, SettingsManager } from '@earendil-works/pi-coding-agent';
import { createAssistantMessageEventStream } from '@earendil-works/pi-ai/utils/event-stream';
import { PERSONA, TOOL_DEFS } from './persona.mjs';

const send = value => process.stdout.write(JSON.stringify(value)+'\n');
const input = readline.createInterface({input:process.stdin,crlfDelay:Infinity});
const waiting = new Map();
let start, started=false;
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
      const result=await rpc('model_request',toOpenAI(context),options?.signal);
      const content=[];
      if(result.content)content.push({type:'text',text:result.content});
      for(const c of result.tool_calls??[])content.push({type:'toolCall',id:c.id,name:c.function.name,arguments:JSON.parse(c.function.arguments)});
      const m={...empty,content,stopReason:result.tool_calls?.length?'toolUse':'stop'};
      s.push({type:'start',partial:{...empty}});
      s.push({type:'done',reason:m.stopReason,message:m});s.end();
    }catch{const m={...empty,stopReason:'error',errorMessage:'模型代理失败或已取消'};s.push({type:'error',reason:'error',error:m});s.end();}
  });return s;
}
async function run(request){
  const resources={getExtensions:()=>({extensions:[],errors:[],runtime:createExtensionRuntime()}),getSkills:()=>({skills:[],diagnostics:[]}),getPrompts:()=>({prompts:[],diagnostics:[]}),getThemes:()=>({themes:[],diagnostics:[]}),getAgentsFiles:()=>({agentsFiles:[]}),getSystemPrompt:()=>PERSONA,getSystemPromptSource:()=>undefined,getAppendSystemPrompt:()=>[],getAppendSystemPromptSources:()=>[],extendResources:()=>{},reload:async()=>{}};
  const runtime=await ModelRuntime.create({credentials:{read:async()=>undefined,list:async()=>[],modify:async()=>{throw Error("凭据由宿主管理");},delete:async()=>{}},modelsPath:null,allowModelNetwork:false,refreshOnCreate:false});
  runtime.registerProvider('helmmeet',{api:'openai-completions',apiKey:'local-broker-no-secret',baseUrl:'http://127.0.0.1/disabled',streamSimple:stream,models:[{id:'bound-model',name:'用户绑定模型',input:['text'],reasoning:false,contextWindow:request.context_tokens,maxTokens:request.max_tokens,cost:{input:0,output:0,cacheRead:0,cacheWrite:0}}]});
  const tools=TOOL_DEFS.map(([name,description,properties])=>({name,label:name,description,parameters:{type:'object',properties,additionalProperties:false},execute:async(_id,args,signal)=>({content:[{type:'text',text:JSON.stringify(await rpc('tool_request',{name,args},signal))}],details:{}})}));
  const manager=SessionManager.inMemory(process.cwd());
  for(const m of request.history??[])manager.appendMessage(m.role==='user'?{role:'user',content:m.text,timestamp:Date.now()}:{role:'assistant',content:[{type:'text',text:m.text}],api:'openai-completions',provider:'helmmeet',model:'bound-model',timestamp:Date.now(),usage:{input:0,output:0,cacheRead:0,cacheWrite:0,totalTokens:0,cost:{input:0,output:0,cacheRead:0,cacheWrite:0,total:0}},stopReason:'stop'});
  const {session}=await createAgentSession({cwd:process.cwd(),agentDir:process.cwd(),modelRuntime:runtime,model:runtime.getModel('helmmeet','bound-model'),thinkingLevel:'off',resourceLoader:resources,tools:tools.map(t=>t.name),customTools:tools,sessionManager:manager,settingsManager:SettingsManager.inMemory({compaction:{enabled:false},retry:{enabled:false,maxRetries:0},cacheWarming:{mode:'off'}})});
  try{
    if(session.getActiveToolNames().some(n=>!tools.some(t=>t.name===n)))throw Error('工具边界错误');
    send({type:'ready',engine:'pi-coding-agent/1.3.0',tools:session.getActiveToolNames()});
    await session.prompt(JSON.stringify({question:request.question,scope:request.scope}));
    for(let attempt=0;attempt<2;attempt++){
      const text=session.getLastAssistantText();
      const verdict=await rpc('validate',{text});
      if(verdict.ok){send({type:'result',text});return;}
      if(attempt===0)await session.prompt('本轮回答未通过宿主结构/引用校验：'+String(verdict.error??'格式无效')+'。请依据已取得的原文重新输出合规JSON，不扩充事实，不重复操作草稿。');
    }
    throw Error('回答校验失败');
  }finally{session.dispose();}
}
try{await run(await initial);}catch{send({type:'failure',error:'小舵Agent运行失败，请重试或检查模型工具能力'});process.exitCode=1;}
input.close();
