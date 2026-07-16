
const $=(s,r=document)=>r.querySelector(s);
const $$=(s,r=document)=>[...r.querySelectorAll(s)];

/* theme */
const themeBtn=$('#theme-toggle');
const setTheme=t=>{document.documentElement.dataset.theme=t; themeBtn.textContent=t==='dark'?'🌙':'☀️';
  try{localStorage.setItem('hb-theme',t)}catch(e){}
  if(window.mermaid){try{document.querySelectorAll('.mermaid[data-processed]').length}catch(e){}}};
setTheme((()=>{try{return localStorage.getItem('hb-theme')||'dark'}catch(e){return 'dark'}})());
themeBtn.onclick=()=>setTheme(document.documentElement.dataset.theme==='dark'?'light':'dark');

/* mobile nav */
const toggle=$('#menu-toggle');
toggle&&(toggle.onclick=()=>document.body.classList.toggle('nav-open'));
$('#scrim').onclick=()=>document.body.classList.remove('nav-open');
$$('#sidebar a').forEach(a=>a.addEventListener('click',()=>document.body.classList.remove('nav-open')));

/* copy buttons */
$$('.copy-btn').forEach(btn=>btn.onclick=()=>{
  const code=btn.closest('.code-block').querySelector('.hl').innerText;
  navigator.clipboard.writeText(code).then(()=>{btn.textContent='copied';btn.classList.add('copied');
    setTimeout(()=>{btn.textContent='copy';btn.classList.remove('copied')},1400)});
});

/* reading progress */
addEventListener('scroll',()=>{const h=document.documentElement;const max=h.scrollHeight-h.clientHeight;
  $('#progress').style.width=(max>0?h.scrollTop/max*100:0)+'%';},{passive:true});

/* scroll-spy for TOC + sidebar subsections */
const tocLinks=$$('#toc-rail a, .nav-sub');
const heads=$$('.prose h2[id], .prose h3[id]');
if(heads.length){
  const spy=new IntersectionObserver(es=>{
    es.forEach(e=>{ if(!e.isIntersecting)return;
      tocLinks.forEach(a=>a.classList.toggle('active', a.getAttribute('data-target')===e.target.id));
    });
  },{rootMargin:'-12% 0px -78% 0px',threshold:0});
  heads.forEach(h=>spy.observe(h));
}

/* ---- cross-page search ---- */
const search=$('#search'), results=$('#search-results');
const INDEX=window.SEARCH_INDEX||[];
function esc(s){return s.replace(/[.*+?^${}()|[\]\\]/g,'\\$&');}
function mark(text,q){return text.replace(new RegExp(esc(q),'ig'),m=>`<mark>${m}</mark>`);}
function snippet(text,q){
  const i=text.toLowerCase().indexOf(q.toLowerCase());
  if(i<0)return text.slice(0,130);
  const s=Math.max(0,i-45);
  return (s>0?'…':'')+mark(text.slice(s,i+q.length+95),q)+'…';
}
let sel=-1, items=[];
function run(){
  const q=search.value.trim();
  if(q.length<2){results.hidden=true; return;}
  const ql=q.toLowerCase();
  const hits=[];
  for(const r of INDEX){
    const inHead=r.heading&&r.heading.toLowerCase().includes(ql);
    const inText=r.text.toLowerCase().includes(ql);
    if(inHead||inText){
      hits.push({...r, score:(inHead?2:0)+(r.heading?0:1)*0 + (inText?1:0)});
    }
  }
  hits.sort((a,b)=>b.score-a.score);
  const seen=new Set(); const uniq=[];
  for(const h of hits){const key=h.page+'#'+h.hid; if(seen.has(key))continue; seen.add(key); uniq.push(h); if(uniq.length>=30)break;}
  items=uniq; sel=-1;
  if(!uniq.length){results.innerHTML=`<div class="sr-empty">No matches for “${q}”.</div>`;}
  else{results.innerHTML=uniq.map((h,i)=>{
    const href=h.hid?`${h.page}#${h.hid}`:h.page;
    const head=h.heading?mark(h.heading,q):'Chapter overview';
    return `<a class="sr-item" data-i="${i}" href="${href}">
      <div class="sr-ch">Ch ${h.num} · ${h.title}</div>
      <div class="sr-head">${head}</div>
      <div class="sr-snip">${snippet(h.text,q)}</div></a>`;}).join('');}
  results.hidden=false;
}
let t; search&&search.addEventListener('input',()=>{clearTimeout(t);t=setTimeout(run,110);});
search&&search.addEventListener('focus',()=>{if(search.value.trim().length>=2)run();});
document.addEventListener('click',e=>{if(!e.target.closest('.search-wrap'))results.hidden=true;});
search&&search.addEventListener('keydown',e=>{
  if(results.hidden)return;
  if(e.key==='ArrowDown'){e.preventDefault();sel=Math.min(sel+1,items.length-1);}
  else if(e.key==='ArrowUp'){e.preventDefault();sel=Math.max(sel-1,0);}
  else if(e.key==='Enter'){const el=results.querySelector(`[data-i="${sel}"]`)||results.querySelector('.sr-item');if(el)location.href=el.getAttribute('href');return;}
  else return;
  $$('.sr-item',results).forEach(a=>a.classList.toggle('sel',+a.dataset.i===sel));
  const cur=results.querySelector('.sr-item.sel'); if(cur)cur.scrollIntoView({block:'nearest'});
});
document.addEventListener('keydown',e=>{
  if(e.key==='/'&&document.activeElement!==search){e.preventDefault();search.focus();}
  if(e.key==='Escape'){results.hidden=true;if(document.activeElement===search)search.blur();}
});

/* mermaid */
if(!window.__noMermaid&&window.mermaid){
  try{mermaid.initialize({startOnLoad:true,theme:document.documentElement.dataset.theme==='dark'?'dark':'default'});}catch(e){}
}
