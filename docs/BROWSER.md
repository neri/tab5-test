# ハイパーテキストビューア（browser）

> 索引: [`../DESIGN.md`](../DESIGN.md) ／ 段階分けと実機での判断記録:
> [`WEB_BROWSER_PLAN.md`](WEB_BROWSER_PLAN.md)

`browser`コマンドで開く全画面のビューアです。**Webブラウザではありません。**
HTTPまたはHTTPSで取得したHTMLから文章、リンク、table、form、静止画像を取り出し、
画面幅へ折り返して読むものだけを実装してあります。CSSとJavaScriptはありません。
`img`はPNGとbaseline JPEGを取得・decodeしてRGB565で拡縮描画します。TLSは
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
| 再読込、リダイレクト5回まで | animated／progressive画像、SVG、video、audio |
| UTF-8とShift_JIS（下記「文字符号化」） | それ以外の符号化（EUC-JP、ISO-2022-JPなど） |
| chunked転送、form送信、ファイルcache | cookie、HTTP認証、client証明書 |
| キーボード・タッチ・USBマウス | 日本語入力 |
| 読み込み中のキャンセル | IPv6、HTTP/2、HTTP/3、WebSocket |

**認証情報を保持する機能を持ちません。** cookie、Basic／Bearer認証、client証明書は
無く、それはTLSが入っても変わりません。formの入力値は送信できます。

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

**scrollは文書内のpixel offsetです。** viewportと交差する行を描き、A4文字、
linkの選択背景・下線、table背景・罫線はviewport上下でclipします。そのため
見出しや本文の一部が上下端に見えてもtoolbarやstatus行へはみ出しません。
上下キーは20 px、wheelは1 detent 60 px、PageUp/Downは1画面から20 px重ねて移動します。
この表示経路は2026-09-12に長い組み込みtable fixtureで実機確認済みです。

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
| `r`、`F5`、`Ctrl+R`、再読込ボタン | 今のアドレスを取得し直す。scroll位置は保つ。cacheが新鮮でも使わず、`ETag`があれば再検証する。POST結果の上では再送確認を出す（下記） |
| `R`（Shift+R） | 強制再読込。cacheを見ずに、ページと画像を必ず転送し直す |
| `y`／`Enter`、`n`／`Escape`、status行の`Send (y)`／`Cancel (n)` | POST送信確認が出ている間だけ、送る／送らない。確認中は`Ctrl+Q`以外のキーを受け付けず、それ以外の場所のタップは「送らない」として扱ってからそのタップを処理する |
| `Escape`、中止ボタン | 読み込み中は中止／アドレス欄が開いていれば閉じる／それ以外はリンク選択とstatusのメッセージを解除。**終了しません** |
| 南京錠のタップ | セキュリティ状態の文言をstatus行へ |
| Wi-Fiのタップ | Network settingsミニを開く。戻ると同じBrowser状態を再描画 |
| M（URL欄・text input・textareaの編集中以外）／F3 | Launcherを開く |
| Batteryのタップ | Battery detailsミニを開く |
| `i`、`F1` | ヒープ・ソケット・履歴・ページの大きさをstatus行とUARTへ（下記） |
| `↑` `↓` | 20 pixelスクロール |
| `PageUp` `PageDown` `Space` | 1画面スクロール（20 pixel重ねる）。checkbox／radioにfocusがあるときの`Space`はそのcontrolを切り替える |
| `Home` `End` | 文書の先頭／末尾 |
| `←` `→` `Home` `End` `Delete` | アドレス編集中のカーソル移動と削除 |
| `Enter`、`↑` `↓`、`Escape`、`Tab` | textarea編集中は改行の挿入、表示行の上下移動、編集終了、次のcontrolへ。`Enter`で送信はしない |
| `↑` `↓` `PageUp` `PageDown` `Home` `End`、`Enter`／`Space`、`Escape`、`Tab` | select一覧が開いている間は候補の移動、選択（multipleは切替）、閉じる、閉じて次のcontrolへ。一覧の外のtapは閉じるだけ |
| タッチ／クリック | リンクの選択と移動 |
| ホイール | Browser content上だけ1 detent 60 pixelスクロール |

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

### ページ内fragment

URLの`#`以降はページ内位置です。同じ文書へのfragment付き新規遷移は、通信や
ローカルファイルの再読出しをせず、現在の文書とlayoutを使います。すべての要素の
`id`と互換用の`a[name]`をanchorとして扱い、`%HH`は検索時だけUTF-8へ復号します。
空fragment `#`と、対象が無い場合の`top`（ASCII大小文字を無視）は文書先頭です。
対象が無ければURLと履歴は更新し、位置を変えず`fragment not found`を表示します。

履歴項目は訪問URLと、その項目を離れる直前のviewportのpixel offsetを持ちます。同じ文書の
戻る／進むは再読込もanchor再検索もせず、項目に保存した行へ戻ります。一方、新規の
fragmentなしURL（`href=""`を含む）と明示的な再読込は従来どおり文書を読み直します。
redirectの`Location`にfragmentが無ければ直前のfragmentを引き継ぎ、明示した`#`は
空fragmentとして引き継ぎを止めます。HTTP request targetへfragmentは送りません。

