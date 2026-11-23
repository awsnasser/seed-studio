import { createPortal } from "react-dom";
import { createContext, useCallback, useContext, useEffect, useLayoutEffect, useMemo, useRef, useState } from "react";
import {
  Background, Controls, Handle, NodeResizer, Position, ReactFlow, ReactFlowProvider, SelectionMode, applyEdgeChanges, applyNodeChanges, useReactFlow,
  type Connection, type Edge, type EdgeChange, type Node, type NodeChange, type NodeProps,
} from "@xyflow/react";
import { AppError, listen, rpc } from "../api";
import { Icon, Project, ProviderBadge, SETTING_LABEL, STATUS_LABEL, Thumb, VALUE_LABEL, useShell } from "../shared";
import { importImage, useAssets } from "./Gallery";
import { AspectPicker } from "./ImageOptions";
import { ROLE_LABEL, VideoPlanDialog, useVideos } from "./Video";
import { assetUrl } from "../api";
import StoryboardDialog from "./Storyboard";
import BundleDialog from "./Bundle";
import { importImages, importPrompts, importShotPackage } from "./ShotPackage";
import { AudioClipEditor, VoiceSection } from "./Voice";
import { fileToBase64, pickFile, pickFiles } from "../api";
import { inside, tidy, type Box } from "./Tidy";
import { useElements, type Element } from "./Elements";

/** The gateway's character library: its entries, and the entry each image node would send in a video. */
type Library = { available: boolean; uploads?: boolean; uploaded?: Record<string, string>; assets: { id: string; name: string; status: string; group: string }[]; byNode: Record<string, { id: string; name: string }>; fetchedAt?: number };
const NO_LIBRARY: Library = { available: false, assets: [], byNode: {} };
const LibraryCtx = createContext<{ library: Library; refresh: (fetch?: boolean) => Promise<void>; add: (assetId: string) => Promise<void>; adding: string | null }>({ library: NO_LIBRARY, refresh: async () => {}, add: async () => {}, adding: null });

type GNode = { id: string; type: string; position: { x: number; y: number }; data: any };
type GEdge = { id: string; source: string; sourceHandle: string; target: string; targetHandle: string; data?: { order: number } };
type CanvasState = {
  revision: number; graph: { nodes: GNode[]; edges: GEdge[] }; viewport: { x: number; y: number; zoom: number };
  results: Record<string, any>; undo: { label: string; actor: string } | null; redo: { label: string; actor: string } | null;
  notes?: string[]; createdIds?: string[];
};

const KIND_LABEL: Record<string, string> = { prompt: "Prompt", imageAsset: "Reference image", seedText: "Rewrite with Seed", seedream: "Seedream", preview: "Result", seedance: "Seedance video", describe: "Describe image", audio: "Audio clip", note: "Note", group: "Group" };
/** Colours for notes and groups, with what each usually means. */
const COLORS: [string, string][] = [["gray", "To do"], ["amber", "In progress"], ["green", "Done"], ["red", "Needs fixing"], ["blue", "Info"], ["purple", "Idea"]];
/** Size used for a node React Flow has not measured yet (it only measures what is on screen). */
const SIZE: Record<string, [number, number]> = { prompt: [220, 130], imageAsset: [220, 200], seedText: [220, 150], seedream: [220, 340], preview: [220, 260], seedance: [240, 330], describe: [220, 150], audio: [220, 130], note: [220, 130], group: [420, 300] };
/** What past generations took and used (from this app's own records), for progress and size estimates. */
type Stats = { video: Record<string, { n: number; secs: number | null; tokensPerSec: number | null }>; image: { n: number; secs: number | null; slowest?: number; tokens: number | null; by?: Record<string, { n: number; secs: number | null; slowest: number }> }; project: { videoTokens: number; videos: number; imageTokens: number; images: number } };
const NO_STATS: Stats = { video: {}, image: { n: 0, secs: null, tokens: null }, project: { videoTokens: 0, videos: 0, imageTokens: 0, images: 0 } };
const StatsCtx = createContext<{ stats: Stats; prices: { videoPerM: number | null; imageEach: number | null } }>({ stats: NO_STATS, prices: { videoPerM: null, imageEach: null } });
const fmtTok = (n: number) => (n >= 1e6 ? `${(n / 1e6).toFixed(2)}M` : n >= 1000 ? `${(n / 1000).toFixed(1)}K` : String(Math.round(n)));
const fmtUsd = (n: number) => `≈ $${n < 0.1 ? n.toFixed(3) : n.toFixed(2)}`;
const clock = (s: number) => `${Math.floor(s / 60)}:${String(Math.floor(s % 60)).padStart(2, "0")}`;
/** Tokens (and money, when the owner entered a price) one result used. */
function useCost() {
  const { prices } = useContext(StatsCtx);
  return {
    video: (tokens?: number | null) => (tokens ? `${fmtTok(tokens)} tokens${prices.videoPerM ? ` · ${fmtUsd((tokens / 1e6) * prices.videoPerM)}` : ""}` : ""),
    image: (tokens?: number | null) => (tokens ? `${fmtTok(tokens)} tokens${prices.imageEach ? ` · ${fmtUsd(prices.imageEach)}` : ""}` : ""),
  };
}
/** Estimated progress: the provider reports no percentage, so the bar follows the usual time of past runs. */
function Progress({ start, estimate, label, slowest }: { start: number; estimate: number; label: string; slowest?: number }) {
  const [now, setNow] = useState(Date.now());
  useEffect(() => { const t = window.setInterval(() => setNow(Date.now()), 500); return () => window.clearInterval(t); }, []);
  const elapsed = Math.max(0, (now - start) / 1000);
  // Fills to 95% over the usual time, then creeps: it never claims to be finished before the result is here.
  const part = elapsed <= estimate ? (elapsed / estimate) * 0.95 : 0.95 + 0.04 * (1 - Math.exp(-(elapsed - estimate) / estimate));
  return (
    <div className="progress" role="progressbar" aria-valuemin={0} aria-valuemax={100} aria-valuenow={Math.round(part * 100)} aria-label={label}>
      <span className="progress-label">{label}</span>
      <div className="progress-track"><div className="progress-fill" style={{ width: `${part * 100}%` }} /></div>
      <span className="progress-time">{clock(elapsed)} of ≈ {clock(estimate)}{elapsed > Math.max(estimate * 1.6, slowest ?? 0) ? " · slower than any before, still working" : elapsed > estimate * 1.25 ? " · on the slow side, still working" : ""}</span>
    </div>
  );
}
const ActionsCtx = createContext<{ resizeGroup: (id: string, r: { x: number; y: number; width: number; height: number }) => void }>({ resizeGroup: () => {} });
const OUT_TYPE: Record<string, string> = { prompt: "text", seedText: "text", describe: "text", audio: "audio", imageAsset: "image", seedream: "image" };
const IN_TYPE: Record<string, string> = { "seedText.prompt": "text", "seedream.prompt": "text", "seedream.refs": "image", "preview.image": "image", "describe.image": "image", "seedance.audio": "audio", "seedance.prompt": "text", "seedance.first": "image", "seedance.last": "image", "seedance.refs": "image" };
const VIDEO_STATUS: Record<string, string> = { submitting: "Sending…", queued: "Queued", running: "Generating…", succeeded: "Ready", failed: "Failed", uncertain: "Outcome unknown" };
const RUN_STATUS: Record<string, string> = { sending: "Sending…", succeeded: "Done", failed: "Failed", uncertain: "Outcome unknown", skipped: "Not run" };

// ───────────── node renderers (canvas is LTR; node text is RTL) ─────────────

function Shell_({ kind, title, selected, children, status }: { kind: string; title: string; selected?: boolean; children?: React.ReactNode; status?: string }) {
  return (
    <div className={`node node-${kind} ${selected ? "selected" : ""} ${status ? `st-${status}` : ""}`}>
      <div className="node-head"><span className="node-kind">{KIND_LABEL[kind]}</span><span className="node-title" dir="auto">{title}</span></div>
      {children}
    </div>
  );
}
const Out = ({ type }: { type: string }) => <Handle type="source" position={Position.Right} id={type} className={`port ${type}`} title={type === "text" ? "Text" : type === "audio" ? "Audio" : "Image"} />;
const In = ({ id, type, top, label }: { id: string; type: string; top: number; label: string }) => (
  <>
    <Handle type="target" position={Position.Left} id={id} className={`port ${type}`} style={{ top }} title={label} />
    <span className="port-label" style={{ top: top - 8 }}>{label}</span>
  </>
);

function PromptNode({ data, selected }: NodeProps) {
  const d = data as any;
  return <Shell_ kind="prompt" title={d.title ?? "Prompt"} selected={selected}><p className="node-text" dir="auto">{d.text || <i className="muted">Write the prompt in the Properties panel</i>}</p><Out type="text" /></Shell_>;
}
function AssetNode({ id, data, selected }: NodeProps) {
  const d = data as any;
  const hit = useContext(LibraryCtx).library.byNode[id];
  return (
    <Shell_ kind="imageAsset" title={d.title ?? "Reference"} selected={selected}>
      {d.assetId ? <Thumb id={d.assetId} className="node-img" /> : <p className="node-text muted">No image</p>}
      {hit && <span className="lib-badge" title={`Videos send this as the library image “${hit.name}”, so its face is accepted`}><Icon name="check" size={11} />Character library</span>}
      <Out type="image" />
    </Shell_>
  );
}
function SeedTextNode({ data, selected }: NodeProps) {
  const d = data as any;
  return (
    <Shell_ kind="seedText" title={d.title ?? "Rewrite"} selected={selected} status={d._result?.status}>
      <p className="node-text" dir="auto">{d._result?.text ?? d.instructions ?? <i className="muted">Rewrite instructions</i>}</p>
      <In id="prompt" type="text" top={40} label="prompt" /><Out type="text" />
    </Shell_>
  );
}
function AudioNode({ data, selected }: NodeProps) {
  const d = data as any;
  const has = Number(d.end || 0) > Number(d.start || 0);
  return (
    <Shell_ kind="audio" title={d.title ?? "Audio"} selected={selected}>
      <div className="audio-body"><Icon name="film" size={16} /><span>{d.audioId ? (has ? `Clip ${Number(d.start || 0).toFixed(1)}–${Number(d.end).toFixed(1)}s` : "Whole file") : "No audio yet — double-click"}</span></div>
      <Out type="audio" />
    </Shell_>
  );
}
function DescribeNode({ data, selected }: NodeProps) {
  const d = data as any;
  return (
    <Shell_ kind="describe" title={d.title ?? "Describe"} selected={selected} status={d._result?.status}>
      <p className="node-text" dir="auto">{d._result?.text ?? <i className="muted">Run it to turn the image into a prompt</i>}</p>
      <In id="image" type="image" top={40} label="image" /><Out type="text" />
    </Shell_>
  );
}
function SeedreamNode({ data, selected }: NodeProps) {
  const { stats } = useContext(StatsCtx);
  const cost = useCost();
  // What this model usually takes, with or without a reference image.
  const usual = stats.image.by?.[`${(data as any)._result?.modelId}:${(data as any)._hasRefs ? "ref" : "text"}`];
  const d = data as any;
  const r = d._result;
  return (
    <Shell_ kind="seedream" title={d.title ?? "Generate"} selected={selected} status={r?.status}>
      <div className="node-model">{d._modelLabel}</div>
      <div className="node-chips">{Object.entries(d.settings ?? {}).map(([k, v]) => <span key={k} className="chip">{VALUE_LABEL[v as string] ?? (v as string)}</span>)}</div>
      <p className={`node-prompt-text ${d._promptFrom ? "linked" : ""}`} dir="auto">{d._promptFrom ? `Prompt from “${d._promptFrom}”` : d.prompt || <i>Double-click to write the prompt</i>}</p>
      {r?.status === "sending"
        ? <div className="node-player running square"><Progress start={r.startedAt ?? Date.now()} estimate={usual?.secs ?? stats.image.secs ?? 30} slowest={usual?.slowest ?? stats.image.slowest} label="Generating…" /></div>
        : r?.assetId ? <Thumb id={r.assetId} className="node-img" /> : <div className="node-player empty square"><Icon name="image" size={22} /><span>The image will appear here</span></div>}
      {r?.status && r.status !== "succeeded" && r.status !== "sending" && <div className={`node-status ${r.status}`}>{RUN_STATUS[r.status] ?? r.status}</div>}
      {r?.status === "succeeded" && r.provider !== "mock" && cost.image(r.usage?.total_tokens) && <div className="node-cost">{cost.image(r.usage?.total_tokens)}</div>}
      {r?.provider === "mock" && r?.assetId && <ProviderBadge provider="mock" />}
      <In id="prompt" type="text" top={44} label="prompt" /><In id="refs" type="image" top={72} label="refs" /><Out type="image" />
    </Shell_>
  );
}
function PreviewNode({ data, selected }: NodeProps) {
  const d = data as any;
  const r = d._upstream;
  return (
    <Shell_ kind="preview" title={d.title ?? "Result"} selected={selected}>
      {r?.assetId ? <Thumb id={r.assetId} className="node-img large" /> : <p className="node-text muted">{r?.status ? RUN_STATUS[r.status] : "Empty until you run"}</p>}
      <In id="image" type="image" top={40} label="image" />
    </Shell_>
  );
}
function SeedanceNode({ data, selected }: NodeProps) {
  const d = data as any;
  const v = d._video;
  const s = d.settings ?? {};
  const busy = v && ["submitting", "queued", "running"].includes(v.status);
  const { stats } = useContext(StatsCtx);
  const cost = useCost();
  const usual = v ? stats.video[`${v.kind}:${v.params?.resolution ?? ""}`]?.secs ?? (v.kind === "final" ? 145 : 75) : 75;
  return (
    <Shell_ kind="seedance" title={d.title ?? "Video"} selected={selected} status={busy ? "sending" : v?.status}>
      <div className="node-model">{d._modelLabel || "Seedance 2.5"}</div>
      <div className="node-chips tall">
        <span className="chip">{s.pass === "final" ? `Final ${s.resolution}` : "Draft 480p"}</span>
        <span className="chip">{s.ratio === "adaptive" ? "Auto ratio" : s.ratio}</span>
        <span className="chip">{s.duration === "-1" ? "Auto length" : `${s.duration}s`}</span>
        {d._audioFrom ? <span className="chip strong">Voice: {d._audioFrom}</span> : s.voice === "on" && <span className="chip strong">Voice {Number(s.voiceStart || 0).toFixed(1)}–{Number(s.voiceEnd || 0).toFixed(1)}s</span>}
      </div>
      <p className={`node-prompt-text ${d._promptFrom ? "linked" : ""}`} dir="auto">{d._promptFrom ? `Prompt from “${d._promptFrom}”` : d.prompt || <i>Double-click to write the prompt</i>}</p>
      <div className={`node-player ${v?.status ?? "empty"}`}>
        {v?.status === "succeeded" && v.hasFile
          ? <video className="nodrag nowheel" src={assetUrl(v.id)} controls playsInline loop preload="metadata" />
          : busy
            ? <Progress start={v.createdAt} estimate={usual} label={VIDEO_STATUS[v.status] ?? v.status} />
            : v
              ? <><Icon name="film" size={22} /><span>{VIDEO_STATUS[v.status] ?? v.status}</span></>
              : <><Icon name="play" size={22} /><span>The video will play here</span></>}
        {v?.status === "succeeded" && <span className="player-tag">{v.kind === "draft" ? "Draft" : "Final"} {v.params?.resolution}{v.provider === "mock" ? " · Simulation" : ""}</span>}
      </div>
      {v?._failed && <div className={`node-status ${v._failed.status}`}>{v._failed.kind === "draft" ? "New draft" : `Final ${v._failed.params?.resolution ?? ""}`} {v._failed.status === "uncertain" ? "outcome unknown" : "failed"} · {v.kind === "draft" ? "draft" : "earlier video"} kept</div>}
      {v?.status === "succeeded" && v.provider !== "mock" && cost.video(v.usage?.total_tokens) && <div className="node-cost">{cost.video(v.usage?.total_tokens)}</div>}
      <In id="prompt" type="text" top={44} label="prompt" /><In id="first" type="image" top={66} label="first" />
      <In id="last" type="image" top={88} label="last" /><In id="refs" type="image" top={110} label="refs" /><In id="audio" type="audio" top={132} label="audio" />
    </Shell_>
  );
}
function NoteNode({ data, selected }: NodeProps) {
  const d = data as any;
  return (
    <div className={`note c-${d.color ?? "amber"} ${selected ? "selected" : ""}`}>
      {d.title && d.title !== "Note" && <b dir="auto">{d.title}</b>}
      <p dir="auto">{d.text || <i>Double-click to write a note</i>}</p>
    </div>
  );
}
function GroupNode({ id, data, selected }: NodeProps) {
  const d = data as any;
  const { resizeGroup } = useContext(ActionsCtx);
  return (
    <div className={`group-frame c-${d.color ?? "gray"} ${selected ? "selected" : ""}`}>
      <NodeResizer isVisible={!!selected} minWidth={160} minHeight={120} onResizeEnd={(_, p) => resizeGroup(id, p)} />
      <div className="group-title" dir="auto" title="Drag here to move the group with everything inside it">{d.title || "Group"}</div>
    </div>
  );
}
const nodeTypes = { note: NoteNode, group: GroupNode, audio: AudioNode, describe: DescribeNode, seedance: SeedanceNode, prompt: PromptNode, imageAsset: AssetNode, seedText: SeedTextNode, seedream: SeedreamNode, preview: PreviewNode };

