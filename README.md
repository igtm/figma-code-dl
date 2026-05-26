# figma-code-dl

[English](README.md) · [日本語](README.ja.md)


Pull a Figma node from the local **Dev Mode MCP** server and turn it into a
clean React+Tailwind `.tsx` file — with inlined-instance replacement, asset
download, and an unmapped-instance histogram for growing your mapping.

## What it does

```
Figma URL → figma-code-dl → src/Foo.tsx
                    + src/Foo/assets/*.png|svg
```

Given a Figma URL, `figma-code-dl`:

1. Connects to the **local** Figma Dev Mode MCP server
   (`http://127.0.0.1:3845/mcp`, no authentication).
2. Calls `get_design_context` with the URL's nodeId (and `forceCode: true`).
3. Extracts the TSX code block, drops a useful header on top.
4. **Replaces inlined Figma instances** with `<Component />` references from
   your codebase, per a `.figma/instance-map.json` you maintain. Injects the
   matching `import` statements at the top.
5. **Optionally substitutes `<img src={imgXxx} ... />` with reusable React
   icon components** (typically SVG components) from a configurable directory
   (`--icons` / `--icons-config`). The `const imgXxx = "..."` declaration is
   removed when safe, and a matching `import` is added.
6. **Downloads referenced assets** (PNG/SVG/JPEG/WebP/GIF) into a local
   directory and rewrites the URLs to relative paths. After the icons pass
   above, only the unmatched URLs are downloaded.
7. **Reports** which `data-name` instances had no mapping yet — useful for
   deciding which design-system component to take in next.
8. **Optionally rewrites bare hex colors as `var(--name, #hex)`** based on
   the Figma Variables that have a `codeSyntax.WEB` code-syntax set
   (`--colors` / `--colors-file`). Variables are dumped into
   `.figma/variables.json` with `--dump-variables` and only exact-hex matches
   are substituted.
9. **Optionally trims redundant Tailwind classes and layer-metadata
   attributes** per a TOML config (`--trim` / `--trim-config`), to keep the
   output small for both humans and LLMs that will later read it.
10. **Optionally captures a PNG screenshot of the node** via MCP
    `get_screenshot` (`--screenshot <path>`). Unlike `get_design_context`,
    this works on `section` nodes too, so it's useful as a quick preview
    when a URL turns out to be a section and the code path can't run.

No authentication, no OAuth, no API keys — the Figma desktop app handles
auth via its existing Figma session, and exposes the MCP endpoint on
loopback only when you opt in.

## Prerequisites

- **Figma desktop app** installed and running
- Open the design file you want to pull from, and make sure it's the
  **active tab** (no need to click/select any specific node inside the file)
- `Figma menu → Preferences → "Enable Dev Mode MCP server"` is **on**
- Sanity check:
  ```bash
  curl -s -o /dev/null -w '%{http_code}\n' \
    -X POST http://127.0.0.1:3845/mcp \
    -H 'Content-Type: application/json' \
    -H 'Accept: application/json, text/event-stream' \
    -d '{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2024-11-05","capabilities":{},"clientInfo":{"name":"probe","version":"0"}}}'
  # 200 → ok
  ```

## Install

### Pre-built binary (Linux / macOS)

Install the latest GitHub release:

```bash
curl -fsSL https://raw.githubusercontent.com/igtm/figma-code-dl/main/install.sh | sh
```

By default the binary is placed in `/usr/local/bin/figma-code-dl` (you may need
`sudo`). Override the install directory:

```bash
curl -fsSL https://raw.githubusercontent.com/igtm/figma-code-dl/main/install.sh \
  | sh -s -- -b=$HOME/.local/bin
```

Pin to a specific release:

```bash
curl -fsSL https://raw.githubusercontent.com/igtm/figma-code-dl/main/install.sh \
  | sh -s -- -v=v0.1.0
```