この経路は2026-09-11に、各anchor間を複数画面分にした組み込み
`http://built-in/fragments`で実機確認済みです。

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
アドレスの途中を直すこともできます。`Ctrl+A`は全選択し、次の入力または削除で置換します。
編集状態はformでも再利用する共通部品で、UTF-8境界を守るcaret・選択・byte上限、単一行／
複数行、確定本文とは別の未確定文字列を持ちます。caret、選択背景、横scrollは比例幅の
pixel測定で描画します。現在の物理キーadapterがアドレス欄へ渡す文字はASCIIだけで、長さは
`MAX_URL_BYTES`までです。共通部品がUTF-8を保持できること自体は日本語入力手段を意味しません。
編集中のキー操作はtoolbar全体を再描画しません。横scroll位置が変わらないcaret移動は旧・新caret、
選択変更は旧・新選択範囲、文字の追加・削除は変更位置から右端までのdamageだけをpanelへ
writebackします。framebufferの塗りとglyph描画にも同じ横clipを掛けるため、直接scanout中に
damage外が一度白くなることもありません。横scrollが動いた場合だけアドレス欄の32 pixel高矩形全体を更新します。
ボタンと南京錠は状態が変わらない限りpanelへ送り直しません。

### 漏れを見るための`i`

`i`（または`F1`）を押すと、漏れがあれば現れる数値をstatus行に出し、同じ行を
UARTへも書きます。

```text
heap 21592K sockets 1 back 3 fwd 2 post 1/2K+0K cache h4 r1 s3 p0 x0 page 41K peak 13K
```

| 項目 | 意味 |
| --- | --- |
| `heap` | グローバルアロケータが今渡している総量 |
| `sockets` | socket setに入っている数。`+1 loading`が付けば取得が走っている。`-`はstackが無い |
| `back`／`fwd` | 履歴と進む履歴の件数 |
| `post` | 履歴に保持しているPOST結果の件数/その文書の合計KiB＋再送用に保持している要求bodyの合計KiB |
| `cache` | Browserを開いてからのHTTP cacheの回数。`h`は通信せず使った数、`r`は`304`で確認して使った数、`s`は保存した数、`p`は期限切れ・容量不足・読み出し不能で消した数、`x`はPOSTにより無効化した数 |
| `page` | 表示中のページの費用（文書＋layout） |
| `peak` | 直前の取得でparserが一度に持った最大。固定予算との合否判定ではなく、同じ操作の前後差を見る診断値 |

**値ではなく差を見るためのものです。**「同じ操作を20回して、この行が元へ戻るか」
が知りたいことで、`sockets`が戻らないのはとくに厄介です——socket setが尽きるのは
数分後の別の場所で、そこには原因が何も残っていません。

キーにしてあるのは、これがページ遷移ごとに出ていた`BROWSER page`ログの
置き換えだからです。毎回出るとログが読めなくなる上に、知りたいのは
**読み手が選んだ2つの瞬間の差**なので、そもそも毎回出しても答えになりません。

`sockets`はsocket set全体で、この画面の取り分ではありません（DHCPとDNSが
自分のものを持っています）。前後で同じ数かどうかだけが意味を持ちます。

### HTTPキャッシュ

ページと画像のGET応答は、RAMディスクの`/tmp/browser-cache/`へファイルとして保存します。
履歴（戻る・進む、POST結果の保持）は従来どおりメモリにあり、cacheとは別です。`/tmp`は起動ごとに
作り直されるので、cacheはリセットで消えます。Browserを閉じて開き直しても、リセットまでは残ります。

保存形式はURL（fragmentを除きqueryを含む）の64 bit FNV-1a hashを16進16桁の名前にし、先頭1桁の
bucketディレクトリへ置きます。`<hash>.body`が転送符号を外した応答本文、`<hash>.meta`がkey（URL全文）・
`ETag`・media type・charset・本文byte数・保存時刻・期限・最終利用時刻・`no-cache`・接続の安全性を
行ごとに書いたテキストです。hashが衝突した場合はmetaのkeyが違うので不一致として扱います。時刻は
起動からのミリ秒です。RTCは未設定のことがあり、`/tmp`はリセットで消えるため、起動からの時間で
期限を数えられます。

保存するのは、redirectを経ない`200`で、`Cache-Control: no-store`がなく、`Vary`が`Accept-Encoding`
以外を含まず、identity符号で、1 entry 512 KiB（`MAX_HTTP_CACHE_ENTRY_BYTES`）以内の応答だけです。
鮮度は`Cache-Control: max-age`、なければ`Expires`から応答自身の`Date`を引いた時間、どちらも
なければ既定の1時間（`DEFAULT_CACHE_FRESHNESS_SECS`）で、`Age`を差し引きます。受信時点で期限が
切れている応答（`max-age=0`、`Date`より前の`Expires`、日付として読めない`Expires`）は保存しません。
`no-cache`の応答は`ETag`がある場合だけ保存し、毎回`304`で確認してから使います。保存できない応答が
届いた場合は、そのURLの古いentryを消します。cache全体の容量上限はありません。RAMディスクの空きはshellの`df`で
確認できます。

