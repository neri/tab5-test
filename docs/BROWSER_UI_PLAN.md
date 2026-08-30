# ブラウザUI改修計画

> 索引: [`../DESIGN.md`](../DESIGN.md)
> この文書は作業計画です。現在の実装仕様は[`BROWSER.md`](BROWSER.md)と
> コードを優先してください。

## 状態: **完了**（Stage 0〜10、実機確認済み）

| Stage | 内容 | 状態 |
| --- | --- | --- |
| 0 | 現状確認と方針の固定（この文書） | 完了 |
| 1 | 邪魔な表示とログを削る | 完了 |
| 2 | エラーページのアドレスを実アドレスにする | 完了 |
| 3 | HTTPステータスエラーはサーバの本文を表示する | 完了 |
| 4 | 進む履歴・再読込・中止の動作とキー割り当て | 完了 |
| 5 | toolbarの再配置（ボタン、セキュリティアイコン、全消去） | 完了 |
| 6 | Shift_JISの復号 | 完了（ホスト試験済み） |
| 7 | 現状文書の更新と実機受入 | 完了（実機受入済み） |
| 8 | `text/plain`ほかの`text/*`の表示 | 完了（実機確認済み） |
| 9 | `file:`スキーム | 実装完了、実機確認済み |
| 10 | 本文の余白8 px、Wi-Fiインジケーター | 完了（実機確認済み） |

現在の実装仕様は[`BROWSER.md`](BROWSER.md)を参照してください。

Stageは上から順に依存します。1〜3は`src/app/browser.rs`と`src/app/fetch.rs`の
局所的な変更で、toolbarのレイアウトに触れません。4は動作だけを足し、5がその
動作にボタンを与えます。6は独立していて、他とぶつからないので最後でも先でも
構いませんが、`bt`の巡回で確認したいので5の後に置いてあります。

## 依頼された8項目

| # | 依頼 | Stage |
| --- | --- | --- |
| 1 | 戻るボタン。可能なら進む・リロード・中止も | 4、5 |
| 2 | 左上のHTTPSステータス表記が長すぎる。アイコンにできないか | 5 |
| 3 | URL入力欄に全消去ボタン（入力時のみアクティブ） | 5 |
| 4 | 右上の「N links N lines」を削る | 1 |
| 5 | ページ遷移時の`BROWSER page`ログを削る | 1 |
| 6 | エラーページのアドレスが`http://built-in/error`になっている | 2 |
| 7 | HTTPステータスエラーはサーバの本文を表示してほしい | 3 |
| 8 | 対応コストが軽ければShift_JISも | 6 |

8だけは「軽ければ」という条件付きの依頼なので、下の「Shift_JISの費用」で
見積もりを先に出します。結論は**やる**です。

## 設計判断

### 1. toolbarの再配置

現在のtoolbarは左からバッジ（14セル＝112 px固定）、アドレス欄、右端に
リンク数と行数（26セル＝208 px確保）です。依頼2と4はどちらも
「アドレス欄以外が場所を取りすぎている」という同じ問題の別の面で、
依頼1と3はそこへ更に物を置けと言っています。先に場所を作ります。

削るもの: リンク数・行数（208 px）、バッジの文字（112 px → アイコン24 px）。
足すもの: ボタン3つ（132 px）、アドレス欄内の全消去ボタン（32 px、欄の内側）。

差し引きでアドレス欄は115セルから135セルへ増えます。**「削ってから足す」順に
なっているのがこの改修が成立する理由**で、逆順だとアドレス欄が短くなります。

TOOLBAR_HEIGHTは40 pxのままにします。48 pxにすればタップ目標が1割広がりますが、
viewportとstatus行の定数、`content_bottom`、`painted_bottom`の初期値まで
連動して動くので、見返りに対して触る範囲が広すぎます。
（**実機で押し比べた結果48 pxにしました**。下の判断記録を参照。）

当初の配置（40 px版）。最終的な数値は[`BROWSER.md`](BROWSER.md)にあります。

| 部位 | x | 幅 | 備考 |
| --- | ---: | ---: | --- |
| 戻る | 12 | 44 | 履歴が空なら灰色 |
| 進む | 56 | 44 | 進む履歴が空なら灰色 |
| 再読込／中止 | 100 | 44 | 読み込み中は中止に変わる |
| （空き） | 144 | 12 | |
| セキュリティアイコン | 156 | 24 | 押すと文言をstatus行に出す |
| アドレス欄 | 188 | 1080 | 右端は`WIDTH - MARGIN` = 1268 |
| 全消去 | 1236 | 32 | 欄の内側。編集中だけ |

当たり判定はどのボタンもtoolbarの高さ全体（y=0..40）を使います。見えている
矩形より広い方が押しやすく、この帯には他に押すものがないためです。

ボタンの絵はフォントのglyphを2倍角（16×32）で描きます。専用のビットマップも
`fill_rect`による手描きも要りません——`←`（U+2190）、`→`（U+2192）、
`↻`（U+21BB）、`×`（U+00D7）はいずれもフォントに収録済みで、半角（advance 8）
です。収録は`font/data/tab5font16.txt`の「requested but absent: 0」で確認済み
です。ただし`✕`（U+2715）や`⚠`（U+26A0）は**入っていません**——`advance`が
16を返す＝未収録なので、これらを使うと中空の四角が描かれます。

