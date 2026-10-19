// IPC and persisted history remain untrusted at the rendering boundary.
export type AssistantEvidence = {source_id:string;kind:string;text:string;name?:string;meeting_id?:string;segment_id?:string;revision?:number;start_ms?:number;version?:number;stale?:boolean;assistant_playback_overlap?:boolean};
export type AssistantAnswer = {legacy?:boolean;coverage?:{through_ms?:number;transcript_returned:number;transcript_total:number};blocks:{kind:string;text:string;sources:string[]}[];evidence:AssistantEvidence[]};
const object=(v:unknown):Record<string,unknown>|undefined=>v!==null&&typeof v==='object'&&!Array.isArray(v)?v as Record<string,unknown>:undefined;
const text=(v:unknown)=>typeof v==='string'?v:undefined;
const number=(v:unknown)=>typeof v==='number'&&Number.isFinite(v)&&v>=0?v:undefined;
export function readAssistantAnswer(value:unknown):AssistantAnswer|undefined {
 const answer=object(value);
 if(!answer||!Array.isArray(answer.blocks)||!Array.isArray(answer.evidence))return;
 const blocks:AssistantAnswer['blocks']=[];
 for(const value of answer.blocks){const b=object(value);if(!b||typeof b.kind!=='string'||typeof b.text!=='string'||!Array.isArray(b.sources)||!b.sources.every(s=>typeof s==='string'))return;blocks.push({kind:b.kind,text:b.text,sources:[...b.sources]});}
 const evidence:AssistantEvidence[]=[];
 for(const value of answer.evidence){const e=object(value);if(!e||typeof e.source_id!=='string'||typeof e.kind!=='string'||typeof e.text!=='string')return;evidence.push({source_id:e.source_id,kind:e.kind,text:e.text,name:text(e.name),meeting_id:text(e.meeting_id),segment_id:text(e.segment_id),revision:number(e.revision),start_ms:number(e.start_ms),version:number(e.version),stale:e.stale===true,assistant_playback_overlap:e.assistant_playback_overlap===true});}
 const c=object(answer.coverage);const returned=number(c?.transcript_returned),total=number(c?.transcript_total);
 return {blocks,evidence,legacy:answer.legacy===true,coverage:returned!==undefined&&total!==undefined?{transcript_returned:returned,transcript_total:total,through_ms:number(c?.through_ms)}:undefined};
}