ページを開くと、まず`/tmp/browser-cache`を引きます。期限内のentryは通信せずにファイルから読み、
status行へ`shown from the cache; no request was sent`と表示します。この経路はネットワークがなくても
使えます。`r`・再読込ボタンは期限内でもcacheを直接使わず、`ETag`があれば`If-None-Match`付きで取得し、
`304`ならファイルから読んで`not modified (304): the cached copy is shown`と表示し、`304`の
`max-age`・`Expires`で期限を更新します（なければ保存時の有効時間を今から数え直します）。`R`はcacheを
見ずに取得し、得た`200`は保存します。画像も同じ規則で、ページが`R`で開かれた場合は画像もcacheを
見ません。cacheファイルが読めない、本文の長さがmetaと違う場合はそのentryを消して通常の取得へ戻ります。

期限切れのentryは、引いた時点で消します。加えて、Browserが何も読み込んでいない間、1分ごとに
bucketを1つずつ見て、期限切れのentryと、metaのない本文・本文のないmeta・読めないmetaを消します。
本文の書き込みは表示後に64 KiBずつ進め、書き込み中は次の画像の取得を待たせます。新しいページへ
移るときは書きかけの本文を先に書き終えます。書き込みで容量不足（`no space left on volume`）が
出た場合は、書きかけを消し、期限切れのentryと不整合なファイルを消し、それがなければ最終利用が
最も古いentryを1つ消して、最初から書き直します。消せるものがなくなれば保存を諦めます。どの失敗も
ページの表示を失敗させません。

POSTは保存しません。POSTの応答（redirectの途中を含む各hop）が`2xx`か`3xx`だった場合は、RFC 9111
4.4に従い、そのPOSTを送ったURLと、同じoriginへのredirect先URLのentryを消します。応答headを
受け取った時点で対象を記録するので、その後に中止・失敗したPOSTでも無効化します。削除はcacheの
保守処理の最初に行い、同じURLの本文を書き込み中ならその書き込みも捨てます。`Content-Location`は
見ません。

LAN fixtureは`/cache/index.html`です。`/cache/stats.html`がpathごとの要求数、`If-None-Match`付きの
数、`200`・`304`の数を表示し、`/cache/reset`で0へ戻します。`max-age.html`（60秒）、`expires.html`
（`Date`の30秒後）、`plain.html`（cache関連headerなし、既定1時間）、`no-cache.html`、
`expired.html`（`max-age=0`）、`etag.html`、`no-store.html`、`vary.html`、`vary-encoding.html`、
`large.html`（約700 KiB）、`image.html`、容量不足を起こすための`sized.html?kib=<KiB>&id=<任意>`（indexに
400 KiB×20件、100 KiB×10件のlink）があります。このファイル保存cacheは2026-09-13に実機受入済みです。
`counter.html`は1時間cacheされる値のページで、同じURLへのPOSTと、303で戻るPOSTの2つのformを
持ちます。POST後に戻ると再取得され、値が増えて見えます。このPOST後の無効化は2026-09-13に実機受入済みです。

### システムバーとミニアプリ

`M`（URL欄、およびfocusした時点で編集になるtext input・textareaの編集中以外）または`F3`でLauncherへ入る。text control編集中の`M`を文字として入力し、F3だけがLauncherを開く動作は2026-09-13に実機受入済み。Wi-Fi slotはNetwork settings、
Battery slotはBattery detailsを開く。ミニ中はBrowserのhandlerも取得stepも呼ばない。
Escape／戻る／LauncherのBrowserで同じpage・scroll・history・編集中URLへ戻る。
LauncherのEscape／ハンバーガー再クリックは呼出元へ戻り、Consoleの選択はBrowserを終了する。

