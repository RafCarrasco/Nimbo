// Transport/clock double executes the actual injected script; not WebView2 or native attribution evidence.
import test from 'node:test';import assert from 'node:assert/strict';import fs from 'node:fs';import vm from 'node:vm';
const script=fs.readFileSync(new URL('../rust/bridge/picker.js',import.meta.url),'utf8');
function page({url='https://nimbo-picker.invalid/',top=true}={}) {
 const sent=[],listeners=new Map(),events=new Map(),timers=new Map();let seq=0;
 const webview={postMessage:m=>sent.push(JSON.parse(JSON.stringify(m))),addEventListener:(type,fn)=>listeners.set(type,fn),removeEventListener:type=>listeners.delete(type)};
 const window={chrome:{webview},addEventListener:(type,fn)=>events.set(type,fn),removeEventListener:type=>events.delete(type)};window.top=top?window:{};
 vm.runInNewContext(script,{window,location:{href:url},setTimeout:(fn)=>{timers.set(++seq,fn);return seq;},clearTimeout:id=>timers.delete(id)});
 return{window,sent,deliver:m=>listeners.get('message')?.({data:m}),hide:()=>events.get('pagehide')?.(),timers};
}
test('foreign documents and iframes never receive the picker API',()=>{
 for(const config of [{url:'https://example.org/'},{top:false},{url:'https://nimbo-picker.invalid/other'},{url:'https://nimbo-picker.invalid/#forged'}]) {
  const p=page(config);assert.equal(p.window.capturePicker,undefined);assert.equal(p.sent.length,0);assert.equal(p.timers.size,0);
 }
});
test('pagehide disposes pending calls and prevents late-ready revival',async()=>{
 const p=page();const result=p.window.capturePicker.list();const rejected=assert.rejects(result,/Ponte indisponível/);
 p.hide();assert.equal(p.timers.size,0);await rejected;
 p.deliver({v:1,kind:'picker-ready',gen:1});
 await assert.rejects(p.window.capturePicker.list(),/Ponte indisponível/);assert.equal(p.sent.length,1);
});
test('picker bridge preserves the four legacy APIs and correlates native replies',async()=>{
 const p=page();assert.deepEqual(Object.keys(p.window.capturePicker).sort(),['cancel','choose','diagnostics','list']);
 assert.ok(Object.isFrozen(p.window.capturePicker));assert.deepEqual(p.sent,[{v:1,kind:'picker-hello'}]);
 const list=p.window.capturePicker.list();assert.equal(p.sent.length,1);
 p.deliver({v:1,kind:'picker-ready',gen:7});assert.deepEqual(p.sent[1],{v:1,kind:'picker-invoke',id:1,gen:7,method:'list',args:[]});
 p.deliver({v:1,kind:'picker-result',id:1,gen:6,ok:true,value:{sources:[],audio:false}});
 p.deliver({v:1,kind:'picker-result',id:1,gen:7,ok:true,value:{sources:[],audio:false}});
 assert.deepEqual(await list,{sources:[],audio:false});
 const chosen=p.window.capturePicker.choose('window:11:0',true);assert.deepEqual(p.sent.at(-1).args,[{id:'window:11:0',audio:true}]);
 p.deliver({v:1,kind:'picker-result',id:2,gen:7,ok:true,value:null});assert.equal(await chosen,null);
 const cancelled=p.window.capturePicker.cancel();p.deliver({v:1,kind:'picker-result',id:3,gen:7,ok:true,value:null});await cancelled;
 const diag=p.window.capturePicker.diagnostics();p.deliver({v:1,kind:'picker-result',id:4,gen:7,ok:true,value:'fixture diagnostics'});assert.equal(await diag,'fixture diagnostics');
 assert.equal(p.timers.size,0);
});
