<!doctype html>
<html lang="en"><head><meta charset="utf-8"><meta name="viewport" content="width=device-width,initial-scale=1">
<title>Join @@MESH_NAME@@</title>
<link rel="icon" href="/logo.png" type="image/png">
<style>
:root{--bg:#07080b;--card:#0e1117;--line:rgba(255,255,255,.08);--text:#e7eaf0;--dim:#98a2b6;--accent:#5b7cfa}
@media (prefers-color-scheme:light){:root{--bg:#f6f7fb;--card:#fff;--line:rgba(0,0,0,.08);--text:#12151c;--dim:#5b6375}}
*{box-sizing:border-box}body{margin:0;background:var(--bg);color:var(--text);font:15px/1.55 system-ui,-apple-system,"Segoe UI",sans-serif;padding:32px 16px}
main{max-width:640px;margin:0 auto}h1{font-size:26px;margin:0 0 6px;letter-spacing:-.02em}p{color:var(--dim);margin:0 0 20px}
.card{background:var(--card);border:1px solid var(--line);border-radius:14px;padding:16px;margin:0 0 12px}
.label{font-size:12px;text-transform:uppercase;letter-spacing:.08em;color:var(--dim);margin-bottom:8px}
code{display:block;font:13px/1.5 ui-monospace,Consolas,monospace;word-break:break-all;background:rgba(127,127,127,.1);padding:10px 12px;border-radius:9px;cursor:pointer}
a.btn{display:inline-block;background:var(--accent);color:#fff;text-decoration:none;padding:10px 16px;border-radius:10px;font-weight:600}
</style></head><body><main>
<img src="/logo.png" alt="GradApp" width="44" height="44" style="display:block;margin-bottom:16px">
<h1>Lend your GPU to @@MESH_NAME@@</h1>
<p>Easiest: install GradApp, open it, and press <b>Join</b>. It finds this mesh by itself.</p>
<div class="card"><a class="btn" href="https://github.com/UJESH2K/gradapp/releases/latest">Get GradApp</a></div>
<p>Or paste one line into a terminal on the machine with the GPU:</p>
<div class="card"><div class="label">Windows (PowerShell)</div><code>irm @@WEB@@/join.ps1 | iex</code></div>
<div class="card"><div class="label">macOS or Linux</div><code>curl -fsSL @@WEB@@/join.sh | sh</code></div>
<p style="font-size:13px">Click a command to copy it. Ctrl+C in that terminal leaves the mesh.</p>
</main><script>document.querySelectorAll("code").forEach(c=>c.onclick=()=>navigator.clipboard&&navigator.clipboard.writeText(c.textContent))</script></body></html>