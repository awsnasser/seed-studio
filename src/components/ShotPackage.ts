import { fileToBase64, rpc } from "../api";

type Shot = { title?: string; prompt: string; voice?: boolean; clipStart?: number; clipEnd?: number; duration?: number; sound?: boolean };

/** A prepared storyboard: frames, prompts and voice timings in one file. */
export type ShotPackage = {
  format: "seed-studio-shots"; name: string;
  /** Shape of the frames, e.g. "16:9": used for shots that send a voice clip (reference mode has no first frame to follow). */
  ratio?: string;
  voice?: { name: string; base64: string };
  shots: (Shot & { title: string; image: { name: string; base64: string } })[];
};
/** A prompts file: one entry per frame already on the canvas, matched by image name (or by order). */
export type PromptsFile = { format: "seed-studio-prompts"; ratio?: string; shots: (Shot & { image?: string })[] };

type Patch = (ops: any[], label: string) => Promise<any>;
type Placed = { position: { x: number; y: number } };
const MAX_OPS = 200; // the validator's limit for one edit
const pad = (n: number) => String(n).padStart(2, "0");
const stem = (name: string) => name.replace(/\.[^.]+$/, "").trim();
const byName = (a: File, b: File) => a.name.localeCompare(b.name, undefined, { numeric: true });
const origin = (nodes: Placed[]) => ({
  x: nodes.length ? Math.min(...nodes.map((n) => n.position.x)) : 0,
  y: nodes.length ? Math.max(...nodes.map((n) => n.position.y)) + 560 : 0,
});
const cell = (base: { x: number; y: number }, i: number) => ({ x: base.x + (i % 3) * 820, y: base.y + Math.floor(i / 3) * 580 });

/**
 * The nodes of one shot next to its frame: a video node (Draft) that holds the
 * prompt, and — when the shot is spoken and a voice track exists — an audio clip
 * node on the video's `audio` input.
 */
function shotNodes(o: { frameId: string; videoId: string; audioId: string; x: number; y: number; title: string; ratio?: string; voiceId?: string; shot: Shot }): any[] {
  const s = o.shot;
  const hasClip = !!s.clipEnd && s.clipEnd > (s.clipStart ?? 0);
  const wired = !!s.voice && hasClip && !!o.voiceId;
  const settings: Record<string, string> = { pass: "draft", sound: s.sound === false ? "off" : "on", voice: s.voice && !wired ? "on" : "off" };
  if (s.duration) settings.duration = String(s.duration);
  if (hasClip && !wired) { settings.voiceStart = String(s.clipStart ?? 0); settings.voiceEnd = String(s.clipEnd); } // unspoken shots: a guide for the length
  if (s.voice && o.ratio) settings.ratio = o.ratio;
  const ops: any[] = [
    { op: "addNode", id: o.videoId, type: "seedance", position: { x: o.x + 300, y: o.y }, data: { title: o.title.slice(0, 120), prompt: s.prompt, settings } },
    { op: "connect", source: o.frameId, target: o.videoId, targetHandle: "first" },
  ];
  if (wired) {
    ops.push({ op: "addNode", id: o.audioId, type: "audio", position: { x: o.x, y: o.y + 230 }, data: { title: `${o.title} · voice`.slice(0, 120), audioId: o.voiceId, start: String(s.clipStart ?? 0), end: String(s.clipEnd) } });
    ops.push({ op: "connect", source: o.audioId, target: o.videoId, targetHandle: "audio" });
  }
  return ops;
}

/** Apply ops in edits of at most MAX_OPS, keeping each shot's ops together. */
async function applyGroups(groups: any[][], patch: Patch, label: string) {
  let ops: any[] = [];
  for (const g of groups) {
    if (ops.length + g.length > MAX_OPS) {
      if (!(await patch(ops, label))) throw new Error("The canvas refused part of the import; what was added so far can be undone.");
      ops = [];
    }
    ops.push(...g);
  }
  if (ops.length && !(await patch(ops, label))) throw new Error("The canvas refused the import.");
}

