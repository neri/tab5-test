# 英字プロポーショナル／スケーラブルフォント表示計画

> 索引: [`../../../DESIGN.md`](../../../DESIGN.md)
> この文書は作業計画です。現在の実装仕様は[`FONT.md`](../../FONT.md)、
> [`GRAPHICS.md`](../../GRAPHICS.md)、[`BROWSER.md`](../../BROWSER.md)、[`APPS.md`](../../APPS.md)、
> [`SYSTEM_BAR.md`](../../SYSTEM_BAR.md)、[`GUI_THEME.md`](../../GUI_THEME.md)とコードを
> 優先してください。

## 状態: 実装済み、実機受入待ち

| Stage | 内容 | 状態 |
| --- | --- | --- |
| 0 | 対象画面、文字範囲、比較条件、変更前実測値の固定 | 完了（既存診断記録をbaselineに採用） |
| 1 | 候補fontのサイズ別生成とhost上の比較 | 完了（DejaVu 2.37を採用） |
| 2 | 共通metrics、fallback、生成済みfont blob | 完了 |
| 3 | RGB565向けrendererのhost検証と`fonttest`実機性能比較 | host完了／実機待ち |
| 4 | ブラウザのlayout、描画、URL編集への導入 | 実装・host test完了／実機待ち |
| 5 | 共有バー、Launcher、ミニアプリ、起動画面への導入 | 実装完了／実機待ち |
| 6 | SDF継続判断または複数strike方式の確定 | 実機結果待ち |
| 7 | release／実機回帰と現状文書の同期 | host完了／実機待ち |

Stage 0〜3で方式を選び、Stage 4〜5で利用画面を増やす。Stage 6は必ず「SDFを実装する」
Stageではない。離散的な3サイズで目的を満たせた場合は、SDFを採らないという判断を記録して
完了にする。

## 結論

初期実装の第一候補は、**英字だけをbuild時に必要サイズへラスタライズした、用途別faceの
4-bit alpha bitmap strike**である。Browserの通常本文には比例幅、コード表示と時計には固定幅を
使う。firmwareはTTF／OTF、BDF／PCF、PNGを解析せず、
生成済みの専用blobからglyph metricsとcolumn-majorのalpha値を読むだけにする。

これは任意のpixel sizeを連続的に生成するoutline rendererではない。ただし本文、GUI label、
大見出しの候補となる3段階を持ち、glyphごとに固有のadvanceを使うので、現在の
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

最初に本計画を作った後、GUIは上部48 pxのsystem barを共有する構成へ変わった。現在は
デスクトップ／Browserが通常GUI、Network settings／Battery detailsがミニアプリで、Launcherと
Powerメニューも同じ`system_bar` host内にある。Startupは共有バーが始まる前の初期化画面、
Consoleと専有診断は共有バーを持たない。旧`win`モックアップは廃止され、現在の`win`は文字を
描かない`desktop.rs`へ入る入口である。このため対象はコマンド名ではなく、実際に文字を描く
surfaceとその所有者で決め直す。

この方式は日本語subsetと固定幅コンソールには適するが、英字本文では細い文字も太い文字も
8 pxで進み、16 pxと32 pxの間の大きさを持てない。既存glyphを非整数拡大するとdot幅が不均一に
なり、outline fontをfirmwareで直接読むと、file parser、quadratic／cubic outline、hinting、
anti-alias、glyph cache、失敗処理まで持つことになる。English Latin setと限られた画面の改善に対して
は責務が大きすぎる。

## 到達目標

- Browserの通常本文は比例幅fontを既定とし、製品画面で使う16／32 pxと比較用の24 pxを生成できる
- Browserの`pre`とコード系inline要素、system barのClockには固定幅fontを必ず用意する
- Browser用の比例幅faceはsans-serifを必須とする。serifは初期版へ収録しない
- 日本語glyphは現在の16 px bitmapと整数倍率をそのまま使う
- 同じ行に英字と日本語が混在してもbaseline、折返し、下線、選択背景、hit判定を一致させる
- 新しい英字glyphは全対象サイズでanti-aliasし、中間coverageをRGB565へalpha blendする
- system bar、Launcher／Power、Network settings、Battery details、起動画面の英字へ
  同じ仕組みを再利用する
- Consoleは既存の8×16固定幅font、cell ID、折返し、cursor、scrollを必須互換経路として維持する
- 通常buildとfirmwareをfont元形式や外部rasterizerへ依存させない
- fontの由来、版、license、元データhash、生成条件、生成物hashを再現可能にする
- PSRAM allocationを増やさず、既存のcache同期、部分flush、C6 service、underrun 0を維持する

## 必須品質要件

alpha blendとanti-aliasは初期候補の評価項目ではなく、**新しい英字fontの受入必須条件**とする。
最終方式はA4、A8、SDFのどれでもよいが、glyph境界を1bppへthresholdして描画する方式は
採用しない。

- 新しいLatin glyphは、輪郭pixelに0%と100%以外のcoverageを持てること
- coverage 1〜最大値−1は、foregroundと描画先backgroundを混ぜたRGB565として書くこと
- 明示backgroundありでは、その色とblendする。background無しではFramebufferの既存pixelを
  読んでblendする
- coverage 0のwrite省略と最大coverageの直接writeは最適化として許すが、中間coverageを
  二値化してはならない
- 黒／白だけでなく、選択行の青、警告色、ティールのDesktop上にあるbarなど、対象theme色で
  anti-aliasが機能すること
- 太字の二度打ちを維持する場合も、各passの中間coverageをalpha blendすること

この必須条件は新しい英字faceすべて（比例幅とUI用固定幅）に適用する。日本語とfallbackの
既存1bpp glyph、Console用の既存8×16 glyph、Wi-Fi／Batteryの状態icon、Startupの画像maskを
同じ作業でA4へ変換する要求ではない。

## 対象範囲

### 画面と描画surfaceの分類

| 区分／surface | 所有する実装 | 判定 | 備考 |
| --- | --- | --- | --- |
| Browser本文・status | `browser.rs` | 対象 | 通常本文は比例幅、`pre`とコード系要素は固定幅。見出し、link、最下部statusを含む |
| Browser app領域 | `browser.rs`＋`system_bar.rs` | 対象 | button、address、caret。共有barのうちapp固有部分 |
| system slot | `system_bar.rs` | 対象 | Clockは固定幅。Wi-Fi／Batteryの`!`・`?`はicon扱いで現行font |
| 通常GUI／ミニのbar title | `system_bar.rs` | 対象 | Desktop名、`< Back`、Network／Battery title、Launcher案内 |
| Launcher／Power | `system_bar.rs` | 対象 | menu label。選択矩形とhit targetは変更しない |
| Network settings | `network_settings.rs` | 対象 | AP一覧、SSID、password、進捗、結果、footer |
| Battery details | `battery.rs` | 対象 | label、動的な数値と単位、footer |
| Startup | `startup_screen.rs` | 対象 | title、進捗、5秒後の案内。共有bar開始前 |
| Desktop content | `desktop.rs` | 直接対象外 | 背景を塗るだけで文字が無い。上のsystem barは対象 |
| Console | `console.rs` | 移行対象外・固定幅必須 | bar無し。既存8×16固定セルを必ず維持 |
| 専有GUI | 各診断／`paint` | 対象外 | bar無し。終了後の遷移先変更もしない |
| 比較用診断 | `font_test.rs` | 補助対象 | 方式とサイズを比較するためだけに拡張 |
| bar固定診断build | `system-bar-static` | 補助対象 | bar geometry回帰に使うが製品画面の完了条件にはしない |

