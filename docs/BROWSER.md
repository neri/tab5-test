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
| `file:`で本体上のファイルとディレクトリ | ファイルへの書き込み |
| HTMLと`text/plain`ほかの`text/*` | それ以外のmedia type（画像、実行形式など） |
| HTMLの文章・見出し・リスト・リンク | CSS（`style`属性・`<style>`・外部stylesheet） |
| 相対リンクの解決、履歴8ページ、戻る・進む | JavaScript、DOM API |
| 再読込、リダイレクト5回まで | 画像のデコード（`img`は`alt`だけ） |
| UTF-8とShift_JIS（下記「文字符号化」） | それ以外の符号化（EUC-JP、ISO-2022-JPなど） |
| chunked転送、`Content-Length`、close終端 | form送信、cookie、認証、キャッシュ |
| キーボード・タッチ・USBマウス | 日本語入力 |
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

toolbarのアドレス欄の左にある**南京錠**がこの区別を出します。

| 錠 | 色 | 意味 | 押すとstatus行に出る文 |
| --- | --- | --- | --- |
| 開いた錠 | 赤 | 平文HTTP。経路上の誰でも読める | `INSECURE HTTP: plaintext; anyone carrying it can read it` |
| 開いた錠 | 赤 | 未認証TLS。暗号化されているが相手が誰かは未確認 | `TLS UNVERIFIED: encrypted, but nobody checked who answered` |
| 閉じた錠 | 緑 | leafのSPKIがfirmware組み込みのpinと一致（pin表は空なので通常buildでは出ません） | `TLS PINNED: the peer's key matches a pin built into this firmware` |
| 輪郭だけ | 地の文字色 | 接続中。まだ何も証明されていない | `Connecting: nothing has been proved yet` |

**平文と未認証TLSは同じ絵です。**読み手にとって意味が同じだからで——画面に
出ているものがアドレスどおりの出所とは限らない——これは以前この2つを同じ赤の
文字バッジにしていたのと同じ判断です。`http`か`https`かの違いはアドレス欄が
数ピクセル右で出しています。`SECURE`という語はどこにも出しません。

以前は`TLS UNVERIFIED`のような文字を14セル（112 px）固定で出していました。
アドレス欄がいちばん幅を必要とする部分なので、絵にして24 pxへ縮め、**文言は
錠を押したときにstatus行へ出す**ことにしました。錠にフォントのglyphはない
（U+1F512はBMP外でUnifont-JPも持たない）ので、ここだけ`fill_rect`で描いています。

読み込み中のバッジは**読み込み中の接続**の状態です。画面に残っている前の
ページのものではありません。アドレス欄は既に新しいURLへ移っているので、
そこに前のページのバッジを併記するのが、唯一積極的に誤解を招く組み合わせに
なるためです。handshakeが終わるまでは`CONNECTING`で、schemeから推測しません。

## 画面

上から共有system bar（48 px）、8 pxの余白、viewport、固定status行（32 px）です。
Browserは通常GUIアプリとして中央のapp領域だけを描き、左右のsystem領域はhostが描きます。
統合経路は実機未確認です（[SYSTEM_BAR.md](SYSTEM_BAR.md)）。

```text
 y=0    ┌───────────────────────────────────────────────┐
        │≡│← → ↻ 🔓 address             │Wi-Fi│BAT│ HH:MM│
 y=48   ├╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌┤  8 pxの余白
 y=56   │ 見出し                                        │
        │                                               │  viewport
        │ 本文。画面幅で折り返し、1行ずつ描く           │
 y=688  ├───────────────────────────────────────────────┤
        │ http://host/選択中リンクの飛び先              │  status
 y=720  └───────────────────────────────────────────────┘
```

- **toolbar**: ボタン3つ、セキュリティの南京錠（上表）、現在のアドレス
  （編集中はアドレス欄）。LauncherとWi-Fi／Battery／Clockはsystem領域
- **余白**: 8 px。ページの色で塗ります。無いと本文の1行目がアドレス欄の下端に
  接します。初回・画面復帰の全面再描画で塗り、address更新だけでは再描画しません