type View = { density: "full" | "compact"; opacity: number; hideText: boolean; hideRefs: boolean; hideAudio: boolean; hideNotes: boolean; hideLinks: boolean };
const VIEW_FULL: View = { density: "full", opacity: 1, hideText: false, hideRefs: false, hideAudio: false, hideNotes: false, hideLinks: false };
const VIEW_PRESETS: [string, View][] = [
  ["Full", VIEW_FULL],
  ["Compact", { ...VIEW_FULL, density: "compact" }],
  ["Results only", { ...VIEW_FULL, density: "compact", hideText: true, hideRefs: true, hideAudio: true }],
];
const sameView = (a: View, b: View) => (Object.keys(b) as (keyof View)[]).every((k) => a[k] === b[k]);
const hiddenKind = (v: View, type: string) =>
  (v.hideText && ["prompt", "seedText", "describe"].includes(type)) || (v.hideRefs && type === "imageAsset") || (v.hideAudio && type === "audio") || (v.hideNotes && (type === "note" || type === "group"));
function ColourRow({ onPick }: { onPick: (color: string | null) => void }) {
  return (
    <div className="ctx-colours" role="group" aria-label="Colour">
      {COLORS.map(([c, meaning]) => <button key={c} className={`dot c-${c}`} title={meaning} aria-label={meaning} onClick={() => onPick(c)} />)}
      <button className="dot none" title="No colour" aria-label="No colour" onClick={() => onPick(null)} />
    </div>
  );
}

// ───────────── main view ─────────────

export default function CanvasView(props: { project: Project; focusNode?: string }) {
  return <ReactFlowProvider><Inner {...props} /></ReactFlowProvider>;
}

