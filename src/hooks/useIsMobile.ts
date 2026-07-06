import { useEffect, useState } from "react";

/**
 * True when the viewport is below the Tailwind `lg` breakpoint (1024px) — the
 * phone/mobile layout. Used to switch navigation from inline (desktop) to
 * full-screen overlay (mobile) and to drive mobile-only auto-dismiss behavior
 * (e.g. closing the conversation list after a selection). Kept in sync with the
 * `lg:` responsive classes used throughout the app.
 */
export function useIsMobile(): boolean {
  const [mobile, setMobile] = useState(() =>
    typeof window !== "undefined"
      ? window.matchMedia("(max-width: 1023px)").matches
      : false,
  );
  useEffect(() => {
    const mql = window.matchMedia("(max-width: 1023px)");
    const onChange = () => setMobile(mql.matches);
    onChange();
    mql.addEventListener("change", onChange);
    return () => mql.removeEventListener("change", onChange);
  }, []);
  return mobile;
}

export default useIsMobile;
