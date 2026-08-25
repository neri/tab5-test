# ハイパーテキストビューア（browser）

> 索引: [`../DESIGN.md`](../DESIGN.md) ／ 段階分けと実機での判断記録:
> [`WEB_BROWSER_PLAN.md`](WEB_BROWSER_PLAN.md)

`browser`コマンドで開く全画面のビューアです。**Webブラウザではありません。**
平文HTTPで取得したHTMLから文章とリンクを取り出し、画面幅へ折り返して読む
ものだけを実装してあります。CSS・JavaScript・画像・TLSはいずれもありません。

対象を狭く固定してあるのは能力不足の言い訳ではなく設計です。対象外の内容を
推測して実行したり、上限なく保持したりしないことが、1つのページで基板ごと
落ちない条件だからです。

## できることとできないこと

| できる | できない |
| --- | --- |
| `http://`の取得 | `https://`（認識して「未対応」と表示。**httpへ落としません**） |
| HTMLの文章・見出し・リスト・リンク | CSS（`style`属性・`<style>`・外部stylesheet） |
| 相対リンクの解決、履歴8ページ、戻る | JavaScript、DOM API |
| リダイレクト5回まで | 画像のデコード（`img`は`alt`だけ） |
| chunked転送、`Content-Length`、close終端 | form送信、cookie、認証、キャッシュ |
| キーボード・タッチ・USBマウス | 日本語フォント、日本語入力 |
| 読み込み中のキャンセル | IPv6、HTTP/2、HTTP/3、WebSocket |

平文HTTPしか話せないので、**認証情報を送る機能を持ちません**。アドレス欄の
左には常に赤で`INSECURE HTTP`と出ます。条件付きではありません——TLSが無い
以上、この表示が消えてよい状態が存在しないからです。

## 画面

上から固定toolbar（40 px）、viewport、固定status行（32 px）の3帯です。

```text
 y=0    ┌───────────────────────────────────────────────┐
        │ INSECURE HTTP  http://host/page      12 links │  toolbar
 y=40   ├───────────────────────────────────────────────┤
        │ 見出し                                        │
        │                                               │  viewport
        │ 本文。画面幅で折り返し、1行ずつ描く           │
 y=688  ├───────────────────────────────────────────────┤
        │ http://host/選択中リンクの飛び先              │  status
 y=720  └───────────────────────────────────────────────┘
```

- **toolbar**: `INSECURE HTTP`、現在のアドレス（編集中はアドレス欄）、
  リンク数と行数。読み込み中は受信KiBに変わります
- **viewport**: scroll位置と交差する行だけを描きます。文書全体のbitmapは
  作りません
- **status**: 直前の操作への返事（赤）があればそれ、無ければ選択中リンクの
  飛び先

行の高さはglyphの箱（5×7フォントの6×8の枠）に**4分の1の空きを下へ足した**
ものです。足さないと行間が2ピクセル（scale 2で16ピクセルの活字に対して）しか
なく、ページが壁のように見えます。本文で4ピクセル、見出し（3倍角）で6ピクセル
です。空きは活字の大きさに対する割合なので、見出しも本文と同じ詰まり具合に
なります。リンクの下線はこの空きに引くので、`g`や`y`の下に届く descender を
潰しません。

**scrollはpixelではなく行単位です。** viewport最上段は必ず行の先頭に揃い、
下端で入りきらない行は描きません。glyph描画に上下のclipを足さずに済ませる
ための割り切りで、代わりに下端に最大1行分の余白が出ます。行高は見出しで
変わるので「1行スクロール」の移動量は場所によって変わります。

**ページは完成したものしか表示しません。** 取得中は直前のページを出したまま
受信量だけを更新し、本文が終わって文書が組み上がった時点で一度に差し替え
ます。途中まで作った文書を表示しないのは、「途中で切れたページ」と「そこで
終わっているページ」が読み手には区別できないからです。

## 操作