- **viewport**: scroll位置と交差する行だけを描きます。文書全体のbitmapは
  作りません
- **status**: 直前の操作への返事（赤）があればそれ、読み込み中なら受信KiB、
  無ければ選択中リンクの飛び先

### toolbarの配置

| 部位 | x | 幅 | 備考 |
| --- | ---: | ---: | --- |
| Launcher | 0 | 52 | system所有 |
| 戻る `←` | 52 | 52 | 履歴が空なら灰色 |
| 進む `→` | 104 | 52 | 進む履歴が空なら灰色 |
| 再読込／中止 | 156 | 52 | 読み込み中は中止 |
| 南京錠 | 220 | 24 | statusへ説明を出す |
| アドレス欄 | 252 | 796 | 本文94 cell分（caretを含む） |
| 全消去 `×` | 1016 | 32 | 編集中だけ |
| Wi-Fi | 1056 | 48 | system所有 |
| Battery | 1104 | 48 | system所有 |
| 音量予約 | 1152 | 48 | 空白、操作なし |
| Clock | 1200 | 80 | system所有 |

bar高48 pixel、viewportのy=56..688、statusのy=688..720は維持する。
横配置は`system-ui/`の共通定数から導き、本文64 cell以上とsystem領域との非重なりをassertする。
URL上限は変えず、編集中はcaretへ横追従する。bar操作は押下開始位置を保持し、releaseで確定する。
一度targetから出れば、戻っても取り消す。全消去領域は編集中だけ別targetになる。

### 文字の幅は数えずに測る

**折返し、piece幅、下線、選択背景、当たり判定はすべてpixelです。** 通常本文・link・
見出しはA4比例幅Sans、`pre`と`code`／`kbd`／`samp`／`tt`／`var`はSans Monoです。
layoutとrendererは同じstrike metricsを使います。日本語は従来glyphへfallbackし、combining
markを伴うLatin baseもcluster単位で従来経路へ戻します。

`pre`はHTML parserのrun styleに依存せず、layoutが`BlockKind::Preformatted`の全pieceで
font roleをMonoにします。色・強調styleとは別の軸なので、`pre`をcode色へ変えません。これにより
内側に`<code>`が無い通常の`<pre>`でも、折返し測定と描画の
両方が固定幅になります。組み込み`http://built-in/sample`には同じ`iiiiiiii`／`WWWWWWWW`／
`00000000`を比例幅段落、inline code、桁ルーラー付き`pre`で並べた比較欄があります。

本文は16 pixel strike、`h1`と`h2`は32 pixel strikeです。`h3`以降は本文と同じ大きさで、
1 pixelずらして二度描きすることで太く見せます。日本語は従来16 pixel glyphの1倍／2倍です。

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
| `Ctrl+Q`、`q` | デスクトップへ戻る。`Ctrl+Q`はアドレス編集中や読み込み中でも効く |
| `Ctrl+L`、`F2`、アドレス欄のクリック | アドレス編集 |
| `Tab` | 次のリンクを選択。status行に飛び先が出る |
| `Enter` | 選択中リンクへ移動。**未選択ならアドレス欄を開く** |
| `Backspace`、`[`、戻るボタン | 戻る（アドレス編集中は1文字削除） |
| `]`、進むボタン | 進む |
| `r`、`F5`、`Ctrl+R`、再読込ボタン | 今のアドレスを取得し直す。scroll位置は保つ |
| `Escape`、中止ボタン | 読み込み中は中止／アドレス欄が開いていれば閉じる／それ以外はリンク選択とstatusのメッセージを解除。**終了しません** |
| 南京錠のタップ | セキュリティ状態の文言をstatus行へ |
| Wi-Fiのタップ | Network settingsミニを開く。戻ると同じBrowser状態を再描画 |
| M（URL編集中以外）／F3 | Launcherを開く |
| Batteryのタップ | Battery detailsミニを開く |
| `i`、`F1` | ヒープ・ソケット・履歴・ページの大きさをstatus行とUARTへ（下記） |
| `↑` `↓` | 1行スクロール |
| `PageUp` `PageDown` `Space` | 1画面スクロール（1行重ねる） |
| `Home` `End` | 文書の先頭／末尾 |
| `←` `→` `Home` `End` `Delete` | アドレス編集中のカーソル移動と削除 |
| タッチ／クリック | リンクの選択と移動 |
| ホイール | Browser content上だけ3行スクロール |

