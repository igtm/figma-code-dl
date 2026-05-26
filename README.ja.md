# figma-code-dl

[English](README.md) · [日本語](README.ja.md)

ローカルの **Figma Dev Mode MCP** server から Figma ノードを取得し、クリーンな
React+Tailwind の `.tsx` ファイルに整形して書き出す CLI。inline 展開された
instance を既存 component への参照に置換、画像のローカル DL、未マップ component
の頻度レポート、生成コードの token 数を抑える各種後処理パスまで内蔵。

## 何ができるか

```
Figma URL → figma-code-dl → src/Foo.tsx
                    + src/Foo/assets/*.png|svg
```

Figma URL を渡すと `figma-code-dl` は：

1. **ローカル** の Figma Dev Mode MCP server (`http://127.0.0.1:3845/mcp`、認証なし) に接続。
2. URL の nodeId と `forceCode: true` で `get_design_context` を呼ぶ。
3. TSX のコードブロックを抜き出し、有用なヘッダコメントを付ける。
4. **inline 展開された Figma instance** を、自分で管理する `.figma/instance-map.json`
   に従って `<Component />` 参照に置換。対応する `import` 文を先頭に注入。
5. （任意）**`<img src={imgXxx} ... />` を再利用可能な React アイコンコンポーネントに置換**
   （`--icons` / `--icons-config`）。設定 directory か明示 override で解決。
   置換できた `const imgXxx = "..."` 宣言は安全な範囲で削除し、対応する `import` を追加。
6. **参照されている asset を DL**（PNG/SVG/JPEG/WebP/GIF）し、URL を相対パスに書き換え。
   icons pass 後の URL のみが対象なので、置換済みのアセットは DL されない。
7. **未マップの `data-name` 一覧をレポート**。次に DS に取り込むべき component を決める材料。
8. （任意）**bare hex color → `var(--name, #hex)`**：Figma Variables のうち
   `codeSyntax.WEB` が設定されているものについて、hex 完全一致で書き換え
   （`--colors` / `--colors-file`）。`--dump-variables` で `.figma/variables.json` を生成。
9. （任意）**冗長な Tailwind class や Figma 由来の layer メタ属性を削る**
   （`--trim` / `--trim-config`）。LLM に読ませる時のトークン消費を抑える。
10. （任意）**ノードの PNG スクリーンショットを取得**（`--screenshot <path>`）。
    MCP `get_screenshot` 経由なので `section` ノードでも動く — `get_design_context`
    が section で失敗する場面でも、何が映る画面なのかを 1 枚の PNG で確認できる。
11. **対象 Figma ファイルを自動アクティブ化**：macOS では MCP 呼び出し前に
    `open -a "Figma" <url>` を実行し、Dev Mode MCP（アクティブタブに対して
    動く）が正しいファイルを見るようにする。ユーザーが別ファイルを開いていても
    勝手に切り替わる。無効化は `--no-activate`。

OAuth / API キー / 認証情報は不要。Figma デスクトップアプリの既存セッションが
そのまま使われ、MCP endpoint はオプトインで loopback にだけ露出される。

## 前提

- **Figma デスクトップアプリ** が起動している
- macOS なら `figma-code-dl` が MCP 呼び出し前に自動で `open -a "Figma" <url>` を
  実行して対象タブを前面に出すので、手動でタブを切り替える必要はない。それ以外の
  OS では取り込みたいファイルを **アクティブタブ** にしてから CLI を実行する。
- `Figma メニュー → Preferences → "Enable Dev Mode MCP server"` を **ON**
- 疎通確認：
  ```bash
  curl -s -o /dev/null -w '%{http_code}\n' \
    -X POST http://127.0.0.1:3845/mcp \
    -H 'Content-Type: application/json' \
    -H 'Accept: application/json, text/event-stream' \
    -d '{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2024-11-05","capabilities":{},"clientInfo":{"name":"probe","version":"0"}}}'
  # 200 → OK
  ```

## インストール

### ビルド済みバイナリ (Linux / macOS)

最新リリースを取得：

```bash
curl -fsSL https://raw.githubusercontent.com/igtm/figma-code-dl/main/install.sh | sh
```

既定では `/usr/local/bin/figma-code-dl` に配置されます（`sudo` が必要な場合あり）。
インストール先を変えるとき：

```bash
curl -fsSL https://raw.githubusercontent.com/igtm/figma-code-dl/main/install.sh \
  | sh -s -- -b=$HOME/.local/bin
```

バージョン固定：

