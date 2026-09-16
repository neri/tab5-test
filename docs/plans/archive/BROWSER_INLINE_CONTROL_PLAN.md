# Browser inline control・装飾button対応計画

> 索引: [`../../../DESIGN.md`](../../../DESIGN.md) ／ 現状仕様:
> [`BROWSER.md`](../../BROWSER.md)、関連する完了済み計画:
> [`BROWSER_EXTENSION_PLAN.md`](BROWSER_EXTENSION_PLAN.md)

## 目的と状態

Browserの`input`、`select`、`button`を、現在の独立したblock配置から本文と同じ行に
参加するinline controlへ変更する。通常本文を先に対応し、同じ配置器をtable cellへ
適用する。あわせて`button`の子として、装飾された文字と静止画像を限定的に表示する。

本書は2026-09-13時点の合意と実装記録である。**Stage 1〜6は実装、host検証、実機受入まで
完了した。** 通常の独立画像を含むtable cellで画像の前後を共通inline配置器へ通す最終差分も、
同日に実機確認済みとの報告を受けた。

| Stage | 内容 | 状態 |
| --- | --- | --- |
| 0 | スコープ、配置規則、段階分けの固定（本書） | 完了 |
| 1 | 文書モデルとparser | 完了（実機受入済み） |
| 2 | 通常本文のcontrol inline配置 | 完了（実機受入済み） |
| 3 | button内の装飾文字 | 完了（実機受入済み） |
| 4 | button内画像 | 完了（実機受入済み） |
| 5 | table cellのcontrol inline配置 | 完了（実機受入済み） |
| 6 | 統合、現状文書更新、実機受入 | 完了（実機受入済み） |

Stageは原則として番号順に進める。Stage 1で後続Stageが共有する文書表現を作り、
Stage 2で通常本文用の共通inline配置器を成立させる。Stage 3と4はbutton内容の
複雑さを文字と画像に分け、Stage 5は同じ配置器をtableの計測・配置へ接続する。

## 現状と変更理由

現在のparserはvisible controlを作るたびに前の本文blockを閉じ、
`BlockKind::Control`を独立して追加する。layoutはそのblockを`x = 0`、text・select等は
最大320 pixel、checkbox・radioは正方形として置き、上下に余白を加える。そのため、
次のHTMLでもlabel、control、後続文字は同じ行にならない。

```html
<label for="q">Query</label> <input id="q" name="q"> <button>Search</button>
```

table cellだけはcontrolのtext offsetを保持するが、現在はtext・image・controlを
文書順に縦へ積む。描画、hit test、focus damageは既に`ControlBox`の任意の矩形座標を
使うため、主な変更対象はdocumentとlayoutである。編集、送信、select popupの状態機械を
別方式へ作り直す理由はない。

## 確定したスコープ

| 項目 | 方針 |
| --- | --- |
| 通常本文 | `input`、`select`、`button`を本文と同じinline flowへ入れる |
| table cell | 通常本文と同じ配置器をStage 5で使用する |
| `textarea` | 今回も独立したblock配置を維持する |
| hidden | 文書順と送信値だけを保持し、幅・高さ・折返しへの影響は0 |
| text・select幅 | 希望幅320 pixelを維持。配置先が狭いときだけ縮小する |
| submit・button幅 | 表示内容の幅＋左右padding合計12 pixel。80〜320 pixelへ収める |
| checkbox・radio | 現在の1行分の高さの正方形を維持する |
| control内改行 | buttonを含め1行。内部折返しは行わず、上限を越す内容はclipする |
| CSS | 引き続き対象外。`display`、`width`等のCSSは解釈しない |
| interaction | focus、touch、編集、選択、送信の意味は変更しない |
| button文字 | 通常文字と既存の装飾タグだけを表示する |
| button画像 | 既存のPNG/JPEG取得・decodeを再利用し、buttonの1行内へ縮小表示する |
| button内の構造 | block性と子の独立したUI interactionを持ち込まない |

`input type=submit`と`button`は同等の操作なので、両方をinlineにする。一方、複数行編集を
行う`textarea`は1行のinline flowへ混ぜず、現在のbox内scrollと高さを維持する。