**終了は`Ctrl+Q`です。`Escape`では終わりません。** ここに置いてあるどの
キーボードでもいちばん押しやすい単独キーが、読んでいたページを捨てる操作を
兼ねているのが誤爆の原因でした。`Escape`に残したのは「今の状態を1つ戻す」
——読み込みの中止、アドレス欄を閉じる、リンク選択の解除——だけです。

`q`でも終了します。**CardKB v1.1にはCtrlキーが無い**ので、CardKBで操作する
ときはこちら、またはMからLauncherのConsoleを選びます（アドレス欄はリンク未選択でEnter）。割り当てて
問題がないのは、アドレス欄以外に文字入力が無いからです。アドレス編集中の`q`は
ただの`q`で、そこから抜けるのは`Ctrl+Q`（終了）か`Escape`（欄を閉じる）です。
再読込の`r`と、戻る・進むの`[`・`]`が単独キーなのも同じ理由です。

**ボタンとキーは同じ動作へ入ります。**中止ボタンは`Escape`と同じ経路
（`Action::Cancel`）を通り、走っている取得のソケットを返すコードは1箇所です。
ボタンのために2つ目を書くと、返し忘れがそちらにだけ残ります。

再読込はscroll位置を保ちます。読んでいる途中でページが変わったかもしれない
というのが再読込する普通の理由なので、先頭へ飛ぶと読み直しになるためです。
エラーページの上で押すと失敗したアドレスをもう一度試します（下記）。

保存profileまたはWi-Fiメニューから始めた管理対象接続では、ページ取得中にSTA切断や
C6リンク喪失を検出しても、その通信をすぐエラーページにはしません。古いsocketは古い
IP stackとともに捨てますが、要求したURLと履歴操作を「Wi-Fi再接続待ち」として保持し、
associationとDHCPが完了したら**同じGETを先頭から自動的にやり直します**。待機中は
`reconnecting Wi-Fi; this page will retry automatically`と表示し、Escape／中止ボタンで
取り消せます。HTTPの途中位置から継続しないのは、切断前と再接続後の応答が同じ内容とは
限らず、途中同士を連結すると1つの壊れたページを成功として表示してしまうためです。

この自動再試行は接続管理器自身が再接続中の場合だけです。CLIの`wifi connect`は資格情報を
保持せず、自動再associationも自動DHCPも行わないため、従来どおり`no-network`になり、
手動で接続とIP設定を行う必要があります（[WIFI.md](WIFI.md)）。

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
アドレスの途中を直すこともできます。caretと横scrollは比例幅のpixel測定で追従します。
入力はASCIIだけ、長さは`MAX_URL_BYTES`
までです。

### 漏れを見るための`i`

`i`（または`F1`）を押すと、漏れがあれば現れる数値をstatus行に出し、同じ行を
UARTへも書きます。

```text
heap 21592K sockets 1 back 3 fwd 2 page 41K peak 13K
```

| 項目 | 意味 |
| --- | --- |
| `heap` | グローバルアロケータが今渡している総量 |
| `sockets` | socket setに入っている数。`+1 loading`が付けば取得が走っている。`-`はstackが無い |
| `back`／`fwd` | 履歴と進む履歴の件数 |
| `page` | 表示中のページの費用（文書＋layout） |
| `peak` | 直前の取得でparserが一度に持った最大（`MAX_BROWSER_OWNED_BYTES`が書かれている相手） |

**値ではなく差を見るためのものです。**「同じ操作を20回して、この行が元へ戻るか」
が知りたいことで、`sockets`が戻らないのはとくに厄介です——socket setが尽きるのは
数分後の別の場所で、そこには原因が何も残っていません。