```bash
curl -fsSL https://raw.githubusercontent.com/igtm/figma-code-dl/main/install.sh \
  | sh -s -- -v=v0.0.1
```

対応 `os-arch`：`apple-darwin` × `{x86_64, aarch64}`、`unknown-linux-gnu` × `{x86_64, aarch64}`、
`x86_64-pc-windows-msvc`（Windows は GitHub Releases ページからアーカイブを直接 DL）。

### ソースから (Cargo)

公開タグから：

```bash
cargo install --git https://github.com/igtm/figma-code-dl --locked
```

ローカルチェックアウトから：

```bash
cargo install --path . --locked
```

どちらでも `~/.cargo/bin/figma-code-dl` に入ります。

## 使い方

```bash
figma-code-dl <figma-url> --out <path/to/file.tsx> [options]
```

URL の `fileKey` は記録用としか使われません — Dev Mode MCP はアクティブな Figma
タブに対して動くので、効くのは `node-id` のみ。

### 例

最小：

```bash
figma-code-dl 'https://www.figma.com/design/<fileKey>/?node-id=1-2' \
  --out src/pages/MyPage.tsx
```

フル pipeline：

```bash
figma-code-dl 'https://www.figma.com/design/<fileKey>/?node-id=1-2' \
  --out src/pages/MyPage.tsx \
  --map .figma/instance-map.json \
  --download-assets src/pages/assets \
  --report-unmapped
```

オフライン（別の MCP client が fetch 済みの JSON を流し込む）：

```bash
figma-code-dl --from-json /tmp/figma-resp.json \
  --source-url 'https://www.figma.com/design/<fileKey>/?node-id=1-2' \
  --out src/pages/MyPage.tsx \
  --map .figma/instance-map.json \
  --download-assets src/pages/assets
```

### フラグ

| フラグ | 何をするか |
|---|---|
| `<url>` (positional) | Figma URL — `--from-json` 指定時を除き必須 |
| `--out <path>` | 出力 `.tsx` パス（`--dump-variables` 単独時を除き必須） |
| `--from-json <path\|->` | MCP `content` ブロックを fetch せず JSON ファイル（`-` で stdin）から読む |
| `--source-url <url>` | 出力ヘッダに記録する URL（既定 `--url` の値） |
| `--mcp-url <url>` | MCP エンドポイント（既定 `http://127.0.0.1:3845/mcp`） |
| `--map <path>` | `.figma/instance-map.json` で instance → React component 置換 |
| `--download-assets <dir>` | 参照アセット（PNG/SVG/JPEG/WebP/GIF）をこの directory に DL、URL を相対パスに書換 |
| `--report-unmapped` | 未マップ `data-name` の頻度を stderr に表示 |
| `--trim` | `.figma/config.toml` を使う trim pass を有効化（冗長 Tailwind class & JSX 属性を削除） |
| `--trim-config <path>` | trim config TOML のパスを明示指定。`--trim` を含意 |
| `--icons` | `.figma/config.toml` (`[icons]`) を使う icons pass を有効化（`<img src={imgXxx} />` → React component） |
| `--icons-config <path>` | icons config TOML のパスを明示指定。`--icons` を含意 |
| `--colors` | colors pass を有効化。`.figma/variables.json` の codeSyntax.WEB を使い bare `[#XXXXXX]` を `[var(--name,#XXX)]` に書換 |
| `--colors-file <path>` | variables ファイルのパスを明示指定。`--colors` を含意 |
| `--dump-variables <path>` | MCP `get_variable_defs` で Figma Variables を取得しこのパスに書き出す。`--out` 無しでも単独実行可 |
| `--screenshot <path>` | MCP `get_screenshot` で対象ノードの PNG を保存。`section` ノードでも動く。`--out` 無しでも単独実行可 |
| `--screenshot-contents-only` | `get_screenshot` に `contentsOnly: true` を渡す（重なっているキャンバス上の他要素を含めず、ノード単体で render）。既定は `false` |
| `--no-activate` | MCP 呼出前の Figma タブ自動アクティブ化を無効化（macOS 既定 ON）。既に手動でアクティブ化済みで focus を奪われたくないとき用 |
| `--activate-wait-ms <ms>` | `open -a Figma <url>` 後、probe ループ前の固定 sleep。既定 `0`（poll するので不要）。Figma が極端に遅い環境で probe 回数を減らしたいときだけ上げる |
| `--mcp-retry-attempts <n>` | MCP tool call が "No node could be found"（タブ未切替）を返したときの最大試行回数。既定 `10` |
| `--mcp-retry-interval-ms <ms>` | 試行間隔（ミリ秒）。既定 `300` → 最大 ~3 秒まで Figma のタブ切替遅延を許容 |