## 共通設計契約

### 文書順とinline object

controlは文字列へ見せかけた空白や代替文字を埋め込まず、文書モデル上のinline objectとして
保持する。文字offsetとcontrol IDの組で出現位置を表し、同じoffsetに複数のcontrolがある場合は
control ID、すなわちparseした順に並べる。placeholder文字を本文へ入れないため、検索対象の本文、
label範囲、anchor、link範囲をcontrolの都合で変えない。

各通常blockは、そのblockに属するcontrol範囲を明示的に持つ。table cellは既存の
`first_control`／`control_count`を使う。これにより文字runが0件でcontrolだけを含むblockも
捨てずにlayoutできる。hiddenも範囲には含めるが、配置時は消費幅0として飛ばす。

通常本文とtable cellは、次の入力を受ける共通inline配置器を使う。

- 配置原点`x`、`y`
- 利用可能幅
- text範囲とstyle run
- その範囲に属するcontrol列
- 見出し／table header等、rendererへ渡す既存属性

配置器は計測と実配置で同じ幅・改行規則を使う。table row高を先に測る経路と、実際の
`Line`／`ControlBox`を作る経路で別の規則を複製しない。

### 折返しと空白

controlの前に現在行の空きが希望幅以上あれば同じ行へ置く。入らず、現在行に既にvisibleな
文字またはcontrolがあればcontrolの直前で改行する。空行の先頭でも配置先全幅より広い場合だけ、
その幅へ縮小する。control後に余地があれば後続文字と次のcontrolは同じ行を続ける。

controlの前後へlayoutが勝手に空白を足さない。HTML sourceにある通常空白だけを従来の規則で
1個へ畳み、`<br>`は強制改行する。次は接して表示する。

```html
before<input>after
```

次は両側に畳まれた空白を1個ずつ持つ。

```html
before <input> after
```

行高はその行の文字line boxとcontrolの最大高とする。文字と各controlは行の上下中央へ置く。
配置にbaseline情報を新設せず、既存fontのline boxを単位にする。これによりcheckbox等が本文の
上端または下端へ偏らず、rendererとlayoutのfont metricsも分岐しない。

### controlの希望寸法

| control | 希望幅 | 高さ |
| --- | ---: | ---: |
| text input、select | 320 px | 本文line box＋上下4 px |
| input submit | 表示値＋左右各6 px、80〜320 px | 本文line box＋上下各4 px |
| button | 子内容＋左右各6 px、80〜320 px | 本文line box＋上下各4 px |
| checkbox、radio | control高と同じ | 本文line box＋上下4 px |
| textarea | 配置先幅と320 pxの小さい方 | 現行のrows計算 |

disabledでも寸法は変えない。selectは選択肢や現在値の長さでlayoutを変えない。選択変更のたびに
文書全体が動くのを避けるためである。buttonの画像は後述の上限へ縮小してから内容幅へ加える。
contentが320 pixelを越すbuttonはboxを320 pixelに保ち、右側をclipする。

### buttonで表示する子内容

buttonの本文は現在の単一`display_label`だけでなく、表示文字のstyle runと画像の挿入位置を
保持する。送信する`value`と表示内容は引き続き別物であり、子内容が送信値を変更しない。

対応する内容は次だけとする。

| 子内容 | 扱い |
| --- | --- |
| 通常text | HTML空白を1個へ畳んで表示 |
| `strong`、`b` | 既存のbold（二度打ち） |
| `em`、`i` | 既存の斜体表示（当初は濃い赤、後に合成斜体へ変更） |
| `code`、`kbd`、`samp`、`tt`、`var` | 既存のcode色・mono face |
| `img` | button内画像として表示 |
| 上記以外の通常inline tag | tagの意味を無視し、子のtext・許可された装飾・画像は残す |
| block tag | 改行や段落を作らずtagのblock性を無視し、子の表示可能内容は残す |
| `a`、`label` | link・labelとしてのinteractionを無視し、子の表示可能内容は残す |
| nested `input`、`select`、`textarea`、`button` | 要素とその専用内容をcontrolとして作らず、要素全体を表示対象外にする |
| `script`、`style` | 現在と同じく内容ごと表示しない |

