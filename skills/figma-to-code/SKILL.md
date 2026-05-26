---
name: figma-to-code
description: Figma URL を渡されたら、(0) プロジェクトルートの FIGMA.md で解釈を把握 → (1) 既存コードを正にして data-name を突合 → (2) figma-code-dl CLI で TSX をローカルファイル化（token 削減）→ (3) 読み取って実装、までを 1 本で回す統合スキル。途中で新規 component が必要になれば cva で取り込み `.figma/instance-map.json` に追記する。「Figma URL を渡された」「この画面を React で実装」「Figma の component を取り込んで」「Switch を DS に追加」等で発火。重い MCP tool（get_design_context など）は CLI が 1 回叩くだけで、それ以外のフェーズは get_metadata / get_libraries / get_variable_defs などの軽量 tool しか使わない。**必須前提**：local Figma Dev Mode MCP server (`http://127.0.0.1:3845`) が起動していること。動いていなければこのスキルは使えない — cloud 版にはフォールバックせず、公式の figma MCP プラグインが使えるならそちらを直接使い、それも無ければ何もしない。
---

# Figma → Code 統合フロー

Figma URL から React 実装までの一連の流れを 1 本にまとめたスキル。

## このスキルがやること

```
[Figma URL] 
   │
   ▼
Phase 0  FIGMA.md セットアップ（軽量 tool のみ）
   │
   ▼
Phase 1  解釈フェーズ：既存コードを正に、data-name を突合
   │
   ▼
Phase 2  figma-code-dl で .tsx に書き出し（重い tool はここで 1 回）
   │     └─ Phase 2.5  未取込 component を見つけたら cva 化 → instance-map に追記 → 再生成
   │
   ▼
Phase 3  出力 .tsx を読んで実装に反映
   │
   ▼
Phase 4  FIGMA.md を軽量に更新（構成変更 + 再現しそうな癖のみ）
```

## 大前提（運用ポリシー）

1. **既存コードが正。** デザイナーはコーディングの専門家ではない。Figma 側の命名・粒度・variant 設定が崩れていても、揃えるべきは Figma ではなくこちらの解釈側。
2. **デザイナーに改善要求は基本投げない。** quirks や naming の差分は FIGMA.md で吸収する。fix が必要な場合のみ `## Open Questions` に書く。
3. **variant が無くても咎めない。** Figma に hover / focused / disabled が無くても普通。実装側の variant で補完する。
4. **token 節約を最優先。** Phase 0 / 1 / 4 では **構成しか取らない軽量 tool だけ** 使う。`get_design_context` は Phase 2 の figma-code-dl 内で 1 回叩くのみ。
5. **local Figma Dev Mode MCP server (`http://127.0.0.1:3845/mcp`) が動いていることが必須前提**。動いていなければ **このスキルも `figma-code-dl` CLI も使えない**。cloud 版（公式の `figma` プラグイン MCP）にフォールバックはしない — その代わり後述の「local MCP が無いとき」セクションを参照して、公式 figma MCP で素直に作業する。

## 事前ゲート (1)：local Dev Mode MCP server が使えるか？

このスキル全体と `figma-code-dl` CLI は、**Figma Desktop の Dev Mode MCP server (`http://127.0.0.1:3845`)** に依存します。これが動いていなければ何もしないでください。**cloud 版にフォールバックしない**。

判定手順：

```bash
curl -s -o /dev/null -w '%{http_code}\n' -X POST http://127.0.0.1:3845/mcp \
  -H 'Content-Type: application/json' \
  -H 'Accept: application/json, text/event-stream' \
  -d '{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2024-11-05","capabilities":{},"clientInfo":{"name":"probe","version":"0"}}}'
```

- **200 が返る** → このスキルで作業を進める。下の「事前確認 (2)」へ。
- **接続失敗 / 200 以外** → このスキルは使えない。次の分岐へ：

### local MCP が無いとき

公式の **`figma` プラグイン MCP**（`mcp__plugin_figma_figma__*` 系の tools — `get_design_context` / `get_metadata` / `get_variable_defs` / `use_figma` 等）が利用可能なら、**このスキルのことは忘れて、その公式 MCP を普通に使う**。

