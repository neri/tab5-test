# 16 pixel bitmap fontの生成

> 索引: [`../../DESIGN.md`](../../DESIGN.md)

`font/data/tab5font16.bin`を作るhost側のtoolです。firmwareはBDFを解析しません。
元BDFはリポジトリへcommitせず、root `Makefile`が固定版をdownloadしてhashを検証します。
3,175 byteの生成物はリポジトリへcommitし、通常のbuildは`include_bytes!`で読むだけです。移行計画は
[`../../docs/plans/archive/FONT_MIGRATION_PLAN.md`](../../docs/plans/archive/FONT_MIGRATION_PLAN.md)にあります。

## ファイル

| パス | 役割 |
| --- | --- |
| `vendor/unifont_jp-17.0.05.bdf.gz` | 元データ。由来とhashは[`vendor/PROVENANCE.md`](vendor/PROVENANCE.md) |
| `vendor/LICENSE-OFL-1.1.txt` | 元データと生成物のlicense (SIL OFL 1.1) |
| `ascii-manifest.txt` | 1bpp blobへ収録するprintable ASCII。人が編集する |
| `manifest.txt` | 旧全repertoireの記録。A4日本語manifestを見直す際の参照 |
| `repertoire/jisx0213-plane1.txt` | JIS X 0213 第1面を範囲へ展開したもの。`derive_jisx0213.py`が生成 |
| `derive_jisx0213.py` | 上記の再生成。CPythonの`euc_jis_2004` codecを使う |
| `generate.py` | BDF + manifest → `font/data/tab5font16.bin` |

## 実行

```sh
make fonts
```

収録glyph数、半角／全角／combining数、範囲数、最大code point、byte数、CRCを表示し、
同じ内容を`font/data/tab5font16.txt`へ書きます。CIや手元での確認には

```sh
make fonts-check
```

を使うと、commit済みの生成物が最新かどうかだけを検査します。

生成は次のいずれかで**失敗**します。黙って文字を落とすことはありません。

- 元BDFの展開後SHA-256が`vendor/PROVENANCE.md`の記録と違う
- glyphがBDFの`BBX`のまま16×16 canvasへ収まらない
- `DWIDTH`が8でも16でもない
- printable ASCII U+0020–U+007Eに欠落があるか、8 pixel幅でないものがある
- 生成物が16 KiBを超える
- glyph数がu16のglyph indexを超える

manifestが要求したのに元データに無いcode pointと、BMP外のcode pointは、数と一覧を
報告した上で落とします。

## 生成物の形式

little endian、全体は`header + ranges + bitmaps + advances`の4区画です。

```text
header (32 byte)
  0  u8[4]  magic "T5F1"
  4  u16    format version (1)
  6  u16    header size (32)
  8  u32    glyph count
 12  u32    range count
 16  u32    ranges offset
 20  u32    bitmaps offset
 24  u32    advances offset
 28  u32    header以降のCRC-32 (ISO-HDLC)

ranges   1件8 byte: u32 先頭code point, u16 個数, u16 先頭glyph index
bitmaps  1件32 byte: u16 × 16列
advances 1件1 byte: 0, 8, 16のいずれか
```

`ranges`はcode point昇順で重複無し、`bitmaps`と`advances`はそのglyph index順です。
lookupは`ranges`の二分探索1回だけで、glyphの位置は即座に計算できます。現repertoireは
printable ASCIIだけなので1 rangeです。

glyphは**column-major**で、`columns[0]`が左端、各列のbit 0が上端です。Framebufferは
logical Xがnative addressの逆順へ写るので、描画は列ごとの縦runになります。データを
その順で持つことで、rendererは1文字ごとのrow/column変換をしません。

ASCII glyphは列8以降を0で埋めた固定32 byteです。可変長化より、glyph位置の即時計算と
offset表を持たない単純さを優先しています。日本語とcombining markはA4 generator側で扱います。
