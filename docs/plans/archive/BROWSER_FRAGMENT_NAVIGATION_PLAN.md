# ブラウザfragment navigation改修計画

> 索引: [`../../../DESIGN.md`](../../../DESIGN.md)
> この文書は作業計画です。現在の実装仕様は[`BROWSER.md`](../../BROWSER.md)と
> コードを優先してください。

## 状態: **完了、実機受入済み**

| Stage | 内容 | 状態 |
| --- | --- | --- |
| 0 | 対応範囲と遷移規則の固定（この文書） | 完了 |
| 1 | URL、表示中page、履歴の状態を分離する | 完了 |
| 2 | HTMLのanchor収集とlayout上の行への対応付け | 完了 |
| 3 | 新規遷移、戻る／進む、redirectへの統合 | 完了 |
| 4 | host testと組み込みfixture | 完了 |
| 5 | release build、現状文書更新、実機受入 | 完了 |

## 目的

URLのfragment（`#`以降）をページ内位置として扱う。現在表示中の文書と同じ
文書を指すfragment遷移では、その文書とlayoutをそのまま使い、通信もローカル
ファイルの再読出しも行わない。

履歴項目ごとにscroll位置を保存する。戻る／進む先が現在表示中の文書と同じなら、
fragmentの有無にかかわらず再読込せず、その履歴項目に保存された位置へ戻る。

## 対象外

- 過去に表示したページ内容のcache
- 履歴から同じURLの文書を探して再利用すること
- HTTP cache header、cacheの有効期限、LRU、stale response
- bookmarkの保存
- CSS selector、JavaScript、DOM API
- pixel単位のscroll

現在メモリ上にある1ページだけを再利用する。`A#x`から`B`へ移動した後で
`A#x`へ戻る場合、`A`の内容は残っていないので従来どおり再取得する。将来page
cacheを追加するときは別の計画で有効期限と容量を決め、この改修へ混在させない。

## 用語と不変条件

### 文書URL

scheme、host、port、path、queryからなる。fragmentを含まない。同じ文書かどうかは
既存の`Url::same_document`と同じ比較で決める。

hostの小文字化、default port、pathの正規化は既存の`Url`生成時に済んでいるため、
表示文字列を再解析したり文字列だけで比較したりしない。queryが異なれば別文書で
ある。

### 訪問URL

アドレス欄と履歴が持つ完全なURLで、fragmentの有無と値を含む。`A`と`A#`は異なる
訪問URLである。

### 表示中文書

現在の`Document`と`Layout`、およびそれを取得した最終文書URLである。訪問URLだけが
fragment移動で変わり、表示中文書は変わらない。この区別により、アドレス欄と履歴を
更新しながら、parse済み文書とlayoutを再利用できる。

相対linkは取得完了時の最終文書URLをbaseとしてparse時に解決する。訪問URLの
fragmentを変更してもbaseは変わらない。

### 履歴項目

履歴項目は次だけを持つ。

- 訪問URL
- その訪問を離れる直前のviewport先頭行

文書内容、layout、anchorから計算した初期位置は持たない。同じ訪問URLが複数回
現れても別の履歴項目であり、それぞれ異なるscroll位置を持てる。履歴件数の上限は
既存の`MAX_HISTORY = 8`を維持する。

## 遷移規則

「新規遷移」はlink、address入力、起動URLから始まる遷移を指す。「履歴遷移」は
戻る／進むを指す。明示的な再読込はどちらとも別に扱う。

| 操作 | 現在 | 移動先 | 文書の扱い | 移動位置 |
| --- | --- | --- | --- | --- |
| 新規 | `A` | `A#x` | 現在の文書を使う | anchor `x` |
| 新規 | `A#x` | `A#y` | 現在の文書を使う | anchor `y` |
| 新規 | `A#x` | `A#` | 現在の文書を使う | 文書先頭 |
| 新規 | `A#x` | `A` | **再読込する** | 文書先頭 |
| 新規 | `A` | `A` | 再読込する | 文書先頭 |
| 履歴 | `A#x` | `A#y` | 現在の文書を使う | `A#y`の保存位置 |
| 履歴 | `A#x` | `A` | 現在の文書を使う | `A`の保存位置 |
| 履歴 | `A` | `A#x` | 現在の文書を使う | `A#x`の保存位置 |
| 履歴 | `A` | `B#x` | `B`を再取得する | 取得後に`B#x`の保存位置 |
| 再読込 | 任意 | 同じ訪問URL | 再取得する | 現在の保存位置 |

