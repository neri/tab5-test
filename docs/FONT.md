# 16ピクセルビットマップフォント

> 索引: [`../DESIGN.md`](../DESIGN.md)

画面に出る文字はすべてこのフォントで描きます。コンソール、ブラウザ、toolbar、
status、全画面アプリのどれも同じglyphデータと同じ`advance`を使い、描画APIは
[`GRAPHICS.md`](GRAPHICS.md)の`draw_glyph`／`draw_text`だけです。移行の経緯と
実機での判断記録は[`FONT_MIGRATION_PLAN.md`](FONT_MIGRATION_PLAN.md)にあります。

## 契約

| 種類 | glyph box | advance |
| --- | ---: | ---: |
| 半角 | 8×16 | 8 px |
| 全角 | 16×16 | 16 px |
| combining mark | 16×16内 | 0 px |
| 未収録 | ASCIIは8×16、それ以外は16×16 | 8 px／16 px |

**幅は`font::advance`が唯一の出所です。** rendererとブラウザのlayoutが別々に
Unicode範囲から幅を推測すると、いつか食い違い、リンクの下線の長さやタップの
当たり判定が描画とずれます。両方が同じ関数を呼ぶことがそれを防ぐ唯一の方法です。

拡大は整数倍だけです。本文は1倍（16 px）、大見出しは2倍（32 px）です。1.5倍は
source pixelごとの幅が1 pxと2 pxで交互になり輪郭が崩れるので使いません。太字は
同じglyphを1 physical pixel右へずらして二度描きします。

未収録文字は空白にも`?`にもせず、`advance`が返す幅と同じ中空の枠を描きます。空白に
すると文字列がそこで終わったように見え、`?`は著者が書いた`?`と区別が付きません。

combining markは直前の文字へ重ねて描き、自分の幅を持ちません。Unifontはcombining
markのinkを直前の文字のcellへ重ねる前提の位置に置いている（U+3099は16幅boxの右上、
U+0301は8幅boxの中央付近）ので、rendererは`pen_x - 直前のadvance`へ、**必ずsparse
（背景を塗らない）で**描きます。opaqueで描くと16列のboxが次のcellを消します。直前が
無いcombining markは重ねる相手がいないので、U+FFFDを1文字として描きます。

## 収録範囲

元データはGNU Unifont Japanese 17.0.05のPlane 0 BDFです。由来、版、hash、licenseは
[`../tools/font/vendor/PROVENANCE.md`](../tools/font/vendor/PROVENANCE.md)にあり、
元データ自体もリポジトリに入れてあります。ネットワーク無しで生成物を再現できることを
優先しています。license はSIL Open Font License 1.1です。

収録するのは`tools/font/manifest.txt`が指定する範囲です。

- JIS X 0213 第1面（第1〜3水準）
- かな・カタカナ・CJK約物の各ブロック全体、全角／半角互換形
- ASCII、Latin-1、Latin Extended-A／B
- 一般句読点、通貨記号、矢印、囲み英数、罫線、ブロック要素、幾何学記号
- 人名の異体字（U+9AD9 と CJK互換漢字 U+FA00–U+FA6D）
- U+FFFD

生成物は9,747 glyph、357,907 byte（349.5 KiB）です。上限は512 KiBで、超えると生成が
**失敗**します。文字を黙って落とさないためです。

JIS X 0213第1面を収録していることが、ブラウザのShift_JIS対応を「表を足すだけ」に
しています。字は最初から全部あり、足りなかったのはbyte列から符号位置への対応
だけでした（[`BROWSER.md`](BROWSER.md)の「文字符号化」）。

UI用の記号がここに入っていることにも依存があります。ブラウザのtoolbarのボタンは
`←`（U+2190）、`→`（U+2192）、`↻`（U+21BB）、`×`（U+00D7）を2倍角で描いた
ものです。矢印ブロック（U+2190–U+21FF）を丸ごと収録しているので使えますが、
似た形の`✕`（U+2715）や`⚠`（U+26A0）は範囲外で、`advance`が16を返す
＝未収録なので中空の枠になります。UIに記号を足すときは
`font/data/tab5font16.txt`で収録を確かめてください。

