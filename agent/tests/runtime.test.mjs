import test from 'node:test';
import assert from 'node:assert/strict';
import {spawn} from 'node:child_process';
import readline from 'node:readline';
import path from 'node:path';

test('real Pi SDK loops through only host tools, carries persona and shared history, repairs once', {timeout:15000},async()=>{
  const bundled=process.env.HELMMEET_TEST_AGENT_DIR&&path.resolve(process.env.HELMMEET_TEST_AGENT_DIR);
  const profile=bundled?`(version 1)(allow default)(deny network*)(deny process-fork)(deny file-write*)(deny file-read-data (require-all (subpath "/Users") (require-not (subpath ${JSON.stringify(bundled)}))))`:'';
  const command=bundled?'/usr/bin/sandbox-exec':process.execPath;
  const args=bundled?['-p',profile,path.join(bundled,'bin/node'),'--permission','--allow-fs-read='+bundled,path.join(bundled,'src/main.mjs')]:['agent/src/main.mjs'];
  const child=spawn(command,args,{cwd:bundled??process.cwd(),env:{PATH:'/usr/bin:/bin',PI_OFFLINE:'1',HOME:bundled?bundled+'/empty-home':'/private/tmp/helmmeet-in-memory-only'},stdio:['pipe','pipe','pipe']});
  let stderr='';child.stderr.on('data',b=>stderr+=b);let models=0,tools=0,validations=0,final;
  const exited=new Promise(resolve=>child.on('exit',resolve));
  child.stdin.write(JSON.stringify({type:'start',question:'再分析一下卡点',scope:{kind:'meeting',meeting_id:'synthetic'},context_tokens:32768,max_tokens:2048,history:[{role:'user',text:'上一轮合成问题'},{role:'assistant',text:'上一轮合成回答'}]})+'\n');
  try{
    for await(const line of readline.createInterface({input:child.stdout})){
      const m=JSON.parse(line);let result;
      if(m.type==='ready'){assert.deepEqual(new Set(m.tools),new Set(['get_context','search_evidence','get_transcript','get_methods','propose_action']));continue;}
      if(m.type==='model_request'){
        models++;assert(m.messages.some(x=>x.role==='system'&&x.content.includes('小舵')));assert(m.messages.some(x=>x.role==='user'&&x.content==='上一轮合成问题'));
        if(models===1)result={content:null,tool_calls:[{id:'call1',type:'function',function:{name:'get_methods',arguments:'{}'}}]};
        else result={content:JSON.stringify({blocks:[{kind:'suggestion',text:'先确认目标和验收条件。',sources:['method:duozhou-v1']}]})};
      }else if(m.type==='tool_request'){tools++;assert.equal(m.name,'get_methods');result={rows:[{source_id:'method:duozhou-v1',text:'目标应有验收条件'}]};}
      else if(m.type==='validate'){validations++;result=validations===1?{ok:false,error:'合成校验错误，测试一次修复'}:{ok:true};}
      else if(m.type==='result'){final=JSON.parse(m.text);break;}
      else assert.fail('Unexpected event '+m.type);
      child.stdin.write(JSON.stringify({type:'reply',id:m.id,result})+'\n');
    }
    child.stdin.end();assert.equal(await exited,0,stderr);assert.equal(models,3);assert.equal(tools,1);assert.equal(validations,2);assert.equal(final.blocks[0].kind,'suggestion');
  }finally{child.kill();}
});
