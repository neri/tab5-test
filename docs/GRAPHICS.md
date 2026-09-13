# 描画API

> 索引: [`../DESIGN.md`](../DESIGN.md)

`src/framebuffer.rs`の`Framebuffer`は次のRGB565描画処理を提供します。

- `fill`、`draw_pixel`
- `draw_line`
- `fill_rect`、`stroke_rect`
- `fill_circle`、`draw_circle`
- `blit_rgb565`、`read_rect`
- `blit_rgb565_scaled`
- `draw_glyph`、`draw_text`
- `set_vertical_clip`、`set_horizontal_clip`
- `scroll_up`

`fill`と`fill_rect`は呼び出し側から見える挙動を変えずに、内部でCPUのストア
ループとPPAのDMA塗りを切り替えます（判断基準と実測は
[`DISPLAY_BANDWIDTH.md`](DISPLAY_BANDWIDTH.md)の「対策: 塗りつぶしと転送を
PPA/2D-DMAへ移す」）。`scroll_up`は2D-DMAのブロックコピーで画面の帯を上へ
ずらすもので、コンソールのスクロールがこれを使います。いずれもDMA経路を通った
場合はキャッシュの整合を自分で取るため、呼び出し側の`flush`／`flush_rect`は
どちらの経路でも正しいままです。

`set_vertical_clip(top, bottom)`は後続のCPU・PPA・A4・従来glyph描画を論理Yの
`[top, bottom)`へ制限し、以前の範囲を返します。Browserはviewport描画の間だけこれを
設定し、上下に一部だけ見える文字・選択背景・罫線がtoolbar/statusへ出ないように
します。呼び出し側は描画後に返された範囲を復元します。この経路は2026-09-12に
Browserの長いtable fixtureで実機確認済みです。

`set_horizontal_clip(left, right)`は後続のpixel、矩形塗り、A4 glyph描画を論理Xの
`[left, right)`へ制限します。Browserのアドレス編集はdamage矩形ごとにこれを一時設定し、
displayが直接走査しているframebuffer上でも矩形外を先に消去しないようにします。

`blit_rgb565_scaled`はrow-major RGB565を指定矩形へ最近傍拡縮し、先頭でclipされた
表示行をsource側の対応行へずらせます。Browserのdecode済み画像がpixel scroll中にも
同じ画像位置を保つために使います。Stage 3の実機表示は未確認です。

診断用にはproductionの自動選択を通さない`diagnostic_fill_rect_with_cpu`と、cache同期を
含まない`diagnostic_ppa_fill_rect_raw`をcrate内だけへ公開します。前者は呼び出し側が
`flush_rect`を行うまでdirty cacheを残し、後者は呼び出し前に対象のcache所有権を解消する
必要があります。通常の描画コードからは使わず、`ppafill`と`displaybench`が特定経路を
強制して比較するためだけのAPIです。

`read_rect`は`blit_rgb565`の逆方向で、論理矩形をフレームバッファから読み出します。
右端・下端のクリップ方法と`pixels`側の行ストライド（`image_width`）が`blit_rgb565`と
同一なので、`read_rect`で退避した矩形を`blit_rgb565`で書き戻せば、矩形が画面外へ
はみ出している場合でも元の内容がそのまま復元されます。動くスプライトが下地を
退避・復元するための経路で、`src/app/pointer.rs`の共通カーソルが使います。読み出しは
通常のキャッシュ経由のロードなので、CPU描画ともPPA描画とも整合します（PPAは転送後に
該当領域を無効化するため、次の読み出しはDMAが書いた内容を取り直します）。

### 文字描画

通常GUIは`draw_ui_text`／`draw_gui_text`を使います。English Latinは生成済みA4 glyphを
RGB565へalpha blendし、Sansは比例幅、Sans Monoは固定幅です。背景なしでは既存pixelを読み、
背景ありではadvance boxを消してから描きます。描画coreは`tab5-ui-font::paint_glyph`にあり、
host testとfirmwareが同じclip／blend処理を使います。日本語、未収録記号、combining clusterは
次の従来経路へfallbackします。

`draw_glyph`は`crate::font`（`tab5-font` crate）の16 pixel glyphを1つ描きます。
lookupは行わず、渡されたglyph自身の幅の枠——半角8列、全角16列、combining markは
16列——にピクセルを置くだけです。`draw_text`がlookup、combining、文字送りを扱い、
描いた幅をpixelで返します。

文字送りは`font::advance`が正で、ブラウザの折返しとhit判定も同じ関数を使います。
描いた結果と測った結果が食い違わないようにするためです。収録していない文字は
飛ばさずに同じ幅の中空枠を描きます。空白にすると、そこで文字列が終わったように
見えるためです。

combining markは直前の文字へ重ねて描き、自分の幅を持ちません。直前が無い場合
（文字列の先頭、改行の直後）は重ねる相手がいないので、U+FFFDを1文字として描きます。
combining markの描画は必ず背景なしです。markの枠は左右の隣の文字と重なるため、
背景ありで描くと隣を消します。

従来1bpp文字描画の経路はこれだけです。以前あった5×7 ASCIIフォントと
`draw_text_5x7`／`draw_ascii_char_5x7`は削除しました
（[`FONT_MIGRATION_PLAN.md`](FONT_MIGRATION_PLAN.md)）。

入口は1つですが、実際の描画は`WideGlyph::paint_opaque`と
`WideGlyph::paint_sparse`に分かれています。避けるべき仕事が逆だからです。背景ありでは
枠内の全ピクセルを書くので、1列が連続したネイティブ範囲になり、回転の解決
（720倍の乗算）をピクセル単位ではなく列単位にできます。背景なしでは書くピクセルが
枠の1/3以下と疎なので、逆に「書かずに飛ばす」ほうを最適化し、ビットが立っていない
グリフ列はアドレス計算の前に飛ばします。どちらも列を降順に回すのは`fill_rect`と
同じ理由で、CW回転により論理Xの降順がネイティブアドレスの昇順になるためです。
いずれも`draw_pixel`を経由しないので、境界判定とフレームバッファ取得は
1ピクセルごとではなく1列ごと・1回だけです。

フォントデータそのもの（文字幅の契約、収録範囲、生成物の形式、由来）は
[`FONT.md`](FONT.md)にあります。

描画APIの論理解像度は1280×720 Landscapeです。CW回転のため、論理座標を
ネイティブフレームバッファへ次のように変換します。

```text
native_x = logical_y
native_y = 1279 - logical_x
```

DSIの解像度やパネル初期化コマンドは変更せず、全描画プリミティブと画像転送に
同じ座標変換を適用します。