進行中のHTTP GETは入る前にsocketを閉じ、URLと履歴操作を保持して復帰時に再開する。POSTは
自動再送せず、復帰後のstatus行に送信確認（未送信なら`POST was not sent`、送信後なら
`POST result unknown`）を出す。送るかどうかは読み手が決める。
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
| `/italic` | 合成斜体。見出し／本文／table／link上のA4字形と、未収録文字の中空枠（16・32 pixel）、run先頭の結合文字 |
| `/table` | caption、header、長短3列、cell内link、rowspan／colspan、空cell、不正span、多列折返し |
| `/fragments` | fragment、anchor、履歴位置、別文書往復の実機確認。各target間は複数画面分 |
| `/forms` | 通常本文／table cellのinline control、装飾button、button内画像失敗、編集・送信 |
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
| `img` | `src`、`alt`、正の`width`／`height`を画像IDへ保持し、PNG／baseline JPEGを取得・decodeする。通常本文では回り込みなしの予約領域、`button`内では内容line高へ縮小したinline画像になる |
| `table`、`caption`、`thead`、`tbody`、`tfoot`、`tr`、`th`、`td` | 本文幅内の表。cell内折返し、見出し背景、罫線、正整数の`rowspan`／`colspan`に対応 |
| `form`、`input`、`select`、`option`、`textarea`、`button`、`label` | GET／urlencoded POST、編集・選択・送信。`textarea`以外のvisible controlは本文とtable cellのinline flowへ参加する |
| `strong`、`b` | 二度打ちで太く見せる |
| `em`、`i` | 行box下端を基準に右へ最大1/5行高ずらす合成斜体（16 pixel行で3 pixel）。未収録文字の中空枠も同じ角度で傾ける。`/italic`で2026-09-13に実機受入済み |
| `code`、`kbd`、`samp`、`tt`、`var` | 緑 |
| `script`、`style` | end tagまで読み飛ばし、内容は表示しない |
| comment、doctype、未知の要素 | 表示しない。既知の子テキストは通常どおり |

未知の要素は**inline扱い**です。blockとして扱う要素名は明示列挙にしてあり
ます。逆にすると`<span>`や`<font>`が文の途中で段落を割ってしまうためで、
取り違えたときの被害が小さい側を既定にしています。

tableは横scrollせず、列の希望幅を本文幅へ縮めてcell内で強制折返しします。
`rowspan`／`colspan`は1〜32で、欠落・空・非数値・`0`は1、上限超過は32です。
`border`は未指定または`0`なら罫線なし、値なし・空または正整数なら罫線ありです。
太さは1〜4 pixelへ丸めます。非数値は罫線なしです。CSS由来の罫線、
`col`／`colgroup`、nested table、`rowspan=0`の特殊意味には対応しません。
cell内で画像と文章が混在する場合は、文章区間と画像を文書順に縦へ積み、画像のalt用
代替文字列は画像表示時の本文から除きます。複数画像とrowspanでも画像高や文章が重ならない
高さを先に測ります。nested tableの罫線・独立した列幅計算は引き続き対象外ですが、その中身は
外側cellから出さず、内側の行ごとに改行し、同じ行のcellを` | `で区切るcompact textとして
平坦化します。
このtable描画経路はhost testとrelease buildに加え、2026-09-11に
`http://built-in/table`とfixture serverを使ってTab5実機で確認済みです。
その後Stage 1のpixel scroll受入用に、組み込み`table`ページを18行の長い表へ
拡張し、2026-09-12に上下端clipとpixel scrollも実機で受入済みです。

画像予約領域は本文、link内、文章と混在するtable cell内に置けます。link画像は枠全体が
選択・touch対象です。組み込み`http://built-in/images`に寸法指定4種類、幅縮小、
link、table cellの受入項目をまとめています。このStage 2表示はhost test済みですが、
2026-09-12にtable cell以外を実機確認済みです。cellでは画像の希望幅も列幅計算へ加え、
tableに余裕があれば指定300×120 pixelを維持します。全列が収まらない場合だけ比率を
保って縮小し、4 pixelのpadding内へ収め、row外枠が画像と上下paddingを包みます。
余裕があるのにalt文字幅まで縮んだ問題の修正を含め、2026-09-12に実機受入済みです。

`file:`文書から相対参照したPNGは
文書表示後に1件ずつ最大512 KiBまで読み、白背景へalpha合成して予約領域へ描画します。
`tools/make_browser_image_fixture.py <mounted-directory>`で隣接する`stage3.html`と
`stage3.png`を作れます。このlocal取得・decode・scroll・clipは2026-09-12に実機受入済みです。
HTTP(S)文書の`image/png`も文書表示後に同じ上限とdecoderで1件ずつ取得します。同一URLは
RGB565を共有します。LAN fixtureは`/images/stage3.html`です。複雑なtest cardを異なる2寸法で
表示するHTTP画像経路は2026-09-12に実機受入済みです。
JPEGはSOF0、8-bit、1または3 componentのbaselineだけを`image/jpeg`から取得し、同じRGB565
描画経路へ接続します。progressiveや他variantは画像単位で拒否します。このJPEG経路は
host testとrelease buildに加え、2026-09-12に実機受入済みです。
PNGはnon-interlacedで、1 sampleが8 bit以下の定義済み形式をすべてdecodeします。グレースケールと
パレットは1／2／4／8 bit、RGB・グレースケール＋α・RGBAは8 bitです。`tRNS`によるパレットごとの
αと、グレースケール／RGBの透明色1色も白背景へ合成します。16 bit sampleとAdam7 interlaceは
非対応variantです。仕様にないcolor typeとbit深度の組み合わせ、`PLTE`を欠くパレット画像、
bit深度で表せる数を超える`PLTE`、パレット外のindex、`PLTE`より前やパレットより長い`tRNS`は
破損として扱います。グレースケール／RGBに付いた`PLTE`とα付き形式の`tRNS`は、画素を変えない
ので無視します。形式ごとの表示はLAN fixtureの`/images/png-formats.html`で確認できます。この低bit深度・パレット・
`tRNS`対応は、同fixtureとGitHub上の1-bitパレットPNGで2026-09-13に実機受入済みです。
PNG chunkのCRC-32不一致、非対応variant、破損、上限超過、OOM、取得失敗は画像枠内へ
理由を表示し、文書と他画像は表示を続けます。CRC不一致の局所失敗表示は
2026-09-12に実機受入済みです。
画像のdecode後は未指定寸法をintrinsic寸法・縦横比で置き換えて再layoutします。その際は
viewport先頭の論理的な文章位置と選択中linkを維持します。5秒のHTTP idle timeoutを避けて
2秒ごとに届く遅延画像を使い、2026-09-12に実機受入済みです。
親文書がPinned TLSなら画像もpin登録済みHTTPSだけを許可し、未認証接続へのidentity downgradeを
画像取得開始前に拒否します。上限近くの640×400 RGBA fixtureも用意しています。
640×400画像のdecode・clip、遷移中止、Wi-Fi／system bar復帰は2026-09-12に実機受入済みです。
Pinned TLSから未認証画像への拒否は2026-09-13に実機受入済みです。この確認用にLAN fixtureの`/images/pinned.html`を
用意しています（`no-store`で、常に実際にTLSで取得されます）。確認手順は次のとおりです。

