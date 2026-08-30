# 16ピクセルUnicodeビットマップフォント移行計画

> 索引: [`../DESIGN.md`](../DESIGN.md)
> この文書は作業計画と実機での判断記録です。現在の実装仕様は現状文書と
> コードを優先してください。

## 状態: 完了（2026-08-27）

| Stage | 内容 | 状態 |
| --- | --- | --- |
| 0 | 現状値、収録文字、ライセンス、試験文字列の固定 | 完了（実機見比べはStage 2へ繰り延べ） |
| 1 | 16pxフォントのsubset生成と`no_std`フォントcrate | 完了 |
| 2 | 8×16／16×16共通glyph rendererと表示診断 | 完了 |
| 3 | コンソールの8px固定セル化と半角表示 | 完了 |
| 4 | ブラウザの可変送りlayoutと日本語表示 | 完了 |
| 5 | 残る全画面UIの移行と5×7フォントの削除 | 完了 |
| 6 | XIP配置、表示帯域、実機回帰、現状文書の更新 | 完了 |

## 背景

現在の描画APIは`src/framebuffer/font.rs`の5×7 ASCIIフォントだけを持ち、通常文字は
6×8の送り枠を2倍して表示する。コンソールは12×16 pixelの固定セルを104列×44行
持ち、ブラウザも1文字を同じ12 pixel幅として折り返す。非ASCII文字はブラウザでは
中空の四角、コンソールでは空白になる。

この構成はハードウェア立ち上げ時の最小コンソールとしては有効だったが、Webページ、
ファイル名、SSIDなどのUTF-8文字列を読む画面としては不足している。16 pixelの日本語
bitmap fontを別経路として足すだけでは、同じ画面に解像度の違う2種類の文字が残り、
ブラウザの折返しやリンクのhit判定も描画結果と一致しない。

従って本計画では、ブラウザだけに日本語フォントを追加せず、**通常文字の基準を
半角8×16／全角16×16へ統一し、5×7フォントを最終的に削除する**。ただしコンソールは
全角表示の利用頻度に対して2セル管理の複雑さが大きいため、16px高の共通描画基盤を使いながら
8px幅の半角固定端末とする。入力方式の変更ではなく表示基盤の変更であり、日本語入力は対象外とする。

## 到達目標

- ASCII・ラテン文字を8×16、かな・漢字などを16×16で、整数倍率だけを使って描く
- コンソール、ブラウザ、toolbar、status、各全画面アプリが同じ16px glyphデータと
  rendererを使い、可変幅UIは同じadvance計算を使う
- コンソールを行高16 pixelのまま8 pixel単位の156列×44行へ変更する
- コンソールは1文字1セルの半角固定とし、全角・結合文字を空白ではなく8×16の可視
  placeholderへ変換する。元のUTF-8出力はUARTで失わない
- ブラウザの折返し、piece幅、リンク下線、選択背景、pointer hit判定を実際のadvanceと
  一致させる
- フォントはFlash XIP上の読み取り専用データとし、PSRAM heapやSDカードを必要としない
- フォントデータの由来、版、変換手順、ライセンス、hashをリポジトリ内で再現可能にする
- 既存の部分再描画、scroll copy、cache同期、表示DMA underrun 0という条件を維持する

## 採用フォント

### 元データ

