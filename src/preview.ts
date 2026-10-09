import { mount } from './main'
import { layouts, emptyState } from './model'
import demoCover from '../assets/demo-cover.webp'
import manifest from '../manifest.json'
import type { AddonValues, CanvasAddonMountContext, JsonValue, NativeConnection } from '../generated/mywallpaper-runtime'
const blob = await (await fetch(demoCover)).blob()
const coverData = await new Promise<string>(resolve => { const reader = new FileReader(); reader.onload = () => resolve(String(reader.result)); reader.readAsDataURL(blob) })
const thumbnail = new URLSearchParams(location.search).get('view') === 'thumbnail'
if (thumbnail) document.body.classList.add('thumbnail')
const defaults = Object.fromEntries(manifest.settings.filter(setting => 'default' in setting).map(setting => [setting.id, setting.default])) as AddonValues
const labels = manifest.settings.find(setting => setting.id === 'layout')!.options!
const examples: Array<{refresh:()=>void, dispose:()=>void, appearance:(partial:AddonValues)=>Promise<void>}>=[]
let empty=false
for(const [i,layout] of (thumbnail ? layouts.slice(0,1) : layouts).entries()){
 const section=document.createElement('section');section.className='example';section.dataset.layout=layout
 const heading=document.createElement('h2');heading.textContent=labels.find(option => option.value === layout)!.label;const root=document.createElement('div');root.className='host';section.append(heading,root);document.getElementById('preview')!.append(section)
 let settings:AddonValues={...defaults,layout};let device:AddonValues={favorites:'[]'}
 const settingsListeners=new Set<(v:AddonValues)=>void>(),deviceListeners=new Set<(v:AddonValues)=>void>(),messages=new Set<(v:JsonValue)=>void>()
 const api=(deviceScope:boolean)=>({get:()=>deviceScope?device:settings,set:async(partial:AddonValues)=>{if(deviceScope)device={...device,...partial};else settings={...settings,...partial};for(const l of deviceScope?deviceListeners:settingsListeners)l(deviceScope?device:settings)},subscribe:(listener:(v:AddonValues)=>void)=>{const listeners=deviceScope?deviceListeners:settingsListeners;listeners.add(listener);return()=>{listeners.delete(listener)}}})
 const sample=()=>(empty?{...emptyState(),kind:'media.state'}:{...emptyState(),kind:'media.state',status:'ready',sessionId:'preview',source:'Preview',title:'Better Days',artist:'Luna River',album:'Horizons',artwork:empty?null:coverData,positionMs:84000,durationMs:236000,playing:false,capabilities:{playPause:true,previous:true,next:true,seek:true,shuffle:true,repeat:true}}) as unknown as JsonValue
 const connection:NativeConnection={state:'open',send:async value=>{for(const l of messages){l(sample());if(typeof value==='object'&&value!==null&&!Array.isArray(value))l({kind:'media.result',requestId:value.requestId??'',ok:true})}},onMessage:listener=>{messages.add(listener);return()=>{messages.delete(listener)}},onStateChange:()=>()=>{},close:()=>{messages.clear()}}
 const context={layer:{root,layerId:`preview-${i}`,settings:api(false),deviceSettings:api(true),native:{companion:{available:true,connect:async()=>connection},hooks:{available:false,status:()=>null,onStateChange:()=>()=>{}}},lifecycle:{onDispose:()=>()=>{}},actions:{on:()=>()=>{}},resources:{resolve:async()=>''},bus:{emit:()=>{},on:()=>()=>{}}},runtime:{mode:'interactive',surface:'interface',instance:{instanceId:'preview',displayIndex:0,displayCount:1,canonical:true,width:500,height:500}},bus:{emit:()=>{},on:()=>()=>{}},services:{connect:async()=>{throw new Error('No preview service')},provide:()=>{throw new Error('No preview service')}}} as CanvasAddonMountContext
 const dispose=mount(context);examples.push({refresh:()=>{for(const l of messages)l(sample())},dispose,appearance:partial=>api(false).set(partial)})
 queueMicrotask(()=>{for(const l of messages){l(sample());l({kind:'media.spectrum',bands:Array.from({length:32},(_,n)=>.08+Math.sin(n*.21)**2*.6)})}})
}
for (const id of ['background', 'backgroundColor', 'opacity', 'blur', 'foreground', 'accent', 'borderOpacity']) {
 const field = document.getElementById(id) as HTMLInputElement | HTMLSelectElement
 field.value = String(defaults[id])
 field.addEventListener('input', () => {
  const value = field instanceof HTMLInputElement && field.type === 'range' ? Number(field.value) : field.value
  for (const example of examples) void example.appearance({[id]:value})
  document.getElementById('background-color-control')!.hidden = (document.getElementById('background') as HTMLSelectElement).value !== 'color'
 })
}
document.getElementById('empty')!.addEventListener('click',()=>{empty=!empty;document.getElementById('empty')!.textContent=empty?'Show sample track':'Show empty states';for(const e of examples)e.refresh()})
window.addEventListener('pagehide',()=>{for(const e of examples)e.dispose()},{once:true})
