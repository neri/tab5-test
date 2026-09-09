# 文字フォント

> 索引: [`../DESIGN.md`](../DESIGN.md)

フォント経路は2つあります。Consoleと専有診断はGNU Unifont-JP由来の16 pixel 1bpp
bitmapを使い、通常GUI（Browser、system bar、Launcher、Network、Battery、Startup）は
DejaVu Sans／Sans Monoを16／24／32 pixelへ事前ラスタライズしたA4 bitmapを使います。
通常buildはBDF、TTF、FreeType、Pillowを解析・リンクしません。

## 通常GUIのA4 font

`tab5-ui-font`はEnglish Latin set 210文字を各strikeへ収録します。Browser本文、link、見出しと
GUI既定は比例幅Sans、`pre`・`code`系とClockは同一advanceのSans Monoです。16／32 pixelが
製品画面、24 pixelは`fonttest`の比較用です。serif、kerning、runtime outline描画、SDFは
採用していません。

各glyphはbitmap幅・高さ、signed x bearing、line box内y、advance、column-major A4 coverageを
持ちます。`ui_text_width`と`draw_ui_text`が同じmetricsを使い、English Latin外は従来fontへ
fallbackします。直後にcombining markがあるLatin baseはcluster全体を従来fontへ送り、markの
位置を比例幅advanceへ誤って重ねません。GUI既定をSansとしたのは本文との一貫性と、狭いbarで
固定幅より多くの文字を表示できるためです。

A4の0は書き込みを省略し、15は前景色を直接使い、中間値はRGB565のchannelごとに前景と背景を
blendします。背景なしでは既存pixelを読み、背景ありではadvance×line boxを消してからblend
します。host testはCRC、全210文字、mono advance、比例幅、combining fallback、blend、clip、
guard、opaque消去を検査します。

`fonttest`の比較専用に、同じglyphとmetricsを使いながらA4 coverageを8で二値化する
`paint_glyph_1bpp`があります。これはanti-aliasの実機目視baselineであり、通常GUIとBrowserは
常に`paint_glyph`のA4 blendを使います。

生成物は`ui-font/data/tab5-ui-fonts.bin`（139,618 byte、SHA-256
`f38451c7d9d01d639e86343971eb3d69e81559ffd78851c14a6f987fca418c2a`）です。由来とlicenseは
`tools/ui-font/vendor/`、生成条件とstrike別byte数は`ui-font/data/tab5-ui-fonts.txt`です。
再生成は`python3 tools/ui-font/generate.py`で行います。

## 従来の16 pixel font

## 契約

| 種類 | glyph box | advance |
| --- | ---: | ---: |
| 半角 | 8×16 | 8 px |
| 全角 | 16×16 | 16 px |
| combining mark | 16×16内 | 0 px |
| 未収録 | ASCIIは8×16、それ以外は16×16 | 8 px／16 px |

この経路では**幅は`font::advance`が唯一の出所です。** rendererとconsumerが別々に
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
| `font/data/tab5font16.bin` | 生成済み平文bitmap。通常buildではbuild scriptの入力 |
| `font/data/tab5font16.txt` | 生成報告（glyph数、内訳、byte数、落とした文字） |
| `ui-font/src/lib.rs` | A4 blob reader、metrics、fallback幅、RGB565 painter |
| `ui-font/data/tab5-ui-fonts.bin` | Sans／Sans Mono 6 strikeの生成済みA4 blob |
| `font-codec/src/lib.rs` | build時LZ4 encoder、起動時bounds付きdecoder、T5L4 wrapper／CRC |
| `src/font.rs` | firmware側の再export（`crate::font`） |
| `tools/font/` | 生成tool、manifest、元データ、license |
| `tools/ui-font/` | A4生成tool、元TTF、license、由来 |

`font/`と`font-codec/`は`no_std` crateで、`browser/`と同じくホストでbuildできます。収録
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

