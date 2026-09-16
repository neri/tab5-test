# Browser table対応計画

> 索引: [`../../../DESIGN.md`](../../../DESIGN.md) ／ 現状仕様:
> [`BROWSER.md`](../../BROWSER.md)

## 目的

Browserで`table`を単なるblock境界ではなく表として表示する。列幅、セル内折返し、
行高、罫線、リンクの当たり判定を揃え、正整数の`rowspan`／`colspan`にも対応する。

横scroll、CSS、表示属性、`col`／`colgroup`、nested table、`rowspan=0`の特殊意味は
初期対象に含めない。表は本文幅内へ収め、長い内容はセル内で折り返す。

## 実装状況

2026-09-11にStage 0〜7を完了した。`mise run test`と`cargo build --release`を通し、
組み込み確認ページ`http://built-in/table`とfixture serverを使ったTab5実機確認について、
表示・操作ともここまで問題なく動作したとの報告を受けて受入済みとした。
追加要件としてHTMLの`border`属性も反映する。未指定／`0`／非数値は罫線なし、
値なし／空／正整数は罫線ありで、描画上限は4 pixelとする。

## Stage 0: 仕様と上限を固定する

- 対応要素は`table`、`caption`、`thead`、`tbody`、`tfoot`、`tr`、`th`、`td`。
- `rowspan`／`colspan`は正整数を受け付ける。欠落、空、非数値、`0`は`1`とし、
  上限超過は上限へ丸める。
- 最大列数と各spanの初期上限は32とする。cellも既存の`MAX_ITEMS`へ算入する。
- 1文字と左右paddingを置ける幅を列の下限とし、表全体を本文幅から出さない。
- 構造上限を超えたページは切り詰めず、既存方針どおりページ全体をerrorにする。

**完了条件:** 互換範囲、縮退規則、上限がコードと本書で一致している。

## Stage 1: tokenizerでspan属性を取得する

`browser/src/html.rs`の`Tag`へ`rowspan`と`colspan`を追加する。既存の`href`、`alt`、
`id`と同じく、対象属性だけを上限付きbufferへ保持する。tokenizerは値を文字列で渡し、
数値化と既定値の判断はdocument builderへ置く。

host testではquoted／unquoted、重複属性、空、非数値、長すぎる値を扱い、HTMLを一括、
1 byteずつ、複数のchunk幅で投入して同じevent列になることを確認する。

**完了条件:** span属性がchunk境界に依存せず渡り、既存tokenizer testがすべて通る。

## Stage 2: flatなtable文書モデルを追加する

DOMは導入せず、table、row、cellをindexで結ぶflatな構造を`Document`へ追加する。

- tableはcell範囲、row数、column数を持つ。
- rowはtable内の行番号と`thead`／`tbody`／`tfoot`の区分を持つ。
- cellはrun範囲、開始row／column、`rowspan`／`colspan`、`th`か`td`かを持つ。
- captionはstyled runを保持し、table直前の独立した内容として扱う。
- 空cellも配置に必要なので保持する。

cell開始時は、過去のrowspanが占有している列を飛ばして最初の空き列へ置く。colspanの
範囲を現在行で占有し、rowspanの範囲を後続行へ予約する。競合する不正markupでは既存cellを
優先し、後続cellを次の空き列へ送って必ず前進させる。

新しいcellは前のcellを、新しいrowは前のcellとrowを、table終了は開いているcellとrowを
暗黙終了する。`tr`なしのcellには暗黙のrowを作る。table外の表要素は通常のblock境界へ
縮退し、nested tableは外側cell内の通常内容として扱う。

host testには通常の2×2、header、caption、row group、終了tag省略、空cell、table外のcell、
nested table、span競合、構造上限を含める。

**完了条件:** HTMLから期待するgridが得られ、cell内のstyle、link、画像alt、改行を失わない。

## Stage 3: 列幅を計算する

表全体を描く前に全cellを測定する。文字幅は既存のfont metricsを使い、比例幅Latin、
日本語、mono、combining markを通常本文と同じ規則で扱う。

