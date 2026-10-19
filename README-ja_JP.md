# Pantagraph

[English](README.md) | [日本語](README-ja_JP.md)

人手で付与したタグによってWebコンテンツを整理・インデックスし、外部ツールを用いたローカルアーカイブにも対応したCLIツールです。

## 特徴

- **タグ管理**: 階層を持たないフラットなタグ管理。エイリアス（別名）の設定に対応し、同名・同一エイリアスのタグもUUIDによって共存・識別可能（`name#N` 記法による曖昧さ解消）。
- **エントリ管理**: WebのURL、説明、タグを紐付けて記録。
- **プラグイン可能な外部アーカイバ**: `tool.toml` の設定により、外部ツール（`single-file` や `yt-dlp` など）を呼び出してローカルスナップショットを自動保存。
- **JSONLベースのストレージ**: タグとエントリを1行1レコードのJSONで保存（`tags.jsonl`, `entries.jsonl`）。既存のTOMLデータは初回起動時にJSONLへ移行されます。

---

## インストールとセットアップ

Cargoでビルド:

```bash
cargo build --release
```

または Cargo 経由で直接実行:

```bash
cargo run -- <COMMAND>
```

---

## 設定
 
### 1. 全体設定 (`config.toml`)
 
Pantagraph はデータディレクトリ直下（例: `data/config.toml`）またはカレントディレクトリから `config.toml` を読み込みます。
サンプル設定ファイルとして [`config.toml.sample`](config.toml.sample) が用意されています:
 
```bash
cp config.toml.sample config.toml
```
 
#### タグ重複ポリシー
 
タグ名と `name@specifier` 参照の重複時の挙動を個別に `config.toml` で指定できます:
 
```toml
[tag]
same_name_policy = "allow" # "allow" | "warn" | "error"
same_qualified_ref_policy = "error" # "allow" | "warn" | "error"
```
 
- **`same_name_policy`** (`"allow"` がデフォルト): specifier のない同名タグの挙動を指定します。
- **`same_qualified_ref_policy`** (`"error"` がデフォルト): `name@specifier` の重複時の挙動を指定します。
- **`"warn"`** は警告を表示して作成を続け、**`"error"`** はエラーで中断します。
 
---
 
### 2. アーカイバ設定 (`tool.toml`)

Pantagraph では、外部のアーカイブツールを柔軟に設定できます。設定ファイル `tool.toml` は、データディレクトリ直下（例: `data/tool.toml`）またはカレントディレクトリから読み込まれます。