したがって`<button><div><strong>OK</strong></div></button>`は、改行のないboldの`OK`に
なる。`<button><a href="/x">Open</a></button>`は`Open`を表示するがlinkを作らず、どこを
押しても外側buttonだけが動く。誤ったnested controlのoptionやvalueがbutton labelやformの
control列へ漏れないよう、nested controlは対応するend tagまで抑止する。

styleは入れ子を数えて合成する。同じbitを使う`strong`と`b`が入れ子になっても、内側のend tagで
外側のboldを消さない。対応外tagはstyle stackを変えない。button表示文字は既存の
`MAX_INPUT_VALUE_BYTES`、style run等の構造は`MAX_ITEMS`、画像は文書全体の`MAX_IMAGES`へ
数え、上限で完成したように切らず既存のpage errorを返す。

### button内画像

button画像は通常本文画像と同じ解決済みURL、指定寸法、intrinsic寸法、decode slot、取得順、
security規則、圧縮・展開上限を使う。ただし文書本文やtable cellの独立画像として配置せず、
所有button IDとbutton label内の挿入offsetを持つ。通常画像とbutton画像を同じ画像ID空間に置き、
遷移、suspend、Wi-Fi復帰、重複URL共有、失敗の寿命を分けない。

画像は指定寸法または仮寸法／intrinsic寸法を求めた後、buttonの内側の本文line box高を越える場合に
縦横比を保って縮小する。さらにbuttonの最大内容幅へ収まらない場合も縦横比を保って縮小する。
拡大はしない。画像と文字はmarkup順に1行へ並べ、画像の上下位置はbutton内で中央にする。

decode前は同じ寸法のplaceholderを置く。decode成功で未指定軸が変わった場合は既存の
`ReadingPosition`を保存して再layoutし、viewportの論理位置とfocus中controlを維持する。
失敗時は予約領域内へ`alt`、空なら短い失敗表示をclipして描く。button内画像自身はfocus、link、
hit targetを持たず、画像上のtapも外側buttonの操作となる。

### interactionと局所再描画

`ControlBox`はinline化後もdocument座標の矩形である。既存のcontrol hit test、label touch、
focus順序、focus追従scroll、text編集、checkbox/radio切替、select popup、GET/POST送信をこの矩形へ
適用する。focus順序はlayout後の`(y, x, 文書順)`でlinkとcontrolを統合し、同じ行でも左から右へ
進むようにする。

focus・checkedness・編集のdamageは従来どおり旧新control矩形に限定する。inline化で隣の本文や
controlを消さないよう、damage矩形を使うviewport rendererが同じ行のtextも再描画することを
host testと実機で確認する。select popupの基準位置は新しい`ControlBox`とし、画面上下端で上／下へ
開く既存規則を変えない。

## Stage 0: 設計固定（完了）

本書で通常本文、button内容、button画像、table cellを分ける実装順と、上記の寸法・空白・
無視規則を固定した。実装も実機確認も行っていない。

**完了条件:** 後続Stageが必要とする文書表現、配置規則、button子要素のallowlist、
実機でしか判断できない項目が本書に記録されている。

## Stage 1: 文書モデルとparser（完了・実機受入済み）

通常blockへcontrol範囲を追加し、visibleなinline controlだけを含むblockも保持する。control作成時に
本文blockを強制終了する処理は、textarea以外について撤去する。table cellの既存control範囲と
form内のcontrol順序は維持する。

buttonにはstyled text runと画像挿入位置を持たせる。button中だけで使うstyle stack、対応外tag、
block tag、interaction tag、nested controlの抑止をparserへ追加する。button画像は所有buttonを
識別できるが、Stage 4までは取得・描画へ接続しない。既存layoutはこのStageではblock風の互換配置を
一時的に維持してよい。

host testには少なくとも次を加える。

- text–input–text、連続control、source空白あり／なしの文書順
- controlだけのblockとhiddenだけのblock
- textareaが独立blockのままであること
- buttonのstyle入れ子、block tag無視、link interaction無視
- nested input/select/textarea/buttonの内容が漏れないこと
- button画像のURL、寸法、alt、所有button、挿入順
- 上限到達時にtruncateした成功文書を返さないこと