以前の「`win`はマウス動作確認兼ジョーク画面なので対象外」という分類は、旧モックアップには
正しかったが現在のコードにはそのまま当てはまらない。現在の`win`は空のDesktop contentへ入る。
本計画は`desktop.rs`を変更せず、そこで共有表示されるsystem barだけを対象にする。

対象は「英字を含む通常利用の文字surfaceを同じ測定／描画契約へ載せる」単位で固定する。
共有barだけ旧fontのまま残すと、Browserからミニへ移った瞬間に同じ48 px帯の字体と幅が変わるため、
Network settingsとBattery detailsを含めて1つの移行単位とする。Stage 5の完了後に専有GUIへ
広げる場合は、この表と[`APPS.md`](../../APPS.md)を先に更新する。

### 文字範囲

初期の新fontはASCIIだけでなく、英語ページで通常現れるaccent付きLatin文字と約物を同じfaceで
表示する。これを本計画では**English Latin set**と呼び、次を必須収録範囲とする。

- U+0020〜U+007E: Basic Latinの表示文字95文字
- U+00A0〜U+00AC、U+00AE〜U+00FF: NBSP、Latin-1の記号、accent付きLatin文字。
  U+00AD SOFT HYPHENは表示glyphではなくlayout機能なので初期対象外
- U+2010〜U+2015: hyphen／non-breaking hyphen／en dash／em dash等
- U+2018〜U+201F: curly quote各種
- U+2022、U+2026: bullet、ellipsis
- U+2032、U+2033: prime、double prime
- U+20AC、U+2122: euro、trademark

合計210 code pointをsans-serifとUI monospaceの各必須strikeへ収録する。

改行、tab、制御文字はglyphではなくlayoutが扱う。結合記号、上記以外の矢印／通貨記号、
全角ASCII、かな、漢字は初期blobへ入れず、現在の`tab5-font`へfallbackする。U+00A0はglyphの
inkを持たずspaceと同じadvanceにするが、Browser layout上の改行可能性は通常spaceと区別する。

英字と日本語が混ざった**文字列全体**ではなく、表示clusterごとにfaceを選ぶ。

- English Latin setの1文字で、その直後にcombining markが無ければ指定された新しいLatin face
- baseの後ろに現在の`font::is_combining`が認識する文字が続けば、baseを含むcluster全体を
  現在のfontで描く
- それ以外は現在のfont
- 新Latin blobでEnglish Latin setのglyphが欠落していた場合は生成／blob検査失敗とし、黙って
  glyph単位fallbackしない

`e`を新fontで描いた後に、UnifontのU+0301だけを比例幅glyphへ重ねると、markの基準位置が
8 px cellを前提としているためずれる。cluster単位fallbackは、この例外のために完全な
OpenType shapingやUnicode grapheme libraryを持ち込まず、現状と同じ表示を保つ境界である。

### font roleとface

用途による選択をサイズや色から推測せず、styleが次のroleを明示する。

| role | 要件 | 初期用途 |
| --- | --- | --- |
| Browser proportional sans-serif | 必須、比例幅 | 通常本文、段落、link、見出し |
| UI monospace | 必須、English Latin setで同一advance | `pre`、`code`／`kbd`／`samp`／`tt`／`var`、Clock |
| GUI default | Stage 1／5で決定 | bar、Launcher、Network、Battery、Startup、Browser chrome |
| Legacy Japanese | 必須互換 | English Latin set外とcombining clusterのfallback |
| Console monospace | 必須互換、今回変更しない | Consoleの既存8×16 cell font |

Browser proportionalはsans-serifを必須かつ既定とする。CSS `font-family`、`<font face>`、
ページ指定Web Font、利用者が実行時に切り替える設定画面を持たない初期版では、serifを収録しても
ページ側から選べず、具体的なconsumerが無い。そのためserifは初期版へ収録せず、CSS対応または
具体的な画面roleが追加された時点の追補計画へ延期する。

GUI defaultは現時点でproportional sans-serifとmonospaceのどちらにも固定しない。hostの画面mockと
実機で、小サイズの視認性、日本語fallbackとのbaseline、狭いslot、操作部品らしさ、blob増分を
比較してStage 5開始前に一つのpolicyとして決める。画面ごとの場当たり的な既定face選択は行わない。

### 対象外

- Console、shell入力、console用font IDと固定セルの移行（既存固定幅対応の維持は必須）
- `desktop.rs`の背景面、paint、touchtest、coordtest、axistest、display診断
- 日本語fontの変更、日本語IME、縦書き、ルビ
- CSS Web Font、ページ指定font、font download
- runtimeでのTTF／OTF／WOFF／BDF／PCF解析
- OpenType shaping、ligature、可変font axis、bidi、Arabic／Indicの文脈字形
- subpixel RGB rendering、color emoji
- SD／USB／networkからのfont読み込み、利用者によるruntimeのfont切替
- 任意の実数倍率を初期版の完了条件にすること

## 方式の候補

### 比較表

| 方式 | 幅 | サイズ | 輪郭品質 | firmware複雑度 | 初期判断 |
| --- | --- | --- | --- | --- | --- |
| 1bpp bitmapをサイズ別に保持 | 比例／固定とも可能 | 離散 | 小サイズは明瞭、斜線は jaggy | 最小 | 性能比較baselineのみ。最終採用不可 |
| **4-bit alpha bitmapをサイズ別に保持** | **比例／固定とも可能** | **離散** | **対象サイズでは良い** | **小** | **必須品質を満たす第一候補** |
| 8-bit SDF＋CPU補間 | 比例／固定とも可能 | ある範囲で連続 | 拡大に強い、小サイズは要比較 | 中〜大 | Stage 6まで保留 |
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

1. BDF／PCF等で入手できる比例幅／固定幅bitmap strikeを正規化する
2. OFL等の再配布可能なsans-serif、monospace outline fontをhost側だけで
   16／24／32 pxへラスタライズする

案1は生成toolが単純だが、必要な3サイズ、比例幅、英字の品質、licenseを同時に満たす元fontが
限られる。案2は元ファイルがTTF／OTFでも、複雑さをfont変更時のhost toolへ封じ込められ、
対象サイズごとのhintingとalpha coverageを得られる。従って**案2も「runtimeでTTF／OTFを採用」
したものとは扱わない**。

生成済みblobと報告をcommitし、通常buildは元fontを解析しない。元font、license、取得元、版、
SHA-256もrepositoryへ保持し、生成toolの版とoptionを固定する。生成toolを更新しなければ再生成結果が
byte単位で一致しなければならない。

## 推奨する生成物

### strike