### 2. セキュリティ表示はアイコンにする（依頼2）

文字のバッジ（`INSECURE HTTP`／`TLS UNVERIFIED`／`TLS PINNED`／`CONNECTING`）を
24 pxのアイコンへ置き換えます。フォントに南京錠のglyphはありません
（U+1F512はBMP外でUnifont-JPも持ちません）ので、ここだけ`fill_rect`で描きます。
本体の矩形とつるの3辺で、20行程度です。

| 状態 | 絵 | 色 |
| --- | --- | --- |
| 平文HTTP | 開いた南京錠 | 赤 |
| 未認証TLS | 開いた南京錠 | 赤 |
| pin一致 | 閉じた南京錠 | 緑 |
| 接続中 | 錠の輪郭のみ | 地の文字色 |

**平文と未認証TLSに同じ絵を割り当てます。** [`BROWSER.md`](BROWSER.md)が
現在のバッジについて書いているのは「前2つが同じ赤なのは、読み手にとって意味が
同じだからです」で、絵を分けるとその主張と食い違います。読み手にとっての違いは
`http://`か`https://`かで、それはアドレス欄に出ています。アイコンが答えるのは
「この接続は何を証明したか」で、どちらも答えは「何も」です。

失われる情報は文言そのものです。**アイコンを押すとstatus行に文言を出す**ことで
戻します。

| 状態 | status行 |
| --- | --- |
| 平文HTTP | `INSECURE HTTP: plaintext; anyone carrying it can read it` |
| 未認証TLS | `TLS UNVERIFIED: encrypted, but nobody checked who answered` |
| pin一致 | `TLS PINNED: the peer's key matches a pin built into this firmware` |
| 接続中 | `CONNECTING: nothing has been proved yet` |

読み込み中のアイコンが「読み込み中の接続」を指す規則（前のページのものを
出さない）は変えません。

### 3. アドレス欄の全消去（依頼3）

編集中だけ、欄の内側右端に`×`を描きます。押すと`text`を空にし、caretを0にし、
**欄は開いたままにします**。編集していないときは描かず、当たり判定も持ちません
（依頼の「入力時のみアクティブ」）。

編集していないときに欄の右端を押した場合は、これまでどおり編集開始です。
全消去は「開く」と「消す」が同じ場所で起きないよう、欄が開いている間だけ
その場所の意味を変えます。

### 4. 「N links N lines」を削る（依頼4）

そのまま削ります。読み込み中に同じ場所へ出ている受信KiBは残しますが、
**status行へ移します**（`loading; Escape stops` →
`loading 12 KiB; Escape or the stop button stops`）。
アドレス欄が右端まで伸びるので、toolbarの右にはもう場所がありません。
`Loading::shown_kib`が立てるdirtyの対象がtoolbarからstatusへ変わります——
帯が小さくなるので再描画は安くなります。

### 5. `BROWSER page`ログを削る（依頼5）

`log_page`とその呼び出しを削ります。同じ統計は`hs <url> p`が出すので、
数値が要るときはそちらです。`BROWSER: slowest viewport repaint so far`は
残します——毎回は出ず、最悪値を更新したときだけで、
[`APPS.md`](APPS.md)が判断根拠として参照しています。

### 6. エラーページのアドレス（依頼6）

現在`error_page`は`Url::parse("http://built-in/error")`を文書のアドレスにして
いるので、toolbarに存在しないアドレスが出ます。**失敗したURLをそのまま
文書のアドレスにします**（`Parser::new(url.clone())`）。

これに伴い「今エラーページを出しているか」の判定をパスの一致から
`Page`のフラグへ変えます（`showing_error`）。判定が要るのは、エラーが
エラーを置き換えるときに履歴を積まないためです。

副作用が2つあり、どちらも良い方向です。

- アドレス欄を開くと失敗したアドレスが入るので、直して打ち直せます
- Stage 4の再読込がそのまま「もう一度試す」になります

エラーページの中身にある`Home`リンクは絶対URLなので、基準アドレスが変わっても
行き先は変わりません。

### 7. HTTPステータスエラーは本文を表示する（依頼7）

現在`fetch::head_ready`は2xx以外を`REFUSED`にして、本文を読まずに接続を
捨てています。`404`や`500`のページに書いてある「なぜ駄目か」が読めません。

変更後の判定順:

```text
redirect            → 追跡（変更なし）
status が無い       → not-http（変更なし）
HTMLではない        → not-html（statusを添える。変更なし）
2xx以外             → 本文を読む。status を覚えておく   ← ここが変更
2xx                 → 本文を読む（変更なし）
```

つまり**HTMLでありさえすれば本文を組み立てます**。通信エラー・TLSエラー・
上限超過・DNSは表示すべき本文が存在しないので、これまでどおりビューア自身の
エラーページです（依頼のとおり）。

本文が空だった場合（`Content-Length: 0`、または文書にテキストが1文字も無い）は
ビューア自身のエラーページへ落とします。真っ白なページと「404」は読み手には
区別が付かないためです。