1. cellごとに折返し不能な最長単位の最小幅と、改行しない希望幅を求める。
2. colspanなしのcellから各列の最小幅と希望幅を作る。
3. colspan cellが要求する不足分を対象列へ分配する。
4. 左右paddingと罫線を含め、希望幅合計が本文幅以内なら希望幅を採る。
5. 超過時は最小幅を下限として余った幅を比例配分する。
6. 最小幅合計も本文幅を超える場合は1 glyphとpaddingまで縮め、cell内で強制改行する。

host testには長さの異なる列、日本語、colspan cellだけが長い表、多列表、本文幅ぎりぎりの
表を含め、右端が本文幅を超えず計算結果が決定的であることを確認する。

**完了条件:** 各列のx座標と幅がhost上で確定し、表が本文領域からはみ出さない。

## Stage 4: cell内layoutとrow高を計算する

確定した列幅から左右paddingを引き、cellごとに既存規則で文章を折り返す。通常rowの高さは
そのrowに始まる`rowspan=1` cellの最大必要高とし、空row／cellにも最低1行分を与える。

rowspan cellの必要高が対象row群の高さと内部境界の合計を超える場合、不足分をspan最終rowへ
加える。内容は結合矩形の上端から配置する。colspan／rowspanの内側を横切る罫線は生成しない。

既存の`Line`／`Piece`を文字に再利用し、cell矩形と罫線を別の軽量配列へ保持する。
pieceのx座標にはtableとcolumnの位置を反映し、既存のlink順序、選択、hit testをその実座標で
動かす。anchorはcell内で生成された該当lineへ対応させる。

host testには複数行cell、空cell、rowspanの高さ不足、colspan内折返し、`br`、code、太字、
link、viewport境界、layout line上限と所有memory計算を含める。

**完了条件:** cell内容と後続rowが重ならず、span矩形、link位置、anchor位置が一致する。

## Stage 5: firmware描画へ接続する

`src/app/browser.rs`で、背景、viewportと交差するcell背景、罫線、cell内文字、link選択の順に
描く。cell paddingは固定pixel値、外枠と内罫線は通常文字より薄い色、`th`は太字とする。
captionは表の上へ通常本文相当の間隔で置く。CSS由来の色や罫線は反映しない。

従来どおり行単位の縦scrollとし、横scrollは追加しない。viewport途中からの描画、部分再描画、
戻る／進む後の再描画でも罫線や文字の残像を残さない。

**完了条件:** host testに加えて`cargo build --release`が通り、通常文章の描画経路に回帰がない。
この時点の実機挙動は未確認と記録する。

## Stage 6: fixtureと統合試験を用意する

fixture serverまたは組み込みページへ次を含むtable確認ページを追加する。

- caption、header、通常cell
- 内容長が大きく異なる3列
- 日本語とASCIIの混在
- cell内link
- rowspan、colspan、両者の組合せ
- 空cell、終了tag省略、不正span値
- 多列で強制折返しになる表

自動確認は次を実行する。

```sh
mise run test
cargo build --release
```

**完了条件:** parser、grid、列幅、layout、hit testの自動試験が通り、release buildが通る。

## Stage 7: 実機受入と現状文書を同期する

人間がrelease firmwareを書き込み、Browserでtable fixtureを開く。先頭から末尾までscrollし、
通常cell、rowspan、colspanの罫線、長文の折返しを確認する。`Tab`とtouchの両方でcell内linkを
辿り、戻った後の再描画も確認する。

期待する結果:

- 表が本文幅からはみ出さず、隣接cellの文字が重ならない。
- 結合cellを内部罫線が横切らない。
- 長い内容がcell内で折り返される。
- linkの選択背景、下線、touch位置が一致する。
- scrollと画面復帰で罫線や文字の残像が出ない。

失敗時に見える症状:

- rowspan後のcellが誤った列へ入る。
- colspan幅と罫線位置が一致しない。
- 次のrowが折返した文字へ重なる。
- scroll後だけ罫線が欠ける。
- table内linkのtouch領域が横へずれる。
- 多列表で不必要にlayout／memory上限へ到達する。

実装完了時は`docs/BROWSER.md`へ対応範囲と制約を反映する。2026-09-11、release firmwareを
書き込んだTab5で組み込み確認ページとfixture serverを確認し、表示・操作とも問題なしとの
報告を受けた。`README.md`は変更しない。

**完了条件:** 人間から実機結果を受け取り、確認日、構成、fixture、結果を本書へ記録する。