Supported `os-arch` combinations: `apple-darwin` × `{x86_64, aarch64}`,
`unknown-linux-gnu` × `{x86_64, aarch64}`, plus `x86_64-pc-windows-msvc`
(install on Windows by downloading the archive from the GitHub Releases page
directly).

### From source (Cargo)

From a published GitHub tag:

```bash
cargo install --git https://github.com/igtm/figma-code-dl --locked
```

From a local checkout:

```bash
cargo install --path . --locked
```

Either route puts `figma-code-dl` in `~/.cargo/bin/`.

## Usage

```bash
figma-code-dl <figma-url> --out <path/to/file.tsx> [options]
```

The URL's `fileKey` is informational only — the Dev Mode MCP operates on the
currently active Figma tab, so it's the `node-id` that actually matters.

### Examples

Minimal:

```bash
figma-code-dl 'https://www.figma.com/design/<fileKey>/?node-id=1-2' \
  --out src/pages/MyPage.tsx
```

Full pipeline:

```bash
figma-code-dl 'https://www.figma.com/design/<fileKey>/?node-id=1-2' \
  --out src/pages/MyPage.tsx \
  --map .figma/instance-map.json \
  --download-assets src/pages/assets \
  --report-unmapped
```

Offline (e.g. fetched by some other MCP client and saved as JSON):

```bash
figma-code-dl --from-json /tmp/figma-resp.json \
  --source-url 'https://www.figma.com/design/<fileKey>/?node-id=1-2' \
  --out src/pages/MyPage.tsx \
  --map .figma/instance-map.json \
  --download-assets src/pages/assets
```

### Flags

| Flag | What it does |
|---|---|
| `<url>` (positional) | Figma URL — required unless `--from-json` is given |
| `--out <path>` | Output `.tsx` path (required) |
| `--from-json <path\|->` | Read MCP `content` blocks from a JSON file (`-` = stdin) instead of fetching |
| `--source-url <url>` | URL recorded in the output header (defaults to `--url`) |
| `--mcp-url <url>` | MCP endpoint (default `http://127.0.0.1:3845/mcp`) |
| `--map <path>` | `.figma/instance-map.json` for instance → React component substitution |
| `--download-assets <dir>` | Download referenced PNG/SVG/JPEG/WebP/GIF into this directory; rewrite URLs to relative paths |
| `--report-unmapped` | Print a histogram of inlined-instance `data-name` values that no mapping handled |
| `--trim` | Enable the trim pass using `.figma/config.toml`. Removes redundant Tailwind classes and listed JSX attributes |
| `--trim-config <path>` | Explicit path to a trim config TOML. Implies `--trim` |
| `--icons` | Enable the icon-substitution pass using `.figma/config.toml` (`[icons]` section). Replaces `<img src={imgXxx} />` with reusable React icon components |
| `--icons-config <path>` | Explicit path to an icons config TOML. Implies `--icons` |
| `--colors` | Enable the colors pass. Rewrites bare `[#XXXXXX]` to `[var(--name,#XXX)]` per `.figma/variables.json` |
| `--colors-file <path>` | Explicit path to the variables file. Implies `--colors` |
| `--dump-variables <path>` | Fetch Figma Variables via MCP `get_variable_defs` and write them to this path. Can run standalone (no `--out`) |
| `--screenshot <path>` | Save a PNG of the target node via MCP `get_screenshot`. Works on `section` nodes too. Can run standalone (no `--out`) |
| `--screenshot-contents-only` | Pass `contentsOnly: true` to `get_screenshot` (render the node in isolation, ignoring overlapping canvas content). Default is `false` |

## Instance replacement (`.figma/instance-map.json`)

When `--map` is given, the tool finds the **root** of each inlined Figma
instance in the TSX and substitutes a React component reference. Detection
rule: a JSX element with a `data-name="X"` attribute whose mapping key is `X`,
*and* whose `data-node-id` is "bare" (no `;` — i.e. not a sub-node of an
already-inlined instance).

