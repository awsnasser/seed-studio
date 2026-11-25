import { ProfileButton } from "./components/Team";
import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { AppError, assetUrl, rpc } from "./api";
import { AppState, Icon, Project, ProviderBadge, Shell, ShellCtx, SETTING_LABEL, Thumb, VALUE_LABEL, planSummary, requests, useShell } from "./shared";
import ChatView from "./components/Chat";
import CanvasView from "./components/Canvas";
import GalleryView from "./components/Gallery";
import ElementsView from "./components/Elements";
import SettingsDialog from "./components/Settings";
import FeaturePreviews from "./components/FeaturePreviews";
import appIcon from "../src-tauri/icons/128x128.png";

type Tab = "chat" | "canvas" | "elements" | "gallery" | "brief";
const TABS: [Tab, string, string][] = [["chat", "Chat", "chat"], ["canvas", "Canvas", "nodes"], ["elements", "Elements", "spark"], ["gallery", "Gallery", "image"], ["brief", "Instructions", "book"]];

function useSystemDark(): boolean {
  const mq = useMemo(() => window.matchMedia("(prefers-color-scheme: dark)"), []);
  const [dark, setDark] = useState(mq.matches);
  // A file dropped outside a drop area must never make the window navigate to that file.
  useEffect(() => {
    const stop = (e: DragEvent) => { e.preventDefault(); if (e.dataTransfer) e.dataTransfer.dropEffect = (e.target as HTMLElement | null)?.closest?.("[data-drop]") ? "copy" : "none"; };
    const drop = (e: DragEvent) => e.preventDefault();
    window.addEventListener("dragover", stop);
    window.addEventListener("drop", drop);
    return () => { window.removeEventListener("dragover", stop); window.removeEventListener("drop", drop); };
  }, []);
  useEffect(() => {
    const on = () => setDark(mq.matches);
    mq.addEventListener("change", on);
    return () => mq.removeEventListener("change", on);
  }, [mq]);
  return dark;
}

