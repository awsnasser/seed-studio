import { useEffect, useMemo, useState } from "react";
import { rpc } from "../api";
import { Icon, ProviderBadge, Thumb, useShell } from "../shared";
import { importImage } from "./Gallery";
import { ImageOptionsBar, ImgOpts, settingsFor } from "./ImageOptions";

type Mode = "auto" | "lines" | "paragraphs";
const MARKER = /^[\s*#>_-]*(?:(?:frame|shot|scene|panel|إطار|لقطة|مشهد)\s*)?#?\d{1,3}\s*[.):\-–—]+[\s*_]*/i;
const MAX_OPS = 200; // the validator's limit for one edit

/** Storyboard text → one prompt per frame. No model is involved. */
export function splitFrames(text: string, mode: Mode): string[] {
  const t = text.replace(/\r/g, "").trim();
  if (!t) return [];
  const lines = t.split("\n");
  const clean = (s: string) => s.replace(MARKER, "").replace(/\s+/g, " ").trim();
  if (mode === "auto" && lines.filter((l) => MARKER.test(l)).length >= 2) {
    const out: string[] = [];
    for (const l of lines) {
      if (MARKER.test(l) || !out.length) out.push(clean(l));
      else if (l.trim()) out[out.length - 1] += ` ${l.trim()}`;
    }
    return out.filter(Boolean);
  }
  const paragraphs = mode === "paragraphs" || (mode === "auto" && /\n\s*\n/.test(t));
  return (paragraphs ? t.split(/\n\s*\n/) : lines).map(clean).filter(Boolean);
}

const withStyle = (frame: string, style: string) => (style.trim() ? `${frame}${/[.!?؟。]$/.test(frame) ? "" : "."} ${style.trim()}` : frame);
const pad = (n: number) => String(n).padStart(2, "0");

export default function StoryboardDialog({ projectId, nodes, patch, onClose }: {
  projectId: string;
  nodes: { position: { x: number; y: number } }[];
  patch: (ops: any[], label: string) => Promise<any>;
  /** Ids of the new generation nodes, and whether to open the run plan for them. */
  onClose: (built?: { generators: string[]; run: boolean }) => void;
}) {
  const { app, toast } = useShell();
  const [text, setText] = useState("");
  const [mode, setMode] = useState<Mode>("auto");
  const [frames, setFrames] = useState<string[]>([]);
  const [style, setStyle] = useState("");
  const [refs, setRefs] = useState<{ id: string; title: string }[]>([]);
  const [modelKey, setModelKey] = useState(app.imageModel);
  const [opts, setOpts] = useState<ImgOpts>({});
  const [count, setCount] = useState("");
  const [busy, setBusy] = useState<string | null>(null);
  const [written, setWritten] = useState<{ provider: string; model: string; complete: boolean } | null>(null);

  const imageModels = app.models.filter((m) => m.kind === "image");
  const model = imageModels.find((m) => m.key === modelKey) ?? imageModels[0];
  const chatLabel = app.providerMode === "mock" ? "the simulation" : app.models.find((m) => m.key === app.chatModel)?.label ?? app.chatModel;
  const maxRefs: number = model?.options?.maxReferences ?? 10;
  const settings = useMemo(() => settingsFor(model, opts), [model, opts]);

  useEffect(() => setFrames(splitFrames(text, mode)), [text, mode]);

  const readFile = async (file?: File | null) => {
    if (!file) return;
    if (file.size > 1_000_000 || !(file.type.startsWith("text/") || /\.(txt|md|csv|tsv|json|fountain)$/i.test(file.name))) {
      return toast("Drop a text file (txt, md, csv). For a PDF or Word file, paste its text here.", "error");
    }
    setText(await file.text());
  };
  const addRef = async () => {
    try {
      const a = await importImage(projectId);
      if (a) setRefs((r) => [...r, { id: a.id, title: a.title }]);
    } catch (e) { toast((e as Error).message, "error"); }
  };
  const write = async () => {
    setBusy("write");
    try {
      const r = await rpc("storyboard_write", { projectId, story: text, frames: Number(count) || 0 });
      setWritten(r);
      setMode("auto");
      setText(r.text);
    } catch (e) { toast((e as Error).message, "error"); } finally { setBusy(null); }
  };

  const build = async (run: boolean) => {
    if (!frames.length || !model) return;
    setBusy("build");
    const tag = Math.random().toString(36).slice(2, 6);
    const baseX = nodes.length ? Math.min(...nodes.map((n) => n.position.x)) : 0;
    const baseY = nodes.length ? Math.max(...nodes.map((n) => n.position.y)) + 460 : 0;
    const refIds = refs.map((_, k) => `sb${tag}-ref${k + 1}`);
    let ops: any[] = refs.map((r, k) => ({ op: "addNode", id: refIds[k], type: "imageAsset", position: { x: baseX - 380, y: baseY + k * 280 }, data: { title: r.title || `Reference ${k + 1}`, assetId: r.id } }));
    const generators: string[] = [];
    let first = 1;
    try {
      for (let i = 0; i < frames.length; i++) {
        const n = i + 1, g = `sb${tag}-f${pad(n)}`;
        const x = baseX + (i % 5) * 300, y = baseY + Math.floor(i / 5) * 500;
        const frameOps: any[] = [
          { op: "addNode", id: g, type: "seedream", position: { x, y }, data: { title: `Frame ${pad(n)}`, modelKey: model.key, settings, prompt: withStyle(frames[i], style) } },
          ...refIds.map((r) => ({ op: "connect", source: r, target: g, targetHandle: "refs" })),
        ];
        if (ops.length + frameOps.length > MAX_OPS) {
          if (!(await patch(ops, `Storyboard frames ${first}–${i}`))) throw new Error("stopped");
          ops = [];
          first = n;
        }
        ops.push(...frameOps);
        generators.push(g);
      }
      if (ops.length && !(await patch(ops, first === 1 ? `Storyboard: ${frames.length} frames` : `Storyboard frames ${first}–${frames.length}`))) throw new Error("stopped");
      onClose({ generators, run });
    } catch {
      // The validator's message was already shown; frames added before it stay and can be undone.
      setBusy(null);
    }
  };

  return (
    <div className="overlay" role="dialog" aria-label="Storyboard">
      <div className="dialog storyboard">
        <div className="dialog-head">
          <h2>Storyboard</h2>
          <button className="round" onClick={() => onClose()} aria-label="Close"><Icon name="x" /></button>
        </div>
        <p className="muted small">Paste your frame prompts, one per line, per paragraph, or numbered. Each frame becomes an image node on the canvas with its prompt inside; nothing is sent until you approve the run plan.</p>

        <section>
          <div className="sb-head">
            <h3>1 · Frames</h3>
            <div className="segmented small" role="radiogroup" aria-label="How to split">
              {([["auto", "Auto"], ["lines", "One per line"], ["paragraphs", "Paragraphs"]] as const).map(([v, l]) => (
                <button key={v} role="radio" aria-checked={mode === v} className={mode === v ? "on" : ""} onClick={() => setMode(v)}>{l}</button>
              ))}
            </div>
          </div>
          <textarea className="sb-text" dir="auto" value={text} onChange={(e) => { setText(e.target.value); setWritten(null); }} rows={7}
            onDragOver={(e) => e.preventDefault()} onDrop={(e) => { e.preventDefault(); void readFile(e.dataTransfer.files?.[0]); }}
            placeholder={"1. Wide shot: a lighthouse on a cliff at dawn…\n2. Close-up: the keeper lights the lamp…\n\nPaste the prompts here, or drop a .txt / .md / .csv file."} aria-label="Storyboard text" />
          <div className="sb-write">
            <span className="muted small">Only have a story or script, not prompts?</span>
            <input className="sb-count" inputMode="numeric" value={count} onChange={(e) => setCount(e.target.value.replace(/\D/g, "").slice(0, 3))} placeholder="Frames (auto)" aria-label="Number of frames" />
            <button className="btn small" disabled={!text.trim() || busy !== null} onClick={write} title="Sends the text above as one chat request and replaces it with numbered prompts. No image is generated.">
              {busy === "write" ? <><span className="spinner" /> Writing…</> : <>Write prompts with {chatLabel} · 1 chat request</>}
            </button>
          </div>
          {written && (
            <div className={`result ${written.complete ? "ok" : "bad"}`}>
              {written.provider === "mock" ? <><ProviderBadge provider="mock" /> One frame per sentence — the simulation does not write prompts.</> : <>Written by <span className="mono">{written.model}</span>. Read them before generating.</>}
              {!written.complete && " The reply was cut off, so the last frames may be missing — ask for fewer frames or split the story."}
            </div>
          )}
          {frames.length > 0 && (
            <ol className="sb-frames">
              {frames.map((f, i) => (
                <li key={i}>
                  <span className="sb-n">{pad(i + 1)}</span>
                  <textarea dir="auto" rows={2} value={f} onChange={(e) => setFrames(frames.map((x, k) => (k === i ? e.target.value : x)))} aria-label={`Frame ${i + 1} prompt`} />
                  <button className="round small" onClick={() => setFrames(frames.filter((_, k) => k !== i))} aria-label={`Remove frame ${i + 1}`}><Icon name="x" size={12} /></button>
                </li>
              ))}
            </ol>
          )}
        </section>

        <section>
          <h3>2 · Shared by every frame</h3>
          <div className="field-row">
            <label htmlFor="sbstyle">Style</label>
            <input id="sbstyle" className="grow" dir="auto" value={style} onChange={(e) => setStyle(e.target.value)} placeholder="e.g. cinematic 35mm film still, soft natural light, muted teal and amber palette" />
          </div>
          <p className="muted small">Added to the end of each prompt, so all frames share one look.</p>
          <div className="field-row">
            <label>References</label>
            {refs.map((r, k) => (
              <span key={r.id} className="role-chip">
                <Thumb id={r.id} className="thumb tiny" />{r.title}
                <button onClick={() => setRefs(refs.filter((_, x) => x !== k))} aria-label="Remove reference"><Icon name="x" size={11} /></button>
              </span>
            ))}
            <button className="btn small" disabled={refs.length >= maxRefs} onClick={addRef}>+ Add image</button>
          </div>
          <p className="muted small">A character, product or location image sent with every frame keeps it consistent across the board (up to {maxRefs} for {model?.label}). Refer to it in the prompts, e.g. “the woman from the reference image”.</p>
          <div className="field-row">
            <label htmlFor="sbmodel">Model</label>
            <select id="sbmodel" value={model?.key} onChange={(e) => setModelKey(e.target.value)}>
              {imageModels.map((m) => <option key={m.key} value={m.key}>{m.label}</option>)}
            </select>
          </div>
          <ImageOptionsBar model={model} opts={opts} onChange={(p) => setOpts((o) => ({ ...o, ...p }))} hasReference={refs.length > 0} />
        </section>

        <div className="row end sb-foot">
          <span className="muted small grow">
            {frames.length ? <><b>{frames.length}</b> frames → {frames.length} image requests × {model?.label}, shown in a run plan first. They run one after another; a failed frame does not stop the rest.</> : "No frames yet."}
          </span>
          {busy === "build" ? <><span className="spinner" /> <span>Adding frames…</span></> : <>
            <button className="btn" disabled={!frames.length || busy !== null} onClick={() => build(false)}>Add to canvas only</button>
            <button className="btn accent" disabled={!frames.length || busy !== null} onClick={() => build(true)}><Icon name="play" size={13} />Add & review run plan</button>
          </>}
        </div>
      </div>
    </div>
  );
}
