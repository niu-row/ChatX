export const DASHBOARD_HTML = String.raw`<!doctype html>
<html lang="zh-CN">
<head>
  <meta charset="utf-8">
  <meta name="viewport" content="width=device-width, initial-scale=1">
  <title>ChatX 本地控制台</title>
  <style>
    :root{color-scheme:dark;--bg:#07100e;--panel:#0d1916;--line:#263b34;--text:#ecf6f1;--muted:#8ca49a;--green:#5cf2a5;--red:#ff8278;--amber:#ffc862}
    *{box-sizing:border-box} body{margin:0;background:linear-gradient(145deg,#060b0a,#091411 55%,#050807);color:var(--text);font:15px/1.5 Inter,"Segoe UI","Microsoft YaHei",sans-serif}
    button,input,textarea,select{font:inherit}.shell{width:min(1220px,calc(100% - 30px));margin:auto;padding:26px 0 42px}header{display:flex;justify-content:space-between;gap:16px;align-items:center;margin-bottom:18px}
    h1{margin:0;font-size:1.35rem}.sub{color:var(--muted);font-size:.84rem}.pill{display:inline-flex;align-items:center;gap:7px;border:1px solid var(--line);border-radius:999px;padding:7px 11px;color:var(--muted);background:#0a1512}
    .dot{width:8px;height:8px;border-radius:50%;background:#6e8179}.dot.on{background:var(--green);box-shadow:0 0 12px #5cf2a5}.grid{display:grid;grid-template-columns:1.05fr .95fr;gap:16px}.stack{display:grid;gap:16px}
    .card{border:1px solid var(--line);border-radius:18px;background:linear-gradient(160deg,#101e1af2,#09110ff2);overflow:hidden}.head{padding:18px 20px 0}.eyebrow{color:var(--green);font:700 .7rem ui-monospace,Consolas,monospace;letter-spacing:.12em;text-transform:uppercase}.head h2{margin:5px 0 0;font-size:1rem}.body{padding:18px 20px 20px}
    label.field{display:grid;gap:6px;margin-bottom:13px;color:#c5d4ce;font-size:.86rem}input,textarea,select{width:100%;color:var(--text);background:#07100e;border:1px solid #304a40;border-radius:10px;padding:11px 12px;outline:none}textarea{min-height:105px;resize:vertical;font-family:ui-monospace,Consolas,monospace;font-size:.8rem}input:focus,textarea:focus,select:focus{border-color:var(--green)}
    .row{display:flex;gap:9px;align-items:center}.row>*{flex:1}.row .auto{flex:0 0 auto}.actions{display:flex;gap:9px;margin-top:13px}button{border:0;border-radius:10px;padding:10px 14px;cursor:pointer;font-weight:700}button:disabled{opacity:.45;cursor:not-allowed}.primary{background:var(--green);color:#062014}.secondary{background:#172621;color:#d4e0db;border:1px solid #304a40}.danger{background:#3a1d1c;color:#ffd8d4;border:1px solid #77423e}
    .help{color:var(--muted);font-size:.75rem}.status{display:grid;grid-template-columns:repeat(2,minmax(0,1fr));gap:9px}.fact{padding:10px 11px;border:1px solid #22362f;border-radius:10px;background:#091310;min-width:0}.fact span{display:block;color:var(--muted);font-size:.7rem}.fact strong{display:block;white-space:nowrap;overflow:hidden;text-overflow:ellipsis;font:600 .79rem ui-monospace,Consolas,monospace;margin-top:2px}
    .perm{display:flex;align-items:center;justify-content:space-between;gap:12px;padding:9px 10px;border:1px solid #22362f;border-radius:10px;background:#091310;margin-bottom:8px}.perm small{display:block;color:var(--muted);font-size:.7rem}.perm input{width:18px;height:18px;flex:0 0 auto;accent-color:var(--green)}
    .log{height:250px;overflow:auto;background:#050a08;border:1px solid #1f312a;border-radius:10px;padding:12px;color:#a8bcb4;font:12px/1.6 ui-monospace,Consolas,monospace;white-space:pre-wrap;word-break:break-word}.error{margin-top:10px;padding:10px;border:1px solid #7b3b37;background:#3b1b1a;color:#ffd2ce;border-radius:10px;white-space:pre-wrap}.check{display:flex;justify-content:space-between;gap:12px;padding:8px 0;border-bottom:1px solid #1d2f28}.check:last-child{border-bottom:0}.ok{color:var(--green)}.bad{color:var(--red)}.warn{color:var(--amber)}
    .preset{display:grid;grid-template-columns:1fr auto;gap:8px;margin-bottom:12px}.secure{padding:10px;border:1px solid #29443a;border-radius:10px;background:#091511;color:var(--muted);font-size:.76rem;margin-top:10px}footer{text-align:center;color:#62776f;font-size:.74rem;margin-top:17px}
    @media(max-width:900px){.grid{grid-template-columns:1fr}}@media(max-width:560px){.shell{width:calc(100% - 18px)}header{align-items:flex-start}.hide-sm{display:none}.status{grid-template-columns:1fr}.row,.actions{flex-direction:column;align-items:stretch}.row .auto{flex:1}.preset{grid-template-columns:1fr}}
  </style>
</head>
<body>
<div class="shell">
  <header><div><h1>ChatX 本地控制台</h1><div class="sub">MCP、Secure Tunnel、权限与诊断</div></div><div class="pill hide-sm"><span class="dot on"></span>仅限本机访问</div></header>
  <div class="grid">
    <div class="stack">
      <section class="card"><div class="head"><div class="eyebrow">Connect</div><h2>OpenAI Secure MCP Tunnel</h2></div><div class="body">
        <label class="field">Tunnel ID<input id="tunnelId" autocomplete="off" spellcheck="false" placeholder="tunnel_..."></label>
        <label class="field">Runtime API Key<input id="apiKey" type="password" autocomplete="off" spellcheck="false" placeholder="输入 Runtime API Key"></label>
        <div class="row"><label class="row" style="justify-content:flex-start"><input class="auto" id="rememberKey" type="checkbox" style="width:18px;height:18px"><span>用 Windows DPAPI 安全保存 Runtime Key</span></label><button id="clearKey" class="danger auto" type="button">清除已保存 Key</button></div>
        <div class="help" id="keyHelp">密钥不会进入 settings.json 或日志。</div>
        <div class="actions"><button class="primary" id="connect" type="button">连接并启动</button><button class="secondary" id="stop" type="button">停止 Tunnel</button></div>
        <div class="secure">Tunnel discovery/probe 会显式发送 <b>Content-Type: application/json</b>；启动后使用 tunnel-client 自己的 <b>/readyz</b> 判定是否真正就绪。</div>
        <div class="error" id="error" hidden></div>
      </div></section>

      <section class="card"><div class="head"><div class="eyebrow">Permissions</div><h2>权限与允许目录</h2></div><div class="body">
        <div class="preset"><select id="preset"><option value="safe">安全</option><option value="developer">开发</option><option value="unrestricted">完全开放</option><option value="custom">自定义</option></select><button id="applyPreset" class="secondary">应用预设</button></div>
        <div id="permissionList"></div>
        <label class="field" style="margin-top:12px">允许目录（每行一个绝对或相对路径）<textarea id="roots"></textarea></label>
        <div class="actions"><button id="saveRoots" class="secondary">保存允许目录</button></div>
        <div class="help">Shell 命令不受允许目录沙箱约束；“高级 Git”可通过 Git 配置执行外部程序，因此必须与 Shell 同时开启。</div>
      </div></section>
    </div>

    <div class="stack">
      <section class="card"><div class="head"><div class="eyebrow">Status</div><h2>运行状态</h2></div><div class="body"><div class="status">
        <div class="fact"><span>MCP 服务</span><strong id="service">-</strong></div><div class="fact"><span>Tunnel</span><strong id="tunnel">-</strong></div>
        <div class="fact"><span>权限预设</span><strong id="presetFact">-</strong></div><div class="fact"><span>Runtime Key</span><strong id="keyFact">-</strong></div>
        <div class="fact"><span>允许目录</span><strong id="rootsFact">-</strong></div><div class="fact"><span>设置版本</span><strong id="settingsVersion">-</strong></div>
      </div></div></section>

      <section class="card"><div class="head"><div class="eyebrow">Diagnostics</div><h2>诊断</h2></div><div class="body"><button id="diagnose" class="secondary">运行诊断</button><div id="diagnostics" style="margin-top:10px"><div class="help">尚未运行诊断。</div></div></div></section>

      <section class="card"><div class="head"><div class="eyebrow">Activity</div><h2>连接日志</h2></div><div class="body"><div class="log" id="log">尚无操作。</div></div></section>
    </div>
  </div>
  <footer id="settingsFile">ChatX</footer>
</div>
<script>
const $ = (id) => document.getElementById(id);
let state = null;
let busy = false;
const permissionMeta = [
  ['filesystemRead','读取文件','目录列表、读取、搜索和元数据'],
  ['filesystemWrite','修改文件','写入、编辑、复制、移动和删除'],
  ['gitRead','Git 读取','status、diff、log'],
  ['gitWrite','Git 写入','受约束的 stage、unstage、branch、commit'],
  ['gitAdvanced','高级 Git','任意 git 参数；等同 Shell，需同时开启 Shell'],
  ['shell','Shell 命令','高权限；不受允许目录边界约束'],
  ['fullAccess','完整文件系统访问','绕过允许目录边界']
];

function showError(message){$('error').hidden=!message;$('error').textContent=message||''}
function renderPermissionRows(permissions){
  const host=$('permissionList');host.innerHTML='';
  permissionMeta.forEach(function(meta){
    const row=document.createElement('label');row.className='perm';
    const copy=document.createElement('span');const strong=document.createElement('strong');strong.textContent=meta[1];const small=document.createElement('small');small.textContent=meta[2];copy.append(strong,small);
    const input=document.createElement('input');input.type='checkbox';input.checked=Boolean(permissions[meta[0]]);input.disabled=busy;input.dataset.permission=meta[0];
    input.addEventListener('change',async function(){
      if((meta[0]==='fullAccess'||meta[0]==='gitAdvanced'||meta[0]==='shell')&&input.checked&&!confirm('这是高权限选项。确定启用？')){input.checked=false;return}
      await updateSettings({[meta[0]]:input.checked});
    });
    row.append(copy,input);host.append(row);
  });
}
function render(payload){
  const s=payload.status||payload;state=s;const t=s.tunnel;const running=t.state==='running';
  $('service').textContent=s.service.name+' '+s.service.version;
  $('tunnel').textContent=(t.installed?t.version.split(' ')[0]:'未安装')+' · '+t.state;
  $('presetFact').textContent=s.policy.permissionPreset;
  $('keyFact').textContent=s.connection.runtimeKeySaved?'DPAPI 已保存':(s.connection.runtimeKeySupported?'未保存':'不支持持久化');
  $('rootsFact').textContent=s.policy.fullAccess?'全部路径':String(s.policy.roots.length)+' 个目录';
  $('rootsFact').title=s.policy.roots.join('; ');$('settingsVersion').textContent=String(s.settings.version);
  $('preset').value=s.policy.permissionPreset;$('roots').value=s.policy.roots.join('\n');renderPermissionRows(s.policy.permissions||{});
  if(s.connection.tunnelId)$('tunnelId').value=s.connection.tunnelId;
  $('settingsFile').textContent='设置文件：'+s.connection.settingsFile;
  $('rememberKey').disabled=!s.connection.runtimeKeySupported||busy;$('clearKey').disabled=!s.connection.runtimeKeySaved||busy;
  $('keyHelp').textContent=s.connection.runtimeKeySupported?(s.connection.runtimeKeySaved?'已使用 Windows DPAPI（CurrentUser）加密保存；API Key 可留空直接重连。':'勾选后会用 Windows DPAPI（CurrentUser）加密保存，不写入 settings.json。'):'当前平台不支持安全持久化，Key 仅驻留本次操作。';
  if(s.connection.runtimeKeySaved&&!$('apiKey').value)$('apiKey').placeholder='已安全保存，可留空';
  $('connect').disabled=busy||running||t.state==='connecting'||!t.installed;$('stop').disabled=busy||!running;
  $('connect').textContent=t.state==='connecting'?'正在连接…':running?'已连接':'连接并启动';
  $('log').textContent=(s.logs&&s.logs.length?s.logs.join('\n'):'尚无操作。');$('log').scrollTop=$('log').scrollHeight;
  showError(t.lastError||'');
}
async function refresh(){try{const r=await fetch('/api/tunnel/status',{cache:'no-store'});render(await r.json())}catch(e){showError(String(e))}}
async function post(url,body){const r=await fetch(url,{method:'POST',headers:{'content-type':'application/json'},body:JSON.stringify(body||{})});const data=await r.json();if(!r.ok)throw new Error(data.error||('HTTP '+r.status));return data}
async function updateSettings(patch){busy=true;try{const data=await post('/api/settings',patch);render(data.status)}catch(e){showError(e.message||String(e))}finally{busy=false;await refresh()}}
$('connect').addEventListener('click',async function(){busy=true;showError('');try{const data=await post('/api/tunnel/connect',{tunnelId:$('tunnelId').value.trim(),apiKey:$('apiKey').value,rememberKey:$('rememberKey').checked});$('apiKey').value='';render(data.status)}catch(e){showError(e.message||String(e))}finally{busy=false;await refresh()}});
$('stop').addEventListener('click',async function(){busy=true;try{const data=await post('/api/tunnel/stop',{});render(data.status)}catch(e){showError(e.message||String(e))}finally{busy=false;await refresh()}});
$('clearKey').addEventListener('click',async function(){if(!confirm('清除本机安全保存的 Runtime Key？'))return;busy=true;try{const data=await post('/api/tunnel/key/clear',{});render(data.status)}catch(e){showError(e.message||String(e))}finally{busy=false;await refresh()}});
$('applyPreset').addEventListener('click',async function(){const preset=$('preset').value;if(preset==='custom')return;if(preset==='unrestricted'&&!confirm('“完全开放”会启用 Shell、完整文件系统访问和高级 Git。确定应用？'))return;await updateSettings({preset:preset})});
$('saveRoots').addEventListener('click',async function(){const roots=$('roots').value.split(/\r?\n/).map(function(x){return x.trim()}).filter(Boolean);if(!roots.length){showError('至少保留一个允许目录。');return}await updateSettings({roots:roots})});
$('diagnose').addEventListener('click',async function(){const host=$('diagnostics');host.innerHTML='<div class="help">正在诊断…</div>';try{const r=await fetch('/api/diagnostics',{cache:'no-store'});const data=await r.json();host.innerHTML='';(data.checks||[]).forEach(function(c){const row=document.createElement('div');row.className='check';const name=document.createElement('span');name.textContent=c.name;const value=document.createElement('span');value.className=c.status==='ok'?'ok':(c.status==='warn'?'warn':'bad');value.textContent=c.message;row.append(name,value);host.append(row)})}catch(e){host.textContent=String(e)}});
let refreshTimer=null;async function refreshLoop(){if(!document.hidden)await refresh();refreshTimer=setTimeout(refreshLoop,document.hidden?15000:2500)}document.addEventListener('visibilitychange',function(){if(!document.hidden){if(refreshTimer)clearTimeout(refreshTimer);refreshLoop()}});refreshLoop();
</script>
</body>
</html>`;