表中の`A`／`B`は文書URLを表す。`A#x`と`A#y`は同じ文書、`A`と`B`は別文書で
ある。

判定の優先順位を次に固定する。

1. 明示的な再読込は常に再取得する。
2. 履歴遷移で移動先と表示中文書が同じなら、fragmentの有無を問わずページ内遷移にする。
3. 新規遷移で移動先にfragmentがあり、表示中文書と同じならページ内遷移にする。
4. それ以外は従来どおり読み込む。

この順序により、新規の`A#x`→`A`は再読込になる一方、戻る／進むによる同じ遷移は
ページ内遷移になる。履歴中の他の項目を検索する条件はない。

### 同じ訪問URLを新規に選んだ場合

fragment付きの同じ訪問URLをlinkまたはaddress入力で再度選んだ場合、履歴項目は
追加せず、そのanchorへ再scrollする。読者が一度手で離れた後に同じ目次linkを
押しても、anchorへ戻れるようにするためである。

戻る／進むで同じ訪問URLの別履歴項目へ移る場合は別である。履歴操作ではanchorへ
再scrollせず、その項目が保存した位置へ移る。

### `A`と`A#`

fragmentなしの`A`と、空fragmentを持つ`A#`を区別する。

- `href=""`はfragmentを取り除いた`A`であり、新規遷移なら再読込する。
- `href="#"`は空fragment付きの`A#`であり、再読込せず文書先頭へ移る。
- address欄へ完全な`A`を入力する操作も、新規の`A#x`→`A`なら再読込する。
- 戻る／進むでは`A`と`A#`の間も保存位置を使うページ内遷移である。

## 履歴の更新

### 新規のページ内遷移

1. 現在の訪問URLと現在のviewport先頭行を戻る履歴へ積む。
2. 進む履歴を空にする。
3. 訪問URLを移動先へ更新する。
4. 移動先fragmentから初期行を求めてscrollする。
5. link選択と一時status messageを解除する。

同じ訪問URLの再選択だけは1と2を行わない。

### 戻る／進むによるページ内遷移

1. 移動前の訪問URLと実際のviewport先頭行を反対側の履歴へ積む。
2. 対象履歴項目を取り出す。
3. 訪問URLを対象のものへ更新する。
4. fragmentを検索せず、対象履歴項目の保存行へscrollする。
5. 保存行が現在のlayout末尾を越える場合は、既存の`scroll_to`で最終画面へ丸める。

表示中文書が同じでない場合は従来どおり取得を開始し、取得完了後に4を行う。つまり
履歴の保存位置は、再取得の有無にもfragmentの内容にも優先する。

## anchorの定義

### 対象属性

- すべてのHTML要素の`id`
- 互換用として`a`要素の`name`

`a`以外の`name`はanchorにしない。同じ名前が複数ある場合は、文書順で最初のものを
採用する。同じ要素に同じ値の`id`と`name`があっても1件として扱う。

空の`id`／`name`は登録しない。空fragment `#`は空名のanchor検索ではなく、明示的に
文書先頭を表す。

### 照合

URL内ではfragmentをpercent-encodedのまま保持する。anchor検索時にだけ`%HH`をbyteへ
戻し、全体が正しいUTF-8ならHTML parserが復号した属性値と照合する。

- 照合はUnicode文字列の完全一致で、大文字小文字を区別する。
- Unicode normalizationは行わない。
- `+`をspaceへ変換しない。fragmentはform encodingではない。
- 不完全な`%`、16進数でない`%HH`、復号後の不正UTF-8は対象なしとして扱う。
- まず通常のanchorを検索する。見つからず、復号したfragmentがASCII大小文字を
  無視して`top`なら文書先頭へ移る。

### layout上の位置

anchorはparse中の論理的な文書位置として保持し、固定pixel座標として保持しない。
layout生成時に、その位置をscroll可能な行indexへ対応付ける。これにより将来fontや
viewport幅が変わってlayoutを作り直しても、古いpixel位置が残らない。