`Fetch`に`status()`を足し、`Outcome::Page`と一緒に読めるようにします。
ビューアはそれを受け取ったとき、ページを普通に表示した上でstatus行に
`the server answered 404`を出します。アドレス欄と履歴の扱いは成功と同じです。

`bt`（`app::browsertest::visit`）は`Outcome::Page`にstatusが付いていたら
名前を`status`として`judge`へ渡します。`judge`は`("status", Some(404))`を
`status-404`に組み立てるので、**fixture serverのmanifestの期待値
（`error:status-404`、`error:status-500`）は変えずに済みます**。ここを
壊さないことが、この変更で一番気を付ける点です。

### 8. 進む・再読込・中止（依頼1）

`Viewer`に`forward: Vec<HistoryEntry>`を足します。

| 操作 | history | forward |
| --- | --- | --- |
| 新しい遷移 | 今のページを積む | **空にする** |
| 戻る | popして行く | 今のページを積む |
| 進む | 今のページを積む | popして行く |
| 再読込 | 触らない | 触らない |

`forward`も`MAX_HISTORY`で頭打ちにします。持つのはURLとscroll位置だけで
文書は持たないので、8件でも数KiBです（現在の履歴と同じ理由）。

再読込は今のアドレスへ`restore = first_line`で遷移し直します（実装では
`Direction::Reload`。下の判断記録を参照）。scroll位置を保つのは、読んでいる途中で更新する使い方が
普通だからです。組み込みページは組み直しになるだけです。

中止は`Escape`と同じ経路（`Action::Cancel`）です。ボタンは再読込と兼用で、
読み込み中は`×`になります。

キー割り当て:

| キー | 動作 |
| --- | --- |
| `Backspace`、`[` | 戻る |
| `]` | 進む |
| `r`、`F5`、`Ctrl+R` | 再読込 |
| `Escape` | 中止（変更なし） |

`[`と`]`はpagerの慣習です。`r`を単独キーに割り当てられるのは、`q`と同じ理由で
アドレス欄以外に文字入力が無いからで、CardKB（Ctrlキーが無い）で再読込できる
唯一の手段でもあります。`Ctrl`付きと`F5`はHIDキーボードのための別名です。

### 9. Shift_JISの費用（依頼8）

**やります。**内訳:

| 項目 | 費用 |
| --- | ---: |
| 変換表（flash） | 22,576 byte（実測） |
| `browser/src/encoding.rs` | 250行程度 |
| 生成スクリプト | 80行程度 |
| `Parser`への接続と`fetch`からのcharset受け渡し | 40行程度 |

flashは現在1.7 MiB／4 MiBなので22 KiBは問題になりません（フォント本体が
349 KiB）。

フォントはJIS X 0213 plane 1を収録しているので、**表示できる文字は既に
揃っています**。足りないのはバイト列から符号位置への対応表だけです。

#### 変換表

`browser/data/shiftjis.bin`（生成物、リポジトリに入れる）を
`tools/encoding/generate_shiftjis.py`が書きます。フォントと同じやり方で、
生成物を入れて生成器も入れ、ホスト側のテストがCRC-32と代表点を照合します。
CPythonの`cp932` codecから作ります——Web上で`Shift_JIS`と名乗る文書が実際に
使っているのはWindows-31Jで、NEC／IBM拡張を落とすと人名や丸数字が消えます。

表は`[u16; 120 * 94]`（区1..120、点1..94）で、0が未対応です。区を120まで
取るのはIBM拡張が区95以降へ乗るためで、この形なら拡張が表の中に自然に入り、
別表が要りません。

SJISのバイトから区点:

```text
lead 0x81..=0x9F → c1 = lead - 0x81
lead 0xE0..=0xFC → c1 = lead - 0xC1
trail 0x40..=0x7E → c2 = trail - 0x40
trail 0x80..=0xFC → c2 = trail - 0x41
区 = c1 * 2 + (c2 < 94 ? 1 : 2)      点 = c2 % 94 + 1
```

`lead = 0xFC`で区は120、これが表の高さの根拠です。

表を引かないもの:

- `0x00..=0x7F`: そのままASCII（`0x5C`は円記号ではなく`\`。WHATWGの規定に従う）
- `0xA1..=0xDF`: 半角カナ。`U+FF61 + (byte - 0xA1)`で計算する
- `0x80`、`0xA0`、`0xFD..=0xFF`、未対応の区点: U+FFFD

trailが不正だったときは、U+FFFDを出した上で**そのバイトを先頭バイトとして
読み直します**（WHATWGと同じ）。壊れた1バイトで残り全部がずれるのを防ぐためです。

EUC-JPは同じ表を別の算術で引くだけなので、後から欲しくなったら20行です。
今回はやりません（依頼はShift_JISだけ）。

#### どこで復号するか

`browser/src/encoding.rs`に`Decoder`を置き、`Parser::feed`の内側に入れます。
`Parser`より外——たとえば`fetch`のsink——に置くと、ホストのfixtureテストと
`hs p`が別経路になります。

```text
バイト列 → Decoder → UTF-8 → Tokenizer → Builder → Document
```

符号化の決め方は上から順に:

1. `Content-Type`の`charset`（`fetch::head_ready`が`Parser::declare_charset`で渡す）
2. BOM（`EF BB BF`）→ UTF-8。読み捨てる
3. 先頭1 KiB内の`<meta charset=...>`または
   `<meta http-equiv=content-type content="...charset=...">`
4. どれも無ければUTF-8

1が無いときは先頭1 KiBを溜めてから決めます。溜めるのは`<meta>`が本文より
前に来るからで、決まった時点でまとめて流します。`charset`をバイト列として
探すのではなく`<meta`から`>`までの範囲だけを見るので、本文中の`charset`という
語に反応しません。

対応するラベル（小文字化済みで比較）:

| ラベル | 符号化 |
| --- | --- |
| `shift_jis`、`shift-jis`、`sjis`、`x-sjis`、`windows-31j`、`cp932`、`ms_kanji` | Shift_JIS |
| `utf-8`、`utf8`、その他すべて | UTF-8 |

未知のラベルをUTF-8として扱うのは現在の動作と同じで、ページが表示されなく
なるより文字化けする方が読み手にとってましだからです。

#### 上限との関係

Shift_JISはほとんどの全角文字で2 byteから3 byteへ増えます。上限のうち
`MAX_DECODED_HTML_BYTES`（2 MiB）は**回線から受け取ったbyte数**への上限のまま
（`Transaction`へ渡している値）なので、tokenizerが受け取る量はその1.5倍まで
あり得ます。テキスト上限（1 MiB）は復号後を数えるので変わりません。
`Parser::owned_bytes`にsniff bufferと出力bufferを足して、メモリのピーク計測が
欠けないようにします。

## 段階分け

### Stage 1: 邪魔な表示とログを削る

- toolbarの`Summary`からリンク数・行数を削る
- 受信KiBの表示をstatus行へ移し、dirtyの対象をtoolbarからstatusへ変える
- `log_page`とその呼び出しを削る
- `SUMMARY_CELLS`と`ADDRESS_RIGHT`を`WIDTH - MARGIN`へ

**確認**: 実機で`browser`を開き、右上に何も出ないこと。読み込み中に
status行へKiBが出て増えること。UARTにページごとのログが出ないこと。

### Stage 2: エラーページのアドレス

- `error_page`が失敗したURLを文書のアドレスにする
- `Page`に`error: bool`を足し、`showing_error`をそれで判定する
- `ERROR_PATH`定数を削る

**確認**: 到達できないアドレスを開き、toolbarにそのアドレスが出ること。
`Backspace`で元のページへ戻れること。エラーからエラーへ移っても履歴が
積み上がらないこと。

### Stage 3: HTTPステータスエラーの本文

- `head_ready`の判定順を入れ替え、2xx以外でもHTMLなら`Parser`を作る
- `Fetch`に`status: Option<u16>`と`status()`を足す
- 本文が空のときはビューアのエラーページへ落とす
- ビューアはstatus行に`the server answered 404`を出す
- `browsertest::visit`の`Page`アームでstatusを`judge`へ渡す

**確認**: `bt <fixture>`で`/status/404`と`/status/500`が
`status-404`／`status-500`のまま通ること。ビューアで
`/status/404`を開くとサーバのページが読めること。

### Stage 4: 進む・再読込・中止の動作

- `Viewer::forward`と`go_forward`、`reload`
- 新しい遷移で`forward`を空にする
- キー割り当て（`[`、`]`、`r`、`F5`、`Ctrl+R`）

**確認**: 3ページ辿って戻る・進むが往復すること。新しい遷移の後に
進むが無効になること。読み込み中の再読込キーが今の取得を捨てて
やり直すこと。

### Stage 5: toolbarの再配置

- ボタン3つの描画（2倍角glyph）と当たり判定、無効時の灰色
- 南京錠アイコン3種と、押したときのstatus行の文言
- アドレス欄の全消去ボタン（編集中のみ）
- 定数の入れ替え（`BADGE_CELLS`→アイコン幅、`ADDRESS_LEFT`）

**確認**: 4種のアイコンが出ること（平文、未認証TLS、pin、接続中）。
指で3つのボタンが押し分けられること。編集中だけ`×`が出て、押すと
欄が空になり開いたままであること。

### Stage 6: Shift_JIS

- `tools/encoding/generate_shiftjis.py`と`browser/data/shiftjis.bin`
- `browser/src/encoding.rs`（表の読み出し、`Decoder`、ラベル判定）
- `Parser`への組み込みと`declare_charset`
- `fetch::head_ready`からcharsetを渡す
- fixture serverに`/encoding/shift-jis`、`/encoding/shift-jis-meta`、
  `/encoding/shift-jis-broken`、`/encoding/utf8-bom`
- ホストのテスト（下記）

**確認**: `bt <fixture>`で新しい端点が`ok`になること。ビューアで
Shift_JISのページを開いて文字化けしないこと。半角カナが半角のまま出ること。

### Stage 7: 文書と実機受入

- [`BROWSER.md`](BROWSER.md): 画面図、バッジ表→アイコン表、操作表、
  エラーの節、上限の注、符号化の節
- [`APPS.md`](APPS.md): toolbarの構成と当たり判定
- [`NETWORK.md`](NETWORK.md): `charset`が使われるようになったこと
- [`DESIGN.md`](../DESIGN.md): この文書を作業計画リストへ（Stage 0で実施済み）
- `README.md`は**人間が管理**するので触りません。古くなる記述は最終報告で
  指摘します

## 検査

すべて実施済みです（`mise run test`が196＋28件通過）。

### ホスト（`mise run test`）

| 対象 | 内容 |
| --- | --- |
| `shiftjis.bin` | CRC-32、要素数、代表点（`0x8140`→U+3000、`0x82A0`→U+3042、`0x9AD9`相当の人名字、IBM拡張の1点） |
| `Decoder` | 半角カナ、ASCII、2 byte文字、未対応区点→U+FFFD |
| `Decoder`のchunk境界 | 同じ入力を1..N byteの全ての切り方で流し、結果が一致すること |
| ラベル判定 | 上の表の全ラベルと、未知ラベル |
| meta sniff | header優先、meta検出、1 KiB超過、BOM |
| fixture | `--dump`したShift_JISのfixtureを解析してテキストが一致すること |

chunk境界の全通りテストは必須です。`Decoder`は2 byte文字の途中で切れた入力を
またぐ唯一の状態を持つので、そこが壊れると「たまに1文字化ける」という形で
出ます。

`--dump`はバイト列をそのまま書き出すので、Shift_JISのfixtureは
`.sjis.html`のように名前で符号化が分かるようにします。

組み込みのShift_JISページ（ネットワーク無しで見られるもの）は**足していません**。
組み込みページの本文はRustの`&'static str`で、Shift_JISにするにはescape
だらけのbyte列を書くことになる一方、fixture serverの`/encoding/`以下を
ビューアで直接開けば同じことが確かめられるためです。

