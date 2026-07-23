//! Self-contained interactive HTML export.
//!
//! `exporters/html.py` renders with the vis-network library loaded from a CDN.
//! That is not self-contained, so we inline a small canvas force-directed
//! renderer instead (no external script/style src, no `http(s)://` resources).
//! The graph is embedded as JSON in the page; a compact Fruchterman-Reingold
//! simulation runs in-browser with drag, hover-to-inspect, and community colors.

use crate::{attr_str, color_for, community_of, degrees, node_id, xml_escape, Graph};
use serde_json::{json, Value};

/// Escape a JSON payload so it can sit safely inside a `<script>` block.
fn js_safe(v: &Value) -> String {
    v.to_string().replace("</", "<\\/")
}

pub fn to_html(g: &Graph) -> String {
    let deg = degrees(g);
    let max_deg = deg.values().copied().max().unwrap_or(1).max(1) as f64;

    let nodes: Vec<Value> = g
        .nodes
        .iter()
        .map(|n| {
            let id = node_id(n);
            let cid = community_of(n);
            let d = *deg.get(id).unwrap_or(&1);
            let size = 10.0 + 30.0 * (d as f64 / max_deg);
            json!({
                "id": id,
                "label": attr_str(n, "label").unwrap_or(id),
                "color": color_for(cid),
                "size": (size * 10.0).round() / 10.0,
                "community": cid,
                "community_name": n.get("community_name").and_then(Value::as_str)
                    .map(str::to_string).unwrap_or_else(|| format!("Community {cid}")),
                "source_file": attr_str(n, "source_file").unwrap_or(""),
                "file_type": attr_str(n, "file_type").unwrap_or(""),
                "degree": d,
            })
        })
        .collect();

    let edges: Vec<Value> = g
        .links
        .iter()
        .map(|e| {
            let conf = attr_str(e, "confidence").unwrap_or("EXTRACTED");
            json!({
                "from": attr_str(e, "source").unwrap_or(""),
                "to": attr_str(e, "target").unwrap_or(""),
                "relation": attr_str(e, "relation").unwrap_or(""),
                "confidence": conf,
                "dashes": conf != "EXTRACTED",
            })
        })
        .collect();

    let nodes_json = js_safe(&Value::Array(nodes));
    let edges_json = js_safe(&Value::Array(edges));
    let stats = format!("{} nodes &middot; {} edges", g.nodes.len(), g.links.len());
    let title = xml_escape("atlas graph");

    format!(
        r#"<!DOCTYPE html>
<html lang="en">
<head>
<meta charset="UTF-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>{title}</title>
<style>
* {{ box-sizing: border-box; margin: 0; padding: 0; }}
body {{ background:#0f0f1a; color:#e0e0e0; font-family:-apple-system,BlinkMacSystemFont,"Segoe UI",sans-serif; display:flex; height:100vh; overflow:hidden; }}
#graph {{ flex:1; position:relative; }}
canvas {{ display:block; }}
#sidebar {{ width:280px; background:#1a1a2e; border-left:1px solid #2a2a4e; display:flex; flex-direction:column; }}
#info {{ padding:14px; border-bottom:1px solid #2a2a4e; font-size:13px; line-height:1.6; }}
#info h3 {{ font-size:13px; color:#aaa; margin-bottom:8px; text-transform:uppercase; letter-spacing:.05em; }}
#info .empty {{ color:#555; font-style:italic; }}
#stats {{ margin-top:auto; padding:10px 14px; border-top:1px solid #2a2a4e; font-size:11px; color:#555; }}
</style>
</head>
<body>
<div id="graph"><canvas id="c"></canvas></div>
<div id="sidebar">
  <div id="info"><h3>Node Info</h3><div id="info-content"><span class="empty">Click a node to inspect it</span></div></div>
  <div id="stats">{stats}</div>
</div>
<script>
const RAW_NODES = {nodes_json};
const RAW_EDGES = {edges_json};
function esc(s) {{ return String(s).replace(/&/g,'&amp;').replace(/</g,'&lt;').replace(/>/g,'&gt;').replace(/"/g,'&quot;').replace(/'/g,'&#39;'); }}

const canvas = document.getElementById('c');
const ctx = canvas.getContext('2d');
const idx = new Map(RAW_NODES.map((n,i)=>[n.id,i]));
const N = RAW_NODES.length;
const edges = RAW_EDGES.map(e=>({{a:idx.get(e.from), b:idx.get(e.to), dashes:e.dashes}})).filter(e=>e.a!=null&&e.b!=null);

// state
let W=0,H=0, view={{x:0,y:0,z:1}};
const px=new Float64Array(N), py=new Float64Array(N), vx=new Float64Array(N), vy=new Float64Array(N);
for (let i=0;i<N;i++) {{ const a=2*Math.PI*i/N, r=300; px[i]=Math.cos(a)*r+(i%7); py[i]=Math.sin(a)*r+(i%5); }}

function resize() {{
  const box=document.getElementById('graph').getBoundingClientRect();
  W=box.width; H=box.height;
  const dpr=window.devicePixelRatio||1;
  canvas.width=W*dpr; canvas.height=H*dpr; canvas.style.width=W+'px'; canvas.style.height=H+'px';
  ctx.setTransform(dpr,0,0,dpr,0,0);
}}
window.addEventListener('resize', ()=>{{resize();draw();}});
resize();

// Fruchterman-Reingold, cooling then freeze
const k = Math.sqrt((1600*1120)/Math.max(1,N));
let temp=80, ticks=0, MAXT=250;
function step() {{
  const dx=new Float64Array(N), dy=new Float64Array(N);
  for (let i=0;i<N;i++) for (let j=i+1;j<N;j++) {{
    let ex=px[i]-px[j], ey=py[i]-py[j]; let d=Math.hypot(ex,ey)||0.01;
    let f=k*k/d; let ux=ex/d, uy=ey/d;
    dx[i]+=ux*f; dy[i]+=uy*f; dx[j]-=ux*f; dy[j]-=uy*f;
  }}
  for (const e of edges) {{
    let ex=px[e.a]-px[e.b], ey=py[e.a]-py[e.b]; let d=Math.hypot(ex,ey)||0.01;
    let f=d*d/k; let ux=ex/d, uy=ey/d;
    dx[e.a]-=ux*f; dy[e.a]-=uy*f; dx[e.b]+=ux*f; dy[e.b]+=uy*f;
  }}
  for (let i=0;i<N;i++) {{ if(i===dragging) continue; let d=Math.hypot(dx[i],dy[i])||0.01; px[i]+=dx[i]/d*Math.min(d,temp); py[i]+=dy[i]/d*Math.min(d,temp); }}
  temp=Math.max(2, temp*0.97);
}}

function toScreen(x,y) {{ return [ (x-view.x)*view.z + W/2, (y-view.y)*view.z + H/2 ]; }}
function fit() {{
  let mnx=1e9,mny=1e9,mxx=-1e9,mxy=-1e9;
  for(let i=0;i<N;i++){{mnx=Math.min(mnx,px[i]);mny=Math.min(mny,py[i]);mxx=Math.max(mxx,px[i]);mxy=Math.max(mxy,py[i]);}}
  view.x=(mnx+mxx)/2; view.y=(mny+mxy)/2;
  view.z=Math.min(W/((mxx-mnx)+80), H/((mxy-mny)+80), 2);
}}

function draw() {{
  ctx.clearRect(0,0,W,H);
  ctx.lineWidth=0.8;
  for (const e of edges) {{
    const [x0,y0]=toScreen(px[e.a],py[e.a]); const [x1,y1]=toScreen(px[e.b],py[e.b]);
    ctx.strokeStyle= e.dashes ? 'rgba(170,170,170,0.3)' : 'rgba(170,170,170,0.6)';
    ctx.setLineDash(e.dashes?[4,4]:[]);
    ctx.beginPath(); ctx.moveTo(x0,y0); ctx.lineTo(x1,y1); ctx.stroke();
  }}
  ctx.setLineDash([]);
  for (let i=0;i<N;i++) {{
    const n=RAW_NODES[i]; const [x,y]=toScreen(px[i],py[i]); const r=Math.max(3,n.size*view.z*0.4);
    ctx.beginPath(); ctx.arc(x,y,r,0,2*Math.PI); ctx.fillStyle=n.color; ctx.globalAlpha=0.9; ctx.fill(); ctx.globalAlpha=1;
    if (n.degree >= max_deg*0.15) {{ ctx.fillStyle='#fff'; ctx.font='10px sans-serif'; ctx.textAlign='center'; ctx.fillText(n.label, x, y-r-2); }}
  }}
}}
const max_deg = Math.max(1, ...RAW_NODES.map(n=>n.degree));

function loop() {{
  if (ticks<MAXT) {{ step(); ticks++; if(ticks%5===0||ticks===MAXT) fit(); draw(); requestAnimationFrame(loop); }}
  else draw();
}}
loop();

// interaction: hover/click to inspect, drag a node, wheel to zoom
let dragging=null;
function pick(mx,my) {{
  for (let i=N-1;i>=0;i--) {{ const [x,y]=toScreen(px[i],py[i]); const r=Math.max(3,RAW_NODES[i].size*view.z*0.4); if(Math.hypot(mx-x,my-y)<=r+2) return i; }}
  return null;
}}
function showInfo(i) {{
  const n=RAW_NODES[i];
  const nb=edges.filter(e=>e.a===i||e.b===i).length;
  document.getElementById('info-content').innerHTML =
    `<div><b>${{esc(n.label)}}</b></div><div>Type: ${{esc(n.file_type||'unknown')}}</div>`+
    `<div>Community: ${{esc(n.community_name)}}</div><div>Source: ${{esc(n.source_file||'-')}}</div>`+
    `<div>Degree: ${{n.degree}}</div><div>Neighbors: ${{nb}}</div>`;
}}
canvas.addEventListener('mousedown', e=>{{ const i=pick(e.offsetX,e.offsetY); if(i!=null){{dragging=i; showInfo(i);}} }});
window.addEventListener('mousemove', e=>{{ if(dragging==null) return; const r=canvas.getBoundingClientRect(); px[dragging]=(e.clientX-r.left-W/2)/view.z+view.x; py[dragging]=(e.clientY-r.top-H/2)/view.z+view.y; draw(); }});
window.addEventListener('mouseup', ()=>{{dragging=null;}});
canvas.addEventListener('click', e=>{{ const i=pick(e.offsetX,e.offsetY); if(i==null) document.getElementById('info-content').innerHTML='<span class="empty">Click a node to inspect it</span>'; }});
canvas.addEventListener('wheel', e=>{{ e.preventDefault(); view.z*=e.deltaY<0?1.1:0.9; draw(); }}, {{passive:false}});
</script>
</body>
</html>
"#
    )
}

#[cfg(test)]
mod tests {
    use crate::httpx;

    #[test]
    fn self_contained_no_external_resources() {
        let g = httpx();
        let html = super::to_html(&g);
        assert!(html.starts_with("<!DOCTYPE html>"));
        // No external script/style/link src or href pointing at a URL.
        for pat in [
            "src=\"http",
            "src='http",
            "href=\"http",
            "href='http",
            "@import",
            "//unpkg",
            "//cdn",
        ] {
            assert!(
                !html.contains(pat),
                "external resource reference found: {pat}"
            );
        }
        // No stray http(s):// URL anywhere in the document.
        assert!(
            !html.contains("http://") && !html.contains("https://"),
            "raw URL present"
        );
    }

    #[test]
    fn embeds_graph_json() {
        let g = httpx();
        let html = super::to_html(&g);
        assert!(html.contains("const RAW_NODES = ["));
        assert!(html.contains("const RAW_EDGES = ["));
        // a known node id from the httpx golden is present in the embedded data
        assert!(html.contains("\"client\""));
        assert!(html.contains("144 nodes"));
    }
}