- textを持つblock要素は、そのblockの最初の表示行。
- inline要素は、その要素の開始位置を含む表示行。
- 表示textを持たない要素は、その後に来る最初の表示行。
- 文書末尾の空anchorは、最終画面の先頭行。
- `hr`のようにtextを持たず自身が表示行を作る要素は、その表示行。
- 得られた行が画面末尾に近い場合は`scroll_to`で最終画面へ丸める。

現在のbrowserは行単位scrollなので、inline anchorをviewport左端へ横移動したり、
画面上端の途中pixelへ置いたりしない。

### 対象がない場合

新規のページ内遷移では、訪問URLと履歴を通常どおり更新し、scroll位置は変えない。
status行へ`fragment not found`を表示する。URLを更新することで、戻る操作が利用者の
直前の操作を正しく逆向きに辿れる。

別文書を取得してから対象がないと分かった場合は、通常の初期位置である文書先頭に
留まり、同じstatusを表示する。戻る／進むではanchor検索をしないため、対象の有無を
理由に保存位置を変えない。

## redirect

fragmentはHTTP request targetへ送らないという既存契約を維持する。redirectの
`Location`は取得中URLをbaseとして解決し、fragmentは次の規則で決める。

- `Location`がfragmentを明示した場合はその値を使う。空の`#`も明示である。
- `Location`が`#`を含まない場合は、redirect前の訪問URLのfragmentを引き継ぐ。
- redirect chainの各hopで同じ規則を適用する。
- address欄、履歴、同一文書判定には最後に到達したURLを使う。

通常の相対link解決はこの継承規則を使わない。たとえば`A#x`上の`href="B"`は`B#x`に
ならない。fragment継承はredirect処理だけの規則として`Url::resolve`の外側に置く。

redirect後の文書が、redirect開始前に表示していた文書と同じURLになっても、開始済みの
取得を途中でpage内遷移へ置き換えない。新規遷移のpage内判定は取得開始前に一度だけ行い、
redirectは1回の取得として完了させる。

## 状態と責務

### `browser/src/url.rs`

既存のfragment保持、`fragment()`、`same_document()`、request targetからfragmentを
外す契約を維持する。anchor検索用のpercent-decodeは、request用decodeと誤認されない
独立した関数にする。redirect時の「fragmentが明示されたか」は空fragmentと欠落を
区別して判定する。

### `browser/src/document.rs`

`Document`へanchor名と論理位置の表を追加する。HTML tokenizerから`id`と、`a`要素の
`name`をbuilderへ渡す。文書のflatなblock／run構造は維持し、DOM treeは導入しない。

### `browser/src/layout.rs`

文書の論理anchor位置を行indexへ対応付ける。描画時に毎回文書全体を検索せず、layout
完成時に検索可能な対応を作る。対応表も`owned_bytes()`へ含める。

### `src/app/browser.rs`

`Page`が表示中文書のURLと現在の訪問URLを区別して持つ。toolbar、status、履歴への
保存、再読込は訪問URLを使い、相対linkと表示中文書の同一性は文書URLを使う。

`Direction::Fresh`／`Back`／`Forward`／`Reload`を失わずに、取得開始前の純粋な判定で
「再取得」「anchorへ新規移動」「履歴位置へ移動」の3つへ分ける。この判定はhost test
可能な層へ置き、入力handlerごとに条件を複製しない。

### `src/app/fetch.rs`

redirectだけにfragment継承規則を追加する。networkへ渡すrequest target、security
downgrade拒否、redirect回数上限は変更しない。

## 上限と失敗

anchor追加によってbrowser-owned peakを不必要に増やさない。

- anchor件数上限は1,024件。
- 1つのanchor名は`MAX_URL_BYTES`を越えたら登録しない。その名前はbrowserが持てる
  URLから指定できないためである。
- 登録するanchor名の合計capacityは256 KiBを上限とする。
- 件数または合計capacityを越えたページは専用のlimit errorにして、途中までのanchor
  表を正常な完成文書として表示しない。
- 重複anchorは最初の1件だけを保持し、件数・capacityへ重ねて数えない。
- anchor表とlayout対応表を`Document::stats().owned_bytes`およびlayoutの
  `owned_bytes()`へ含める。

上限値はhost testで構造体費用を計算し、不自然に大きければ上限を緩めず表現を
小さくする。page cacheのための容量は確保しない。