比例幅sans-serifとUI monospaceは必須faceである。初期比較では各候補を16、24、32 pxで生成するが、
製品blobへ残すstrikeは実際のroleに合わせて減らしてよい。

| face | 16 px | 24 px | 32 px |
| --- | --- | --- | --- |
| Browser proportional sans-serif | 必須: 本文、link | 比較候補 | 必須: `h1`／`h2` |
| UI monospace | 必須: `pre`、コード系、Clock | 比較候補 | 必須: 見出し内のコード系 |

現在の製品画面は16 pxと32 pxだけを使い、24 pxを要求する配置はまだ無い。24 pxは「中間サイズを
生成できること」と、Launcher／固定ASCII labelで32 pxより適するかを比較する候補として作る。
日本語16 pxを1.5倍にしないため、SSID、bar title、任意のerror文など日本語が混ざり得るsurfaceへ
24 pxを場当たり的に使わない。Stage 3〜5で採用箇所が無ければ、24 pxを製品blobから外してもよい。
その場合も比較結果と削減byte数を記録し、任意倍率が必要かはStage 6で別に判断する。

### 現行surfaceとのstyle対応

| surface | 現行 | 初期移行 | 注意点 |
| --- | ---: | ---: | --- |
| Browser通常本文／link | 16 px | proportional sans-serif＋legacy 16 px | Browserの既定。layout、hitを同時に移す |
| Browser `h1`／`h2` | 32 px | proportional sans-serif＋legacy 32 px | 同じbaselineとline box |
| Browser `pre`／コード系 | 親styleのsize | UI monospace＋legacy | 空白のcolumnを維持。見出し内は32 px |
| Browser status／address | 16 px | GUI default＋legacy 16 px | 本文の既定faceと独立に決定可能 |
| system bar title | 16 px | GUI default＋legacy 16 px | GUI policyを共有 |
| system bar Clock | 16 px | UI monospace | `--:--`を含め固定advance、slot全体を再描画 |
| Launcher／Power row | 32 px | GUI default、24 pxも比較 | `デスクトップ`だけ小さく見えないこと |
| Network一覧／footer | 16 px | GUI default＋legacy 16 px | SSIDを列境界でclipする |
| Network dialog title | 32 px | GUI default＋legacy 32 px | 動的detailは16 px |
| Battery label／footer | 16 px | GUI default | 数値fieldとの左端を維持 |
| Battery value／中央% | 32 px | GUI default＋tabular数字、またはUI monospace | 桁変化でink位置を揺らさない |
| Startup状態／案内 | 16 px | GUI default＋legacy 16 px | 部分再描画範囲を維持 |
| Startup title | 32 px | GUI default | 中央揃えを再測定 |

### glyph record

形式名とfield幅はStage 1の実測で確定するが、意味は次で固定する。