build scriptはmagic、format version、header長を検査してmetadata定数とT5L4 containerを
生成します。T5L4は28 byteのlittle-endian wrapperで、magic、version、header長、展開長、
圧縮block長、展開後CRC32、圧縮block CRC32、予約領域を持ち、その後ろにLZ4 raw blockが続きます。
起動時にはwrapperとblockを検査して展開し、さらにfont側のmagic、全offsetと連続性、出力長、CRCを
再検査します。host testは圧縮生成物の展開結果がcommit済み生成物とbyte単位で一致すること、入力切れ、
不正offset、入出力超過、wrapper長、両CRCの不一致を拒否することを検査します。

## 再生成

```sh
python3 tools/font/generate.py
```

`manifest.txt`を編集したときだけ実行し、生成物と生成報告を一緒にcommitします。
通常のbuildはBDFを解析しません。commit済み生成物が最新かどうかは
`python3 tools/font/generate.py --check`で確認できます。

## 配置

commit済み生成物は平文ですが、firmware build時に従来fontを227,004 byte、A4 UI fontを
76,826 byteの独立したT5L4 containerへ変換し、その合計303,830 byteだけをDROMへリンクします。
無圧縮497,525 byteに対して193,695 byte（38.9%）の削減です。PSRAM heap初期化直後に各containerを
357,907 byteと139,618 byteの永続bufferへ直接展開し、検証に合格したPSRAM addressだけをlookupへ
公開します。追加の大きな作業buffer、LZ4 frame、外部dictionary、XOR scrambleは使いません。
firmwareのlookupは未初期化ならpanicし、DROMの圧縮byte列をfontとして直接読みません。起動失敗の
診断はUARTを正とします（[`BOOT.md`](BOOT.md)）。

比較用feature `font-drom-direct`を有効にしたbuildだけは、commit済み平文blobをDROMから直接
参照し、PSRAM確保、LZ4展開、CRCを通りません。通常buildはPSRAM経路です。同一コードの
描画時間をA/Bするための一時的なbaselineであり、起動ログと`fonttest`に`DROM direct`または
`PSRAM decoded`を表示します。

比較buildだけは平文2 blobを収めるためDROM/IROM境界を`0x400c0000`へ戻します。既定のLZ4 buildは
`0x40090000`境界で、固定paddingを含むfirmware imageを192 KiB小さくします。

実機の長文スクロールで両buildを比較した結果、PSRAM版はslowest viewport repaintの数値が
少し増えたものの、更新ログが出るタイミングはほぼ同じで、表示上の問題はありませんでした。
利用者判断でこの差は許容し、PSRAM経路を既定として維持します。具体的な時間値は未記録です。

LZ4 generator／decoder、host malformed-input test、release ELF配置検査までは完了しています。
既定LZ4 buildの起動、PSRAM展開、フォント表示、Browser長文scrollは実機で正常動作を確認済みです。
具体的な展開cycle値は未記録です。詳細は
[`SCALABLE_PROPORTIONAL_FONT_PLAN.md`](SCALABLE_PROPORTIONAL_FONT_PLAN.md)に記録しています。

```sh
# PSRAM経路（既定）
cargo run --release

# 以前と同じDROM直接参照
cargo run --release --features font-drom-direct
```

storage partitionやSDへ置く案は採っていません。起動後に別媒体をmountし、glyphをcacheし、
抜去やI/O失敗を描画中に扱う複雑さに対して、DROMのsubsetで得る節約が小さいためです。

## コンソールのセル

コンソールは1文字1セルの半角固定端末なので、`advance`をlayoutには使いません。
`font::console`がすべてのUnicode scalarを1 byteのセルIDへ写します。規則と
IDの割り当ては[`CONSOLE_SHELL.md`](CONSOLE_SHELL.md)にあります。

## 対象外

日本語IME、かな漢字変換、CSS Web Font、可変font、runtime outline renderer、SDF、kerning、
OpenType shaping、Arabic／Indic等の文脈字形、color emoji、
Unicode全plane、runtimeでのfont読み込みと切り替えは扱いません。