Schema:

```jsonc
{
  "mappings": {
    "Switch":      { "module": "@/components/ds/Switch",   "export": "Switch" },
    "DropdownBox": { "module": "@/components/ds/Dropdown", "export": "Dropdown", "alias": "Dropdown" },
    "Button":      { "module": "@/components/ds/Button",   "export": "default" },
    "system/checklist": { "module": "@/icons/Checklist",   "export": "default" }
  },
  "byNodeId": {
    "1:2": { "module": "@/components/PageHeader", "export": "default" }
  }
}
```

- Keys are Figma layer names (`data-name` values), exact match.
- `export: "default"` + no `alias` → local binding is the sanitised PascalCase
  of the layer name (e.g. `system/checklist` → `SystemChecklist`).
- Multiple occurrences of the same name are all substituted; a warning is
  printed to stderr. Use `byNodeId` to disambiguate.
- The tool also detects server-extracted helper components like
  `function Button({ className }) { ... }` and, if the function name matches a
  mapping key, removes the declaration so the injected import binds the same
  identifier its existing call sites already use.

## Output size reduction

`figma-code-dl` produces output that's read by both humans and LLMs, so size
matters. Three independent passes can shrink it (or, in the icons case,
realign it onto your existing components):

- **`--map`** — substitutes inlined Figma instances with `<Component />`
  references; the entire subtree of each replaced instance disappears.
- **`--icons`** — substitutes `<img src={imgXxx} />` with reusable React
  icon components (typically SVG) from a configured directory or explicit
  overrides; removes the matching `const imgXxx = "..."` declarations.
- **`--colors`** — rewrites bare `[#XXXXXX]` color tokens in `className`
  attributes to `[var(--name,#XXX)]` references, using
  `.figma/variables.json` as the source of truth. Only colors whose hex
  exactly matches a Figma Variable with a `codeSyntax.WEB` set are
  rewritten. The file is auto-generated by `--dump-variables`.
- **`--trim`** — drops redundant Tailwind classes (per prefix/exact rules)
  and listed JSX attributes per a TOML config (typically `.figma/config.toml`).

On a real Figma screen we tested (a single mid-sized page with repeated row
components and a navigation), the raw extracted TSX was roughly 100 KB /
~960 lines. Applying combinations of the three passes:

| Output | Bytes | vs. raw | Lines |
|---|---:|---:|---:|
| (no passes) | 101,846 | 100.0%  (baseline) | 960 |
| `--map` | 47,608 | 46.7%  (−53.3%) | 466 |
| `--trim` | 66,968 | 65.7%  (−34.3%) | 960 |
| `--icons` | 99,725 | 97.9%  (−2.1%) | 960 |
| `--map` + `--trim` | 32,877 | 32.3%  (−67.7%) | 466 |
| `--map` + `--icons` | 46,440 | 45.6%  (−54.4%) | 466 |
| `--map` + `--icons` + `--trim` | 31,709 | 31.1%  (−68.9%) | 466 |
| `--map` + `--icons` + `--colors` + `--trim` | **32,138** | **31.6%  (−68.4%)** | **466** |

The passes are **complementary but not strictly additive**: `--map` collapses
entire instance subtrees, which removes a lot of what `--icons` and `--trim`
would otherwise have targeted.

A few notes:

- `--map` did the heavy lifting on this sample (−54 KB by replacing 16
  inlined instances with imports).
- `--trim` removed ~35 KB by dropping ~1,400 redundant Tailwind class tokens,
  ~570 layer-metadata attributes, and ~80 now-empty `className=""` attributes.
- `--icons` is **roughly size-neutral on its own** but is the pass that
  matters most for *code quality*: it replaces ~50 inline `<img>` references
  with the design system's actual icon components and removes the matching
  `const imgXxx = "..."` URL declarations, so the output uses your reusable
  components instead of an opaque URL.
