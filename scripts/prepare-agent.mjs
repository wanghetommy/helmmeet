// Build-only preparation; remote downloads require --download and SHA256 verification.
import fs from 'node:fs/promises';
import path from 'node:path';
import crypto from 'node:crypto';
import {execFileSync} from 'node:child_process';
import {fileURLToPath} from 'node:url';
const root=path.resolve(path.dirname(fileURLToPath(import.meta.url)),'..');
const lock=JSON.parse(await fs.readFile(path.join(root,'agent/dependencies.lock.json'),'utf8'));
const args=process.argv.slice(2),ti=args.indexOf('--target');
const host=execFileSync('/bin/sh',[path.join(root,'scripts/with-toolchain.sh'),'rustc','-vV'],{encoding:'utf8'}).match(/^host: (.+)$/m)?.[1];
const target=ti>=0?args[ti+1]:(process.env.CARGO_BUILD_TARGET??host);
const runtime=lock.runtimes[target];if(!runtime)throw Error('Agent仅支持macOS arm64/x86_64');
const cache=path.join(root,'.tools/agent-runtime'),downloads=path.join(cache,'downloads');
await fs.mkdir(downloads,{recursive:true});
const archive=path.join(downloads,runtime.archive);
try{await fs.access(archive);}catch{
  if(!args.includes('--download'))throw Error('Agent Node缓存未准备，请运行 node scripts/prepare-agent.mjs --download --target '+target);
  execFileSync('/usr/bin/curl',['--fail','--location','--proto','=https','--tlsv1.2','--connect-timeout','20','--max-time','180','-o',archive+'.download',`https://nodejs.org/dist/v${lock.node}/${runtime.archive}`],{stdio:'inherit'});
  await fs.rename(archive+'.download',archive);
}
const hash=crypto.createHash('sha256').update(await fs.readFile(archive)).digest('hex');
if(hash!==runtime.sha256)throw Error('Node运行时SHA256不匹配，拒绝解压');
const directory=path.join(cache,'targets',target);
await fs.mkdir(directory,{recursive:true});
await fs.rm(path.join(directory,'node_modules'),{recursive:true,force:true});
await fs.rm(path.join(directory,'src'),{recursive:true,force:true});
// Only extract the verified node binary and its LICENSE into this generated target.
execFileSync('/usr/bin/tar',['-xzf',archive,'--strip-components=1','-C',directory,`${runtime.archive.replace('.tar.gz','')}/bin/node`,`${runtime.archive.replace('.tar.gz','')}/LICENSE`]);
const list=execFileSync('npm',['ls','--omit=dev','--all','--parseable','-w','@helmmeet/agent'],{cwd:root,encoding:'utf8'}).trim().split('\n');
for(const location of list){
  const relative=path.relative(root,location);
  if(!relative.startsWith('node_modules/')||relative==='node_modules/@helmmeet/agent')continue;
  await fs.cp(location,path.join(directory,relative),{recursive:true,dereference:true});
}
await fs.cp(path.join(root,'agent/src'),path.join(directory,'src'),{recursive:true});
await fs.writeFile(path.join(directory,'package.json'),JSON.stringify({private:true,type:'module'}));
await fs.writeFile(path.join(directory,'manifest.json'),JSON.stringify({target,node:lock.node,pi:lock.pi,node_archive_sha256:hash},null,2));
console.log('Prepared private Agent runtime:',target);
