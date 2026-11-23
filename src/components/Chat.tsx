import { useCallback, useEffect, useRef, useState } from "react";
import { AppState, Icon, Project, ProviderBadge, Thumb, useShell } from "../shared";
import { fileToBase64, listen, rpc } from "../api";
import { ImageOptionsBar, ImgOpts, settingsFor } from "./ImageOptions";
import { ROLE_LABEL, VideoCard, VideoInput, VideoOptionsBar, VideoOpts, VideoPlanDialog, useVideos, videoOptionsFor } from "./Video";

type Msg = { id: string; role: string; content: string; status: string; modelId?: string; provider?: string; pinned: boolean; meta: any };
type Thread = { id: string; title: string; messageCount: number };
type Attachment = { kind: "image" | "document"; id: string; name: string };

function pickFiles(accept?: string): Promise<File[]> {
  return new Promise((resolve) => {
    const input = document.createElement("input");
    input.type = "file";
    input.multiple = true;
    if (accept) input.accept = accept;
    input.onchange = () => resolve(Array.from(input.files ?? []));
    input.oncancel = () => resolve([]);
    input.click();
  });
}

export default function ChatView({ project, onChanged }: { project: Project; onChanged: () => void }) {
  const { toast, reviewPlan, viewAsset, app, setApp, goCanvas, openSettings } = useShell();
  const [opened, setOpened] = useState(false);
  const [threads, setThreads] = useState<Thread[]>([]);
  const [threadId, setThreadId] = useState<string | null>(null);
  const [messages, setMessages] = useState<Msg[] | null>(null);
  const [draft, setDraft] = useState("");
  const [mode, setMode] = useState<"chat" | "image" | "video">("chat");
  const [attachments, setAttachments] = useState<Attachment[]>([]);
  const [editRef, setEditRef] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const [importing, setImporting] = useState(0);
  const [live, setLive] = useState<Record<string, string>>({});
  const [dragging, setDragging] = useState(false);
  const [historyOpen, setHistoryOpen] = useState(() => localStorage.getItem("chatHistory") === "1");
  const [confirmDelete, setConfirmDelete] = useState<string | null>(null);
  const [imgOpts, setImgOptsState] = useState<ImgOpts>(() => { try { return JSON.parse(localStorage.getItem("imgOpts") ?? "") as ImgOpts; } catch { return { aspect: "1:1" }; } });
  const setImgOpts = (patch: ImgOpts) => setImgOptsState((prev) => { const next = { ...prev, ...patch }; localStorage.setItem("imgOpts", JSON.stringify(next)); return next; });
  const imageModel = app.models.find((m) => m.key === app.imageModel);
  const [savedVideoOpts, setVideoOptsState] = useState<VideoOpts>(() => { try { return { modelKey: "seedance-2.5", draft: true, resolution: "1080p", ratio: "adaptive", duration: -1, ...JSON.parse(localStorage.getItem("videoOpts") ?? "") }; } catch { return { modelKey: "seedance-2.5", draft: true, resolution: "1080p", ratio: "adaptive", duration: -1 }; } });
  const videoModel = app.models.find((m) => m.key === savedVideoOpts.modelKey && m.kind === "video");
  const videoOpts = videoOptionsFor(videoModel, savedVideoOpts);
  const setVideoOpts = (patch: Partial<VideoOpts>) => setVideoOptsState((prev) => {
    const merged = { ...prev, ...patch };
    const model = app.models.find((m) => m.key === merged.modelKey && m.kind === "video");
    const next = videoOptionsFor(model, merged);
    localStorage.setItem("videoOpts", JSON.stringify(next));
    return next;
  });
  const [videoInputs, setVideoInputs] = useState<VideoInput[]>([]);
  const [videoPlan, setVideoPlan] = useState<any>(null);
  const { videos, notes: videoNotes, reload: reloadVideos } = useVideos(project.id);
  const endRef = useRef<HTMLDivElement>(null);
  const inputRef = useRef<HTMLTextAreaElement>(null);
  const dragDepth = useRef(0);
  const projectId = project.id;

  const loadThreads = useCallback(async () => {
    const list = await rpc<Thread[]>("threads_list", { projectId });
    setThreads(list);
    return list;
  }, [projectId]);
  const loadMessages = useCallback(async (id: string) => setMessages(await rpc<Msg[]>("messages_list", { threadId: id })), []);

  /** Open an empty conversation: reuse one if it exists, otherwise create it. */
  const freshThread = useCallback(async () => {
    const list = await loadThreads();
    const empty = list.find((t) => t.messageCount === 0);
    if (empty) return setThreadId(empty.id);
    const t = await rpc<Thread>("thread_create", { projectId });
    await loadThreads();
    setThreadId(t.id);
  }, [loadThreads, projectId]);

  const started = useRef(false);
  useEffect(() => {
    if (started.current) return;
    started.current = true;
    void freshThread().catch((e) => toast((e as Error).message, "error"));
  }, [freshThread, toast]);

  useEffect(() => {
    if (!threadId) return;
    setMessages(null);
    void loadMessages(threadId);
    const offs = [
      listen("messages-changed", (p) => p.threadId === threadId && void loadMessages(threadId)),
      listen("chat-delta", (p) => p.threadId === threadId && setLive((l) => ({ ...l, [p.messageId]: (l[p.messageId] ?? "") + p.delta }))),
    ];
    return () => offs.forEach((off) => off());
  }, [threadId, loadMessages]);

  const isHero = messages !== null && messages.length === 0;
  useEffect(() => { inputRef.current?.focus(); }, [threadId, isHero]);

  const toggleHistory = () => setHistoryOpen((v) => { localStorage.setItem("chatHistory", v ? "0" : "1"); return !v; });

  const addFiles = async (files: File[], as: "chat" | "image" | "video" = mode) => {
    for (const file of files) {
      setImporting((n) => n + 1);
      try {
        const r = await rpc("file_import", { projectId, name: file.name, base64: await fileToBase64(file) });
        if (r.kind === "image") {
          if (as === "image") setEditRef(r.id);
          else if (as === "video") setVideoInputs((v) => [...v, { assetId: r.id, role: "reference_image" }]);
          else setAttachments((a) => [...a, { kind: "image", id: r.id, name: file.name }]);
        } else if (as !== "chat") {
          toast(`“${file.name}” is a document. ${as === "video" ? "Video" : "Image"} mode only takes images; switch to Chat to attach it.`, "error");
        } else {
          setAttachments((a) => [...a, { kind: "document", id: r.id, name: file.name }]);
          if (r.truncated) toast(`“${file.name}” is long; only the first 40,000 characters are sent with each message.`);
        }
      } catch (e) {
        toast((e as Error).message, "error");
      } finally {
        setImporting((n) => n - 1);
      }
    }
    onChanged();
    inputRef.current?.focus();
  };

  const send = async () => {
    const text = draft.trim();
    if (!text || !threadId || busy || importing > 0) return;
    setBusy(true);
    setDraft("");
    try {
      if (mode === "video") {
        const plan = await rpc("video_plan", { projectId, threadId, prompt: text, modelKey: videoOpts.modelKey, draft: videoOpts.draft, resolution: videoOpts.resolution, ratio: videoOpts.ratio, duration: videoOpts.duration, inputs: videoInputs });
        setDraft(text); // kept until the plan is confirmed
        setVideoPlan(plan);
      } else if (mode === "image") {
        const flow = await rpc("chat_image_flow", { projectId, threadId, prompt: text, refAssetId: editRef ?? undefined, settings: settingsFor(imageModel, imgOpts) });
        await loadMessages(threadId);
        setEditRef(null);
        await reviewPlan(flow.plan, threadId);
      } else {
        const files = attachments;
        setAttachments([]);
        await rpc("chat_send", {
          threadId, text,
          attachmentIds: files.filter((f) => f.kind === "image").map((f) => f.id),
          documentIds: files.filter((f) => f.kind === "document").map((f) => f.id),
        });
      }
    } catch (e) {
      // Provider errors are already shown in the conversation as an error message.
      if (!(e as any).code?.startsWith("provider_") && (e as any).code !== "no_key") toast((e as Error).message, "error");
    } finally {
      setBusy(false);
      setLive({});
      void loadMessages(threadId);
      void loadThreads();
      onChanged();
    }
  };

  const setModel = async (key: string) => {
    try {
      setApp(await rpc<AppState>("setting_set", { key: mode === "image" ? "image_model" : "chat_model", value: key }));
    } catch (e) { toast((e as Error).message, "error"); }
  };
  const toCanvasPrompt = async (text: string) => {
    try {
      await rpc("canvas_patch", { projectId, label: "Prompt from chat", operations: [{ op: "addNode", type: "prompt", data: { title: "From chat", text: text.slice(0, 8000) } }] });
      toast("Added to the canvas as a prompt node.");
    } catch (e) { toast((e as Error).message, "error"); }
  };
  const toCanvasImage = async (assetId: string) => {
    try {
      await rpc("canvas_patch", { projectId, label: "Image from chat", operations: [{ op: "addNode", type: "imageAsset", data: { title: "From chat", assetId } }] });
      toast("Sent to the canvas as a reference image.");
    } catch (e) { toast((e as Error).message, "error"); }
  };
  const startEdit = (assetId: string) => {
    setMode("image");
    setEditRef(assetId);
    inputRef.current?.focus();
  };
  const pin = async (m: Msg) => {
    await rpc("message_pin", { messageId: m.id, pinned: !m.pinned });
    if (threadId) void loadMessages(threadId);
  };
  const deleteThread = async (id: string) => {
    try {
      await rpc("thread_delete", { threadId: id });
      setConfirmDelete(null);
      if (id === threadId) { setThreadId(null); await freshThread(); } else await loadThreads();
    } catch (e) { toast((e as Error).message, "error"); }
  };
  /** After an access error: the user explicitly picks another model, then resends. */
  const retryWith = async (key: string, index: number) => {
    await setModel(key);
    const prev = [...(messages ?? [])].slice(0, index).reverse().find((m) => m.role === "user");
    if (prev) setDraft(prev.content);
    inputRef.current?.focus();
  };

  useEffect(() => { endRef.current?.scrollIntoView({ block: "end" }); }, [messages, live, videos, mode]);
  const upscale = async (videoId: string, resolution: string) => {
    try {
      setVideoPlan(await rpc("video_plan", { projectId, threadId, draftOf: videoId, resolution }));
    } catch (e) { toast((e as Error).message, "error"); }
  };
  /** Bring an earlier video's prompt, frames, references and options back into the composer. */
  const recreate = (videoId: string) => {
    const v = videos[videoId];
    if (!v) return;
    const source = v.draftOf && videos[v.draftOf] ? videos[v.draftOf] : v; // an upscale carries its draft's options
    setMode("video");
    setEditRef(null);
    setDraft(v.prompt ?? "");
    setVideoInputs((v.inputs ?? []).map((i: VideoInput) => ({ assetId: i.assetId, role: i.role })));
    const p = source.params ?? {};
    setVideoOpts({ modelKey: source.modelKey ?? "seedance-2.5", draft: source.kind === "draft", ratio: p.ratio ?? "adaptive", duration: p.duration ?? -1, ...(v.kind === "final" && !v.draftOf ? { resolution: p.resolution } : {}) });
    toast("The prompt, images and options are back in the composer. Edit them and generate a new video.");
    inputRef.current?.focus();
  };

  const onDrag = (e: React.DragEvent, delta: number) => {
    if (!Array.from(e.dataTransfer.types).includes("Files")) return;
    e.preventDefault();
    dragDepth.current = Math.max(0, dragDepth.current + delta);
    setDragging(dragDepth.current > 0);
  };
  const onDrop = (e: React.DragEvent) => {
    if (!Array.from(e.dataTransfer.types).includes("Files")) return;
    e.preventDefault();
    dragDepth.current = 0;
    setDragging(false);
    void addFiles(Array.from(e.dataTransfer.files));
  };
  const onPaste = (e: React.ClipboardEvent) => {
    const files = Array.from(e.clipboardData.files);
    if (files.length) { e.preventDefault(); void addFiles(files); }
  };

  const hero = isHero;
  const models = app.models.filter((m) => m.kind === mode || (mode === "chat" && m.kind === "chat"));
  const currentModel = mode === "image" ? app.imageModel : mode === "video" ? (videoModel?.key ?? "") : app.chatModel;
  const stack: Attachment[] = mode === "image" ? (editRef ? [{ kind: "image", id: editRef, name: "Reference" }] : [])
    : mode === "video" ? videoInputs.map((i) => ({ kind: "image" as const, id: i.assetId, name: ROLE_LABEL[i.role] })) : attachments;
  const placeholder = mode === "video" ? "Describe the motion and the scene…" : mode === "image"
    ? (editRef ? "Describe the edit — e.g. “change only the background to a sand color”" : "Describe the image you want…")
    : (attachments.length ? "Summarize, rephrase, convert…" : "Ask Seed a question or make a request…");

  // On the empty home page the bar starts as one quiet line and opens when it is used.
  const barOpen = !hero || opened || draft.length > 0 || stack.length > 0 || mode !== "chat";
  const composer = (
    <div className={`composer-wrap ${hero ? "centered" : "docked"} ${hero && !barOpen ? "closed" : "open"}`}>
      {stack.length > 0 && (
        <div className="file-stack" aria-hidden>
          {stack.slice(0, 4).map((f, i) => (
            <div key={f.id} className={`paper p${i}`}>
              {f.kind === "image" ? <Thumb id={f.id} className="paper-img" /> : <div className="paper-doc"><Icon name="doc" size={18} /><span>{f.name}</span></div>}
            </div>
          ))}
        </div>
      )}
      <div className="composer">
        <div className="composer-input">
          {stack.length > 0 && (
            <span className="files-chip">
              {mode === "image" ? "Editing this image" : `${stack.length} ${mode === "video" ? (stack.length === 1 ? "image" : "images") : stack.length === 1 ? "file" : "files"}`}
              <button onClick={() => (mode === "image" ? setEditRef(null) : mode === "video" ? setVideoInputs([]) : setAttachments([]))} aria-label="Remove attachments"><Icon name="x" size={12} /></button>
            </span>
          )}
          {importing > 0 && <span className="files-chip"><span className="spinner" /> Reading…</span>}
          <textarea ref={inputRef} dir="auto" value={draft} rows={hero ? (barOpen ? 3 : 1) : 2} maxLength={8000} placeholder={placeholder}
            onFocus={() => setOpened(true)} onChange={(e) => setDraft(e.target.value)} onPaste={onPaste}
            onKeyDown={(e) => { if (e.key === "Enter" && !e.shiftKey && !e.nativeEvent.isComposing) { e.preventDefault(); void send(); } }} />
        </div>
        {mode === "video" && <VideoOptionsBar model={videoModel} opts={videoOpts} onChange={setVideoOpts} inputs={videoInputs} setInputs={setVideoInputs} />}
        {mode === "image" && <ImageOptionsBar model={imageModel} opts={imgOpts} onChange={setImgOpts} hasReference={!!editRef} />}
        <div className="composer-bar">
          <button className="round ghost" title="Attach files" aria-label="Attach files"
            onClick={async () => addFiles(await pickFiles(mode !== "chat" ? "image/png,image/jpeg,image/webp" : undefined))}><Icon name="clip" size={18} /></button>
          <div className="segmented small" role="radiogroup" aria-label="What to send">
            <button role="radio" aria-checked={mode === "chat"} className={mode === "chat" ? "on" : ""} onClick={() => { setMode("chat"); setEditRef(null); }}>Chat</button>
            <button role="radio" aria-checked={mode === "image"} className={mode === "image" ? "on" : ""} onClick={() => setMode("image")}>Image</button>
            <button role="radio" aria-checked={mode === "video"} className={mode === "video" ? "on" : ""} onClick={() => { setMode("video"); setEditRef(null); }}>Video</button>
          </div>
          <label className="model-select" title="Model used for this request">
            <span className={`dot st-${models.find((m) => m.key === currentModel)?.status}`} />
            <select value={currentModel} onChange={(e) => mode === "video" ? setVideoOpts({ modelKey: e.target.value }) : setModel(e.target.value)} aria-label="Model">
              {models.map((m) => <option key={m.key} value={m.key}>{m.label}{m.status === "needs_activation" ? " — no access" : ""}</option>)}
            </select>
          </label>
          <span className="grow" />
          <span className="muted small counter">{draft.length}/8000</span>
          {busy && mode === "chat"
            ? <button className="send stop" onClick={() => threadId && rpc("chat_cancel", { threadId })} title="Stop" aria-label="Stop"><span className="stop-square" /></button>
            : <button className="send" onClick={send} disabled={busy || importing > 0 || !draft.trim()} title={mode === "chat" ? "Send" : "Show the run plan"} aria-label="Send"><Icon name="arrow" size={18} /></button>}
        </div>
      </div>
    </div>
  );

  return (
    <div className="chat" data-drop onDragEnter={(e) => onDrag(e, 1)} onDragLeave={(e) => onDrag(e, -1)} onDragOver={(e) => onDrag(e, 0)} onDrop={onDrop}>
      {historyOpen && (
        <aside className="threads card">
          <div className="threads-head"><b>Chats</b><button className="round ghost" onClick={toggleHistory} aria-label="Hide chats"><Icon name="x" size={14} /></button></div>
          {threads.map((t) => (
            <div key={t.id} className={`thread ${t.id === threadId ? "active" : ""}`}>
              <button className="thread-title" dir="auto" onClick={() => setThreadId(t.id)}>{t.title}</button>
              {confirmDelete === t.id
                ? <button className="link danger" onClick={() => deleteThread(t.id)} onBlur={() => setConfirmDelete(null)}>Delete?</button>
                : <button className="thread-x" onClick={() => setConfirmDelete(t.id)} aria-label={`Delete chat ${t.title}`}><Icon name="trash" size={13} /></button>}
            </div>
          ))}
        </aside>
      )}
      <div className="chat-main">
        <div className="chat-tools">
          {!historyOpen && <button className="round" onClick={toggleHistory} title="Show chats" aria-label="Show chats"><Icon name="history" /></button>}
          <button className="round" onClick={() => void freshThread()} title="New chat" aria-label="New chat"><Icon name="plus" /></button>
        </div>

        {hero ? (
          <div className="hero">
            <div className="hero-mark rise"><Icon name="spark" size={22} /></div>
            <h1 className="hero-title rise d1">Ideas turned into <em>images</em></h1>
            <div className="rise d3">{composer}</div>
          </div>
        ) : (
          <>
            <div className="messages">
              {(messages ?? []).map((m, index) => {
                const text = m.status === "streaming" ? (live[m.id] ?? "") : m.content;
                const images: string[] = m.meta?.assetIds ?? [];
                const code = m.meta?.errorCode;
                return (
                  <article key={m.id} className={`msg ${m.role} ${m.status}`}>
                    {m.meta?.kind === "image_request" && <div className="msg-tag">{m.meta.refAssetId ? "Image edit" : "Image request"}</div>}
                    {m.meta?.kind === "video_request" && <div className="msg-tag">Video request</div>}
                    {m.meta?.kind === "video_upscale" && <div className="msg-tag">Upscale the draft to {videos[m.meta.videoId]?.params?.resolution}</div>}
                    {m.meta?.kind === "video_request" && (videos[m.meta.videoId]?.inputs ?? []).length > 0 && (
                      <div className="msg-files">
                        {videos[m.meta.videoId].inputs.map((i: VideoInput, ix: number) => (
                          <span key={ix} className="role-thumb"><Thumb id={i.assetId} className="thumb small" onClick={() => viewAsset(i.assetId)} alt={ROLE_LABEL[i.role]} /><small>{ROLE_LABEL[i.role]}</small></span>
                        ))}
                      </div>
                    )}
                    {m.meta?.kind === "video" && (
                      <VideoCard video={videos[m.meta.videoId]} note={videoNotes[m.meta.videoId]} draftSource={videos[videos[m.meta.videoId]?.draftOf]}
                        onUpscale={(resolution) => upscale(m.meta.videoId, resolution)} onRecreate={() => recreate(m.meta.videoId)} />
                    )}
                    {(m.meta?.refAssetId || m.meta?.attachments?.length > 0 || m.meta?.documents?.length > 0) && (
                      <div className="msg-files">
                        {m.meta?.refAssetId && <Thumb id={m.meta.refAssetId} className="thumb small" onClick={() => viewAsset(m.meta.refAssetId)} alt="Reference image" />}
                        {(m.meta?.attachments ?? []).map((a: string) => <Thumb key={a} id={a} className="thumb small" onClick={() => viewAsset(a)} alt="Attachment" />)}
                        {(m.meta?.documents ?? []).map((d: any) => <span key={d.id} className="doc-chip"><Icon name="doc" size={14} />{d.name}</span>)}
                      </div>
                    )}
                    {text && <div className="msg-text" dir="auto">{text}</div>}
                    {m.status === "streaming" && (text ? <span className="cursor" aria-label="Writing" /> : <span className="typing" aria-label="Thinking"><i /><i /><i /></span>)}
                    {m.status === "error" && (code === "provider_activation" || code === "no_key" || code === "provider_auth") && (
                      <div className="msg-actions">
                        {code === "provider_activation" && app.models.filter((x) => x.kind === "chat" && x.key !== app.chatModel).map((x) => (
                          <button key={x.key} className="btn small primary" onClick={() => retryWith(x.key, index)}>Switch to {x.label} and retry</button>
                        ))}
                        <button className="btn small" onClick={openSettings}>Open Settings</button>
                        {code === "provider_activation" && <a className="btn small" href="https://console.byteplus.com/ark" target="_blank" rel="noreferrer">BytePlus console</a>}
                      </div>
                    )}
                    {images.length > 0 && (
                      <div className="msg-images">
                        {images.map((a) => (
                          <figure key={a}>
                            <Thumb id={a} className="msg-img" onClick={() => viewAsset(a)} alt="Result" />
                            <figcaption className="row">
                              <button className="btn small" onClick={() => startEdit(a)}>Edit this image</button>
                              <button className="btn small" onClick={() => toCanvasImage(a)}>To canvas</button>
                              <button className="btn small" onClick={async () => { try { const r = await rpc("asset_export", { assetId: a }); toast(`Saved to ${r.path}`); } catch (e) { toast((e as Error).message, "error"); } }}>Save</button>
                            </figcaption>
                          </figure>
                        ))}
                      </div>
                    )}
                    {m.role === "assistant" && m.status !== "streaming" && (
                      <footer className="msg-foot">
                        {m.modelId && <span className="mono muted">{m.modelId}</span>}
                        <ProviderBadge provider={m.provider} />
                        {m.status === "incomplete" && <span className="badge warnb">Incomplete</span>}
                        {m.content && m.status !== "error" && <>
                          <button className="link" onClick={() => toCanvasPrompt(m.content)}>To canvas as prompt</button>
                          <button className="link" onClick={() => pin(m)}>{m.pinned ? "Unpin" : "Pin"}</button>
                        </>}
                      </footer>
                    )}
                  </article>
                );
              })}
              <div ref={endRef} />
            </div>
            {composer}
          </>
        )}
      </div>
      {videoPlan && <VideoPlanDialog key={videoPlan.id} plan={videoPlan} onClose={(started) => {
        setVideoPlan(null);
        if (started) { setDraft(""); setVideoInputs([]); void reloadVideos(); if (threadId) void loadMessages(threadId); void loadThreads(); }
      }} />}
      {dragging && (
        <div className="drop-veil">
          <div className="drop-card"><Icon name="clip" size={26} /><b>Drop to attach</b><span className="muted">Images, PDFs and text files</span></div>
        </div>
      )}
    </div>
  );
}