## Instance 置換 (`.figma/instance-map.json`)

`--map` を渡すと、TSX 中の inline 展開された Figma instance の **root** を見つけ
React component 参照に差し替えます。検出ルール：`data-name="X"` 属性を持つ JSX 要素で、
key が `X` に一致、かつ `data-node-id` が "bare"（`;` を含まない、つまり既に inline
化された instance のサブノードではない）。

スキーマ：

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

- キーは Figma レイヤー名（`data-name` の値）と **完全一致**。
- `export: "default"` + `alias` 未指定 → ローカル束縛は layer 名を sanitize した PascalCase
  （例：`system/checklist` → `SystemChecklist`）。
- 同名 instance が複数あれば全部置換 + stderr に警告。個別オーバーライドは `byNodeId`。
- サーバが自動 extract した `function Button({ className }) { ... }` のような関数も検出し、
  関数名が mapping のキーに一致すれば宣言を削除（既存の `<Foo />` 呼び出しは注入された
  import で resolve）。

## 出力サイズの削減

`figma-code-dl` の出力は人間にも LLM にも読まれるのでサイズが重要。
独立した 3 つの pass で縮められます（icons は「サイズ削減」というより
「既存コンポーネントへの寄せ替え」）：

- **`--map`** — inline 展開された Figma instance を `<Component />` 参照に置換。
  置換された instance のサブツリーは丸ごと消える。
- **`--icons`** — `<img src={imgXxx} />` を、設定 directory か明示 override で
  見つけた React アイコンコンポーネント（多くは SVG）に置換。対応する
  `const imgXxx = "..."` 宣言も削除。
- **`--colors`** — `className` 内の bare `[#XXXXXX]` を `[var(--name,#XXX)]` に
  書き換え。`.figma/variables.json` を一次情報とし、codeSyntax.WEB が設定されている
  Figma Variable と hex 完全一致するものだけ。ファイルは `--dump-variables` で自動生成。
- **`--trim`** — 冗長な Tailwind class（prefix / exact ルール）と指定 JSX 属性を削除。
  `.figma/config.toml` で設定。

実際の Figma 画面（中規模のページ。繰り返しの行コンポーネントとサイドナビあり）で
試したところ、生 TSX は 100 KB / ~960 行：

| 適用 | バイト | vs 元 | 行 |
|---|---:|---:|---:|
| (なし) | 101,846 | 100.0%（基準） | 960 |
| `--map` | 47,608 | 46.7% (−53.3%) | 466 |
| `--trim` | 66,968 | 65.7% (−34.3%) | 960 |
| `--icons` | 99,725 | 97.9% (−2.1%) | 960 |
| `--map` + `--trim` | 32,877 | 32.3% (−67.7%) | 466 |
| `--map` + `--icons` | 46,440 | 45.6% (−54.4%) | 466 |
| `--map` + `--icons` + `--trim` | 31,709 | 31.1% (−68.9%) | 466 |
| `--map` + `--icons` + `--colors` + `--trim` | **32,138** | **31.6% (−68.4%)** | **466** |

3 pass は **補完的だが単純加算ではない**：`--map` が instance サブツリーを丸ごと
畳むので、`--icons` や `--trim` の対象が前もって減る。

メモ：

- `--map` がこのサンプルでは最大の効き（16 instance を import に置き換えて −54 KB）。
- `--trim` は ~1,400 個の冗長 class、~570 個の layer メタ属性、~80 個の空 `className=""`
  を削って −35 KB。
- `--icons` は **単独ではほぼサイズ中立** だが **コード品質に直結する pass**：
  ~50 個の inline `<img>` を DS のアイコン component に置き換え、対応する
  `const imgXxx = "..."` 宣言も削除。結果として出力が「不透明な URL」ではなく
  「再利用可能な component」を参照する形になる。
- `--colors` は通常バイト数が増える（+~1%。`var(...)` ラッパーの分）が、価値は **整合性**：
  同じ hex に同じ変数名が一貫して出るので、grep / theming / dark-mode が均一に効く。
  このサンプルでは 22 箇所の bare hex（5 色）を書き換えて、`bg-[#def4f2]` のような
  形式は output から消滅。

画面によって最適な組合せは変わります — instance が多く詰まった画面は `--map` が、
アイコンが多いナビや一覧は `--icons` が、フラットで component 化されていない画面は
`--trim` が相対的に効きます。