公式 MCP は：
- 認証済みで cloud 経由
- 動作対象は active tab に限らず URL から自由
- 同等の API（`get_design_context` 等）が直接呼べる

公式 MCP も使えない場合は、ユーザーに「Figma Desktop を起動して Dev Mode MCP server を ON にするか、Claude の figma プラグイン MCP を有効化してください」と案内して止まる。**何もしない**のが正解。

## 事前確認 (2)：Figma ファイルがアクティブタブか？

local Dev Mode MCP は **Figma Desktop で今アクティブになっているタブのファイル** に対してしか動きません。URL の fileKey は飾りで、active tab が違うファイルだと `get_metadata` / `get_design_context` / `get_variable_defs` のいずれも `No node could be found ...` で失敗します。

なので **MCP を叩く前に必ずユーザーに確認**してください。最初に次のように聞いてから作業に入る：

> Figma Desktop で対象のファイルを開いて、**そのタブをアクティブ（最前面）** にしてください。準備できたら教えてください。

これが終わったら以下のフローへ。

## いつ発火するか

- Figma URL（`figma.com/design/...` / `figma.com/file/...`）を渡された
- 「この画面を React で」「Figma 取り込んで」「DS にこの component 追加」
- `.figma/instance-map.json` を新規・追記する
- Figma ファイル・page 構成を聞かれた

---

## Phase 0: FIGMA.md セットアップ

プロジェクトルートの `FIGMA.md` を読む。**無ければここで作る**。

### 0-A. 無いとき：ブートストラップ

**目的：** 構成だけ書く。中身は見ない。token 消費を最小化。

集めるものは以下だけ：

- File（複数あり得る）：fileKey, ファイル名, URL, 役割
- Page 一覧：各 File ごとに page 名と node id
- Library / Variable Collection の **名前一覧**（値は取らない）
- 既存コード側の目次：`src/components/`, `.figma/instance-map.json` 等の **トップレベル名のみ**

使う tool（軽量のみ）：

| 用途 | tool | 注意 |
|---|---|---|
| ファイル/page メタ | `mcp__plugin_figma_figma__get_metadata`（file ルートに対して） | 重ければ単一 page だけ |
| ライブラリ名一覧 | `mcp__plugin_figma_figma__get_libraries` | 値は取らない |
| variable コレクション名 | `mcp__plugin_figma_figma__get_variable_defs` | コレクション名だけスキャン、全 token は引かない |
| 既存コード目次 | `ls src/components` / `ls .figma` | コード本体は読まない |

**禁止 tool（重い）：** `get_design_context`, `get_screenshot`, `get_code_connect_map`, `use_figma`

手順：

1. URL から fileKey と nodeId を抽出（`-` → `:` 変換）。
2. 上の「事前ゲート (1)」で local MCP が生きていることは前提。死んでいたらこのスキル全体を中断しているはずなのでここに来ない。
3. `get_metadata` で page list を取得。
4. `get_libraries` で library 名のみメモ。
5. `ls src/components` 等でローカルの目次。
6. 後述のテンプレートで `FIGMA.md` を **プロジェクトルート**に書き出す。
7. **ここで一旦止める。** quirks / mapping は埋めない。「最低限の地図ができた」状態でユーザーに見せる。

### 0-B. あるとき：構成チェックと軽量追記

- 今回扱う Figma URL の fileKey / page が登録済みか確認。
- 新しい file → YAML の `files:` に 1 エントリ追加（key, name, url のみ）。
- 新しい page → 該当 file の `pages:` に 1 行追加。
- 追記は **構成部分の Edit だけ**。`get_metadata` を 1 回だけ叩いて確認 → 数行 Edit → 終了。

---

## Phase 1: 解釈フェーズ（既存コードを正に）

Phase 2 で重い tool を叩く **前に**、対象 node に対する解釈を作る。

