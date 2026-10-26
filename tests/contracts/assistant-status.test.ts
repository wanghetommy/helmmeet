import test from 'node:test';
import assert from 'node:assert/strict';
import {assistantDisplay,type VoiceStatus} from '../../apps/desktop/src/assistantStatus.ts';
const turn={id:'synthetic-turn',conversation_id:'synthetic-chat',meeting_id:null,state:'succeeded',source:'text'};
const show=(value:VoiceStatus,complete?:string,now=2000)=>assistantDisplay(value,'小舵同学',complete,now);
test('listening takes precedence over a late completed answer and uses only bounded real input level',()=>{
 const value={state:'listening',dialogue:turn,outcome:'spoken',updated_at_ms:1999,input_level:.4};
 assert.equal(show(value,turn.id).title,'正在听你讲话');assert.equal(show(value).level,.4);
 assert.equal(show({...value,input_level:NaN}).level,0);assert.equal(show({...value,input_level:12}).level,1);
 assert.equal(show({...value,input_level:undefined}).level,0);
});
test('generation, TTS preparation and actual playback are distinct including local TTS rendering',()=>{
 assert.equal(show({state:'transcribing'}).title,'正在思考');assert.equal(show({state:'thinking'}).title,'正在思考');
 for(const value of [{state:'synthesizing'},{state:'speaking',audio_playing:false}]) {
  assert.equal(show(value).title,'正在思考（语音）');assert.equal(show(value).motion,'thinking');
 }
 const playing=show({state:'speaking',audio_playing:true,speech:{provider:'api'}});
 assert.equal(playing.title,'正在回答（语音）');assert.equal(playing.motion,'output');assert.match(playing.detail,/火山 TTS/);
 const gap=show({state:'synthesizing',continuing:true});assert.equal(gap.title,playing.title);assert.equal(gap.motion,'none');assert.match(gap.detail,/下一段/);
});
test('text generation is visible with voice disabled and old history never replays completion',()=>{
 assert.equal(show({state:'off',reason:'disabled',dialogue:{...turn,state:'running'}}).title,'正在思考');
 assert.equal(show({state:'off',reason:'disabled',dialogue:turn}).title,'语音未开启');
 assert.equal(show({state:'off',reason:'disabled',dialogue:turn},turn.id).title,'已回答');
 assert.notEqual(show({state:'off',dialogue:{...turn,state:'failed'}},turn.id).title,'已回答');
 assert.notEqual(show({state:'off',dialogue:{...turn,id:'new-turn'}},turn.id).title,'已回答');
});
test('actual completion is brief and stop or speech failure cannot masquerade as completed playback',()=>{
 const value={state:'standby',outcome:'spoken',updated_at_ms:1000};
 assert.equal(show(value,undefined,1100).title,'回答完毕');assert.equal(show(value,undefined,2800).title,'正在听会');
 assert.equal(show(value,undefined,999).title,'正在听会');
 for(const outcome of ['stopped','speech_failed'])assert.equal(show({...value,outcome,dialogue:turn},turn.id,1100).title,'正在听会');
 assert.equal(show({...value,state:'listening'},undefined,1100).title,'正在听你讲话');
});
test('unavailable capture cannot show listening and echo-reference fallback keeps a button instruction',()=>{
 for(const [reason,title] of [['meeting_paused','听会已暂停'],['microphone_disabled','麦克风已关闭'],['locked','听会已暂停'],['status_unavailable','状态暂不可用']])assert.equal(show({state:'off',reason}).title,title);
 assert.match(show({state:'speaking',interruption:{available:false,reason:'synthetic'}}).detail,/点击/);
 assert.equal(show({state:'standby',message:'待唤醒'}).title,'正在听会');
});