### `--colors` の設定

colors pass は `.figma/variables.json` を読みます。これは手書きじゃなく **Figma から
自動生成** するファイル：

```bash
figma-code-dl <figma-url> --dump-variables .figma/variables.json
```

MCP の `get_variable_defs` を呼んで色 variable を抽出、各 variable の `codeSyntax.WEB`
（Figma 側で登録した CSS 変数名、例：`--blue-100`）と一緒に書き出します：

```json
{
  "$comment": "Auto-generated. 同じ hex が複数 variable にあるときは先頭が勝つ。",
  "generated_at": "2026-05-25T10:30:00Z",
  "variables": [
    { "css": "--blue-100",   "figma_name": "color/semantic/background/green", "hex": "#DEF4F2" },
    { "css": "--text-main",  "figma_name": "color/semantic/text/main",        "hex": "#3D4047" },
    { "css": null,           "figma_name": "black/black_60",                  "hex": "#6E7075" }
  ]
}
```

生の MCP レスポンスも `.figma/variables.raw.json` として隣に保存されるので、
parser がうまく扱えていなければそれを見て調整できます。`codeSyntax.WEB` 未設定
（`css: null`）の variable は参考情報として残しますが、置換対象にはなりません。

以降の運用では `--colors` を付けるだけ：

```bash
figma-code-dl <figma-url> --out src/page.tsx --colors
```

出力中のすべての `[#XXXXXX]`（3 桁 hex も自動で 6 桁化）を、`css` が null じゃない
variable と hex 完全一致するときに `[var(--name,#XXX)]` に書き換えます。一致しない
hex は触りません。

### `--trim` と `--icons` の設定

リポジトリルートに `.figma/config.toml` を置きます。`[trim]` と `[icons]` は独立で、
どちらか・両方・どちらも無し、を CLI フラグで選べます。

```toml
[trim]
# token 全体一致で削除する class 名
exclude_exact = [
  "relative", "absolute", "block", "shrink-0",
  "content-stretch",    # Figma 内部マーカー。Tailwind に実在しない
  "max-w-none", "size-full",
]

# 前方一致で削除する class 名
# 任意値バリアント (`inset-[12.5%]`, `mask-position-[-3px_-3px]` 等) も拾う
exclude_prefixes = ["inset-", "mask-"]

# trim で `className=""` だけ残ったら属性ごと削除
drop_empty_classname = true

# 無条件で削除する JSX 属性
strip_attributes = ["data-node-id", "data-name"]


[icons]
# 既存アイコン component (*.tsx) をスキャンする filesystem 上の directory。
# 各 *.tsx の stem (PascalCase) が候補名。`imgXxx` const 名から `img` を剥がして照合。
component_dir = "src/components/icons"

# 生成 import の module 部分。component 名と合わせて `<module>/<ComponentName>` になる。
module = "@/components/icons"

# "named" (既定) または "default"
default_export = "named"

# `ChevronForward1` が見つからなければ `ChevronForward` で再試行。
# 同じアイコンが 2 箇所で使われると Figma は `imgChevronForward` と
# `imgChevronForward1` の 2 つを出すため。
strip_trailing_digits = true

# 元の <img> の className を component の className として渡す
forward_classname = true

# 残った `const imgXxx = "./assets/foo.svg"` を
# `import Xxx from "./assets/foo.svg"` + `<img>` → `<Xxx />` に変換する。
# `--download-assets` の後、URL がローカルパスのときだけ有効。
# プロジェクトのバンドラが SVG の default import から React component を返す前提
# (Vite + svgr, webpack + @svgr/webpack 等)。
local_svg_import = true

# 明示オーバーライドは自動スキャンより優先。書式は 2 通り：
[icons.overrides]
# imgChevronForward1 = "ChevronForward"
# imgLogo = { name = "Logo", module = "@/branding", export = "default" }
```

`--trim` / `--icons` は既定で `.figma/config.toml` を読みます。プロジェクトごとに
別パスを使いたければ `--trim-config <path>` / `--icons-config <path>` を指定。

## 出力の形

```tsx
// Auto-generated by figma-code-dl
// Source: <url>
// file=… node=…
// NOTE: image URLs … 7-day TTL …
// Styles digest (from get_design_context):
//   Heading/Bold-18: Font(…), …

// NOTE: figma-code-dl auto-generated the imports below from Figma layer /
// asset names. ...
import { Switch } from "@/components/ds/Switch";
import Button from "@/components/ds/Button";

// NOTE: The asset files below were downloaded directly from Figma. They
// may duplicate assets already in this codebase — ...
const imgImage145 = "./assets/image-145.png";
…

export default function NodeName() { … }
```

