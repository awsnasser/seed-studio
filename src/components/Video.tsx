import { useCallback, useEffect, useRef, useState } from "react";
import { assetUrl, listen, rpc } from "../api";
import { Icon, ProviderBadge, Thumb, useShell } from "../shared";
import { AspectPicker } from "./ImageOptions";

export type VideoInput = { assetId: string; role: "first_frame" | "last_frame" | "reference_image" };
export type VideoOpts = { modelKey: string; draft: boolean; resolution: string; ratio: string; duration: number };
/** Keep saved composer settings valid when a model is changed or a catalog is updated. */
export function videoOptionsFor(model: any, opts: VideoOpts): VideoOpts {
  const controls = model?.options?.controls ?? {};
  return {
    ...opts,
    draft: controls.draftMode === true && opts.draft,
    resolution: (controls.resolutions ?? []).includes(opts.resolution) ? opts.resolution : "720p",
  };
}

export const ROLE_LABEL: Record<string, string> = { first_frame: "First frame", last_frame: "Last frame", reference_image: "Reference" };
const ACTIVE = ["queued", "running"];
const STATUS: Record<string, string> = {
  submitting: "Sending…", queued: "Queued", running: "Generating…", succeeded: "Ready", failed: "Failed", uncertain: "Outcome unknown",
};

/** Project videos, kept fresh: running tasks are polled (a GET) until their file is saved. */
export function useVideos(projectId: string) {
  const [videos, setVideos] = useState<Record<string, any>>({});
  const [notes, setNotes] = useState<Record<string, string>>({});
  const ref = useRef(videos);
  ref.current = videos;
  const reload = useCallback(async () => {
    const list: any[] = await rpc("videos_list", { projectId });
    setVideos(Object.fromEntries(list.map((v) => [v.id, v])));
  }, [projectId]);
  useEffect(() => {
    void reload();
    const off = listen("video-update", (p) => p.projectId === projectId && void reload());
    const timer = window.setInterval(async () => {
      for (const v of Object.values(ref.current).filter((x: any) => ACTIVE.includes(x.status))) {
        try {
          const next = await rpc("video_poll", { videoId: (v as any).id });
          setVideos((m) => ({ ...m, [next.id]: next }));
          setNotes((n) => ({ ...n, [next.id]: next.pollNote ?? "" }));
        } catch { /* provider not configured right now; try again on the next tick */ }
      }
    }, 4000);
    return () => { off(); window.clearInterval(timer); };
  }, [projectId, reload]);
  return { videos, notes, reload };
}

