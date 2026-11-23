/** Tidy up: lay nodes out in neat blocks. Pure geometry; nothing is sent anywhere. */
export type Box = { id: string; type: string; x: number; y: number; w: number; h: number };
type Link = { source: string; target: string };
type Placed = { w: number; h: number; at: Record<string, { x: number; y: number }>; x: number; y: number };

const GAP_X = 70, GAP_Y = 30, BLOCK_GAP = 70, PAD = 30, HEAD = 46;

export const inside = (n: Box, g: Box) => {
  const cx = n.x + n.w / 2, cy = n.y + n.h / 2;
  return cx >= g.x && cx <= g.x + g.w && cy >= g.y && cy <= g.y + g.h;
};

/** One connected set of nodes: columns follow the connections, left to right. */
function layoutComponent(ids: string[], boxes: Map<string, Box>, links: Link[]): Placed {
  const set = new Set(ids);
  const inner = links.filter((l) => set.has(l.source) && set.has(l.target));
  const layer = new Map<string, number>(ids.map((id) => [id, 0]));
  for (let pass = 0; pass < ids.length; pass++) {
    let moved = false;
    for (const l of inner) {
      const want = layer.get(l.source)! + 1;
      if (want > layer.get(l.target)! && want < ids.length + 1) { layer.set(l.target, want); moved = true; }
    }
    if (!moved) break;
  }
  const cols: string[][] = [];
  for (const id of ids) (cols[layer.get(id)!] ??= []).push(id);
  const at: Placed["at"] = {};
  let x = 0, height = 0;
  for (const col of cols) {
    if (!col) continue;
    col.sort((a, b) => boxes.get(a)!.y - boxes.get(b)!.y || boxes.get(a)!.x - boxes.get(b)!.x);
    let y = 0, width = 0;
    for (const id of col) {
      const b = boxes.get(id)!;
      at[id] = { x, y };
      y += b.h + GAP_Y;
      width = Math.max(width, b.w);
    }
    height = Math.max(height, y - GAP_Y);
    x += width + GAP_X;
  }
  const xs = ids.map((id) => boxes.get(id)!.x), ys = ids.map((id) => boxes.get(id)!.y);
  return { w: x - GAP_X, h: height, at, x: Math.min(...xs), y: Math.min(...ys) };
}

/** Blocks in rows, in the reading order they already have; a row is filled until it is about as wide as a screen-shaped page. */
function pack(blocks: Placed[]): Placed {
  const rowOf = (b: Placed) => Math.round(b.y / 260);
  const order = [...blocks].sort((a, b) => rowOf(a) - rowOf(b) || a.x - b.x || a.y - b.y);
  const area = order.reduce((sum, b) => sum + (b.w + BLOCK_GAP) * (b.h + BLOCK_GAP), 0);
  const limit = Math.max(Math.sqrt(area * 2.4), ...order.map((b) => b.w));
  const at: Placed["at"] = {};
  let x = 0, y = 0, rowH = 0, w = 0;
  for (const b of order) {
    if (x > 0 && x + b.w > limit) { x = 0; y += rowH + BLOCK_GAP; rowH = 0; }
    for (const [id, p] of Object.entries(b.at)) at[id] = { x: p.x + x, y: p.y + y };
    x += b.w + BLOCK_GAP;
    rowH = Math.max(rowH, b.h);
    w = Math.max(w, x - BLOCK_GAP);
  }
  return { w, h: y + rowH, at, x: Math.min(...blocks.map((b) => b.x)), y: Math.min(...blocks.map((b) => b.y)) };
}

/**
 * New positions for `targets` (never group frames), and a new frame for each group
 * that holds some of them: its members stay together and the frame is fitted around them.
 */
export function tidy(all: Box[], links: Link[], targets: string[]) {
  const boxes = new Map(all.map((b) => [b.id, b]));
  const ids = targets.filter((id) => boxes.has(id) && boxes.get(id)!.type !== "group");
  if (ids.length < 2) return { moves: {} as Record<string, { x: number; y: number }>, frames: {} as Record<string, { x: number; y: number; width: number; height: number }> };
  const groups = all.filter((b) => b.type === "group");
  const groupOf = new Map<string, string>();
  for (const id of ids) { const g = groups.find((g) => inside(boxes.get(id)!, g)); if (g) groupOf.set(id, g.id); }

  // Connected components; a connection never pulls a node out of its group.
  const parent = new Map(ids.map((id) => [id, id]));
  const find = (a: string): string => (parent.get(a) === a ? a : (parent.set(a, find(parent.get(a)!)), parent.get(a)!));
  const idSet = new Set(ids);
  const usable = links.filter((l) => idSet.has(l.source) && idSet.has(l.target) && groupOf.get(l.source) === groupOf.get(l.target));
  for (const l of usable) parent.set(find(l.source), find(l.target));
  const comps = new Map<string, string[]>();
  for (const id of ids) { const r = find(id); comps.set(r, [...(comps.get(r) ?? []), id]); }

  const loose: Placed[] = [];
  const byGroup = new Map<string, Placed[]>();
  for (const members of comps.values()) {
    const placed = layoutComponent(members, boxes, usable);
    const g = groupOf.get(members[0]);
    if (g) byGroup.set(g, [...(byGroup.get(g) ?? []), placed]); else loose.push(placed);
  }
  const frameAt: Record<string, { x: number; y: number; w: number; h: number }> = {};
  for (const [g, blocks] of byGroup) {
    const inner = pack(blocks);
    const at: Placed["at"] = {};
    for (const [id, p] of Object.entries(inner.at)) at[id] = { x: p.x + PAD, y: p.y + HEAD };
    const gb = boxes.get(g)!;
    at[`frame:${g}`] = { x: 0, y: 0 };
    frameAt[g] = { x: 0, y: 0, w: inner.w + PAD * 2, h: inner.h + HEAD + PAD };
    loose.push({ w: frameAt[g].w, h: frameAt[g].h, at, x: gb.x, y: gb.y });
  }
  const whole = pack(loose);
  const ox = Math.min(...loose.map((b) => b.x)), oy = Math.min(...loose.map((b) => b.y));
  const moves: Record<string, { x: number; y: number }> = {};
  const frames: Record<string, { x: number; y: number; width: number; height: number }> = {};
  for (const [id, p] of Object.entries(whole.at)) {
    const pos = { x: Math.round(ox + p.x), y: Math.round(oy + p.y) };
    if (id.startsWith("frame:")) { const g = id.slice(6); frames[g] = { ...pos, width: Math.round(frameAt[g].w), height: Math.round(frameAt[g].h) }; }
    else moves[id] = pos;
  }
  return { moves, frames };
}