```text
header
  magic, format_version, face_count, strike_count, glyph_count
  section offsets, total length, CRC

strike
  face_id, pixel_size, ascent, descent, line_gap
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
1/64 pixel等のfractional advanceは持たない。UI monospaceは同じstrikeのEnglish Latin setで
同じadvanceを持たせ、inkだけをcell内でglyphごとに配置する。日本語fallbackと混在する16／32 pxでは
advanceをそれぞれ8／16 pxとし、従来の全角glyphが固定幅2 columnになる契約を維持する。24 pxは
日本語混在へ使わない比較strikeなので、このcolumn契約の対象外とする。

A4はalpha 0〜15で、RGB565とのblend時に15を分母として整数演算する。0／15だけを残す二値化は
許さず、1〜14をanti-aliasされた輪郭として保持する。gamma補正tableやsubpixel renderingは
初期版へ入れない。A4とA8の見た目に明確な差があり、A8でも容量と時間のbudgetを満たす場合だけ
Stage 1の判断記録で変更する。

bitmapはPNGのまま埋めず、通常build時のdecoderも持たない。汎用BMFont binary parserも持たず、
本projectが使うfieldだけを持つversion付きblobにする。

### coverageとkerning

初期coverageはEnglish Latin setを完全収録し、1文字でも欠けたら生成を失敗させる。使用頻度を
理由にglyphを黙って落とさない。

比例幅とkerningは別の機能である。初期版はglyph固有の`x_advance`を必須とし、kerning pairは
Stage 1で次の両方を測ってから選ぶ。

- pair無しの`AVATAR To Wi-Fi`が実機で不自然か
- pair tableを足した場合のblob byte数、lookup時間、browserの測定APIへの影響

kerningを入れる場合、rendererだけに実装してはならない。layoutの折返し、piece幅、下線、選択背景、
hit判定、caret位置がすべて同じ「直前glyph＋現在glyph」の測定器を使う。複雑さに見合わなければ
初期版はpair無しと明記して完了してよい。UI monospaceにはkerning adjustmentを適用せず、どのpairも
同じcolumn数を進む。

## APIと責務

### 現行font APIを維持する

`font::advance`、`font::text_width`、`font::console`と現在の1bpp glyph APIは意味を変えない。
コンソールと対象外画面が、呼び出し側を変更せず従来の表示を続けられるようにする。

新しいUI向けAPIは別の名前空間に置く。配置は実装時に調整してよいが、概念は次とする。

```rust
pub struct UiTextStyle {
    pub latin_face: LatinFace,      // ProportionalSans, Monospace, GuiDefault
    pub latin_size: LatinSize,      // Px16, Px24, Px32
    pub legacy_scale: u8,           // 1 or 2
    pub weight: Weight,
    pub numerals: NumeralMode,      // Proportional or Tabular
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

Clockは`LatinFace::Monospace`を明示し、比例幅faceのtabular numeralだけで固定幅要件を満たしたことに
しない。Battery details、Network一覧のRSSI／channel／AP数は値が定期的に変わる。比例数字のまま
同じ左端へ描くと桁のink位置が揺れ、狭いslotでは消去範囲も分かりにくい。`Tabular`は0〜9の
advanceをstrike内の最大digit advanceへ揃え、inkをその枠内で中央に置く。別glyphやOpenType
featureをruntimeで選ばず、生成metricsのoverrideとして持つ。URL、本文、通常labelは
`Proportional`のままとする。

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
[`SYSTEM_BAR.md`](../../SYSTEM_BAR.md)の順を変えない。alpha blendがcursor込みの画素をbackgroundとして
読まないことも実機で確認する。通常GUI hostが画面遷移とpointerを一括所有するため、Browserと
ミニアプリがそれぞれ独自にcursorを退避してはならない。

## 容量と性能のbudget

Stage 0で変更前の値を記録し、次を初期の判断線とする。満たせない場合は理由と実測をこの文書へ
記録し、A4 rendererの最適化、未使用strike削減、kerning見送り、A8／SDFの再評価の順で
範囲を見直す。1bppへの縮退は選択肢にしない。

| 項目 | 初期budget |
| --- | ---: |
| 必須face／strike（比例幅sans 16／32＋UI monospace 16／32） | 160 KiB以下 |
| 比較用24 pxを含む最終font blob | 192 KiB以下 |
| release imageの増加（blob、code、metadata合計） | 256 KiB以下 |
| 内部`.data + .bss`増加 | 4 KiB以下 |
| runtime PSRAM／heap allocation | 0 byte |
| browserの文字で埋まったviewport再描画 | 変更前の125%以下、かつ25 ms以下を目標 |
| 通常操作中のDPI FIFO underrun | 0 |
| browser再描画中のC6 link再同期／切断 | 0 |
| system slot／bar titleの描画・flush | 変更前値を悪化させないことを目標 |

25 msは絶対合否だけでなく、現在[`APPS.md`](../../APPS.md)に記録された約17.5 msとの比較値も残す。
PPAによるviewport背景塗りだけで約12 msかかるため、fontだけの費用は全面再描画時間と、背景塗りを
除いたglyph phaseの両方を測る。実機差で25 msをわずかに超えても、125%、service間隔、操作感、
underrunを満たすなら、数値と判断を記録して継続できる。

## 検証方針: 正しさはhostで先に閉じる

renderer、font blob、metrics、layoutのようにデバイス固有hardwareを必要としない検査は、速度、
再現性、失敗時の観測性に優れるhost testで完了させる。実機を正しさの最初の検出場所にしない。
firmware用renderer coreはRGB565 buffer、stride、clip矩形を引数に取るtarget非依存の処理として分離し、
同じ実装をhost testから呼ぶ。host専用のrendererを別実装して、その結果だけを信用してはならない。
`Framebuffer`側には回転後の連続span取得、PSRAM access、cache同期、flushだけを残す。

hostでは少なくとも次を自動検査する。

- A4のalpha 0〜15すべてについて、十分なbit幅を使う単純な参照式とRGB565結果を比較する
- opaque／transparent、clip、負のbearing、画面四辺、空bitmap、不正blobをguard付きRAM bufferで検査し、
  対象外pixelへのwriteを検出する
- 実際の生成済みglyphを黒、白、theme色、既存画像の各背景へ描き、RGB565のgolden imageと、確認用に
  RGB888へ展開したPNG contact sheetを生成・比較する
- 測定、折返し、caret、hit範囲、選択背景と描画結果を同じiteratorに対して検査する
- fuzz／property testを利用できる場合は、任意の短い文字列、clip矩形、描画位置でもpanic、範囲外write、
  測定幅と描画advanceの不一致が無いことを検査する

実機に残すのは、PSRAMとcacheの実効速度、回転framebufferへの接続、PPA／scanoutとの競合、flush範囲、
DPI FIFO underrun、C6 service間隔、LCD上の最終的な視認性、pointerとの描画順、操作感である。
実機目視で不具合を見つけた場合も、可能な限り同じ入力と期待pixelをhost testへ追加して再現してから
修正する。host test合格は実機性能やLCD表示の確認を代替せず、実機試験はhostで確認できる論理を
重ねて試す場にしない。

## 実装Stage

### Stage 0: 契約と変更前の値を固定する

- 対象画面とEnglish Latin setを本計画の表と照合する
- `system_bar.rs`、`browser.rs`、`network_settings.rs`、`battery.rs`、`startup_screen.rs`の
  全`draw_text`／`draw_glyph`／`text_width`をsurface別に棚卸しする
- 上のrole／style対応表をfixtureへ固定し、Browser通常本文、コード系、Clock、GUIのface選択を
  独立したpolicyとして扱う
- 混在見本を固定する: `Wi-Fi設定`, `Tab5 Browser 日本語`, `AVATAR To Wi-Fi`,
  `e\u{301}`, `が`, `デスクトップ`, SSID、URL、時計、battery値、未収録emoji
- 現行fontの混在用baseline候補を16／32 pxで表示し、観測記録を残す
- releaseのDROM／IROM、image、`.data`、`.bss`、残りstackを記録する
- browserの背景塗り、glyph、flush、viewport全体の各時間と、最長C6 service間隔を測る
- `SYSTEM BAR: max service/handler ms=`と各slotのdraw／flush時間、Network／Battery／Launcher／
  Startupの初回描画と部分再描画、underrunを記録する
- 現在のsystem bar統合経路は実機未確認なので、font変更前の実機baselineを人間へ依頼する。
  先に得られない場合はhost作業を進めてもよいが、Stage 4／5の実機差分を確認済みにしない

**完了条件:** 文字、画面、style、baseline候補、容量、時間の変更前値が記録され、Stage 1で
「見た目が良い」以外にも比較できる状態になる。画面のfontはまだ変更しない。

### Stage 1: host上でfaceと生成候補を比較する

- sans-serifとmonospaceを少なくとも各2候補選び、同じ条件で比較する
- 各候補のlicense、版、取得元、元データSHA-256を固定する
- 同じEnglish Latin set、16／24／32 px、A4条件でface別の専用blobとPNG contact sheetを生成する
- 1bppは性能／容量baselineとしてだけ生成し、A4、必要ならA8とのbyte数を報告する
- A4の代表glyphが0／15だけでなく中間coverageを持つことを生成時に検査する
- advance、bearing、ascent、descent、space、`j`のoverhang、`g`のdescenderを検査する
- monospace候補はEnglish Latin setすべてのadvanceが同一であることを生成時に検査する
- Browser本文のsans-serif contact sheet、コード／Clockのmonospace、各GUI画面mockをhostで
  比較し、Browser proportional sans-serifとGUI defaultを別々に評価する
- ASCIIだけの場合とEnglish Latin setを収録した場合のblob／release image差を報告する
- 生成を2回行い、blobと報告のhashが一致することをtestする
- 通常buildからgeneratorと元fontを参照しない構成にする

**完了条件:** sans-serifとmonospaceのanti-aliased英字見本、blob容量、license、再現性が揃う。
Browser proportional sans-serifとUI monospaceを1つずつ選ぶ。GUI defaultは候補を絞るが、
Stage 5の画面比較前には確定しなくてよい。1bpp見本は採用候補に数えない。

### Stage 2: 共通metricsとblob reader

- magic、version、section境界、件数、CRCをhost testまたはcompile時assertで検査する
- English Latin setの完全収録、code point順、重複無し、metrics範囲を生成時に検査する
- face IDを含む`UiTextStyle`、cluster選択、`ResolvedGlyph`、line metrics、stateful測定iteratorを
  追加する
- Browser proportional sans-serifとUI monospaceの全必須strikeを検査する
- combining cluster全体がlegacyへfallbackすることをtestする
- 日本語、全角記号、emojiが現行fontへfallbackし、English Latin setの欠落はblob検査で
  拒否されることをtestする
- rendererを使わないhost testでmixed textの幅、cluster境界、line boxを固定する
- 現行の`font::advance`／`text_width`／`console`のtestを無変更で通す

**完了条件:** browser layoutとfirmware rendererが同じiteratorを使える。コンソールのAPI、
データ、表示結果は変わらない。

### Stage 3: rendererのhost検証と実機性能比較

- column-major A4 glyphのopaque／transparent painterをtarget非依存のcoreとして追加し、firmwareと
  host testで同じ処理を使う
- alpha 0／15のfast path、clip、negative bearing、descender、scale外サイズをhostで検査する
- alpha 0〜15をforeground／backgroundのRGB565各channelへblendし、単純な参照実装と一致して
  二値化しないことをhostで検査する
- guard付きRAM buffer、golden image、PNG contact sheetにより、背景既知と既存pixelを読む経路、
  四辺clip、部分再描画、太字の二度打ちをhostで検査する
- `fonttest`に現行font、sans-serif／monospaceのA4候補、比較用1bppを同じ文と色で並べる
- 黒／白／色背景、選択背景、透明背景、細い線、太字、`AVATAR To Wi-Fi`、混在日本語を表示する
- 実機ではglyph phaseと全面描画の時間、cache flush範囲、underrunを候補別に記録する
- hostのPNGと実機LCDを比較し、LCD上でalpha edgeが濁る、細線が消える、背景の古いpixelが残る
  問題だけを目視確認する。不一致は再現可能ならhost回帰testへ移す

**判断gate:** A4が容量と性能budgetを満たし、全対象theme色で中間coverageが視認できればA4を
採用する。A4の階調が不足する場合はA8、性能が不足する場合はblend／走査の最適化、未使用strikeの
削減を試す。それでも必須品質とbudgetを両立できなければStageを完了せず、SDF prototypeまたは
計画中止を判断する。1bpp strikeへ戻して完了扱いにはしない。

### Stage 4: browserへ導入する

- `browser::layout`の幅とline metricsを新しい測定iteratorへ接続する
- 通常本文、段落、link、見出しはBrowser proportional sans-serifを既定にする
- `pre`と現在`STYLE_CODE`が表す`code`／`kbd`／`samp`／`tt`／`var`をUI monospaceへ割り当てる。
  色、bold、link、選択状態とface選択を別のstyle軸にし、組み合わせで片方を失わない
- 見出し内のコード系要素は見出しsizeのmonospaceを使い、通常本文へsizeを落とさない
- piece幅、折返し、禁則、下線、選択背景、touch／mouse hitを同じcluster幅でtestする
- URL欄の表示、左右移動、Home／End、Delete、Backspace、caret x、横clipを比例幅へ対応する
- `APP`領域のbutton、address、caretはBrowser側の対象とし、system slotの描画を重複して持たない
- Browser chromeのGUI defaultは本文のBrowser proportional sans-serifから独立させる
- `ADDRESS_CELLS`を実際の表示可能文字数として扱わない。hit geometryは`system-ui`のpixel矩形に
  残し、表示側は`ADDRESS_TEXT_PIXELS`相当のbudgetと同じ測定iteratorでclipする。
  `MAX_URL_BYTES`は編集buffer上限として別に維持する
- toolbarの矢印／再読込／全消去等、English Latin set外の記号は現行fontで描く
- boldはまず現行の1 px二度打ちを使い、別bold strikeは容量と視認性に根拠がある場合だけ足す
- italicの現在の色分けは維持し、synthetic slantやitalic strikeを初期版へ足さない
- 全面再描画中のC6 serviceを経過時間でも分割し、最長停止時間を記録する

**完了条件:** host layout／描画testで折返し、link下線、選択背景、hit範囲、caretが期待pixelと一致し、
実機ではframebuffer接続、性能、LCD視認性に固有の差が無い。
通常本文は比例幅、`pre`とコード系要素は固定幅、日本語は従来glyphのまま、Consoleは差分無しで、
viewport性能budgetとunderrun 0を満たす。

### Stage 5: 共有バー、ミニアプリ、起動画面へ導入する

#### 5A: system barとLauncher

- Stage 1の候補を各画面のhost mockと実機で比較し、GUI defaultをproportional sans-serifと
  monospaceのどちらにするか決定して理由を記録する
- Clock、通常GUI／ミニのtitle、Launcher／Power rowへUI text helperを導入する
- Wi-Fi／Battery slotの`!`・`?`は文字ではなく状態iconとして現行fontと位置を維持する
- ClockはUI monospaceにし、`--:--`と`00:00`〜`23:59`を80 px slot内で中央に置く
- app titleは`APP`領域からsystem slotへはみ出さないよう、測定後にclipする
- Launcher／Powerは32 pxを既定とし、24 px候補は全項目を同じsizeにできる場合だけ採用する
- `デスクトップ`はlegacy glyphのまま、英字項目だけ不自然に上下へずれないことを確認する

#### 5B: Network settingsとBattery details

- `network_settings.rs`と`battery.rs`の全textを同じUI text helperへ載せる
- SSID、接続結果、errorは日本語を含み得るため、16／32 pxのmixed fallbackだけを使う
- SSIDはbyte数や「最大半角32文字」をpixel幅の保証にせず、次の固定columnまででclipする。
  選択背景、接続済み太字、tick、signal、channel、security、AP数を消さない
- password maskと文字数、RSSI、channel、AP数、Batteryの値と百分率は更新前のfield全体を消して
  再描画し、数値にはtabular metricsを使う
- 中央揃え／右揃えを新しい測定APIへ変更し、`text_width * scale`の局所計算を残さない
- `< Back`の二度打ちとtitleの通常weightを維持し、barとcontentで別の幅計算を持たない

#### 5C: Startupと対象外確認

- Startupのtitle、状態、案内へUI text helperを導入し、iconのalpha maskには触れない
- 初回全画面描画と状態行だけの部分更新範囲を維持する
- `desktop.rs`には文字APIを追加せず、Desktop上で見えるsystem barだけを5Aで更新する
- console、paint、touchtest、coordtest、axistest、display診断へ変更が漏れていないことを`rg`で確認する
- `fonttest`と`system-bar-static`は比較／回帰に必要な範囲だけ更新する

**完了条件:** GUI defaultのfaceと選定理由が記録され、同じnormal GUI host内でBrowser、Desktop、
Launcher／Power、Network settings、Battery detailsを切り替えてもbarの字体、baseline、幅契約が
一貫する。Startupを含め、日本語と
iconは現行font／画像のままで、部分再描画、pointer、画面遷移、Console復帰に残像とunderrunが無い。

### Stage 6: SDFを続けるか確定する

Stage 3〜5の実機結果から次を問う。

1. 実際に16／24／32 px以外を利用者が必要としたか
2. 採用したface／strike集合の容量が問題になったか
3. 同じfontを別倍率へ拡大したときだけ解決できるUIがあるか
4. A4 strikeの小サイズ品質に未解決の問題があるか

すべて「いいえ」なら、SDFを不採用として理由と数値を記録し、このStageを完了する。いずれかが
「はい」の場合だけ、Basic Latin 1 strikeのSDF prototypeを`fonttest`限定で作り、次を比較する。

- nearest／bilinear相当の補間とedge smoothingのCPU時間
- A8 SDFのblob容量
- 16、20、24、28、32 pxの細線、corner、`iIl1Wm`
- RGB565 blend後の輪郭
- C6 service間隔とunderrun

SDFが採用したA4 face／strike集合より容量を減らし、任意サイズに実需要があり、browser viewportとGUIのperformance
budgetに加えてalpha blend／anti-aliasの必須品質を満たした場合だけ本採用の追補Stageを作る。
SDFのdistance値を単純thresholdして1bpp表示する方式は採らない。SDF採用のためにpixel shader相当の
汎用描画層やPSRAM glyph cacheが必要なら、本計画から分離する。

### Stage 7: release、実機回帰、文書同期

- `mise run test`とrelease build、ELF layout、ESP image検査を通す
- font blob、code、DROM／IROM、`.data`、`.bss`、stack、imageの前後差を記録する
- cold boot、Browser長文、URL編集、Launcher／Power、Network scan／password、Battery details、
  Desktop、Console復帰を確認する
- `bt` fixture、`ui`巡回、`dp 100`、利用可能なら`di 30`を実行する
- glyph描画と全面再描画、system slot draw／flush、normal GUI handlerの時間、最長C6 service間隔、
  underrun数を記録する
- [`FONT.md`](../../FONT.md)、[`GRAPHICS.md`](../../GRAPHICS.md)、[`BROWSER.md`](../../BROWSER.md)、
  [`APPS.md`](../../APPS.md)、[`SYSTEM_BAR.md`](../../SYSTEM_BAR.md)、[`GUI_THEME.md`](../../GUI_THEME.md)、
  [`FILE_LAYOUT.md`](../../FILE_LAYOUT.md)、[`BOOT.md`](../../BOOT.md)、[`../../../DESIGN.md`](../../../DESIGN.md)を
  実装結果へ同期する
- `README.md`は変更せず、古くなった記述があれば最終報告で箇所と不一致を示す

**完了条件:** host test、release検査、実機matrixが合格し、採用方式、font、strike、最終byte数、
時間、未採用候補の理由がこの文書に残る。

## host test matrix

| ケース | 検査内容 |
| --- | --- |
| English Latin coverage | 定義した全code pointが全必須strikeに1件ずつあり、U+00ADを含まない |
| metrics | advance > 0、bitmap境界、bearing、ascent／descentがformat範囲内 |
| alpha coverage | 代表glyphの輪郭に中間coverageがあり、1bppへ二値化されていない |
| RGB565 blend | alpha 0／最大値／中間値を黒・白・theme色へblendした期待値と一致 |
| transparent draw | 既存pixelを背景としてblendし、周囲とcoverage 0のpixelを変更しない |
| renderer reference | alpha 0〜15、代表的なRGB565 channel値で単純な高精度参照式と同じpixelになる |
| buffer bounds | 四辺clip、負のbearing、空bitmapでもguard領域と対象外pixelを変更しない |
| golden image | 実glyphのopaque／transparent、色背景、太字、混在文字列が固定RGB565画像と一致 |
| renderer fuzz | 任意の短い入力、位置、clipでpanic、範囲外write、advance不一致が無い |
| space | bitmap長0でも正のadvanceを持つ |
| proportional | `III` < `MMM`、測定値がglyph advance合計と一致 |
| face routing | 通常本文／link／見出しは比例幅、`pre`／コード系／ClockはUI monospaceになる |
| monospace | English Latin setのadvanceが同一で、space、`i`、`W`も同じcolumnを進む。16／32 pxは8／16 px advance |
| tabular numerals | `0`〜`9`が同じadvanceで、Battery等の比例幅fieldが値により揺れない |
| mixed | `Tab5日本語Wi-Fi`でLatin／legacyの選択と幅が期待どおり |
| combining | `e\u{301}`はcluster全体がlegacyになり、途中でpieceを切らない |
| missing | English Latin set欠落は生成失敗、範囲外はlegacy fallback |
| kerning | 採用時だけ、測定／描画／折返しが同じpair adjustmentを使う |
| line metrics | `A`と`g`、英字と日本語が混じってもstyle内の行高が安定 |
| browser wrap | piece幅、禁則、link hit、下線、selectionがcluster境界と一致 |
| URL caret | 挿入、削除、左右移動、Home／Endでcaret xが測定結果と一致 |
| system bar bounds | 全titleと`00:00`〜`23:59`がAPP／CLOCK領域から出ない |
| Network columns | 最長ASCII／日本語SSIDをclipして隣のsignal列へ入れない |
| numeric fields | RSSI、channel、AP数、Battery値の桁変化で古いinkが残らない |
| blob破損 | magic、version、offset、length、CRC異常を拒否 |
| 再現生成 | 同じ入力とtoolでblob／reportのSHA-256が一致 |
| console回帰 | 現行console glyph ID、cell count、幅testが無変更で通る |

## 実機受入matrix

| ケース | 期待結果 |
| --- | --- |
| `fonttest` | sans-serif候補とmonospaceの16／24／32 pxが比較でき、anti-aliasされ、黒・白・theme色背景で残像が無い |
| 英日混在 | baselineと行間が不自然に跳ねず、日本語glyphの形が変更前と同じ |
| browser本文 | 通常本文は比例幅で折り返し、選択、下線、link hitがpixel単位で一致 |
| browser固定幅 | `pre`とコード系要素が固定columnを保ち、通常本文へ戻ると比例幅へ復帰する |
| browser見出し | 32 px英字と2倍日本語が同じline boxへ収まる |
| URL編集 | 可変幅でもcaret、削除位置、右端clip、全消去がずれない |
| browser長文 | 再描画時間budget、C6 service、scroll、戻る／進むが正常 |
| 起動画面 | cold bootの表示順を変えず、英字titleと日本語案内が欠けない |
| system bar | Clockが固定幅で、titleは決定したGUI defaultを使い、状態iconとともにslot境界内で残像が無い |
| Launcher／Power | 英字項目と`デスクトップ`のbaselineが揃い、選択矩形とhit範囲が従来どおり |
| Network settings | SSID混在、列clip、password、button、error、pointer再描画が正常 |
| Battery details | 数値更新で位置が揺れず、単位、中央%、戻る操作が正常 |
| Desktop | contentは従来の背景だけで、対象fontは共有barにだけ現れる |
| console | 156×44固定セル、入力、cursor、scroll、UART出力が変更前と同じ |
| 画面巡回 | 対象外の診断画面を含め、復帰後の描画崩れとunderrunが0 |
| cold boot反復 | DROM上のfont blobを毎回正しく参照し、CRC／XIP異常が無い |

## 中止・見直し条件

- fontのlicense、元データ、生成tool、hashをrepository内で追跡できない
- English Latin setを完全収録できない
- Browser proportional sans-serifまたはUI monospaceの必須strikeを用意できない
- A4または同等以上のcoverageを持つ方式で、必要strikeが容量budgetへ収まらない
- alpha blendを外す、または中間coverageを1bppへthresholdしなければ性能budgetを満たせない
- 内部RAMまたはPSRAMにruntime glyph cacheを置かないと通常操作の性能を満たせない
- browserの測定と描画が別APIになり、折返し、hit、caretのどれかがずれる
- system barのtitle、Clock、Launcherとミニアプリが別々の幅計算を持つ
- combining cluster fallbackにより既存日本語／結合文字の表示が後退する
- C6 serviceが遅れてlink再同期が起きる、または表示underrunが再発する
- consoleのcell、API、表示、release memoryが変化する

中止条件に当たったStageを完了扱いにしない。新fontを対象画面から外せば現在の
`draw_text`／`font::advance`へ戻れるよう、既存APIと生成物を移行完了まで削除しない。

## 実装時に残す判断記録

各Stageの完了時に少なくとも次を追記する。

- sans-serif／monospace候補のfont名、版、license、取得元、元データSHA-256
- generator名／版／option、再生成結果、生成物SHA-256
- face／strikeごとのglyph数、A4／A8 byte数、比較用1bpp byte数、metadataとkerning byte数
- Browser proportional sans-serifとGUI defaultの選定理由、serif延期の判断
- English Latin set coverageとfallback試験結果
- 中間coverageの分布、RGB565 blend試験、対象theme色でのanti-alias実機結果
- baseline、ascent、descent、line gapを選んだ実機上の理由
- kerning採否と`AVATAR To Wi-Fi`の比較結果
- DROM／IROM、release image、`.data`、`.bss`、stackの前後差
- glyph phase、viewport、GUI部分再描画の時間と最長C6 service間隔
- system slot、bar title、Launcher、Network、Battery、Startupの描画／flush時間
- host test数、実機matrix、underrun、未確認項目
- SDFを採用または不採用にした実需要と実測値
- README.mdに古い記述が生じた場合の箇所（README自体は変更しない）

## 対象画面の再精査記録

### 2026-09-09: system bar統合後

- 通常GUIをDesktop／Browser、ミニアプリをNetwork settings／Battery detailsとして再分類した
- 旧`wifi_menu.rs`相当の画面名ではなく、現行の`network_settings.rs`を対象にした
- Launcher、Power、Clock、bar titleは`system_bar.rs`が描くため、Browserとミニアプリを
  移行するなら共有barも同じStageで移すと判断した
- Battery detailsは旧Consoleコマンド画面ではなく、通常GUI host内のミニアプリになったため
  対象へ追加した

## 2026-09-09 実装記録

- DejaVu Sans 2.37をBrowser proportionalとGUI default、DejaVu Sans Mono 2.37をUI monospaceに
  採用した。いずれもDejaVu licenseで、元TTF、license、由来、SHA-256を`tools/ui-font/vendor/`
  へ固定した。GUI defaultは本文との一貫性とbar内での表示量から比例幅Sansとした
- Pillow 10.2.0で13／20／27 px emを16／24／32 px line boxへラスタライズした。English Latin
  setは各strike 210 glyph、Sans Monoのadvanceは8／12／16 pxで全glyph同一である
- `T5A4` v1 blobはcolumn-major A4、6 strike、1,260 glyph、metadata 20,288 byte、bitmap
  119,330 byte、合計139,618 byte、CRC32 `307f3760`、SHA-256
  `f38451c7d9d01d639e86343971eb3d69e81559ffd78851c14a6f987fca418c2a`となった
- `tab5-ui-font`にbounds付きreader、共通metrics、English Latin判定、combining cluster fallback、
  RGB565 blend、target非依存painterを実装した。host testは全必須glyph、CRC、mono／比例幅、
  中間coverage、transparent／opaque、負座標clip、guardを検査する
- Browser本文とlink／見出しはSans、code系はSans Monoになり、piece、wrap、underline、selection、
  hitは同じmetricsを使う。URL表示とcaretの追従はpixel幅へ変更した
- `BlockKind::Preformatted`はlayout時に全pieceのfont roleをMonoにする。色・強調styleとは分離し、
  `<pre>`単独でも測定・描画がMonoになる。組み込みsample pageで比例幅本文、inline code、
  桁ルーラー付き`pre`を比較できる
- system bar title、Launcher、Network settings、Battery details、StartupをGUI helperへ移し、
  ClockだけSans Monoにした。Consoleと専有診断は従来APIのままである
- `fonttest`は従来sheetの次にA4 16／24／32 px、色背景、英日混在、太字を表示し、glyph phase
  時間をUARTへ出す。実機のLCD品質、時間、C6 service間隔、underrunは未確認である
- `fonttest`へ3枚目の左右比較sheetを追加した。左は通常A4、右は同じA4 glyphをcoverage 8で
  二値化した診断専用1bit baselineで、metricsとoutline由来の差を混ぜずにSans／Sans Monoの
  16／24／32 pxを黒・青・白背景で実機比較できる。製品surfaceのA4必須条件は変更していない
- 圧縮方式の前段測定として、従来fontとA4 fontのDROM blobをbuild時に位置依存XORでscrambleし、
  起動時に合計497,525 byteをPSRAMへ復号コピーする経路へ変更した。lookupが参照するのは検証済み
  PSRAM addressだけで、起動時にcopy＋CRC cycleとaddress、`fonttest`で両rendererの描画時間を
  UARTへ出す。起動時間への影響は実機未計測である
- A/B用`font-drom-direct` featureを追加した。既定buildは上記PSRAM経路、feature buildは
  commit済み平文blobを以前どおりDROMから直接参照する。起動ログと画面にsourceを表示し、
  `fonttest`の時間ラベルを両buildで共通化した。ソース差し戻しなしで同一操作を比較できる
- DROMはblob追加後も64 KiB以上のpaddingを残すため`0x40000020..0x400bfff8`へ広げ、IROMを
  `0x400c0000`へ移した。XIP窓終端`0x40400000`とPSRAM allocationは変更していない
- release検査値はIRAM 10,336 byte、DRAM rodata 1,364 byte、DROM 786,136 byte、IROM
  1,271,150 byte、stack 188,928 byte。XIP 2本と
  page offset整合、RAM load 2本をhostで確認した
- SDFの要否はA4の実機品質と性能を見て決めるため、Stage 6は未完了のままとした
- 旧`win`モックアップは廃止済みで、現在の`win`は文字を描かない`desktop.rs`へ入る。
  Desktop contentは直接対象外、そこに重なるsystem barは対象とした
- `fonttest`と`system-bar-static`は製品surfaceではなく比較／回帰用の補助対象とした
- Consoleと専有GUIはbarを持たず、今回のfont移行対象外のままとした
- system bar統合経路自体の実機動作は現状文書で未確認なので、font導入前baselineを人間に
  依頼し、結果を受け取るまで実機受入済みとは記録しない

### 2026-09-09: font PSRAM経路の実機A/B

- 利用者が既定のdecoded PSRAM buildと`font-drom-direct` buildを同じ長文スクロールで比較した
- PSRAM buildでも表示上の問題はなかった
- `BROWSER: slowest viewport repaint so far`の数値はDROM直接時より少し増えたが、ログが出る
  タイミングはほぼ同じだった。具体値は記録を受け取っていないため推測で補わない
- 利用者判断でこの差を許容し、PSRAM font経路を既定のまま維持する。A/B featureは回帰比較用に残す
- この結果はPSRAM読出し経路の受入であり、圧縮方式、起動時展開時間、SDF要否の受入ではない

### 2026-09-09: ROM font圧縮方式にLZ4 raw blockを採用

- 利用者判断で、圧縮率より展開コードの単純さと速度を優先し、LZ4 raw block方式を採用した
- 従来fontとA4 fontを独立したblockにし、LZ4 frame、外部dictionary、streaming、連結frameは
  実装しない。展開済み出力をmatch参照に使い、追加の大きな作業bufferを持たない
- hostのliblz4による候補測定では2 block合計319,894 byteだった。実装した決定的generatorでは
  従来font 227,004 byte、A4 font 76,826 byte、合計303,830 byteとなり、無圧縮497,525 byteから
  193,695 byte（38.9%）減った
- 独自wrapperにmagic、format version、圧縮長、展開長、CRC32を持たせる。decoderは入力切れ、
  literal／matchの出力超過、offset 0、展開済み範囲を越えるoffset、長さ計算overflow、余分な
  trailing byteを拒否し、展開後に既存font header、layout、CRCも検査する
- 圧縮済みbyte列は平文fontとして解釈できないため、圧縮導入後はXOR scrambleを重ねない
- `font-drom-direct`は平文DROMとの性能A/B用に維持する
- `tab5-font-codec`へallocator不要のdecoderとbuild-script専用encoderを実装した。decoderは上記の
  malformed inputに加えwrapper長、圧縮block CRC、展開後CRCをhost testで検査する。実生成物は
  system liblz4の`LZ4_decompress_safe`でも元blobとのbyte単位一致を確認した
- 既定release ELFは2つのT5L4 containerを各1個含み、平文blobを含まないことを検査した。
  DROM/IROM境界は`0x400c0000`から`0x40090000`へ戻し、192 KiBのpaddingを削減した。release検査値は
  IRAM 10,592 byte、DRAM rodata 1,364 byte、DROM 589,528 byte、IROM 1,279,072 byte、stack
  188,416 byteで、XIP 2本、page offset整合、RAM load配置の検査に合格した
- generator、decoder、host malformed-input test、release配置検査は完了した。起動時間の数値は
  未記録だが、起動、表示、Browser長文scrollの実機動作は確認済みである

### 2026-09-09: LZ4 font buildの実機確認

- 利用者が既定LZ4 buildを書き込み、起動、PSRAM展開後のフォント表示、Browser長文scrollが
  正常に動作することを確認した
- LZ4／validation失敗による起動停止や表示異常は報告されなかった
- `FONT PSRAM: LZ4+CRC cycles`の具体値は受け取っていないため、推測で補わない
- これによりROM font圧縮導入の実機受入を完了とする。SDF要否など比例幅font計画全体の
  未完了項目はこの確認だけでは完了扱いにしない

### 2026-09-09: alpha blendとanti-aliasを必須化

- 利用者判断により、alpha blendとanti-aliasを候補比較上の加点ではなく受入必須条件にした
- 1bpp strikeは容量／速度の比較baselineにだけ残し、製品方式にも性能不足時のfallbackにも使わない
- A4で不足する場合はA8、renderer最適化、strike削減、SDFを比較し、二値化でStageを完了しない
- 必須化の対象は新しい英字faceすべてであり、従来の日本語／Consoleの1bpp glyphと状態iconは
  変更しない

### 2026-09-09: rendererの正しさをhost優先で検証する

- renderer coreをtarget非依存に分離し、firmwareとhost testで同じ実装を使う
- pixel結果、clip、buffer境界、golden image、layout整合はhostで完了させる
- 実機試験はPSRAM／cache／scanout、性能、LCD視認性などdevice固有項目へ絞る
- 実機で見つけた論理不具合は可能な限りhost回帰testへ移す

### 2026-09-09: sans-serif既定と固定幅roleを分離する

- Browserの通常本文は比例幅sans-serifを既定とする
- CSSや具体的なconsumerが無いserifは初期版へ収録せず、必要になった時点の追補計画へ延期する
- `pre`、コード系inline要素、Clockには新しいanti-aliased UI monospaceを必須とする
- Consoleは既存8×16固定幅fontのまま移行対象外とし、固定幅互換を必ず維持する
- GUI defaultは決め打ちせず、Browser本文のsans-serifとは独立にStage 5で決定する

### 2026-09-09: 英語向け収録範囲を拡張する

- ASCIIだけでなくLatin-1のaccent付き文字、curly quote、dash、ellipsis、bullet、euro、trademark等を
  English Latin setとして必須収録する
- U+00AD SOFT HYPHENはglyph収録で意味を誤魔化さず、layout対応を別途行うまで初期対象外とする
- U+00A0 NBSPはspaceと同じadvanceを持つが改行可能なspaceとして扱わない

### 2026-09-10: ASCII 1bppをROMへ残し、日本語を16 px A4へ移行

- 利用者の指示により、従来1bpp blobはprintable ASCII U+0020–U+007Eだけへ縮小した。95 glyph、
  3,175 byteで、圧縮せず通常buildと比較buildの両方からDROMを直接参照する。Consoleと低水準診断で
  非ASCIIを要求した場合は8 pixel幅の中空枠とする
- 日本語はNoto Sans CJK JP Regularを16 pxのA4 strikeとして収録した。JIS X 0213第1面を中心とする
  BMP外のJIS第1面26文字と`𠮷`を含む8,923 glyph、bitmap 738,877 byteで、32 px表示は保存strikeを増やさず整数2倍描画する。日本語24 px
  strikeは収録しない
- 8,923 glyphのうちNotoから生成したものは8,735、Notoに無いか16 px line boxへ収まらないglyphと
  combining markの計188 glyphはUnifont-JPの1bpp形状をA4値0／15として同じblobへ格納した。欠落枠への置換ではないが、この188 glyph
  だけはanti-aliasの中間階調を持たない
- Latin 6 strikeを含むA4 blob全体は10,183 glyph、平文1,021,279 byte、LZ4 container 792,089 byte。
  通常buildでPSRAMへ展開するのはこのA4 blobだけで、ASCIIにはinstall／copy処理を持たせない
- `font-drom-direct`はA4だけを平文DROM参照へ切り替えるA/B featureとして維持した。DROM/IROM境界は
  通常buildが`0x40110000`、比較buildが`0x40150000`
- `fonttest`は1枚目をDROM ASCII専用、2枚目をLatin／日本語A4、3枚目をLatin／日本語のA4対1bit
  比較へ更新し、画面とUARTでASCIIとA4の参照元を別々に判別できるようにした
- generator再現性、host test（browser 206＋fixture 29、ASCII font 17、codec 5、UI font 9ほか）、
  通常／比較release buildとELF XIP配置検査は合格した。通常buildのDROMは1,113,816 byte、IROMは
  1,278,284 byte。比較buildもDROM 1,375,960 byte、IROM 1,275,720 byteで同じ検査に合格した。
  新しい日本語A4の起動、表示品質、長文scroll性能は実機未確認である
- Unifont BDF、DejaVu Sans／Sans Mono TTF、Noto TTC 19,484,784 byteと生成済みA4 blob
  1,021,279 byteはrepositoryへcommitしない。root `Makefile`の`make fonts`が版固定URLからsourceを
  取得し、配布物と使用ファイルのSHA-256一致後にASCII／A4 blobを生成する。新規checkoutは最初の
  Cargo build前にこの処理が必要