**完了条件:** `cargo test -p tab5-browser`が通り、旧formの送信順・label範囲・table cell所属を
壊さず、新しい文書表現をhostから検査できる。表示変更は未確認と明記する。

## Stage 2: 通常本文のcontrol inline配置（完了・実機受入済み）

共通inline配置器の最初の利用先を通常本文にする。文字とcontrolをoffset順に走査し、残り幅、
強制改行、source空白、可変行高を反映して`Line`と`ControlBox`を作る。textareaの
`BlockKind::Control`経路は残す。描画側は既存の任意座標`ControlBox`を再利用し、submit/buttonだけ
内容幅に基づく希望幅へ変える。

host layout testには、同じ行へ収まる場合、control直前で折り返す場合、control後へ本文が続く場合、
極端に狭い幅、checkbox/radio、hidden、`br`、複数control、見出し／list内を含める。hit testと
focus順序が画面上の左から右、上から下になることも固定する。

**完了条件:** host testと`cargo build --release`が通る。通常本文のinline表示、focus、touch、編集、
select popupは実機未確認のままStage 3へ進められるが、回帰原因を分離するためfixtureはこのStageで
用意する。

## Stage 3: button内の装飾文字（完了・実機受入済み）

buttonの希望幅をstyled textの実測幅から求め、rendererがrunごとに既存のbold、italic、code描画を
使うようにする。button面がfocus色のときも可読な前景色を選び、code／italic色がfocus面で
読めない場合は白を優先する。clipはbutton内側とviewport damageの積を使い、長いlabelが隣の本文へ
はみ出さないようにする。

host testはstyle別の幅、複合・入れ子run、空button、長いlabel、非ASCII、unsupported tagを扱う。
組み込みform fixtureへplain、styled、block tag内、link tag内のbuttonを追加する。

**完了条件:** host testとrelease buildが通り、装飾のない既存buttonの送信値・表示labelを維持する。
装飾色と二度打ちの見た目は実機未確認とする。

## Stage 4: button内画像（完了・実機受入済み）

button所属画像を既存の画像取得列へ接続し、decode slot共有、失敗状態、intrinsic寸法確定後の
再layoutを通常画像と同じpage世代で管理する。layoutとrendererはbutton内で文字runと画像を
挿入offset順に並べ、画像をcontent line高と残り幅へ縮小する。button画像を通常本文の
`ImageBox`、link focus順、tableの独立画像計測へ混入させない。

pure testと組み込みfixtureには寸法4種、画像の前後に文字、複数画像、装飾文字との混在、長い内容、
alt、decode失敗、同一URL共有を加える。LAN fixtureには遅延画像を含むbuttonを追加し、仮寸法から
intrinsic寸法へ変わる再layoutを確認できるようにする。

**完了条件:** host testとrelease buildが通り、画像の成功・失敗がbutton外の本文と他画像を
壊さない。取得、表示、再layout、tapは実機未確認とする。

## Stage 5: table cellのcontrol inline配置（完了・実機受入済み）

table cellでtextとcontrolを縦積みする現在の計測・配置を、Stage 2の共通inline配置器へ置き換える。
cellの左右padding内を利用可能幅とし、controlが収まれば前後のtextと同じ行、収まらなければ直前で
折り返す。buttonの装飾文字と画像も同じcontrol寸法を使う。

列希望幅は選択変更で変わらないcontrol希望幅から一度だけ計算し、既存のtable全体fit後に確定した
cell幅で再度折返しを測る。実配置から列幅を戻して再計算する循環は作らない。row高はinline flowの
全行高と上下paddingを包み、rowspanの既存分配規則を維持する。button所属画像はcellの独立画像数・
画像高へ二重計上しない。

host testには次を含める。

- label–input–textが1行へ収まるcellと、狭くて折り返すcell
- 同一行のcheckbox/radio/select/button
- button内画像のあるcell
- 隣接cell、複数row、rowspan／colspanとの非重なり
- cell padding内のhit testとfocus順序
- 画像・control・通常の独立画像が同じcellにある場合の文書順

**完了条件:** host testとrelease buildが通り、既存tableの文字、罫線、独立画像のfixtureに回帰が
ない。実機では未確認とする。

