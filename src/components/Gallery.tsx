import { useCallback, useEffect, useState } from "react";
import { fileToBase64, listen, pickImage, rpc } from "../api";
import { Icon, Project, ProviderBadge, SETTING_LABEL, Thumb, VALUE_LABEL, useShell } from "../shared";

export function useAssets(projectId: string) {
  const [assets, setAssets] = useState<any[]>([]);
  const reload = useCallback(async () => setAssets(await rpc("assets_list", { projectId })), [projectId]);
  useEffect(() => {
    void reload();
    return listen("run-update", (p) => p.projectId === projectId && void reload());
  }, [projectId, reload]);
  return { assets, reload };
}

export async function importImage(projectId: string): Promise<any | null> {
  const file = await pickImage();
  if (!file) return null;
  return rpc("asset_import", { projectId, name: file.name.replace(/\.[^.]+$/, ""), base64: await fileToBase64(file) });
}

export default function GalleryView({ project, onChanged }: { project: Project; onChanged: () => void }) {
  const { toast, viewAsset, goCanvas } = useShell();
  const { assets, reload } = useAssets(project.id);
  const [selected, setSelected] = useState<string | null>(null);
  const current = assets.find((a) => a.id === selected);

  const add = async () => {
    try {
      if (await importImage(project.id)) { await reload(); onChanged(); }
    } catch (e) { toast((e as Error).message, "error"); }
  };
  const toCanvas = async (assetId: string, title: string) => {
    try {
      await rpc("canvas_patch", { projectId: project.id, label: "Reference from gallery", operations: [{ op: "addNode", type: "imageAsset", data: { title, assetId } }] });
      toast("Added to the canvas as a reference.");
    } catch (e) { toast((e as Error).message, "error"); }
  };

  return (
    <div className="gallery">
      <div className="card gallery-main rise">
        <div className="row between">
          <h2>Gallery</h2>
          <button className="btn" onClick={add}><Icon name="plus" size={14} />Import image</button>
        </div>
        {assets.length === 0 && <p className="muted empty-note">No images yet. Import a reference, or generate one from the chat or the canvas.</p>}
        <div className="grid">
          {assets.map((a, i) => (
            <button key={a.id} className={`tile ${a.id === selected ? "active" : ""}`} style={{ animationDelay: `${Math.min(i, 12) * 30}ms` }} onClick={() => setSelected(a.id)} aria-label={a.title || "Image"}>
              <Thumb id={a.id} className="tile-img" alt={a.title} />
              <span className="tile-cap">
                <span className="tile-title" dir="auto">{a.title || (a.kind === "reference" ? "Reference" : "Result")}</span>
                <span className={`badge ${a.kind}`}>{a.kind === "reference" ? "Reference" : "Generated"}</span>
                <ProviderBadge provider={a.provider} />
              </span>
            </button>
          ))}
        </div>
      </div>
      {current && (
        <aside className="card detail slide-in" key={current.id}>
          <Thumb id={current.id} className="detail-img" onClick={() => viewAsset(current.id)} alt={current.title} />
          <h3 dir="auto">{current.title || "Image"} <ProviderBadge provider={current.provider} /></h3>
          <dl>
            {current.modelId && <><dt>Model</dt><dd><span className="mono">{current.modelId}</span>{current.region && current.region !== "-" && <> · {current.region}</>}</dd></>}
            <dt>Size</dt><dd>{current.width}×{current.height} · {current.mime.replace("image/", "").toUpperCase()}</dd>
            {current.prompt && <><dt>Final prompt</dt><dd dir="auto">{current.prompt}</dd></>}
            {current.settings && <><dt>Settings</dt><dd>{Object.entries(current.settings).map(([k, v]) => `${SETTING_LABEL[k] ?? k}: ${VALUE_LABEL[v as string] ?? v}`).join(" · ")}</dd></>}
            {current.parentAssetIds.length > 0 && <><dt>References used</dt><dd className="row">{current.parentAssetIds.map((p: string) => <Thumb key={p} id={p} className="thumb small" onClick={() => setSelected(p)} alt="Reference" />)}</dd></>}
          </dl>
          <div className="col">
            <button className="btn" onClick={async () => { try { const r = await rpc("asset_export", { assetId: current.id }); toast(`Saved a copy to ${r.path}`); } catch (e) { toast((e as Error).message, "error"); } }}>Save a copy to Finder</button>
            <button className="btn" onClick={() => toCanvas(current.id, current.title || "Reference")}>Use as a reference on the canvas</button>
            {current.nodeId && <button className="btn" onClick={() => goCanvas(current.nodeId)}>Open the node that made it</button>}
          </div>
        </aside>
      )}
    </div>
  );
}