### 実機（**受入済み**）

1〜4は確認済みです（`bt`の全端点、10巡回でのソケット・ヒープ、Shift_JISの
2ページ、`/status/404`）。5は`tall-toolbar` featureで両方をbuildして押し比べ、
**48 pxに決定**（featureは削除済み）。6〜7は画面から数値が見えないと確かめ
ようがなかったので`i`キーを足しました（下の判断記録）。

| # | 内容 |
| --- | --- |
| 1 | 戻る・進むを10往復してヒープが増えないこと |
| 2 | 読み込み中に中止ボタン→再読込ボタンでソケットが戻ること（`hs`の`sockets`表示で確認） |
| 3 | `bt <fixture>`が全端点で期待どおり（TLSとplaintextの両方） |
| 4 | `bt <fixture> 10`でソケットもヒープも増えないこと |
| 5 | `browser <fixture>/encoding/shift-jis` と `…/shift-jis-meta` が化けずに読め、半角カナが半角のままであること |
| — | 指でボタンが押し分けられること → **48 pxに決定** |
| 6 | `browser <fixture>/status/404` がサーバのページを表示し、status行に`the server answered 404`が出ること |
| 7 | エラーページのtoolbarに失敗したアドレスが出て、`r`で再試行できること |
| 8 | 南京錠を押すと文言が出ること（平文と、fixtureのTLS listenerで未認証TLS） |
| 9 | 編集中だけ`×`が出て、押すと欄が空になり開いたままであること |
| 10 | viewport再描画の最悪値が1フレーム前後のままであること |

2を挙げてあるのは、中止ボタンが`Escape`と同じ経路を通ることを目で確かめる
ためです。別経路を作るとソケットの返し忘れがそこにだけ残ります。

## 中止条件

- ~~toolbarのボタンが指で押し分けられない（44 pxでは足りない）と分かったら、
  TOOLBAR_HEIGHTを48 pxへ上げてviewportの定数を追従させる。それでも駄目なら
  ボタンを戻る1つに減らし、進む・再読込はキーだけにする~~
  → 48 pxへ上げて解決。ボタンは3つのまま（下の判断記録）
- Shift_JISの表がflashを圧迫する、または`Decoder`のchunk境界テストが
  素直に通らない場合、Stage 6を落として他を出す。6は独立しているので
  他のStageを巻き込まない

## 追加: `text/plain`と`file:`（Stage 8、9）

| Stage | 内容 | 状態 |
| --- | --- | --- |
| 8 | `text/plain`ほかの`text/*`を表示する | 完了（ホスト試験済み、実機確認済み） |
| 9 | `file:`スキーム | 完了（ホスト試験済み、実機確認済み） |

どちらも利用者からの追加依頼で、9は8の上に乗っています——ファイルシステムに
`Content-Type`は無いので、`.html`以外を読む手段が先に要りました。

### Stage 8の判断: plainはtokenizerの旗にした

`<pre>`で囲んで`<`と`&`をescapeしながら流す案（エラーページと同じ手口）と、
builderへ直接テキストを送る案がありました。採ったのは**tokenizerに旗を立てる**
案です。

tokenizerがtag認識**以外に**やっていることが、plain textにもそのまま必要
だからです——chunk境界をまたぐUTF-8の持ち越し、不正byteのU+FFFD置換、入力
上限の計数、sinkへの分割送出。builderへ直接送る案はこの4つを書き直すことに
なり、escapeする案は入力を1.2倍にして「escapeが漏れたら表示が変わる」経路を
足すことになります。旗は`step`の先頭3行です。

