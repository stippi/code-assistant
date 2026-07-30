#!/usr/bin/env python3
"""Render coverage_summary.json into a self-contained single-page HTML report."""
import json, os, datetime, subprocess

REPO = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
with open(os.path.join(REPO, "coverage_summary.json")) as f:
    data = json.load(f)

try:
    commit = subprocess.check_output(["git", "-C", REPO, "rev-parse", "--short", "HEAD"]).decode().strip()
except Exception:
    commit = "unknown"

generated = datetime.datetime.now().strftime("%Y-%m-%d %H:%M")
payload = json.dumps(data)

HTML = """<!DOCTYPE html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>code-assistant &middot; Test Coverage</title>
<style>
  :root {
    --bg: #0f1420;
    --bg-soft: #161c2b;
    --card: #1b2333;
    --card-2: #212b3f;
    --line: #2b3550;
    --text: #e6ecf7;
    --muted: #93a1c0;
    --accent: #6ea8fe;
    --good: #38d39f;
    --mid: #f4c542;
    --bad: #f2637a;
    --track: #2a3450;
    --shadow: 0 10px 30px rgba(0,0,0,.35);
  }
  * { box-sizing: border-box; }
  body {
    margin: 0; background: radial-gradient(1200px 600px at 80% -10%, #1d2740 0%, var(--bg) 55%);
    color: var(--text); font: 15px/1.5 -apple-system, BlinkMacSystemFont, "Segoe UI", Roboto, Helvetica, Arial, sans-serif;
    padding: 40px 24px 80px;
  }
  .wrap { max-width: 1120px; margin: 0 auto; }
  header { margin-bottom: 28px; }
  h1 { font-size: 26px; margin: 0 0 6px; letter-spacing: .2px; }
  .sub { color: var(--muted); font-size: 14px; }
  .sub code { background: var(--bg-soft); padding: 2px 6px; border-radius: 6px; color: var(--accent); }

  .cards { display: grid; grid-template-columns: repeat(auto-fit,minmax(190px,1fr)); gap: 16px; margin: 24px 0 32px; }
  .stat { background: linear-gradient(180deg,var(--card) 0%, var(--bg-soft) 100%); border: 1px solid var(--line);
    border-radius: 16px; padding: 18px 20px; box-shadow: var(--shadow); }
  .stat .label { color: var(--muted); font-size: 12px; text-transform: uppercase; letter-spacing: .8px; }
  .stat .value { font-size: 30px; font-weight: 700; margin-top: 6px; }
  .stat .value small { font-size: 14px; color: var(--muted); font-weight: 500; }

  .toolbar { display:flex; align-items:center; gap:12px; flex-wrap:wrap; margin-bottom: 14px; }
  .toolbar .hint { color: var(--muted); font-size: 13px; }
  .toolbar button { background: var(--card-2); color: var(--text); border:1px solid var(--line); border-radius:8px;
    padding:7px 12px; cursor:pointer; font-size:13px; }
  .toolbar button:hover { border-color: var(--accent); }

  table { width: 100%; border-collapse: collapse; background: var(--card); border-radius: 14px; overflow: hidden;
    box-shadow: var(--shadow); border: 1px solid var(--line); }
  thead th { text-align: left; font-size: 12px; text-transform: uppercase; letter-spacing: .6px; color: var(--muted);
    padding: 14px 16px; background: var(--bg-soft); cursor: pointer; user-select:none; white-space:nowrap; }
  thead th.num { text-align: right; }
  thead th .arrow { opacity:.5; font-size:11px; }
  tbody td { padding: 11px 16px; border-top: 1px solid var(--line); vertical-align: middle; }
  td.num { text-align: right; font-variant-numeric: tabular-nums; color: var(--muted); }
  tr.crate { cursor: pointer; }
  tr.crate:hover { background: var(--card-2); }
  tr.crate td:first-child { font-weight: 600; }
  .caret { display:inline-block; width: 14px; color: var(--muted); transition: transform .15s; }
  tr.crate.open .caret { transform: rotate(90deg); }

  tr.mod { background: #131a29; font-size: 14px; }
  tr.mod td:first-child { padding-left: 40px; color: var(--muted); }
  tr.mod td:first-child .mname { color: var(--text); }

  .bar { position: relative; height: 8px; width: 130px; background: var(--track); border-radius: 6px; overflow: hidden; display:inline-block; vertical-align: middle; }
  .bar > span { position:absolute; left:0; top:0; bottom:0; border-radius: 6px; }
  .pct { display:inline-block; min-width: 52px; text-align:right; font-variant-numeric: tabular-nums; margin-left: 10px; font-weight:600; }
  .cellbar { display:flex; align-items:center; justify-content:flex-end; gap: 4px; }

  .pill { display:inline-block; padding: 2px 9px; border-radius: 999px; font-size:12px; font-weight:600; }
  footer { color: var(--muted); font-size: 12px; margin-top: 28px; text-align:center; }
  .legend { display:flex; gap:18px; flex-wrap:wrap; color:var(--muted); font-size:12px; margin-top:10px; }
  .legend span b { display:inline-block; width:10px; height:10px; border-radius:3px; margin-right:6px; vertical-align:middle; }
  .note { color: var(--muted); font-size: 13px; margin: 4px 0 20px; }
</style>
</head>
<body>
<div class="wrap">
  <header>
    <h1>Test Coverage &middot; code-assistant</h1>
    <div class="sub">Line coverage via <code>cargo llvm-cov</code> &middot; commit <code>__COMMIT__</code> &middot; generated __GENERATED__</div>
  </header>

  <div class="cards" id="cards"></div>

  <p class="note">Measured with <code>cargo llvm-cov --workspace --no-default-features</code>. Percentages are line coverage; function coverage is shown in the &ldquo;Fn&rdquo; column. One test file failed in this environment (<code>write_to_piped_session</code>); the report was generated from the collected profile data.</p>

  <div class="toolbar">
    <button id="expandAll">Expand all modules</button>
    <button id="collapseAll">Collapse all</button>
    <span class="hint">Click a column header to sort &middot; click a crate row to show its modules</span>
  </div>

  <table id="tbl">
    <thead>
      <tr>
        <th data-key="crate">Crate / module</th>
        <th data-key="line_pct" class="num">Line coverage <span class="arrow">&#9660;</span></th>
        <th data-key="lines_total" class="num">Lines</th>
        <th data-key="fn_pct" class="num">Fn</th>
        <th data-key="files" class="num">Files</th>
        <th data-key="tests" class="num">Tests</th>
      </tr>
    </thead>
    <tbody id="tbody"></tbody>
  </table>

  <div class="legend">
    <span><b style="background:var(--good)"></b>&ge; 80&nbsp;%</span>
    <span><b style="background:var(--mid)"></b>50&ndash;79&nbsp;%</span>
    <span><b style="background:var(--bad)"></b>&lt; 50&nbsp;%</span>
    <span><b style="background:var(--track)"></b>no tests / no code</span>
  </div>

  <footer>__TESTS__ test functions &middot; __CRATES__ crates &middot; __LC__/__LT__ lines covered (__GP__&nbsp;%)</footer>
</div>

<script>
const DATA = __PAYLOAD__;

function color(pct){
  if(pct===null||pct===undefined) return 'var(--track)';
  if(pct>=80) return 'var(--good)';
  if(pct>=50) return 'var(--mid)';
  return 'var(--bad)';
}
function pctText(p){ return (p===null||p===undefined)?'&mdash;':(p.toFixed(1)+'%'); }
function barCell(p){
  const c = color(p);
  const w = (p===null||p===undefined)?0:Math.max(2,p);
  return `<div class="cellbar"><span class="bar"><span style="width:${w}%;background:${c}"></span></span><span class="pct" style="color:${c}">${pctText(p)}</span></div>`;
}

// summary cards
const g = DATA.grand;
const cards = [
  {label:'Overall coverage', value:g.line_pct.toFixed(1)+'<small>%</small>', c:color(g.line_pct)},
  {label:'Lines covered', value:g.lines_covered.toLocaleString('en-US')+'<small> / '+g.lines_total.toLocaleString('en-US')+'</small>'},
  {label:'Function coverage', value:g.fn_pct.toFixed(1)+'<small>%</small>', c:color(g.fn_pct)},
  {label:'Test functions', value:g.tests_total.toLocaleString('en-US')},
  {label:'Crates', value:g.crate_count},
];
document.getElementById('cards').innerHTML = cards.map(c=>
  `<div class="stat"><div class="label">${c.label}</div><div class="value" ${c.c?`style="color:${c.c}"`:''}>${c.value}</div></div>`).join('');

let sortKey='line_pct', sortDir=-1;
const tbody = document.getElementById('tbody');

function render(){
  const crates = [...DATA.crates].sort((a,b)=>{
    let x=a[sortKey], y=b[sortKey];
    if(sortKey==='crate'){ return sortDir*String(x).localeCompare(String(y)); }
    x = (x===null||x===undefined)?-1:x; y=(y===null||y===undefined)?-1:y;
    return sortDir*(x-y);
  });
  let html='';
  crates.forEach((c,ci)=>{
    const fileCount = c.modules.reduce((s,m)=>s+m.files,0);
    html += `<tr class="crate" data-i="${ci}">
      <td><span class="caret">&#9656;</span> ${c.crate}</td>
      <td class="num">${barCell(c.line_pct)}</td>
      <td class="num">${c.lines_total.toLocaleString('en-US')}</td>
      <td class="num" style="color:${color(c.fn_pct)}">${pctText(c.fn_pct)}</td>
      <td class="num">${fileCount}</td>
      <td class="num">${c.tests}</td>
    </tr>`;
    const mods = [...c.modules].sort((a,b)=>{
      let x=(a.line_pct===null)?-1:a.line_pct, y=(b.line_pct===null)?-1:b.line_pct; return y-x;
    });
    mods.forEach(m=>{
      html += `<tr class="mod" data-parent="${ci}" style="display:none">
        <td><span class="mname">${m.module}</span></td>
        <td class="num">${barCell(m.line_pct)}</td>
        <td class="num">${m.lines_total.toLocaleString('en-US')}</td>
        <td class="num" style="color:${color(m.fn_pct)}">${pctText(m.fn_pct)}</td>
        <td class="num">${m.files}</td>
        <td class="num">&mdash;</td>
      </tr>`;
    });
  });
  tbody.innerHTML = html;
  bind();
}

function bind(){
  document.querySelectorAll('tr.crate').forEach(row=>{
    row.addEventListener('click',()=>{
      const i=row.dataset.i; const open=row.classList.toggle('open');
      document.querySelectorAll(`tr.mod[data-parent="${i}"]`).forEach(m=> m.style.display = open?'table-row':'none');
    });
  });
}

document.querySelectorAll('thead th').forEach(th=>{
  th.addEventListener('click',()=>{
    const k=th.dataset.key;
    if(k===sortKey){ sortDir*=-1; } else { sortKey=k; sortDir = (k==='crate')?1:-1; }
    document.querySelectorAll('thead th .arrow').forEach(a=>a.remove());
    const arrow=document.createElement('span'); arrow.className='arrow';
    arrow.innerHTML = sortDir<0?' &#9660;':' &#9650;'; th.appendChild(arrow);
    render();
  });
});

document.getElementById('expandAll').addEventListener('click',()=>{
  document.querySelectorAll('tr.crate:not(.open)').forEach(r=>r.click());
});
document.getElementById('collapseAll').addEventListener('click',()=>{
  document.querySelectorAll('tr.crate.open').forEach(r=>r.click());
});

render();
</script>
</body>
</html>
"""

html = (HTML
    .replace("__COMMIT__", commit)
    .replace("__GENERATED__", generated)
    .replace("__PAYLOAD__", payload)
    .replace("__TESTS__", str(data["grand"]["tests_total"]))
    .replace("__CRATES__", str(data["grand"]["crate_count"]))
    .replace("__LC__", f'{data["grand"]["lines_covered"]:,}')
    .replace("__LT__", f'{data["grand"]["lines_total"]:,}')
    .replace("__GP__", str(data["grand"]["line_pct"]))
)

out = os.path.join(REPO, "coverage-report.html")
with open(out, "w") as f:
    f.write(html)
print("wrote", out, len(html), "bytes")