サンプル設定ファイルとして [`tool.toml.sample`](file:///home/yuya/projects/20_sandbox/pantagraph/tool.toml.sample) が用意されています:

```bash
cp tool.toml.sample tool.toml
```

### `tool.toml` の設定例

```toml
[archiver.single-file]
command = "single-file"
args = [
  "--browser-executable-path=google-chrome",
  "--browser-headless=true",
  "--output-directory={outdir}",
  "{url}"
]

[archiver.ytdlp]
command = "yt-dlp"
args = [
  "-P", "{outdir}",
  "{url}"
]
```

#### `args` で使用できるプレースホルダー
- `{url}`: アーカイブ対象のURL。
- `{outdir}` (または `{output}`): そのエントリ専用に割り当てられた出力先ディレクトリパス（`<data_dir>/archives/<entry_id>/`）。

---

## 使い方

### 1. タグ操作 (`tag`)

#### タグの新規作成
```bash
pantagraph tag create <NAME> [--specifier <SPEC>] [--description <DESC>] [--alias <ALIAS>...]
# 例:
pantagraph tag create rust -d "プログラミング言語Rust" -a rs -a rust-lang
# 修飾子（specifier）を付与して作成（フラグまたは name@specifier 形式）:
pantagraph tag create "rust@lang" -d "プログラミング言語Rust"
pantagraph tag create rust -s game -d "サバイバルゲームRust"
```

#### タグ一覧の表示
```bash
pantagraph tag list [--name <FILTER>]
```
*※ タグ名およびエイリアスで部分一致検索が行われます。*

#### タグ詳細の表示
```bash
pantagraph tag show <NAME_OR_ALIAS_OR_ID>
```
*※ 同名・同一エイリアスのタグが複数存在する場合は、`name@specifier`（例: `rust@lang`）、`name#N`（例: `rust#1`）、またはタグのUUIDで一意に特定できます。*

#### タグ名の変更
```bash
pantagraph tag rename <NAME_OR_ALIAS_OR_ID> <NEW_NAME>
# 例:
pantagraph tag rename rust@lang rust-lang
```
*※ 同名・同一エイリアスのタグが複数存在する場合は、`name@specifier`、`name#N`、またはタグのUUIDで一意に特定できます。*

#### タグエイリアスの管理
エイリアスは通常のタグと同様に、すべてのコマンド（`tag show`, `tag rename`, `entry add --tag`, `entry list --tag`, `entry tag` など）で使用できます。

```bash
# エイリアスの追加
pantagraph tag alias add <NAME_OR_ALIAS_OR_ID> <ALIAS>
# 例:
pantagraph tag alias add rust rs

# エイリアスの削除
pantagraph tag alias remove <NAME_OR_ALIAS_OR_ID> <ALIAS>
# 例:
pantagraph tag alias remove rust rs

# エイリアス一覧の表示
pantagraph tag alias list <NAME_OR_ALIAS_OR_ID>
```

---

### 2. エントリ操作 (`entry`)

#### エントリの追加（URLブックマークのみ）
`--archiver` を指定しない（または `""`）場合、外部ツールを実行せずURLのみを記録します:

アーカイバ実行時には、アーカイブメタデータ（`archived`、`archiver`、`archive_path`）が `entries.jsonl` の `meta` フィールドに記録されます。
```bash
pantagraph entry add <URL> --tag <TAG> [-d <DESC>]
# 例:
pantagraph entry add https://doc.rust-lang.org --tag rust -d "Rust公式ドキュメント"
```

#### アーカイブ付きでエントリを追加
`--archiver=<NAME>` に `tool.toml` で定義したアーカイバ名を指定して実行します:
```bash
pantagraph entry add <URL> --archiver single-file --tag <TAG>
```
外部ツールが実行され、ファイルが `<data_dir>/archives/<entry_id>/` に保存された上でエントリが登録されます。

#### エントリ一覧の表示
```bash
# 全エントリを一覧表示
pantagraph entry list

# タグで絞り込み（複数指定時はすべてを含むエントリを抽出）
pantagraph entry list --tag rust --tag dev

# アーカイバ種別で絞り込み
pantagraph entry list --archiver single-file

# 未アーカイブ（URLブックマークのみ）のエントリを絞り込み
pantagraph entry list --archiver ""
```

#### エントリ詳細の表示
```bash
pantagraph entry show <ENTRY_ID>
```

#### 既存エントリへのタグ追加
```bash
pantagraph entry tag <ENTRY_ID> <TAG>
```

---

## データ保存先

データはデータディレクトリ（デフォルト: `data/`、`--data-dir <PATH>` で変更可能）に保存されます:

```
data/
├── entries.jsonl
├── tags.jsonl
├── tool.toml        # (tool.toml をここに配置することも可能)
└── archives/
    └── <entry_id>/  # 外部アーカイバによって保存されたファイル群
```

## クエリ構文

`--query` オプションでは、タグ参照の論理式でエントリを絞り込めます。演算子は以下が利用できます：

- 空白または `*` : AND（積集合）
- `+` : OR（和集合）
- `!` : NOT（補集合）
- `(` `)` : グループ化

さらに、`meta:<キー>[=<値>]` でメタ属性による絞り込みが可能です（値を省略した場合はキーの存在のみチェックします）。

例：
```bash
# rust と game の両方にマッチ
pantagraph entry list --query "rust game"
pantagraph entry list --query "rust * game"

# rust または game
pantagraph entry list --query "rust + game"

# rust を含み、game は含まない
pantagraph entry list --query "rust * !game"

# yt-dlp でアーカイブされたもの
pantagraph entry list --query "meta:archiver=yt-dlp"

# いずれかのアーカイバでアーカイブされたもの
pantagraph entry list --query "meta:archiver"

# アーカイブされていないもの
pantagraph entry list --query "!meta:archiver"

# 複合条件
pantagraph entry list --query "(rust + cpp) * meta:archived=true"
```

### メタ属性

エントリのメタ属性は `entries.jsonl` の `meta` フィールド（文字列のキー・バリューマップ）に格納されます。アーカイバ関連情報は `meta` に記録されます：

- `archived`（文字列）: 外部アーカイバが正常に実行された場合は `"true"`
- `archiver`（文字列）: 使用したアーカイバ名
- `archive_path`（文字列）: アーカイバが記録したパスまたは識別子（通常 `<data_dir>/archives/<entry_id>/`）

例：
```json
{"id":"...","url":"...","description":"...","tag_ids":["..."],"meta":{"archived":"true","archiver":"single-file","archive_path":"archives/<entry_id>/"}}
```

任意のメタキーは以下のコマンドで管理できます：

- `pantagraph entry meta set <ENTRY_ID> <KEY> <VALUE>`
- `pantagraph entry meta remove <ENTRY_ID> <KEY>`
- `pantagraph entry meta list <ENTRY_ID>`
