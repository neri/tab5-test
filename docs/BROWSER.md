# ハイパーテキストビューア（browser）

> 索引: [`../DESIGN.md`](../DESIGN.md) ／ 段階分けと実機での判断記録:
> [`WEB_BROWSER_PLAN.md`](WEB_BROWSER_PLAN.md)

`browser`コマンドで開く全画面のビューアです。**Webブラウザではありません。**
HTTPまたはHTTPSで取得したHTMLから文章とリンクを取り出し、画面幅へ折り返して読む
ものだけを実装してあります。CSS・JavaScript・画像はいずれもありません。TLSは
ありますが、接続先の身元は確認しません（下記「未認証TLS」）。

対象を狭く固定してあるのは能力不足の言い訳ではなく設計です。対象外の内容を
推測して実行したり、上限なく保持したりしないことが、1つのページで基板ごと
落ちない条件だからです。

## できることとできないこと

| できる | できない |
| --- | --- |
| `http://`と`https://`の取得 | 接続先の**身元の確認**（未認証TLS。下記） |
| HTMLの文章・見出し・リスト・リンク | CSS（`style`属性・`<style>`・外部stylesheet） |
| 相対リンクの解決、履歴8ページ、戻る | JavaScript、DOM API |
| リダイレクト5回まで | 画像のデコード（`img`は`alt`だけ） |
| chunked転送、`Content-Length`、close終端 | form送信、cookie、認証、キャッシュ |
| キーボード・タッチ・USBマウス | 日本語フォント、日本語入力 |
| 読み込み中のキャンセル | IPv6、HTTP/2、HTTP/3、WebSocket |

**認証情報を送る機能を持ちません。** cookie、form送信、Basic／Bearer認証の
いずれも無く、それはTLSが入っても変わりません。理由はTLSの中身にあります。

### 未認証TLS

`https://`で接続すると、serverの`CertificateVerify`とFinished、全recordの
AEAD tagを検証します。しかしそれで分かるのは「相手が、提示した証明書の
秘密鍵を持っている」ことだけで、その証明書がアドレス欄のhostのものかは
**確認していません**。chainを辿らず、rootを見ず、名前を照合せず、有効期限も
読みません（[NETWORK.md](NETWORK.md)、[TLS_PLAN.md](TLS_PLAN.md)）。

したがって受動的な盗聴は防ぎますが、能動的な攻撃者は自前の証明書で接続を
終端でき、上の検証はすべて通ります。**これは通常のHTTPSと同じものではありません。**

toolbarの左端のバッジはこの区別をそのまま出します。

| バッジ | 色 | 意味 |
| --- | --- | --- |
| `INSECURE HTTP` | 赤 | 平文。経路上の誰でも読める |
| `TLS UNVERIFIED` | 赤 | 暗号化されているが、相手が誰かは未確認 |
| `TLS PINNED` | 緑 | leafのSPKIがfirmware組み込みのpinと一致（pin表は空なので通常buildでは出ません） |
| `CONNECTING` | 地色 | 接続中。まだ何も証明されていない |

前2つが同じ赤なのは、読み手にとって意味が同じだからです——画面に出ている
ものがアドレスどおりの出所とは限らない。`SECURE`という語はどのバッジにも
出しません。バッジ幅は最長の文字列に固定してあり、ページが変わっても
アドレス欄が横にずれません。

読み込み中のバッジは**読み込み中の接続**の状態です。画面に残っている前の
ページのものではありません。アドレス欄は既に新しいURLへ移っているので、
そこに前のページのバッジを併記するのが、唯一積極的に誤解を招く組み合わせに
なるためです。handshakeが終わるまでは`CONNECTING`で、schemeから推測しません。

## 画面

上から固定toolbar（40 px）、viewport、固定status行（32 px）の3帯です。

```text
 y=0    ┌───────────────────────────────────────────────┐
        │ TLS UNVERIFIED https://host/page     12 links │  toolbar
 y=40   ├───────────────────────────────────────────────┤
        │ 見出し                                        │
        │                                               │  viewport
        │ 本文。画面幅で折り返し、1行ずつ描く           │
 y=688  ├───────────────────────────────────────────────┤
        │ http://host/選択中リンクの飛び先              │  status
 y=720  └───────────────────────────────────────────────┘
```

- **toolbar**: セキュリティバッジ（上表）、現在のアドレス（編集中はアドレス欄）、
  リンク数と行数。読み込み中は受信KiBに変わります
