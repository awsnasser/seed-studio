import { useRef } from "react";
import seedream from "../../docs/assets/feature-seedream.jpg";
import canvas from "../../docs/assets/feature-canvas.jpg";
import seedance from "../../docs/assets/feature-seedance.jpg";

const previews = [
  { image: seedream, description: "Build the frames. Generate and edit with Seedream. Reuse references and keep image versions." },
  { image: canvas, description: "Connect the shots. Import a storyboard and connect prompts, references and generators on the visual canvas." },
  { image: seedance, description: "Make the shot move. Bring frames to life with Seedance 2.5 or Seedance 2.5 Flash." },
];

export default function FeaturePreviews({ onClose }: { onClose: () => void }) {
  const closeButton = useRef<HTMLButtonElement>(null);
  const gallery = useRef<HTMLDivElement>(null);

  return (
    <div className="overlay" onClick={onClose}>
      <div className="dialog feature-previews" role="dialog" aria-modal="true" aria-labelledby="feature-previews-title"
        onClick={(event) => event.stopPropagation()}
        onKeyDown={(event) => {
          if (event.key === "Escape") {
            event.stopPropagation();
            onClose();
          }
          if (event.key === "Tab") {
            event.preventDefault();
            const next = document.activeElement === closeButton.current ? gallery.current : closeButton.current;
            next?.focus();
          }
        }}>
        <div className="dialog-head">
          <h2 id="feature-previews-title">Seed Studio features</h2>
          <button ref={closeButton} className="btn small" autoFocus onClick={onClose}>Close</button>
        </div>
        <p className="muted">Illustrated with AI-generated stills from <em>Dune Survey</em>.</p>
        <div className="feature-previews-gallery" ref={gallery} tabIndex={0} role="region" aria-label="Feature cards">
          {previews.map(({ image, description }) => (
            <img key={image} src={image} alt={description} width={1280} height={720} />
          ))}
        </div>
      </div>
    </div>
  );
}
