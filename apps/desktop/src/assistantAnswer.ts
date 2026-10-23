// IPC and persisted history remain untrusted at the rendering boundary.
export type AssistantEvidence = {source_id:string;kind:string;text:string;name?:string;meeting_id?:string;segment_id?:string;revision?:number;start_ms?:number;version?:number;stale?:boolean;assistant_playback_overlap?:boolean};
export type AssistantCoverage = {
 transcript_returned:number; transcript_total:number; through_ms?:number;
 basis?:string; memory_processed?:number; memory_total?:number; memory_pending?:number;
 context_plan?:{full_transcript?:boolean;partial?:boolean;meeting_rows_returned?:number;meeting_rows_total?:number;degradations?:string[]};
 chat_history_rows?:number; chat_summary_pending?:number;
 asr_missing_ranges?:number; recording_through_ms?:number; playback_overlap_segments?:number;
 review_completed?:number; review_total?:number;
};
export type AssistantAnswer = {legacy?:boolean;coverage?:AssistantCoverage;blocks:{kind:string;text:string;sources:string[]}[];evidence:AssistantEvidence[]};
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
 const c=object(answer.coverage);const plan=object(c?.context_plan);const degradations=Array.isArray(plan?.degradations)?plan.degradations.filter((v):v is string=>typeof v==='string'):[];const contextPlan=plan?{full_transcript:plan.full_transcript===true,partial:plan.partial===true,meeting_rows_returned:number(plan.meeting_rows_returned),meeting_rows_total:number(plan.meeting_rows_total),degradations}:undefined;const returned=number(c?.transcript_returned),total=number(c?.transcript_total);
 return {blocks,evidence,legacy:answer.legacy===true,coverage:returned!==undefined&&total!==undefined?{
  transcript_returned:returned,transcript_total:total,through_ms:number(c?.through_ms),basis:text(c?.basis),context_plan:contextPlan,
  memory_processed:number(c?.memory_processed),memory_total:number(c?.memory_total),memory_pending:number(c?.memory_pending),
  chat_history_rows:number(c?.chat_history_rows),chat_summary_pending:number(c?.chat_summary_pending),
  asr_missing_ranges:number(c?.asr_missing_ranges),recording_through_ms:number(c?.recording_through_ms),
  playback_overlap_segments:number(c?.playback_overlap_segments),review_completed:number(c?.review_completed),review_total:number(c?.review_total)
 }:undefined};
}