function Inner({ project, focusNode }: { project: Project; focusNode?: string }) {
  const { toast, reviewPlan, resolvedTheme, app } = useShell();
  const rf = useReactFlow();
  const wrap = useRef<HTMLDivElement>(null);
  const [state, setState] = useState<CanvasState | null>(null);
  const [nodes, setNodes] = useState<Node[]>([]);
  const [edges, setEdges] = useState<Edge[]>([]);
  const [panel, setPanel] = useState<"props" | "agent" | null>(() => (localStorage.getItem("canvasPanel") as any) ?? "props");
  const [notes, setNotes] = useState<string[]>([]);
  /** How the canvas is drawn. A view preference kept on this Mac; it changes nothing in the project. */
  const [view, setViewState] = useState<View>(() => { try { return { ...VIEW_FULL, ...JSON.parse(localStorage.getItem("canvasView") ?? "{}") }; } catch { return VIEW_FULL; } });
  const setView = (v: Partial<View>) => setViewState((old) => { const next = { ...old, ...v }; localStorage.setItem("canvasView", JSON.stringify(next)); return next; });
  const [viewOpen, setViewOpen] = useState(false);
  useEffect(() => {
    if (!viewOpen) return;
    const close = (e: MouseEvent) => { if (!(e.target as HTMLElement).closest?.(".view-anchor")) setViewOpen(false); };
    window.addEventListener("pointerdown", close, true);
    return () => window.removeEventListener("pointerdown", close, true);
  }, [viewOpen]);
  const stateRef = useRef<CanvasState | null>(null);
  stateRef.current = state;
  const projectId = project.id;
  const { videos, reload: reloadVideos } = useVideos(projectId);
  const [videoPlan, setVideoPlan] = useState<any>(null);
  const [storyboard, setStoryboard] = useState(false);
  const [menu, setMenu] = useState<{ x: number; y: number; flow: { x: number; y: number }; nodeId?: string; edgeId?: string; from?: { id: string; out: string } } | null>(null);
  const [importing, setImporting] = useState<string | null>(null);
  const [bundle, setBundle] = useState<File[] | null>(null);
  /** Newest video of each video node. */
  const nodeVideo = useMemo(() => {
    const m: Record<string, any> = {};
    const kept: Record<string, any> = {};
    for (const v of Object.values(videos).sort((a: any, b: any) => a.createdAt - b.createdAt) as any[]) {
      if (!v.nodeId) continue;
      m[v.nodeId] = v;
      if (v.status === "succeeded" && v.hasFile) kept[v.nodeId] = v;
    }
    // A later attempt that failed (an upscale refused, say) never takes a finished video off the node:
    // the last good one stays in place and stays usable, with the failure shown beside it.
    for (const [id, v] of Object.entries(m)) {
      if ((v.status === "failed" || v.status === "uncertain") && kept[id]) m[id] = { ...kept[id], _failed: v };
    }
    return m;
  }, [videos]);
  /**
   * One video = one request, described on the Generate button itself, so it starts
   * straight away: no window in front. The node shows the progress.
   */
  const inFlight = useRef(new Set<string>());
  const planVideo = async (args: Record<string, unknown>, cmd = "video_plan_node") => {
    const key = String(args.nodeId ?? args.draftOf ?? "");
    if (inFlight.current.has(key)) return; // a second click never creates a second task
    inFlight.current.add(key);
    try {
      const plan = await rpc(cmd, { projectId, ...args });
      const notes: string[] = (plan.notes ?? []).filter((n: string) => !n.startsWith("Drafts are always") && !n.startsWith("The final video reuses"));
      // What was adjusted is said in a few words; the full explanation stays in the README, and anything
      // that costs or uploads something is still spelled out.
      const brief = notes.map((n) =>
        n.startsWith("Sent from the gateway") ? `face sent from the character library (${n.split(": ").pop()?.replace(/\.$/, "")})`
        : n.startsWith("With a voice clip") ? "voice clip attached for lip sync"
        : n.startsWith("Library images go in") ? ""
        : n).filter(Boolean);
      toast(`Video started${brief.length ? " · " + brief.join(" · ") : ""}`);
      const started = rpc("video_start", { videoId: plan.id });
      setTimeout(() => void reloadVideos(), 200); // shows “Sending…” on the node while the request is uploaded
      const v = await started;
      if (v.status === "failed" || v.status === "uncertain") toast(v.error ?? "The request failed.", "error");
    } catch (e) { toast((e as Error).message, "error"); } finally { inFlight.current.delete(key); void reloadVideos(); void refreshLibrary(); }
  };

  const [stats, setStats] = useState<Stats>(NO_STATS);
  const statsCtx = useMemo(() => ({ stats, prices: app.prices ?? { videoPerM: null, imageEach: null } }), [stats, app.prices]);
  // Re-read after anything finishes; it only reads this app's own records.
  const finished = useMemo(() => Object.values(videos).filter((v: any) => v.status === "succeeded").length + Object.values(state?.results ?? {}).filter((r: any) => r.status === "succeeded").length, [videos, state?.results]);
  useEffect(() => { void rpc<Stats>("gen_stats", { projectId }).then(setStats).catch(() => {}); }, [projectId, finished]);
  /** Gateway budget: read only when asked for (it uses the key), then again after each finished generation. */
  const [balance, setBalance] = useState<string | null>(null);
  const [balanceBusy, setBalanceBusy] = useState(false);
  const readBalance = useCallback(async (quiet = false) => {
    setBalanceBusy(true);
    try { setBalance(balanceText(await rpc("gateway_balance"))); }
    catch (e) { if (!quiet) toast((e as Error).message, "error"); } finally { setBalanceBusy(false); }
  }, [toast]);
  const balanceWanted = useRef(false);
  useEffect(() => { if (balanceWanted.current) void readBalance(true); }, [finished, readBalance]);

  const [library, setLibrary] = useState<Library>(NO_LIBRARY);
  const hasLibrary = app.providerMode === "modelark" && !!app.gateway?.assetsUrl;
  const refreshLibrary = useCallback(async (fetch = false) => {
    if (!hasLibrary) return setLibrary(NO_LIBRARY);
    try { setLibrary(await rpc<Library>("gateway_library", { projectId, refresh: fetch })); }
    catch (e) { if (fetch) toast((e as Error).message, "error"); }
  }, [projectId, hasLibrary]);
  const [adding, setAdding] = useState<string | null>(null);
  const addingRef = useRef(false);
  const addToLibrary = useCallback(async (assetId: string) => {
    if (addingRef.current) return; // one upload at a time; a second click never sends a second one
    addingRef.current = true;
    setAdding(assetId);
    try {
      setLibrary(await rpc<Library>("library_add", { projectId, assetId }));
      toast("Added to the gateway’s character library.");
    } catch (e) { toast((e as Error).message, "error"); } finally { addingRef.current = false; setAdding(null); }
  }, [projectId]);
  const libraryCtx = useMemo(() => ({ library, refresh: refreshLibrary, add: addToLibrary, adding }), [library, refreshLibrary, addToLibrary, adding]);
  // The cached list only: opening the canvas sends nothing and does not read the key.
  useEffect(() => { void refreshLibrary(); }, [refreshLibrary, state?.revision]);

  const load = useCallback(async () => {
    const got = await rpc<CanvasState>("canvas_get", { projectId });
    // A refresh that was asked for before a newer edit landed must not put the older canvas back.
    if (stateRef.current && (stateRef.current as any).projectId === projectId && got.revision < stateRef.current.revision) return;
    stateRef.current = got;
    setState(got);
  }, [projectId]);
  useEffect(() => {
    void load();
    const offs = [
      listen("canvas-changed", (p) => p.projectId === projectId && void load()),
      listen("run-update", (p) => p.projectId === projectId && void load()),
    ];
    return () => offs.forEach((off) => off());
  }, [projectId, load]);

  // Server graph → React Flow nodes/edges, keeping local selection.
  useEffect(() => {
    if (!state) return;
    const label = (key: string) => app.models.find((m) => m.key === key)?.label ?? key;
    const want = selectNext.current && state.graph.nodes.some((n) => n.id === selectNext.current) ? selectNext.current : null;
    if (want) selectNext.current = null;
    setNodes((prev) => state.graph.nodes.map((n) => {
      const upstream = n.type === "preview" ? state.graph.edges.find((e) => e.target === n.id)?.source : undefined;
      const from = (handle: string) => { const e = state.graph.edges.find((x) => x.target === n.id && x.targetHandle === handle); return e ? state.graph.nodes.find((x) => x.id === e.source)?.data.title ?? "a node" : undefined; };
      const old = prev.find((p) => p.id === n.id);
      // A group is a frame behind the other nodes: only its title and edge take the mouse, so a box selection can start inside it.
      const frame = n.type === "group" ? { width: n.data.width ?? SIZE.group[0], height: n.data.height ?? SIZE.group[1], zIndex: -1, className: "group-wrap" } : { className: n.data.color && n.type !== "note" ? `c-${n.data.color}` : "" };
      return {
        ...old, // keeps React Flow's measured size, so nodes are not hidden and re-measured on every refresh
        ...frame,
        id: n.id, type: n.type, position: n.position, selected: want ? n.id === want : old?.selected ?? false, hidden: hiddenKind(view, n.type),
        data: { ...n.data, _result: state.results[n.id], _upstream: upstream ? state.results[upstream] : undefined, _modelLabel: label(n.data.modelKey ?? (n.type === "seedance" ? "seedance-2.5" : undefined)), _video: nodeVideo[n.id], _promptFrom: from("prompt"), _hasRefs: state.graph.edges.some((x) => x.target === n.id && x.targetHandle === "refs"), _audioFrom: n.type === "seedance" ? from("audio") : undefined },
      };
    }));
    const gone = new Set(state.graph.nodes.filter((n) => hiddenKind(view, n.type)).map((n) => n.id));
    setEdges((prev) => state.graph.edges.map((e) => ({
      hidden: view.hideLinks || gone.has(e.source) || gone.has(e.target),
      id: e.id, source: e.source, target: e.target, sourceHandle: e.sourceHandle, targetHandle: e.targetHandle,
      selected: prev.find((p) => p.id === e.id)?.selected ?? false,
      className: OUT_TYPE[state.graph.nodes.find((n) => n.id === e.source)?.type ?? ""] ?? "",
      label: e.targetHandle === "refs" && state.graph.edges.filter((x) => x.target === e.target && x.targetHandle === "refs").length > 1 ? String((e.data?.order ?? 0) + 1) : undefined,
    })));
  }, [state, app.models, nodeVideo, view]);

  useEffect(() => {
    if (!focusNode || !state) return;
    setNodes((ns) => ns.map((n) => ({ ...n, selected: n.id === focusNode })));
    setTimeout(() => rf.fitView({ nodes: [{ id: focusNode }], duration: 300, maxZoom: 1, padding: 1 }), 50);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [focusNode, state === null]);

  /** A node just created is selected once the canvas has drawn it. */
  const selectNext = useRef<string | null>(null);
  const selectOnly = (id: string) => { selectNext.current = id; setNodes((ns) => ns.map((n) => ({ ...n, selected: n.id === id }))); setState((s) => (s ? { ...s } : s)); };
  /** Every edit, by hand or from a button, goes through the same validated patch. */
  const queue = useRef<Promise<unknown>>(Promise.resolve());
  const patch = useCallback((operations: any[], label: string): Promise<CanvasState | null> => {
    // One edit at a time: two quick edits (a title, then a colour) would otherwise both start
    // from the same revision and the second would be refused as out of date.
    const job = queue.current.then(async (): Promise<CanvasState | null> => {
      const send = (revision: number) => rpc<CanvasState>("canvas_patch", { projectId, expectedRevision: revision, operations, label });
      const cur = stateRef.current;
      if (!cur) return null;
      try {
        let next: CanvasState;
        try {
          next = await send(cur.revision);
        } catch (e) {
          if ((e as AppError).code !== "conflict") throw e;
          // The canvas moved on (a run finished, the assistant edited it): read it and apply this edit once more.
          const fresh = await rpc<CanvasState>("canvas_get", { projectId });
          stateRef.current = fresh;
          next = await send(fresh.revision);
        }
        stateRef.current = next;
        setState(next);
        setNotes(next.notes ?? []);
        return next;
      } catch (e) {
        const err = e as AppError;
        toast(err.code === "conflict" ? "The canvas was changing at the same moment, so that edit was not applied. Try it again." : err.message, "error");
        if (err.code === "conflict") void load();
        else setState({ ...(stateRef.current ?? cur) }); // snap local drags/edits back to the saved graph
        return null;
      }
    });
    queue.current = job.catch(() => null);
    return job;
  }, [projectId, toast, load]);

  const selectedIds = useMemo(() => nodes.filter((n) => n.selected).map((n) => n.id), [nodes]);
  const selectedNode = selectedIds.length === 1 ? state?.graph.nodes.find((n) => n.id === selectedIds[0]) : undefined;

  const onNodesChange = (changes: NodeChange[]) => setNodes((ns) => applyNodeChanges(changes.filter((c) => c.type !== "remove"), ns));
  const onEdgesChange = (changes: EdgeChange[]) => setEdges((es) => applyEdgeChanges(changes.filter((c) => c.type !== "remove"), es));
  const isValidConnection = (c: Connection | Edge) => {
    const g = stateRef.current?.graph;
    const s = g?.nodes.find((n) => n.id === c.source);
    const t = g?.nodes.find((n) => n.id === c.target);
    return !!s && !!t && c.source !== c.target && OUT_TYPE[s.type] === IN_TYPE[`${t.type}.${c.targetHandle}`];
  };
  const onConnect = (c: Connection) => void patch([{ op: "connect", source: c.source, target: c.target, targetHandle: c.targetHandle }], "Connect");
  /** Edits larger than the validator's 200-operation limit go in several steps. */
  const patchMany = async (ops: any[], label: string) => {
    for (let i = 0; i < ops.length; i += 200) if (!(await patch(ops.slice(i, i + 200), label))) return false;
    return true;
  };
  const boxOf = (n: Node): Box => ({ id: n.id, type: n.type ?? "", x: n.position.x, y: n.position.y, w: n.width ?? n.measured?.width ?? SIZE[n.type ?? ""]?.[0] ?? 220, h: n.height ?? n.measured?.height ?? SIZE[n.type ?? ""]?.[1] ?? 160 });
  /** Dragging a group carries what is inside it. */
  const carried = useRef<{ from: { x: number; y: number }; lead: string; members: { id: string; x: number; y: number }[] } | null>(null);
  const onNodeDragStart = (_: unknown, node: Node, dragged: Node[]) => {
    const moving = new Set(dragged.map((n) => n.id));
    const frames = dragged.filter((n) => n.type === "group").map(boxOf);
    const members = frames.length ? nodes.filter((n) => !moving.has(n.id) && n.type !== "group" && frames.some((f) => inside(boxOf(n), f))).map((n) => ({ id: n.id, x: n.position.x, y: n.position.y })) : [];
    carried.current = members.length ? { from: { ...node.position }, lead: node.id, members } : null;
  };
  const onNodeDrag = (_: unknown, node: Node) => {
    const c = carried.current;
    if (!c || c.lead !== node.id) return;
    const dx = node.position.x - c.from.x, dy = node.position.y - c.from.y;
    const at = new Map(c.members.map((m) => [m.id, { x: m.x + dx, y: m.y + dy }]));
    setNodes((ns) => ns.map((n) => (at.has(n.id) ? { ...n, position: at.get(n.id)! } : n)));
  };
  const onNodeDragStop = (_: unknown, node: Node | null, dragged: Node[]) => {
    const g = stateRef.current?.graph;
    const c = carried.current;
    carried.current = null;
    const moved: { id: string; position: { x: number; y: number } }[] = dragged.map((n) => ({ id: n.id, position: n.position }));
    if (c && node && c.lead === node.id) {
      const dx = node.position.x - c.from.x, dy = node.position.y - c.from.y;
      for (const m of c.members) moved.push({ id: m.id, position: { x: m.x + dx, y: m.y + dy } });
    }
    const ops = moved.filter((n) => {
      const old = g?.nodes.find((x) => x.id === n.id)?.position;
      return old && (old.x !== n.position.x || old.y !== n.position.y);
    }).map((n) => ({ op: "moveNode", nodeId: n.id, position: { x: Math.round(n.position.x), y: Math.round(n.position.y) } }));
    if (ops.length) void patchMany(ops, ops.length > 1 ? `Move ${ops.length} nodes` : "Move");
  };
  const actions = useMemo(() => ({
    resizeGroup: (id: string, r: { x: number; y: number; width: number; height: number }) =>
      void patch([{ op: "moveNode", nodeId: id, position: { x: Math.round(r.x), y: Math.round(r.y) } }, { op: "setNodeData", nodeId: id, changes: { width: Math.round(r.width), height: Math.round(r.height) } }], "Resize group"),
  }), [patch]);
  /** A frame around the selected nodes (or an empty one where asked). */
  const groupSelected = async (at?: { x: number; y: number }) => {
    const picked = nodes.filter((n) => n.selected && n.type !== "group").map(boxOf);
    const pos = at ?? center();
    const frame = picked.length
      ? { x: Math.min(...picked.map((b) => b.x)) - 30, y: Math.min(...picked.map((b) => b.y)) - 46, w: 0, h: 0 }
      : { x: pos.x, y: pos.y, w: 420, h: 300 };
    if (picked.length) { frame.w = Math.max(...picked.map((b) => b.x + b.w)) + 30 - frame.x; frame.h = Math.max(...picked.map((b) => b.y + b.h)) + 30 - frame.y; }
    const next = await patch([{ op: "addNode", type: "group", position: { x: Math.round(frame.x), y: Math.round(frame.y) }, data: { title: "Group", color: "gray", width: Math.round(Math.max(160, frame.w)), height: Math.round(Math.max(120, frame.h)) } }], picked.length ? `Group ${picked.length} nodes` : "Add group");
    const id = next?.createdIds?.[0];
    if (id) { selectOnly(id); togglePanelOpen(); }
  };
  /** Tidy up the selection (two or more nodes), or the whole canvas. */
  const tidyUp = async () => {
    const boxes = nodes.map(boxOf);
    const picked = nodes.filter((n) => n.selected && n.type !== "group").map((n) => n.id);
    const { moves, frames } = tidy(boxes, edges.map((e) => ({ source: e.source, target: e.target })), picked.length >= 2 ? picked : boxes.map((b) => b.id));
    const ops: any[] = [];
    for (const [id, p] of Object.entries(moves)) { const b = boxes.find((x) => x.id === id)!; if (b.x !== p.x || b.y !== p.y) ops.push({ op: "moveNode", nodeId: id, position: p }); }
    for (const [id, f] of Object.entries(frames)) ops.push({ op: "moveNode", nodeId: id, position: { x: f.x, y: f.y } }, { op: "setNodeData", nodeId: id, changes: { width: Math.max(160, f.width), height: Math.max(120, f.height) } });
    if (!ops.length) return toast("Already tidy.");
    if (await patchMany(ops, "Tidy up")) {
      toast(ops.length > 200 ? "Tidied up. Undo brings it back in a few steps." : "Tidied up. Undo brings the old layout back.");
      if (picked.length < 2) setTimeout(() => rf.fitView({ padding: 0.15, duration: 300 }), 80);
    }
  };
  const removeSelected = () => {
    const gone = new Set(selectedIds);
    const ops: any[] = selectedIds.map((id) => ({ op: "removeNode", nodeId: id }));
    for (const e of edges) if (e.selected && !gone.has(e.source) && !gone.has(e.target)) ops.push({ op: "disconnect", edgeId: e.id });
    if (ops.length) void patch(ops, "Delete");
  };
  const onKeyDown = (e: React.KeyboardEvent) => {
    const tag = (e.target as HTMLElement).tagName;
    if (tag === "INPUT" || tag === "TEXTAREA" || tag === "SELECT") return;
    if (e.key === "Delete" || e.key === "Backspace") removeSelected();
    if ((e.metaKey || e.ctrlKey) && e.key.toLowerCase() === "z") { e.preventDefault(); void history(e.shiftKey ? "canvas_redo" : "canvas_undo"); }
  };
  const history = async (cmd: "canvas_undo" | "canvas_redo") => {
    try {
      const next = await rpc<CanvasState & { label: string }>(cmd, { projectId });
      setState(next);
      toast(`${cmd === "canvas_undo" ? "Undid" : "Redid"}: ${next.label}`);
    } catch (e) { toast((e as Error).message, "error"); }
  };

  const center = () => {
    const r = wrap.current!.getBoundingClientRect();
    const p = rf.screenToFlowPosition({ x: r.left + r.width / 2, y: r.top + r.height / 2 });
    const jitter = (stateRef.current?.graph.nodes.length ?? 0) % 5 * 24;
    return { x: Math.round(p.x - 110 + jitter), y: Math.round(p.y - 60 + jitter) };
  };
  const addNode = async (type: string, data: any = {}, at?: { x: number; y: number }) => {
    const next = await patch([{ op: "addNode", type, position: at ?? center(), data: { title: KIND_LABEL[type], ...data } }], `Add ${KIND_LABEL[type]}`);
    const id = next?.createdIds?.[0];
    if (id) selectOnly(id);
  };
  const addReference = async (at?: { x: number; y: number }) => {
    try {
      const a = await importImage(projectId);
      if (a) await addNode("imageAsset", { title: a.title, assetId: a.id }, at);
    } catch (e) { toast((e as Error).message, "error"); }
  };
  /**
   * Starts a plan without a window in front of the canvas (the owner's request): what is being sent is
   * said in one line, each node shows its own progress, and a bar offers to stop the rest.
   * The plan window is kept only for a plan that cannot run as it is, because it says what to fix.
   */
  const [batch, setBatch] = useState<{ id: string; text: string } | null>(null);
  const startPlan = async (plan: any) => {
    const fresh = (plan.items as any[]).filter((i) => i.action === "new").length;
    if (plan.blocked > 0 || plan.items.length === 0) return void (await reviewPlan(plan));
    if (fresh > 1) {
      const parts = (plan.byModel as any[]).map((m) => `${m.count} × ${m.label}`);
      if (plan.newTexts) parts.push(`${plan.newTexts} text`);
      toast(`Started ${fresh} requests: ${parts.join(", ")}`);
      setBatch({ id: plan.id, text: `Generating ${fresh} requests, one after another` });
    }
    try {
      const r = await rpc("run_execute", { planId: plan.id });
      const bad = ((r.results as any[] | undefined) ?? []).filter((x) => x.status === "failed" || x.status === "uncertain");
      if (r.stale) toast("The inputs changed while starting. Nothing was sent; press Run again.", "error");
      else if (bad.length) toast(bad.length > 1 ? `${bad.length} requests did not finish. The first: ${bad[0].error ?? "failed"}` : bad[0].error ?? "The request failed.", "error");
      else if (fresh > 1) toast(r.status === "completed" ? `Done: ${fresh} requests finished.` : "Stopped. Requests that had already started were kept.");
    } finally { setBatch(null); }
  };
  const run = async (ids: string[], forceNew: boolean) => {
    const videoNodes = ids.filter((id) => stateRef.current?.graph.nodes.find((n) => n.id === id)?.type === "seedance");
    if (videoNodes.length === 1 && ids.length === 1) return planVideo({ nodeId: videoNodes[0] });
    if (videoNodes.length > 0 && ids.length === videoNodes.length) return toast("Run video nodes one at a time: each is a separate paid task with its own plan.");
    const runnable = ids.filter((id) => ["seedream", "seedText", "describe", "preview"].includes(stateRef.current?.graph.nodes.find((n) => n.id === id)?.type ?? ""));
    if (!runnable.length) return toast("Select a generation or result node to run.");
    if (runnable.some((id) => inFlight.current.has(id))) return; // already starting
    runnable.forEach((id) => inFlight.current.add(id));
    try {
      await startPlan(await rpc("plan_run", { projectId, nodeIds: runnable, forceNew }));
      void load();
    } catch (e) { toast((e as Error).message, "error"); } finally { runnable.forEach((id) => inFlight.current.delete(id)); }
  };
  const storyboardDone = async (built?: { generators: string[]; run: boolean }) => {
    setStoryboard(false);
    if (!built) return;
    setNodes((ns) => ns.map((n) => ({ ...n, selected: built.generators.includes(n.id) })));
    setTimeout(() => rf.fitView({ nodes: built.generators.map((id) => ({ id })), padding: 0.15, duration: 300 }), 120);
    if (!built.run) return toast(`Added ${built.generators.length} frames. They are selected — press “Run selected” when ready.`);
    try {
      await startPlan(await rpc("plan_run", { projectId, nodeIds: built.generators, forceNew: false }));
      void load();
    } catch (e) { toast((e as Error).message, "error"); }
  };
  const importPackage = async () => {
    const file = await pickFile(".json,application/json");
    if (!file) return;
    setImporting("Reading the package…");
    try {
      const ids = await importShotPackage(projectId, file, stateRef.current?.graph.nodes ?? [], patch, setImporting);
      setNodes((ns) => ns.map((n) => ({ ...n, selected: false })));
      setTimeout(() => rf.fitView({ nodes: ids.slice(0, 3).map((id) => ({ id })), padding: 0.6, duration: 300 }), 150);
      toast(`${ids.length} shots are on the canvas, each set to Draft. Nothing was generated — select a video node to review it, then run it.`);
    } catch (e) { toast((e as Error).message, "error"); } finally { setImporting(null); void load(); }
  };
  const importManyImages = async () => {
    const files = await pickFiles("image/png,image/jpeg,image/webp");
    if (!files.length) return;
    setImporting("Reading the images…");
    try {
      const n = await importImages(projectId, files, stateRef.current?.graph.nodes ?? [], patch, setImporting);
      setTimeout(() => rf.fitView({ padding: 0.2, duration: 300 }), 150);
      toast(`${n} images are on the canvas, in file-name order. Add their prompts with “Import prompts for frames…”.`);
    } catch (e) { toast((e as Error).message, "error"); } finally { setImporting(null); void load(); }
  };
  const importPromptsFile = async () => {
    const file = await pickFile(".json,application/json");
    if (!file || !stateRef.current) return;
    try {
      const r = await importPrompts(projectId, file, stateRef.current.graph, patch);
      toast(r.missing.length
        ? `Added video nodes with prompts to ${r.added} frames. No frame found for: ${r.missing.slice(0, 5).join(", ")}${r.missing.length > 5 ? "…" : ""}`
        : `Added a video node (Draft) with its prompt to ${r.added} frames. Nothing was generated.`, r.missing.length ? "error" : "info");
    } catch (e) { toast((e as Error).message, "error"); } finally { void load(); }
  };
  /** Bring files or web images onto the canvas at a point: images, audio, or a package/prompts file. */
  const bringIn = async (files: File[], urls: string[], text: string, at: { x: number; y: number }) => {
    // A ZIP, a PDF or documents: read them together and build whole shots.
    const isDoc = (f: File) => /\.(zip|pdf|txt|md|markdown|csv|tsv|srt|html?)$/i.test(f.name);
    if (files.some(isDoc)) { setBundle(files); return; }
    const ops: any[] = [];
    let k = 0;
    const place = () => ({ x: at.x + (k % 4) * 260, y: at.y + Math.floor(k++ / 4) * 220 });
    const tag = Math.random().toString(36).slice(2, 7);
    setImporting("Adding to the canvas…");
    try {
      for (const f of files.sort((a, b) => a.name.localeCompare(b.name, undefined, { numeric: true }))) {
        const title = f.name.replace(/\.[^.]+$/, "").slice(0, 80) || "Image";
        if (/^image\/(png|jpeg|webp)$/.test(f.type)) {
          const a = await rpc("asset_import", { projectId, name: title, base64: await fileToBase64(f) });
          ops.push({ op: "addNode", id: `in${tag}-${k}`, type: "imageAsset", position: place(), data: { title, assetId: a.id } });
        } else if (f.type.startsWith("audio/") || /\.(mp3|wav|m4a)$/i.test(f.name)) {
          const a = await rpc("audio_import", { projectId, name: f.name, base64: await fileToBase64(f) });
          ops.push({ op: "addNode", id: `in${tag}-${k}`, type: "audio", position: place(), data: { title, audioId: a.id, start: "0", end: String(Math.min(30, Math.round(a.durationMs / 100) / 10)) } });
        } else if (f.type === "application/json" || /\.json$/i.test(f.name)) {
          const head = await f.slice(0, 200).text();
          if (head.includes("seed-studio-shots")) { await importShotPackage(projectId, f, stateRef.current?.graph.nodes ?? [], patch, setImporting); toast("The shot package is on the canvas. Nothing was generated."); }
          else if (head.includes("seed-studio-prompts") && stateRef.current) { const r = await importPrompts(projectId, f, stateRef.current.graph, patch); toast(`Added video nodes with prompts to ${r.added} frames.`); }
          else toast("That JSON file is not a shot package or a prompts file.", "error");
        } else if (f.type.startsWith("video/")) {
          toast("Video files cannot be used as inputs yet: Seedance accepts reference videos only from a public link, not from this Mac.", "error");
        } else {
          toast(`“${f.name}” is not an image, an audio file or a shot package.`, "error");
        }
      }
      for (const url of urls) {
        const a = await rpc("asset_import_url", { projectId, url });
        ops.push({ op: "addNode", id: `in${tag}-${k}`, type: "imageAsset", position: place(), data: { title: a.title, assetId: a.id } });
      }
      if (!files.length && !urls.length && text.trim()) {
        ops.push({ op: "addNode", id: `in${tag}-${k}`, type: "prompt", position: place(), data: { title: "Pasted text", text: text.trim().slice(0, 8000) } });
      }
      if (ops.length) await patch(ops, ops.length === 1 ? "Add to canvas" : `Add ${ops.length} items`);
    } catch (e) { toast((e as Error).message, "error"); } finally { setImporting(null); }
  };
  /** Image links inside dragged or pasted web content. */
  const webImages = (dt: DataTransfer): string[] => {
    const html = dt.getData("text/html");
    const fromHtml = html ? Array.from(new DOMParser().parseFromString(html, "text/html").querySelectorAll("img")).map((i) => i.getAttribute("src") ?? "") : [];
    const uri = (dt.getData("text/uri-list") || "").split("\n").map((u) => u.trim()).filter((u) => u && !u.startsWith("#"));
    const plain = dt.getData("text/plain").trim();
    const all = [...fromHtml, ...uri.filter((u) => /\.(png|jpe?g|webp)(\?|#|$)/i.test(u)), ...(/^https?:\/\/\S+\.(png|jpe?g|webp)(\?\S*)?$/i.test(plain) ? [plain] : [])];
    return Array.from(new Set(all.filter((u) => /^https?:\/\//i.test(u)))).slice(0, 12);
  };
  const onDropFiles = (e: React.DragEvent) => {
    e.preventDefault();
    const files = Array.from(e.dataTransfer.files ?? []);
    const urls = files.length ? [] : webImages(e.dataTransfer);
    if (!files.length && !urls.length) return;
    void bringIn(files, urls, "", rf.screenToFlowPosition({ x: e.clientX, y: e.clientY }));
  };
  // Paste on the canvas: copied images (files or from a web page) become image nodes, text becomes a prompt node.
  useEffect(() => {
    const onPaste = (e: ClipboardEvent) => {
      const el = document.activeElement as HTMLElement | null;
      if (!e.clipboardData || !wrap.current || (el && (["INPUT", "TEXTAREA", "SELECT"].includes(el.tagName) || el.isContentEditable))) return;
      if (document.querySelector(".overlay")) return;
      const files = Array.from(e.clipboardData.files ?? []);
      const urls = files.length ? [] : webImages(e.clipboardData);
      const text = files.length || urls.length ? "" : e.clipboardData.getData("text/plain");
      if (!files.length && !urls.length && !text.trim()) return;
      e.preventDefault();
      const r = wrap.current.getBoundingClientRect();
      void bringIn(files, urls, text, rf.screenToFlowPosition({ x: r.left + r.width / 2 - 100, y: r.top + r.height / 2 - 80 }));
    };
    window.addEventListener("paste", onPaste);
    return () => window.removeEventListener("paste", onPaste);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [projectId]);
  const openMenu = (e: React.MouseEvent | MouseEvent, nodeId?: string) => {
    e.preventDefault();
    const r = wrap.current!.getBoundingClientRect();
    const flow = rf.screenToFlowPosition({ x: e.clientX, y: e.clientY });
    // Right-clicking one of several selected nodes keeps the selection, so it can be grouped or tidied.
    if (nodeId && !nodes.find((n) => n.id === nodeId)?.selected) setNodes((ns) => ns.map((n) => ({ ...n, selected: n.id === nodeId })));
    setMenu({ x: Math.min(e.clientX - r.left, r.width - 230), y: e.clientY - r.top, flow: { x: Math.round(flow.x), y: Math.round(flow.y) }, nodeId });
  };
  const pick = (fn: () => void) => { setMenu(null); fn(); };
  /** Which group of the right-click menu is open; the menu is kept inside the canvas and scrolls when it is taller. */
  const [menuTab, setMenuTab] = useState<string | null>(null);
  const menuRef = useRef<HTMLDivElement>(null);
  useEffect(() => { setMenuTab(null); }, [menu?.x, menu?.y, menu?.nodeId, menu?.edgeId]);
  useLayoutEffect(() => {
    const el = menuRef.current, box = wrap.current;
    if (!el || !box || !menu) return;
    const room = box.clientHeight - 16;
    el.style.maxHeight = `${room}px`;
    el.style.top = `${Math.max(8, Math.min(menu.y, box.clientHeight - 8 - Math.min(el.scrollHeight, room)))}px`;
  }, [menu, menuTab]);
  /** A colour tag on nodes (null clears it). It is only a mark for the owner; nothing is re-generated. */
  const colourNodes = async (ids: string[], color: string | null) => {
    const g = stateRef.current?.graph;
    const ops = ids.filter((id) => g?.nodes.some((n) => n.id === id)).map((nodeId) => ({ op: "setNodeData", nodeId, changes: { color } }));
    if (ops.length) await patchMany(ops, "Colour");
  };
  /** Clicking a connection offers to remove it. */
  const openEdgeMenu = (e: React.MouseEvent, edgeId: string) => {
    e.preventDefault();
    const r = wrap.current!.getBoundingClientRect();
    setEdges((es) => es.map((x) => ({ ...x, selected: x.id === edgeId })));
    setNodes((ns) => ns.map((n) => (n.selected ? { ...n, selected: false } : n)));
    setMenu({ x: Math.min(e.clientX - r.left + 8, r.width - 250), y: e.clientY - r.top + 8, flow: { x: 0, y: 0 }, edgeId });
  };
  const edgeEnds = (edgeId: string) => {
    const g = stateRef.current?.graph;
    const e = g?.edges.find((x) => x.id === edgeId);
    const name = (id?: string) => g?.nodes.find((n) => n.id === id)?.data.title ?? "node";
    return e ? `${name(e.source)} → ${name(e.target)}` : "";
  };
  /** A connector dropped on empty canvas: offer what can be made from that output. */
  const onConnectEnd = (event: MouseEvent | TouchEvent, cs: any) => {
    if (cs?.isValid || cs?.toNode || !cs?.fromNode || cs?.fromHandle?.type !== "source") return;
    const out = OUT_TYPE[cs.fromNode.type as string];
    if (!out) return;
    const pt = "changedTouches" in event ? event.changedTouches[0] : event;
    const r = wrap.current!.getBoundingClientRect();
    const flow = rf.screenToFlowPosition({ x: pt.clientX, y: pt.clientY });
    setMenu({ x: Math.min(pt.clientX - r.left, r.width - 250), y: pt.clientY - r.top, flow: { x: Math.round(flow.x), y: Math.round(flow.y) }, from: { id: cs.fromNode.id, out } });
  };
  /** Add a node at the drop point, already connected to the output the connector came from. */
  const addConnected = async (type: string, targetHandle: string, data: any = {}) => {
    if (!menu?.from) return;
    const id = `${type}-${Math.random().toString(36).slice(2, 10)}`;
    const next = await patch([
      { op: "addNode", id, type, position: { x: menu.flow.x, y: menu.flow.y - 40 }, data: { title: KIND_LABEL[type], ...data } },
      { op: "connect", source: menu.from.id, target: id, targetHandle },
    ], `Add ${KIND_LABEL[type]}`);
    if (next) setNodes((ns) => ns.map((n) => ({ ...n, selected: n.id === id })));
  };
  const saveViewport = useRef<number>();
  const onMoveEnd = (_: unknown, viewport: { x: number; y: number; zoom: number }) => {
    window.clearTimeout(saveViewport.current);
    saveViewport.current = window.setTimeout(() => void rpc("canvas_viewport", { projectId, viewport }).catch(() => {}), 300);
  };
  const togglePanelOpen = () => { setPanel("props"); localStorage.setItem("canvasPanel", "props"); };
  const togglePanel = (p: "props" | "agent") => {
    const next = panel === p ? null : p;
    setPanel(next);
    localStorage.setItem("canvasPanel", next ?? "");
  };

  if (!state) return <div className="boot">…</div>;
  const empty = state.graph.nodes.length === 0;

  return (
    <LibraryCtx.Provider value={libraryCtx}>
    <ActionsCtx.Provider value={actions}>
    <StatsCtx.Provider value={statsCtx}>
    <div className="canvas">
      {videoPlan && <VideoPlanDialog key={videoPlan.id} plan={videoPlan} onClose={() => { setVideoPlan(null); void reloadVideos(); }} />}
      {bundle && <BundleDialog projectId={projectId} files={bundle} nodes={state.graph.nodes} patch={patch} onClose={(ids) => {
        setBundle(null);
        void load();
        if (ids?.length) setTimeout(() => rf.fitView({ nodes: ids.slice(0, 3).map((id) => ({ id })), padding: 0.7, duration: 300 }), 200);
      }} />}
      {storyboard && <StoryboardDialog projectId={projectId} nodes={state.graph.nodes} patch={patch} onClose={storyboardDone} />}
      <div className="canvas-bar">
        {/* Adding nodes, notes, groups, tidy-up and imports live in the right-click menu; Delete is the Delete key; Fit is at the bottom left. */}
        <div className="bar-group">
          <button className="btn small" disabled={!state.undo} onClick={() => history("canvas_undo")} title={state.undo ? `Undo: ${state.undo.label} (⌘Z)` : "Nothing to undo"}>Undo</button>
          <button className="btn small" disabled={!state.redo} onClick={() => history("canvas_redo")} title={state.redo ? `Redo: ${state.redo.label} (⇧⌘Z)` : "Nothing to redo"}>Redo</button>
          <div className="view-anchor">
            <button className={`btn small ${viewOpen || !sameView(view, VIEW_FULL) ? "on" : ""}`} onClick={() => setViewOpen((o) => !o)} aria-expanded={viewOpen} title="Make the canvas calmer: hide details or kinds of nodes, fade the connections">View</button>
            {viewOpen && (
              <div className="view-menu" role="dialog" aria-label="View">
                <span className="field-label">Presets</span>
                <div className="segmented">
                  {VIEW_PRESETS.map(([name, v]) => <button key={name} className={sameView(view, v) ? "on" : ""} onClick={() => setView(v)}>{name}</button>)}
                </div>
                <span className="field-label">Node details</span>
                <div className="segmented">
                  <button className={view.density === "full" ? "on" : ""} onClick={() => setView({ density: "full" })}>Everything</button>
                  <button className={view.density === "compact" ? "on" : ""} onClick={() => setView({ density: "compact" })}>Title and picture</button>
                </div>
                <span className="field-label">Show</span>
                {([["hideText", "Prompt and text nodes"], ["hideRefs", "Reference images"], ["hideAudio", "Audio clips"], ["hideNotes", "Notes and groups"], ["hideLinks", "Connections"]] as const).map(([k, label]) => (
                  <label key={k} className="view-check"><input type="checkbox" checked={!view[k]} onChange={(e) => setView({ [k]: !e.target.checked })} />{label}</label>
                ))}
                <span className="field-label">Connection lines · {Math.round(view.opacity * 100)}%</span>
                <input type="range" min={10} max={100} step={5} value={Math.round(view.opacity * 100)} onChange={(e) => setView({ opacity: Number(e.target.value) / 100 })} aria-label="Connection opacity" />
                <p className="muted small">Fades the lines between nodes; a selected line stays solid. Hidden nodes still exist and still run; this only changes what you see. To colour a node, right-click it.</p>
              </div>
            )}
          </div>
        </div>
        <div className="bar-group end">
          {(stats.project.videos > 0 || stats.project.images > 0) && (
            <span className="spend" title={`This project so far: ${stats.project.videos} videos (${fmtTok(stats.project.videoTokens)} tokens) and ${stats.project.images} images (${fmtTok(stats.project.imageTokens)} tokens). Simulation results are not counted.`}>
              Used here: <b>{fmtTok(stats.project.videoTokens + stats.project.imageTokens)}</b> tokens
              {(app.prices?.videoPerM || app.prices?.imageEach) ? <> · {fmtUsd((stats.project.videoTokens / 1e6) * (app.prices?.videoPerM ?? 0) + stats.project.images * (app.prices?.imageEach ?? 0))}</> : null}
            </span>
          )}
          {app.providerMode === "modelark" && app.gateway?.balanceUrl && (
            <button className="btn small" disabled={balanceBusy} onClick={() => { balanceWanted.current = true; void readBalance(); }} title="Reads the gateway balance (no generation). After the first read it refreshes when a generation finishes.">
              {balanceBusy ? "Budget…" : balance ? `Budget: ${balance}` : "Show budget"}
            </button>
          )}
          <button className="btn small" disabled={!selectedIds.length} onClick={() => run(selectedIds, true)} title="Regenerates on purpose and keeps both versions">New version</button>
          <button className="btn small accent" disabled={!selectedIds.length} onClick={() => run(selectedIds, false)}>Run selected</button>
          <span className="sep" />
          <button className={`btn small ${panel === "props" ? "on" : ""}`} onClick={() => togglePanel("props")}>Properties</button>
          <button className={`btn small ${panel === "agent" ? "on" : ""}`} onClick={() => togglePanel("agent")}>Assistant</button>
        </div>
      </div>
      <div className="canvas-body">
        {/* Editor coordinates are never mirrored for RTL. */}
        <div className={`flow density-${view.density}`} style={{ "--wire-alpha": view.opacity } as React.CSSProperties} dir="ltr" ref={wrap} onKeyDown={onKeyDown} tabIndex={0} data-drop onDragOver={(e) => e.preventDefault()} onDrop={onDropFiles}>
          <ReactFlow
            nodes={nodes} edges={edges} nodeTypes={nodeTypes} colorMode={resolvedTheme}
            onNodesChange={onNodesChange} onEdgesChange={onEdgesChange} onConnect={onConnect} isValidConnection={isValidConnection}
            onNodeDragStart={onNodeDragStart} onNodeDrag={onNodeDrag} onNodeDragStop={onNodeDragStop} onSelectionDragStop={(e, ns) => onNodeDragStop(e, null, ns)} onMoveEnd={onMoveEnd} defaultViewport={state.viewport}
            elevateNodesOnSelect={false} selectionOnDrag selectionMode={SelectionMode.Partial} panOnDrag={[1]} panOnScroll onSelectionContextMenu={(e, ns) => openMenu(e, ns[0]?.id)}
            onNodeDoubleClick={(_, n) => { setNodes((ns) => ns.map((x) => ({ ...x, selected: x.id === n.id }))); togglePanelOpen(); setTimeout(() => (document.querySelector('.side-panel .prompt-box, .side-panel textarea') as HTMLElement | null)?.focus(), 120); }} onConnectStart={() => setMenu(null)} onConnectEnd={onConnectEnd} onEdgeClick={(e, edge) => openEdgeMenu(e, edge.id)} onEdgeContextMenu={(e, edge) => openEdgeMenu(e, edge.id)} onPaneContextMenu={(e) => openMenu(e)} onNodeContextMenu={(e, n) => openMenu(e, n.id)} onPaneClick={() => setMenu(null)} onMoveStart={() => setMenu(null)}
            deleteKeyCode={null} minZoom={0.1} onlyRenderVisibleElements maxZoom={2} proOptions={{ hideAttribution: true }}
          >
            <Background gap={24} />
            <Controls position="bottom-left" showInteractive={false} />
          </ReactFlow>
          {menu && (
            <div className="ctx-menu" ref={menuRef} style={{ left: menu.x, top: menu.y }} role="menu" onContextMenu={(e) => e.preventDefault()}>
              {menu.edgeId ? (
                <>
                  <span className="ctx-title" dir="auto">{edgeEnds(menu.edgeId)}</span>
                  <button role="menuitem" className="danger" onClick={() => pick(() => void patch([{ op: "disconnect", edgeId: menu.edgeId }], "Disconnect"))}><Icon name="x" size={14} />Disconnect<small>Both nodes stay; Undo reconnects</small></button>
                </>
              ) : menu.from ? (
                menu.from.out === "text" ? (
                  <>
                    <span className="ctx-title">Use this text to…</span>
                    <button role="menuitem" onClick={() => pick(() => void addConnected("seedream", "prompt"))}><Icon name="image" size={14} />Generate an image<small>Seedream, with this as its prompt</small></button>
                    <button role="menuitem" onClick={() => pick(() => void addConnected("seedance", "prompt"))}><Icon name="film" size={14} />Generate a video<small>Seedance, with this as its prompt</small></button>
                    <button role="menuitem" onClick={() => pick(() => void addConnected("seedText", "prompt"))}><Icon name="chat" size={14} />Rewrite it<small>Seed improves or adapts the text</small></button>
                  </>
                ) : menu.from.out === "audio" ? (
                  <>
                    <span className="ctx-title">Use this audio to…</span>
                    <button role="menuitem" onClick={() => pick(() => void addConnected("seedance", "audio"))}><Icon name="film" size={14} />Generate a video that speaks it<small>The character lip-syncs to this clip</small></button>
                  </>
                ) : (
                  <>
                    <span className="ctx-title">Use this image to…</span>
                    <button role="menuitem" onClick={() => pick(() => void addConnected("seedance", "first"))}><Icon name="film" size={14} />Generate a video<small>Animate it: the video starts on this image</small></button>
                    <button role="menuitem" onClick={() => pick(() => void addConnected("seedance", "last"))}><Icon name="film" size={14} />Video that ends on it<small>Use it as the last frame</small></button>
                    <button role="menuitem" onClick={() => pick(() => void addConnected("seedance", "refs"))}><Icon name="film" size={14} />Video with it as a reference<small>Keeps a character, product or place</small></button>
                    <button role="menuitem" onClick={() => pick(() => void addConnected("seedream", "refs"))}><Icon name="image" size={14} />Regenerate or edit the image<small>Seedream, with this image as reference</small></button>
                    <button role="menuitem" onClick={() => pick(() => void addConnected("describe", "image"))}><Icon name="chat" size={14} />Describe it<small>Seed writes a prompt from this image</small></button>
                    <button role="menuitem" onClick={() => pick(() => void addConnected("preview", "image"))}><Icon name="book" size={14} />Show it large<small>A result node to compare versions</small></button>
                  </>
                )
              ) : menu.nodeId && selectedIds.length > 1 ? (
                <>
                  <span className="ctx-title">{selectedIds.length} nodes selected</span>
                  <button role="menuitem" onClick={() => pick(() => void groupSelected())}><Icon name="nodes" size={14} />Group them<small>A coloured frame around them</small></button>
                  <button role="menuitem" onClick={() => pick(() => void tidyUp())}><Icon name="nodes" size={14} />Tidy them up<small>Arrange them neatly</small></button>
                  <button role="menuitem" onClick={() => pick(() => run(selectedIds, false))}><Icon name="play" size={14} />Run selected</button>
                  <ColourRow onPick={(c) => pick(() => void colourNodes(selectedIds, c))} />
                  <button role="menuitem" className="danger" onClick={() => pick(removeSelected)}><Icon name="x" size={14} />Delete</button>
                </>
              ) : menu.nodeId ? (
                <>
                  <button role="menuitem" onClick={() => pick(() => run([menu.nodeId!], false))}><Icon name="play" size={14} />Run this node</button>
                  <button role="menuitem" onClick={() => pick(() => togglePanelOpen())}><Icon name="book" size={14} />Settings</button>
                  <ColourRow onPick={(c) => pick(() => void colourNodes([menu.nodeId!], c))} />
                  <button role="menuitem" className="danger" onClick={() => pick(() => void patch([{ op: "removeNode", nodeId: menu.nodeId }], "Delete"))}><Icon name="x" size={14} />Delete</button>
                </>
              ) : (
                <>
                  <span className="ctx-title">Add here</span>
                  <button role="menuitem" onClick={() => pick(() => void addNode("seedream", {}, menu.flow))}><Icon name="image" size={14} />Image generation<small>Write a prompt, get an image</small></button>
                  <button role="menuitem" onClick={() => pick(() => void addNode("seedance", {}, menu.flow))}><Icon name="film" size={14} />Video generation<small>Write a prompt, add a frame and a voice</small></button>
                  <button role="menuitem" onClick={() => pick(() => void addReference(menu.flow))}><Icon name="plus" size={14} />Image from this Mac<small>A frame or a reference picture</small></button>
                  <span className="ctx-sep" />
                  <button role="menuitem" className="ctx-more" aria-expanded={menuTab === "nodes"} onClick={() => setMenuTab(menuTab === "nodes" ? null : "nodes")}><Icon name="nodes" size={14} />More nodes<i>{menuTab === "nodes" ? "–" : "+"}</i></button>
                  {menuTab === "nodes" && <div className="ctx-sub">
                    <button role="menuitem" onClick={() => pick(() => void addNode("audio", {}, menu.flow))}><Icon name="clip" size={14} />Audio clip<small>A voice for a video to speak</small></button>
                    <button role="menuitem" onClick={() => pick(() => void addNode("prompt", { text: "" }, menu.flow))}><Icon name="chat" size={14} />Shared prompt<small>One text feeding several nodes</small></button>
                    <button role="menuitem" onClick={() => pick(() => void addNode("describe", {}, menu.flow))}><Icon name="chat" size={14} />Describe image<small>Seed writes a prompt from an image</small></button>
                    <button role="menuitem" onClick={() => pick(() => void addNode("seedText", {}, menu.flow))}><Icon name="chat" size={14} />Rewrite text<small>Seed improves or adapts a prompt</small></button>
                    <button role="menuitem" onClick={() => pick(() => void addNode("preview", {}, menu.flow))}><Icon name="book" size={14} />Result<small>Shows an image large</small></button>
                  </div>}
                  <button role="menuitem" className="ctx-more" aria-expanded={menuTab === "organise"} onClick={() => setMenuTab(menuTab === "organise" ? null : "organise")}><Icon name="book" size={14} />Organise<i>{menuTab === "organise" ? "–" : "+"}</i></button>
                  {menuTab === "organise" && <div className="ctx-sub">
                    <button role="menuitem" onClick={() => pick(() => void addNode("note", { text: "", color: "amber" }, menu.flow))}><Icon name="book" size={14} />Note<small>A coloured sticky note</small></button>
                    <button role="menuitem" onClick={() => pick(() => void groupSelected(menu.flow))}><Icon name="nodes" size={14} />Group<small>A coloured frame to gather nodes</small></button>
                    <button role="menuitem" onClick={() => pick(() => void tidyUp())}><Icon name="nodes" size={14} />Tidy up<small>Arrange the canvas neatly</small></button>
                  </div>}
                  <button role="menuitem" className="ctx-more" aria-expanded={menuTab === "import"} onClick={() => setMenuTab(menuTab === "import" ? null : "import")}><Icon name="clip" size={14} />Import<i>{menuTab === "import" ? "–" : "+"}</i></button>
                  {menuTab === "import" && <div className="ctx-sub">
                    <button role="menuitem" onClick={() => pick(() => setStoryboard(true))}><Icon name="nodes" size={14} />Storyboard from prompts…</button>
                    <button role="menuitem" onClick={() => pick(async () => { const f = await pickFiles(".zip,.pdf,.txt,.md,.csv,.json,image/png,image/jpeg,image/webp,audio/*"); if (f.length) void bringIn(f, [], "", menu.flow); })}><Icon name="clip" size={14} />Files, a PDF or a ZIP…<small>Builds every shot with its prompt and voice</small></button>
                    <button role="menuitem" onClick={() => pick(() => void importManyImages())}><Icon name="image" size={14} />Many images…</button>
                    <button role="menuitem" onClick={() => pick(() => void importPromptsFile())}><Icon name="chat" size={14} />Prompts for frames…</button>
                    <button role="menuitem" onClick={() => pick(() => void importPackage())}><Icon name="book" size={14} />Shot package…</button>
                  </div>}
                </>
              )}
            </div>
          )}
          {batch && (
            <div className="canvas-busy">
              <span className="spinner" /> {batch.text}
              <button className="btn small" onClick={() => void rpc("run_cancel", { runId: batch.id }).catch(() => {})} title="Stops the nodes that have not started; a request already sent is not refunded">Stop the rest</button>
            </div>
          )}
          {importing && <div className="canvas-busy"><span className="spinner" /> {importing}</div>}
          {empty && !importing && (
            <div className="canvas-empty">
              <h3>The canvas is empty</h3>
              <p>Right-click anywhere to add a prompt, an image or a video node.</p>
              <p className="muted small">Or drop files here: images, audio, a storyboard PDF or a ZIP of a whole project.</p>
              <button className="btn" onClick={importPackage}>Import a shot package</button>
            </div>
          )}
        </div>
        {panel && (
          <aside className="side-panel">
            {panel === "props"
              ? <Properties key={selectedNode?.id ?? "none"} node={selectedNode} state={state} projectId={projectId} patch={patch} run={run} notes={notes} count={selectedIds.length}
                  video={selectedNode ? nodeVideo[selectedNode.id] : undefined} planVideo={planVideo} />
              : <AgentPanel projectId={projectId} state={state} selection={selectedIds} patch={patch} reload={load} onUndo={() => history("canvas_undo")} />}
          </aside>
        )}
      </div>
    </div>
    </StatsCtx.Provider>
    </ActionsCtx.Provider>
    </LibraryCtx.Provider>
  );
}

// ───────────── properties ─────────────

function Field({ label, value, onCommit, multiline, placeholder }: { label: string; value: string; onCommit: (v: string) => void; multiline?: boolean; placeholder?: string }) {
  const [v, setV] = useState(value);
  useEffect(() => setV(value), [value]);
  const common = { value: v, placeholder, dir: "auto" as const, onChange: (e: any) => setV(e.target.value), onBlur: () => v !== value && onCommit(v) };
  return <label className="field"><span>{label}</span>{multiline ? <textarea rows={5} {...common} /> : <input {...common} />}</label>;
}

/** The node's own prompt: a large box that saves when you leave it. `pending` lets Generate wait for the save. */
function PromptBox({ value, linked, onCommit, pending, placeholder, elements }: { value: string; linked?: string; onCommit: (v: string) => Promise<unknown>; pending: React.MutableRefObject<Promise<unknown> | null>; placeholder: string; elements: Element[] }) {
  const [v, setV] = useState(value);
  const [ask, setAsk] = useState<{ from: number; to: number; word: string } | null>(null);
  const box = useRef<HTMLTextAreaElement>(null);
  useEffect(() => setV(value), [value]);
  if (linked) return <div className="prompt-linked"><b>Prompt</b> comes from the connected node “{linked}”. Disconnect it to write the prompt here.</div>;
  const look = (text: string, caret: number) => {
    const m = /@([\p{L}\p{N}_]*)$/u.exec(text.slice(0, caret));
    setAsk(m && elements.length ? { from: caret - m[0].length, to: caret, word: m[1].toLowerCase() } : null);
  };
  const options = ask ? elements.filter((e) => e.handle.toLowerCase().includes(ask.word)).slice(0, 6) : [];
  const choose = (e: Element) => {
    if (!ask) return;
    const next = `${v.slice(0, ask.from)}@${e.handle} ${v.slice(ask.to)}`;
    setV(next);
    setAsk(null);
    const caret = ask.from + e.handle.length + 2;
    setTimeout(() => { box.current?.focus(); box.current?.setSelectionRange(caret, caret); }, 0);
  };
  const used = elements.filter((e) => new RegExp(`@${e.handle}(?![\\p{L}\\p{N}_])`, "iu").test(v));
  return (
    <label className="field prompt-field"><span>Prompt</span>
      <textarea ref={box} className="prompt-box" dir="auto" rows={7} value={v} placeholder={placeholder}
        onChange={(e) => { setV(e.target.value); look(e.target.value, e.target.selectionStart ?? e.target.value.length); }}
        onKeyDown={(e) => {
          if (!options.length) return;
          if (e.key === "Enter" || e.key === "Tab") { e.preventDefault(); choose(options[0]); }
          if (e.key === "Escape") setAsk(null);
        }}
        onBlur={() => { setTimeout(() => setAsk(null), 150); if (v !== value) pending.current = onCommit(v); }} />
      {options.length > 0 && (
        <div className="at-menu" role="listbox" aria-label="Elements">
          {options.map((e, i) => (
            <button key={e.id} role="option" aria-selected={i === 0} className={i === 0 ? "first" : ""} onMouseDown={(ev) => { ev.preventDefault(); choose(e); }}>
              {e.assetId ? <Thumb id={e.assetId} className="thumb tiny" /> : <span className="thumb tiny placeholder" />}
              <b>@{e.handle}</b><small>{e.kind}</small>
            </button>
          ))}
        </div>
      )}
      {used.length > 0
        ? <div className="at-used">{used.map((e) => <span key={e.id} className="role-chip">{e.assetId && <Thumb id={e.assetId} className="thumb tiny" />}{e.name}</span>)}<small className="muted">sent as reference{used.length > 1 ? "s" : ""}, with {used.length > 1 ? "their" : "its"} locked description</small></div>
        : elements.length > 0 && <small className="muted at-hint">Type @ to call a character, a place or a prop from Elements.</small>}
    </label>
  );
}

function Properties({ node, state, projectId, patch, run, notes, count, video, planVideo }: {
  video?: any; planVideo: (args: Record<string, unknown>, cmd?: string) => void;
  node?: GNode; state: CanvasState; projectId: string; patch: (ops: any[], label: string) => Promise<CanvasState | null>;
  run: (ids: string[], force: boolean) => void; notes: string[]; count: number;
}) {
  const { app, toast, viewAsset } = useShell();
  const { library, refresh: refreshLibrary, add: addToLibrary, adding } = useContext(LibraryCtx);
  const { stats } = useContext(StatsCtx);
  const cost = useCost();
  const { elements } = useElements(projectId);
  // Expected size and time of this node's next video, from past videos of the same pass and resolution.
  const videoGuess = (() => {
    if (node?.type !== "seedance") return "";
    const s = node.data.settings ?? {};
    const draftRes = app.models.find((m) => m.key === (node.data.modelKey ?? "seedance-2.5"))?.options?.controls?.draftResolution ?? "480p";
    const g = stats.video[s.pass === "final" ? `final:${s.resolution}` : `draft:${draftRes}`];
    const secs = Number(s.duration);
    if (!g) return "";
    const tokens = g.tokensPerSec && secs > 0 ? cost.video(g.tokensPerSec * secs) : "";
    return ` · usually ${[tokens, g.secs ? `≈ ${clock(g.secs)} to make` : ""].filter(Boolean).join(", ")}`;
  })();
  const { assets } = useAssets(projectId);
  const pending = useRef<Promise<unknown> | null>(null);
  const afterSave = async (fn: () => void) => { await pending.current; pending.current = null; fn(); };
  if (!node) return <div className="panel-pad muted">{count > 1 ? `${count} nodes selected. “Run selected” computes only what they need.` : "Select a node to see its properties."}</div>;
  const set = (changes: any, label = "Edit properties") => patch([{ op: "setNodeData", nodeId: node.id, changes }], label);
  const result = state.results[node.id];
  const incoming = (handle: string) => state.graph.edges.filter((e) => e.target === node.id && e.targetHandle === handle).sort((a, b) => (a.data?.order ?? 0) - (b.data?.order ?? 0));
  const src = (id: string) => state.graph.nodes.find((n) => n.id === id);

  let body: React.ReactNode = null;
  if (node.type === "prompt") {
    body = <Field label="Prompt text" multiline value={node.data.text ?? ""} onCommit={(text) => set({ text })} placeholder="Describe the image…" />;
  } else if (node.type === "imageAsset") {
    body = (
      <>
        {node.data.assetId ? <Thumb id={node.data.assetId} className="panel-img" onClick={() => viewAsset(node.data.assetId)} /> : <p className="muted">No image chosen.</p>}
        <button className="btn" onClick={async () => { try { const a = await importImage(projectId); if (a) await set({ assetId: a.id }, "Replace image"); } catch (e) { toast((e as Error).message, "error"); } }}>Import image…</button>
        {library.available && (
          <div className="lib-box">
            <span className="field-label">Character library</span>
            <select value={node.data.library ?? ""} onChange={(e) => set({ library: e.target.value }, "Character library")} aria-label="Character library entry">
              <option value="">Auto — match by file name</option>
              <option value="off">Don’t use — send the file</option>
              {library.assets.map((a) => <option key={a.id} value={a.id} disabled={a.status.toLowerCase() !== "active"}>{a.name}{a.status.toLowerCase() !== "active" ? ` (${a.status})` : ""}</option>)}
            </select>
            {library.byNode[node.id]
              ? <div className="result ok">Videos send this image as the library entry “{library.byNode[node.id].name}” (by reference, not the file), so its face is accepted.</div>
              : node.data.library !== "off" && library.uploads
                ? <p className="muted small">Not in the library yet. It is added when a video first uses it, declared AI-generated (your setting), then sent by reference.</p>
                : <p className="muted small">{library.assets.length ? "Not matched: videos send the file itself, and Seedance refuses a realistic face that is not in the library. Pick the entry that shows this image, or turn on adding new images in Settings." : "The list has not been read yet."}</p>}
            {!library.byNode[node.id] && node.data.assetId && node.data.library !== "off" && (
              <>
                <button className="btn generate" disabled={!library.uploads || adding !== null} onClick={() => addToLibrary(node.data.assetId)}>
                  {adding === node.data.assetId ? <><span className="spinner" /> Adding… up to 90 s</> : "Add to character library"}
                </button>
                <span className="gen-note">{library.uploads
                  ? "1 upload to the gateway, declared AI-generated with no real person. Uses one place in the library."
                  : "Turn on “My reference images are AI-generated” in Settings → Provider first."}</span>
              </>
            )}
            <button className="btn small" onClick={() => refreshLibrary(true)}>{library.assets.length ? `Refresh list (${library.assets.length})` : "Read the library"}</button>
          </div>
        )}
        {assets.length > 0 && <>
          <span className="field-label">Or pick from the project gallery</span>
          <div className="pick-grid">{assets.map((a) => <Thumb key={a.id} id={a.id} className={`thumb small ${a.id === node.data.assetId ? "picked" : ""}`} onClick={() => set({ assetId: a.id }, "Pick image")} alt={a.title} />)}</div>
        </>}
      </>
    );
  } else if (node.type === "seedText") {
    const chat = app.models.filter((m) => m.kind === "chat");
    body = (
      <>
        <label className="field"><span>Model</span>
          <select value={node.data.modelKey ?? app.chatModel} onChange={(e) => set({ modelKey: e.target.value })}>{chat.map((m) => <option key={m.key} value={m.key}>{m.label}</option>)}</select>
        </label>
        <Field label="Rewrite instructions" multiline value={node.data.instructions ?? ""} onCommit={(instructions) => set({ instructions })} placeholder="e.g. Rewrite the prompt in a product-photography style" />
        <p className="muted small">Running it is a text request shown in the plan. It has no canvas-editing tools.</p>
        {result?.text && <div className="result-text">{result.text}</div>}
        <button className="btn accent" onClick={() => run([node.id], false)}>Run</button>
      </>
    );
  } else if (node.type === "note" || node.type === "group") {
    const color = node.data.color ?? (node.type === "note" ? "amber" : "gray");
    body = (
      <>
        {node.type === "note" && <Field label="Note" multiline value={node.data.text ?? ""} onCommit={(text) => set({ text })} placeholder="What is done, what is left, a reminder…" />}
        <span className="field-label">Colour</span>
        <div className="swatches" role="radiogroup" aria-label="Colour">
          {COLORS.map(([c, meaning]) => (
            <button key={c} role="radio" aria-checked={color === c} className={`swatch c-${c} ${color === c ? "on" : ""}`} onClick={() => set({ color: c }, "Colour")} title={meaning}><span />{meaning}</button>
          ))}
        </div>
        <p className="muted small">{node.type === "group"
          ? "Drag the group by its title to move everything inside it. Drag its corners to resize. Deleting the group keeps the nodes. The title is yours — e.g. “Done”, “Scene 2”."
          : "Notes are only for you: they are never sent with a request."}</p>
      </>
    );
  } else if (node.type === "audio") {
    body = <AudioClipEditor projectId={projectId} audioId={node.data.audioId} start={node.data.start ?? ""} end={node.data.end ?? ""} onChange={(changes) => set(changes, "Audio clip")} />;
  } else if (node.type === "describe") {
    const chat = app.models.filter((m) => m.kind === "chat");
    const img = incoming("image")[0];
    body = (
      <>
        <label className="field"><span>Model</span>
          <select value={node.data.modelKey ?? app.chatModel} onChange={(e) => set({ modelKey: e.target.value })}>{chat.map((m) => <option key={m.key} value={m.key}>{m.label}</option>)}</select>
        </label>
        <Field label="What to describe (optional)" multiline value={node.data.instructions ?? ""} onCommit={(instructions) => set({ instructions })} placeholder="Leave empty for a detailed image-generation prompt. Or ask: describe only the lighting, describe the motion for a video…" />
        <p className="muted small">{img ? "Running it sends the connected image and this instruction as one chat request, shown in the plan." : "Connect an image to the “image” port first."} The text it writes can feed an image or a video node.</p>
        {result?.text && <div className="result-text" dir="auto">{result.text}</div>}
        <button className="btn accent" disabled={!img} onClick={() => run([node.id], false)}>Describe</button>
      </>
    );
  } else if (node.type === "seedream") {
    const model = app.models.find((m) => m.key === node.data.modelKey);
    const fields = model?.options?.fields ?? {};
    const settings = node.data.settings ?? {};
    const refs = incoming("refs");
    const promptSrc = incoming("prompt")[0] && src(incoming("prompt")[0].source);
    const needsAlpha = String(model?.options?.transparentRule ?? "").includes("one_png_reference");
    const swap = (i: number) => {
      const [a, b] = [refs[i], refs[i - 1]];
      void patch([
        { op: "disconnect", edgeId: a.id }, { op: "disconnect", edgeId: b.id },
        { op: "connect", source: a.source, target: node.id, targetHandle: "refs", order: b.data?.order ?? i - 1 },
        { op: "connect", source: b.source, target: node.id, targetHandle: "refs", order: a.data?.order ?? i },
      ], "Reorder references");
    };
    body = (
      <>
        <PromptBox value={node.data.prompt ?? ""} linked={promptSrc ? promptSrc.data.title ?? "Prompt" : undefined} pending={pending} onCommit={(prompt) => set({ prompt }, "Edit prompt")} placeholder="Describe the image you want…" elements={elements} />
        {fields.aspect && (
          <div className="field"><span>Aspect ratio</span>
            <AspectPicker value={settings.aspect ?? "auto"} options={fields.aspect} onChange={(a) => set({ settings: { aspect: a === "auto" ? null : a } }, "Aspect ratio")} />
          </div>
        )}
        <button className="btn generate" disabled={result?.status === "sending"} onClick={() => afterSave(() => run([node.id], false))}><Icon name="play" size={15} />{result?.status === "sending" ? "Generating…" : "Generate image"}</button>
        <div className="gen-note">1 request · {model?.label} · starts right away{stats.image.tokens ? ` · usually ${cost.image(stats.image.tokens)}, ≈ ${Math.round(stats.image.secs ?? 25)}s` : ""}</div>
        <div className="row">
          <button className="btn" onClick={() => afterSave(() => run([node.id], true))} disabled={!result?.assetId}>New version</button>
        </div>
        <details className="more">
          <summary>More options</summary>
        <label className="field"><span>Model</span>
          <select value={node.data.modelKey} onChange={(e) => set({ modelKey: e.target.value }, "Switch model")}>
            {app.models.filter((m) => m.kind === "image").map((m) => <option key={m.key} value={m.key}>{m.label} — {STATUS_LABEL[m.status]}</option>)}
          </select>
        </label>
        <div className="muted small">ID sent: <bdi dir="ltr" className="mono">{model?.resolvedId}</bdi></div>
        {notes.length > 0 && <div className="notice"><b>Changed with the model:</b>{notes.map((n, i) => <div key={i}>{n}</div>)}</div>}
        {Object.entries(fields).filter(([k, opts]) => opts && k !== "aspect").map(([k, opts]) => (
          <label key={k} className="field inline"><span>{SETTING_LABEL[k] ?? k}</span>
            <select value={settings[k] ?? ""} onChange={(e) => set({ settings: { [k]: e.target.value || null } })}>
              {k === "optimize_prompt" && <option value="">Not set</option>}
              {(opts as string[]).map((o) => <option key={o} value={o}>{VALUE_LABEL[o] ?? o}</option>)}
            </select>
          </label>
        ))}
        {!fields.background && <p className="muted small">This model has no background option; the field is not sent.</p>}
        {fields.background && needsAlpha && <p className="muted small">Transparency here needs exactly one PNG reference with an alpha channel, and PNG output.</p>}
        {fields.background && !needsAlpha && <p className="muted small">A transparent background requires PNG output.</p>}

        </details>
        <details className="more">
          <summary>What will be sent</summary>
        <div className="send-preview">
          <div><b>Final prompt:</b> {promptSrc ? (promptSrc.type === "prompt" ? (promptSrc.data.text || <span className="warn">The prompt node is empty</span>) : "Output of the connected text node") : (node.data.prompt || <span className="warn">No prompt yet</span>)}</div>
          {node.data.camera && <div><b>Added to the prompt:</b> Camera and composition — frame the picture exactly like this, and take nothing else from it: {node.data.camera}</div>}
          <div><b>References ({refs.length}/{model?.options?.maxReferences ?? 10}):</b> {refs.length === 0 && "none — text to image"}</div>
          {refs.map((e, i) => {
            const s = src(e.source);
            const aid = s?.type === "imageAsset" ? s.data.assetId : state.results[e.source]?.assetId;
            return (
              <div key={e.id} className="ref-row">
                <span className="ref-index">{i + 1}</span>
                {aid ? <Thumb id={aid} className="thumb small" onClick={() => viewAsset(aid)} /> : <span className="thumb small placeholder" />}
                <span className="grow">{s?.data.title ?? e.source}</span>
                {i > 0 && <button className="btn small" onClick={() => swap(i)} aria-label="Move reference up">↑</button>}
              </div>
            );
          })}
          <div><b>Output:</b> one image · {Object.entries(settings).map(([k, v]) => `${SETTING_LABEL[k] ?? k} ${VALUE_LABEL[v as string] ?? v}`).join(" · ")}</div>
        </div>
        </details>
        <LookBlock projectId={projectId} node={node} result={result} set={set} patch={patch} />
        <ResultBlock result={result} projectId={projectId} nodeId={node.id} />
      </>
    );
  } else if (node.type === "seedance") {
    const model = app.models.find((m) => m.key === (node.data.modelKey ?? "seedance-2.5"));
    const controls = model?.options?.controls ?? {};
    const s = node.data.settings ?? {};
    const setS = (patchS: Record<string, string>) => set({ settings: patchS }, "Video options");
    const frames = [...incoming("first"), ...incoming("last")];
    const refs = incoming("refs");
    const promptSrc = incoming("prompt")[0] && src(incoming("prompt")[0].source);
    const finals: string[] = app.models.find((m) => m.key === video?.modelKey)?.options?.controls?.finalFromDraft ?? [];
    const durations = ["-1", ...Array.from({ length: 27 }, (_, i) => String(i + 4))];
    const thumbOf = (e: GEdge) => { const n = src(e.source); return n?.type === "imageAsset" ? n.data.assetId : state.results[e.source]?.assetId; };
    const busy = video && ["submitting", "queued", "running"].includes(video.status);
    body = (
      <>
        <PromptBox value={node.data.prompt ?? ""} linked={promptSrc ? promptSrc.data.title ?? "Prompt" : undefined} pending={pending} onCommit={(prompt) => set({ prompt }, "Edit prompt")} placeholder="Describe the motion and the camera: what moves, how the camera travels, what is heard…" elements={elements} />
        <label className="field inline"><span>Video model</span>
          <select value={node.data.modelKey ?? "seedance-2.5"} onChange={(e) => set({ modelKey: e.target.value }, "Switch video model")}>
            {app.models.filter((m) => m.kind === "video").map((m) => <option key={m.key} value={m.key}>{m.label}</option>)}
          </select>
        </label>
        {controls.draftMode && <div className="field"><span>Pass</span>
          <div className="segmented small" role="radiogroup" aria-label="Draft or final">
            <button role="radio" aria-checked={s.pass !== "final"} className={s.pass !== "final" ? "on" : ""} onClick={() => setS({ pass: "draft" })}>Draft · {controls.draftResolution}</button>
            <button role="radio" aria-checked={s.pass === "final"} className={s.pass === "final" ? "on" : ""} onClick={() => setS({ pass: "final" })}>Final</button>
          </div>
        </div>}
        {s.pass === "final" && (
          <label className="field inline"><span>Quality</span>
            <select value={s.resolution} onChange={(e) => setS({ resolution: e.target.value })}>{(controls.resolutions ?? []).map((r: string) => <option key={r} value={r}>{r}</option>)}</select>
          </label>
        )}
        <label className="field inline"><span>Length</span>
          <select value={s.duration} onChange={(e) => setS({ duration: e.target.value })}>{durations.map((d) => <option key={d} value={d}>{d === "-1" ? "Auto" : `${d}s`}</option>)}</select>
        </label>
        <div className="field"><span>Aspect ratio</span>
          {frames.length > 0
            ? <span className="muted small">Follows the first frame — with frames, Seedance 2.5 accepts only the adaptive ratio.</span>
            : <AspectPicker value={s.ratio === "adaptive" ? "auto" : s.ratio} options={(controls.ratios ?? []).map((r: string) => (r === "adaptive" ? "auto" : r))} onChange={(r) => setS({ ratio: r === "auto" ? "adaptive" : r })} />}
        </div>
        <button className="btn generate" disabled={busy} onClick={() => afterSave(() => planVideo({ nodeId: node.id }))}><Icon name="play" size={15} />{busy ? "Generating…" : s.pass === "final" ? "Generate final video" : "Generate draft video"}</button>
        <div className="gen-note">1 request · {model?.label} · {s.pass === "final" ? `Final ${s.resolution}` : `Draft ${controls.draftResolution}`} · {frames.length > 0 && !incoming("audio")[0] && s.voice !== "on" ? "follows the frame" : s.ratio === "adaptive" ? "auto ratio" : s.ratio} · {s.duration === "-1" ? "auto length" : `${s.duration}s`} · starts right away{videoGuess}</div>
        {incoming("audio")[0]
          ? <div className="prompt-linked"><b>Voice</b> comes from the audio node “{src(incoming("audio")[0].source)?.data.title ?? "Audio"}”. The character lip-syncs to its clip.</div>
          : (
            <details className="more" open={s.voice === "on"}>
              <summary>Voice and lip sync{s.voice === "on" ? " · on" : ""}</summary>
              <VoiceSection projectId={projectId} s={s} setS={setS} />
            </details>
          )}
        <details className="more">
          <summary>What will be sent</summary>
        <div className="muted small">{model?.label} · ID sent: <bdi dir="ltr" className="mono">{model?.resolvedId}</bdi></div>
        <div className="send-preview">
          <div><b>Prompt:</b> {promptSrc ? (promptSrc.type === "prompt" ? (promptSrc.data.text || <span className="muted">empty</span>) : "Output of the connected text node") : (node.data.prompt || <span className="muted">none yet</span>)}</div>
          {[["first", "first_frame"], ["last", "last_frame"], ["refs", "reference_image"]].flatMap(([h, role]) => incoming(h).map((e) => (
            <div key={e.id} className="ref-row">
              {thumbOf(e) ? <Thumb id={thumbOf(e)} className="thumb small" onClick={() => viewAsset(thumbOf(e))} /> : <span className="thumb small placeholder" />}
              <span className="grow">{ROLE_LABEL[role]} · {src(e.source)?.data.title ?? e.source}</span>
            </div>
          )))}
          {frames.length > 0 && refs.length > 0 && <div className="warn">Frames and references are separate modes in Seedance. Disconnect one kind before running.</div>}
          <div><b>Output:</b> one video · {s.pass === "final" ? `Final ${s.resolution}` : `Draft ${controls.draftResolution}`}</div>
        </div>
        </details>
        {video && (
          <div className="result-block">
            {video._failed && (
              <div className="kept-note" dir="auto">
                <b>{video._failed.kind === "draft" ? "The new draft" : `The ${video._failed.params?.resolution ?? ""} final`} {video._failed.status === "uncertain" ? "has an unknown outcome and was not sent again" : "failed"}.</b> Your {video.kind === "draft" ? "draft" : "earlier video"} below is kept and can still be saved{video.kind === "draft" ? " or upscaled again" : ""}.
                <div className="warn">{video._failed.error}</div>
              </div>
            )}
            <span className="field-label">{video._failed ? "Kept" : "Latest"}: {video.kind === "draft" ? "Draft" : video.draftOf ? "Final from draft" : "Final"} {video.params?.resolution} — <span className={`run-state ${video.status}`}>{busy && <span className="spinner" />} {VIDEO_STATUS[video.status] ?? video.status}</span> <ProviderBadge provider={video.provider} /></span>
            {video.status === "succeeded" && video.hasFile && <video className="video-player" src={assetUrl(video.id)} controls playsInline loop preload="metadata" />}
            {video.error && <div className="warn" dir="auto">{video.error}</div>}
            {video.status === "succeeded" && (
              <div className="video-actions">
                {video.kind === "draft" && <span className="upscale"><span className="muted small">Upscale to</span>
                  {finals.map((r) => <button key={r} className="btn small primary" onClick={() => planVideo({ draftOf: video.id, resolution: r }, "video_plan")}>{r}</button>)}</span>}
                <button className="btn small" onClick={async () => { try { const x = await rpc("video_export", { videoId: video.id }); toast(`Saved to ${x.path}`); } catch (e) { toast((e as Error).message, "error"); } }}>Save</button>
              </div>
            )}
          </div>
        )}
      </>
    );
  } else if (node.type === "preview") {
    const upstream = incoming("image")[0]?.source;
    const r = upstream ? state.results[upstream] : undefined;
    const versions: string[] = r?.versions ?? [];
    const prev = versions.length > 1 ? versions[versions.length - 2] : null;
    body = r?.assetId ? (
      <>
        <div className={prev ? "compare" : ""}>
          <figure><Thumb id={r.assetId} className="panel-img" onClick={() => viewAsset(r.assetId)} /><figcaption>{prev ? "Current" : ""} <ProviderBadge provider={r.provider} /></figcaption></figure>
          {prev && <figure><Thumb id={prev} className="panel-img" onClick={() => viewAsset(prev)} /><figcaption>Previous version</figcaption></figure>}
        </div>
        <button className="btn" onClick={async () => { try { const x = await rpc("asset_export", { assetId: r.assetId }); toast(`Saved a copy to ${x.path}`); } catch (e) { toast((e as Error).message, "error"); } }}>Save a copy to Finder</button>
        <button className="btn" onClick={() => patch([{ op: "addNode", type: "imageAsset", position: { x: node.position.x + 320, y: node.position.y }, data: { title: `Ref: ${node.data.title ?? "Result"}`, assetId: r.assetId } }], "Use result as reference")}>Use this image as a reference</button>
      </>
    ) : <p className="muted">{upstream ? "No result yet. Run the connected node." : "Connect a generation node to the “image” port."}</p>;
  }

  return (
    <div className="panel-pad">
      <div className="panel-kind">{KIND_LABEL[node.type]} <bdi dir="ltr" className="mono muted">{node.id}</bdi></div>
      <Field label="Title" value={node.data.title ?? ""} onCommit={(title) => set({ title })} />
      {body}
    </div>
  );
}

/** Pick one of the project's images. Choosing sends nothing by itself. */
function ImagePick({ projectId, title, hint, exclude, children, onPick, onClose }: { projectId: string; title: string; hint: string; exclude?: string; children?: React.ReactNode; onPick: (assetId: string) => void; onClose: () => void }) {
  const { toast } = useShell();
  const { assets, reload } = useAssets(projectId);
  const list = assets.filter((a) => a.id !== exclude && String(a.mime ?? "image/").startsWith("image/"));
  // In front of the whole window, not inside the side panel.
  return createPortal(
    <div className="overlay" role="dialog" aria-label={title}>
      <div className="dialog image-pick">
        <div className="dialog-head">
          <h2>{title}</h2>
          <button className="round" onClick={onClose} aria-label="Close"><Icon name="x" /></button>
        </div>
        <p className="muted">{hint}</p>
        {children}
        <div className="pick-grid tall">
          {list.map((a) => <Thumb key={a.id} id={a.id} className="thumb pickable" onClick={() => onPick(a.id)} alt={a.title} />)}
          {list.length === 0 && <span className="muted">This project has no other images yet.</span>}
        </div>
        <div className="row end">
          <button className="btn" onClick={async () => { try { const a = await importImage(projectId); if (a) { await reload(); onPick(a.id); } } catch (e) { toast((e as Error).message, "error"); } }}>Choose from this Mac…</button>
          <button className="btn" onClick={onClose}>Cancel</button>
        </div>
      </div>
    </div>,
    document.body,
  );
}

const REGRADE_PROMPT = "Reference image 1 is the picture to keep: reproduce it exactly, with the same composition, camera, people, faces, clothing, objects, positions and details. Reference image 2 is only a colour palette strip, not a scene: do not draw the strip or its blocks anywhere. Re-grade reference image 1 so that its colours, contrast and light temperature match that palette. Change nothing else.";

/** Copy the camera or the colour of another image onto this image node, without copying what is in it. */
function LookBlock({ projectId, node, result, set, patch }: { projectId: string; node: GNode; result: any; set: (changes: any, label?: string) => Promise<any>; patch: (ops: any[], label: string) => Promise<CanvasState | null> }) {
  const { toast, app } = useShell();
  const [pick, setPick] = useState<"camera" | "match" | "regrade" | null>(null);
  const [busy, setBusy] = useState<string | null>(null);
  const [strength, setStrength] = useState(1);
  const chat = app.models.find((m) => m.key === app.chatModel)?.label ?? "the chat model";
  const going = useRef(false);
  const act = async (what: string, fn: () => Promise<void>) => {
    if (going.current) return; // a second click never sends a second request
    going.current = true;
    setBusy(what);
    try { await fn(); } catch (e) { toast((e as Error).message, "error"); } finally { going.current = false; setBusy(null); }
  };
  const chosen = (assetId: string) => {
    const mode = pick;
    setPick(null);
    if (mode === "camera") void act("camera", async () => {
      const r = await rpc("camera_describe", { projectId, assetId });
      await set({ camera: r.text }, "Copy camera");
      toast("Camera copied. It is added to this node’s prompt; press Generate to use it.");
    });
    if (mode === "match") void act("match", async () => {
      await rpc("colour_match", { projectId, nodeId: node.id, fromAssetId: assetId, strength });
      toast("Colour matched on this Mac — nothing was sent. It is a new version; the earlier one is still under Versions.");
    });
    if (mode === "regrade") void act("regrade", async () => {
      const pal = await rpc("palette_asset", { projectId, fromAssetId: assetId });
      const tag = Math.random().toString(36).slice(2, 10);
      const [palId, newId] = [`imageAsset-${tag}`, `seedream-${tag}`];
      const { x, y } = node.position;
      await patch([
        { op: "addNode", id: palId, type: "imageAsset", position: { x: x + 280, y: y + 300 }, data: { title: pal.title, assetId: pal.assetId } },
        { op: "addNode", id: newId, type: "seedream", position: { x: x + 560, y }, data: { title: `${node.data.title ?? "Image"} · regraded`.slice(0, 120), modelKey: node.data.modelKey, settings: node.data.settings ?? {}, prompt: REGRADE_PROMPT } },
        { op: "connect", source: node.id, target: newId, targetHandle: "refs", order: 0 },
        { op: "connect", source: palId, target: newId, targetHandle: "refs", order: 1 },
      ], "Regrade with a palette");
      toast("Added a node set up to regenerate this image with that colour. Nothing was sent — select it and press Generate.");
    });
  };
  return (
    <details className="more" open={!!node.data.camera}>
      <summary>Copy camera or colour from another image</summary>
      <span className="field-label">Camera angle and composition</span>
      {node.data.camera
        ? <>
            <Field label="Camera copied (you can edit it)" multiline value={node.data.camera} onCommit={(camera) => set({ camera: camera.trim() || null }, "Edit camera")} />
            <div className="row">
              <button className="btn small" disabled={!!busy} onClick={() => setPick("camera")}>{busy === "camera" ? "Reading…" : "Copy from another image…"}</button>
              <button className="btn small" onClick={() => set({ camera: null }, "Remove camera")}>Remove</button>
            </div>
          </>
        : <button className="btn" disabled={!!busy} onClick={() => setPick("camera")}>{busy === "camera" ? <><span className="spinner" /> Reading the camera…</> : "Copy camera from an image…"}</button>}
      <span className="gen-note">1 text request · {chat} reads the image you pick and writes only its camera and framing. The image itself is not sent with the generation, so its people, objects and colours cannot come along.</span>
      <span className="field-label">Colour</span>
      <div className="row">
        <button className="btn" disabled={!!busy || !result?.assetId} onClick={() => setPick("match")}>{busy === "match" ? <><span className="spinner" /> Matching…</> : "Match colour to an image…"}</button>
        <button className="btn" disabled={!!busy || !result?.assetId} onClick={() => setPick("regrade")}>Regenerate with its colour…</button>
      </div>
      <span className="gen-note">{result?.assetId
        ? "Match: done on this Mac, no request, the picture itself does not change; it becomes a new version. Regenerate: adds a Seedream node with this image and a palette strip; 1 image request when you press Generate there."
        : "Generate this image first; then its colour can be matched to an approved image."}</span>
      {pick && (
        <ImagePick projectId={projectId} exclude={pick === "camera" ? undefined : result?.assetId} onPick={chosen} onClose={() => setPick(null)}
          title={pick === "camera" ? "Copy the camera from…" : pick === "match" ? "Match the colour to…" : "Regenerate with the colour of…"}
          hint={pick === "camera" ? `Choosing an image sends it once to ${chat} (1 text request) to describe its camera.` : pick === "match" ? "Choose the approved image. Nothing is sent: the colour is matched on this Mac." : "Choose the approved image. Nothing is sent now: a palette strip and a new node are added."}>
          {pick === "match" && (
            <div className="row">
              <span className="field-label">How much</span>
              <div className="segmented small" role="radiogroup" aria-label="Strength">
                {[[1, "Full"], [0.6, "Medium"], [0.3, "Light"]].map(([v, l]) => <button key={l} role="radio" aria-checked={strength === v} className={strength === v ? "on" : ""} onClick={() => setStrength(v as number)}>{l}</button>)}
              </div>
            </div>
          )}
        </ImagePick>
      )}
    </details>
  );
}

function ResultBlock({ result, projectId, nodeId }: { result: any; projectId: string; nodeId: string }) {
  const { viewAsset, toast } = useShell();
  const { library, add: addToLibrary, adding } = useContext(LibraryCtx);
  const [sel, setSel] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  if (!result) return null;
  const versions: string[] = result.versions ?? [];
  const shown = sel && versions.includes(sel) ? sel : result.assetId;
  const n = versions.indexOf(shown) + 1;
  const act = async (fn: () => Promise<void>) => { setBusy(true); try { await fn(); } catch (e) { toast((e as Error).message, "error"); } finally { setBusy(false); } };
  return (
    <div className="result-block">
      <span className="field-label">Last run: <span className={`run-state ${result.status}`}>{RUN_STATUS[result.status] ?? result.status}</span> <ProviderBadge provider={result.provider} /></span>
      {result.error && result.status !== "succeeded" && <div className="warn" dir="auto">{result.error}</div>}
      {shown && (
        <div className="versions">
          <Thumb id={shown} className="panel-img" onClick={() => viewAsset(shown)} alt={`Version ${n}`} />
          <span className="muted small">Version {n} of {versions.length}{shown === result.assetId ? " · in use" : " · not in use"}{versions.length > 1 ? " — click a version below to look at it" : ""}</span>
          {versions.length > 1 && (
            <div className="pick-grid">{versions.map((v, i) => (
              <span key={v} className={`version ${v === result.assetId ? "current" : ""}`}>
                <Thumb id={v} className={`thumb small ${v === shown ? "picked" : ""}`} onClick={() => setSel(v)} alt={`Version ${i + 1}`} />
                <small>{i + 1}{v === result.assetId ? " ✓" : ""}</small>
              </span>
            ))}</div>
          )}
          <div className="row">
            {shown !== result.assetId && <button className="btn primary" disabled={busy} onClick={() => act(async () => { await rpc("node_use_version", { projectId, nodeId, assetId: shown }); setSel(null); toast(`Version ${n} is now the one in use. Nothing was sent.`); })}>Use this version</button>}
            <button className="btn" disabled={busy} onClick={() => act(async () => { const x = await rpc("asset_export", { assetId: shown }); toast(`Saved to ${x.path}`); })}>Download</button>
          </div>
          {library.available && (
            <div className="lib-box">
              <span className="field-label">Character library</span>
              {library.uploaded?.[shown]
                ? <div className="result ok">This version is in the gateway’s character library. Videos send it by reference, so its face is accepted.</div>
                : <>
                    <button className="btn generate" disabled={!library.uploads || adding !== null} onClick={() => addToLibrary(shown)}>
                      {adding === shown ? <><span className="spinner" /> Adding… up to 90 s</> : "Add to character library"}
                    </button>
                    <span className="gen-note">{library.uploads
                      ? "1 upload to the gateway, declared AI-generated with no real person. Uses one place in the library."
                      : "Turn on “My reference images are AI-generated” in Settings → Provider first."}</span>
                  </>}
            </div>
          )}
        </div>
      )}
    </div>
  );
}

// ───────────── canvas assistant ─────────────

function AgentPanel({ projectId, state, selection, patch, reload, onUndo }: {
  projectId: string; state: CanvasState; selection: string[]; patch: (ops: any[], label: string) => Promise<CanvasState | null>;
  reload: () => Promise<void>; onUndo: () => void;
}) {
  const { toast, reviewPlan, app } = useShell();
  const [threadId, setThreadId] = useState<string | null>(null);
  const [messages, setMessages] = useState<any[]>([]);
  const [draft, setDraft] = useState("");
  const [busy, setBusy] = useState(false);
  const [dismissed, setDismissed] = useState<Set<string>>(new Set());
  const endRef = useRef<HTMLDivElement>(null);

  const loadThread = useCallback(async () => {
    const t = await rpc("agent_thread", { projectId });
    setThreadId(t.id);
    setMessages(t.messages);
  }, [projectId]);
  useEffect(() => { void loadThread(); }, [loadThread]);
  useEffect(() => { endRef.current?.scrollIntoView({ block: "end" }); }, [messages, busy]);

  const send = async () => {
    const text = draft.trim();
    if (!text || !threadId || busy) return;
    setBusy(true);
    setDraft("");
    setMessages((m) => [...m, { id: "pending", role: "user", content: text, meta: {} }]);
    try {
      await rpc("agent_send", { projectId, threadId, text, selection });
    } catch (e) {
      toast((e as Error).message, "error");
    } finally {
      setBusy(false);
      await Promise.all([loadThread(), reload()]);
    }
  };
  const titles = selection.map((id) => state.graph.nodes.find((n) => n.id === id)?.data.title ?? id);
  const lastAgent = [...messages].reverse().find((m) => m.role === "assistant" && m.meta?.changed);

  return (
    <div className="agent">
      <div className="agent-head">
        <b>Canvas assistant</b>
        <span className="muted small">{app.models.find((m) => m.key === app.chatModel)?.label}{app.providerMode === "mock" && " · simulation"}</span>
      </div>
      <div className="agent-messages">
        {messages.length === 0 && <p className="muted small">Select an image or a node, then ask for example: “Make two versions of this ad, one in morning light and one in evening light.” The assistant really edits the graph; nothing is generated until you press Run.</p>}
        {messages.map((m) => (
          <div key={m.id} className={`amsg ${m.role} ${m.status ?? ""}`}>
            <div className="msg-text" dir="auto">{m.content}</div>
            {m.meta?.actions?.length > 0 && (
              <div className="action-log">
                {m.meta.actions.map((a: string, i: number) => <div key={i}>✓ {a}</div>)}
                {(m.meta.notes ?? []).map((n: string, i: number) => <div key={`n${i}`} className="muted" dir="auto">{n}</div>)}
                {m === lastAgent && state.undo?.actor === "agent" && <button className="btn small" onClick={onUndo}>Undo everything the assistant did</button>}
              </div>
            )}
            {m.meta?.plan && (
              <div className="action-log">
                <div>Proposed plan: {m.meta.plan.newImages} image request(s), {m.meta.plan.reused} saved.</div>
                <button className="btn small primary" onClick={async () => {
                  try { await reviewPlan(await rpc("plan_run", { projectId, nodeIds: m.meta.plan.targets, forceNew: false })); await reload(); } catch (e) { toast((e as Error).message, "error"); }
                }}>Review the plan and run</button>
              </div>
            )}
            {m.meta?.pendingPatch && !dismissed.has(m.id) && (
              <div className="action-log warn-box">
                <div>The assistant proposes deleting: {m.meta.pendingPatch.titles.join(", ")} ({m.meta.pendingPatch.edges} links go with them). Saved images are not deleted.</div>
                <div className="row">
                  <button className="btn small danger" onClick={async () => { if (await patch(m.meta.pendingPatch.operations, "Deletion proposed by the assistant")) setDismissed((d) => new Set(d).add(m.id)); }}>Apply the deletion</button>
                  <button className="btn small" onClick={() => setDismissed((d) => new Set(d).add(m.id))}>Ignore</button>
                </div>
              </div>
            )}
            {m.role === "assistant" && m.modelId && <div className="muted small"><bdi dir="ltr" className="mono">{m.modelId}</bdi> <ProviderBadge provider={m.provider} /></div>}
          </div>
        ))}
        {busy && <div className="amsg assistant"><span className="spinner" aria-hidden /> Working on the canvas…</div>}
        <div ref={endRef} />
      </div>
      <div className="agent-compose">
        <div className="muted small">{titles.length ? <>Selection: {titles.join(", ")}</> : "No selection — the assistant sees only the canvas summary"}</div>
        <textarea rows={3} value={draft} onChange={(e) => setDraft(e.target.value)} placeholder="Ask for a change on the canvas…"
          onKeyDown={(e) => { if (e.key === "Enter" && !e.shiftKey && !e.nativeEvent.isComposing) { e.preventDefault(); void send(); } }} />
        <button className="btn primary" onClick={send} disabled={busy || !draft.trim()}>Send to assistant</button>
      </div>
    </div>
  );
}

/** The gateway's budget in a few words, whatever its exact field names. */
function balanceText(b: any): string {
  const flat: [string, number][] = [];
  const walk = (o: any) => { for (const [k, v] of Object.entries(o ?? {})) { if (typeof v === "number") flat.push([k.toLowerCase(), v]); else if (v && typeof v === "object" && !Array.isArray(v)) walk(v); } };
  walk(b);
  const pick = (...names: string[]) => flat.find(([k]) => names.some((n) => k.includes(n)));
  const left = pick("left", "remaining", "available");
  const spent = pick("spent", "used_usd", "used");
  const num = (n: number) => (Number.isInteger(n) ? String(n) : n.toFixed(2));
  if (left) return `${num(left[1])} left${spent ? ` · ${num(spent[1])} spent` : ""}`;
  return flat.slice(0, 2).map(([k, v]) => `${k.replace(/_/g, " ")} ${num(v)}`).join(" · ") || "read";
}
