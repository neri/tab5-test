# 英字プロポーショナル／スケーラブルフォント表示計画

> 索引: [`../DESIGN.md`](../DESIGN.md)
> この文書は作業計画です。現在の実装仕様は[`FONT.md`](FONT.md)、
> [`GRAPHICS.md`](GRAPHICS.md)、[`BROWSER.md`](BROWSER.md)、[`APPS.md`](APPS.md)と
> コードを優先してください。

## 状態: 未着手

| Stage | 内容 | 状態 |
| --- | --- | --- |
| 0 | 対象画面、文字範囲、比較条件、変更前実測値の固定 | 未着手 |
| 1 | 候補fontのサイズ別生成とhost上の比較 | 未着手 |
| 2 | 共通metrics、fallback、生成済みfont blob | 未着手 |
| 3 | RGB565向けrendererと`fonttest`実機比較 | 未着手 |
| 4 | ブラウザのlayout、描画、URL編集への導入 | 未着手 |
| 5 | 起動画面、Wi-Fi画面への導入 | 未着手 |
| 6 | SDF継続判断または複数strike方式の確定 | 未着手 |
| 7 | release／実機回帰と現状文書の同期 | 未着手 |

Stage 0〜3で方式を選び、Stage 4〜5で利用画面を増やす。Stage 6は必ず「SDFを実装する」
Stageではない。離散的な3サイズで目的を満たせた場合は、SDFを採らないという判断を記録して
完了にする。

## 結論

初期実装の第一候補は、**英字だけをbuild時に16／24／32 pxへラスタライズした、比例幅の
4-bit alpha bitmap strike**である。firmwareはTTF／OTF、BDF／PCF、PNGを解析せず、
生成済みの専用blobからglyph metricsとcolumn-majorのalpha値を読むだけにする。

これは任意のpixel sizeを連続的に生成するoutline rendererではない。ただし今回必要な本文、
中間サイズのGUI label、大見出しの3段階を持ち、glyphごとに固有のadvanceを使うので、現在の
「16 px bitmapを1倍か2倍で表示する」方式に対して次を改善できる。

- `i`と`W`が同じ8 pxを使わない比例幅表示
- 16／24／32 pxそれぞれで作った輪郭とhinting
- alpha coverageによる斜線と曲線の平滑化
- 画面ごとにfont parserやglyph cacheを持たない固定量の実装

TTF／OTFを**実行時形式として採用しない**。outline fontを元データに選ぶ場合も、host側の
生成toolが変更時だけ読み、通常buildとfirmwareにはparser、rasterizer、shaping engineを
リンクしない。現在のUnifont BDFと同じく、生成済みblobをcommitし、通常buildは外部toolや
networkを必要としない。

真に任意倍率が必要だとStage 3の実機比較で判明した場合だけSDFを第二候補とする。GPUの
bilinear samplingやpixel shaderが無いCPU＋RGB565 framebufferでSDFを描くには、各出力pixelで
補間、threshold／smoothstep相当、RGB565 blendが必要になる。現状の1bpp描画より実行時負担が
大きいため、「スケーラブル」という語だけを理由に先に導入しない。

## 背景

現在は全画面でGNU Unifont-JP由来の16 px bitmapを使う。半角は8×16、全角は16×16、
見出しは整数2倍の32 pxで、文字幅の正は`tab5_font::advance`である。ブラウザは同じ関数を
折返し、piece幅、下線、選択背景、pointer hit判定にも使うため、測定と描画が一致している。

この方式は日本語subsetと固定幅コンソールには適するが、英字本文では細い文字も太い文字も
8 pxで進み、16 pxと32 pxの間の大きさを持てない。既存glyphを非整数拡大するとdot幅が不均一に
なり、outline fontをfirmwareで直接読むと、file parser、quadratic／cubic outline、hinting、
anti-alias、glyph cache、失敗処理まで持つことになる。Basic Latinと限られた画面の改善に対して
は責務が大きすぎる。

