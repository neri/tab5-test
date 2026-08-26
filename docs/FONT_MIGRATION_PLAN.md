# 16ピクセルUnicodeビットマップフォント移行計画

> 索引: [`../DESIGN.md`](../DESIGN.md)
> この文書は作業計画と実機での判断記録です。現在の実装仕様は現状文書と
> コードを優先してください。

## 状態: 未着手

| Stage | 内容 | 状態 |
| --- | --- | --- |
| 0 | 現状値、収録文字、ライセンス、試験文字列の固定 | 未着手 |
| 1 | 16pxフォントのsubset生成と`no_std`フォントcrate | 未着手 |
| 2 | 8×16／16×16共通glyph rendererと表示診断 | 未着手 |
| 3 | コンソールの8px固定セル化と半角表示 | 未着手 |
| 4 | ブラウザの可変送りlayoutと日本語表示 | 未着手 |
| 5 | 残る全画面UIの移行と5×7フォントの削除 | 未着手 |
| 6 | XIP配置、表示帯域、実機回帰、現状文書の更新 | 未着手 |

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
