# 16 pixel bitmap fontの生成

> 索引: [`../../DESIGN.md`](../../DESIGN.md)

`font/data/tab5font16.bin`を作るhost側のtoolです。firmwareはBDFを解析しません。
生成物はリポジトリへcommitし、通常のbuildは`include_bytes!`で読むだけです。移行計画は
[`../../docs/FONT_MIGRATION_PLAN.md`](../../docs/FONT_MIGRATION_PLAN.md)にあります。

## ファイル

| パス | 役割 |
| --- | --- |
| `vendor/unifont_jp-17.0.05.bdf.gz` | 元データ。由来とhashは[`vendor/PROVENANCE.md`](vendor/PROVENANCE.md) |
| `vendor/LICENSE-OFL-1.1.txt` | 元データと生成物のlicense (SIL OFL 1.1) |
| `manifest.txt` | 収録するUnicode範囲。人が編集する |
| `repertoire/jisx0213-plane1.txt` | JIS X 0213 第1面を範囲へ展開したもの。`derive_jisx0213.py`が生成 |
| `derive_jisx0213.py` | 上記の再生成。CPythonの`euc_jis_2004` codecを使う |
| `generate.py` | BDF + manifest → `font/data/tab5font16.bin` |

## 実行

```sh
python3 tools/font/generate.py
```

収録glyph数、半角／全角／combining数、範囲数、最大code point、byte数、CRCを表示し、
同じ内容を`font/data/tab5font16.txt`へ書きます。CIや手元での確認には

```sh
python3 tools/font/generate.py --check
```

を使うと、commit済みの生成物が最新かどうかだけを検査します。

生成は次のいずれかで**失敗**します。黙って文字を落とすことはありません。

- 元BDFの展開後SHA-256が`vendor/PROVENANCE.md`の記録と違う
- glyphがBDFの`BBX`のまま16×16 canvasへ収まらない
- `DWIDTH`が8でも16でもない
- コンソールのrepertoire（U+0020–U+007E、U+00A0–U+00FF、U+FF61–U+FF9F）に
  欠落があるか、8 pixel幅でないものがある（U+00AD SOFT HYPHENのみ例外）
- 生成物が512 KiBを超える
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
lookupは`ranges`の二分探索1回だけで、glyphの位置は即座に計算できます。JIS漢字は
Unicode上で連続しないため範囲は4,500件ほどありますが、glyph 1件ごとのu32 index表より
20 KiBほど小さくなります。

glyphは**column-major**で、`columns[0]`が左端、各列のbit 0が上端です。Framebufferは
logical Xがnative addressの逆順へ写るので、描画は列ごとの縦runになります。データを
その順で持つことで、rendererは1文字ごとのrow/column変換をしません。

半角glyphは列8以降を0で埋めた固定32 byteです。16 byte可変にすると9,747 glyphで
20 KiBほど縮みますが、glyph位置の即時計算とoffset表の省略を優先しています。

`advance`が0のものはcombining markです。Unifontはcombining markのinkを、**直前の
文字のcellへ重ねる前提の位置**に置いています（U+3099は16幅boxの右上、U+0301は
8幅boxの中央付近）。したがってrendererはcombining markを`pen_x - 直前のadvance`へ
描き、かつ必ずsparse（背景を塗らない）で描きます。opaqueで描くと、16列のboxが
次のcellを消してしまいます。