1. `get_metadata` を対象 nodeId に対して **1 回だけ** 叩いて、子孫の `data-name` 候補を見渡す（中身まで読まない）。
2. 既存コード側（`src/components/ds/*`、`.figma/instance-map.json`）と突き合わせ：
   - 名寄せは LLM 推測で OK。例：`btn` → 既存 `<Button>`、`list` / `1`〜`9` → 既存 `<ListRow>` の繰り返し、`Component 2` → 文脈で `<IconBadge>` 等。
   - **Figma 側に variant が無いことを欠陥扱いしない。** 既存 component の variant に従って実装側で補完。
3. 解釈結果は **既存コードを変えず**、FIGMA.md の `## Component Mapping` に `confidence: high / medium / low` 付きで追記。
4. その mapping を `.figma/instance-map.json` にも反映（次の Phase 2 で `--map` 経由で効く）。

---

## Phase 2: figma-code-dl 実行（ここだけ重い）

ローカル CLI が `get_design_context` を内部で 1 回だけ叩いて、結果を `.tsx` ファイルに書き出す。
**Claude が直接 `get_design_context` を呼ばない理由**：レスポンスが 100KB 級になり token を消費するため。
**ファイルに書けば後で必要箇所だけ Read で読める**。

### 前提セットアップ

- Figma Desktop 起動 → 対象ファイルを **active tab**（**毎回確認**：別ファイルがアクティブだと MCP が「No node found」で落ちる）
- Preferences → **"Enable Dev Mode MCP server"** ON
- 疎通確認：
  ```bash
  curl -s -o /dev/null -w '%{http_code}\n' -X POST http://127.0.0.1:3845/mcp \
    -H 'Content-Type: application/json' \
    -H 'Accept: application/json, text/event-stream' \
    -d '{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2024-11-05","capabilities":{},"clientInfo":{"name":"probe","version":"0"}}}'
  ```
- CLI インストール：`cd path/to/figma-code-dl && cargo install --path .`

### 実行

```bash
figma-code-dl '<figma-url>' \
  --out src/pages/<Name>.tsx \
  --map .figma/instance-map.json \
  --download-assets src/pages/assets \
  --report-unmapped
```

| フラグ | 用途 |
|---|---|
| `<url>`（positional） | Figma URL（nodeId のみ抽出） |
| `--from-json <path\|->` | URL の代わりに事前 fetch JSON を読む |
| `--out <path>` | 出力 .tsx パス（必須） |
| `--source-url <url>` | 出力ヘッダ用元 URL |
| `--map <path>` | `.figma/instance-map.json` で instance 置換 |
| `--download-assets <dir>` | 画像/SVG をローカル DL + URL 書換 |
| `--report-unmapped` | 未マップ `data-name` を頻度順に stderr 表示 |
| `--mcp-url <url>` | MCP エンドポイント（既定 `http://127.0.0.1:3845/mcp`） |

### 出力ファイル

```tsx
// Auto-generated by figma-code-dl
// Source: <figma-url>
// file=... node=...
// Styles digest: ...

import { Switch } from "@/components/ds/Switch";    // ← --map 経由
import Button from "@/components/ds/Button";        // ← 自動 extract された Button を削除 + import

const imgImage145 = "./assets/image-145.png";       // ← --download-assets 経由
// ...

export default function NodeName() { ... }
```

`import React` は付かない（auto JSX runtime 前提）。`data-node-id` / `data-name` は **意図的に残す**（instance 置換の根拠）。

### `.figma/instance-map.json` スキーマ

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

- キーは Figma の `data-name` と完全一致
- `export: "default"` + `alias` 未指定なら、local 名は Figma 名を PascalCase 化（例: `system/checklist` → `SystemChecklist`）
- 同名衝突時は全部置換 + stderr 警告。個別オーバーライドは `byNodeId`

---

## Phase 2.5: 未取込 component の取り込み（cva 変換）

`--report-unmapped` で頻度上位の `data-name` が出てきたら、上位から 1 個ずつ DS に取り込む。

### 取り込み手順

