# 文字フォント

> 索引: [`../DESIGN.md`](../DESIGN.md)

現行のフォント経路は用途と格納先で2つに分けます。

| 経路 | 収録内容 | 形式 | 実行時の参照元 |
| --- | --- | --- | --- |
| `tab5-font` | printable ASCII U+0020–U+007E、95 glyph | 16 px、1bpp、固定8 px advance | 非圧縮DROMを直接参照 |
| `tab5-ui-font` | Latin 6 strikeと日本語1 strike | column-major A4 | 通常buildはLZ4をPSRAMへ展開 |

Consoleと`draw_text`を使う低水準診断はASCII 1bppだけを表示します。ASCII外は空白にせず中空枠に
します。通常GUI（Browser、system bar、Launcher、Network、Battery、Startup）はA4経路を使います。
通常buildはBDF、TTC、TTF、FreeType、Pillowを解析・リンクしません。

## ASCII 1bpp font

GNU Unifont Japanese 17.0.05からprintable ASCIIだけを生成します。生成物
`font/data/tab5font16.bin`は95 glyph、1 range、3,175 byte、CRC-32 `0d9dcf0d`です。
圧縮、PSRAMへのコピー、起動時installは行わず、通常buildと比較buildの両方でDROMを直接読みます。

Consoleは1文字1セルの8×16固定端末です。U+0020は空セル、U+0021–U+007EはASCII glyph、
それ以外のUnicode scalarは8×16の中空枠へ写します。UARTへ送る元のUTF-8は変更しません。

生成元、hash、licenseは[`../tools/font/vendor/PROVENANCE.md`](../tools/font/vendor/PROVENANCE.md)、
形式は[`../tools/font/README.md`](../tools/font/README.md)に記録しています。元BDFを含む全sourceの
取得と再生成はrepository rootから次を実行します。

```sh
make fonts
make fonts-check
```

## A4 UI font

LatinはDejaVu Sans／Sans MonoのEnglish Latin set 210文字を16／24／32 pxに事前ラスタライズします。
Browser本文、link、見出しとGUI既定は比例幅Sans、`pre`・`code`系とClockはSans Monoです。
日本語はNoto Sans CJK JP Regularの**16 pxだけ**を収録し、32 px表示は同じA4 bitmapを整数2倍で
描画します。日本語24 px strikeは持たず、24 px styleで日本語を要求した場合も16 pxで描きます。

日本語repertoireはJIS X 0213第1面、かな、カタカナ、CJK約物、全角／半角互換形、人名異体字と、
UIが使う`←`、`→`、`↻`、`✓`と`𠮷`です。BMP外のJIS第1面26文字も含む8,923 glyphをすべて
収録します。そのうち8,735 glyphはNotoのoutlineから生成し、Notoに無い、16 px line boxへ
収まらない、またはcombining markである188 glyphはUnifontの1bpp形状をA4 containerへ値0／15として
格納します。combining markをoutlineから単独描画すると暗黙のbase用bearingでcell外へ広がるため、
Unifontのoverlay位置を優先します。coverageは維持しますが、この188 glyphには中間階調がありません。

各glyph recordはcode point、bitmap幅・高さ、signed bearing、advance、A4 bitmap offsetを持ちます。
layoutとrendererは同じadvanceを参照します。combining markは直前のglyphへ背景なしで重ね、幅0です。
未収録文字は16 pxまたは32 pxの中空枠になり、折返しとhit testの幅も描画に一致します。

A4の0は書込みを省略し、15は前景色を直接使い、中間値はRGB565のchannelごとに前景と背景を
blendします。背景指定時はadvance×line boxを先に消去します。`fonttest`の比較専用
`paint_glyph_1bpp`は同じA4 glyphをcoverage 8で二値化するため、輪郭とmetricsを変えずに
anti-aliasの効果だけを左右比較できます。

`make fonts`が生成する`ui-font/data/tab5-ui-fonts.bin`は次の構成です。