- `--colors` typically adds bytes (~+1%) because the `var(...)` wrapper is
  longer than a bare hex, but the value is **consistency**: the same hex
  becomes the same variable name everywhere in the output, so search /
  theming / dark-mode work uniformly. On this sample it rewrote 22 bare-hex
  occurrences (5 distinct colors) — the `bg-[#def4f2]` form is now gone.

Your mileage will vary with the screen — denser layouts with more repeated
DS components save more from `--map`; icon-heavy navigation/list screens
benefit more from `--icons`; flatter, less component-ised designs save
proportionally more from `--trim`.

### Configuring `--colors`

The colors pass reads `.figma/variables.json` — a file you generate from
Figma itself, not a hand-edited config:

```bash
figma-code-dl <figma-url> --dump-variables .figma/variables.json
```

This calls the MCP `get_variable_defs` tool, extracts every color variable
along with its `codeSyntax.WEB` value (the CSS variable name you registered
on the Figma side, e.g. `--blue-100`), and writes:

```json
{
  "$comment": "Auto-generated. Order matters: when multiple variables share a hex, the first wins.",
  "generated_at": "2026-05-25T10:30:00Z",
  "variables": [
    { "css": "--blue-100",   "figma_name": "color/semantic/background/green", "hex": "#DEF4F2" },
    { "css": "--text-main",  "figma_name": "color/semantic/text/main",        "hex": "#3D4047" },
    { "css": null,           "figma_name": "black/black_60",                  "hex": "#6E7075" }
  ]
}
```

The raw MCP response is also saved alongside as `.figma/variables.raw.json`
for inspection. Variables without a `codeSyntax.WEB` (`css: null`) are
saved for reference but are skipped during substitution.

Subsequent runs use `--colors`:

```bash
figma-code-dl <figma-url> --out src/page.tsx --colors
```

For every `[#XXXXXX]` in the output (3-digit hex also normalized), if the
hex exactly matches a variable with a non-null `css`, the token is rewritten
as `[var(--name,#XXX)]`. Other hexes are left untouched.

### Configuring `--trim` and `--icons`

Drop a `.figma/config.toml` at the repository root. Both sections are
independent; either, both, or neither pass can be enabled per-invocation.

```toml
[trim]
# Class names removed when they appear as a whole token.
exclude_exact = [
  "relative", "absolute", "block", "shrink-0",
  "content-stretch",    # Figma-internal marker, not a real Tailwind class
  "max-w-none", "size-full",
]

# Class names removed when they start with any of these prefixes.
# Catches arbitrary-value variants (e.g. `inset-[12.5%]`, `mask-position-[-3px_-3px]`).
exclude_prefixes = ["inset-", "mask-"]

# When trimming leaves `className=""`, drop the whole attribute.
drop_empty_classname = true

# JSX attributes removed unconditionally.
strip_attributes = ["data-node-id", "data-name"]


[icons]
# Filesystem directory scanned for existing icon components (*.tsx). Each
# *.tsx file's PascalCase stem is a candidate; the `imgXxx` const is matched
# by stripping the `img` prefix.
component_dir = "src/components/icons"

# Module specifier used in generated imports. Combined with the matched
# component name as `<module>/<ComponentName>`.
module = "@/components/icons"

# "named" (default) or "default" — affects the shape of the emitted import.
default_export = "named"

# If `ChevronForward1` isn't found, retry as `ChevronForward`. Useful because
# Figma renders the same icon used twice as `imgChevronForward` and
# `imgChevronForward1`.
strip_trailing_digits = true

# Forward className from the original <img> to the icon component.
forward_classname = true

# Explicit overrides take priority over auto-scan. Either form is accepted:
[icons.overrides]
# imgChevronForward1 = "ChevronForward"
# imgLogo = { name = "Logo", module = "@/branding", export = "default" }
```

`--trim` / `--icons` read `.figma/config.toml` by default; pass
`--trim-config <path>` / `--icons-config <path>` for per-project overrides.