`Decoder`にも同じ旗を立てました。plain textに`<meta>`は無いので、先頭1 KiBを
溜める理由もありません（BOMの3 byteだけ見ます）。

### Stage 9の判断: `Pending`を2種類にした

`file:`は名前解決もソケットもredirectもstatusも持たないので`Fetch`には
入りません。しかし**読み手から見れば同じ行為**で、進め方・中止・描画・完了は
同じコードであるべきです。

そこでビューアの`Pending`を`Source::Network(Fetch)`と`Source::Local(LocalRead)`
の2択にし、`landed`／`security`／`status`／`received`／`peak_owned`を
`Pending`のメソッドにしました。ループ本体は分岐が1箇所（step）だけです。
`close_pending`が1つあるので、ソケットとファイルハンドルの返し忘れが片方に
だけ残ることもありません。

### Stage 9の判断: 拡張子で決め、`.html`以外はテキスト

ファイルシステムに`Content-Type`はありません。先頭を覗いて推測する案は、
推測のあいだ溜めることになります。

`.html`／`.htm`だけmarkup、**それ以外はすべてplain text**にしました。この
向きが安全なのは、テキストをmarkupとして読むと山括弧の中身が黙って消えるのに
対し、markupをテキストとして読んでも「markupに見える」だけだからです。

### Stage 9で発覚: 一覧ページのベースが1階層ずれていた（実機で発覚）

利用者の報告「ファイル一覧から個別のファイルを選択するときのURLのベース
ディレクトリがずれてる」。

`file:///tmp`を開くと一覧は正しく出ますが、その上の`notes.txt`というリンクが
`file:///tmp/notes.txt`ではなく`file:///notes.txt`になっていました。相対参照は
基準URLの**最後の`/`まで**に対して解決される（RFC 3986）ので、末尾に`/`の無い
アドレスでは最後の区切りが`/tmp`の前になり、`tmp`が兄弟として捨てられます。

一覧ページは相対リンクの塊なので、これは1つのリンクの問題ではなく**全部**の
問題でした。

`Url::as_directory`（末尾に`/`を足した同じURL、queryとfragmentは落とす）を
browserクレートへ足し、ディレクトリと分かった時点で**ページを組む前に**
変換します。見出し・`..`・一覧・表示アドレスがすべて同じ値から出るので、
ページが名乗るディレクトリとリンク先が食い違う形も同時に無くなります。

ホスト側のテストに、直す前の挙動（`file:///tmp` + `notes.txt` →
`file:///notes.txt`）をそのまま書いてあります。バグの形を残しておかないと、
「なぜ`/`を足しているのか」が次に読む人に分かりません。

### Stage 9の判断: redirectでは`file:`を開かない

serverからの`Location: file:///...`は`file-redirect`で拒否します。アドレスを
打つのもページ上のリンクを選ぶのもアドレスが見えた上での読み手の選択ですが、
これはserverが代わりに選ぶことです。httpsからhttpへの降格を拒否するのと
同じ規則で、同じ場所（`redirect_to`）にあります。

`Fetch::start`と`Fetch::connect`も非ネットワークschemeを断るので、取得の
状態機械に`file:`が入る道は2つとも塞がっています。

### Stage 9で分かったこと: `Scheme`を増やすと決定点が全部出る

`Scheme`に`File`を足したら、`match`が網羅でないというエラーが4箇所出ました
——`fetch::connect`のtransport選択、`fetch::note_security`、
`browsertest::security_for`、そして`url.rs`のテスト。**scheme（＝何をするか）を
決めている場所がちょうどこの4つ**だということで、どれも黙って既定に落ちる
書き方ではなかったのが効いています。`_ =>`があったら、`file:`はport 0へ
接続しようとしていました。

## 追加: 本文の余白とWi-Fiインジケーター（Stage 10）

利用者からの追加依頼2件。どちらも実装済みで、実機確認済みです。

### toolbarと本文の間に8 px

`VIEWPORT_TOP = TOOLBAR_HEIGHT + CONTENT_GAP`（8 px）にしました。無いと本文の
1行目がアドレス欄の下端に接します——見出しはglyphの箱が行の最上段から始まる
ので、実際に触れます。

**余白はtoolbarの再描画が塗ります。**viewportの再描画は`VIEWPORT_TOP`から下
だけを触るので、余白をviewport側の責任にすると誰も塗らない帯ができます。
起動時の全画面クリアで一度白くなり、その後は永久に正しく見えるので、間違いが
何ヶ月も気付かれない種類のものです。toolbarのflushも`VIEWPORT_TOP`までに
広げてあります。

viewportは640 pxから632 pxになりました。

### Wi-Fiインジケーター

toolbarの右端に3本の棒。強度ではなく**接続がどこまで進んだか**です。

RSSIにしなかったのは、取るのがC6へのRPC往復である以前に、この画面で訊かれて
いる質問に答えないからです。ページが出ないときに知りたいのは「そもそも
ネットワークに乗っているか」で、-67 dBmはその答えになりません。