キーにしてあるのは、これがページ遷移ごとに出ていた`BROWSER page`ログの
置き換えだからです。毎回出るとログが読めなくなる上に、知りたいのは
**読み手が選んだ2つの瞬間の差**なので、そもそも毎回出しても答えになりません。

`sockets`はsocket set全体で、この画面の取り分ではありません（DHCPとDNSが
自分のものを持っています）。前後で同じ数かどうかだけが意味を持ちます。

### システムバーとミニアプリ

`M`（URL編集中以外）または`F3`でLauncherへ入る。Wi-Fi slotはNetwork settings、
Battery slotはBattery detailsを開く。ミニ中はBrowserのhandlerも取得stepも呼ばない。
Escape／戻る／LauncherのBrowserで同じpage・scroll・history・編集中URLへ戻る。
LauncherのEscape／ハンバーガー再クリックは呼出元へ戻り、Consoleの選択はBrowserを終了する。

進行中のHTTP GETは入る前にsocketを閉じ、URLと履歴操作を保持して復帰時に再開する。
local readは停止したVFS handleとして保持し、復帰時に処理を続ける。
別の前景routeへ移る場合はnetwork／localとも`close_pending`で返す。
indicatorの意味と部分更新は[`SYSTEM_BAR.md`](SYSTEM_BAR.md)にまとめてある。

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

## HTML以外のもの

### `text/plain`とその他の`text/*`

`text/html`以外の`text/*`——`text/plain`、`text/markdown`、`text/csv`など——は
**そのまま表示します**。以前は`not-html`のエラーでした。

扱いは`<pre>`と同じ1ブロックです。`text/plain`が意味するのは「空白と改行は
書いた人のもの」なので、それを保つのが正しい読み方だからです。画面幅では
折り返します——画面より長い行はどこかへ行かなければならないので。

**markupとしては一切読みません。**`<`はタグを開かず、`&amp;`は4文字のまま
です。tokenizerに「plain」の旗を立てて実現していて、旗が立つと状態機械を
まったく回さず全byteをテキストにします。

旗をtokenizerに置いたのは、tokenizerがtag認識**以外に**やっていることが
plain textにもそのまま必要だからです——chunk境界をまたぐUTF-8の持ち越し、
不正byteのU+FFFD置換、入力上限の計数、sinkへの分割送出。どれもmarkupの話では
ありません。

文字符号化の決め方は同じですが、`<meta>`は**探しません**。plain textに
`<meta charset=...>`と書いてあるのは「そう書いてあるファイル」であって
「そう言っているファイル」ではないためです。したがってheaderの`charset`か
BOMだけで、先頭1 KiBを溜める必要もありません。

`Content-Type`が無い応答は従来どおりHTML扱いです（[NETWORK.md](NETWORK.md)）。
`text/*`でもHTMLでもないもの——`application/octet-stream`、画像——はこれまで
どおり`not-html`で拒否します。

### `file:` — 本体の上のもの

`file:`で、マウント済みのボリュームにあるファイルとディレクトリを読めます
（[FILESYSTEM.md](FILESYSTEM.md)）。**書き込みはありません。**

| 打った形 | 結果 |
| --- | --- |
| `file:///tmp/notes.txt` | 正式な形 |
| `file://localhost/tmp/notes.txt` | 同じ。`localhost`はこの機械のこと |
| `file:/tmp/notes.txt` | 同じ。人が打つのはこれなので受ける |
| `file://other-machine/x` | 拒否。`file:`にできる他の機械は無い |
| `file:tmp/x` | 拒否。絶対パスでない |

3つの形は解析後に**同じ値**になります。1つのファイルに3つのアドレスがある
状態を作らないためで、toolbarの表示も比較できる形になります。