- **viewport**: scroll位置と交差する行だけを描きます。文書全体のbitmapは
  作りません
- **status**: 直前の操作への返事（赤）があればそれ、無ければ選択中リンクの
  飛び先

### 文字の幅は数えずに測る

**折返し、piece幅、下線、選択背景、当たり判定はすべてpixelで、幅は
`font::advance`から来ます。** rendererが描くときに使うのと同じ関数なので、
測った幅と描いた幅が食い違いません。半角は8 pixel、全角は16 pixel、combining
markは0 pixelです。文字数を数えて一定幅を掛ける計算はどこにもありません。

本文は16 pixel等倍、`h1`と`h2`は2倍の32 pixelです。`h3`以降は本文と同じ大きさで、
1 pixelずらして二度描きすることで太く見せます。bitmapなので拡大は整数倍だけです。

行の高さはglyphの箱に**4分の1の空きを下へ足した**ものです。足さないと16 pixelの
活字が隙間なく並び、ページが壁のように見えます。本文で4 pixel、見出し（2倍角）で
8 pixelです。空きは活字の大きさに対する割合なので、見出しも本文と同じ詰まり具合に
なります。リンクの下線はこの空きに引くので、`g`や`y`の下に届く descender を
潰しません。

### 禁則

日本語には単語間の空白が無いので、行は幅に達したところで切れます。そのままだと
句読点や閉じ括弧が次の行の先頭に来るので、最小限の禁則を入れています。

- 行頭に置かない: `、。，．・：；？！）」』】〉》〕］｝`、小書きかな、長音符など
- 行末に置かない: `（「『【〈《〔［｛`など

禁則で切り直せる位置は最大4文字前までで、それでも見つからないときや、切り直すと
行が空になるときは幅どおりの位置で切ります。整った行より、必ず前へ進むことを
優先します。`pre`は書かれたとおりに出すので禁則を適用しません。

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
| `Ctrl+Q`、`q` | 終了。`Ctrl+Q`はアドレス編集中や読み込み中でも効く |
| `Ctrl+L`、`F2`、アドレス欄のクリック | アドレス編集 |
| `Tab` | 次のリンクを選択。status行に飛び先が出る |
| `Enter` | 選択中リンクへ移動。**未選択ならアドレス欄を開く** |
| `Backspace` | 戻る（アドレス編集中は1文字削除） |
| `Escape` | 読み込み中は中止／アドレス欄が開いていれば閉じる／それ以外はリンク選択とstatusのメッセージを解除。**終了しません** |
| `↑` `↓` | 1行スクロール |
| `PageUp` `PageDown` `Space` | 1画面スクロール（1行重ねる） |
| `Home` `End` | 文書の先頭／末尾 |
| `←` `→` `Home` `End` `Delete` | アドレス編集中のカーソル移動と削除 |
| タッチ／クリック | リンクの選択と移動 |
| ホイール | 3行スクロール |

**終了は`Ctrl+Q`です。`Escape`では終わりません。** ここに置いてあるどの
キーボードでもいちばん押しやすい単独キーが、読んでいたページを捨てる操作を
兼ねているのが誤爆の原因でした。`Escape`に残したのは「今の状態を1つ戻す」
——読み込みの中止、アドレス欄を閉じる、リンク選択の解除——だけです。

`q`でも終了します。**CardKB v1.1にはCtrlキーが無い**ので、CardKBで操作する
ときはこちらが唯一の終了手段です（アドレス欄も`Ctrl+L`ではなく`F2`）。割り当てて
問題がないのは、アドレス欄以外に文字入力が無いからです。アドレス編集中の`q`は
ただの`q`で、そこから抜けるのは`Ctrl+Q`（終了）か`Escape`（欄を閉じる）です。

`Ctrl`は`input::Key`の`Control(letter)`として届きます。届くのはHIDキーボード
（Tab5 KeyboardとUSB）だけです（[INPUT.md](INPUT.md)）。`Ctrl+Q`だけはアドレス欄が
開いていても読み込み中でも先に処理するので、「出られなくなる画面の状態」が
ありません。

アドレス欄への入り口が3つあるのは、`Ctrl+L`がデスクトップのブラウザと同じ、
`F2`がCtrlキーの無いキーボード用、`Enter`（リンク未選択時）が教わらなくても
見つかる経路だからです。`Enter`が2つの意味を持ちますが、両方が同時に可能に
なることはありません（`Tab`を押していない読み手には辿る先が無い）。

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
| `/japanese` | 半角と全角の混在、禁則、行をまたぐリンク、結合文字、未収録文字 |
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

