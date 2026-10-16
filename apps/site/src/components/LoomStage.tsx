import { WeavePoster } from "./WeavePoster";

/** The hero visual: the static weave. */
export function LoomStage() {
  return (
    <div className="relative aspect-[4/3] w-full sm:aspect-[5/2]" data-3d="off">
      <WeavePoster />
    </div>
  );
}