## 到達目標

- ブラウザの英字を比例幅で表示し、本文／GUI／見出しに少なくとも3つの実用サイズを持つ
- 日本語glyphは現在の16 px bitmapと整数倍率をそのまま使う
- 同じ行に英字と日本語が混在してもbaseline、折返し、下線、選択背景、hit判定を一致させる
- 起動画面、Wi-Fi画面の英字labelへ同じ仕組みを再利用する
- コンソールの8×16固定セル、cell ID、折返し、cursor、scrollを一切変更しない
- 通常buildとfirmwareをfont元形式や外部rasterizerへ依存させない
- fontの由来、版、license、元データhash、生成条件、生成物hashを再現可能にする
- PSRAM allocationを増やさず、既存のcache同期、部分flush、C6 service、underrun 0を維持する

## 対象範囲

### 対象画面

| 画面 | 対象 | 備考 |
| --- | --- | --- |
| ブラウザ本文 | 本文、見出し、link、`pre` | 英字だけ新font、日本語は現行font |
| ブラウザchrome | toolbar、status、アドレス欄、caret | 矢印等の非Basic Latin記号は現行font |
| 起動画面 | title、進捗、案内 | 起動前UART診断や表示初期化順は変更しない |
| Wi-Fi画面 | title、button、status、SSID | SSID内の日本語は現行font |

「一部のGUI」は上の2画面で固定する。`fonttest`は方式比較用に拡張するが、マウス動作確認兼
ジョーク画面の`win`、battery、axis、touch、coordinate、paintとその他の診断画面へは
自動的に広げない。Stage 5の完了後に別画面へ導入する場合は、この表と
[`APPS.md`](APPS.md)を先に更新する。

### 文字範囲

初期の新fontはU+0020〜U+007EのBasic Latin 95文字だけとする。改行、tab、制御文字はglyphでは
なくlayoutが扱う。Latin-1、結合記号、矢印、通貨記号、全角ASCII、かな、漢字は初期blobへ
入れず、現在の`tab5-font`へfallbackする。

英字と日本語が混ざった**文字列全体**ではなく、表示clusterごとにfaceを選ぶ。

- Basic Latin 1文字で、その直後にcombining markが無ければ新しいLatin face
- baseの後ろに現在の`font::is_combining`が認識する文字が続けば、baseを含むcluster全体を
  現在のfontで描く
- それ以外は現在のfont
- 新Latin blobでBasic Latin glyphが欠落していた場合も現在のfont

`e`を新fontで描いた後に、UnifontのU+0301だけを比例幅glyphへ重ねると、markの基準位置が
8 px cellを前提としているためずれる。cluster単位fallbackは、この例外のために完全な
OpenType shapingやUnicode grapheme libraryを持ち込まず、現状と同じ表示を保つ境界である。

### 対象外

- コンソール、シェル入力、console用font IDと固定セル
- マウス動作確認兼ジョーク画面の`win`と、その他の診断画面
- 日本語fontの変更、日本語IME、縦書き、ルビ
- CSS Web Font、ページ指定font、font download
- runtimeでのTTF／OTF／WOFF／BDF／PCF解析
- OpenType shaping、ligature、可変font axis、bidi、Arabic／Indicの文脈字形
- subpixel RGB rendering、color emoji
- SD／USB／networkからのfont読み込み、runtimeのfont切替
- 任意の実数倍率を初期版の完了条件にすること

## 方式の候補

### 比較表

