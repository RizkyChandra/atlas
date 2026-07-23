//! SVG export — a hand-rolled string, NO matplotlib.
//!
//! `export.py::to_svg` used matplotlib + `nx.spring_layout`. We reproduce the
//! *look* (dark canvas, community-colored nodes sized by degree, dashed edges
//! for non-EXTRACTED confidence) with a compact, deterministic
//! Fruchterman-Reingold layout so the output is reproducible without any Python
//! plotting stack.

use crate::{attr_str, color_for, community_of, degrees, node_id, xml_escape, Graph};
use std::collections::HashMap;
use std::f64::consts::PI;

const W: f64 = 1600.0;
const H: f64 = 1120.0;
const MARGIN: f64 = 60.0;

/// Deterministic FR layout. Returns node-index -> (x, y) in [0,W]x[0,H].
// ponytail: O(n^2) per iteration; fine for graphify's ~5k node viz ceiling,
// swap in a Barnes-Hut quadtree only if that ceiling ever rises.
fn layout(n: usize, edges: &[(usize, usize)]) -> Vec<(f64, f64)> {
    if n == 0 {
        return Vec::new();
    }
    let area = W * H;
    let k = (area / n as f64).sqrt();
    // Deterministic init on a circle (+ index jitter so no two points coincide).
    let mut pos: Vec<(f64, f64)> = (0..n)
        .map(|i| {
            let a = 2.0 * PI * i as f64 / n as f64;
            let r = W.min(H) * 0.35;
            (
                W / 2.0 + r * a.cos() + (i % 7) as f64 * 0.5,
                H / 2.0 + r * a.sin() + (i % 5) as f64 * 0.5,
            )
        })
        .collect();

    let iters = 200;
    let mut temp = W * 0.1;
    let cool = temp / (iters as f64 + 1.0);
    for _ in 0..iters {
        let mut disp = vec![(0.0f64, 0.0f64); n];
        // repulsion
        for i in 0..n {
            for j in (i + 1)..n {
                let dx = pos[i].0 - pos[j].0;
                let dy = pos[i].1 - pos[j].1;
                let d = (dx * dx + dy * dy).sqrt().max(0.01);
                let f = k * k / d;
                let (ux, uy) = (dx / d, dy / d);
                disp[i].0 += ux * f;
                disp[i].1 += uy * f;
                disp[j].0 -= ux * f;
                disp[j].1 -= uy * f;
            }
        }
        // attraction along edges
        for &(a, b) in edges {
            let dx = pos[a].0 - pos[b].0;
            let dy = pos[a].1 - pos[b].1;
            let d = (dx * dx + dy * dy).sqrt().max(0.01);
            let f = d * d / k;
            let (ux, uy) = (dx / d, dy / d);
            disp[a].0 -= ux * f;
            disp[a].1 -= uy * f;
            disp[b].0 += ux * f;
            disp[b].1 += uy * f;
        }
        // apply, capped by temperature
        for i in 0..n {
            let (dx, dy) = disp[i];
            let d = (dx * dx + dy * dy).sqrt().max(0.01);
            pos[i].0 += dx / d * d.min(temp);
            pos[i].1 += dy / d * d.min(temp);
        }
        temp -= cool;
    }

    // rescale to viewport with margins
    let (mut minx, mut miny, mut maxx, mut maxy) = (f64::MAX, f64::MAX, f64::MIN, f64::MIN);
    for &(x, y) in &pos {
        minx = minx.min(x);
        miny = miny.min(y);
        maxx = maxx.max(x);
        maxy = maxy.max(y);
    }
    let sx = (W - 2.0 * MARGIN) / (maxx - minx).max(1e-6);
    let sy = (H - 2.0 * MARGIN) / (maxy - miny).max(1e-6);
    pos.iter()
        .map(|&(x, y)| (MARGIN + (x - minx) * sx, MARGIN + (y - miny) * sy))
        .collect()
}