1. `tools/pins/fixture_pins.txt`のLANアドレスの2行を、fixture serverを動かすPCのIPv4へ書き換え、
   `tools/pins/generate.py`を実行する（同じアドレスなら変更不要）
2. `cargo run --release --features tls-fixture-pins`でpin入りbuildを書き込む
3. PCで`python3 tools/browser_fixture_server.py --pinned-board`を起動する
4. `browser https://<PCのIPv4>:8443/images/pinned.html`を開き、南京錠が`TLS PINNED`であることを確認する

| 画像 | 期待 |
| --- | --- |
| 1. 同じpin済みhostのHTTPS | 表示される |
| 2. 同じhostの平文HTTP | `HTTPS downgrade refused` |
| 3. pinの無いhost（`tls-unpinned.invalid`） | `TLS identity downgrade refused`（通信しない） |
| 4. 同じhostのEd25519 listener（pinと鍵が違う） | 接続失敗の表示で、画像は出ない |

pinを持たない通常buildでは南京錠が`TLS UNVERIFIED`になり、3は名前解決の失敗になるので、この確認には
使えません。確認後は`cargo run --release`で通常buildへ戻してください。fixture pinは通常buildに
入れてはならず、`tools/pins/generate.py --check-release`で確認できます。

フォームcontrolは本文のinline objectです。text inputと`select`は希望幅320 pixel、
submit／`button`は内容幅＋左右各6 pixel（80〜320 pixel）、checkbox／radioは1行高の正方形で、
現在行へ収まれば前後の本文と同じ行に置きます。収まらないときだけcontrol直前で折り返し、
狭い配置先ではその全幅へ縮めます。sourceにある空白だけを通常どおり1個へ畳み、layout自身は
control前後へ空白を足しません。`textarea`だけは複数行編集のため独立blockを維持します。
hiddenは文書順と送信値を持ちますが表示領域を持ちません。このinline配置はhost testとrelease buildに
加え、2026-09-13に実機受入済みです。
disabledは薄い枠と文字で表示します。組み込み`http://built-in/forms`は複数viewport分の本文を
control群の前後に置き、上下端でのclip、前後の本文との非重なり、初期値、hidden非表示、
disabled、submit外観をスクロールしながら確認できます。上端を越えたcontrolも元の文書座標を
保って描画し、viewport内へbox全体を押し戻さず、
framebufferのvertical clipで見えている部分だけを残します。この上端clipは2026-09-13に
実機受入済みです。Tab順序はlinkと有効なvisible controlをlayout位置の文書順に統合し、
hiddenとdisabledを飛ばします。controlのfocusは青い枠（submitは青い面）で示します。
checkbox／radioだけはline box全体の外枠を持たず、実際の四角／円から2 pixel空けた青い二重ringで
focusを示します。Tab移動時は
対象全体がviewportへ入る位置まで自動scrollします。focus順序・表示・自動scrollは
2026-09-13に実機受入済みです。focus移動だけなら旧対象と新対象の矩形をclipに設定して通常の
viewport rendererを再利用し、viewport全体の背景消去とwritebackを行いません。この局所再描画は
2026-09-13に実機受入済みです。入力イベントが描画より先に複数届く場合も、最大4対象のfocus
damageを上書きせず蓄積します。それを超える場合だけviewport全体を再描画し、過去のfocus表示を
残しません。この複数focus damage処理は2026-09-13に実機受入済みです。text controlはTabでfocusした時点、
またはtouchで共通
`TextInput`の単一行編集を開始し、Escapeで現在値を保持して編集を終了、Tabで保持して次の
focusへ進みます。q/r/[ / ]を含む印字可能ASCIIは編集中はBrowser shortcutではなく文字として
扱います。caret移動は旧新caret、
選択変更は旧新選択、挿入・削除は変更位置以右だけをhorizontal clipして再描画します。横scrollが
変わるときだけcontrolのtext内側全幅を更新します。Tabから直接編集へ入る操作に加え、その他の
編集キーと編集時の局所再描画も2026-09-13に実機受入済みです。単一行text編集中の
Enterは現在値を
保持してformを暗黙送信し、submitはEnterまたはtouchで送信します。GET送信は現在のtext値、
hidden、実際に起動したsubmitだけを文書順に集め、disabledと空nameを除外します。組み込みfixtureは
`/forms`自身へ送信し、同名`q`、空の`empty`、submitの`go`をaddress欄で確認できます。このGETの
UI接続は2026-09-13に実機受入済みです。`button`は既定でsubmit controlとなり、`value`属性を
送信値、要素内の空白を畳んだtextを表示ラベルとして別々に保持します。表示ラベルでは
`strong`／`b`、`em`／`i`、`code`系の既存装飾とPNG／JPEG画像を文書順の1行で描画します。
block tagは改行を作らず、`a`／`label`はinteractionを作りません。nested controlは要素ごと捨てます。
画像は通常画像と同じ取得・decode・cache・再layoutを使い、button内のline高と内容幅へ縦横比を保って
縮小します。画像自身はfocus／link／hit targetにならず、その矩形も外側buttonのtapになります。
内容が320 pixelを越す部分はbutton内でclipします。この装飾文字・画像表示はhost testとrelease buildに
加え、2026-09-13に成功・遅延・破損画像を含めて実機受入済みです。組み込みfixtureの
`Apply changes`は`mode=advanced`を送り、起動していない`go=Search`は送りません。このbutton接続は
2026-09-13に実機受入済みです。

