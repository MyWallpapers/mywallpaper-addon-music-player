import { mount } from './main'
import { layouts, emptyState } from './model'
import demoCover from '../assets/demo-cover.webp'
import type { AddonValues, CanvasAddonMountContext, JsonValue, NativeConnection } from '../generated/mywallpaper-runtime'
const blob = await (await fetch(demoCover)).blob()
const coverData = await new Promise<string>(resolve => { const reader = new FileReader(); reader.onload = () => resolve(String(reader.result)); reader.readAsDataURL(blob) })
const thumbnail = new URLSearchParams(location.search).get('view') === 'thumbnail'
if (thumbnail) document.body.classList.add('thumbnail')
const labels=['Minimal', 'Artwork + spectrum', 'Centered', 'Full artwork', 'Frosted controls', 'Compact bar', 'Editorial']
const examples: Array<{refresh:()=>void, dispose:()=>void}>=[]
let empty=false
for(const [i,layout] of (thumbnail ? layouts.slice(0,1) : layouts).entries()){
 const section=document.createElement('section');section.className='example';section.dataset.layout=layout
 const heading=document.createElement('h2');heading.textContent=labels[i];const root=document.createElement('div');root.className='host';section.append(heading,root);document.getElementById('preview')!.append(section)
 let settings:AddonValues={layout,opacity:.5,blur:16,showAlbum:true,showFavorite:true};let device:AddonValues={favorites:'[]'}
 const settingsListeners=new Set<(v:AddonValues)=>void>(),deviceListeners=new Set<(v:AddonValues)=>void>(),messages=new Set<(v:JsonValue)=>void>()
 const api=(deviceScope:boolean)=>({get:()=>deviceScope?device:settings,set:async(partial:AddonValues)=>{if(deviceScope)device={...device,...partial};else settings={...settings,...partial};for(const l of deviceScope?deviceListeners:settingsListeners)l(deviceScope?device:settings)},subscribe:(listener:(v:AddonValues)=>void)=>{const listeners=deviceScope?deviceListeners:settingsListeners;listeners.add(listener);return()=>{listeners.delete(listener)}}})
 const sample=()=>(empty?{...emptyState(),kind:'media.state'}:{...emptyState(),kind:'media.state',status:'ready',sessionId:'preview',source:'Preview',title:'Better Days',artist:'Luna River',album:'Horizons',artwork:empty?null:coverData,positionMs:84000,durationMs:236000,playing:false,capabilities:{playPause:true,previous:true,next:true,seek:true,shuffle:true,repeat:true}}) as unknown as JsonValue
 const connection:NativeConnection={state:'open',send:async value=>{for(const l of messages){l(sample());if(typeof value==='object'&&value!==null&&!Array.isArray(value))l({kind:'media.result',requestId:value.requestId??'',ok:true})}},onMessage:listener=>{messages.add(listener);return()=>{messages.delete(listener)}},onStateChange:()=>()=>{},close:()=>{messages.clear()}}
 const context={layer:{root,layerId:`preview-${i}`,settings:api(false),deviceSettings:api(true),native:{companion:{available:true,connect:async()=>connection},hooks:{available:false,status:()=>null,onStateChange:()=>()=>{}}},lifecycle:{onDispose:()=>()=>{}},actions:{on:()=>()=>{}},resources:{resolve:async()=>''},bus:{emit:()=>{},on:()=>()=>{}}},runtime:{mode:'interactive',surface:'interface',instance:{instanceId:'preview',displayIndex:0,displayCount:1,canonical:true,width:500,height:500}},bus:{emit:()=>{},on:()=>()=>{}},services:{connect:async()=>{throw new Error('No preview service')},provide:()=>{throw new Error('No preview service')}}} as CanvasAddonMountContext
 const dispose=mount(context);examples.push({refresh:()=>{for(const l of messages)l(sample())},dispose})
 queueMicrotask(()=>{for(const l of messages){l(sample());l({kind:'media.spectrum',bands:Array.from({length:32},(_,n)=>.08+Math.sin(n*.21)**2*.6)})}})
}
document.getElementById('empty')!.addEventListener('click',()=>{empty=!empty;document.getElementById('empty')!.textContent=empty?'Show sample track':'Show empty states';for(const e of examples)e.refresh()})
window.addEventListener('pagehide',()=>{for(const e of examples)e.dispose()},{once:true})
