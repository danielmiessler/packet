import { fileURLToPath } from "node:url";
// Generates bundle/AppIcon.svg and bundle/MenuGlyph.svg. Run: bun design/icon/gen.ts
// One gold isometric cube, the packet, flying outward through the gates of the layers it checks.

const f = (n: number) => n.toFixed(1);
type P = [number, number];
const pt = (p: P) => `${f(p[0])},${f(p[1])}`;

// Superellipse body with macOS-style continuous corners (n = 5, 824 px, 100 px margin).
const C = 512, R = 412, N = 5;
const bodyPts: string[] = [];
for (let i = 0; i < 360; i++) {
  const t = (i / 360) * 2 * Math.PI, c = Math.cos(t), s = Math.sin(t);
  bodyPts.push(pt([C + R * Math.sign(c) * Math.abs(c) ** (2 / N), C + R * Math.sign(s) * Math.abs(s) ** (2 / N)]));
}
const body = `M${bodyPts.join("L")}Z`;

// A polygon with each corner rounded by a quadratic curve that starts r before the vertex.
const rounded = (p: P[], r: number): string => {
  const n = p.length, out: string[] = [];
  for (let i = 0; i < n; i++) {
    const a = p[(i + n - 1) % n], b = p[i], c = p[(i + 1) % n];
    const toward = (from: P, to: P): P => { const dx = to[0] - from[0], dy = to[1] - from[1], l = Math.hypot(dx, dy); return [from[0] + (dx / l) * r, from[1] + (dy / l) * r]; };
    out.push(`${i ? "L" : "M"}${pt(toward(b, a))} Q${pt(b)} ${pt(toward(b, c))}`);
  }
  return out.join(" ") + " Z";
};
const poly = (p: P[]) => `M${p.map(pt).join("L")}Z`;

// Isometric cube with centre (cx, cy) and edge s; returns its seven visible vertices.
const cube = (cx: number, cy: number, s: number) => {
  const a = s * Math.cos(Math.PI / 6);
  return { T: [cx, cy - s] as P, R: [cx + a, cy - s / 2] as P, C: [cx, cy] as P, L: [cx - a, cy - s / 2] as P, BR: [cx + a, cy + s / 2] as P, B: [cx, cy + s] as P, BL: [cx - a, cy + s / 2] as P };
};

// ---------- App icon ----------
// Isometric axes: U runs up-right (outward, toward the world), V runs up-left (depth), H is height. Smaller u is nearer.
// The packet, a gold cube, flies along U through three closed gates that grow outward (link, edge, world).
// It has passed the first, is inside the second, and the third waits ahead, unlit.
const S = 172;                        // cube edge
const LIFT = 0.16 * S;                // the packet is in flight, clear of the gate sills
const gates = [
  { u: -1.02 * S, w: 0.68 * S, h: 1.36 * S, lit: true },
  { u: 0.3 * S, w: 0.82 * S, h: 1.64 * S, lit: true },
  { u: 1.86 * S, w: 0.94 * S, h: 1.86 * S, lit: false },
];
const BAR = 0.2 * S, DEPTH = 0.13 * S;
const raw = (u: number, v: number, h = 0): P => [Math.cos(Math.PI / 6) * (u - v), -0.5 * (u + v) - h];
type Gate = (typeof gates)[number];
const v0 = (g: Gate) => S / 2 - g.w, v1 = (g: Gate) => S / 2 + g.w;
// Centre the whole mark: the packet and all three gates.
const extent: P[] = gates.flatMap((g) => [raw(g.u, v0(g)), raw(g.u, v1(g)), raw(g.u, v1(g), g.h), raw(g.u, v0(g), g.h)]).concat([raw(S, S, S + LIFT), raw(S, 0, LIFT)]);
const xs = extent.map((p) => p[0]), ys = extent.map((p) => p[1]);
const ox = 512 - (Math.min(...xs) + Math.max(...xs)) / 2, oy = 516 - (Math.min(...ys) + Math.max(...ys)) / 2;
const G = (u: number, v: number, h = 0): P => { const r = raw(u, v, h); return [r[0] + ox, r[1] + oy]; };
const K = { B: G(0, 0, LIFT), BR: G(S, 0, LIFT), BL: G(0, S, LIFT), C: G(0, 0, S + LIFT), R: G(S, 0, S + LIFT), L: G(0, S, S + LIFT), T: G(S, S, S + LIFT) };
const hex = [K.T, K.R, K.BR, K.B, K.BL, K.L];
const quad = (u: number, va: number, vb: number, ha: number, hb: number): P[] => [G(u, va, ha), G(u, vb, ha), G(u, vb, hb), G(u, va, hb)];
const frame = (g: Gate, du: number) => rounded(quad(g.u + du, v0(g), v1(g), 0, g.h), 0.2 * S) + " " + rounded(quad(g.u + du, v0(g) + BAR, v1(g) - BAR, BAR, g.h - BAR), 0.08 * S);
const fills = (g: Gate) => g.lit ? { face: "url(#gateLit)", side: "#2A4FB5", o: 1 } : { face: "#5A7FD6", side: "#081A44", o: 0.4 };
const gateSvg = (g: Gate) => g.lit
  ? `<path fill-rule="evenodd" d="${frame(g, 0)}" fill="#6E9BFF" opacity="0.55" filter="url(#halo)"/><path fill-rule="evenodd" d="${frame(g, DEPTH)}" fill="#2A4FB5"/><path fill-rule="evenodd" d="${frame(g, 0)}" fill="url(#gateLit)"/>`
  : `<path fill-rule="evenodd" d="${frame(g, 0)}" fill="#3D63C9" opacity="0.16"/><path d="${frame(g, 0)}" fill="none" stroke="#9DB8FF" stroke-opacity="0.5" stroke-width="5" stroke-dasharray="2 16" stroke-linecap="round"/>`;