BMP外は入っていません。元のUnifont-JPがPlane 0しか持たないためで、JIS X 0213第1面の
うち26文字と`𠮷`（U+20BB7）が該当します。これらは欠落枠として16 px幅で表示され、
`advance`も16 pxを返すので折返しと当たり判定はずれません。落とした文字の一覧は
`font/data/tab5font16.txt`にあります。

## モジュール

| パス | 役割 |
| --- | --- |
| `font/src/lib.rs` | `glyph`、`glyph_or_replacement`、`advance`、`is_combining`、`text_width` |
| `font/src/console.rs` | コンソール専用の半角セルID変換 |
| `font/data/tab5font16.bin` | 生成済みbitmap。`include_bytes!`でDROMへ入る |
| `font/data/tab5font16.txt` | 生成報告（glyph数、内訳、byte数、落とした文字） |
| `src/font.rs` | firmware側の再export（`crate::font`） |
| `tools/font/` | 生成tool、manifest、元データ、license |

`font/`は依存ゼロの`no_std` crateで、`browser/`と同じくホストでbuildできます。収録
範囲、文字幅、コンソールのセル変換は`cargo test`で検査します（`mise run test`）。
firmwareと`tab5-browser`の両方がこのcrateへ依存し、幅の判定を複製しません。

## 生成物の形式

little endianで、`header + ranges + bitmaps + advances`の4区画です。詳細は
[`../tools/font/README.md`](../tools/font/README.md)にあります。要点だけ:

- lookupは`ranges`（先頭code point、個数、先頭glyph index）の二分探索1回
- glyphは1件固定32 byteの**column-major**。`columns[0]`が左端、各列のbit 0が上端
- `advances`は1件1 byteで、値は0／8／16

column-majorなのは、Framebufferがlogical Xをnative addressの逆順へ写すためです。
描画は列ごとの縦runになるので、データをその順で持てばrendererは1文字ごとの
row/column変換をしません。

magic、format version、区画のoffsetと連続性は`const _: () = assert!(...)`でcompile時に
検査します。CRCと中身はhost testで検査します。

## 再生成

```sh
python3 tools/font/generate.py
```

`manifest.txt`を編集したときだけ実行し、生成物と生成報告を一緒にcommitします。
通常のbuildはBDFを解析しません。commit済み生成物が最新かどうかは
`python3 tools/font/generate.py --check`で確認できます。

## 配置

生成物はDROM（`0x40000020`から）にあり、`include_bytes!`でリンクされます。内部RAMには
置きません。数百KiBを常時RAMへ載せる理由が無く、最初のコンソール描画はPSRAM初期化と
cold XIP probeの後、通常のIROM上のコードから行われるためです。DROMが読めない状態では
通常のアプリコードも動かないので、緊急表示用に別のフォントをRAMへ残す意味もありません。
起動失敗の診断はUARTを正とします（[`BOOT.md`](BOOT.md)）。

storage partitionやSDへ置く案は採っていません。起動後に別媒体をmountし、glyphをcacheし、
抜去やI/O失敗を描画中に扱う複雑さに対して、DROMのsubsetで得る節約が小さいためです。

## コンソールのセル

コンソールは1文字1セルの半角固定端末なので、`advance`をlayoutには使いません。
`font::console`がすべてのUnicode scalarを1 byteのセルIDへ写します。規則と
IDの割り当ては[`CONSOLE_SHELL.md`](CONSOLE_SHELL.md)にあります。

## 対象外

日本語IME、かな漢字変換、CSS Web Font、可変font、outline font renderer、anti-alias、
kerning、比例幅Latin font、OpenType shaping、Arabic／Indic等の文脈字形、color emoji、
Unicode全plane、runtimeでのfont読み込みと切り替えは扱いません。
