// Generates the developer docs site from ../dev-docs/*.md into docs/ (one page per file, served
// at /docs/), which Vite then builds with the brand stylesheet. Run before `vite build`:
//
//     node scripts/docs.mjs
//
// Links between docs pages become .html links. Links to files outside dev-docs (the design,
// the source) aren't on the site, so they are kept as their text, marked as repository paths.
import { mkdirSync, readFileSync, readdirSync, rmSync, statSync, writeFileSync } from "node:fs";
import { dirname, join, posix, relative, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { Marked } from "marked";
import { gfmHeadingId } from "marked-gfm-heading-id";

const web = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const src = resolve(web, "../dev-docs");
const out = resolve(web, "docs");

const files = (function walk(dir) {
  return readdirSync(dir).flatMap((f) => {
    const p = join(dir, f);
    return statSync(p).isDirectory() ? walk(p) : f.endsWith(".md") ? [relative(src, p)] : [];
  });
})(src).sort();

const pageOf = (md) => (md === "README.md" ? "index.html" : md.replace(/\.md$/, ".html"));
const esc = (s) => s.replace(/&/g, "&amp;").replace(/</g, "&lt;").replace(/>/g, "&gt;").replace(/"/g, "&quot;");

// The sidebar: README's "## Section" headings and the [Title](page.md) links in their tables.
const nav = [];
for (const line of readFileSync(join(src, "README.md"), "utf8").split("\n")) {
  const h = line.match(/^## (.+)/);
  if (h) nav.push({ title: h[1], items: [] });
  const l = line.match(/^\| \[([^\]]+)\]\(([^)#]+\.md)\)/);
  if (l && nav.length) nav.at(-1).items.push({ title: l[1], md: l[2] });
}

function render(md) {
  const here = posix.dirname(md);
  const marked = new Marked(gfmHeadingId());
  marked.use({
    renderer: {
      link({ href, title, tokens }) {
        const text = this.parser.parseInline(tokens);
        if (/^(https?:|mailto:|#|\/)/.test(href)) return `<a href="${esc(href)}"${title ? ` title="${esc(title)}"` : ""}>${text}</a>`;
        const [path, anchor] = href.split("#");
        const target = posix.normalize(posix.join(here, path));
        if (target.startsWith("..") || !target.endsWith(".md")) {
          // Outside the docs: the repository path, not a link.
          const repo = posix.normalize(posix.join("dev-docs", here, path));
          return `<span class="repo" title="In the ZOL repository: ${esc(repo)}">${text}</span>`;
        }
        const rel = posix.relative(here, pageOf(target)) || pageOf(target);
        return `<a href="${esc(rel)}${anchor ? `#${esc(anchor)}` : ""}">${text}</a>`;
      },
      code({ text, lang }) {
        if (lang === "mermaid") return `<pre class="mermaid">${esc(text)}</pre>`;
        return `<pre><code${lang ? ` class="lang-${esc(lang)}"` : ""}>${esc(text)}</code></pre>`;
      },
      table(token) {
        // Wide tables scroll inside their own box on narrow screens.
        const head = token.header.map((c) => `<th>${this.parser.parseInline(c.tokens)}</th>`).join("");
        const rows = token.rows.map((r) => `<tr>${r.map((c) => `<td>${this.parser.parseInline(c.tokens)}</td>`).join("")}</tr>`).join("");
        return `<div class="table"><table><thead><tr>${head}</tr></thead><tbody>${rows}</tbody></table></div>`;
      },
    },
  });
  return marked.parse(readFileSync(join(src, md), "utf8"));
}

const logo = readFileSync(join(web, "index.html"), "utf8").match(/<svg viewBox="0 0 67\.5 23"[\s\S]*?<\/svg>/)[0];

rmSync(out, { recursive: true, force: true });
for (const md of files) {
  const body = render(md);
  const title = (body.match(/<h1[^>]*>(.*?)<\/h1>/)?.[1] ?? "Docs").replace(/<[^>]+>/g, "");
  const page = pageOf(md);
  const link = (target) => posix.relative(posix.dirname(page), target) || target;
  const sidebar = nav
    .filter((s) => s.items.length)
    .map((s) => `<p>${esc(s.title)}</p><ul>${s.items.map((i) => `<li><a href="${link(pageOf(i.md))}"${pageOf(i.md) === page ? ' aria-current="page"' : ""}>${esc(i.title)}</a></li>`).join("")}</ul>`)
    .join("");
  const html = `<!doctype html>
<!-- Generated from dev-docs/${md} by scripts/docs.mjs; edit the markdown. -->
<html lang="en">
<head>
  <meta charset="utf-8" />
  <meta name="viewport" content="width=device-width, initial-scale=1" />
  <title>${esc(title)} · ZOL docs</title>
  <link rel="icon" href="/favicon.svg" type="image/svg+xml" />
  <link rel="stylesheet" href="/src/brand.css" />
  <link rel="stylesheet" href="/src/docs.css" />
</head>
<body class="docs">
  <header class="docs-top">
    <a class="logo" href="/" aria-label="ZOL home">${logo}</a>
    <a class="docs-name" href="${link("index.html")}">Docs</a>
    <nav aria-label="Site"><a href="/">Home</a><a href="/play.html">Play on testnet</a></nav>
  </header>
  <div class="docs-body">
    <details class="docs-nav" open>
      <summary>Contents</summary>
      <nav aria-label="Docs"><ul><li><a href="${link("index.html")}"${page === "index.html" ? ' aria-current="page"' : ""}>Introduction</a></li></ul>${sidebar}</nav>
    </details>
    <main class="prose">
${body}
    </main>
  </div>
  <script type="module" src="/src/docs.ts"></script>
</body>
</html>
`;
  mkdirSync(dirname(join(out, page)), { recursive: true });
  writeFileSync(join(out, page), html);
}
console.log(`wrote ${files.length} pages to docs/`);