画像取得まで含む受入fixtureは`tools/browser_fixture_server.py`の
`/forms/inline-controls.html`です。通常／装飾button、成功するchecker、2秒間隔で届くslow PNG、
CRCを壊したPNG、table cell内button、および独立画像の前後にあるinline controlを1ページで確認できます。

明示的な`label for=id`はparse完了時に対応するcontrol IDへ解決します。label文字のlayout範囲を
tapすると、disabledでない対応controlへfocusし、textなら直接編集を開始します。対応先が後で
宣言されるlabelも扱い、対応先なしとdisabledは操作しません。このlabel操作は2026-09-13に
実機受入済みです。

table cellは自身に含まれるtext・image・controlの文書範囲を保持します。textとcontrolは通常本文と
同じinline配置器をcellの4 pixel padding内で使い、収まれば同じ行、収まらなければcontrol直前で
折り返します。button内画像は独立画像として二重計上しません。row高は確定したcell幅でinline flowを
測ってから決めるため、後続textや隣接cell、cell枠と重なりません。独立した通常画像を含むcellでは、
その画像を従来どおり文書順の独立行に置き、画像の前後のtextとcontrolはそれぞれ共通inline配置器で
組みます。cell内controlも通常と同じTab順序、
label touch、編集、GET送信を使います。組み込みfixtureの`Cell query`は`cell=inside+cell`として
送信されます。通常のtable cellにおけるinline配置とcontrol操作は2026-09-13に実機受入済みです。
その後に追加した、独立画像を挟むcellの前後をinline配置する経路も2026-09-13に実機受入済みです。

`input`の省略type、未知のtype、および専用UIをまだ実装していないtypeはHTMLのfallbackに合わせて
Text stateとして扱い、通常の単一行text controlとして編集・GET送信します。組み込みfixtureの
`Date fallback`は`type=date`ですが、現在はtextとして表示し、`when=現在値`を送信します。
このinput type fallbackは2026-09-13に実機受入済みです。

`input type=checkbox`と`type=radio`は、1行分の高さの正方形boxとしてinline flowに置きます。
通常時はline box全体の四角い外枠や塗り背景を描かず、checkboxは小さい四角の枠と、checked時の
塗りつぶし、radioは円と、checked時の内側の円で表示し、disabledは薄い色です。checkedness初期値は
`checked`属性で、同じform ownerかつ同じ空でない`name`のradio groupでは、文書順で最後の
`checked`だけが残ります。`value`属性が無ければ`on`を送ります。

checkbox／radioもTab順序に入り、Enter、focus中の`Space`、boxのtap、`label for`の文字のtapで
操作します。checkboxは反転し、radioは自身をcheckedにして同じgroupの他を外します。checked
なradioを再度操作しても変わりません。表示中pageが持つchecked状態だけを変え、再描画は状態が
変わったcontrolの矩形に限ります。送信時はcheckedでdisabledでないものだけを`name=value`として
文書順に含めます。focus中はcheckboxなら四角、radioなら円に沿う青い二重ringを記号の外側へ描き、
status行にもfocus中の種類とchecked状態を表示します。このfocus ringの外観は2026-09-13に
実機受入済みです。