- **ディレクトリ**はリンクの一覧ページになります。`..`で上へ、ディレクトリの
  リンクは末尾に`/`が付くので中の相対リンクが正しく解決します。エントリ数は
  512で頭打ちにし、切ったことをページに書きます。ファイルシステム由来の`.`と`..`は
  表示せず、件数にも含めません。一覧の先頭に自動生成する親ディレクトリへの`..`リンクは
  維持します（ルートでは生成しません）。`.hidden`等の名前は表示します

  **ディレクトリのアドレスは末尾に`/`を付けた形へ直してから**ページを組みます
  （`Url::as_directory`）。相対参照は基準の**最後の`/`まで**に対して解決される
  ので、`file:///tmp`を基準にすると`notes.txt`は`file:///tmp/notes.txt`ではなく
  `file:///notes.txt`——1階層上——になります。一覧ページは相対リンクの塊なので、
  自分のアドレスが`/`で終わっていないと**載っているリンクが全部ずれます**。
  見出しも`..`も一覧も表示アドレスも同じ値から作るので、ページが名乗る
  ディレクトリとリンク先のディレクトリが食い違うこともありません
- **ファイル**は名前の末尾で読み方が決まります。`.html`／`.htm`はmarkup、
  **それ以外はすべてplain text**です。ファイルシステムに`Content-Type`は
  無く、先頭を覗いて推測すればその分を溜めることになります。この向きが安全
  なのは、テキストをmarkupとして読むと山括弧の中身が黙って消えるのに対し、
  markupをテキストとして読んでも「markupに見える」だけだからです
- 大きさの上限はネットワークのページと同じ2 MiBで、開く前に`size`で弾きます

**読み込みは1回16 KiBずつ**で、フレームループへ戻ります。ネットワークと同じ
理由です——C6のリンクは読まれない時間が長いと遅くなるのではなく死ぬので
（[APPS.md](APPS.md)）、「決まった量やって返る」はこちらにも要ります。
ディレクトリの一覧だけは一度に組みます。中にあるものが上限で、ファイルの
ように「開くまで分からない」ということが無いためです。

`hs`と`bt`は`file:`のアドレスを断ります。どちらも`Transaction`を回す診断で、
`file:`には引く名前もソケットもstatusもありません。

#### redirectでは開かない

**serverからの`Location: file:///...`は拒否します**（`file-redirect`）。
アドレスを打つのも、ページ上のリンクを選ぶのも、アドレスが見えた上での
読み手の選択です。禁じているのは、その選択をserverが代わりに行うことで、
これはhttpsからhttpへの降格を拒否するのと同じ規則です。

`Fetch::start`も非ネットワークschemeを断ります。取得の状態機械に`file:`が
入る道が2つとも塞がっているので、port 0への接続が生まれる余地がありません。

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

## 文字符号化

UTF-8とShift_JISの2つです。それ以外はUTF-8として読みます。

Shift_JISが要るのは、日本語の小さいserverや古いページが今も使っているためです。
Shift_JISのページをUTF-8として読むと「何文字か化ける」のではなく、**漢字が
すべて不正なUTF-8になるのでページ全体が置換文字になり、読むものが何も残りません**。

復号は`browser/src/encoding.rs`にあり、`Parser`の**内側**です。

```text
バイト列 → Decoder → UTF-8 → Tokenizer → Builder → Document
```

`Parser`の外（たとえば取得側のsink）に置くと、ホストのfixtureテストと`hs p`が
別経路になります。ここに置いてあるので、ビューア・`hs`・`bt`・`cargo test`の
どれもが同じ復号を通ります。

**UTF-8のときは何も起きません。**符号化が決まっていてUTF-8なら、`Decoder::feed`は
渡されたスライスをそのまま渡します。copyもbufferも確保もありません。

### どうやって決めるか

上から順に、先に決まったものが勝ちます。

| # | 根拠 |
| --- | --- |
| 1 | `Content-Type`の`charset`（`fetch`が本文の前に`Parser::declare_charset`で渡す） |
| 2 | BOM（`EF BB BF`）→ UTF-8。3 byteは読み捨てる |
| 3 | 先頭1 KiB内の`<meta charset=…>`または`<meta http-equiv=… content="…; charset=…">` |
| 4 | どれも無ければUTF-8 |

1が無いときは先頭1 KiBを溜めてから決めます。`<meta>`は`<head>`にしか意味が
無く、1 KiB以内に宣言していない`<head>`は事実上宣言していないからです。溜めるのは
ここだけで、決まった時点でまとめて流します。

