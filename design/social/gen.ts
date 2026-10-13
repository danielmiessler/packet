// Writes the static gate row into card.html (bun design/social/gen.ts). The capture renders the DOM as authored,
// so the SVG is generated here, not by a script in the page.
import { readFileSync, writeFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
const page = fileURLToPath(new URL("./card.html", import.meta.url));
const f = (n: number) => +n.toFixed(1);
// Layer, value, at fault. Values are the app's readings at the moment the README screenshot shows.
const layers: [string, string, boolean][] = [
  ["Link", "Ethernet", false], ["Router", "0.7 ms", false], ["Internet", "8.2 ms", false], ["DNS", "225 ms", true], ["World", "8 regions", false],
];
const xs = [118, 376, 634, 892, 1150], w = 92, h = 136, drop = w * Math.tan(Math.PI / 6), t = 18;
const mid = t + (drop + h) / 2;
const gate = (x: number, stroke: string, dx: number, dy: number, z = 1) => {
  const W = w * z, H = h * z, D = W * Math.tan(Math.PI / 6);
  const l = x - W / 2 + dx, y = mid - (D + H) / 2 + dy;
  return `<path d="M${f(l)},${f(y)} L${f(l + W)},${f(y + D)} L${f(l + W)},${f(y + D + H)} L${f(l)},${f(y + H)} Z" stroke="${stroke}"/>`;
};
const cx = xs[3] + 6, cy = mid + 6, s = 54, k = Math.cos(Math.PI / 6) * s;
const poly = (pts: number[][], fill: string) => `<polygon points="${pts.map((p) => p.map(f).join(",")).join(" ")}" fill="${fill}"/>`;
// Each gate: a contact shadow, its dark offset frame, the route passing through it, then its lit front frame.
// Healthy gates recede a little so the faulty layer carries scale and value as well as hue; every layer is measured, so the route is solid.
const svg = [
  ...xs.map((x, i) => `<ellipse cx="${x + 16}" cy="${f(mid + (layers[i][2] ? 1.2 : 1) * (drop + h) / 2 + 12)}" rx="${layers[i][2] ? 78 : 64}" ry="9" fill="#01040f" opacity=".55"/>`),
  `<g fill="none" stroke-linejoin="round" stroke-linecap="round" stroke-width="13">`,
  ...layers.map(([, , bad], i) => `<g opacity="${bad ? 1 : 0.8}">${gate(xs[i], bad ? "#7a5a1f" : "#2a4fb5", 11, 7, bad ? 1.2 : 1)}</g>`),
  `</g>`,
  `<line x1="60" y1="${f(mid)}" x2="1220" y2="${f(mid)}" stroke="#7aa2f7" stroke-opacity=".55" stroke-width="5" stroke-linecap="round"/>`,
  `<g fill="none" stroke-linejoin="round" stroke-linecap="round" stroke-width="13">`,
  ...layers.map(([, , bad], i) => `<g opacity="${bad ? 1 : 0.8}">${gate(xs[i], bad ? "#e0af68" : "#7aa2f7", 0, 0, bad ? 1.2 : 1)}</g>`),
  `</g>`,
  poly([[cx, cy - s], [cx + k, cy - s / 2], [cx, cy], [cx - k, cy - s / 2]], "url(#top)"),
  poly([[cx - k, cy - s / 2], [cx, cy], [cx, cy + s], [cx - k, cy + s / 2]], "url(#left)"),
  poly([[cx, cy], [cx + k, cy - s / 2], [cx + k, cy + s / 2], [cx, cy + s]], "url(#right)"),
].join("\n    ");
const labels = layers.map(([name, value, bad], i) =>
  `<div class="layer${bad ? " fault" : ""}" style="left:${xs[i]}px"><div class="name"><i></i>${name}</div><div class="value">${value}</div></div>`).join("\n    ");
let html = readFileSync(page, "utf8");
html = html.replace(/<!-- gates:start -->[\s\S]*<!-- gates:end -->/, `<!-- gates:start -->\n    ${svg}\n    <!-- gates:end -->`);
html = html.replace(/<!-- labels:start -->[\s\S]*<!-- labels:end -->/, `<!-- labels:start -->\n    ${labels}\n    <!-- labels:end -->`);
writeFileSync(page, html);
console.log("card.html gates and labels written");