export default function App() {
  const [app, setApp] = useState<AppState | null>(null);
  const [projects, setProjects] = useState<Project[]>([]);
  const [projectId, setProjectId] = useState<string | null>(null);
  const [tab, setTab] = useState<Tab>("canvas");
  const [newProjectOpen, setNewProjectOpen] = useState(false);
  const [newProjectName, setNewProjectName] = useState("");
  const [creatingProject, setCreatingProject] = useState(false);
  const [settingsOpen, setSettingsOpen] = useState(false);
  const [featuresOpen, setFeaturesOpen] = useState(false);
  const featuresButton = useRef<HTMLButtonElement>(null);
  const [toasts, setToasts] = useState<{ id: number; text: string; kind: string }[]>([]);
  const [planReq, setPlanReq] = useState<{ plan: any; threadId?: string; done: (ran: boolean) => void } | null>(null);
  const [viewer, setViewer] = useState<string | null>(null);
  const [focusNode, setFocusNode] = useState<string | undefined>();
  const [bootError, setBootError] = useState<string | null>(null);
  const systemDark = useSystemDark();
  const resolvedTheme: "light" | "dark" = app?.theme === "system" || !app ? (systemDark ? "dark" : "light") : app.theme;

  useEffect(() => {
    document.documentElement.dataset.theme = resolvedTheme;
  }, [resolvedTheme]);

  const toast = useCallback((text: string, kind: "info" | "error" = "info") => {
    const id = Date.now() + Math.random();
    setToasts((t) => [...t.slice(-3), { id, text, kind }]);
    setTimeout(() => setToasts((t) => t.filter((x) => x.id !== id)), kind === "error" ? 9000 : 4500);
  }, []);

  const loadProjects = useCallback(async () => {
    const list = await rpc<Project[]>("projects_list");
    setProjects(list);
    return list;
  }, []);

  const booted = useRef(false);
  useEffect(() => {
    if (booted.current) return;
    booted.current = true;
    (async () => {
      try {
        const state = await rpc<AppState>("app_state");
        const list = await loadProjects();
        setApp(state);
        setProjectId(list.find((p) => p.id === state.lastProjectId)?.id ?? list[0]?.id ?? null);
        // Startup self-check of the local image channel (no network involved).
        const probe = new Image();
        const report = (imageChannel: boolean) => {
          void rpc("ui_ready", { imageChannel }).catch(() => {});
          if (!imageChannel) toast("Local images cannot be displayed in this window.", "error");
        };
        probe.onload = () => report(true);
        probe.onerror = () => report(false);
        probe.src = assetUrl("__probe");
      } catch (e) {
        setBootError((e as Error).message);
      }
    })();
  }, [loadProjects, toast]);

  const openProject = (id: string) => {
    setProjectId(id);
    void rpc("setting_set", { key: "last_project", value: id }).catch(() => {});
  };
  const createProject = async () => {
    if (!newProjectName.trim() || creatingProject) return;
    setCreatingProject(true);
    try {
      const p = await rpc<{ id: string }>("project_create", { name: newProjectName.trim() });
      await loadProjects();
      openProject(p.id);
      setTab("canvas");
      setNewProjectOpen(false);
      setNewProjectName("");
    } catch (e) { toast((e as Error).message, "error"); }
    finally { setCreatingProject(false); }
  };

  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.key !== "Escape") return;
      if (viewer) setViewer(null);
      else if (settingsOpen) setSettingsOpen(false);
      else if (newProjectOpen && !creatingProject) setNewProjectOpen(false);
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [viewer, settingsOpen, newProjectOpen, creatingProject]);

  const shell: Shell | null = app && {
    app, setApp, toast, resolvedTheme,
    reviewPlan: (plan, threadId) => new Promise<boolean>((done) => setPlanReq({ plan, threadId, done })),
    viewAsset: setViewer,
    goCanvas: (nodeId) => { setFocusNode(nodeId); setTab("canvas"); },
    openSettings: () => setSettingsOpen(true),
  };
  const project = projects.find((p) => p.id === projectId) ?? null;

  if (bootError) return <div className="boot">Seed Studio could not start: {bootError}</div>;
  if (!app || !shell) return <div className="boot"><span className="spinner" /></div>;

  const keyMissing = app.providerMode === "modelark" && !app.hasKey[app.region];

  return (
    <ShellCtx.Provider value={shell}>
      <div className="app">
        <header className="topbar">
          <div className="brand"><img className="brand-mark" src={appIcon} alt="" />Seed Studio</div>
          <nav className="project-tabs" aria-label="Projects">
            {projects.map((p) => (
              <button key={p.id} className={`ptab ${p.id === projectId ? "active" : ""}`} onClick={() => openProject(p.id)} title={p.name}>
                <span className="ptab-name">{p.name}</span>
              </button>
            ))}
            <button className="round dashed" onClick={() => setNewProjectOpen(true)} title="New project" aria-label="New project"><Icon name="plus" /></button>
          </nav>
          <div className="top-right">
            <button ref={featuresButton} className="btn small" aria-haspopup="dialog" onClick={() => setFeaturesOpen(true)}>
              <Icon name="image" size={14} />Features
            </button>
            {/* Shown only when it matters: simulation (results are not real) or a missing key. A working connection stays quiet; it is in Settings. */}
            {(app.providerMode === "mock" || keyMissing) && (
              <button className={`provider-pill ${app.providerMode} ${keyMissing ? "warn" : ""}`} onClick={() => setSettingsOpen(true)} title="Provider used for every request. Click to change.">
                <span className="dot" />
                {app.providerMode === "mock" ? "Simulation · offline" : <>ModelArk · {app.region} · no key</>}
              </button>
            )}
            <ProfileButton onSettings={() => setSettingsOpen(true)} />
            <button className="round" onClick={() => setSettingsOpen(true)} title={`Settings · ${app.providerMode === "mock" ? "Simulation" : `ModelArk · ${app.region}`}`} aria-label="Settings"><Icon name="gear" /></button>
          </div>
        </header>

        {project ? (
          <>
            <div className="subbar">
              <ProjectTitle key={project.id} project={project} onChanged={loadProjects} onDeleted={async () => {
                const list = await loadProjects();
                setProjectId(list[0]?.id ?? null);
              }} />
              <div className="pills" role="tablist">
                {TABS.map(([id, label, icon]) => (
                  <button key={id} role="tab" aria-selected={tab === id} className={`pill ${tab === id ? "active" : ""}`} onClick={() => setTab(id)}>
                    <Icon name={icon} size={14} />{label}
                    {id === "gallery" && project.assetCount > 0 && <span className="count">{project.assetCount}</span>}
                  </button>
                ))}
              </div>
            </div>
            <main className="content" key={`${project.id}-${tab}`}>
              {tab === "chat" && <ChatView project={project} onChanged={loadProjects} />}
              {tab === "canvas" && <CanvasView project={project} focusNode={focusNode} />}
              {tab === "elements" && <ElementsView project={project} />}
              {tab === "gallery" && <GalleryView project={project} onChanged={loadProjects} />}
              {tab === "brief" && <Brief project={project} onSaved={loadProjects} />}
            </main>
          </>
        ) : (
          <main className="welcome">
            <div className="welcome-content rise">
              <img className="welcome-icon" src={appIcon} alt="" />
              <p className="welcome-eyebrow">YOUR SPACE TO CREATE</p>
              <h1>Start with a blank canvas.</h1>
              <p className="welcome-description">Bring an idea. Build the frames. Make it move.<br />Your projects and media stay together on your Mac.</p>
              <div className="row welcome-actions">
                <button className="btn accent" onClick={() => setNewProjectOpen(true)}><Icon name="plus" size={16} />Create a project</button>
                <button className="btn" onClick={() => setSettingsOpen(true)}>Set up your API key</button>
              </div>
              <p className="muted small">Free, open source, and yours to shape.<br />Use your own BytePlus key for generation, or explore with local simulation.</p>
            </div>
          </main>
        )}

        {newProjectOpen && (
          <div className="overlay" onClick={() => { if (!creatingProject) setNewProjectOpen(false); }}>
            <form className="card new-project-dialog" role="dialog" aria-modal="true" aria-labelledby="new-project-heading" onClick={(e) => e.stopPropagation()} onSubmit={(e) => { e.preventDefault(); void createProject(); }}>
              <h2 id="new-project-heading">Create a project</h2>
              <p className="muted">A fresh canvas for your images, videos and ideas.</p>
              <label className="col" htmlFor="new-project-name">Project name
                <input id="new-project-name" autoFocus required maxLength={80} dir="auto" placeholder="Name your project" value={newProjectName} disabled={creatingProject} onChange={(e) => setNewProjectName(e.target.value)} />
              </label>
              <div className="row end">
                <button type="button" className="btn" disabled={creatingProject} onClick={() => setNewProjectOpen(false)}>Cancel</button>
                <button type="submit" className="btn accent" disabled={!newProjectName.trim() || creatingProject}>{creatingProject ? "Creating…" : "Create project"}</button>
              </div>
            </form>
          </div>
        )}

        {settingsOpen && <SettingsDialog onClose={() => setSettingsOpen(false)} />}
        {featuresOpen && <FeaturePreviews onClose={() => {
          setFeaturesOpen(false);
          featuresButton.current?.focus();
        }} />}
        {planReq && <PlanDialog key={planReq.plan.id} request={planReq} onClose={(ran) => { planReq.done(ran); setPlanReq(null); void loadProjects(); }} />}
        {viewer && (
          <div className="overlay" onClick={() => setViewer(null)} role="dialog" aria-label="Image preview">
            <img className="viewer-img" src={assetUrl(viewer)} alt="Full-size image" />
            <button className="btn viewer-save" onClick={async (e) => { e.stopPropagation(); try { const r = await rpc("asset_export", { assetId: viewer }); toast(`Saved to ${r.path}`); } catch (err) { toast((err as Error).message, "error"); } }}>Download</button>
          </div>
        )}
        <div className="toasts" aria-live="polite">
          {toasts.map((t) => <div key={t.id} className={`toast ${t.kind}`}>{t.text}</div>)}
        </div>
      </div>
    </ShellCtx.Provider>
  );
}