| 方式 | 比例幅 | サイズ | 輪郭品質 | firmware複雑度 | 初期判断 |
| --- | --- | --- | --- | --- | --- |
| 1bpp bitmapをサイズ別に保持 | 対応 | 離散 | 小サイズは明瞭、斜線は jaggy | 最小 | alpha blendが重い場合のfallback |
| **4-bit alpha bitmapをサイズ別に保持** | **対応** | **離散** | **対象サイズでは良い** | **小** | **第一候補** |
| 8-bit SDF＋CPU補間 | 対応 | ある範囲で連続 | 拡大に強い、小サイズは要比較 | 中〜大 | Stage 6まで保留 |
| Hershey等のstroke font | 対応可能 | 連続 | plotter向けで本文の塗りとhintingが弱い | 中 | 診断／図表向け。本文には不採用 |
| TTF／OTF runtime renderer | 対応 | 連続 | 高い | 最大 | 今回は不採用 |

bitmap font descriptorの一般例では、glyphごとに画像矩形だけでなく`xoffset`、`yoffset`、
`xadvance`とkerning pairを持つ。AngelCode BMFontの
[file format](https://angelcode.com/products/bmfont/doc/file_format.html)もこの分離をしている。
本計画ではBMFont parserやtexture pageをそのまま導入せず、このmetricsの分け方だけを
専用blobへ採る。回転Framebufferでは大きい2D atlasより、glyphごとに連続したcolumn-major dataの
ほうが現在のrendererへ合わせやすいためである。

SDFは高解像度の入力から低解像度のdistance textureを作り、描画時に補間して境界を復元する方式で
ある。原方式はGPUのtexture samplingを前提にしており、8-bit channelとbilinear interpolationを
利用する（Valve, [Improved Alpha-Tested Magnification for Vector Textures and Special
Effects](https://steamcdn-a.akamaihd.net/apps/valve/2007/SIGGRAPH2007_AlphaTestedMagnification.pdf)）。
本機で同じ仕事をCPUへ移した場合の費用は論文のGPU上の費用から推定せず、Stage 3でA4 rendererを
測った後に小さいprototypeで判断する。

stroke fontは点列を線で結ぶのでoutline parser無しで拡大できる。Hershey fontの元配布もglyphを
`<x,y>`のpoint-to-point dataと説明している
（[Hershey font data notice](https://github.com/bjnortier/hershey/blob/develop/HERSHEY-LICENSE)）。
ただし単線のplotter字形を画面本文へ使うには太線化、join、cap、hinting相当が必要で、文字の
読みやすさを得ようとするとrenderer側が大きくなる。座標図や大きい数値の別計画には使えても、
今回のbrowser本文の第一候補にはしない。

### 元font形式とruntime形式を分ける

候補選定では次の2案を同じ生成物へ変換して比較する。

1. BDF／PCF等で入手できる比例幅bitmap strikeを正規化する
2. OFL等の再配布可能なoutline fontをhost側だけで16／24／32 pxへラスタライズする

案1は生成toolが単純だが、必要な3サイズ、比例幅、英字の品質、licenseを同時に満たす元fontが
限られる。案2は元ファイルがTTF／OTFでも、複雑さをfont変更時のhost toolへ封じ込められ、
対象サイズごとのhintingとalpha coverageを得られる。従って**案2も「runtimeでTTF／OTFを採用」
したものとは扱わない**。

生成済みblobと報告をcommitし、通常buildは元fontを解析しない。元font、license、取得元、版、
SHA-256もrepositoryへ保持し、生成toolの版とoptionを固定する。生成toolを更新しなければ再生成結果が
byte単位で一致しなければならない。

## 推奨する生成物

### strike

初期候補は16、24、32 pxの3 strikeとする。

| strike | 主用途 | 日本語との混在 |
| ---: | --- | --- |
| 16 px | browser本文、status、小さいlabel | 現行16 px日本語と同じline box |
| 24 px | GUI button、補助title | 日本語が混ざる場合は現行16 pxをbaseline合わせする |
| 32 px | browser `h1`／`h2`、大title | 現行16 px日本語の2倍と同じline box |

24 pxを16 px bitmapの1.5倍で作らず、元fontから24 pxとして生成する。日本語は拡大せず、24 pxの
英字と16 pxの日本語を同じbaselineへ置く。この見た目が不自然なら、24 px styleを日本語混在箇所で
使わない。日本語16 pxを非整数拡大することはfallbackにしない。

### glyph record

形式名とfield幅はStage 1の実測で確定するが、意味は次で固定する。

```text
header
  magic, format_version, strike_count, glyph_count
  section offsets, total length, CRC

strike
  pixel_size, ascent, descent, line_gap
  glyph index range, bitmap offset

glyph
  code_point
  bitmap width, bitmap height
  x bearing, y bearing
  x advance
  bitmap offset, bitmap length

bitmap
  column-major A4 coverage, two pixels per byte
```

bitmap boxとadvance boxを同一にしない。`j`の左overhang、`g`のdescender、spaceの「inkは無いが
advanceはある」を表せない形式は比例幅fontに使わない。初期値はpixel単位の整数metricsとし、
1/64 pixel等のfractional advanceは持たない。

A4はalpha 0〜15で、RGB565とのblend時に15を分母として整数演算する。gamma補正tableや
subpixel renderingは初期版へ入れない。A4とA8の見た目に明確な差があり、A8でも容量と時間の
budgetを満たす場合だけStage 1の判断記録で変更する。

bitmapはPNGのまま埋めず、通常build時のdecoderも持たない。汎用BMFont binary parserも持たず、
本projectが使うfieldだけを持つversion付きblobにする。

### coverageとkerning

初期coverageはBasic Latin 95文字を完全収録し、1文字でも欠けたら生成を失敗させる。使用頻度を
理由にglyphを黙って落とさない。

比例幅とkerningは別の機能である。初期版はglyph固有の`x_advance`を必須とし、kerning pairは
Stage 1で次の両方を測ってから選ぶ。

- pair無しの`AVATAR To Wi-Fi`が実機で不自然か
- pair tableを足した場合のblob byte数、lookup時間、browserの測定APIへの影響

kerningを入れる場合、rendererだけに実装してはならない。layoutの折返し、piece幅、下線、選択背景、
hit判定、caret位置がすべて同じ「直前glyph＋現在glyph」の測定器を使う。複雑さに見合わなければ
初期版はpair無しと明記して完了してよい。

## APIと責務

### 現行font APIを維持する

`font::advance`、`font::text_width`、`font::console`と現在の1bpp glyph APIは意味を変えない。
コンソールと対象外画面が、呼び出し側を変更せず従来の表示を続けられるようにする。

新しいUI向けAPIは別の名前空間に置く。配置は実装時に調整してよいが、概念は次とする。

```rust
pub struct UiTextStyle {
    pub latin_size: LatinSize,      // Px16, Px24, Px32
    pub legacy_scale: u8,           // 1 or 2
    pub weight: Weight,
}

pub struct LineMetrics {
    pub ascent: u8,
    pub descent: u8,
    pub line_gap: u8,
}

pub enum ResolvedGlyph<'a> {
    LatinAlpha(AlphaGlyph<'a>),
    Legacy1bpp(font::Glyph),
}
```

`tab5-font` crateまたは同等の`no_std` crateが、cluster選択、metrics、測定、blob検査を持つ。
Framebufferへのpixel writeとRGB565 blendはfirmware側だけに置く。`tab5-browser`はFramebufferへ
依存せず、同じ純粋な測定APIをhost testから使う。

### 測定はstateful iteratorにする

kerningを後から足してもAPIを再度壊さないよう、`advance(char)`を単独で足すだけの新APIにはしない。
文字列をclusterへ分け、直前glyphの状態も受け取る`measure_run(style, text)`相当を幅の正にする。
rendererも同じiteratorが返す次の情報を使う。

- 選ばれたfaceとglyph
- baselineからの描画offset
- 描画前のkerning（採用時）
- 描画後のadvance
- clusterの元UTF-8範囲

browserのpiece境界がcluster途中へ入らないようにする。単独のcombining markは現行規則どおり
replacementとして進め、無限loopや0幅だけの行を作らない。

### baselineとline box

英字strikeはascent／descentを持つ一方、現行日本語glyphは16×16のtop-left boxである。
Stage 0の見本で現行boxのUI用baseline定数を決め、scale 1／2についてLatinと同じbaselineへ
配置する。consoleの描画位置にはこの定数を使わない。

混在lineのascent、descent、line gapは、そのlineの各styleとfallback faceの最大値を使う。
glyphごとのink boxから行高を変えない。`A`だけの行と`g`を含む行でscroll量が変わるのを防ぐためで
ある。browserのline scrollは引き続き完成済みline単位とする。

## renderer

### RGB565 alpha blend

coverage 0はwriteを飛ばし、15はforegroundを直接書き、1〜14だけ既存pixelまたは明示backgroundと
blendする。背景あり描画ではadvance box全体を指定backgroundで塗った上にglyphをblendし、前に
描いた幅の広い文字やcaretを残さない。背景無しではFramebufferをread-modify-writeする。

回転変換とclipを1 pixelごとに最初から解き直さない。現在のglyph rendererと同じくcolumn-majorを
使い、clipした列ごとにnative連続範囲を進める。A4 unpack、RGB565 channel展開、blend、packの費用を
Stage 3で実測する。

PPAにはglyph alpha blendを任せない。対応操作が確認できておらず、文字のためだけにDMA job、
一時surface、cache ownershipを増やすと、小さい文字列の部分再描画がかえって重くなる。将来の
最適化はCPU版の正しい結果と実測を基準に別Stageで判断する。

### browser中のservice

browser viewport全面再描画中は現在どおりC6 linkを途中でserviceする。新rendererがA4 blendで
遅くなっても、「glyph 8行ごと」という字形依存の単位だけを信用せず、最後にserviceしてからの
経過時間にも上限を置く。Stage 0で現在の最長service間隔を測り、Stage 4ではそれを悪化させない。

pointerを使う画面は、cursorを外す→dirty領域を描く→cursorを載せる→和集合をflush、という
[`APPS.md`](APPS.md)の順を変えない。alpha blendがcursor込みの画素をbackgroundとして読まない
ことも実機で確認する。

## 容量と性能のbudget

Stage 0で変更前の値を記録し、次を初期の判断線とする。満たせない場合は理由と実測をこの文書へ
記録し、A4→1bpp、strike削減、kerning見送り、SDF中止の順で範囲を戻す。

| 項目 | 初期budget |
| --- | ---: |
| Basic Latin 3 strikeのfont blob | 128 KiB以下 |
| release imageの増加（blob、code、metadata合計） | 192 KiB以下 |
| 内部`.data + .bss`増加 | 4 KiB以下 |
| runtime PSRAM／heap allocation | 0 byte |
| browserの文字で埋まったviewport再描画 | 変更前の125%以下、かつ25 ms以下を目標 |
| 通常操作中のDPI FIFO underrun | 0 |
| browser再描画中のC6 link再同期／切断 | 0 |

25 msは絶対合否だけでなく、現在[`APPS.md`](APPS.md)に記録された約17.5 msとの比較値も残す。
PPAによるviewport背景塗りだけで約12 msかかるため、fontだけの費用は全面再描画時間と、背景塗りを
除いたglyph phaseの両方を測る。実機差で25 msをわずかに超えても、125%、service間隔、操作感、
underrunを満たすなら、数値と判断を記録して継続できる。

## 実装Stage

### Stage 0: 契約と変更前の値を固定する

- 対象画面とBasic Latin 95文字を本計画の表と照合する
- browser body 16 px、`h1`／`h2` 32 px、GUI 24 pxというstyle mappingをfixtureで固定する
- 混在見本を固定する: `Wi-Fi設定`, `Tab5 Browser 日本語`, `AVATAR To Wi-Fi`,
  `e\u{301}`, `が`, URL、数字、punctuation、未収録emoji
- 現行fontの混在用baseline候補を16／32 pxで表示し、観測記録を残す
- releaseのDROM／IROM、image、`.data`、`.bss`、残りstackを記録する
- browserの背景塗り、glyph、flush、viewport全体の各時間と、最長C6 service間隔を測る
- 対象GUIの初回描画と部分再描画、underrunを記録する

**完了条件:** 文字、画面、style、baseline候補、容量、時間の変更前値が記録され、Stage 1で
「見た目が良い」以外にも比較できる状態になる。画面のfontはまだ変更しない。

### Stage 1: host上で生成候補を比較する

- 比例幅bitmap source 1候補と、offline outline rasterize 1〜2候補を選ぶ
- 各候補のlicense、版、取得元、元データSHA-256を固定する
- 同じBasic Latin、16／24／32 px、A4条件で専用blobとPNG contact sheetを生成する
- 1bpp、A4、必要ならA8のbyte数を報告する
- advance、bearing、ascent、descent、space、`j`のoverhang、`g`のdescenderを検査する
- 生成を2回行い、blobと報告のhashが一致することをtestする
- 通常buildからgeneratorと元fontを参照しない構成にする

**完了条件:** 3サイズの英字見本、blob容量、license、再現性が揃う。font名だけで決めず、
Stage 3で実機表示する候補を最大2つへ絞る。

### Stage 2: 共通metricsとblob reader

- magic、version、section境界、件数、CRCをhost testまたはcompile時assertで検査する
- Basic Latin 95文字の完全収録、code point順、重複無し、metrics範囲を生成時に検査する
- `UiTextStyle`、cluster選択、`ResolvedGlyph`、line metrics、stateful測定iteratorを追加する
- combining cluster全体がlegacyへfallbackすることをtestする
- Latin欠落時、日本語、全角記号、emojiが現行fontへfallbackすることをtestする
- rendererを使わないhost testでmixed textの幅、cluster境界、line boxを固定する
- 現行の`font::advance`／`text_width`／`console`のtestを無変更で通す

**完了条件:** browser layoutとfirmware rendererが同じiteratorを使える。コンソールのAPI、
データ、表示結果は変わらない。

### Stage 3: rendererと実機比較

- column-major A4 glyphのopaque／transparent painterを追加する
- alpha 0／15のfast path、clip、negative bearing、descender、scale外サイズを検査する
- `fonttest`に現行font、1bpp strike、A4候補を同じ文と色で並べる
- 黒／白／色背景、選択背景、細い線、太字、`AVATAR To Wi-Fi`、混在日本語を表示する
- glyph phaseと全面描画の時間、cache flush範囲、underrunを候補別に記録する
- alpha edgeがRGB565で濁る、細線が消える、背景の古いpixelが残る問題を目視確認する

**判断gate:** A4が容量と性能budgetを満たし、1bppより実機で読みやすければA4を採用する。
A4の差が見えないか遅すぎる場合は1bpp strikeへ戻す。どちらも16／24／32 pxで許容できなければ、
ここで初めてSDF prototypeの費用をStage 6候補として記録する。

### Stage 4: browserへ導入する

- `browser::layout`の幅とline metricsを新しい測定iteratorへ接続する
- body、`h1`／`h2`、`h3`以降、bold、link、`pre`のstyle mappingを一箇所に置く
- piece幅、折返し、禁則、下線、選択背景、touch／mouse hitを同じcluster幅でtestする
- URL欄の表示、左右移動、Home／End、Delete、Backspace、caret x、横clipを比例幅へ対応する
- toolbarの矢印／再読込／全消去等、Basic Latin外の記号は現行fontで描く
- boldはまず現行の1 px二度打ちを使い、別bold strikeは容量と視認性に根拠がある場合だけ足す
- italicの現在の色分けは維持し、synthetic slantやitalic strikeを初期版へ足さない
- 全面再描画中のC6 serviceを経過時間でも分割し、最長停止時間を記録する

**完了条件:** host layout testと実機で折返し、link下線、選択背景、hit範囲、caretが描画と一致する。
日本語は従来glyphのまま、コンソールは差分無し、viewport性能budgetとunderrun 0を満たす。

### Stage 5: 一部GUIへ導入する

- 起動画面、Wi-Fi画面だけにUI text helperを導入する
- 中央揃え／右揃えを新しい測定APIへ変更し、`text_width * scale`の局所計算を残さない
- 24 px labelに日本語が混じる箇所を洗い出し、16 px日本語とのbaselineを実機確認する
- SSID、error文、時刻、button labelが枠を越えた場合のclip／省略規則を固定する
- pointerのあるWi-Fi画面でcursor退避中だけalpha blendすることを確認する
- `win`、battery、axis、touch、coordinate、paint等へ変更が漏れていないことを`rg`で確認する

**完了条件:** 2画面の英字が同じstyle／rendererで描かれ、日本語と記号は現行fontのまま表示される。
部分再描画、pointer、画面遷移、コンソール復帰で残像とunderrunが無い。

### Stage 6: SDFを続けるか確定する

Stage 3〜5の実機結果から次を問う。

1. 実際に16／24／32 px以外を利用者が必要としたか
2. 3 strikeの容量が問題になったか
3. 同じfontを別倍率へ拡大したときだけ解決できるUIがあるか
4. A4 strikeの小サイズ品質に未解決の問題があるか

すべて「いいえ」なら、SDFを不採用として理由と数値を記録し、このStageを完了する。いずれかが
「はい」の場合だけ、Basic Latin 1 strikeのSDF prototypeを`fonttest`限定で作り、次を比較する。

- nearest／bilinear相当の補間とedge smoothingのCPU時間
- A8 SDFのblob容量
- 16、20、24、28、32 pxの細線、corner、`iIl1Wm`
- RGB565 blend後の輪郭
- C6 service間隔とunderrun

SDFがA4の3 strikeより容量を減らし、任意サイズに実需要があり、browser viewportとGUIのperformance
budgetを満たした場合だけ本採用の追補Stageを作る。SDF採用のためにpixel shader相当の汎用描画層や
PSRAM glyph cacheが必要なら、本計画から分離する。

### Stage 7: release、実機回帰、文書同期

- `mise run test`とrelease build、ELF layout、ESP image検査を通す
- font blob、code、DROM／IROM、`.data`、`.bss`、stack、imageの前後差を記録する
- cold boot、browser長文、URL編集、Wi-Fi scan／password、コンソール復帰を確認する
- `bt` fixture、`ui`巡回、`dp 100`、利用可能なら`di 30`を実行する
- glyph描画と全面再描画の時間、最長C6 service間隔、underrun数を記録する
- [`FONT.md`](FONT.md)、[`GRAPHICS.md`](GRAPHICS.md)、[`BROWSER.md`](BROWSER.md)、
  [`APPS.md`](APPS.md)、[`FILE_LAYOUT.md`](FILE_LAYOUT.md)、[`BOOT.md`](BOOT.md)、
  [`../DESIGN.md`](../DESIGN.md)を実装結果へ同期する
- `README.md`は変更せず、古くなった記述があれば最終報告で箇所と不一致を示す

**完了条件:** host test、release検査、実機matrixが合格し、採用方式、font、strike、最終byte数、
時間、未採用候補の理由がこの文書に残る。

## host test matrix

| ケース | 検査内容 |
| --- | --- |
| Basic Latin coverage | U+0020〜U+007Eが全strikeに1件ずつある |
| metrics | advance > 0、bitmap境界、bearing、ascent／descentがformat範囲内 |
| space | bitmap長0でも正のadvanceを持つ |
| proportional | `III` < `MMM`、測定値がglyph advance合計と一致 |
| mixed | `Tab5日本語Wi-Fi`でLatin／legacyの選択と幅が期待どおり |
| combining | `e\u{301}`はcluster全体がlegacyになり、途中でpieceを切らない |
| missing | Basic Latin欠落は生成失敗、範囲外はlegacy fallback |
| kerning | 採用時だけ、測定／描画／折返しが同じpair adjustmentを使う |
| line metrics | `A`と`g`、英字と日本語が混じってもstyle内の行高が安定 |
| browser wrap | piece幅、禁則、link hit、下線、selectionがcluster境界と一致 |
| URL caret | 挿入、削除、左右移動、Home／Endでcaret xが測定結果と一致 |
| blob破損 | magic、version、offset、length、CRC異常を拒否 |
| 再現生成 | 同じ入力とtoolでblob／reportのSHA-256が一致 |
| console回帰 | 現行console glyph ID、cell count、幅testが無変更で通る |

## 実機受入matrix

| ケース | 期待結果 |
| --- | --- |
| `fonttest` | 16／24／32 pxの`iIl1Wm AVATAR`が読め、alpha edgeと背景に残像が無い |
| 英日混在 | baselineと行間が不自然に跳ねず、日本語glyphの形が変更前と同じ |
| browser本文 | 比例幅で折り返し、選択、下線、link hitがpixel単位で一致 |
| browser見出し | 32 px英字と2倍日本語が同じline boxへ収まる |
| URL編集 | 可変幅でもcaret、削除位置、右端clip、全消去がずれない |
| browser長文 | 再描画時間budget、C6 service、scroll、戻る／進むが正常 |
| 起動画面 | cold bootの表示順を変えず、英字titleと日本語案内が欠けない |
| Wi-Fi画面 | SSID混在、password、button、error、pointer再描画が正常 |
| `win` | scope外のまま従来fontとマウス動作確認を変更しない |
| console | 156×44固定セル、入力、cursor、scroll、UART出力が変更前と同じ |
| 画面巡回 | 対象外の診断画面を含め、復帰後の描画崩れとunderrunが0 |
| cold boot反復 | DROM上のfont blobを毎回正しく参照し、CRC／XIP異常が無い |

## 中止・見直し条件

- fontのlicense、元データ、生成tool、hashをrepository内で追跡できない
- Basic Latin 95文字を完全収録できない
- A4／1bppのどちらでも3 strikeが容量budgetへ収まらない
- 内部RAMまたはPSRAMにruntime glyph cacheを置かないと通常操作の性能を満たせない
- browserの測定と描画が別APIになり、折返し、hit、caretのどれかがずれる
- combining cluster fallbackにより既存日本語／結合文字の表示が後退する
- C6 serviceが遅れてlink再同期が起きる、または表示underrunが再発する
- consoleのcell、API、表示、release memoryが変化する

中止条件に当たったStageを完了扱いにしない。新fontを対象画面から外せば現在の
`draw_text`／`font::advance`へ戻れるよう、既存APIと生成物を移行完了まで削除しない。

## 実装時に残す判断記録

各Stageの完了時に少なくとも次を追記する。

- 候補font名、版、license、取得元、元データSHA-256
- generator名／版／option、再生成結果、生成物SHA-256
- strikeごとのglyph数、A4／1bpp／A8 byte数、metadataとkerning byte数
- Basic Latin coverageとfallback試験結果
- baseline、ascent、descent、line gapを選んだ実機上の理由
- kerning採否と`AVATAR To Wi-Fi`の比較結果
- DROM／IROM、release image、`.data`、`.bss`、stackの前後差
- glyph phase、viewport、GUI部分再描画の時間と最長C6 service間隔
- host test数、実機matrix、underrun、未確認項目
- SDFを採用または不採用にした実需要と実測値
- README.mdに古い記述が生じた場合の箇所（README自体は変更しない）
