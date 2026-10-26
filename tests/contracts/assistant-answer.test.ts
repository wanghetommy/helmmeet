import test from 'node:test';
import assert from 'node:assert/strict';
import {readAssistantAnswer} from '../../apps/desktop/src/assistantAnswer.ts';
test('voice pending and failed answers do not become renderable partial objects',()=>{
 for(const value of [null,undefined,{}, {evidence:null},{blocks:null,evidence:[]},{blocks:{},evidence:[]},{blocks:[{kind:'suggestion',text:'合成建议'}],evidence:[]},{blocks:[{kind:'suggestion',text:{unexpected:true},sources:[]}],evidence:[]}])assert.equal(readAssistantAnswer(value),undefined);
});
test('validated answers retain citations while unsafe optional metadata cannot crash React',()=>{
 const input={blocks:[{kind:'suggestion',text:'合成建议',sources:['method:test']}],evidence:[{source_id:'method:test',kind:'method',text:'合成方法',name:{unexpected:true},start_ms:'wrong'}],coverage:{transcript_returned:1,transcript_total:2,through_ms:1000}};
 const answer=readAssistantAnswer(input)!;assert.equal(answer.blocks[0]?.text,'合成建议');assert.equal(answer.evidence[0]?.source_id,'method:test');assert.equal(answer.evidence[0]?.name,undefined);assert.equal(answer.evidence[0]?.start_ms,undefined);assert.equal(answer.coverage?.transcript_total,2);assert.deepEqual(input.evidence[0]?.name,{unexpected:true});
 assert.equal(readAssistantAnswer({...input,blocks:[{kind:'suggestion',text:'合成建议',sources:[{}]}]}),undefined);
 assert.equal(readAssistantAnswer({...input,evidence:[null]}),undefined);
});

test('experience provenance and technical failure remain readable without raw evidence metadata',()=>{
 const text='目前我还没找到相关讨论。根据我的知识与经验，我建议先小范围测试。';
 const result=readAssistantAnswer({blocks:[{kind:'suggestion',basis:'experience',text,sources:[]}],evidence:[]})!;
 assert.equal(result.blocks[0]?.text,text);assert.deepEqual(result.evidence,[]);
 const failure=readAssistantAnswer({blocks:[{kind:'unanswered',reason:'validation_failed',text:'这轮回答的格式或引用校验未通过，请重试。',sources:[]}],evidence:[]})!;
 assert.match(failure.blocks[0]!.text,/格式或引用校验/);assert(!failure.blocks[0]!.text.includes('经验'));
});