function ProjectTitle({ project, onChanged, onDeleted }: { project: Project; onChanged: () => void; onDeleted: () => void }) {
  const { toast } = useShell();
  const [name, setName] = useState(project.name);
  const [confirm, setConfirm] = useState(false);
  const save = async () => {
    if (name.trim() && name !== project.name) {
      await rpc("project_update", { projectId: project.id, name });
      onChanged();
    } else setName(project.name);
  };
  return (
    <div className="project-title">
      <input className="title-input" dir="auto" value={name} maxLength={80} aria-label="Project name" size={Math.max(8, name.length)} onChange={(e) => setName(e.target.value)}
        onBlur={save} onKeyDown={(e) => e.key === "Enter" && (e.target as HTMLInputElement).blur()} />
      {confirm ? (
        <span className="confirm-inline">
          Delete this project and its images for good?
          <button className="btn small danger" onClick={async () => { try { await rpc("project_delete", { projectId: project.id }); onDeleted(); } catch (e) { toast((e as Error).message, "error"); } }}>Delete</button>
          <button className="btn small" onClick={() => setConfirm(false)}>Cancel</button>
        </span>
      ) : (
        <button className="round ghost" onClick={() => setConfirm(true)} title="Delete project" aria-label="Delete project"><Icon name="trash" size={15} /></button>
      )}
    </div>
  );
}