組み込み`http://built-in/forms`のmain GET formへ、checked checkbox、未checked checkbox、
checkedかつdisabled、`value`なし、2つとも`checked`を持つradio groupを追加しました。LAN fixtureの
`/forms/post.html`には`Send checkable POST`を追加し、初期状態のbodyは`pick=alpha&choice=one&go=Checks`
です。このcheckbox／radio対応は2026-09-13に実機受入済みです。

`textarea`は複数行のtext controlです。内容は開始tagから終了tagまでをmarkupとして解釈しない
raw textとして読み、文字参照だけを復号します。開始tag直後の改行1つを除き、CRLFとCRはLFへ
揃えて初期値にします。文書本文には出しません。`rows`は正の整数だけを採用し、既定2、最大8行です。
boxは通常のtext controlと同じ幅（最大320 pixel）で、高さは行数分です。初期値が
`MAX_INPUT_VALUE_BYTES`（4 KiB）を超えるtextareaは収まった分だけを表示し、編集を拒否し、その
formの送信も`a textarea's initial text is too long to submit`として拒否します。切り詰めた値は
送りません。

Tab、tap、`label for`で共通`TextInput`の複数行編集を始めます。文字幅で折り返した表示行を持ち、
`Enter`は改行の挿入、`↑`／`↓`は表示行の移動（先頭行の上は文頭、最終行の下は文末）、`Home`／
`End`は改行で区切った行の先頭／末尾、`Escape`は値を保持して終了、`Tab`は保持して次へ進みます。
折り返しは語単位ではなく文字単位です。caretの行がboxの外へ出ると、box内の表示を行単位で
scrollします。編集中の変更はtextareaのtext領域全体だけを再描画します。送信時は改行をCRLFとして
`%0D%0A`で符号化します。この改行の正規化はform送信のすべての名前と値に適用します。

組み込み`http://built-in/forms`のmain GET formへ3行の`Note (textarea)`を追加しました。初期状態の
`Search`のqueryは下記のselect追加後の値を参照してください。
続く`Textarea scrolling`は2行boxに4行（最後は折り返す長い行）を持ち、`Send long`で送信します。
LAN fixtureの`/forms/post.html`の`Send textarea POST`は、初期状態でbody
`text=line+one%0D%0Aline+two&go=Text`を送ります。このtextarea対応は2026-09-13に実機受入済みです。

`select`は1行分のboxに選択中optionのlabel（multipleは`, `区切り、なければ`(none)`）と右端の▼を
表示します。optionのlabelは要素内の文字を空白を畳んで持ち、`value`属性が無ければlabelを値に
します。select内のoption以外の文字やtagは表示しません。`optgroup`のlabelは表示せず、
`disabled`だけを子のoptionへ継承します。単一選択では文書順で最後の`selected`だけを残し、
`selected`が無ければ最初の有効なoptionを選びます。`multiple`はその調整をしません。optionは
1文書で`MAX_SELECT_OPTIONS`（4096）件、labelは`MAX_INPUT_VALUE_BYTES`までで、超えるとページを
上限エラーにします。

Tab順序に入り、Enter、focus中の`Space`、boxや`label for`のtapで一覧を開きます。一覧はboxの下
（入らなければ上、どちらも入らなければviewport内）に最大10行で重ねて描き、それ以上は一覧内を
scrollし、右端に位置のbarを出します。単一選択は選択を丸、multipleは四角で示します。上下キー等で
強調行を動かし、単一選択はEnter／Space／行のtapで選んで閉じ、multipleは切り替えて開いたままに
します。disabledのoptionは選べず、一覧を開いたまま`this option is disabled`を表示します。Escape、Tab、一覧外のtap、ホイール、page scrollで閉じます。
一覧の開閉と移動は一覧の矩形だけをviewport rendererで再描画し、選択の変更はboxも再描画します。
送信時は選択中かつ有効なoptionごとに`name=value`をoption順で含めます。

組み込み`http://built-in/forms`のmain GET formへ、value省略とdisabled optionを含む`Color (select)`、
disabled optgroupを含む`Tags (multiple)`を追加しました。初期状態の`Search`のqueryは
`q=initial+value&q=hidden+duplicate&empty=&cell=inside+cell&when=2026-09-13&topic=news&size=large&
note=first+line%0D%0Asecond+%26+%3Cb%3Eline%3C%2Fb%3E&color=Green&tag=a&go=Search`です。
`Textarea scrolling` formには12件の`Number`を追加し、初期状態では`number=1`を送ります。LAN fixtureの
`/forms/post.html`の`Send select POST`は、初期状態でbody`fruit=pear&many=x&many=y&go=Select`を
送ります。このselect対応は2026-09-13に実機受入済みです。