const routeDone = `M${pt(G(gates[0].u - 0.5 * S, S / 2))}L${pt(G(0, S / 2))}`;
const routeAhead = `M${pt(G(S + 0.32 * S, S / 2))}L${pt(G(gates[2].u + 0.35 * S, S / 2))}`;

const icon = `<svg xmlns="http://www.w3.org/2000/svg" width="1024" height="1024" viewBox="0 0 1024 1024">
  <defs>
    <linearGradient id="ground" x1="0" y1="0" x2="0" y2="1"><stop offset="0" stop-color="#163A96"/><stop offset="0.55" stop-color="#0B1C4E"/><stop offset="1" stop-color="#050B20"/></linearGradient>
    <radialGradient id="glow" cx="0.5" cy="0.08" r="0.75"><stop offset="0" stop-color="#4C7DFF" stop-opacity="0.42"/><stop offset="1" stop-color="#4C7DFF" stop-opacity="0"/></radialGradient>
    <linearGradient id="top" x1="0" y1="0" x2="0" y2="1"><stop offset="0" stop-color="#FFE7A1"/><stop offset="1" stop-color="#FFC94D"/></linearGradient>
    <linearGradient id="left" x1="0" y1="0" x2="0" y2="1"><stop offset="0" stop-color="#F7B03A"/><stop offset="1" stop-color="#E08A1E"/></linearGradient>
    <linearGradient id="right" x1="0" y1="0" x2="0" y2="1"><stop offset="0" stop-color="#C96A14"/><stop offset="1" stop-color="#94460A"/></linearGradient>
    <linearGradient id="gateLit" x1="0" y1="0" x2="0" y2="1"><stop offset="0" stop-color="#DCE6FF"/><stop offset="1" stop-color="#7AA2F7"/></linearGradient>
    <filter id="drop" x="-10%" y="-10%" width="120%" height="125%"><feGaussianBlur in="SourceAlpha" stdDeviation="12"/><feOffset dy="12"/><feComponentTransfer><feFuncA type="linear" slope="0.32"/></feComponentTransfer><feMerge><feMergeNode/><feMergeNode in="SourceGraphic"/></feMerge></filter>
    <filter id="halo" x="-30%" y="-30%" width="160%" height="160%"><feGaussianBlur stdDeviation="14"/></filter>
    <filter id="soft" x="-50%" y="-50%" width="200%" height="200%"><feGaussianBlur stdDeviation="16"/></filter>
    <clipPath id="bodyclip"><path d="${body}"/></clipPath>
    <clipPath id="cubeclip"><path d="${rounded(hex, 0.15 * S)}"/></clipPath>
  </defs>
  <g filter="url(#drop)">
    <path d="${body}" fill="url(#ground)"/>
    <path d="${body}" fill="url(#glow)"/>
  </g>
  <g clip-path="url(#bodyclip)">
    <path d="${body}" fill="none" stroke="#FFFFFF" stroke-opacity="0.14" stroke-width="4"/>
    <path d="${routeDone}" stroke="#8FB0FF" stroke-opacity="0.62" stroke-width="${0.11 * S}" stroke-linecap="round" fill="none"/>
    <path d="${routeAhead}" stroke="#8FB0FF" stroke-opacity="0.4" stroke-width="${0.11 * S}" stroke-linecap="round" stroke-dasharray="0 ${0.24 * S}" fill="none"/>
    ${gateSvg(gates[2])}
    ${gateSvg(gates[1])}
    <path d="${poly([G(0, 0), G(S, 0), G(S, S), G(0, S)])}" fill="#01040F" opacity="0.5" filter="url(#soft)"/>
    <g clip-path="url(#cubeclip)">
      <path d="${poly([K.L, K.C, K.B, K.BL])}" fill="url(#left)"/>
      <path d="${poly([K.C, K.R, K.BR, K.B])}" fill="url(#right)"/>
      <path d="${poly([K.T, K.R, K.C, K.L])}" fill="url(#top)"/>
      <path d="M${pt(K.L)}L${pt(K.C)}L${pt(K.R)}" fill="none" stroke="#FFF4CF" stroke-opacity="0.9" stroke-width="5" stroke-linejoin="round"/>
      <path d="M${pt(K.C)}L${pt(K.B)}" fill="none" stroke="#7A3606" stroke-opacity="0.35" stroke-width="4"/>
    </g>
    ${gateSvg(gates[0])}
  </g>
</svg>
`;