**3本目の条件をビューアの取得条件（`addressed_network`）と一致させました。**
`State::Online`だけを見て満杯にすると、`no-network`で失敗するページの上に
3本が立ちます。アイコンが画面の他の部分と食い違うのは、アイコンが無いより
悪いことです。

押すと文言がstatus行に出ます——南京錠と同じ取り決めで、同じ理由（絵が場所を
節約し、言葉は訊かれたときに出す）です。

`Action`に`Wifi`を足したので、touchとmouseの2経路が同じ`answer_click`を通る
ようにしました。それまで`matches!(..., Action::Cancel)`が2箇所にあり、
`Action`が増えるたびに片方だけ対応する余地がありました。

## 判断記録

### Stage 3: `REFUSED`は「本文が空だった」ときの失敗になった

2xx以外でも本文を読むようにすると、`REFUSED`（name `status`）が出る場面が
変わります。以前は「2xx以外だった」、いまは「2xx以外で、しかも見せられる本文が
無かった」です。detailの文も`Its own page for this is not shown`から
`It sent no page to go with the refusal`へ書き換えました。

manifestの期待値は変えずに済みました。`judge`が`("status", Some(404))`を
`status-404`へ組み立てる経路がもともとあり、`visit`の`Page`アームから
`fetch.status()`を同じ経路へ流せば名前が一致するためです。

### Stage 4: 「履歴に積むか」の真偽値では足りなかった

`Navigation`は`push_history: bool`を持っていました。進む履歴ができると、
`back`と`forward`はどちらも「積まない」側でありながら**もう一方のstackに対して
逆のこと**をし、`reload`はどちらにも触りません。真偽値1つでは表せないので
`Direction`（`Fresh`／`Back`／`Forward`／`Reload`）にし、4つの振る舞いを
`settle_history`1箇所へ集めました。5つ目の振る舞いが別の場所に生えるのが、
進むボタンが「誰も訪れていないページ」へ行くようになる経路です。

### Stage 5: アイコンはフォントのglyphで足り、南京錠だけが足りなかった

`←`（U+2190）、`→`（U+2192）、`↻`（U+21BB）、`×`（U+00D7）は収録済みで、
2倍角（16×32）で描けば44×40のボタンに収まります。専用bitmapは要りません。

**似た形の記号が全部あるわけではありません。**`✕`（U+2715）、`⚠`（U+26A0）、
`✖`（U+2716）は範囲外で、`tab5_font::advance`が16を返す（＝未収録の既定）ので
中空の枠になります。使う前に`advance`が8を返すことを確かめるのが確認方法です。

南京錠はU+1F512がBMP外でUnifont-JPが持たないので、`fill_rect`で描いています。
本体1つとつる3つ、開いた錠は同じつるを左側で外した形です。

### Stage 5: アドレス欄の箱を2倍角glyphの高さにした

全消去の`×`を2倍角で描くと32 px高になり、24 px高だった編集中の白い箱から
上下にはみ出しました。箱を`CELL_HEIGHT * 2`（32 px）にすると、既存の
`CHROME_TEXT_Y`がそのまま上下8 pxずつの中央になります。

### Stage 6: 変換表はcp932から作る

`shift_jis` codecで作ると、fixtureに入れた`髙`・`﨑`・`①`が
`UnicodeEncodeError`になります。NEC・IBM拡張はplain JIS X 0208に無いためで、
Web上で`Shift_JIS`と名乗る文書が実際に使っているのはWindows-31Jです。
生成器もfixture serverも`cp932`に統一しました。

### Stage 6: 復号器の再帰でmonomorphizationが爆発した

`Decoder::feed<F>`が、sniffの終わりで残りのchunkを自分自身へ渡す形にした
ところ、`&mut F`が1段ずつ増えて
`feed::<&mut &mut &mut ...>`でrustcが再帰上限に達しました。内部を
`&mut dyn FnMut(&[u8]) -> Result<(), Error>`（`type Sink`）に統一して解決。
公開APIだけがgenericです。

### Stage 6: 不正なtrail byteを読み直すのはASCIIのときだけ

最初は「trailでなかったbyteは必ず先頭バイトとして読み直す」にしていました。
`0x84 0xBF`（未対応の区点）が置換文字1つで済むはずのところ、`0xBF`が半角カナ
として復活して2文字になります。WHATWGの規則どおり**ASCIIのときだけ**読み直す
ようにしました。`>`や`<`はmarkupとして残さないといけない一方、もう1つの高位
byteは同じ壊れ方の一部です。

### Stage 6: 1 byteだけ壊したfixtureは壊れていなかった

`/encoding/shift-jis-broken`の最初の版は`表`の先頭byteを`0x93`へ置き換えた
ものでしたが、`0x935C`は`貼`という**まっとうな別の文字**になり、置換文字が
1つも出ませんでした。Shift_JISは密なので、byte 1つの化けはたいてい「壊れた
ページ」ではなく「違うページ」になります。2 byteの文字を「lead byte＋ASCII」
（`\x93.`）へ置き換える形に変え、以降のbyteの並びを保ったまま
trailだけを欠けさせています。

### Stage 6: `hs p`のsinkをpollごとに作り直した