| 内容 | glyph数 | bitmap byte |
| --- | ---: | ---: |
| DejaVu Sans 16／24／32 px | 210 × 3 | 60,321 |
| DejaVu Sans Mono 16／24／32 px | 210 × 3 | 59,009 |
| Noto Sans CJK JP 16 px | 8,923 | 738,877 |
| metadata | — | 163,072 |
| 合計 | 10,183 | 1,021,279 |

blobのCRC-32は`f43cfa02`、SHA-256は
`d5a86be6c6b01b167bb24981835f0d2ac196c746607195327ce756baddbc72a1`です。strike別の値は
`ui-font/data/tab5-ui-fonts.txt`、由来とlicenseは`tools/ui-font/vendor/`にあります。
Unifont BDF、DejaVu Sans／Sans Mono TTF、19,484,784 byteのNoto TTCと1,021,279 byteのA4 blobは
repositoryへcommitせず、`.gitignore`対象です。repository rootの`make fonts`が版を固定した配布URLから
3種のsource fontをdownloadし、配布物と使用ファイルのSHA-256を検証してASCIIとA4を再生成します。
検証済みsourceが既にあればdownloadしません。新しいcheckoutではCargo buildより先に一度実行する
必要があります。A4 blobが無い場合、build scriptは`run make fonts`を示して失敗します。生成物の
再現性確認は`make fonts-check`です。DejaVuの抽出には`dpkg-deb`と`tar`を使用します。

## LZ4と配置

LZ4を使うのはA4 blobだけです。build scriptが1,021,279 byteの平文blobを792,089 byteの
T5L4 containerへ変換してDROMへリンクします。PSRAM allocator初期化直後に最終bufferへ直接展開し、
wrapper、圧縮block、展開長、font header、layout、両CRCを検査してからlookupへ公開します。
別の大きな作業buffer、LZ4 frame、外部dictionary、XOR scrambleは使いません。ASCII blobは
3,175 byteのままDROMにあり、このinstall処理の対象外です。

LZ4は最高の圧縮率よりdecoderの単純さと展開速度を優先して採用しました。展開済み出力をmatch元に
できるためPSRAM上の最終bufferへ直接展開でき、decoderはallocatorやstreaming stateを持たず、
入力・出力境界、不正offset、長さoverflowを局所的に検査できます。

比較feature `font-drom-direct`はA4平文blobもDROMから直接参照し、A4用のPSRAM確保、LZ4展開、CRCを
省略します。ASCIIはどちらのbuildでもDROM直接です。既定buildはDROM/IROM境界`0x40110000`、
比較buildは平文A4を収めるため`0x40150000`です。既定buildの新しい日本語A4構成は実機未確認です。
以前の小さいA4 blobで行ったPSRAM対DROMのscroll比較結果は
[`SCALABLE_PROPORTIONAL_FONT_PLAN.md`](SCALABLE_PROPORTIONAL_FONT_PLAN.md)に履歴として残します。

## モジュールと検査

| パス | 役割 |
| --- | --- |
| `font/src/lib.rs` | ASCII glyph、replacement、advance、text width |
| `font/src/console.rs` | ASCII console cell ID変換 |
| `ui-font/src/lib.rs` | A4 lookup、metrics、PSRAM install、RGB565 painter |
| `font-codec/src/lib.rs` | build時LZ4 encoder、起動時decoder、T5L4 wrapper／CRC |
| `src/font.rs` | firmware側exportとA4 PSRAM install |
| `src/framebuffer.rs` | 1bpp／A4 text renderer |
| `tools/font/`、`tools/ui-font/` | manifest、生成tool、元font、license |

host testはASCII範囲、console mapping、A4全glyph、比例幅／固定幅advance、日本語16 px、32 px整数拡大、
combining、blend、clip、opaque消去、LZ4 malformed inputを検査します。`fonttest`は1枚目に常時DROMの
ASCII、2枚目にLatin／日本語A4、3枚目にA4対二値化を表示します。

## 対象外

日本語IME、かな漢字変換、CSS Web Font、可変font、runtime outline renderer、SDF、kerning、
OpenType shaping、Arabic／Indic等の文脈字形、color emoji、Unicode全plane、runtime font切替は
扱いません。