Stage 5時点ではPOSTを送信前に拒否する経路を2026-09-13に実機受入済みとしました。Stage 6以降はnetwork HTTP(S) actionに対する`application/x-www-form-urlencoded` POSTを送信できます。
method・URL・bodyは1つの要求値として保持し、固定header、正確な`Content-Length`、48 KiBの
encoded body上限を適用します。送信後の通信断、中止、system bar中断では自動再送せず、結果不明を
statusへ表示します。301/302/303はGETへ変えてbodyを破棄し、307/308は同一originならPOST bodyを
維持します。LAN fixtureは`/forms/post.html`で、応答ページにmethod、POST回数、action query、
content-type、content-length、bodyを表示します。この通常POST UI経路は
2026-09-13に実機受入済みです。

POSTを読み手の明示操作なしに再送する経路はありません。再送が必要になりうる場面では、status行に
送信先のscheme・host・portを示した確認を出し、`y`／`Enter`／`Send (y)`でだけ送ります。
`n`／`Escape`／`Cancel (n)`、または他の場所のタップでは送らず、`POST was not resent`を表示します。

| 場面 | 確認文 | 送らない場合 |
| --- | --- | --- |
| Wi-Fi断、timeout等の失敗、system bar中断で要求を送る前に止まった | `POST was not sent. Send to …?` | 元pageと入力値のまま |
| 同じく送信開始後に止まり、応答を読めていない、または応答途中で切れた | `POST result unknown. Resend to …?` | 同上 |
| 307/308が別originへbodyを送り直すよう求めた | `Server redirects the POST body to …?` | 送信元のpageのまま。source側の1回だけが届いている |
| POST結果の上でreload、または保持していない結果へback/forward | `Resend POST to …?` | 表示中pageと履歴を変えない |

中止ボタン／`Escape`で読み手が自分で止めた場合と、応答を受け取ったうえで表示できなかった場合
（HTMLでない、上限超過など）は確認を出さず、理由だけを表示します。確認への「送る」は
新しい要求として扱い、別origin redirectの確認後も、それまでに辿ったredirect回数を引き継ぎます。
HTTPS→HTTP、Pinned TLSから未pin host、`file:`へのredirectは確認を出す前に従来どおり拒否します。

POST結果（redirectでGETに変わらなかったもの）を離れると、その文書を履歴項目へ保持します。
back/forwardで戻るときは通信せずに保持した文書から再layoutし、`POST result shown from memory;
nothing was resent`と表示します。画像は通常どおりGETで取り直します。保持するのはparse済み
Documentだけで、HTTP cacheとは別の予算です。

| 保持対象 | 上限 |
| --- | ---: |
| POST結果の文書 | 2件、合計320 KiB（`MAX_RETAINED_POST_RESULTS`／`MAX_RETAINED_POST_RESULT_BYTES`） |
| 再送用の要求body | 合計96 KiB（`MAX_RETAINED_POST_REQUEST_BYTES`） |

1件で320 KiBを超える結果、`Cache-Control: no-store`付きの結果は保持しません。予算を超えると
表示中pageから遠い履歴項目から先に文書を、次に要求bodyを捨てます。文書が無い項目へ戻ると
再送確認になり、要求も無ければformがあったpageへGETで戻ります。どちらも無ければ移動せず
理由を表示します。同じPOST結果内のfragment移動は従来どおり通信しません。表示中pageと
入力値は予算のために捨てません。

LAN fixtureの`/forms/post.html`には、`Cache-Control: no-store`を返す`Send no-store POST`と、
保持予算を超える約400 KiBの結果を返す`Send large POST`を追加しました。どちらも結果から
`form again`で離れて戻ると再送確認になり、応答のPOST回数で再送の有無を確認できます。
これらの確認UI、保持と復元、no-store、予算超過は2026-09-13に実機受入済みです。

POST redirectの確認には`/forms/post-redirects.html`を使います。301/302/303/307/308ごとに
source POST回数と最終要求回数を独立して数え、最終method・query・Content-Type・
Content-Length・bodyを応答本文へ表示します。同じ画面の`Try cross-origin 307`／`308`は
plaintext listenerからTLS listener（既定8443）へ307/308を返し、別originへの送信確認を出させます。
TLS listenerから開いた場合はHTTPへの降格として`https-downgrade`になります。カウンタを持つ
source/result endpointはmanifest自動巡回の対象外です。このredirect UI経路（別originの確認を含む）は
2026-09-13に実機受入済みです。

組み込みfixtureの`Try POST`はnetwork actionではないため、現在は
`POST needs a network HTTP(S) action`と表示して送信しません。form/windowの`target`は保持・適用せず、
このBrowserの単一文書navigationだけを扱います。

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
| 文書item（block＋run＋table構造） | 16,384件 |
| 有効なlink | 最大4,096件、解決済みURL合計2 MiB。以後は本文を残して非link化 |
| 折返し後の行 | 32,768行 |
| list／inlineの深さ | 32 |
| 1要素で処理する属性 | 16個 |
| tableの列数、`rowspan`／`colspan` | 32 |

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
| `item-limit`／`line-limit` | 文書・行の上限 |
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