`charset`という語をwindow全体から探すのではなく、`<meta`から`>`までの範囲だけを
見ます。本文の1段落目が文字コードの話をしていても、それでページが化けることは
ありません。これはHTMLの解析ではなく、そうしようともしていません——ちゃんと解析
するtokenizerはこの判断より後ろにあり、2回通すのがこの仕組みで避けている費用
そのものです。

### ラベル

| ラベル | 符号化 |
| --- | --- |
| `shift_jis`、`shift-jis`、`sjis`、`s-jis`、`x-sjis`、`shiftjis`、`ms_kanji`、`windows-31j`、`cp932`、`csshiftjis` | Shift_JIS |
| `utf-8`、`utf8`、`us-ascii`、`ascii` | UTF-8 |
| その他すべて | UTF-8として読む |

未知のラベルを拒否しないのは、化けたページの方が「何も出ないページ」より読み手に
とってましだからです。

### 変換表

`browser/data/shiftjis.bin`（22,576 byte、DROM）を
`tools/encoding/generate_shiftjis.py`がCPythonの`cp932`から生成します。生成物を
リポジトリに入れ、生成器も入れ、ホスト側のテストが代表点を照合する形は
フォント（[FONT.md](FONT.md)）と同じです。

`shift_jis`ではなく**`cp932`（Windows-31J）**なのは、Web上で`Shift_JIS`と
名乗る文書が実際に使っているのがこちらだからです。狭い方で作るとNEC・IBM拡張
——丸数字、ローマ数字、人名の異体字——が落ちます。WHATWGのencoding標準も
`shift_jis`ラベルをWindows-31Jの表へ写します。

表はJIS区点（区1..120、点1..94）で引く`[u16; 120 * 94]`で、0が未対応です。
区を120まで取るのはIBM拡張が区95以降に乗るためで、この形なら拡張が表の中に
自然に入り、2つ目の表も2回目の探索も要りません。

| バイト | 扱い |
| --- | --- |
| `0x00..=0x7F` | そのままASCII（`0x5C`は円記号ではなく`\`。WHATWGと同じ） |
| `0xA1..=0xDF` | 半角カナ。`U+FF61 + (byte - 0xA1)`で計算。表を引かない |
| `0x81..=0x9F`、`0xE0..=0xFC` | 2 byte文字の先頭。表を引く |
| `0x80`、`0xA0`、`0xFD..=0xFF`、未対応の区点 | U+FFFD |

2 byte目が不正だったときはU+FFFDを出した上で、**そのバイトがASCIIなら先頭
バイトとして読み直します**（WHATWGと同じ）。`>`や`<`はmarkupとして残さないと
いけない一方、もう1つの高位バイトは同じ壊れ方の一部なので、戻すと置換文字が
1つで済むところが2つになります。

フォントはJIS X 0213 plane 1を収録しているので、**表示できる文字は既に
揃っています**（[FONT.md](FONT.md)）。足りなかったのはバイト列から符号位置への
対応だけでした。

EUC-JPは同じ表を別の算術で引くだけですが、今は対応していません。

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

**Shift_JISのページでは、この2 MiBが2箇所で別のものを数えます。**転送側
（`net::http::Transaction`）が数えるのは回線から受け取ったbyte数、tokenizerが
数えるのは復号後のbyte数です。全角文字は2 byteから3 byteへ増えるので、
回線上で約1.33 MiBのページがtokenizer側の2 MiBに先に当たります
（`input-limit`）。テキスト上限（1 MiB）は元から復号後を数えているので
変わりません。

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

**エラーページのアドレスは失敗したアドレスそのものです。**以前は
`http://built-in/error`という、どこにも存在しないアドレスがtoolbarに出ていま
した。読み手は何が失敗したのか見えず、アドレス欄を開いて打ち間違いを直すことも、
再読込することもできませんでした。エラーページ上のリンクはすべて絶対URLなので、
基準アドレスが変わっても行き先は変わりません。「今エラーを出しているか」は
アドレスの一致ではなく`Page`のフラグで判定します。