export function VideoOptionsBar({ model, opts, onChange, inputs, setInputs }: {
  model: any; opts: VideoOpts; onChange: (patch: Partial<VideoOpts>) => void; inputs: VideoInput[]; setInputs: (v: VideoInput[]) => void;
}) {
  const c = model?.options?.controls ?? {};
  const hasFrames = inputs.some((i) => i.role !== "reference_image");
  const finals: string[] = c.finalFromDraft ?? [];
  const durations = [-1, 4, 5, 6, 8, 10, 12, 15, 20, 30].filter((d) => d === -1 || (d >= (c.duration?.min ?? 4) && d <= (c.duration?.max ?? 30)));
  return (
    <div className="img-options">
      {inputs.length > 0 && (
        <div className="opt-group wrap">
          <span className="opt-label">Images</span>
          {inputs.map((i, ix) => (
            <span key={`${i.assetId}-${ix}`} className="role-chip">
              <Thumb id={i.assetId} className="thumb tiny" />
              <select value={i.role} aria-label="Image role" onChange={(e) => setInputs(inputs.map((x, k) => (k === ix ? { ...x, role: e.target.value as VideoInput["role"] } : x)))}>
                {Object.entries(ROLE_LABEL).map(([v, l]) => <option key={v} value={v}>{l}</option>)}
              </select>
              <button onClick={() => setInputs(inputs.filter((_, k) => k !== ix))} aria-label="Remove image"><Icon name="x" size={11} /></button>
            </span>
          ))}
        </div>
      )}
      <div className="opt-row">
        {c.draftMode && <div className="opt-group">
          <span className="opt-label">Pass</span>
          <div className="segmented small" role="radiogroup" aria-label="Draft or final">
            <button role="radio" aria-checked={opts.draft} className={opts.draft ? "on" : ""} onClick={() => onChange({ draft: true })} title="A quick 480p preview to check motion and framing. Upscale it afterwards.">Draft · {c.draftResolution}</button>
            <button role="radio" aria-checked={!opts.draft} className={!opts.draft ? "on" : ""} onClick={() => onChange({ draft: false })}>Final</button>
          </div>
        </div>}
        {!opts.draft && (
          <div className="opt-group">
            <span className="opt-label">Quality</span>
            <div className="segmented small" role="radiogroup" aria-label="Resolution">
              {(c.resolutions ?? []).map((r: string) => <button key={r} role="radio" aria-checked={opts.resolution === r} className={opts.resolution === r ? "on" : ""} onClick={() => onChange({ resolution: r })}>{r}</button>)}
            </div>
          </div>
        )}
        {opts.draft && <span className="muted small">Then upscale the approved draft to {finals.join(" / ")}.</span>}
        <div className="opt-group">
          <span className="opt-label">Length</span>
          <select className="mini-select" value={opts.duration} onChange={(e) => onChange({ duration: Number(e.target.value) })} aria-label="Duration">
            {durations.map((d) => <option key={d} value={d}>{d === -1 ? "Auto" : `${d}s`}</option>)}
          </select>
        </div>
      </div>
      <div className="opt-group">
        <span className="opt-label">Ratio</span>
        {hasFrames
          ? <span className="muted small">Follows the first frame — with frames, {model?.label} follows the input image’s aspect ratio.</span>
          : <AspectPicker value={opts.ratio === "adaptive" ? "auto" : opts.ratio} options={(c.ratios ?? []).map((r: string) => (r === "adaptive" ? "auto" : r))} onChange={(r) => onChange({ ratio: r === "auto" ? "adaptive" : r })} />}
      </div>
    </div>
  );
}

function describe(v: any): string {
  const p = v.params ?? {};
  const parts = [v.kind === "draft" ? "Draft" : v.draftOf ? "Final from draft" : "Final", p.resolution];
  if (p.ratio) parts.push(p.ratio === "adaptive" ? "auto ratio" : p.ratio);
  if (p.duration !== undefined) parts.push(p.duration === -1 ? "auto length" : `${p.duration}s`);
  return parts.filter(Boolean).join(" · ");
}

/** Shows exactly what one video task will send. One click = one task. */
export function VideoPlanDialog({ plan, onClose }: { plan: any; onClose: (started: boolean) => void }) {
  const { app, toast } = useShell();
  const [busy, setBusy] = useState(false);
  const started = useRef(false);
  const run = async () => {
    if (started.current) return;
    started.current = true;
    setBusy(true);
    try {
      const v = await rpc("video_start", { videoId: plan.id });
      if (v.status === "failed" || v.status === "uncertain") toast(v.error ?? "The request failed.", "error");
      onClose(true);
    } catch (e) {
      toast((e as Error).message, "error");
      onClose(true);
    }
  };
  return (
    <div className="overlay" role="dialog" aria-label="Video plan">
      <div className="dialog plan">
        <div className="dialog-head"><h2>Video plan</h2>{plan.provider === "mock" && <ProviderBadge provider="mock" />}</div>
        <p className="plan-summary">1 video request × {plan.modelLabel}</p>
        <p className="muted small">
          {plan.provider === "mock"
            ? "Provider: local simulation — no network, no cost. The result is a short test pattern labelled “Simulation”."
            : <>Provider: BytePlus ModelArk · {plan.region}. This is a request count, not a bill; video is priced per output — see <a href="https://docs.byteplus.com/en/docs/modelark/model-pricing" target="_blank" rel="noreferrer">ModelArk pricing</a>.</>}
          {plan.provider !== "mock" && !app.hasKey[plan.region] && <b className="warn"> No API key is saved for this region.</b>}
        </p>
        {(plan.notes ?? []).map((n: string, i: number) => <div key={i} className="notice">{n}</div>)}
        <ul className="plan-items">
          <li className="plan-item new">
            <div className="plan-item-head">
              <b>{describe(plan)}</b><span className="badge new">New request</span>
              <span className="muted">{plan.modelLabel} · <span className="mono">{plan.modelId}</span></span>
            </div>
            {plan.prompt && <div className="plan-prompt" dir="auto">{plan.prompt}</div>}
            <div className="plan-meta">
              {plan.draftOf && <span className="chip strong">Built on the approved draft</span>}
              {plan.params?._audio && <span className="chip strong">Voice clip {(plan.params._audio.startMs / 1000).toFixed(1)}–{(plan.params._audio.endMs / 1000).toFixed(1)}s · sent for lip sync</span>}
              {(plan.inputs ?? []).map((i: any, ix: number) => <span key={ix} className="ref-chip"><Thumb id={i.assetId} className="thumb tiny" /> {ROLE_LABEL[i.role]}{i.library ? (i.library.pending ? " · will be added to the library" : ` · library “${i.library.name}”`) : ""}</span>)}
            </div>
          </li>
        </ul>
        <div className="row end">
          {busy ? <><span className="spinner" aria-hidden /> <span>Creating the task…</span></> : <>
            <button className="btn" onClick={() => onClose(false)}>Cancel</button>
            <button className="btn accent" onClick={run}><Icon name="play" size={13} />Run — 1 request</button>
          </>}
        </div>
      </div>
    </div>
  );
}