pub fn to_svg(g: &Graph) -> String {
    let idx: HashMap<&str, usize> = g
        .nodes
        .iter()
        .enumerate()
        .map(|(i, n)| (node_id(n), i))
        .collect();

    let edges: Vec<(usize, usize)> = g
        .links
        .iter()
        .filter_map(|e| {
            let s = idx.get(attr_str(e, "source")?)?;
            let t = idx.get(attr_str(e, "target")?)?;
            Some((*s, *t))
        })
        .collect();

    let pos = layout(g.nodes.len(), &edges);
    let deg = degrees(g);
    let max_deg = deg.values().copied().max().unwrap_or(1).max(1) as f64;

    let mut out = String::new();
    out.push_str(&format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
         <svg xmlns=\"http://www.w3.org/2000/svg\" width=\"{W}\" height=\"{H}\" \
         viewBox=\"0 0 {W} {H}\">\n"
    ));
    out.push_str(&format!(
        "  <rect width=\"{W}\" height=\"{H}\" fill=\"#1a1a2e\"/>\n"
    ));

    // edges first (under the nodes)
    out.push_str("  <g stroke=\"#aaaaaa\">\n");
    for e in &g.links {
        let (s, t) = match (
            attr_str(e, "source").and_then(|v| idx.get(v)),
            attr_str(e, "target").and_then(|v| idx.get(v)),
        ) {
            (Some(s), Some(t)) => (*s, *t),
            _ => continue,
        };
        let extracted = attr_str(e, "confidence").unwrap_or("EXTRACTED") == "EXTRACTED";
        let (dash, opacity) = if extracted {
            ("", 0.6)
        } else {
            (" stroke-dasharray=\"4,4\"", 0.3)
        };
        let (x0, y0) = pos[s];
        let (x1, y1) = pos[t];
        out.push_str(&format!(
            "    <line x1=\"{x0:.1}\" y1=\"{y0:.1}\" x2=\"{x1:.1}\" y2=\"{y1:.1}\" \
             stroke-width=\"0.8\" stroke-opacity=\"{opacity}\"{dash}/>\n"
        ));
    }
    out.push_str("  </g>\n");

    // nodes
    out.push_str("  <g>\n");
    for (i, n) in g.nodes.iter().enumerate() {
        let (x, y) = pos[i];
        let d = *deg.get(node_id(n)).unwrap_or(&1) as f64;
        let r = 4.0 + 10.0 * (d / max_deg);
        let color = color_for(community_of(n));
        out.push_str(&format!(
            "    <circle cx=\"{x:.1}\" cy=\"{y:.1}\" r=\"{r:.1}\" fill=\"{color}\" \
             fill-opacity=\"0.9\"/>\n"
        ));
    }
    out.push_str("  </g>\n");

    // labels on top
    out.push_str("  <g fill=\"#ffffff\" font-family=\"sans-serif\" font-size=\"7\" text-anchor=\"middle\">\n");
    for (i, n) in g.nodes.iter().enumerate() {
        let (x, y) = pos[i];
        let label = attr_str(n, "label").unwrap_or_else(|| node_id(n));
        out.push_str(&format!(
            "    <text x=\"{x:.1}\" y=\"{:.1}\">{}</text>\n",
            y - 8.0,
            xml_escape(label)
        ));
    }
    out.push_str("  </g>\n</svg>\n");
    out
}

#[cfg(test)]
mod tests {
    use crate::httpx;

    #[test]
    fn one_circle_per_node_wellformed() {
        let g = httpx();
        let svg = super::to_svg(&g);
        assert!(svg.contains("<svg"));
        assert!(svg.trim_end().ends_with("</svg>"));
        assert_eq!(svg.matches("<circle ").count(), g.nodes.len());
        // edges are lines, not shapes counted as nodes
        assert_eq!(svg.matches("<line ").count(), g.links.len());
        // no NaN leaked from the layout
        assert!(!svg.contains("NaN"));
    }
}