`hs <url> p`は`Parser`をloopの外で作り、sinkがそれを可変借用したまま
loopを回していました。`HeadReady`のアームから同じ`Parser`へ`charset`を渡す
必要ができたので、sinkをpollごとに作る形へ変えました。これで`hs p`が
ビューアとまったく同じ復号を通ります。

### 完了後: barは48 pxになった（実機で判断）

「48ピクセル版も試して判断したい」。実行時に切り替えられれば同じ画面で
比べられますが、`TOOLBAR_HEIGHT`から導かれる定数は79箇所で使われていて、
そのうちscrollの計算（`lines_per_screen`、`last_top_line`、`content_bottom`）は
壊すと気付きにくい部類です。一度きりの判断のために、いちばん描画の多い画面へ
恒久的な間接参照と実行時分岐を入れる取引になります。

そこで`tall-toolbar` cargo featureにして両方をbuildし、実機で押し比べて
もらいました。**結果は48 px**（ボタン52 px幅）。featureは役目を終えたので
削除し、`TOOLBAR_HEIGHT = 48`が唯一の値です。

viewportは648 pxから640 pxになりました。1画面あたり本文でおよそ半行分ですが、
scrollは行単位なので端数は元から出ています。

barの上のものがすべて`TOOLBAR_HEIGHT`から導いてある（`BUTTON_WIDTH`も
`TOOLBAR_HEIGHT + 4`）ことが、この比較を「1つの数字を変えて2回buildする」で
済ませました。導けているかどうかは`const`のassertが見ます——欄がbarより高い、
ボタンの絵がbarより高い、ボタンがアドレス欄を食う、欄が編集できないほど狭い。
featureを消してもassertは残します。次に誰かがこの数字を触るときに要るのは
featureではなくこちらです。

### 完了後: 漏れを見る`i`キーを足した（利用者の指摘）

受入項目6（戻る・進む10往復でヒープが増えない）と7（中止→再読込でソケットが
戻る）は、**画面から確かめる方法がありませんでした**。ヒープもソケット数も
`hs`でしか読めず、`hs`を打つにはブラウザを出る必要があり、出た時点でブラウザ
自身の後始末が走ってしまいます。つまり「ブラウザの中で漏れているか」は
ブラウザの中でしか見られません。

`i`／`F1`でstatus行とUARTへ1行出します。ページ遷移ごとに出していた
`BROWSER page`ログの置き換えでもありますが、キーにしたのは、知りたいものが
値ではなく**読み手が選んだ2つの瞬間の差**だからです。毎回出しても答えには
ならず、ログが読めなくなるだけでした。

`peak`のために`Fetch::peak_owned`をビューアへ戻しています（Stage 1で
`log_page`と一緒に落としていた唯一の有用な数値）。

### 完了後: `cargo fmt`が利用者の作業中コードを整形した（作業中の失敗）

自分が触ったファイルを整えるつもりで`cargo fmt -p tab5-hello-world`を実行し、
**触っていない14ファイルと、作業中だった`src/net/http.rs`・`src/net/tls.rs`の
未整形部分まで書き換えました**（`src/net/pins/generated.rs`という生成物を
含む）。整形前に汚れていなかったファイルは`git checkout`で戻し、2つの
作業中ファイルは整形だけのhunkを手で戻しました。

`cargo fmt`はpackage全体に効きます。作業中の差分が同じpackageにあるときは
`rustfmt <file>`でファイルを名指しすること。

### 完了後: `bt`の2件の失敗はfixture serverの既定が逆だった（実機で発覚）

実機の巡回で`https://<addr>:8445/`と`:8446/`が
`expect=error:tls-pin got=tls-cert`／`got=ok`で失敗しました。

**基板は正しく、期待値が間違っていました。**`tls-cert`（Ed25519の明示拒否）と
`ok`（RSA-PSS）は[`BROWSER.md`](BROWSER.md)の表が「pin無しbuild」の欄に書いて
いる値そのもので、`tools/pins/pins.txt`は空、`tls-fixture-pins`はdefault
featureではないので、`cargo build --release`にpinは1つも入りません。

fixture serverの`BOARD_HAS_FIXTURE_PINS`は既定`True`で、pinを持たないbuildを
`--unpinned-board`で申告する形でした。つまり**普通にbuildして普通に巡回すると
必ず2件失敗し、正常だと分かるには引数を足す必要がある**状態です。既定を`False`に
し、フラグを`--pinned-board`へ入れ替えました。

基板がpinを持つかはサーバから見えません——照合が成功したpinは回線に痕跡を
残しません——ので、これはフラグでしか伝わりません。伝わらないものの既定は、
普通の側に置く必要があります。サーバは起動時にどちらを仮定しているかを1行
出すようにしました。巡回がこの2つのlistenerだけで落ちたら、その行が違って
いるという読み方ができます。

### Stage 1: `Fetch::elapsed_ms`が死んだ

ページごとのUARTログ（`log_page`）を削ったら、所要時間の唯一の読み手が
消えました。`started_ms`ごと削除しています。`bt`は自前で`tick::now_ms`の差を
取っており、`hs`は`Transaction`の統計を使います。