| 入力 | 動作 |
| --- | --- |
| `Tab` | 次のリンクを選択。status行に飛び先が出る |
| `Enter` | 選択中リンクへ移動。**未選択ならアドレス欄を開く** |
| `Backspace` | 戻る（アドレス編集中は1文字削除） |
| `Escape` | 読み込み中は中止／アドレス欄が開いていれば閉じる／それ以外は終了 |
| `↑` `↓` | 1行スクロール |
| `PageUp` `PageDown` `Space` | 1画面スクロール（1行重ねる） |
| `Home` `End` | 文書の先頭／末尾 |
| `F2`、アドレス欄のクリック | アドレス編集 |
| `←` `→` `Home` `End` `Delete` | アドレス編集中のカーソル移動と削除 |
| タッチ／クリック | リンクの選択と移動 |
| ホイール | 3行スクロール |

`Enter`が2つの意味を持ちますが、両方が同時に可能になることはありません
（`Tab`を押していない読み手には辿る先が無い）。新しいキーを増やさずに
アドレス入力へ到達できます。

**アドレス欄は今出ているアドレスを保持したまま開き**、カーソルは末尾に付き
ます。当初は全選択状態にして最初の1文字で置き換わるようにしていましたが、
それが正しいのは「アドレス全体を貼り付ける」のが主な操作であるデスクトップの
ブラウザで、ここでは違います。よくやるのは末尾を変えることで、親指キーボードで
打ち直すのがいちばん高くつく部分です。

`←`／`→`でカーソルを動かし、`Home`／`End`で両端へ飛び、`Backspace`と`Delete`で
前後の1文字を消します。表示は常にカーソルが見える位置へ追従するので、長い
アドレスの途中を直すこともできます。入力はASCIIだけ、長さは`MAX_URL_BYTES`
までです。

## 組み込みページ

`http://built-in/`以下はflash上にあり、ネットワークを必要としません。
Wi-Fiが落ちているときに表示側だけを確認できる唯一の文書で、`browser`を
引数なしで開いたときの起点でもあります。

| アドレス | 内容 |
| --- | --- |
| `/` | 操作説明とリンク集（ホーム） |
| `/sample` | 対応要素を一通り含む見本 |
| `/long` | scroll用の長い文書 |
| `/wide` | URL上限と同じ長さの1行 |
| `/empty` | 空文書 |

ホスト名`built-in`はresolverに渡す前に判定します。LAN上に同名のホストが
居ても、flash上のページが外部への要求に化けることはありません。

## 対応するHTML

| 要素 | 扱い |
| --- | --- |
| `title` | タイトルとして保持。本文には入れない |
| `h1`〜`h6` | 見出し。`h1`・`h2`は3倍角、`h3`以降は本文サイズで二度打ち |
| `p`、`div`、`section`、`article`ほか | 段落境界（列挙は`document::is_block`） |
| `br` | 強制改行 |
| `pre` | 空白と改行を保持。画面幅では折り返す |
| `a href` | リンク。青＋下線、選択中は反転 |
| `ul`、`ol`、`li` | markerと字下げ。深さは`MAX_NESTING_DEPTH`で頭打ち |
| `hr` | 水平線 |
| `img alt` | `[alt text]`、`alt`が無ければ`[image]` |
| `strong`、`b` | 二度打ちで太く見せる |
| `em`、`i` | 濃い赤 |
| `code`、`kbd`、`samp`、`tt`、`var` | 緑 |
| `script`、`style` | end tagまで読み飛ばし、内容は表示しない |
| comment、doctype、未知の要素 | 表示しない。既知の子テキストは通常どおり |

未知の要素は**inline扱い**です。blockとして扱う要素名は明示列挙にしてあり
ます。逆にすると`<span>`や`<font>`が文の途中で段落を割ってしまうためで、
取り違えたときの被害が小さい側を既定にしています。

