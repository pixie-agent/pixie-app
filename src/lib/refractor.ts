/**
 * Curated refractor singleton — the single place syntax-highlighting languages
 * are registered for the whole app.
 *
 * The default `react-syntax-highlighter` `Prism` build imports `refractor/all`,
 * pulling in all ~600 Prism language grammars (≈700 kB) even though this app
 * only previews a few dozen file types. We instead build refractor from
 * `refractor/core` (zero languages) and register just the languages
 * `src/lib/languages.ts` maps to. That collapses the standalone `one-dark`
 * chunk from ~781 kB to a small fraction.
 *
 * `react-syntax-highlighter`'s `PrismLight` build imports the *same*
 * `refractor/core` singleton this module registers on, so the languages
 * registered here are available to both the diff viewer's own tokenizer
 * (`highlight.ts`) and every `<SyntaxHighlighter/>`. Import this module
 * anywhere those are used so registration runs before the first highlight.
 */
import { refractor } from "refractor/core";

import clike from "refractor/clike";
import markup from "refractor/markup";
import css from "refractor/css";
import javascript from "refractor/javascript";
import jsx from "refractor/jsx";
import typescript from "refractor/typescript";
import tsx from "refractor/tsx";
import rust from "refractor/rust";
import python from "refractor/python";
import ruby from "refractor/ruby";
import go from "refractor/go";
import java from "refractor/java";
import kotlin from "refractor/kotlin";
import scala from "refractor/scala";
import swift from "refractor/swift";
import dart from "refractor/dart";
import c from "refractor/c";
import cpp from "refractor/cpp";
import csharp from "refractor/csharp";
import php from "refractor/php";
import sql from "refractor/sql";
import scss from "refractor/scss";
import sass from "refractor/sass";
import less from "refractor/less";
import json from "refractor/json";
import yaml from "refractor/yaml";
import toml from "refractor/toml";
import graphql from "refractor/graphql";
import bash from "refractor/bash";
import powershell from "refractor/powershell";
import markdown from "refractor/markdown";
import docker from "refractor/docker";
import makefile from "refractor/makefile";

// `refractor.register` reads `displayName`/`aliases` off each module, so each
// call also exposes the language's aliases (js, ts, html, xml, cs, …). Bases
// (clike, markup) are listed first; the rest follow in the order they appear in
// `src/lib/languages.ts`. Languages without a refractor grammar (fish, vue,
// svelte) are intentionally absent — `tokenizeSource` and PrismLight fall back
// to plain uncolored text for them via `refractor.registered()`.
for (const lang of [
  clike, markup, css, javascript, jsx, typescript, tsx, rust, python, ruby, go,
  java, kotlin, scala, swift, dart, c, cpp, csharp, php, sql, scss, sass, less,
  json, yaml, toml, graphql, bash, powershell, markdown, docker, makefile,
]) {
  refractor.register(lang);
}

export { refractor };
