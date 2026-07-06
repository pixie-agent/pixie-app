import type { AgentEngineId } from "../types";
import { AGENT_ENGINES } from "../types";

const BUILTIN_ICON = new URL("../assets/engine-icons/builtin.svg", import.meta.url).href;

export default function EngineBadge({
  engine,
  showLabel = false,
  tone = "color",
  className = "",
}: {
  engine: AgentEngineId;
  showLabel?: boolean;
  /** "color" (default) renders the engine's brand-tinted pill; "onAccent"
   *  renders a white/translucent chip for placement on a colored (accent)
   *  background where the brand tint would wash out. */
  tone?: "color" | "onAccent";
  className?: string;
}) {
  const label = AGENT_ENGINES.find((e) => e.id === engine)?.label ?? engine;
  const colors =
    tone === "onAccent"
      ? "bg-white/15 text-white ring-white/25"
      : "bg-violet-500/15 text-violet-200 ring-violet-400/30";
  return (
    <span
      className={`inline-flex items-center gap-0.5 px-1 py-0.5 rounded text-[9px] leading-none ring-1 ${colors} ${className}`}
      title={label}
      aria-label={label}
    >
      <img
        src={BUILTIN_ICON}
        alt=""
        className={tone === "onAccent" ? "w-3 h-3" : "w-2.5 h-2.5"}
        draggable={false}
      />
      {showLabel && <span className="truncate max-w-[120px]">{label}</span>}
    </span>
  );
}
