import { useEffect, useRef, useState } from "react";
import { fileToBase64, rpc } from "../api";
import { Icon, ProviderBadge, Thumb, useShell } from "../shared";
import { BundleAudio, BundleImage, BundleShot, buildFromBundle } from "./ShotPackage";

type Doc = { name: string; text: string; truncated?: boolean };
type Manifest = { images: BundleImage[]; audio: BundleAudio[]; documents: Doc[]; promptsFile: any | null; skipped: string[] };
const BATCH = 8; // shots per chat request, so a reply fits the model's output limit

/**
 * A ZIP, a PDF or documents dropped on the canvas: shows what was found, then builds
 * every shot — frame, prompt, voice — ready to generate. Nothing is generated here.
 */
export default function BundleDialog({ projectId, files, nodes, patch, onClose }: {
  projectId: string; files: File[];
  nodes: { position: { x: number; y: number } }[];
  patch: (ops: any[], label: string) => Promise<any>;
  onClose: (ids?: string[]) => void;
}) {
  const { app, toast } = useShell();
  const [found, setFound] = useState<Manifest | null>(null);
  const [status, setStatus] = useState("Reading the files…");
  const [busy, setBusy] = useState(true);
  const [error, setError] = useState<string | null>(null);
  const [kind, setKind] = useState<"video" | "image">("video");
  const started = useRef(false);
  const mock = app.providerMode === "mock";
  const chatLabel = mock ? "the simulation" : app.models.find((m) => m.key === app.chatModel)?.label ?? app.chatModel;

  useEffect(() => {
    if (started.current) return;
    started.current = true;
    (async () => {
      const all: Manifest = { images: [], audio: [], documents: [], promptsFile: null, skipped: [] };
      try {
        for (const f of files) {
          setStatus(`Reading ${f.name}…`);
          if (f.size > 500_000_000) { all.skipped.push(`${f.name} (larger than 500 MB)`); continue; }
          const m: Manifest = await rpc("bundle_ingest", { projectId, name: f.name, base64: await fileToBase64(f) });
          all.images.push(...m.images); all.audio.push(...m.audio); all.documents.push(...m.documents); all.skipped.push(...m.skipped);
          all.promptsFile = all.promptsFile ?? m.promptsFile;
        }
        setFound(all);
      } catch (e) { setError((e as Error).message); } finally { setBusy(false); setStatus(""); }
    })();
  }, [files, projectId]);

  const finish = async (shots: BundleShot[], note?: string) => {
    if (!found) return;
    setStatus("Placing the shots on the canvas…");
    try {
      const r = await buildFromBundle({ images: found.images, audio: found.audio, shots, kind, ratio: "16:9" }, nodes, patch);
      toast(`${r.shots} shots are on the canvas${r.withPrompt ? `, ${r.withPrompt} with their prompt` : ""}, ready to generate. Nothing was generated.${note ? ` ${note}` : ""}`);
      onClose(r.ids);
    } catch (e) { setError((e as Error).message); setBusy(false); setStatus(""); }
  };
  const fromFile = () => {
    setBusy(true);
    const shots: BundleShot[] = (found?.promptsFile?.shots ?? []).map((s: any) => ({ ...s, image: String(s.image ?? "") }));
    void finish(shots);
  };
  const withAi = async () => {
    if (!found) return;
    setBusy(true);
    setError(null);
    const names = found.images.map((i) => i.name);
    const shots: BundleShot[] = [];
    try {
      for (let i = 0; i < names.length; i += BATCH) {
        setStatus(`Writing prompts ${i + 1}–${Math.min(names.length, i + BATCH)} of ${names.length} with ${chatLabel}…`);
        const r = await rpc("bundle_write", { projectId, kind, shots: names.slice(i, i + BATCH), allShots: names, documents: found.documents });
        shots.push(...r.shots);
      }
      await finish(shots, mock ? "The simulation does not read documents, so the prompts are empty." : undefined);
    } catch (e) {
      // Keep what was written so far: the rest can be filled by hand or by trying again.
      setError(`${(e as Error).message}${shots.length ? ` ${shots.length} prompts were written before this; you can still build with them.` : ""}`);
      partial.current = shots;
      setBusy(false);
      setStatus("");
    }
  };
  const partial = useRef<BundleShot[]>([]);
  const requests = found ? Math.ceil(found.images.length / BATCH) : 0;
  const nothing = found && !found.images.length && !found.audio.length && !found.documents.length;

  return (
    <div className="overlay" role="dialog" aria-label="Import files">
      <div className="dialog bundle">
        <div className="dialog-head">
          <h2>Import to canvas</h2>
          <button className="round" onClick={() => onClose()} aria-label="Close" disabled={busy && !!found}><Icon name="x" /></button>
        </div>
        {!found && !error && <div className="bundle-wait"><span className="spinner" /> {status}</div>}
        {error && <div className="result bad">{error}</div>}
        {found && (
          <>
            <div className="bundle-found">
              <div className="bundle-stat"><b>{found.images.length}</b><span>images</span></div>
              <div className="bundle-stat"><b>{found.audio.length}</b><span>audio files</span></div>
              <div className="bundle-stat"><b>{found.documents.length}</b><span>documents</span></div>
            </div>
            {found.images.length > 0 && (
              <div className="bundle-strip">{found.images.slice(0, 14).map((i) => <Thumb key={i.assetId} id={i.assetId} className="thumb" alt={i.title} />)}{found.images.length > 14 && <span className="muted">+{found.images.length - 14}</span>}</div>
            )}
            {found.documents.length > 0 && <p className="muted small">Documents: {found.documents.map((d) => `${d.name}${d.truncated ? " (first part only)" : ""}`).join(", ")}</p>}
            {found.audio.length > 0 && <p className="muted small">Audio named like an image (for example <span className="mono">07.mp3</span> with <span className="mono">07.png</span>) is connected to that shot as its voice. Other audio is placed beside the shots.</p>}
            {found.skipped.length > 0 && <p className="muted small">Left out: {found.skipped.slice(0, 6).join(", ")}{found.skipped.length > 6 ? "…" : ""}</p>}
            {nothing && <div className="result bad">Nothing usable was found in these files.</div>}
            {found.images.length > 0 && (
              <div className="field-row">
                <label>Each image becomes</label>
                <div className="segmented small" role="radiogroup" aria-label="What to build">
                  <button role="radio" aria-checked={kind === "video"} className={kind === "video" ? "on" : ""} onClick={() => setKind("video")}>A video shot</button>
                  <button role="radio" aria-checked={kind === "image"} className={kind === "image" ? "on" : ""} onClick={() => setKind("image")}>A new image</button>
                </div>
              </div>
            )}
            {busy && <div className="bundle-wait"><span className="spinner" /> {status}</div>}
            {!busy && !nothing && (
              <div className="bundle-actions">
                {found.images.length > 0 && found.promptsFile && (
                  <button className="btn generate" onClick={fromFile}><Icon name="play" size={15} />Build {found.images.length} shots from the included prompts file</button>
                )}
                {found.images.length > 0 && !found.promptsFile && found.documents.length > 0 && (
                  <>
                    <button className="btn generate" onClick={withAi}><Icon name="play" size={15} />Build {found.images.length} shots with their prompts</button>
                    <div className="gen-note">{mock ? <><ProviderBadge provider="mock" /> Simulation: files are placed, prompts stay empty.</> : <>{requests} chat {requests === 1 ? "request" : "requests"} × {chatLabel} read your documents and write each shot’s prompt. No image or video is generated.</>}</div>
                  </>
                )}
                {partial.current.length > 0 && <button className="btn" onClick={() => { setBusy(true); void finish(partial.current); }}>Build with the {partial.current.length} prompts written so far</button>}
                {(found.images.length > 0 || found.audio.length > 0) && (
                  <button className="btn" onClick={() => { setBusy(true); void finish([]); }}>{found.documents.length || found.promptsFile ? "Add the files without prompts" : `Add ${found.images.length ? `${found.images.length} shots` : "the audio"} to the canvas`}</button>
                )}
                {found.images.length === 0 && found.documents.length > 0 && <p className="muted small">These documents hold no pictures. To make images from their prompts, use “Storyboard from prompts…” and paste the text.</p>}
              </div>
            )}
          </>
        )}
      </div>
    </div>
  );
}