文字参照は数値参照（10進・16進）と`amp`・`lt`・`gt`・`quot`・`apos`・`nbsp`
の6つだけです。**終端の`;`は必須**で、`&amp`（`;`なし）は`&amp`とそのまま
表示されます。解決できない参照は入力を失わない形でそのまま出します。

HTMLは壊れているのが通常だという前提です。未終了タグ・不正UTF-8・未知の
属性は読み飛ばして次のテキストへ復帰します。不正なUTF-8はU+FFFDへ置換して
前後のテキストを残します。ただし**上限超過だけは「読み飛ばして成功」に
しません**。

### 非ASCIIの表示

5×7フォントにglyphが無い文字は、1文字1マスの**中空の四角**で描きます。
空白にすると日本語のページが白紙に見え、`?`にすると著者が書いた`?`と
区別が付きません。折り返し幅の計算も1文字1マスなので、行の長さは合います。
日本語フォントは[`WEB_BROWSER_PLAN.md`](WEB_BROWSER_PLAN.md)の将来拡張です。

## 上限

すべて`browser/src/limits.rs`にあります。到達したら**エラーとして表示し、
切り詰めたページを成功として出しません**。

| 項目 | 上限 |
| --- | ---: |
| HTTP応答ヘッダ | 4 KiB |
| URL 1件 | 2,048 byte |
| redirect | 5回 |
| 履歴 | 8ページ |
| decode済みHTML入力 | 2 MiB |
| 保持する表示テキスト | 1 MiB |
| 文書item（block＋run） | 8,192件 |
| link | 1,024件 |
| 折返し後の行 | 32,768行 |
| list／inlineの深さ | 32 |
| 1要素で処理する属性 | 16個 |
| ブラウザ所有の動的メモリ | peak 4 MiB |

上限が複数あるとき「どれに先に当たるか」は入力の中身が決まります。同じ
2 MiBでも、markupばかりなら入力上限に、テキストばかりなら1 MiBのテキスト
上限に先に当たります。

入力依存の確保はすべて`try_reserve`／`try_reserve_exact`を通します
（`browser/src/memory.rs`）。通常の`push`はOOMでabortするので、ページ1枚で
基板が落ちることになるためです。本文バッファだけは伸長幅を128 KiBで頭打ちに
してあります。倍々のままだと1 MiBの本文が容量2 MiBを占め、半分が表示中ずっと
遊ぶためです。

## エラー

失敗した遷移は、ビューア自身が組み立てたHTMLを**いつもと同じ**tokenizer・
document builder・layoutに通してページとして表示します。手書きで描かないのは、
エラー画面が「折返しや描画のバグが唯一隠れる場所」になるのを避けるためです。
ページには失敗したアドレス・理由・statusコード・`Home`リンクが載り、
`Backspace`で元居たページへ戻れます。

理由は1語の識別子を持ちます（`app::fetch::Failure::name`）。fixture serverの
manifestはこの名前で期待値を書きます。

| name | 意味 |
| --- | --- |
| `https` | HTTPSは未対応。httpへ落とさない |
| `redirect-limit` | 5回を超えた、または循環している |
| `redirect-broken` | `Location`が無い／読めない |
| `not-http` | 1行目がステータス行ではない |
| `status` | 2xx以外（表示は`status-404`のように数字が付く） |
| `not-html` | HTMLではない |
| `header-limit` | ヘッダが4 KiBまでに終わらない |
| `truncated` | 本文が終わる前に接続が切れた |
| `chunk` | chunked転送の書式が壊れている |
| `body-limit` | 本文が2 MiBを超えた |
| `text-limit` | 表示テキストが1 MiBを超えた |
| `item-limit`／`link-limit`／`line-limit` | 文書・リンク・行の上限 |
| `encoding` | `identity`以外の`Content-Encoding` |
| `dns` | 名前を引けなかった |
| `no-network` | station接続かDHCPが済んでいない |

## URLの扱い

**表示するアドレスと実際に接続するhost／portは同じ`Url`から生成します。**
別々の文字列を正にすると、アドレス欄が嘘をつける実装になります。