// ---------- Menu-bar glyph (viewBox 36, one fill #000000, recoloured by verdict) ----------
// The app mark reduced to two shapes: the packet (three faces split by knocked-out seams) and the gate it has passed.
const g = cube(23.4, 14.6, 9.6);
const seam = 1.15; // inset of each face, so seams read at 18 pt
const shrink = (p: P[], toward: P, by: number): P[] => p.map(([x, y]) => { const dx = toward[0] - x, dy = toward[1] - y, l = Math.hypot(dx, dy) || 1; return [x + (dx / l) * by, y + (dy / l) * by]; });
const centroid = (p: P[]): P => [p.reduce((q, r) => q + r[0], 0) / p.length, p.reduce((q, r) => q + r[1], 0) / p.length];
const face = (p: P[]) => rounded(shrink(p, centroid(p), seam), 1.1);
// The gate: an upright frame in the V-H plane, drawn with an even-odd cut-out. V runs up-left, H straight up.
const gv: P = [-Math.cos(Math.PI / 6), -0.5];
const gp = (base: P, v: number, h: number): P => [base[0] + gv[0] * v, base[1] + gv[1] * v - h];
const gb: P = [13.6, 31.6];          // the gate sill front corner
const gw = 11.6, gh = 18.2, gbar = 2.7;
const gateOuter = [gp(gb, 0, 0), gp(gb, gw, 0), gp(gb, gw, gh), gp(gb, 0, gh)];
const gateInner = [gp(gb, gbar, gbar), gp(gb, gw - gbar, gbar), gp(gb, gw - gbar, gh - gbar), gp(gb, gbar, gh - gbar)];
const glyph = `<svg xmlns="http://www.w3.org/2000/svg" width="36" height="36" viewBox="0 0 36 36">
  <path fill="#000000" d="${face([g.T, g.R, g.C, g.L])}"/>
  <path fill="#000000" d="${face([g.L, g.C, g.B, g.BL])}"/>
  <path fill="#000000" d="${face([g.C, g.R, g.BR, g.B])}"/>
  <path fill="#000000" fill-rule="evenodd" d="${rounded(gateOuter, 2.2)} ${rounded(gateInner, 0.9)}"/>
</svg>
`;

// fileURLToPath decodes the URL: .pathname kept %20 escapes and wrote to a literal "Mobile%20Documents" tree.
const out = process.argv[2] ?? fileURLToPath(new URL("../../bundle", import.meta.url));
await Bun.write(`${out}/AppIcon.svg`, icon);
await Bun.write(`${out}/MenuGlyph.svg`, glyph);
console.log(`${out}/AppIcon.svg\n${out}/MenuGlyph.svg`);