1. **component set の把握**：Figma の `get_metadata` を **component set ノード**（紫の◇）に対して呼ぶ。配下に各 variant frame（`state=on, size=md` 形式の name）が並ぶ。
   - component set（紫）= 複数 variant frame を持つ
   - single component（緑）= 単一 frame のみ
2. **代表 variant を抽出**：variant が ≤4 なら全部、多ければ最小・最大・典型の 2〜3 個に絞る。各 variant の node id ごとに：
   ```bash
   figma-code-dl '<figma-url>?node-id=<variant-1>' --out tmp/Switch-on-md.tsx
   figma-code-dl '<figma-url>?node-id=<variant-2>' --out tmp/Switch-off-md.tsx
   ```
3. **差分を cva の `variants:` に変換**：
   ```tsx
   import { cva, type VariantProps } from "class-variance-authority";
   import { cn } from "@/lib/utils";

   const switchVariants = cva(
     "relative inline-flex shrink-0 cursor-pointer rounded-full transition-colors",
     {
       variants: {
         state: {
           on:  "bg-[var(--color-semantic-background-greeninverse,#29c1af)]",
           off: "bg-[var(--color-semantic-background-blackinverse,#6e7075)]",
         },
         size: {
           sm: "h-[12px] w-[24px]",
           md: "h-[16px] w-[32px]",
           lg: "h-[20px] w-[40px]",
         },
       },
       defaultVariants: { state: "off", size: "md" },
     },
   );

   const thumbVariants = cva("absolute rounded-full bg-white transition-transform", {
     variants: {
       state: { on: "translate-x-full", off: "translate-x-0" },
       size:  { sm: "size-[10px]", md: "size-[14px]", lg: "size-[18px]" },
     },
   });

   export interface SwitchProps
     extends Omit<React.ButtonHTMLAttributes<HTMLButtonElement>, "type">,
       VariantProps<typeof switchVariants> {}

   export function Switch({ state, size, className, ...props }: SwitchProps) {
     return (
       <button
         type="button"
         role="switch"
         aria-checked={state === "on"}
         className={cn(switchVariants({ state, size }), className)}
         {...props}
       >
         <span className={cn(thumbVariants({ state, size }))} />
       </button>
     );
   }
   ```
   変換のポイント：
   - 全 variant 共通 → base、variant 別 → `variants:`
   - 子要素にも variant 差分があれば element ごとに cva 分割（上記 `thumbVariants`）
   - `var(--color/semantic/...)` はそのまま残す（DS token と整合）。`\/` エスケープは Tailwind arbitrary value 記法上は剥がして OK
   - `className` prop は `cn(..., className)` で末尾合成
   - 「組合せ専用差分」は `compoundVariants` で：
     ```ts
     compoundVariants: [{ state: "on", size: "lg", class: "shadow-lg" }],
     ```
4. **ファイル配置**：`src/components/ds/<Name>/index.tsx`
5. **`.figma/instance-map.json` に追記**（キーは `data-name` 完全一致）
6. **再生成**：同じ画面を `--map ... --report-unmapped` 付きで再実行し、置換が走るか確認。
7. **`--report-unmapped` 頻度上位** から次の component へ。

### 取り込み時の注意

- **variant 全列挙より使用実態優先**：Figma 側で 20+ variant あっても、実際に画面で使う組合せだけ実装。後追加は容易。
- **Property 名の日↔英揺れ**：Figma が日本語（`状態`, `サイズ`）でも、cva 側は英語（`state`, `size`）に統一。対応表は当該 component の README か Storybook に。
- **Boolean property** → cva の `{ true / false }`
- **Text property** → cva ではなく素直な props（`<Button>{children}</Button>`）
- **Instance swap property** → cva では表現しきれない。slot 設計に倒す（`<Button icon={<DownloadIcon />}>`）

### Phase 2.5 の前提（一度入れれば終わり）

```bash
npm i class-variance-authority clsx tailwind-merge
```

```ts
// @/lib/utils
import { clsx, type ClassValue } from "clsx";
import { twMerge } from "tailwind-merge";
export function cn(...inputs: ClassValue[]) { return twMerge(clsx(inputs)); }
```

---

## Phase 3: 読み取り & 実装