日本語は16 pixelフォントのglyphでそのまま表示します。収録範囲は
[`FONT.md`](FONT.md)にあります。

フォントに無い文字は、**中空の四角**で描きます。空白にすると日本語のページが
白紙に見え、`?`にすると著者が書いた`?`と区別が付きません。枠の幅は`advance`が
返す幅（ASCIIは8 pixel、それ以外は16 pixel）と同じなので、折返しと当たり判定も
ずれません。

`http://built-in/japanese`が組み込みの日本語fixtureです。半角と全角の混在、
句読点の禁則、行をまたぐリンク、分解された濁点、人名の異体字、未収録のemojiを
1ページに入れてあり、ネットワーク無しで確認できます。

## 上限

すべて`browser/src/limits.rs`にあります。到達したら**エラーとして表示し、
切り詰めたページを成功として出しません**。

| 項目 | 上限 |
| --- | ---: |
| HTTP応答ヘッダ | 16 KiB |
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
| `https-downgrade` | `https`から`http`へのredirect。**自動では落ちません** |
| `tls-auth-downgrade` | pin済みの接続から、pinの無いhostへのredirect |
| `tls-connect`／`tls-cert`／`tls-alert`ほか | TLS層の失敗（[NETWORK.md](NETWORK.md)） |
| `redirect-limit` | 5回を超えた、または循環している |
| `redirect-broken` | `Location`が無い／読めない |
| `not-http` | 1行目がステータス行ではない |
| `status` | 2xx以外（表示は`status-404`のように数字が付く） |
| `not-html` | HTMLではない |
| `header-limit` | ヘッダが16 KiBまでに終わらない |
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

- 接続できるschemeは`http`と`https`。それ以外は拒否
- **redirectでの降格は拒否します。** `https`→`http`は`https-downgrade`、
  pin済み接続からpinの無いhostへは`tls-auth-downgrade`。`http`→`https`は
  許可します。これは「利用者が`http://`のアドレスを打つ」「ページ上の
  `http`リンクを選ぶ」ことを禁じる規則ではありません——それらはアドレスが
  見えた上での選択です。禁じているのは、その選択を相手のserverが代わりに
  行うことです
- hostはASCII DNS名またはIPv4。IDNA変換は行わない（punycode表記は入力可）
- userinfo（`user@host`）とIPv6 literalは拒否
- 数字とドットだけのhostは厳格なdotted quad（先頭ゼロ禁止）でなければ拒否し、
  DNSへ投げない。`0177.0.0.1`のような表記が読み手によって別の意味になる余地を
  残さないため
- path／queryの非ASCIIはpercent-encodeする。`%2e`は`.`と**同一視しない**
  （`..`の正規化対象にしない＝directory traversalを作らない）
- host・request target・header valueにCTL・空白・CR・LFを許さない
- request targetにfragmentを載せない

### schemeを打たなかったとき

**キーボードから来たアドレスにschemeが無ければ`http://`を補います。**
`browser`・`hs`・`bt`の引数と、ビューアのアドレス欄が対象で、入口は
`Url::parse_typed`の1つです。`href`や`Location`が通る`Url::parse`は従来どおり
schemeを必須にします——文書の中のscheme無しは壊れた文書であって、省略では
ないからです。

「schemeがある」の判定は**`scheme://`の形をしているか、`http:`／`https:`で
始まるか**です。コロンの有無では判定できません。`localhost:8080`や
`example.com:8080/x`はURLの文法上schemeが`localhost`／`example.com`に見え、
`Url::classify`もこれを`Absolute`と答えます。いちばん普通に打たれる形が
「未対応のscheme」になっていたのがこの判定を分けた理由です。

| 打った文字列 | 結果 |
| --- | --- |
| `192.168.0.2:8080/x` | `http://192.168.0.2:8080/x` |
| `localhost:8080` | `http://localhost:8080/` |
| `example.com/page.html` | `http://example.com/page.html` |
| `//example.com/x` | `http://example.com/x`（schemeだけ補う） |
| `https://example.com/` | そのまま。**httpへは落としません** |
| （`https`ページ上の`http`リンク） | 隠さず平文で読み込み、完了後は`INSECURE HTTP`になる |
| `ftp://example.com/` | そのまま拒否。`http://ftp://…`にはしません |
| `http:example.com` | `http:`はschemeなので補完せず、「hostが無い」のまま |