/** Put a shot package on the canvas, shot by shot, wired and set to Draft. Nothing is generated. */
export async function importShotPackage(projectId: string, file: File, nodes: Placed[], patch: Patch, progress: (text: string) => void): Promise<string[]> {
  let pkg: ShotPackage;
  try { pkg = JSON.parse(await file.text()); } catch { throw new Error("This file is not a shot package (it is not valid JSON)."); }
  if (pkg?.format !== "seed-studio-shots" || !Array.isArray(pkg.shots) || !pkg.shots.length) throw new Error("This file is not a Seed Studio shot package.");
  let voiceId: string | undefined;
  if (pkg.voice) {
    progress("Importing the voice track…");
    voiceId = (await rpc("audio_import", { projectId, name: pkg.voice.name, base64: pkg.voice.base64 })).id;
  }
  const tag = Math.random().toString(36).slice(2, 6);
  const base = origin(nodes);
  const videos: string[] = [];
  const groups: any[][] = [];
  for (let i = 0; i < pkg.shots.length; i++) {
    const s = pkg.shots[i], n = i + 1;
    progress(`Importing shot ${n} of ${pkg.shots.length}…`);
    const asset = await rpc("asset_import", { projectId, name: `Shot ${pad(n)} · ${s.title}`, base64: s.image.base64 });
    const f = `sh${tag}-frame${pad(n)}`, v = `sh${tag}-video${pad(n)}`;
    const { x, y } = cell(base, i);
    const title = `${pad(n)} · ${s.title}`;
    groups.push([
      { op: "addNode", id: f, type: "imageAsset", position: { x, y }, data: { title, assetId: asset.id } },
      ...shotNodes({ frameId: f, videoId: v, audioId: `sh${tag}-audio${pad(n)}`, x, y, title, shot: s, ratio: pkg.ratio, voiceId }),
    ]);
    videos.push(v);
  }
  await applyGroups(groups, patch, `${pkg.name}: ${pkg.shots.length} shots`);
  return videos;
}

/** Put many images on the canvas at once, in file-name order, one frame node each. */
export async function importImages(projectId: string, files: File[], nodes: Placed[], patch: Patch, progress: (text: string) => void): Promise<number> {
  const images = files.filter((f) => /^image\/(png|jpeg|webp)$/.test(f.type)).sort(byName);
  if (!images.length) throw new Error("Choose PNG, JPEG or WebP images.");
  const tag = Math.random().toString(36).slice(2, 6);
  const base = origin(nodes);
  const groups: any[][] = [];
  for (let i = 0; i < images.length; i++) {
    progress(`Importing image ${i + 1} of ${images.length}…`);
    const asset = await rpc("asset_import", { projectId, name: stem(images[i].name), base64: await fileToBase64(images[i]) });
    groups.push([{ op: "addNode", id: `im${tag}-${pad(i + 1)}`, type: "imageAsset", position: cell(base, i), data: { title: stem(images[i].name).slice(0, 80), assetId: asset.id } }]);
  }
  await applyGroups(groups, patch, `Import ${images.length} images`);
  return images.length;
}

/**
 * Give frames that are already on the canvas a video node (Draft) holding the prompt.
 * A frame is found by its node title = the image file name; entries without `image`
 * take the frames in order. Frames that already feed a video node are left alone.
 */
export async function importPrompts(
  projectId: string, file: File,
  graph: { nodes: { id: string; type: string; position: { x: number; y: number }; data: any }[]; edges: { source: string; target: string }[] },
  patch: Patch,
): Promise<{ added: number; missing: string[] }> {
  let doc: PromptsFile;
  try { doc = JSON.parse(await file.text()); } catch { throw new Error("This file is not valid JSON."); }
  if (doc?.format !== "seed-studio-prompts" || !Array.isArray(doc.shots)) throw new Error("This is not a Seed Studio prompts file (format: \"seed-studio-prompts\").");
  const kind = (id: string) => graph.nodes.find((n) => n.id === id)?.type;
  const free = graph.nodes
    .filter((n) => n.type === "imageAsset" && !graph.edges.some((e) => e.source === n.id && kind(e.target) === "seedance"))
    .sort((a, b) => a.position.y - b.position.y || a.position.x - b.position.x);
  const norm = (t: string) => stem(String(t ?? "")).toLowerCase();
  const voiceId: string | undefined = (await rpc("audio_get", { projectId }))?.id;
  const tag = Math.random().toString(36).slice(2, 6);
  const used = new Set<string>();
  const missing: string[] = [];
  const groups: any[][] = [];
  for (let i = 0; i < doc.shots.length; i++) {
    const s = doc.shots[i];
    const frame = s.image ? free.find((n) => !used.has(n.id) && norm(n.data.title) === norm(s.image!)) : free.find((n) => !used.has(n.id));
    if (!frame) { missing.push(s.image ?? `entry ${i + 1}`); continue; }
    used.add(frame.id);
    groups.push(shotNodes({
      frameId: frame.id, videoId: `pr${tag}-video${pad(i + 1)}`, audioId: `pr${tag}-audio${pad(i + 1)}`,
      x: frame.position.x, y: frame.position.y, title: String(s.title ?? frame.data.title ?? `Shot ${pad(i + 1)}`), shot: s, ratio: doc.ratio, voiceId,
    }));
  }
  await applyGroups(groups, patch, `Import prompts for ${groups.length} frames`);
  return { added: groups.length, missing };
}