## Stage 6: 統合、文書更新、実機受入（完了・実機受入済み）

組み込み`http://built-in/forms`を、通常本文とtable cellの両方で次を一度に見られるfixtureへ
更新する。

- label、text input、select、submit/button、後続textのinline配置
- 行末での折返しと上下端clip
- checkbox/radioとlabelの位置関係
- plain／bold／italic／code／複合styleのbutton
- block tag、link tag、nested controlを含むbutton
- 成功画像、遅延画像、壊れた画像を含むbutton
- tableの広いcellと狭いcell、rowspan／colspan

実装の現状に合わせて`BROWSER.md`を更新する。責務分割や描画APIが変わった場合は
`FILE_LAYOUT.md`／`GRAPHICS.md`も同期する。実機結果は人間から報告された後にだけ本書と現状文書へ
記録する。

**完了条件:** 全host testと`cargo build --release`が通り、下記の実機確認結果を人間から受け取り、
失敗を修正した後にStage 1〜6を完了へ更新する。

## 実機確認依頼

エージェントは`cargo run --release`、`espflash`、書き込み、serial取得を実行しない。実装完了時に
人間へ通常のrelease buildを書き込み、CardKB／Tab5 KeyboardまたはUSB keyboard、touchまたは
USB mouseを使って確認してもらう。LAN画像fixtureを使う場合はPCで次を起動する。

```sh
python3 tools/browser_fixture_server.py
```

Tab5のConsoleから組み込み`http://built-in/forms`を開く。成功・遅延・破損画像を含むbuttonは、
fixture serverを起動したPCのIPを使って`http://<PCのIP>:8080/forms/inline-controls.html`を開く。

| 確認 | 期待 | 失敗時の症状 |
| --- | --- | --- |
| 通常inline | label、control、後続textが収まる限り同じ行 | controlごとの独立行、重なり、不自然な空白 |
| 折返し | 入らないcontrolは直前で次行へ移る | boxの切断、右端越え、無限layout |
| focus・編集 | Tab順が左→右→下で、枠・caret・選択が正しい矩形だけ更新 | 順序逆転、本文消失、残像、caretずれ |
| select | popupがinline boxを基準に開き、選択後も周囲が動かない | 旧位置に表示、画面外、本文の再配置 |
| button style | plain、bold、italic、codeと入れ子が区別できる | style消失、閉じtag後もstyleが残る、clip漏れ |
| button画像 | 読込前後ともbutton内に収まり、画像上のtapでbuttonが1回動く | 巨大化、二重表示、画像だけfocus、二重送信 |
| 画像失敗 | alt／失敗表示がbutton内に留まり、他の操作を継続 | page全体失敗、枠外描画、停止 |
| table | cell内でもinlineになり、枠・隣接cell・rowspanと重ならない | row高不足、罫線越え、列幅の揺れ |
| scroll・再layout | 遅延画像の前後で読んでいた位置とfocusを維持 | 大きなjump、別controlへfocus移動 |
| 回帰 | GET/POST値、disabled、hidden、label、checkbox/radio、textareaが従来どおり | 値欠落、余計なcontrol、textareaのinline化 |

実機で未確認の段階は「実装済み」であっても「受入済み」や「動作確認済み」と書かない。
回数、表示結果、UART出力は人間から受け取った事実だけを判断記録へ追記する。

## 実機確認結果

2026-09-13に、上記fixtureと確認表について、通常本文とtable cellのinline control、focus・編集、
select、buttonの装飾文字と成功・遅延・破損画像、scroll・再layout、既存form操作まで実機確認済みとの
報告を受けた。その後に追加した「通常の独立画像を挟むtable cellで、画像の前後のtextとcontrolを
それぞれinline配置する」差分も、同日に実機確認済みとの報告を受け、全Stageを完了とした。

## README.md

README.mdは変更しない。この機能の技術的な配置規則、対応子要素、失敗時挙動は本書と、実装後の
`BROWSER.md`へ置く。実装によってREADME.mdと不一致が生じた場合も、明示的な依頼がない限り
README.mdを編集せず、最終報告で該当箇所を知らせる。