- 接続できるschemeは`http`だけ。`https`は認識して拒否、それ以外は拒否
- hostはASCII DNS名またはIPv4。IDNA変換は行わない（punycode表記は入力可）
- userinfo（`user@host`）とIPv6 literalは拒否
- 数字とドットだけのhostは厳格なdotted quad（先頭ゼロ禁止）でなければ拒否し、
  DNSへ投げない。`0177.0.0.1`のような表記が読み手によって別の意味になる余地を
  残さないため
- path／queryの非ASCIIはpercent-encodeする。`%2e`は`.`と**同一視しない**
  （`..`の正規化対象にしない＝directory traversalを作らない）
- host・request target・header valueにCTL・空白・CR・LFを許さない
- request targetにfragmentを載せない

`hbase <url>`をシェルで設定しておくと、`browser <path>`とアドレス欄の部分
指定がそれに対して解決されます。補完はページ上の`href`と同じ`Url::resolve`
で、独自の短縮記法はありません。

## 実装の分かれ方

URL・HTML・文書モデル・折返しは、依存ゼロの別クレート`browser/`
（パッケージ名`tab5-browser`）にあります。ホストでビルドできるので
`cargo test`で検査できるためで、firmware側は`src/browser.rs`が再exportする
だけです。パスは`crate::browser::url::Url`のようになり、呼び出し側は
クレート境界を意識しません。

```sh
mise run test    # cargo test -p tab5-browser --target x86_64-unknown-linux-gnu
```

| モジュール | 責務 |
| --- | --- |
| `browser/src/url.rs` | URL解析・検証・相対参照解決・request target生成 |
| `browser/src/html.rs` | 増分HTML tokenizer。chunk境界に依存しない |
| `browser/src/document.rs` | 文書モデル（flat arena）とビルダ |
| `browser/src/layout.rs` | 折返しレイアウト、当たり判定、リンク順序 |
| `browser/src/limits.rs` | 上限の一覧 |
| `browser/src/memory.rs` | 失敗を返す確保 |
| `src/app/fetch.rs` | 取得の状態機械。DNS→接続→head判定→本文→文書 |
| `src/app/browser.rs` | 画面、入力、履歴、アドレス編集、組み込みページ |
| `src/app/pointer.rs` | `win`と共有するカーソル |
| `src/app/browsertest.rs` | `hs`／`bt`診断 |

`src/app/fetch.rs`をビューアから切り出してあるのは、診断（`bt`）が同じ
ものを回すためです。redirect追跡・statusの判定・「そもそもHTMLか」の判定が
二重にあると、診断が実際に使われるコードとは別のコードを検査することに
なります。

## 診断コマンド

| コマンド | 内容 |
| --- | --- |
| `hbase [<url>\|off]` | 部分指定の基準アドレス。表示・設定・解除 |
| `hs <url\|path> [r <n>\|p [n]\|c <n>]` | 1回の取得を数値で報告。`p`は文書も組む。`r`は繰り返し、`c`は開始直後の中止 |
| `bt [rounds]` | `/manifest.txt`の全端点を巡回し、期待値と突き合わせる |

試験素材は`tools/browser_fixture_server.py`です。正常なHTMLに加えて、
ヘッダが終わらない応答・`Content-Length`と食い違う本文・16進でないchunk
size・途中で切れる接続など、まともなサーバなら送らない応答を返します。
上限値はPython側とRust側の両方にあり、起動時に突き合わせて食い違えば
起動しません。

```sh
python3 tools/browser_fixture_server.py            # 0.0.0.0:8080
python3 tools/browser_fixture_server.py --list     # 端点と期待値
python3 tools/browser_fixture_server.py --dump browser/tests/fixtures
```

`--dump`で書き出したHTMLは`browser/tests/fixtures.rs`が解析します。実機が
返すのと同じbyte列をホストでも解析するので、期待値の写しが2つある状態を
作らずに済みます。