補完してもhostの検査は変わりません。`0177.0.0.1/x`は`http://`が付いたあとに
dotted quadとして拒否され、`user@example.com/x`はuserinfoとして拒否されます。

アドレス欄だけは、**ページの隣にあるときだけ意味を持つ参照**——`/path`、
`?q=1`、`#part`、`./x`、`../x`——を今のページに対して`Url::resolve`で解決
します（ページ上の`href`と同じ処理）。先頭がドットでない`page.html`は
参照ではなくhostとして読み、`http://page.html/`になります。どちらかに
決めなければならない形で、ドットの有無を境目にしてあります。

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
| `src/app/fetch.rs` | 取得の状態機械。DNS→接続→head判定→本文→文書。schemeからtransportを選び、redirectでの降格を拒否し、接続が何を証明したか（`PageSecurity`）を持つ |
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
| `hs <url> [r <n>\|p [n]\|c <n>]` | 1回の取得を数値で報告。`p`は文書も組む。`r`は繰り返し、`c`は開始直後の中止 |
| `bt <url> [rounds]` | fixture serverのアドレスを渡し、`/manifest.txt`の全端点を巡回して期待値と突き合わせる |

試験素材は`tools/browser_fixture_server.py`です。正常なHTMLに加えて、
ヘッダが終わらない応答・`Content-Length`と食い違う本文・16進でないchunk
size・途中で切れる接続など、まともなサーバなら送らない応答を返します。
上限値はPython側とRust側の両方にあり、起動時に突き合わせて食い違えば
起動しません。

どちらもschemeを省けるので、`hs 192.168.0.2:8080/simple.html p`や
`bt 192.168.0.2:8080 10`と打てます。基準アドレスを覚える`hbase`は廃止しま
した。覚えている値と打った値の2つが基準になりうる状態をなくすためで、
短く打てる部分はscheme補完が引き受けています。

```sh
python3 tools/browser_fixture_server.py            # 0.0.0.0:8080
python3 tools/browser_fixture_server.py --list     # 端点と期待値
python3 tools/browser_fixture_server.py --dump browser/tests/fixtures
```

fixture serverは同じ端点をTLSでも出します（`--tls-port`、既定8443）。
`bt https://<addr>:8443` とすると全manifestがTLS上で巡回されるので、
平文とHTTPSで結果が一致することを突き合わせられます。

`/manifest.txt`は**接続ごとに内容が変わります**。`/redirect/https`は平文から
なら上へ（TLS listenerへ）redirectして`ok`、TLSからなら下へ（平文listenerへ）
redirectして`https-downgrade`になります。降格の拒否は「TLSを話すserverから
平文へ誘導される」状況でしか作れないので、この向きはTLS listenerが要ります。

`/redirect/unpinned`はpin済み接続からpinの無いhostへのredirectで、
`tls-auth-downgrade`になります。ただしこれは**基板側のbuild**にも依存します
（pinを持たないbuildでは失うidentityが無いので、単に名前が引けず`dns`）。
既定ではpin入りbuildを想定するので、通常buildで巡回するときは
`--unpinned-board`を付けてください。

TLS固有の失敗は専用のlistenerで、manifestには絶対URLとして載ります
（平文の巡回からも届きます）。

| port | 提示するもの | pin無しbuild | pin入りbuild |
| --- | --- | --- | --- |
| `--tls-port` | ECDSA P-256、`--tls-key-name`の鍵 | `ok` | `ok`／`other`鍵なら`tls-pin` |
| +1 | fatalな`handshake_failure` alertだけ | `tls-alert` | `tls-alert` |
| +2 | Ed25519証明書 | `tls-cert` | `tls-pin` |
| +3 | RSA-PSS証明書 | `ok` | `tls-pin` |

**pinはhostname単位で、port単位ではありません。** これらのlistenerはpin済みの
listenerと同じアドレスにいるので、pinを持つ基板はここでもpinを照合し、どちらの
鍵もpinされていないため署名方式に到達する前に拒否します。回避せず期待値として
書いているのは、これがpinを持つ意味そのものだからです——別のportから来た鍵が
通ってしまうpinは、1つのportしか守らないpinです。

したがってEd25519の明示拒否（`tls-cert`）とRSA-PSSの成功（`ok`）を確認するのは
pin無しbuildの巡回です。2つのbuildで1回ずつ巡回すると全部が埋まります。

`--dump`で書き出したHTMLは`browser/tests/fixtures.rs`が解析します。実機が
返すのと同じbyte列をホストでも解析するので、期待値の写しが2つある状態を
作らずに済みます。