### HTTPステータスエラーはサーバの本文を出す

**2xx以外でも、HTMLが付いていればそれを表示します。**サーバ自身の404や500の
ページには、どのresourceが無いのか・どのパラメータが悪いのか・loginが要るのか
といった、そのときだけの情報が書いてあることが多く、捨てると読み手にはビューアの
4語しか残りません。判定の順は次のとおりです。

```text
redirect       → 追跡
statusが無い   → not-http
HTMLではない   → not-html（statusを添える）
2xx以外        → 本文を読む。statusを覚えておく
2xx            → 本文を読む
```

表示は普通のページと同じで、status行に`the server answered 404`が出ます。
本文が空だった場合（`Content-Length: 0`、または文書にテキストが1文字も無い）は
ビューア自身のエラーページへ落とします。真っ白なページと「404」は読み手には
区別が付かないためです。

診断（`bt`）から見た名前は変わりません。`Fetch::status`が付いたページは
`status-404`という名前で判定されるので、fixture serverのmanifestの期待値
（`error:status-404`）はそのままです。

通信エラー・TLSエラー・DNS・上限超過は表示すべき本文が存在しないので、
これまでどおりビューア自身のエラーページです。

理由は1語の識別子を持ちます（`app::fetch::Failure::name`）。fixture serverの
manifestはこの名前で期待値を書きます。

| name | 意味 |
| --- | --- |
| `https-downgrade` | `https`から`http`へのredirect。**自動では落ちません** |
| `tls-auth-downgrade` | pin済みの接続から、pinの無いhostへのredirect |
| `tls-alert` | serverがfatal alertを送って断った |
| `tls-handshake` | serverのhandshakeをこちらが解釈できなかった（こちら側の制約） |
| `tls-connect`／`tls-cert`／`tls-pin`ほか | TLS層の失敗（[NETWORK.md](NETWORK.md)） |
| `redirect-limit` | 5回を超えた、または循環している |
| `redirect-broken` | `Location`が無い／読めない |
| `not-http` | 1行目がステータス行ではない |
| `status` | 2xx以外（表示は`status-404`のように数字が付く）。403が返る場合は要求の内容で弾かれている可能性があります（[NETWORK.md](NETWORK.md)の`User-Agent`） |
| `not-html` | HTMLでも`text/*`でもない |
| `not-network` | ネットワークのアドレスではないものを取得しようとした |
| `file-redirect` | serverが`file:`へredirectした。**追いません** |
| `file-not-found` | そのパスに何も無い |
| `file-not-mounted` | そのパスはどのボリュームにも載っていない |
| `file-not-a-file` | ページとして出せるものではない |
| `file-gone` | 媒体が抜かれた、または別のものに入れ替わった |
| `file-read` | ボリュームは答えたが、要求したbyteではなかった |
| `file-too-large` | 2 MiBを超えるファイル |
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

- 接続できるschemeは`http`と`https`。`file`はネットワークへ行かない
  （上記）。それ以外は拒否
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
| `browser/src/encoding.rs` | Shift_JIS復号、`charset`ラベル判定、`<meta>`のsniff |
| `browser/src/html.rs`のplain mode | markupでない文書のtokenizer（全byteがテキスト） |
| `browser/src/html.rs` | 増分HTML tokenizer。chunk境界に依存しない |
| `browser/src/document.rs` | 文書モデル（flat arena）とビルダ |
| `browser/src/layout.rs` | 折返しレイアウト、当たり判定、リンク順序 |
| `browser/src/limits.rs` | 上限の一覧 |
| `browser/src/memory.rs` | 失敗を返す確保 |
| `src/app/fetch.rs` | 取得の状態機械。DNS→接続→head判定→本文→文書。schemeからtransportを選び、redirectでの降格を拒否し、接続が何を証明したか（`PageSecurity`）を持つ |
| `src/app/localfile.rs` | `file:`の読み出し。ディレクトリ一覧、拡張子による読み方の決定、16 KiBずつの読み込み。ファイルハンドルを所有するので`close`が必須 |
| `src/app/browser.rs` | 停止可能なBrowser状態、app領域・本文・status描画、入力handler、履歴、アドレス編集 |
| `src/app/system_bar.rs` | 通常GUIのイベントloop、Launcher、ミニ配信、system領域、pointer、timer |
| `src/app/pointer.rs` | 通常GUI hostの共通カーソル |
| `src/app/browsertest.rs` | `hs`／`bt`診断 |