fragment検索や履歴へのpushでallocationに失敗した場合、現在の文書、訪問URL、履歴、
scroll位置を変更せず、既存のout-of-memory表示へ進む。状態更新は必要なallocationが
すべて成功した後にcommitする。

## Stage 1: URL、page、履歴の状態分離

- `Page`へ現在の訪問URLを明示して、`Document::url()`だけをaddressとして使わない。
- 同一文書比較と操作種別から遷移方法を返す純粋な判定を作る。
- `push_current`、address編集開始、toolbar、reloadが訪問URLを使うよう統一する。
- 同一文書の履歴移動は`Document`／`Layout`を置換せずにsettleできるようにする。

**完了条件:** 遷移表の全組合せがhost testで固定され、既存の別文書遷移、進む履歴の
破棄、reloadの履歴非追加を変えていない。

## Stage 2: anchorとlayout

- tokenizerが`id`と`name`をboundedに渡す。
- builderが重複、空名、上限を処理し、論理位置を保存する。
- layoutがblock、inline、空要素、rule、文書末尾を行へ対応付ける。
- percent-decodeと`top` fallbackを追加する。

**完了条件:** ASCII、日本語percent-encoding、重複、空fragment、存在しない対象、
inline折返し、空anchor、末尾anchorがhost testで決まる。

## Stage 3: navigation統合

- link、address入力、戻る、進む、reloadを共通判定へ通す。
- page内遷移の履歴更新を追加する。
- redirectのfragment継承を追加する。
- 読み込み中のcancel、Wi-Fi再接続待ち、error pageの既存挙動を回帰確認する。

**完了条件:** page内遷移で`Fetch`／`LocalRead`が開始されず、別文書と新規の
`A#x`→`A`だけが従来の読み込み経路へ入る。

## Stage 4: host testとfixture

最低限、次を自動試験にする。

| 分類 | ケース |
| --- | --- |
| URL | `A`、`A#`、`A#x`の区別、request targetからfragment除外 |
| 判定 | この文書の遷移表の全行、新規と履歴の差 |
| 履歴 | fragmentごとの位置、同じ完全URLの複数項目、forward破棄 |
| parser | 全要素の`id`、`a[name]`、重複、空名、上限 |
| decode | `%20`、UTF-8、`+`、不正`%`、不正UTF-8、case-sensitive |
| layout | heading、inline途中、折返し、空要素、`hr`、文書末尾 |
| targetなし | 新規では位置維持、別文書load後は先頭、履歴では保存位置 |
| redirect | fragmentの指定、空指定、欠落時の継承、複数hop |
| file | `file:`の同一文書fragment移動で再読出しなし |

組み込みfixtureへ、画面を越える前文、目次link、複数の`id`、`a[name]`、inline anchor、
空anchor、存在しないanchor、文書先頭へ戻る`#`を持つページを追加する。

## Stage 5: build、文書、実機受入

実装後に`cargo build --release`を通す。`cargo run --release`と`espflash`は実行しない。
現状仕様が変わるのは実装完了後なので、その時点で[`BROWSER.md`](../../BROWSER.md)を更新する。
`README.md`は変更しない。

実機では書き込み後、組み込みfixtureを使って次を人間に確認してもらう。

1. 目次から2つ以上のfragmentへ移動しても読み込み表示や通信待ちにならない。
2. 各fragmentで手動scrollした後、戻る／進むがそれぞれの位置へ戻る。
3. 戻る／進むの`A#x`↔`A`が再読込にならない。
4. linkによる新規の`A#x`→`A`は再読込になる。
5. `A#x`→`B`→戻るは`A`を再取得してから保存位置へ戻る。
6. keyboard、touch、mouseのlink操作で同じ結果になる。
7. 長文で連続移動しても表示崩れ、残像、著しい遅延、heapの継続増加、display underrunがない。

期待する結果は上の7項目がすべて成立し、UARTにpage内遷移由来のHTTP接続開始やfile
openが出ないことである。失敗時は、不要な読み込み表示、先頭への誤移動、戻るたびの
anchor再計算、address欄のfragment不一致、forward履歴消失、またはheap値の継続増加として
見える。

### 実機結果

2026-09-11、長文化した`http://built-in/fragments`を使った実機確認を受入済み。
確認回数など、報告されていない数値は記録しない。
