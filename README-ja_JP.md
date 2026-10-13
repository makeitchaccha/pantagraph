# Pantagraph

[English](README.md) | [日本語](README-ja_JP.md)

人手で付与したタグによってWebコンテンツを整理・インデックスし、外部ツールを用いたローカルアーカイブにも対応したCLIツールです。

## 特徴

- **タグ管理**: 階層を持たないフラットなタグ管理。同名のタグもUUIDによって共存・識別可能（`name#N` 記法による曖昧さ解消）。
- **エントリ管理**: WebのURL、説明、タグを紐付けて記録。
- **プラグイン可能な外部アーカイバ**: `tool.toml` の設定により、外部ツール（`single-file` や `yt-dlp` など）を呼び出してローカルスナップショットを自動保存。
- **TOMLベースのストレージ**: 人間が読みやすく、Git等のバージョン管理にも適したプレーンテキスト保存（`tags.toml`, `entries.toml`）。

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

## 設定 (`tool.toml`)

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
pantagraph tag create <NAME> [--description <DESC>]
# 例:
pantagraph tag create rust -d "プログラミング言語Rust"
```

#### タグ一覧の表示
```bash
pantagraph tag list [--name <FILTER>]
```

#### タグ詳細の表示
```bash
pantagraph tag show <NAME_OR_ID>
```
*※ 同名のタグが複数存在する場合は、`rust#1` や `rust#2` のような指定、またはタグのUUIDで一意に特定できます。*

---

### 2. エントリ操作 (`entry`)

#### エントリの追加（URLブックマークのみ）
`--archiver` を指定しない（または `""`）場合、外部ツールを実行せずURLのみを記録します:
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
├── entries.toml
├── tags.toml
├── tool.toml        # (tool.toml をここに配置することも可能)
└── archives/
    └── <entry_id>/  # 外部アーカイバによって保存されたファイル群
```