ここで初めて Phase 2 で書き出した `.tsx` を Read する。**必要箇所だけ部分 Read**。

- 全文を一気に展開しない。最初は import と export default の関数シグネチャだけ。
- `data-node-id` / `data-name` は実装後に剥がす（Babel/eslint plugin or 手動）。
- スタイルや構造を既存のプロジェクト規約に **コンバート** する（Tailwind arbitrary value → DS token、絶対パス画像 → import など）。

---

## Phase 4: FIGMA.md の軽量更新

重い tool を起動するための更新はしない。**Phase 2 を叩いた "ついで"** に行う：

- 構成変更（新 file / 新 page）→ YAML を数行 Edit
- 再現しそうな癖（命名衝突パターン、bind 漏れの常連色、半端 px の傾向）→ `## Quirks` に 1〜2 行追記
- 一回限りの揺れは書かない（quirks がノイズで埋まる）
- 解釈の精度が上がったら `## Component Mapping` の `confidence` を更新
- 全体書き換えはしない。Edit ツールで局所的に。

---

## FIGMA.md テンプレート

````markdown
---
version: 0.1
name: <project-name>
files:
  - key: <fileKey>
    name: <human-readable>
    url: https://www.figma.com/design/<fileKey>/...
    role: source                # source | spec | archive | exploration
    last_synced: <YYYY-MM-DD>
    pages:
      - id: <page-node-id>
        name: <page-name>
        notes: <one-line>
libraries:
  - <library-name>
variable_collections:
  - <collection-name>
component_mapping:
  # <figma-name>: { module: "@/components/...", export: "default", confidence: high|medium|low, note: "..." }
---

## Overview

このプロジェクトでデザインを扱うときの参照地図。Figma ファイルそのものが一次情報なので
値は複製しない。書くのは「うちのコードの世界とどう繋がるか」と「Figma 上の癖」だけ。

- 既存コードが正。Figma 側の命名・粒度・variant が崩れていても、揃えるべきはこちらの解釈。
- デザイナーに改善要求は基本投げない。quirks は FIGMA.md で吸収する。
- 重い MCP tool は `figma-code-dl` 実行時のみ。

## Files & Pages

YAML 側の `files:` を一次情報とし、ここでは役割と関係性を散文で補足する。

- `<file-name>`: <この file の位置づけ、誰が更新しているか、関連 PR/Issue など>

## Component Mapping (Figma ↔ Code)

| Figma data-name | 解釈 (Code) | confidence | 補足 |
|---|---|---|---|
| <TBD> | <TBD> | — | — |

機械可読版は YAML の `component_mapping:` 及び `.figma/instance-map.json`。

## Conventions

- ノード命名：`PascalCase` を期待するが、`snake_case` / 日本語 / `system/xxx` 形式の混在を許容
- 色：CSS Variable bind が落ちて hex 直書きになっていることがある。対応する semantic token があれば優先
- スペーシング：4/8 系スケールから外れた半端値は最近接スケールに丸めて読む
- variant 不在：Figma 上に default しか無くても、既存コードの variant に従って実装する

## Quirks

再現しそうな癖だけ記録する（1 回限りの揺れは書かない）。

- <TBD>

## Open Questions

デザイナーに確認したい事項（強要しない、判断材料として渡すだけ）。

- <TBD>

## Changelog

- <YYYY-MM-DD>: ブートストラップ（file + page 一覧のみ）
````

---

## トラブルシュート

**`POST http://127.0.0.1:3845/mcp — is the Figma desktop app running ...`**
→ Figma Desktop 未起動 or Dev Mode MCP server 未有効化。Preferences で ON。

**`response contained no code block. The node may be a Figma section ...`**
→ 渡したノードが section。子 frame の node id に絞る。

**`No node could be found for the provided nodeId ...`**
→ 該当ファイルが active tab になっていない。Figma Desktop で最前面に。

**asset URL の有効性**
- ライブ fetch の localhost URL：Figma Desktop 起動中のみ有効
- `--from-json` の cloud URL：7 日 TTL。`--download-assets` 必須
