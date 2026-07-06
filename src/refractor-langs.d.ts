/**
 * Ambient types for refractor's per-language subpath imports.
 *
 * refractor ships .d.ts only for its root, `/core`, and `/all` entry points —
 * not for the ~600 individual language modules under `refractor/<name>`. Each
 * language module is a `Syntax` function (a grammar-setup callback carrying
 * `displayName` + `aliases`) consumed by `refractor.register`. This wildcard
 * declaration fills that typing gap so the curated language registration in
 * `src/lib/refractor.ts` type-checks. Concrete entries (`refractor/core` etc.)
 * still resolve to their own richer declarations.
 */
import type { Syntax } from "refractor/core";

declare module "refractor/*" {
  const syntax: Syntax;
  export default syntax;
}
