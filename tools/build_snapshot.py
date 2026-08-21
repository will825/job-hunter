#!/usr/bin/env python3
"""Generate a browsable HTML snapshot from jobs.db.

Read-only view of what the tool has stored — ranked to your profile, with
tier/region/work-mode/seniority filters. Not the real app UI (that's Phase 5);
just a quick way to eyeball and test results. Run via ./refresh.sh.
"""
import sqlite3, json, os, sys

DB = os.path.join(os.path.dirname(__file__), "..", "jobs.db")
OUT = os.path.join(os.path.dirname(__file__), "..", "jobs_snapshot.html")

if not os.path.exists(DB):
    sys.exit("jobs.db not found — run `cargo run` first to fetch jobs.")

c = sqlite3.connect(DB)
rows = c.execute("""SELECT company,title,location,source,url,tier,keyword_score,
                           work_mode,region,seniority,substr(description,1,280)
                    FROM jobs ORDER BY keyword_score DESC, company""").fetchall()
cols = ['company','title','location','source','url','tier','score',
        'work_mode','region','seniority','desc']
jobs = [dict(zip(cols, r)) for r in rows]
for j in jobs:
    j['desc'] = (j['desc'] or '').strip()
data = json.dumps(jobs)
tier_counts = {}
for j in jobs:
    tier_counts[j['tier']] = tier_counts.get(j['tier'], 0) + 1
tc = " · ".join(f"{k}: {v}" for k, v in sorted(tier_counts.items(), key=lambda x: -x[1]))

page = r"""<!doctype html><html><head><meta charset=utf-8><meta name=viewport content="width=device-width,initial-scale=1"><title>Job Hunter — profile-ranked</title>
<style>
:root{color-scheme:dark}*{box-sizing:border-box}
body{font:15px/1.5 -apple-system,system-ui,sans-serif;margin:0;background:#0e1116;color:#e6e9ef}
header{position:sticky;top:0;background:#161b22;border-bottom:1px solid #2b3240;padding:14px 20px;z-index:10}
h1{margin:0 0 2px;font-size:17px}.meta{color:#8b96a5;font-size:12px}
.controls{display:flex;gap:8px;flex-wrap:wrap;margin-top:10px}
input,select{padding:8px 10px;border-radius:8px;border:1px solid #2b3240;background:#0e1116;color:#e6e9ef;font-size:14px}
#q{flex:1;min-width:180px}
.wrap{padding:14px 20px;max-width:1000px;margin:0 auto}.count{color:#8b96a5;font-size:13px;margin-bottom:10px}
.card{border:1px solid #2b3240;border-radius:10px;padding:12px 14px;margin-bottom:9px;background:#141922;display:flex;gap:12px;align-items:flex-start}
.sc{flex:0 0 auto;width:42px;height:42px;border-radius:9px;display:flex;align-items:center;justify-content:center;font-weight:700;font-size:15px;background:#1d2530;color:#cdd6e2}
.body{flex:1;min-width:0}.card h2{margin:0 0 3px;font-size:15px}
.card a.title{color:#7cc5ff;text-decoration:none}.card a.title:hover{text-decoration:underline}
.row{display:flex;gap:8px;flex-wrap:wrap;align-items:center;color:#9aa5b4;font-size:12px;margin:3px 0}
.badge{padding:2px 8px;border-radius:20px;font-size:10px;font-weight:700;text-transform:uppercase;letter-spacing:.4px}
.apply_now{background:#123d2a;color:#5fd398}.strong{background:#1d3a4d;color:#7cc5ff}.maybe{background:#3a3410;color:#e6c860}.skip{background:#2b2b2b;color:#888}
.pill{background:#1d2530;color:#aeb8c6;padding:2px 8px;border-radius:20px;font-size:11px}
.desc{color:#8b96a5;font-size:12px;max-height:2.6em;overflow:hidden;margin-top:4px}
</style></head><body>
<header><h1>Job Hunter — __N__ jobs, ranked to your profile</h1>
<div class=meta>__TC__ · scored against profile.toml · click a title to open the apply page</div>
<div class=controls>
<input id=q placeholder="Search title / company…">
<select id=tier><option value="">All tiers</option><option>apply_now</option><option>strong</option><option>maybe</option><option>skip</option></select>
<select id=work_mode><option value="">Any mode</option><option>remote</option><option>hybrid</option><option>onsite</option><option>unknown</option></select>
<select id=region><option value="">Any region</option><option value=us>US</option><option value=uk>UK</option><option value=emea>EMEA</option><option value=apac>APAC</option><option value=canada>Canada</option><option value=latam>LATAM</option><option value=unknown>Unknown</option></select>
<select id=seniority><option value="">Any level</option><option>junior</option><option>mid</option><option>senior</option><option>staff</option><option>lead</option><option>unknown</option></select>
</div></header>
<div class=wrap><div class=count id=count></div><div id=list></div></div>
<script>
const JOBS=__DATA__;const $=id=>document.getElementById(id);
function esc(s){return (s||'').replace(/[&<>]/g,m=>({'&':'&amp;','<':'&lt;','>':'&gt;'}[m]))}
function render(){
 const f=$('q').value.trim().toLowerCase(),t=$('tier').value,w=$('work_mode').value,r=$('region').value,s=$('seniority').value;
 let out='',n=0;
 for(const j of JOBS){
  if(t&&j.tier!==t)continue;if(w&&j.work_mode!==w)continue;if(r&&j.region!==r)continue;if(s&&j.seniority!==s)continue;
  if(f&&!((j.title+' '+j.company).toLowerCase().includes(f)))continue;
  n++;
  out+=`<div class=card><div class=sc>${j.score}</div><div class=body>`+
   `<h2><a class=title href="${j.url}" target=_blank rel=noopener>${esc(j.title)}</a></h2>`+
   `<div class=row><span class="badge ${j.tier}">${j.tier.replace('_',' ')}</span>`+
   `<span class=pill>${esc(j.company)}</span><span class=pill>${j.work_mode}</span>`+
   `<span class=pill>${j.region}</span><span class=pill>${j.seniority}</span>`+
   `<span style="color:#6b7480">${esc(j.location)}</span></div>`+
   `<div class=desc>${esc(j.desc)}…</div></div></div>`;
 }
 $('list').innerHTML=out||'<p class=meta>No matches.</p>';
 $('count').textContent=n+' of '+JOBS.length+' jobs';
}
['q','tier','work_mode','region','seniority'].forEach(id=>$(id).addEventListener('input',render));render();
</script></body></html>"""
page = (page.replace('__N__', str(len(jobs)))
            .replace('__TC__', tc)
            .replace('__DATA__', data))
with open(OUT, 'w') as f:
    f.write(page)
print(f"Wrote {os.path.relpath(OUT)} — {len(jobs)} jobs ({tc})")
