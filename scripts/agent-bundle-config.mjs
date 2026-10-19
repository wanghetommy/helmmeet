import fs from 'node:fs/promises';
import path from 'node:path';
import {fileURLToPath} from 'node:url';
const root=path.resolve(path.dirname(fileURLToPath(import.meta.url)),'..');
const args=process.argv.slice(2),target=args[args.indexOf('--target')+1];
if(!['aarch64-apple-darwin','x86_64-apple-darwin'].includes(target))throw Error('explicit macOS target required');
const runtime=path.join(root,'.tools/agent-runtime/targets',target);
const config={bundle:{resources:{[runtime+'/']:'agent-runtime/',[path.join(root,'.tools/voice/bundle')+'/']:'voice-models/',[path.join(root,'native/voice-assistant/licenses')+'/']:'licenses/voice-assistant/'}}};
// Generated per target: never rewrite a checked-in shared config during builds.
const output=path.join(root,'.tools/bundle-config');await fs.mkdir(output,{recursive:true});
await fs.writeFile(path.join(output,target+'.json'),JSON.stringify(config,null,2));