export type BundleImage = { name: string; title: string; assetId: string };
export type BundleAudio = { name: string; audioId: string; durationMs: number };
export type BundleShot = { image: string; title?: string; prompt: string; speaks?: boolean; duration?: number; voice?: boolean; clipStart?: number; clipEnd?: number };

/**
 * Lay out a bundle's files on the canvas: one frame per image with a generation node
 * holding its prompt. Audio whose file name matches an image is wired to that shot;
 * the rest become stand-alone audio nodes. Nothing is generated.
 */
export async function buildFromBundle(
  o: { images: BundleImage[]; audio: BundleAudio[]; shots: BundleShot[]; kind: "video" | "image"; ratio?: string; imageModel?: string },
  nodes: Placed[], patch: Patch,
): Promise<{ shots: number; withPrompt: number; ids: string[] }> {
  const tag = Math.random().toString(36).slice(2, 6);
  const base = origin(nodes);
  const norm = (t: string) => stem(String(t ?? "")).toLowerCase().replace(/\s+/g, " ");
  const byImage = new Map(o.shots.map((s) => [norm(s.image), s]));
  const audioFor = new Map(o.audio.map((a) => [norm(a.name), a]));
  const usedAudio = new Set<string>();
  const groups: any[][] = [];
  const ids: string[] = [];
  let withPrompt = 0;
  o.images.forEach((img, i) => {
    const n = i + 1;
    const s = byImage.get(norm(img.name)) ?? o.shots[i]; // by name, else by order
    const prompt = s?.prompt?.trim() ?? "";
    if (prompt) withPrompt++;
    const title = (s?.title?.trim() || img.title || `Shot ${pad(n)}`).slice(0, 110);
    const f = `bd${tag}-frame${pad(n)}`, g = `bd${tag}-gen${pad(n)}`, a = `bd${tag}-audio${pad(n)}`;
    const { x, y } = cell(base, i);
    const ops: any[] = [{ op: "addNode", id: f, type: "imageAsset", position: { x, y }, data: { title: img.title.slice(0, 110), assetId: img.assetId } }];
    if (o.kind === "image") {
      ops.push({ op: "addNode", id: g, type: "seedream", position: { x: x + 300, y }, data: { title, prompt, ...(o.imageModel ? { modelKey: o.imageModel } : {}) } });
      ops.push({ op: "connect", source: f, target: g, targetHandle: "refs" });
    } else {
      const clip = audioFor.get(norm(img.name));
      const settings: Record<string, string> = { pass: "draft" };
      if (s?.duration) settings.duration = String(Math.max(4, Math.min(30, Math.round(s.duration))));
      if (clip && o.ratio) settings.ratio = o.ratio;
      ops.push({ op: "addNode", id: g, type: "seedance", position: { x: x + 300, y }, data: { title, prompt, settings } });
      ops.push({ op: "connect", source: f, target: g, targetHandle: "first" });
      if (clip) {
        usedAudio.add(clip.audioId);
        const end = Math.min(30, Math.round(clip.durationMs / 100) / 10);
        ops.push({ op: "addNode", id: a, type: "audio", position: { x, y: y + 230 }, data: { title: `${title} · voice`.slice(0, 120), audioId: clip.audioId, start: "0", end: String(end) } });
        ops.push({ op: "connect", source: a, target: g, targetHandle: "audio" });
      }
    }
    groups.push(ops);
    ids.push(g);
  });
  o.audio.filter((a) => !usedAudio.has(a.audioId)).forEach((a, k) => {
    groups.push([{ op: "addNode", id: `bd${tag}-voice${pad(k + 1)}`, type: "audio", position: { x: base.x - 320, y: base.y + k * 150 }, data: { title: stem(a.name).slice(0, 110), audioId: a.audioId, start: "0", end: String(Math.min(30, Math.round(a.durationMs / 100) / 10)) } }]);
  });
  await applyGroups(groups, patch, `Import ${o.images.length} shots`);
  return { shots: o.images.length, withPrompt, ids };
}
