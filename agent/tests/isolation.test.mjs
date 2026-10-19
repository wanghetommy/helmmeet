import test from 'node:test';
import assert from 'node:assert/strict';
import fs from 'node:fs';
import path from 'node:path';
import {spawnSync} from 'node:child_process';
const target=process.platform==='darwin'?spawnSync('/bin/sh',['scripts/with-toolchain.sh','rustc','-vV'],{encoding:'utf8'}).stdout.match(/^host: (.+)$/m)?.[1]:'not-macos';
const runtime=path.resolve('.tools/agent-runtime/targets',target);
test('packaged Agent cannot read external files, write, connect or spawn', {skip:process.platform!=='darwin',timeout:12000},()=>{
 assert(fs.existsSync(path.join(runtime,'bin/node')),'prepare the host Agent runtime before isolation checks');
 const fixture=path.resolve('artifacts/agent-isolation-fixture.txt');fs.mkdirSync(path.dirname(fixture),{recursive:true});fs.writeFileSync(fixture,'SYNTHETIC-ONLY');
 const code=`import fs from 'node:fs';import net from 'node:net';import cp from 'node:child_process';
 const denied={};try{fs.readFileSync(${JSON.stringify(fixture)})}catch{denied.read=true}
 try{fs.writeFileSync(${JSON.stringify(path.join(runtime,'forbidden-write'))},'test')}catch{denied.write=true}
 try{cp.execFileSync('/usr/bin/true')}catch{denied.spawn=true}
 const socket=net.connect({host:'127.0.0.1',port:9});socket.on('error',e=>{denied.network=['EPERM','EACCES'].includes(e.code);console.log(JSON.stringify(denied));socket.destroy();});socket.setTimeout(1000,()=>{socket.destroy();console.log(JSON.stringify(denied));});`;
 const script=path.join(runtime,'isolation-probe.mjs');fs.writeFileSync(script,code);
 const profile=`(version 1)(allow default)(deny network*)(deny process-fork)(deny file-write*)(deny file-read-data (require-all (subpath "/Users") (require-not (subpath ${JSON.stringify(runtime)}))))`;
 try{
  for(const flags of [['--permission','--allow-fs-read='+runtime]]){
   const result=spawnSync('/usr/bin/sandbox-exec',['-p',profile,path.join(runtime,'bin/node'),...flags,script],{encoding:'utf8',timeout:5000,env:{PATH:'/usr/bin:/bin',HOME:runtime+'/empty-home'}});
   assert.equal(result.status,0,result.stderr);assert.deepEqual(JSON.parse(result.stdout.trim()),{read:true,write:true,spawn:true,network:true});
  }
 }finally{fs.rmSync(script,{force:true});fs.rmSync(fixture,{force:true});}
});