`import React` は付きません（React の auto JSX runtime 前提）。

import や `const imgXxx = "./assets/..."` のブロック頭には自動でコメントが付き、
「これらは推測で生成された名前なので、既存の component / asset があればそちらを
使うように」という案内が入ります（人間にも LLM にも）。

## Asset 取り扱いの詳細

`--download-assets <dir>` を渡すと：

- 絶対パスを `dirForAssetWrites` として MCP server に渡す → Figma が直接そのフォルダに書く
  （Figma の "Image source" 設定による）。
- ツール側でも生成コードの `const imgFoo = "..."` をなぞって URL を fetch、
  `<dir>/<imgFoo を kebab 化>.<ext>` に保存。localhost URL は拡張子を URL に含む
  (`<hash>.png`)、cloud URL は Content-Type / magic bytes で判別。
- コード中の URL は相対パスに書き換え。

## 制限事項 / 既知の問題

- Section ノードは MCP からコードを返さない（子フレーム指示のメタデータだけ）。
  子フレームの node id で叩き直してください。（Tip: `--screenshot` は section でも
  動くので、URL が section だと判明したときの「何が映る画面か」確認用に使える。）
- Cloud 系 asset URL (`https://www.figma.com/api/mcp/asset/<uuid>`、`--from-json` で
  cloud MCP capture を使うと出る) は 7 日 TTL。長期保存するなら `--download-assets`。
- Localhost asset URL は Figma デスクトップ起動中だけ有効。
- Figma 内の layer 名衝突 (例：`header` が 2 つある等) は同じ mapping で全部置換 +
  警告。個別差別化したい場合は `byNodeId` で上書き。
- `data-name` / `data-node-id` 属性は既定で残します（置換ロジックが依存しているため）。
  `--map` 適用後に `--trim` で剥がすか、本番投入前に別 pass で剥がしてください。
- mapping schema の `preserveChildren` と `options.passClassName` はパース対象だが
  まだ動作未実装。
- 公式 Dev Mode MCP の `get_variable_defs` は variable の `codeSyntax.WEB` を
  返しません。`--colors` は variable のフルパス名から CSS 名を fallback 派生します
  (`Theme/Background/Primary` → `--theme/background/primary`)。手書きで
  `.figma/variables.json` の `css` フィールドを編集すれば任意の名前に出来ます。

## 開発

```bash
cargo test                                # unit + integration tests
cargo test --test asset_download -- --ignored   # ネットワーク経由の asset DL smoke test
```

モジュール構成：

| ファイル | 役割 |
|---|---|
| `src/main.rs` | CLI 引数 + パイプライン制御 |
| `src/mcp.rs` | MCP client（Streamable HTTP transport、認証なし） |
| `src/figma_url.rs` | Figma URL を `(fileKey, nodeId)` にパース |
| `src/extract.rs` | MCP レスポンスから TSX コードブロックを取り出し、ヘッダを組む。`ContentBlock` 定義 |
| `src/instance_map.rs` | `.figma/instance-map.json` の load + validate |
| `src/replace.rs` | JSX サブツリー置換、サーバが extract した関数の削除 |
| `src/icons.rs` | `<img src={imgXxx} />` → `<Component />` 置換、const 宣言の整理 |
| `src/imports.rs` | `import { ... } from "..."` を出すための共通 helper |
| `src/assets.rs` | 並列 asset DL（cloud / localhost URL）+ 拡張子判定 + URL 書換 |
| `src/colors.rs` | `[#XXXXXX]` → `[var(--name,#XXX)]` 置換。`.figma/variables.json` がソース |
| `src/variables_dump.rs` | MCP `get_variable_defs` から色 variable を取得、`colors.rs` 用 JSON を出す |
| `src/trim.rs` | token 節約 pass：冗長 Tailwind class と指定 JSX 属性の削除 (`.figma/config.toml`) |

Claude Code 用の skills は `skills/` 配下：

- [`figma-to-code`](skills/figma-to-code/SKILL.md) — エンドツーエンドの単一 workflow:
  `FIGMA.md`（ファイル & ページマップ + コード対応の解釈メモ）の bootstrap / 維持、
  `figma-code-dl` を `--map` / `--icons` / `--colors` / `--trim` 付きで実行、
  新規に見つかった component を `class-variance-authority` で DS に取り込み、
  `.figma/instance-map.json` に登録、までを一本で。