export function VideoCard({ video, note, draftSource, onUpscale, onRecreate }: {
  video: any; note?: string; draftSource?: any; onUpscale: (resolution: string) => void; onRecreate: () => void;
}) {
  const { app, toast } = useShell();
  if (!video) return <div className="video-card"><span className="muted">This video is no longer available.</span></div>;
  const controls = app.models.find((m) => m.key === video.modelKey)?.options?.controls ?? {};
  const finals: string[] = controls.finalFromDraft ?? [];
  const busy = ["submitting", "queued", "running"].includes(video.status);
  const elapsed = Math.max(0, Math.round((Date.now() - video.createdAt) / 1000));
  return (
    <div className={`video-card ${video.status}`}>
      <div className="video-head">
        <span className={`badge ${video.kind === "draft" ? "warnb" : "ok"}`}>{describe(video)}</span>
        <ProviderBadge provider={video.provider} />
        <span className={`video-status ${video.status}`}>{busy && <span className="spinner" />} {STATUS[video.status] ?? video.status}{busy && ` · ${elapsed}s`}</span>
      </div>
      {video.status === "succeeded" && video.hasFile && (
        <video className="video-player" src={assetUrl(video.id)} controls playsInline loop preload="metadata" />
      )}
      {busy && <div className="video-skeleton"><Icon name="film" size={26} /><span>Video tasks take a while. You can keep working; this resumes even if you close the app.</span></div>}
      {note && busy && <div className="muted small">{note}</div>}
      {video.error && <div className="warn" dir="auto">{video.error}</div>}
      {video.status === "uncertain" && <div className="muted small">It was not resent. Check the BytePlus console before trying again, so you are not charged twice.</div>}
      <div className="video-actions">
        {video.status === "succeeded" && video.kind === "draft" && (
          <span className="upscale">
            <span className="muted small">Upscale to</span>
            {finals.map((r) => <button key={r} className="btn small primary" onClick={() => onUpscale(r)}>{r}</button>)}
          </span>
        )}
        <button className="btn small" onClick={onRecreate} title="Puts the prompt, frames and references back in the composer so you can change them">
          <Icon name="undo" size={13} />Recreate
        </button>
        {video.status === "succeeded" && (
          <button className="btn small" onClick={async () => { try { const r = await rpc("video_export", { videoId: video.id }); toast(`Saved to ${r.path}`); } catch (e) { toast((e as Error).message, "error"); } }}>Save</button>
        )}
      </div>
      {draftSource && <div className="muted small">Made from the draft above.</div>}
    </div>
  );
}