## Output shape

```tsx
// Auto-generated by figma-code-dl
// Source: <url>
// file=… node=…
// NOTE: image URLs … 7-day TTL …
// Styles digest (from get_design_context):
//   Heading/Bold-18: Font(…), …

import { Switch } from "@/components/ds/Switch";
import Button from "@/components/ds/Button";

const imgImage145 = "./assets/image-145.png";
…

export default function NodeName() { … }
```

`import React` is not emitted (the output assumes the React auto JSX runtime).

## Asset handling details

When `--download-assets <dir>` is given:

- The absolute path is passed to the MCP server as `dirForAssetWrites`, so
  Figma writes assets directly (depending on Figma's "Image source" setting).
- The tool then walks the `const imgFoo = "..."` declarations in the output,
  fetches each unique URL, and saves it under
  `<dir>/<kebab-from-imgFoo>.<ext>`. Localhost URLs encode the extension
  (`<hash>.png`); cloud URLs are sniffed by Content-Type / magic bytes.
- The URLs in the code are then rewritten to relative paths.

## Limitations / known issues

- Section nodes don't return code from the MCP — only metadata pointing at
  child frames. Re-fetch on a specific child frame. (Tip: `--screenshot` still
  works on sections, so you can use it standalone to preview what a section
  URL points at without recursing.)
- Cloud-style asset URLs (`https://www.figma.com/api/mcp/asset/<uuid>`,
  produced when `--from-json` is used with a cloud MCP capture) expire after
  7 days; use `--download-assets` for anything you intend to keep.
- Localhost asset URLs only work while Figma desktop is running.
- Layer-name collisions in Figma (e.g. two distinct things named `header`)
  cause all occurrences to be substituted with the same mapping. Use
  `byNodeId` to override case-by-case.
- `data-name` and `data-node-id` attributes are kept in the output by default
  because the replacement logic relies on them. Strip them with `--trim`
  (after `--map` has consumed them) or in a separate pass before production.
- `preserveChildren` and `options.passClassName` in the mapping schema are
  parsed but not yet honoured.

## Development

```bash
cargo test                                # unit + integration tests
cargo test --test asset_download -- --ignored   # real-network asset DL test
```

Module layout:

| File | Responsibility |
|---|---|
| `src/main.rs` | CLI args + orchestration |
| `src/mcp.rs` | MCP client over Streamable HTTP transport (no auth) |
| `src/figma_url.rs` | Parse Figma URLs into `(fileKey, nodeId)` |
| `src/extract.rs` | Pull the TSX code block out of the MCP response; build the header; defines `ContentBlock` |
| `src/instance_map.rs` | Load + validate `.figma/instance-map.json` |
| `src/replace.rs` | JSX subtree replacement, server-extracted-function removal |
| `src/icons.rs` | `<img src={imgXxx} />` → `<Component />` substitution, const-decl pruning |
| `src/imports.rs` | Shared `import { ... } from "..."` emission helper used by replace + icons |
| `src/assets.rs` | Parallel asset download (cloud or localhost URL) with extension handling and URL rewriting |
| `src/colors.rs` | `[#XXXXXX]` → `[var(--name,#XXX)]` substitution driven by `.figma/variables.json` |
| `src/variables_dump.rs` | Fetches Figma color Variables via MCP `get_variable_defs` and writes the JSON used by `colors.rs` |
| `src/trim.rs` | Token-saving pass: drop redundant Tailwind classes and listed JSX attributes per `.figma/config.toml` |

Skills used by Claude Code live under `skills/`:

- [`figma-to-code`](skills/figma-to-code/SKILL.md) — single end-to-end
  workflow: bootstrap/maintain `FIGMA.md` (file & page map + code-mapping
  interpretation), run `figma-code-dl` with `--map` / `--trim` for
  token-efficient extraction, take any newly discovered components into the
  design system via `class-variance-authority`, and register them in
  `.figma/instance-map.json`.
