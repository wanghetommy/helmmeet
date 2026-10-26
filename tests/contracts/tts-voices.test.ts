import test from 'node:test';
import assert from 'node:assert/strict';
import {readFileSync} from 'node:fs';
test('office voice presets stay within ten distinct standard Chinese TTS 2.0 voices',()=>{
 const catalog=JSON.parse(readFileSync(new URL('../../apps/desktop/src/ttsVoices.json',import.meta.url),'utf8')) as {source:string;resource_id:string;voices:{id:string;name:string}[]};
 assert.equal(catalog.resource_id,'seed-tts-2.0');
 assert.match(catalog.source,/^https:\/\/docs\.volcengine\.com\//);
 assert.ok(catalog.voices.length>0&&catalog.voices.length<=10);
 assert.equal(new Set(catalog.voices.map(v=>v.id)).size,catalog.voices.length);
 assert.ok(catalog.voices.every(v=>/^zh_(female|male)_[a-z0-9]+_uranus_bigtts$/.test(v.id)&&v.name.trim()));
 assert.ok(catalog.voices.some(v=>v.id.includes('_male_')));
 assert.ok(catalog.voices.some(v=>v.id.includes('_female_')));
});