`src/app/fetch.rs`をビューアから切り出してあるのは、診断（`bt`）が同じ
ものを回すためです。redirect追跡・statusの判定・「そもそもHTMLか」の判定が
二重にあると、診断が実際に使われるコードとは別のコードを検査することに
なります。

`src/app/localfile.rs`は`fetch.rs`と同じ形（start／step／close）にしてあり、
`Failure`と`Outcome`も共有しますが、中身は何も共有しません。Browserの
取得handlerから見ると、`Pending`が`Network`か`Local`かの違いだけで、進め方・
中止・描画・完了はすべて同じコードを通ります。読み手にとって同じ行為だから
です。

## 診断コマンド

| コマンド | 内容 |
| --- | --- |
| `hs <url> [r <n>\|p [n]\|c <n>]` | 1回の取得を数値で報告。`p`は文書も組む。`r`は繰り返し、`c`は開始直後の中止 |
| `bt <url> [rounds]` | fixture serverのアドレスを渡し、`/manifest.txt`の全端点を巡回して期待値と突き合わせる |

試験素材は`tools/browser_fixture_server.py`です。正常なHTMLに加えて、
ヘッダが終わらない応答・`Content-Length`と食い違う本文・16進でないchunk
size・途中で切れる接続など、まともなサーバなら送らない応答を返します。
`/encoding/`以下はShift_JISの4通り——ヘッダで宣言、`<meta>`だけで宣言、
ヘッダが`<meta>`と食い違う、先頭バイトが1つ宙に浮いている——とBOM付きUTF-8です。
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

**既定はpinを持たないbuildです。**`tls-fixture-pins`はdefault featureでは
ないので、`cargo build --release`——CLAUDE.mdが書いているbuild——にpinは1つも
入りません。pin入りbuildで巡回するときだけ`--pinned-board`を付けてください。

この既定は逆でした。既定をpin入りにしていたため、普通にbuildした基板で普通に
巡回すると、Ed25519とRSAのlistenerで必ず2件失敗しました（期待`tls-pin`、実際は
`tls-cert`と`ok`）。基板のpinはサーバから見えず、成功したpin照合は回線に何の
痕跡も残さないので、これはフラグでしか伝えられません。だからこそ**普通のbuildを
説明するのに引数が要る側を既定にすべきではありません**。サーバは起動時に
どちらを仮定しているか1行出します。

TLS固有の失敗は専用のlistenerで、manifestには絶対URLとして載ります
（平文の巡回からも届きます）。

| port | 提示するもの | pin無しbuild | pin入りbuild |
| --- | --- | --- | --- |
| `--tls-port` | ECDSA P-256、`--tls-key-name`の鍵 | `ok` | `ok`／`other`鍵なら`tls-pin` |
| +1 | fatalな`access_denied` alertだけ | `tls-alert` | `tls-alert` |
| +2 | Ed25519証明書 | `tls-cert` | `tls-pin` |
| +3 | RSA-PSS証明書 | `ok` | `tls-pin` |
| +4 | TLS 1.2しか話さないserver | `tls-version` | `tls-version` |

+1が`access_denied`なのは、`handshake_failure`／`protocol_version`／
`insufficient_security`の3つを「話せるものが無い」として`tls-version`へ
振り分けているためです。+1は「話せたのに断った」もう一方のnoを見ます。
+4は本物のTLS 1.2 serverで、実際に`protocol_version`を返します。

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

Browserの終了操作はConsoleへ戻らずデスクトップへ切り替える。共通host内で本文・履歴を保持し、
LauncherからBrowserを選ぶと復帰する。詳細は[`SYSTEM_BAR.md`](SYSTEM_BAR.md)。
