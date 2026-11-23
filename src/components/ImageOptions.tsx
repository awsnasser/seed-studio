import { VALUE_LABEL } from "../shared";

/** A small outline whose proportions are the ratio itself. */
export function AspectIcon({ ratio, size = 18 }: { ratio: string; size?: number }) {
  const [a, b] = ratio.includes(":") ? ratio.split(":").map(Number) : [1, 1];
  const w = a >= b ? size : (size * a) / b;
  const h = b >= a ? size : (size * b) / a;
  const auto = ratio === "auto";
  return (
    <svg width={size + 2} height={size + 2} viewBox={`0 0 ${size + 2} ${size + 2}`} aria-hidden className="aspect-icon">
      <rect x={(size + 2 - w) / 2} y={(size + 2 - h) / 2} width={w} height={h} rx={2.5} fill="none" stroke="currentColor" strokeWidth={1.6} strokeDasharray={auto ? "3 2.5" : undefined} />
    </svg>
  );
}

export function AspectPicker({ value, options, onChange }: { value: string; options: string[]; onChange: (v: string) => void }) {
  return (
    <div className="aspect-picker" role="radiogroup" aria-label="Aspect ratio">
      {options.map((o) => (
        <button key={o} role="radio" aria-checked={value === o} className={`aspect ${value === o ? "on" : ""}`} onClick={() => onChange(o)}
          title={o === "auto" ? "Auto — the model decides the shape from your prompt" : `${o} — sent as explicit pixels`}>
          <AspectIcon ratio={o} />
          <span>{o === "auto" ? "Auto" : o}</span>
        </button>
      ))}
    </div>
  );
}

export type ImgOpts = { aspect?: string; size?: string; output_format?: string; background?: string };

/** Keep only the options the chosen model actually offers; fill the rest from its defaults. */
export function settingsFor(model: any, opts: ImgOpts): Record<string, string> {
  const fields = model?.options?.fields ?? {};
  const defaults = model?.options?.defaults ?? {};
  const out: Record<string, string> = {};
  for (const k of ["size", "output_format", "background"] as const) {
    const allowed: string[] | null = fields[k];
    if (!allowed) continue;
    const want = opts[k];
    if (want && allowed.includes(want)) out[k] = want;
    else if (defaults[k]) out[k] = defaults[k];
  }
  if (out.background === "transparent" && out.output_format !== "png") out.output_format = "png";
  if (opts.aspect && opts.aspect !== "auto" && (fields.aspect ?? []).includes(opts.aspect)) out.aspect = opts.aspect;
  return out;
}

export function ImageOptionsBar({ model, opts, onChange, hasReference }: { model: any; opts: ImgOpts; onChange: (patch: ImgOpts) => void; hasReference: boolean }) {
  const fields = model?.options?.fields ?? {};
  const eff = settingsFor(model, opts);
  const needsAlphaRef = String(model?.options?.transparentRule ?? "").includes("one_png_reference");
  const set = onChange;
  return (
    <div className="img-options">
      <div className="opt-group">
        <span className="opt-label">Ratio</span>
        <AspectPicker value={opts.aspect ?? "auto"} options={fields.aspect ?? ["auto"]} onChange={(aspect) => set({ aspect })} />
      </div>
      <div className="opt-row">
        {fields.size && (
          <div className="opt-group">
            <span className="opt-label">Quality</span>
            <div className="segmented small" role="radiogroup" aria-label="Quality">
              {(fields.size as string[]).map((s) => <button key={s} role="radio" aria-checked={eff.size === s} className={eff.size === s ? "on" : ""} onClick={() => set({ size: s })}>{s}</button>)}
            </div>
          </div>
        )}
        {fields.output_format && (
          <div className="opt-group">
            <span className="opt-label">Format</span>
            <div className="segmented small" role="radiogroup" aria-label="Format">
              {(fields.output_format as string[]).map((s) => <button key={s} role="radio" aria-checked={eff.output_format === s} className={eff.output_format === s ? "on" : ""}
                onClick={() => set({ output_format: s, background: s !== "png" && eff.background === "transparent" ? "opaque" : opts.background })}>{VALUE_LABEL[s] ?? s}</button>)}
            </div>
          </div>
        )}
        {fields.background && (
          <div className="opt-group">
            <span className="opt-label">Background</span>
            <div className="segmented small" role="radiogroup" aria-label="Background">
              {(fields.background as string[]).map((s) => {
                const blocked = s === "transparent" && needsAlphaRef && !hasReference;
                return <button key={s} role="radio" aria-checked={eff.background === s} disabled={blocked} className={eff.background === s ? "on" : ""}
                  title={blocked ? "This model needs one PNG reference with transparency for a transparent result" : s === "transparent" ? "Requires PNG output" : undefined}
                  onClick={() => set({ background: s, output_format: s === "transparent" ? "png" : opts.output_format })}>{VALUE_LABEL[s] ?? s}</button>;
              })}
            </div>
          </div>
        )}
      </div>
    </div>
  );
}