第一候補は[GNU Unifont-JP 17.0.05](https://unifoundry.com/unifont/index.html)の16 pixel
bitmap glyphとする。Unifont-JPの`JP`は日本語だけのsubsetという意味ではなく、Unicodeの
CJK統合漢字に日本式字形を選ぶvariantである。Plane 0 BDFは8×16または16×16の二幅を持ち、
57,086 glyphを収録する。

Unifont 13.0.04以降のフォントはSIL Open Font License 1.1と、GNU Font Embedding
Exception付きGPLv2+のdual licenseである。本プロジェクトではOFL 1.1側を採用し、元の
license本文、copyright、版、取得元、配布archiveのSHA-256をフォントデータと一緒に保持する。
subset化、row/column転置、格納形式の変更は派生フォントの生成として扱い、生成物から
license情報を切り離さない。

### 全BMP版をそのまま採用しない理由

調査時に公式`unifont_jp-17.0.05.bdf`を解析した結果は次のとおりだった。容量はBDFの
テキストサイズではなく、半角を16 byte、全角を32 byteとした生bitmap量である。

| 領域 | glyph数 | 生bitmap量 |
| --- | ---: | ---: |
| CJK統合漢字 | 20,992 | 656.0 KiB |
| CJK統合漢字拡張A | 6,592 | 206.0 KiB |
| ハングル音節 | 11,172 | 349.1 KiB |
| CJK互換漢字 | 512 | 16.0 KiB |
| ハングル字母 | 464 | 14.5 KiB |
| 日本語かな・約物 | 351 | 10.0 KiB |
| Yi文字 | 1,232 | 38.5 KiB |
| ラテン文字 | 848 | 14.3 KiB |
| ギリシャ・コプト・キリル文字 | 1,072 | 19.4 KiB |
| ヘブライ・アラビアなど | 1,888 | 38.7 KiB |
| 南・東南アジア系文字 | 3,968 | 115.1 KiB |
| アフリカ系文字 | 784 | 24.4 KiB |
| 記号・句読点・数学・矢印 | 3,072 | 69.7 KiB |
| 全角・半角互換形 | 177 | 4.6 KiB |
| その他Plane 0 | 3,530 | 80.4 KiB |
| **合計** | **57,086** | **1,710,240 byte** |

lookup indexと幅情報を足した単純な組み込み形式は約1.83 MB（約1.75 MiB）になる。
このうち約75%はCJK全域とハングルであり、日本語の本文表示だけを目的にすると過剰である。
また、Plane 0の未割当位置にもcode pointを示す代替glyphが入り、私用領域、surrogate、
最後のnoncharacter 2個以外をほぼ埋める設計である。

### 採用subset

初期subsetは次を収録する。

- ASCII、Latin-1、Latin ExtendedのWeb本文とファイル名で使う範囲
- JIS X 0213のかな、漢字、記号
- 一般句読点、通貨記号、矢印、数学記号、罫線、幾何学記号
- 日本語の全角・半角互換形
- JIS X 0213だけでは落ちる人名・地名の異体字を明示リストで追加する
  （少なくとも`髙`、`﨑`、`𠮷`を試験文字に含める）
- U+FFFD replacement character

調査時の実測では、JIS第一・第二水準相当の7,171 glyphをUnifont-JPの16px bitmapで
持つ場合、code point indexと幅bitを含め約237,463 byte（約232 KiB）だった。JIS X 0213の
Plane 0部分11,096 glyphでは約366,091 byte（約357.5 KiB）だった。Plane 0外の303文字、
Latin拡張、追加異体字を含め、**生成物512 KiB以下**をStage 1の上限とする。期待値は
約400〜450 KiBである。

上限へ収めるために文字を黙って落とさない。追加リストと各Unicode範囲を機械可読のmanifestに
し、生成時に収録数、半角／全角数、最大code point、最終byte数を表示する。512 KiBを超えたら
生成を失敗させ、DROM境界を広げるか収録方針を変更する判断をこの文書へ記録する。

### 代替候補の位置付け

[美咲ゴシック](https://littlelimit.net/misaki.htm)は8×8、JIS第一・第二水準を持ち、
調査時の7,171 glyphを組み込み形式へ正規化すると約72,607 byteだった。小容量でlicenseも
改変・商用利用・再配布を許すが、漢字の多くを7×7 dotへ収めるため、16px高の長文では
Unifont-JPの真の16×16 glyphより情報量が少ない。低容量fallbackやレトロUIの選択肢にはなるが、
本計画の既定フォントにはしない。

`k8x12`やM+ bitmap 10/12 dotは字形の候補になるが、現在の16px行高に整数倍率で一致しない。
非整数scaleはdotの太さを不均一にするため採らない。字形を変更するときもrendererとlayoutの
契約は8／16 pixel advance、16 pixel高に固定する。

## 文字サイズとUI規則

### 通常文字

| 種類 | glyph box | advance | 用途 |
| --- | ---: | ---: | --- |
| 半角 | 8×16 | 8 px | ASCII、Latin、半角カナなど |
| 全角 | 16×16 | 16 px | かな、漢字、全角記号など |
| combining mark | 16×16内 | 0 px | 直前glyphへ重ねる |
| 欠落glyph | 16×16 | 16 px | 非ASCIIの未収録文字を中空枠等で示す |

advanceはUnicode範囲をrendererと可変幅layoutで別々に推測せず、フォントcrateの同じlookup結果を
正とする。これにより半角カナ、通貨記号、ambiguous width文字でも描画、折返し、hit判定が
ずれない。ASCIIの欠落は8px、非ASCIIの欠落は16pxとして必ず可視のplaceholderを描く。
コンソールだけはこのadvanceをlayoutに使わず、半角glyphは8pxで表示し、それ以外は8×16の
コンソール専用placeholderへ写して常に1セル進める。

結合濁点U+3099／U+309Aなどは0 advanceで直前glyphへ重ねる。variation selectorとZWJは
初期版では異体字選択や合字を実装せず、幅を増やさない。先頭に単独で現れたcombining markは
情報を失わないplaceholderとして1全角分を使う。

### 拡大文字

bitmapは整数倍率だけを許す。

- 本文、コンソール、toolbar、status: 16px、1倍
- 大見出し、診断の大きい数値: 32px、2倍
- 太字: 同じglyphを右へ1 physical pixelずらして二度描きする現行方式を当面維持
- 旧5×7の3倍表示に相当する24pxは廃止し、画面ごとに16px太字または32pxへ選び直す

16pxから24pxへの1.5倍拡大は採らない。source pixelごとの幅が1pxと2pxで交互になり、
bitmap fontとしての輪郭が崩れるためである。

## モジュール構成

### `font/` workspace crate

`no_std`の新しいworkspace member（package名の候補は`tab5-font`）を作り、次を一箇所へ置く。

- 生成済みsubset binary
- code pointからglyphを引くlookup
- glyphの8／16px advance、combining属性
- 欠落時の幅とreplacement方針、およびコンソール用の1セルglyph変換
- host側で動くlookup／coverage／幅のunit test

firmware crateと`tab5-browser`の両方がこのcrateへ依存する。ブラウザcrateへFramebufferを
依存させず、純粋な`advance(char)`だけをhost testでも使えるようにする。root crateと
browser crateがUnicode幅判定を複製してはならない。

### 生成物形式

通常buildでBDFやOpenTypeを解析しない。host側の変換toolが版を固定した元データと収録manifestを
読み、column-majorのcompact binaryを生成し、検査済み生成物をリポジトリへ置く。通常buildは
`include_bytes!`相当でそれをDROMへリンクするだけにする。

初期形式は実装容易性を優先し、次を候補とする。

- 昇順のUnicode scalar index。Plane 0外を含むため`u32`
- 1 glyphあたり固定32 byteの16×16 column data。半角は未使用側を0で埋める
- 半角／全角とcombining属性のbit table
- headerにmagic、format version、glyph count、各領域offset、生成物CRC

固定32 byteは半角glyphで16 byteを浪費するが、JIS X 0213規模では約180 KiB以下の差であり、
glyph位置を即座に計算できる。512 KiB上限に入らない場合だけ、16／32 byte可変dataとoffset表を
比較する。圧縮のためにruntime decompressorやPSRAM展開を導入しない。

Framebufferはlogical Xがnative addressの逆順へ写るため、既存5×7と同様にcolumn-majorを使う。
変換toolがBDFのBBXとoffsetを16×16 canvasへ正規化した後に転置し、rendererは毎文字の描画時に
row/column変換を行わない。

### 描画API

API名は実装時に調整してよいが、責務を次に分ける。

```rust
pub struct Glyph {
    pub columns: [u16; 16],
    pub advance: u8,       // 0, 8, 16
}

impl Framebuffer {
    pub fn draw_glyph(
        &mut self,
        x: usize,
        y: usize,
        glyph: &Glyph,
        scale: usize,
        foreground: u16,
        background: Option<u16>,
    );

    pub fn draw_text(/* UTF-8 text and style */) -> DrawnWidth;
}
```

`draw_glyph`はglyph lookupを行わず、pixelを描くだけにする。`draw_text`または呼び出し側の
text iteratorがlookup、combining、advanceを扱う。opaque描画はadvance box全体を1回で塗り、
sparse描画は立っているbitだけを書くという現在の分離を維持する。

## コンソール設計

### grid

画面1280×720、左右margin各16px、上margin 8pxを維持する場合、8×16セルでは次になる。

```text
COLUMNS = (1280 - 16 * 2) / 8 = 156
ROWS    = (720 - 8) / 16      = 44
```

現在の104×44から156×44となり、縦の情報量を減らさず1行を50%長くできる。現在のASCIIセルは
12×16 = 192 pixelだが、新しいASCIIセルは8×16 = 128 pixelなので、1文字のopaque再描画量は
約33%減る。すべての表示文字が1セルなので、部分再描画、cursor、scrollの範囲計算も固定幅のまま
維持できる。

現在の`cells: [[char; COLUMNS]; ROWS]`は1セル4 byteで、104×44セルに18,304 byteを使う。
新しいgridはUnicode scalarを直接保持せず、表示用glyph IDを`u8`で保持する。156×44セルでも
6,864 byteとなり、現在より11,440 byte減る見込みである。実装時にはcell以外の属性配列を含む
実測値をlink mapで確認し、release linkの残りstack 128 KiB以上という既存ASSERTを維持する。

### 半角固定契約

コンソールは1セル8×16 pixel、1文字1セルの固定端末とし、全角glyphを2セルへまたがって表示しない。
cellには次のいずれかを表す`u8`のglyph IDを保持する。

- ASCIIと、コンソール用repertoireに明示した8×16の半角glyph
- 空白
- 制御文字を除く表示不能文字に使う8×16の可視placeholder

UTF-8出力はbyte単位ではなくUnicode scalar単位で走査する。全角文字、combining mark、未収録文字は
それぞれ1 scalarにつきplaceholder 1セルへ変換し、空白へ落とさない。これにより文字の内容はLCD上で
読めなくても存在とおおよその文字数は分かる。診断情報として必要な元文字列は従来どおりUARTへUTF-8で
出力し、コンソール用変換によってUART側を置き換えない。

シェルのコマンド入力は当面ASCIIだけなので、`Submission`、引数parser、CardKB／USB keyboardの
入力契約は変更しない。cursor、Backspace、Delete、Home／End、入力行の退避と復元は常に1セル単位で
扱い、全角lead／continuation、0 advance、Unicode表示幅の状態をコンソールへ導入しない。

### 折返しと整形

`write_output_line_pixels`にあるASCII外を空白へ変換する処理は、上記のconsole glyph変換へ置き換える。
表示可能な半角glyphもplaceholderも必ず1列を使い、156セルに達した時点だけで折り返す。

`ls`の複数列配置、右寄せfield、件数表示などは、UTF-8 byte数ではなくconsole glyph変換後のセル数を
使う。初期版では制御文字を除く1 Unicode scalarが1セルになるため`chars().count()`と一致するが、
呼び出し側がその実装詳細を前提にせず、共通の`console_cell_count`相当を使う。

## ブラウザ設計

現在の`browser::layout::Metrics`は1文字の`char_width`を一つだけ持ち、blockを「何文字入るか」で
折り返し、piece幅も`chars().count() * advance`で求める。これをpixel budget方式へ変える。

- `next_line`は文字数上限ではなく、各文字のadvance合計がviewport幅へ収まる位置を探す
- `push_pieces`も同じadvance関数でpiece幅と次のXを求める
- 下線、選択背景、link hit、touch／mouse hitは保存したpixel幅を使う
- ASCII spaceでのword wrapを維持し、日本語はspace無しでもpixel端で進む
- 最低限の禁則として、`、。）」』】〉》〕］｝`等を行頭へ、`（「『【〈《〔［｛`等を
  行末へ孤立させない
- 禁則で1文字も置けなくなる狭い幅では必ず1文字進め、無限loopにしない
- preformatted textもadvanceで端を判定し、内容を失わず折り返す

bodyは16px 1倍、h1／h2は32px 2倍とする。h3以降は16px太字を基本にする。行間はglyph boxの
25%という現行方針を維持する場合、body 20px、h1／h2 40pxとなる。実機で詰まり具合を見て
割合を変えるときは、本文と見出しで別の場当たり的pixel値を持たず、この文書へ理由を残す。

組み込みページへ日本語fixtureを追加し、network無しで描画を確認できるようにする。fixtureには
半角／全角混在、句読点、リンク途中の折返し、combining濁点、人名異体字、未収録emojiを含める。

## 起動とFlash配置

5×7の現行`FONT`は640 byteを`.data.font`へ置く。16px subsetは数百KiBなので内部RAMへ置かず、
DROMから直接参照する。最初のFramebufferコンソール描画はPSRAM初期化とpre/post DROM/IROM cold
probeの後、通常IROM上の`app::run`から行われる。DROMが読めない状態では通常アプリコードも
継続できないため、5×7だけを緊急LCD表示用にHP SRAMへ残す意味はない。起動失敗の診断は従来どおり
UARTを正とする。

調査時のrelease ELFでは、固定長DROM末尾のzero paddingは約64,379 byteだった。美咲の約72.6 KiB
さえ現行境界には収まらず、16px subsetではDROM／IROM境界の移動が必要である。

`memory.x`の変更は次の条件で決める。

- 生成したfont、既存rodata、最低1ページ（64 KiB）の将来余裕が入る位置までDROMを広げる
- IROM開始も同じ64 KiB境界へ動かし、DROM payload終端とIROM payloadのpage offset一致を保つ
- XIP窓の終端`0x40400000`と、ESP-IDF ECO2 bootloaderが要求するXIP segment 2本を変えない
- 512 KiB上限のfontなら、IROM開始は概ね`0x400a0000`〜`0x400b0000`で足りる見込みだが、
  推測値で固定せず生成後のlink mapから決める
- 現行IROMコードは調査時約0.74 MiBであり、この境界でも2.5 MiB前後のIROM余裕が残ることを確認する
- factory partition `0x10000..0x400000`へRAM load segmentを含む最終imageが収まることを検査する

フォントを`storage` partitionやSDへ置く案は初期実装では採らない。起動後に別媒体をmountし、glyphを
cacheし、抜去やI/O失敗を画面描画中に扱う複雑さに対して、512 KiB以下のDROM subsetで得る節約が
小さいためである。将来、収録言語を大幅に増やしてDROMがIROMを圧迫した時点で別計画にする。

## 実装ステージ

### Stage 0: 現状と選択を固定する

- 現行releaseのDROM実使用量、zero padding、IROM、`.data`、`.bss`、残りstack、ESP imageサイズを記録
- 現行コンソールの104×44、cell repaint、scroll、full redrawの時間とunderrunを測定
- Unifont-JP 17.0.05元データ、license、SHA-256を固定
- subset manifestと代表試験文字列を決める
- 5×7 scale 2、美咲ゴシック scale 2、Unifont-JP 16px nativeの同じ日本語／英数字見本を実機表示し、
  Unifont-JPを採用する視認性判断を写真または観測記録として残す

**完了条件:** 変更前の数値、元データのhash、収録規則、目視で選んだ理由がこの文書へ記録される。

### Stage 1: subset生成とフォントcrate

- BDF／Unifont hexから16×16 canvasへ正規化し、column-major binaryを生成するhost toolを追加
- 収録manifest、追加異体字リスト、license、provenanceを追加
- `font/` workspace crateで`glyph(char)`、`advance(char)`、`is_combining(char)`を公開
- 半角glyph IDまたは8×16 placeholderへ変換する`console_glyph(char)`相当を公開し、
  すべてのUnicode scalarが必ず1セルへ写ることをtestする
- 生成物のmagic、version、offset、glyph count、CRCをcompile時またはunit testで検査
- index昇順、重複無し、Unicode scalarのみ、全角／半角数、512 KiB上限を生成時に検査
- 代表glyphのbitmap hashと、欠落文字のfallbackをhost testで固定

**完了条件:** host testだけでデータ破損、幅ずれ、収録漏れを検出でき、通常buildは元フォントを
解析せず生成済みbinaryを読む。まだ画面の描画は変えない。

### Stage 2: 共通rendererと表示診断

- `Framebuffer`へ16-column glyphのopaque／sparse painterを追加
- 半角8px、全角16px、0 advance combining、scale 1／2、画面端clipを実装
- 旧`draw_text`を直ちに意味変更せず、新APIへ移す呼び出しを段階的に増やす
- ASCII、かな、単純／複雑な漢字、記号、欠落glyph、色、背景、太字を並べる`fonttest`診断を追加
- glyph行の帯ごとにC6 linkをserviceする必要性をブラウザと同じ基準で確認

**完了条件:** network無しの実機で代表文字が欠けず、背景あり再描画で古いglyph／cursor pixelが
残らない。scale 1／2とも表示DMA underrunが0で、部分flush範囲が実glyph幅と一致する。

### Stage 3: コンソール移行

- CELL_WIDTHを8、CELL_HEIGHTを16とし156×44へ変更
- cellを`char`から表示用の`u8` glyph IDへ変更し、空白、半角glyph、placeholderを保持
- 出力のASCII外を空白にするfilterを、Unicode scalarごとの1セルconsole glyph変換へ置き換える
- 全角、combining、未収録文字が各1セルの可視placeholderになり、byte数分へ分裂しないことをtest
- cursor、Backspace、Delete、Home／End、入力途中へのautomount出力、scroll、clearを回帰確認
- `ls`等の列整形をconsole glyph変換後のセル数へ変更
- コンソールの入力と`Submission`はASCIIのまま維持
- LCD用変換の前後でUARTへ出すUTF-8文字列が変化しないことを確認

**完了条件:** ASCIIコマンド編集が従来どおり動き、半角文字とplaceholderが常に1セルで揃う。
全角を含む長いUTF-8出力、右端折返し、連続scrollでもcursorと行境界がずれず、UARTでは元のUTF-8を
確認できる。cell用静的RAMが見積もりどおり減り、stack 128 KiB以上を保つ。

### Stage 4: ブラウザ移行

- `tab5-browser`をfont crateのadvanceへ接続し、文字数layoutをpixel layoutへ変更
- mixed-widthの折返し、piece幅、link順、hit判定、禁則のhost testを追加
- browser rendererを16px glyphへ変更し、placeholder専用描画を共通fallbackへまとめる
- toolbar／statusはASCII固定の前提を外すが、URL入力自体はASCII契約を維持
- 組み込み日本語fixtureでscroll、選択、touch／mouse click、太字、code色、下線を確認

**完了条件:** 日本語fixtureの全文字、改行位置、link hit範囲がhost期待値と実機表示で一致し、
長文scroll中もC6のlink serviceを止めず、underrunを発生させない。

### Stage 5: 全画面UI移行と5×7削除

- `win`、Wi-Fi menu、battery、axis、paint、touch／coordinate診断の`draw_text`呼び出しを移す
- 旧24px相当の見出しを16px太字または32pxへ画面ごとに選び、固定座標を調整
- すべての呼び出しが新rendererへ移った後に`src/framebuffer/font.rs`の5×7 table、
  `ascii_or_space`、`draw_ascii_char`を削除
- `.data.font`の640 byteを内部RAMから除去
- rendererが二系統残っていないことを`rg`とcode reviewで確認

**完了条件:** 全画面モードで文字の切れ、重なり、画面外描画が無く、5×7 glyph tableと旧APIへの
参照が0件になる。

### Stage 6: 配置・実機受入・文書同期

- font実サイズに合わせて`memory.x`のDROM／IROM境界を64 KiB単位で変更
- release build、`check_elf_layout.py`、ESP image検査を通す
- XIP segment 2本、RAM load segment、entry／critical closure、stack ASSERTを確認
- 起動時のpre/post DROM／IROM cold probeを実機で通す
- コンソール連続出力、末尾scroll、全画面復帰、browser長文scroll、全UI遷移を実機確認
- `dp 100`、利用可能なら`di 30`と`ui`診断を実行し、underrun 0を確認
- [`GRAPHICS.md`](GRAPHICS.md)、[`CONSOLE_SHELL.md`](CONSOLE_SHELL.md)、
  [`BROWSER.md`](BROWSER.md)、[`APPS.md`](APPS.md)、[`BOOT.md`](BOOT.md)、
  [`FILE_LAYOUT.md`](FILE_LAYOUT.md)、[`../DESIGN.md`](../DESIGN.md)を実装結果へ同期
- `README.md`は変更せず、記述が古くなった箇所だけを最終報告する

**完了条件:** host test、release検査、実機matrixが合格し、最終font byte数、glyph数、DROM／IROM、
内部RAM、imageサイズ、描画時間、underrun数をこの文書へ記録する。

## 実機受入matrix

| ケース | 期待結果 |
| --- | --- |
| 起動直後 | cold XIP probe後に16pxコンソールが出て、旧5×7へ依存しない |
| ASCII入力編集 | 挿入、削除、Home／End、cursor点滅、Enterが従来どおり動く |
| 非半角コンソール出力 | 1 Unicode scalarが可視placeholder 1セルになり、右端折返しとscroll後も列がずれない |
| コンソールとUART | LCDでplaceholderになった日本語をUARTでは元のUTF-8で確認できる |
| 入力途中のautomount出力 | 入力行を退避・復元しても固定幅の出力とcursorが壊れない |
| `ls` | 日本語名を1文字1 placeholderで示し、列幅と右端が揃う |
| browser組み込みfixture | 日本語本文、句読点、link、太字、下線、禁則が期待どおり |
| browser pointer | 全角を含むlinkの左右端でhit範囲が描画と一致する |
| 未収録文字 | 空白や`?`ではなくreplacement glyphで1文字の存在が分かる |
| browser combining | 分解濁点が余分な全角幅を取らず直前文字へ重なる |
| 全画面UI巡回 | 文字の切れ、24px旧前提の位置ずれ、復帰後の残像が無い |
| 表示帯域 | 部分更新、scroll、full redraw、browser scrollでunderrun 0 |
| 再起動 | 複数回のcold bootでfont DROM参照とscanout開始が安定する |

## 中止・見直し条件

- font subsetが512 KiBを超え、収録根拠のない文字削減が必要になった場合
- DROM拡大後のIROM余裕が、既存機能の成長を考慮して1 MiB未満になる場合
- console cell表現の変更後に想定外の静的RAM増加が生じ、release stackが128 KiB未満になる場合
- glyph lookupや描画がC6 link serviceを長時間止める場合
- full redrawまたは通常の連続出力で表示DMA underrunが再発し、既存の部分更新契約を
  維持できない場合
- font license、元データ、生成手順、hashのいずれかをリポジトリ内で追跡できない場合

中止条件に当たったStageを完了扱いにしない。subset範囲、cell表現、DROM配置、font選択の
どれを戻すかを実測値とともにこの文書へ追記する。

## 対象外

- 日本語IME、かな漢字変換、Unicode keyboard入力
- CSS Web Font、ページ指定font、可変font、outline font renderer
- anti-alias、subpixel rendering、RGB以外のglyph bitmap
- kerning、比例幅Latin font、OpenType shaping
- Arabic／Indic等の文脈字形、ligature、完全なbidi
- color emoji、Unicode全plane、全CJK拡張の初期収録
- Shift_JIS、EUC-JP等からUTF-8へのcharset変換
- TLS／HTTPS、CSS、JavaScriptなどブラウザ本体の別機能
- runtimeでのSD／USB／network font読み込みとfont切り替え

## 実装時に残す判断記録

各Stageの完了時に少なくとも次を追記する。

- 使用したUnifont版、archive SHA-256、license file名
- subset manifestの版、glyph総数、半角／全角／combining数、最終byte数
- 代表文字の収録／欠落結果と、追加異体字の根拠
- host test数と対象
- console列×行、cell byte数、静的RAM増減、残りstack
- DROM／IROM境界、各section実使用量、ESP imageサイズとsegment数
- console cell repaint、scroll、full redraw、browser 1画面描画の時間
- 実機matrixの実行結果、underrun数、未確認項目と理由
- 5×7削除後に古くなったREADME.mdの箇所（README自体は変更しない）

## Stage 0の記録

### 変更前の実測値

`cargo build --release`のELFを`tools/check_elf_layout.py`と`readelf`で測った値。

| 項目 | 値 |
| --- | ---: |
| `.flash.rodata`（DROM）領域 | 196,312 byte |
| うち実使用（末尾の0埋めを除く） | 137,965 byte |
| 末尾zero padding | 58,347 byte |
| `.flash.appdesc` | 256 byte |
| `.flash.text`（IROM） | 806,896 byte |
| `.iram.text` | 10,100 byte |
| `.dram.rodata` | 1,364 byte |
| `.data` | 19,100 byte |
| `.bss` | 53,248 byte |
| `.stack` | 178,176 byte |

DROMは`ROM_RODATA`（`0x40000020`から`0x0002ffd8`）を必ず埋め切る構成なので、領域サイズは
固定で、余裕は末尾zero paddingの58,347 byteである。349.5 KiBのフォントはこの余裕に入らない。
`memory.x`のDROM／IROM境界移動が必要になる。

### 計画からの変更: 境界移動をStage 6からStage 2へ前倒しする

計画では`memory.x`の変更をStage 6に置いていたが、フォントをfirmwareへリンクした時点で
DROMが溢れてlinkが通らないため、実際にリンクが必要になるStage 2で行う。Stage 6では
最終サイズに合わせた再調整と、image検査・実機受入だけを扱う。

### 元データとlicense

| 項目 | 値 |
| --- | --- |
| フォント | GNU Unifont Japanese 17.0.05（BDFの`FONT_VERSION`） |
| ファイル | `tools/font/vendor/unifont_jp-17.0.05.bdf.gz` |
| archive SHA-256 | `d3a4c98e41efcf38b49bd520a049230cc040d44433ab9c2cdcd9f1f481443976` |
| 展開後BDF SHA-256 | `044463a47a5b320a1281dcd15fcb3010d6a4ec19603e4193bf28d12909cd009c` |
| license file | `tools/font/vendor/LICENSE-OFL-1.1.txt`（SIL OFL 1.1） |
| 由来の記録 | `tools/font/vendor/PROVENANCE.md` |

元データはリポジトリへcommitした。ネットワーク無しで生成物を再現できることを優先している。
生成toolが検証するのは展開後BDFのhashで、gzipの再圧縮やファイル名変更の影響を受けない。

### 収録範囲の決定

計画では「JIS X 0213のかな、漢字、記号」とだけ書いていた。実測して次のように決めた。

| 候補 | glyph数 | 生成物 |
| --- | ---: | ---: |
| JIS X 0208のみ | 6,879 | 約249 KiB |
| **JIS X 0213 第1面（第1〜3水準）** | **8,773** | **採用** |
| JIS X 0213 第1面＋第2面 | 14,368 | 約490 KiB |

第1面＋第2面は512 KiB上限まで20 KiB強しか残らず、以後の追加余地がほぼ無い。第1面までとし、
人名異体字はmanifestの明示追加で補う方針にした。実際の生成物は他の範囲を含めて349.5 KiBで、
上限まで162 KiBの余裕がある。

### Plane 0外の文字を落とす判断

`unifont_jp-17.0.05.bdf`はPlane 0だけを収録する（`ENCODING`の最大は65533）。JIS X 0213
第1面のうち26文字はBMPの外にあり、`𠮷` U+20BB7（第2面）も同様に元データに無い。これらを
入れるには`unifont_upper-17.0.05`の追加取得が必要になるため、初期版ではBMPのみとした。

落とした26文字は`font/data/tab5font16.txt`に一覧として記録される。表示は空白ではなく
16px幅の欠落placeholderになり、`advance`も16pxを返すので折返しとhit判定はずれない。
`tab5-font`のhost testが`𠮷`の欠落とplaceholderへのfallbackを固定している。

### 5×7／美咲／Unifontの実機見比べ

Stage 0では行わず、Stage 2の`fonttest`診断で実機確認する。Unifont-JPの採用はこの計画の
「代替候補の位置付け」で既に決まっており、Stage 0のためだけに捨てる表示コードを書くより、
本実装のrendererで見るほうが判断材料として正確なためである。視認性が期待に届かない場合は
Stage 2の完了条件を満たさないものとして、この節へ結果を追記する。

### 試験文字列

`fonttest`診断とブラウザのfixtureで共通に使う。

```text
ASCII      The quick brown fox jumps over the lazy dog 0123456789 !"#$%&'()
かな       あいうえお アイウエオ ぁぃぅぇぉ ヴヵヶ がぎぐげご ぱぴぷぺぽ
漢字       日本語表示 東京都渋谷区 髙﨑 灣鬱靄 一二三四五六七八九十
記号       、。・「」『』（）［］｛｝〜―…※＿ ￥＄€£ →←↑↓ ①②③ ★☆■□◆
半角カナ   ｱｲｳｴｵ ｶﾞｷﾞｸﾞ ｰ｡､･｢｣
Latin      àéîõü ÀÉÎÕÜ ß æ œ Ł ż Ġ
結合       が き゚ é (U+3099 U+309A U+0301)
欠落       𠮷 😀 ﷽ (BMP外・emoji・未収録)
```

## Stage 1の記録

### 生成tool

| パス | 役割 |
| --- | --- |
| `tools/font/generate.py` | BDFとmanifestから`font/data/tab5font16.bin`を生成・検査 |
| `tools/font/manifest.txt` | 収録するUnicode範囲。人が編集する |
| `tools/font/repertoire/jisx0213-plane1.txt` | JIS X 0213第1面の範囲展開（4,650範囲、8,773文字） |
| `tools/font/derive_jisx0213.py` | 上記の再生成。CPythonの`euc_jis_2004` codecを使う |
| `tools/font/README.md` | 形式と実行方法 |

JIS漢字はUnicode上で連続しないため、manifestで「JIS X 0213」と名前で指定して生成時に
展開すると、生成物が実行したCPythonの版に依存する。展開結果のほうをcommitし、再生成用の
scriptを別に置く形にした。`generate.py --check`はcommit済み生成物が最新かだけを検査する。

### 生成物

| 項目 | 値 |
| --- | ---: |
| glyph数 | 9,747 |
| 半角（advance 8） | 1,382 |
| 全角（advance 16） | 8,327 |
| combining（advance 0） | 38 |
| 範囲数 | 4,528 |
| 最大code point | U+FFFD |
| byte数 | 357,907（349.5 KiB / 上限512 KiB） |
| payload CRC-32 | `e0fefdc3` |
| manifestが要求してBMP外で落とした数 | 26 |
| manifestが要求して元データに無かった数 | 0 |

同じ内容を`font/data/tab5font16.txt`へ生成報告として書き出し、commitしている。生成物の
差分をレビューするときはこのファイルを見る。

### 形式の変更: code point index → 範囲表

計画は「昇順のUnicode scalar index」を候補にしていたが、glyph 1件ごとにu32を持つと
9,747件で38,988 byteになる。連続runへまとめた範囲表（u32先頭 + u16個数 + u16先頭glyph、
1件8 byte）は4,528件で36,224 byteと、この規模ではほぼ同じ大きさになる。にもかかわらず
範囲表を採ったのは、収録範囲を広げるほど差が開く（連続範囲を足してもrun数は増えない）
ためである。lookupは範囲表の二分探索1回で、glyph位置はindexから即座に計算できる。

半角glyphも固定32 byteで持つ点は計画のままとした。可変長にすると1,382件で約20 KiB縮むが、
上限まで162 KiBの余裕があるため、offset表を足す価値がない。

幅とcombining属性はbit tableではなく、glyph 1件につき1 byteのadvance値（0／8／16）で
持つ。9,747 byteで、2 bit packingとの差は7 KiB程度である。rendererとlayoutが必要とする
値そのものを持つほうが、bit演算を挟むより取り違えにくい。

### `tab5-font` crate

`font/`をworkspace memberに追加した。`no_std`で依存無し、`cargo test`ではhostへ向けて
buildする（`tab5-browser`と同じ構成）。公開API:

- `glyph(char) -> Option<Glyph>`、`glyph_or_replacement(char) -> Glyph`
- `advance(char) -> u8`、`is_combining(char) -> bool`、`text_width(&str) -> usize`
- `console::id(char) -> u8`、`console::character(u8) -> Option<char>`、
  `console::cell_glyph(u8) -> Glyph`、`console::cell_count(&str) -> usize`

magic、format version、各offset、区画の連続性は`const _: () = assert!(...)`でcompile時に
検査する。CRCと中身はhost testで検査する。

コンソールのセルIDは1 byteで、次の空間を使う。

| id | 内容 |
| --- | --- |
| 0 | 空白 |
| 1 | placeholder（1セルで表示できない文字） |
| 2..=95 | U+0021..U+007E |
| 96..=191 | U+00A0..U+00FF |
| 192..=254 | U+FF61..U+FF9F（半角カナ） |
| 255 | 予備。placeholderとして描く |

U+00AD SOFT HYPHENだけはLatin-1を連続させるためにidを持つが、Unifontがこの文字を16px幅の
code point boxとして描くため、コンソールではplaceholderへ写す。`generate.py`のrepertoire
検査も同じ1文字だけを除外する。

### combining markの描画位置（Stage 2への申し送り）

Unifontはcombining markのinkを、直前の文字のcellへ重ねる前提の位置に置いている。
U+3099は16幅boxの右上（列12と14）、U+0301は8幅boxの中央付近（列2〜5）である。したがって
rendererはcombining markを`pen_x - 直前のadvance`へ、**必ずsparse（背景を塗らない）で**
描く。opaqueで描くと16列のboxが次のcellを消す。

### host test

`cargo test -p tab5-font --target x86_64-unknown-linux-gnu`で18件。`mise run test`は
`tab5-browser`（163件）と合わせて実行するようにした。

| 対象 | 件数 |
| --- | ---: |
| データ整合（CRC、範囲の昇順・重複無し・glyph数一致、advance値、半角の右半分が空） | 5 |
| lookup（全収録文字のindex往復、ASCII／かな／漢字／半角カナの幅、combining） | 3 |
| 収録と欠落（`髙`、`﨑`、`𠮷`欠落、emoji欠落、U+FFFD、`A`のbitmap固定） | 4 |
| 幅の合計（`text_width`） | 1 |
| コンソール（id往復、id一意、全idが8px、非表示文字がplaceholder、全scalarが1セル、`cell_count`） | 5 |

`every_scalar_takes_exactly_one_cell`はU+0000からU+10FFFFまで実際に回して、どのscalarも
1セルに写り、空白へ落ちないことを確認している。

### この時点で変えていないもの

画面の描画は何も変えていない。`src/framebuffer/font.rs`の5×7フォントと`draw_text`、
`draw_ascii_char`はそのままで、firmware crateはまだ`tab5-font`へ依存していない。DROMを
溢れさせないため、依存の追加は`memory.x`の境界移動と同じStage 2で行う。

## Stage 2の記録

### 追加した描画API

`Framebuffer`へ2つ足した。5×7側は名前を変えただけで意味は変えていない。

| API | 内容 |
| --- | --- |
| `draw_glyph` | 16 pixel glyphを1つ描く。lookupはしない |
| `draw_text` | UTF-8を描き、占めた幅をpixelで返す |
| `draw_text_5x7` | 旧`draw_text`。改名のみ |
| `draw_ascii_char_5x7` | 旧`draw_ascii_char`。改名のみ |

旧APIを改名したのは、移行の残りを`rg draw_text_5x7`で数えられるようにするためである。
Stage 5では改名した2つを消すだけになる。

`draw_glyph`はglyphの幅の枠だけを塗る。半角8列、全角16列、combining markは16列である。
描画本体は`WideGlyph::paint_opaque`と`WideGlyph::paint_sparse`で、既存の`Glyph`と同じ
「背景ありは枠内を全部書いて列単位で回転を解決、背景なしは立っているbitだけ書く」分離を
16列 × `u16`へ移したものである。

`draw_text`の規則:

- 送りは`font::advance`のみ。rendererが独自にUnicode範囲を判定する箇所は無い
- 未収録文字は同じ幅の中空枠。空白にはしない
- combining markは`pen_x - 直前のadvance`へ、必ず背景なしで描く
- 直前が無いcombining markはU+FFFDを1文字として描き、その幅だけ進む
- `\n`は開始xへ戻り、`font::HEIGHT * scale`だけ下げる
- 戻り値は最も広い行の幅

### `memory.x`の境界移動

| 項目 | 変更前 | 変更後 |
| --- | ---: | ---: |
| `ROM_RODATA` LENGTH | `0x0002ffd8` | `0x0008ffd8` |
| `ROM_TEXT` ORIGIN | `0x40030000` | `0x40090000` |
| `ROM_TEXT` LENGTH | `0x003d0000` | `0x00370000` |

結果（`tools/check_elf_layout.py`と`readelf`）:

| 項目 | 値 |
| --- | ---: |
| DROM領域 | 589,528 byte |
| DROM実使用 | 497,997 byte |
| DROM末尾zero padding | 91,531 byte（89.4 KiB） |
| IROM | 815,126 byte |
| IROM余裕 | 約2.83 MiB |
| `.iram.text` | 10,100 byte（変化なし） |
| `.data` | 19,100 byte（変化なし） |
| `.bss` | 53,248 byte（変化なし） |
| `.stack` | 178,176 byte（変化なし） |
| ESP image | 1,435,568 byte（factory partition 4,128,768 byteに対し34.8%） |

`tools/check_esp_image.py`はXIP segment 2本、page整合、RAM load 2本を確認済み。DROMを
広げた分はimageサイズに乗るが、mapされるだけなのでRAMは増えない。

### `fonttest`診断

`src/app/font_test.rs`。`fonttest`コマンドと、`ui`受入試験の2番目のstageとして起動する。
1画面に次を並べる。

- ASCII、かな、漢字、記号、半角カナ、Latin／ギリシャ／キリルの見本
- combining mark 3種が直前の文字へ重なり幅を増やさないこと
- 直前が無いcombining markがU+FFFDになること
- 未収録文字（BMP外、emoji、subset外）が中空枠になること
- 前景色5色、太字、2倍拡大
- 背景ありの再描画: 16セル分の`M`を赤地青で描いた上に、同じ128 pixelの枠を全角8文字で
  塗り直す。赤や青が残ればopaque描画が枠を塗り切っていない
- 罫線の枠と塗り潰しブロック: `\n`の行送りがglyph高と、送りがglyph幅と1 pixelでも
  ずれていれば角が割れる
- 16 pixelの日本語本文4行。可読性の判断はここで行う

静止画面で、network serviceは行わない。全画面のtextでもPSRAMへの書き込みは数ms規模で、
ブラウザが長文scroll中にC6 linkをserviceする基準の間隔には届かないためである。

### 実装前のoff-board検証

`font/data/tab5font16.bin`から`draw_text`と同じ手順でこの画面をhost側で1280×720へ
描き起こし、送り、combining位置、opaque枠、罫線の連結、orphan markのU+FFFD化を目視で
確認してから実機へ渡している。実機で見るべきものはpixel配置ではなく、パネル上の
可読性、回転・clip経路、underrunである。

### 計画からの変更

- `memory.x`の境界移動をStage 6からここへ前倒しした（Stage 0の記録に理由）
- 旧`draw_text`／`draw_ascii_char`を`_5x7`付きへ改名した。計画は「旧`draw_text`を
  直ちに意味変更しない」としており、意味は変えていない
- `fonttest`を`ui`受入試験のstageにも入れた。Stage 6で`ui`のunderrunを見るときに
  16px描画が対象へ入る

### 実機確認

`fonttest`の目視は合格（2026-08-27）。代表文字の欠落なし、背景ありの再描画で残像なし、
罫線と塗り潰しブロックの継ぎ目なし、16pxの日本語本文の可読性も問題なしと確認された。
これをもってUnifont-JPの採用判断（Stage 0から繰り延べた見比べ）を確定とする。

underrunはStage 3と合わせて計測した（下記）。0件のためStage 2の完了条件を満たす。

## Stage 3の記録

### grid

| 項目 | 変更前 | 変更後 |
| --- | ---: | ---: |
| セル | 12×16 pixel（5×7を2倍） | 8×16 pixel（16px半角glyph、拡大なし） |
| 列×行 | 104×44 | 156×44 |
| セル1個の型 | `char`（4 byte） | `font::console::Id`（1 byte） |
| セル配列 | 18,304 byte | 6,864 byte |
| `MAX_LINE` | 102 | 154 |

`COLUMNS == 156 && ROWS == 44`は`src/console.rs`のcompile時assertで固定した。margin
（左右16px、上8px）は変えていない。

### 半角固定の契約

出力はUnicode scalar単位で走査し、`font::console::id`でどのscalarも必ず1セルへ写す。

- ASCII、Latin-1、半角カナ → そのglyph
- 空白 → 空セル
- それ以外（全角、combining mark、フォントに無い文字、制御文字）→ 8×16の可視placeholder

`write_output_line_pixels`にあった「ASCII外を空白にする」filterはこの変換に置き換えた。
空白へは落とさない。UART側は従来どおり元のUTF-8をそのまま出す。

コマンド入力はASCIIのまま。`Submission`、引数parser、入力の契約は変えていない。cursor、
Backspace、Delete、Home／End、入力行の退避と復元は1セル単位のままで、全角のlead／
continuationやUnicode表示幅の状態はコンソールへ入っていない。cellから入力行を復元する
`ascii_of`は、`font::console::character`でIDを文字へ戻す。

`ls`の複数列配置は`chars().count()`ではなく`font::console::cell_count`を使うようにした。
初期版では両者が一致するが、呼び出し側がその一致を前提にしないためである。

### 静的RAM

| 項目 | 変更前 | 変更後 | 差 |
| --- | ---: | ---: | ---: |
| `.data` | 19,100 | 7,712 | −11,388 |
| `.bss` | 53,248 | 53,248 | 0 |
| `.stack` | 178,176 | 189,440 | +11,264 |
| IROM | 815,126 | 814,058 | −1,068 |
| ESP image | 1,435,568 | 1,423,120 | −12,448 |

残りstackは189,440 byteで、`memory.x`の128 KiB ASSERTに対して十分な余裕がある。
見積り（−11,440 byte）とのわずかな差はPROMPTとalignmentによる。

### 実装前のoff-board検証

`font/data/tab5font16.bin`から`render_cell`と同じ変換でコンソール画面を1280×720へ
描き起こし、156列に収まること、日本語が1文字1 placeholderになること、半角カナと
Latin-1が実glyphで出ることを確認してから実機へ渡している。

### 実機確認（2026-08-27）

ASCIIのコマンド編集、日本語を含む出力の1セル1文字表示、右端折返しとscroll、UART側の
元UTF-8、`ls`の列揃えを実機で確認した。表示帯域は次のとおりで、Stage 2の16px描画と
Stage 3のコンソールをまとめて計測している。

```text
ui visual: underruns=0 dma_error=0x00000000
dp: production phase=0ms burst=128 ICM=15/15
production 0ms  b128  5989us  0/100  f200
```

`ui`は全画面モード巡回（座標チャート、font sheet、paint、multi-touch、axis、desktop）と
console復帰を含めてunderrun 0、DMA error 0。`dp 100`はproduction経路の全画面更新100回で
underrun 0（`0/100`）、1回あたり5,989 us、経過200 frame。8×16セルと16px glyphの描画は
既存の部分更新・scroll・full redrawの帯域条件を崩していない。

## Stage 4の記録

### layoutをpixel budgetへ

`tab5-browser`が`tab5-font`へ依存するようになった。ブラウザcrateの依存はこれ1つで、
hardware crateではなくhostでもbuildできるデータとlookupである。行を折るのとpixelを
置くのが同じ`advance`を使うことが、描画と測定を一致させる唯一の方法である。

| 変更 | 前 | 後 |
| --- | --- | --- |
| `Metrics` | `char_width`, `glyph_height`, `line_gap_percent` | `glyph_height`, `line_gap_percent` |
| 幅 | `chars().count() * char_width * scale` | `advance(char) * scale`の合計 |
| `next_line` | 文字数上限 | pixel budget |
| `BODY_SCALE` | 2（5×7で16px） | 1（16pxフォント等倍） |
| `HEADING_SCALE` | 3（24px） | 2（32px） |
| 行送り | 本文20px、見出し30px | 本文20px、見出し40px |
| ASCII 1行 | 106文字 | 160文字 |

`char_width`は消した。indentだけは文字数で指定するので、半角1文字分を
`layout::CELL_WIDTH`（8 pixel）として公開している。`Piece::width`と`Piece::x`は
最初からpixelだったので、下線、選択背景、link hit、pointer hitは`Piece`を読むだけで
実際のadvanceと一致するようになった。

### 禁則

`next_line`が決めた位置が禁則に触れる場合だけ、最大4文字前まで切り直す。

- 行頭に置かない: 句読点、閉じ括弧、小書きかな、長音符、半角の対応字
- 行末に置かない: 開き括弧

4文字前までで見つからない場合と、切り直すと行が空になる場合は幅どおりの位置で切る。
必ず1文字は進むので、viewportがglyph 1個より狭くても無限loopにならない。`pre`は
書かれたとおりに出すので適用しない。

### renderer

- `CELL_WIDTH`は`layout::CELL_WIDTH`（8）、`CELL_HEIGHT`は`font::HEIGHT`（16）を参照
- `CHROME_SCALE`を2から1へ。chromeの文字の高さは16 pixelのままで、1文字が12 pixelから
  8 pixelになったのでaddress欄に入る文字数が63から116へ増えた
- `draw_text_run`は`Framebuffer::draw_text`1回（太字はもう1回）になった。文字ごとの
  ASCII判定とplaceholder描画は無くなり、欠落文字の枠はフォント側の責任へ移った
- `draw_clipped`は文字数ではなくpixelでclipする。status messageは今後ASCIIとは限らない
- `draw_placeholder`と`PLACEHOLDER_INSET`を削除
- この時点で`draw_ascii_char_5x7`の呼び出しが0になったので、`Framebuffer`の
  5×7 1文字描画（`draw_ascii_char_5x7`と`Glyph` painter）も削除した。Stage 5に残るのは
  `draw_text_5x7`とfont tableだけである

URL入力自体はASCII契約のまま。address欄のcaretは半角1文字＝8 pixelの前提で位置を出す。

### 組み込みfixture

`http://built-in/japanese`を追加し、ホームからlinkした。network無しで次を確認できる。

- 半角と全角の混在した本文の折返し
- 句読点の禁則（実際に「…確かめられま／す。」で切り直しが起きる）
- 行をまたぐリンク（2行にまたがり、両方に下線が付く）
- 分解された濁点`か`+U+3099、`き`+U+309A、`e`+U+0301
- 人名の異体字`髙`と`﨑`
- 未収録文字`𠮷`、emoji、U+FDFD
- 半角カナ、Latin-1、全角英数
- `pre`のpixel折返し

`/sample`の「非ASCIIはglyphが無いので枠になる」という記述も実態に合わせた。

### host test

`tab5-browser`は163件から175件へ。追加した12件:

| 対象 | 件数 |
| --- | ---: |
| 幅（全角＝半角の2倍、combining 0、scaleの掛かり方） | 2 |
| mixed-widthの折返しが実際にpixel端で止まること | 1 |
| `Piece`幅が自分のtextの実測幅と一致すること、pieceが隙間なく並ぶこと | 2 |
| 全角の後ろのlinkのhit範囲が描画と一致すること | 1 |
| 禁則（行頭の閉じ括弧、行頭の句点、行末の開き括弧、`pre`は適用外） | 4 |
| 禁則が行を空にする前に諦めること | 1 |
| glyph 1個より狭いviewportでも進むこと | 1 |

既存testのうち「106文字」を前提にしていた3件は、`WIDTH / CELL_WIDTH`から導く形へ
直した。幅を変えてもtestの意図が変わらないようにするためである。

### 実装前のoff-board検証

日本語fixtureを`Layout`にかけて行とpieceを書き出し、その座標と幅から
`font/data/tab5font16.bin`のglyphで1280×720へ描き起こして確認した。禁則の切り直し、
2行にまたがるlinkの下線、欠落文字の枠、markerの右寄せ、`pre`の空白保持が
期待どおりであることを見てから実機へ渡している。

### 実機確認（Stage 6で実施済み）

着手時は次を未実施として残していた。

- 日本語fixtureの全文字、改行位置、link hit範囲がhost期待値と一致するか
- 全角を含むlinkの左右端でhit範囲が描画と一致するか
- 長文scroll中もC6のlink serviceが止まらず、underrunが出ないか
- toolbarとstatusの表示、address編集、太字、code色、下線

いずれもStage 6の「実機回帰: 目視（2026-08-27）」と「実機受入matrixの結果」で確認し、
合格している。

## Stage 5の記録

### 倍率の読み替え

旧5×7は1文字6×8の枠を拡大していた。16pxフォントでは枠が8×16なので、同じ見た目の
高さになる倍率が変わる。

| 旧scale | 旧の高さ | 新scale | 新の高さ |
| ---: | ---: | ---: | ---: |
| 2 | 16 px | 1 | 16 px |
| 3 | 24 px | 2 | 32 px |
| 4 | 32 px | 2 | 32 px |
| 5 | 40 px | 2 | 32 px |

計画どおり24 pxは廃止した。旧scale 3は画面ごとに32 pxか16 pxのどちらかへ寄せている。
`win`のタスクバーとウィンドウタイトルは枠の高さが32 pxと30 pxで32 pxの文字が入らない
ため16 pxを選び、`wifi_menu`と`battery`と`axis_test`の見出しは32 pxにした。

1文字の幅は12 pxから8 pxへ、あるいは18 pxから16 pxへ変わるので、固定座標で中央寄せや
右寄せをしていた箇所はずれる。数え上げた文字数に一定幅を掛ける計算は残さず、
`font::text_width`で実測して置く形へ変えた。

- `Framebuffer::draw_coordinate_chart`: 軸ラベルの中央寄せ、タイトルの中央寄せ、
  右下・右上の隅ラベル（`draw_corner_label`）
- `wifi_menu`: `centred`と`centred_in`を追加し、画面中央とパネル中央の文字に使う
- `battery`: 「WAITING FOR INA226 DATA」の中央寄せ、電池図形の中のパーセント表示。
  後者は元が固定offsetで、1〜4桁のうち一番長いときしか中央に来ていなかった
- `win`: `CLOCK_TEXT_WIDTH`、`ICON_LABEL_WIDTH`、`ICON_BOUNDS_HEIGHT`、
  `STATUS_LINE_HEIGHT`をセル定数から導出する形へ

`axis_test`のタイトルはy=12のままだと32 pxで`HUD_TOP`（40）へ4 px食い込み、HUDの
毎フレーム消去で下端が削れる。y=8へ移して8..40でちょうど接するようにした。

### 5×7の削除

- `src/framebuffer/font5x7.rs`（旧`font.rs`）を削除
- `Framebuffer::draw_text_5x7`、`draw_ascii_char_5x7`、`ascii_or_space`、
  5×7用の`Glyph` painterを削除
- `.data.font`の640 byteが内部RAMから消えた
- `rg '5x7|5×7|draw_text_5x7|draw_ascii_char_5x7'`で残るのは`src/font.rs`の
  「これが置き換えた」という1行だけ。`*_PLAN.md`の当時の記録はそのまま残す

`draw_ascii_char_5x7`の呼び出しはStage 4のブラウザ移行時点で0になっていたので、
5×7 1文字描画とその painter はそこで先に削除している。

### section

| 項目 | Stage 4後 | Stage 5後 | 差 |
| --- | ---: | ---: | ---: |
| `.data` | 7,712 | 7,072 | −640（5×7 table） |
| `.stack` | 189,440 | 189,952 | +512 |
| IROM | 816,734 | 817,234 | +500 |
| DROM実使用 | 497,997 | 500,149 | +2,152 |
| DROM padding | 91,531 | 89,379（87.3 KiB） | −2,152 |
| ESP image | 1,425,792 | 1,425,648 | −144 |

## Stage 6の記録（配置と文書）

### 配置

`memory.x`はStage 2で移した境界のままで足りる。最終のfontは349.5 KiB、DROM実使用は
500,149 byte、末尾paddingは87.3 KiBで、計画が求める64 KiBの余裕を上回る。IROMは
817,234 byteで、`ROM_TEXT`の3.44 MiBに対して約2.65 MiBの余裕がある。

`tools/check_elf_layout.py`と`tools/check_esp_image.py`はいずれも合格。XIP segmentは
2本、物理／仮想のpage内offsetは一致、RAM load segmentは2本、ESP imageは1,425,648 byteで
factory partition（4,128,768 byte）の34.5%。

### 現状文書

| 文書 | 変更 |
| --- | --- |
| [`FONT.md`](FONT.md) | 新規。文字幅の契約、収録範囲、モジュール、形式、再生成、配置 |
| [`../DESIGN.md`](../DESIGN.md) | ドキュメント構成へ`FONT.md`を追加 |
| [`GRAPHICS.md`](GRAPHICS.md) | `draw_glyph`／`draw_text`の説明、5×7経路の記述を削除 |
| [`CONSOLE_SHELL.md`](CONSOLE_SHELL.md) | 156列×44行、セルID、半角固定の契約、`cell_count` |
| [`BROWSER.md`](BROWSER.md) | pixel layout、禁則、日本語fixture、非ASCIIの表示 |
| [`APPS.md`](APPS.md) | `fonttest`の節、行間、太字の記述 |
| [`BOOT.md`](BOOT.md) | DROM／IROM境界、paddingの余裕 |
| [`FILE_LAYOUT.md`](FILE_LAYOUT.md) | `src/font.rs`、`font/`、`src/app/font_test.rs`、`font5x7.rs`の削除 |
| [`FILESYSTEM.md`](FILESYSTEM.md) | `ls`の折返し桁数を156へ、桁数の数え方 |
| [`USB.md`](USB.md) | 文字列記述子をASCIIへ畳む理由 |
| [`DISPLAY_BANDWIDTH.md`](DISPLAY_BANDWIDTH.md) | 実測表の「コンソール1セル」注記 |

`*_PLAN.md`の当時の記述（`WEB_BROWSER_PLAN.md`、`PPA_FILL_PLAN.md`の5×7への言及）は
その時点の判断記録なので変更していない。

### 実機回帰: 表示帯域（2026-08-27）

Stage 4とStage 5を載せた状態で計測した。

```text
dp: production phase=0ms burst=128 ICM=15/15
production 0ms  b128  5989us  0/100  f200

di: idle soak 30 minutes ICM=15/15
idle       0ms  b128  17468us  0/103200  f103200
```

`dp 100`はproduction経路の全画面更新100回でunderrun 0、1回あたり5,989 us。`di 30`は
30分のidle走査103,200 frameでunderrun 0。値はStage 3時点の`dp`と同じで、16px描画と
8×16セルは表示帯域の条件を何も変えていない。

### 実機回帰: 目視（2026-08-27）

次を実機で確認し、いずれも合格。

- `ui`の全画面モード巡回。文字の切れ、旧24px前提の位置ずれ、console復帰後の残像が
  無いこと。Stage 5で座標を変えた`win`（タスクバー時計、アイコンキャプション、
  ウィンドウ内テキスト）、`battery`（電池図形内のパーセント）、`axis_test`
  （タイトルy=12→8）、`coordtest`（ラベルの中央寄せ、隅ラベル）を含む
- ブラウザの日本語fixture。全角を含むlinkの左右端でpointer hitが下線の端と一致すること
- 複数回のcold bootでfont DROM参照とscanout開始が安定すること

## 実機受入matrixの結果

| ケース | 結果 |
| --- | --- |
| 起動直後 | 合格。cold XIP probe後に16pxコンソールが出る。5×7は存在しない |
| ASCII入力編集 | 合格 |
| 非半角コンソール出力 | 合格。1 scalarが可視placeholder 1セル。折返しとscroll後も列が揃う |
| コンソールとUART | 合格。LCDでplaceholderになった日本語をUARTで元のUTF-8として確認 |
| 入力途中のautomount出力 | 合格 |
| `ls` | 合格 |
| browser組み込みfixture | 合格。日本語本文、句読点の禁則、link、太字、下線 |
| browser pointer | 合格。全角を含むlinkの左右端でhit範囲が描画と一致 |
| 未収録文字 | 合格。空白や`?`ではなく中空枠 |
| browser combining | 合格。分解濁点が余分な全角幅を取らず直前文字へ重なる |
| 全画面UI巡回 | 合格 |
| 表示帯域 | 合格。`dp 100`が0/100、`di 30`が0/103,200、`ui`が0 |
| 再起動 | 合格 |

## 最終値

| 項目 | 移行前 | 移行後 |
| --- | ---: | ---: |
| font byte数 | 640（5×7、内部RAM） | 357,907（16px、DROM） |
| glyph数 | 128 | 9,747 |
| DROM領域 | 196,312 | 589,528 |
| DROM実使用 | 137,965 | 500,149 |
| DROM末尾padding | 58,347 | 89,379（87.3 KiB） |
| IROM | 806,896 | 817,234 |
| `.data` | 19,100 | 7,072 |
| `.bss` | 53,248 | 53,248 |
| 残りstack | 178,176 | 189,952 |
| ESP image | 1,435,104 | 1,425,648 |
| コンソール | 104列×44行、`char`セル18,304 byte | 156列×44行、`u8`セル6,864 byte |
| ブラウザASCII 1行 | 106文字 | 160文字 |
| `dp 100` | — | 5,989 us、underrun 0/100 |
| `di 30` | — | underrun 0/103,200 |
| host test | 163（browser） | 175（browser）＋18（font） |

内部RAMは全体で12,668 byte空き、そのうち11,388 byteがコンソールのセル配列、640 byteが
5×7 table、残りがその他である。349.5 KiBのフォントはDROMへ入り、内部RAMを一切使わない。

## README.mdについて

この移行で古くなった記述は`README.md`に無い。フォント、コンソールの桁数、ブラウザの
折返し方式のいずれにも言及していないためである。`README.md`は変更していない。