function Brief({ project, onSaved }: { project: Project; onSaved: () => void }) {
  const [text, setText] = useState(project.instructions);
  const [saved, setSaved] = useState(true);
  const save = async () => {
    await rpc("project_update", { projectId: project.id, instructions: text });
    setSaved(true);
    onSaved();
  };
  return (
    <div className="page-scroll">
      <div className="card brief rise">
        <h2>Project instructions</h2>
        <p className="muted">Sent with every chat and with the canvas assistant in this project only. Example: “The bottle is glass, the logo is black, images are square.”</p>
        <textarea dir="auto" value={text} onChange={(e) => { setText(e.target.value); setSaved(false); }} onBlur={save} rows={10} placeholder="What should Seed know about this project?" />
        <div className="row"><button className="btn primary" onClick={save} disabled={saved}>{saved ? "Saved" : "Save"}</button></div>
        <p className="muted small">Project data stays on this Mac. Only the text and images used in a request you send reach BytePlus.</p>
      </div>
    </div>
  );
}

const RESULT_LABEL: Record<string, string> = {
  succeeded: "Done", uncertain: "Outcome unknown — it will not be resent automatically", skipped: "Not run", cancelled: "Stopped before sending", failed: "Failed",
};

/** Shows exactly what will be sent. One click = one permission for this plan only. */
function PlanDialog({ request, onClose }: { request: { plan: any; threadId?: string }; onClose: (ran: boolean) => void }) {
  const { app, toast } = useShell();
  const [plan, setPlan] = useState(request.plan);
  const [phase, setPhase] = useState<"review" | "running" | "done">("review");
  const [notice, setNotice] = useState<string | null>(null);
  const [result, setResult] = useState<any>(null);
  const started = useRef(false);
  const calls = plan.items.filter((i: any) => i.action === "new").length;

  const run = async () => {
    if (started.current) return; // a second click never sends a second request
    started.current = true;
    setPhase("running");
    try {
      const r = await rpc("run_execute", { planId: plan.id, threadId: request.threadId });
      if (r.stale) {
        setPlan(r.plan);
        setNotice("The inputs changed after this plan was shown. Here is the updated plan; nothing was sent.");
        started.current = false;
        setPhase("review");
        return;
      }
      setResult(r);
      setPhase("done");
      if (r.status === "completed") onClose(true);
    } catch (e) {
      const err = e as AppError;
      toast(err.message, "error");
      if (err.code === "run_already_started") onClose(true);
      else { setNotice(err.message); setPhase("done"); }
    }
  };

  return (
    <div className="overlay" role="dialog" aria-label="Run plan">
      <div className="dialog plan">
        <div className="dialog-head">
          <h2>Run plan</h2>
          {plan.provider === "mock" && <ProviderBadge provider="mock" />}
        </div>
        <p className="plan-summary">{planSummary(plan)}</p>
        <p className="muted small">
          {plan.provider === "mock"
            ? "Provider: local simulation — no network, no cost. Results are labelled “Simulation”."
            : <>Provider: BytePlus ModelArk · {plan.region}. These are request counts, not a bill; see <a href="https://docs.byteplus.com/en/docs/modelark/model-pricing" target="_blank" rel="noreferrer">ModelArk pricing</a>.</>}
          {plan.provider !== "mock" && !app.hasKey[plan.region] && <b className="warn"> No API key is saved for this region.</b>}
        </p>
        {notice && <div className="notice">{notice}</div>}
        <ul className="plan-items">
          {plan.items.map((it: any) => {
            const res = result?.results?.find((r: any) => r.nodeId === it.nodeId);
            return (
              <li key={it.nodeId} className={`plan-item ${it.action}`}>
                <div className="plan-item-head">
                  <b dir="auto">{it.title}</b>
                  <span className={`badge ${it.action}`}>{it.action === "new" ? "New request" : it.action === "reuse" ? "Saved" : "Needs a fix"}</span>
                  <span className="muted">{it.modelLabel} · <span className="mono">{it.modelId}</span></span>
                </div>
                {it.reason && <div className="warn">{it.reason}</div>}
                {it.finalPrompt && <div className="plan-prompt" dir="auto">{it.finalPrompt}</div>}
                {it.textFrom && <div className="muted small">The prompt comes from the text node before it (rewrite or describe).</div>}
                <div className="plan-meta">
                  {it.refs.map((r: any, i: number) => r.from === "asset"
                    ? <span key={i} className="ref-chip"><Thumb id={r.assetId} className="thumb tiny" /> Reference {i + 1}</span>
                    : <span key={i} className="ref-chip">Reference {i + 1}: output of an earlier node</span>)}
                  {it.pixelSize && <span className="chip strong">Sent as {it.pixelSize} px</span>}
                  {Object.entries(it.settings).map(([k, v]) => <span key={k} className="chip">{SETTING_LABEL[k] ?? k}: {VALUE_LABEL[v as string] ?? (v as string)}</span>)}
                </div>
                {res && res.status !== "reused" && (
                  <div className={res.error ? "warn" : "ok"}>{RESULT_LABEL[res.status] ?? res.status}{res.error && <> — {res.error}</>}</div>
                )}
              </li>
            );
          })}
          {plan.items.length === 0 && <li className="muted">There are no generation nodes in the selection.</li>}
        </ul>
        <div className="row end">
          {phase === "review" && <>
            <button className="btn" onClick={() => onClose(false)}>Cancel</button>
            <button className="btn accent" onClick={run} disabled={plan.blocked > 0 || plan.items.length === 0}>
              <Icon name="play" size={13} />{calls === 0 ? "Use saved results" : `Run — ${requests(calls)}`}
            </button>
          </>}
          {phase === "running" && <>
            <span className="spinner" aria-hidden /> <span>Running… nodes execute one after another.</span>
            <button className="btn" onClick={() => rpc("run_cancel", { runId: plan.id })} title="Stops the remaining nodes; a request that already started is not refunded">Stop the rest</button>
          </>}
          {phase === "done" && <button className="btn primary" onClick={() => onClose(!!result)}>Close</button>}
        </div>
      </div>
    </div>
  );
}
