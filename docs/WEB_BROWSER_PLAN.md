# 簡易Webブラウザ実装計画

> 索引: [`../DESIGN.md`](../DESIGN.md)
> この文書は作業計画です。現在の実装仕様は現状文書とコードを優先してください。

## 状態: **完了**

| Stage | 内容 | 状態 |
| --- | --- | --- |
| 0 | 対応範囲、上限値、試験素材、計測基準の固定 | 完了 |
| 1 | HTTP URLの解析と相対参照の解決 | 完了 |
| 2 | 中断可能なHTTPトランザクション | 完了（実機確認済み） |
| 3 | ストリーミングHTML解析と文書モデル | 完了（実機確認済み） |
| 4 | ローカル文書による表示、スクロール、リンク操作 | 完了（実機確認済み） |
| 5 | ネットワーク統合、遷移、戻る、キャンセル | 完了（実機確認済み） |
| 6 | 上限検査、異常系、メモリ・表示・通信の実機受入 | 完了（実機受入済み） |
| 7 | 現状文書の更新 | 完了 |

現在の実装仕様は[`BROWSER.md`](BROWSER.md)を参照してください。

この計画でいう「ブラウザ」は、一般的なHTML/CSS/JavaScriptブラウザではなく、
**HTTPで取得したHTMLから文章とリンクを取り出し、画面幅に折り返して閲覧する
ハイパーテキストビューア**である。最初から対象を狭く固定し、対象外の内容を
推測して実行したり、無制限に保持したりしない。

## 背景と前提

既存実装には、ブラウザの土台として次がある。

- ESP32-C6経由の2.4 GHz Wi-Fiと、smoltcpによるIPv4、DHCP、AレコードのDNS、TCP
- `src/net/http.rs`の最小HTTP/1.0 GET。応答ヘッダは最大4 KiB、TCPの送受信
  バッファは各8 KiBで、本文を512 byteずつsinkへ渡す
- 論理1280×720のRGB565シングルフレームバッファと、矩形・線・5×7 ASCII文字の描画
- CardKB、Tab5 Keyboard、USBキーボード、USBマウス、タッチをまとめる`InputManager`
- 32 MiB PSRAM内の固定フレームバッファ、固定8 MiB RAMディスク、残り約22.24 MiBの
  グローバルヒープ

現在のPSRAM配置は次のとおりである。

```text
0x48000000..0x481c2000  framebuffer  1,843,200 byte
0x481c2000..0x489c2000  /tmp RAM disk 8 MiB
0x489c2000..0x4a000000  heap          23,322,624 byte（約22.24 MiB）
```

`DESIGN.md`の制約欄にはフレームバッファ以外の約30.24 MiBをヒープとする古い要約が
残っているが、実装と[`PSRAM.md`](PSRAM.md)の上記3分割が正しい。Stage 0で要約を
現状へ合わせ、以後の見積もりは22.24 MiBを基準にする。

内部RAMは256 KiBの共通安全範囲しかなく、通常コードはFlash XIP、動的な文書データは
PSRAMへ置く。大きい固定配列やページ依存データを`.bss`へ置かない。

## 到達目標

- `browser`コマンドで全画面ブラウザを開く
- ASCIIでHTTP URLを入力し、既存のDNSとIPv4/TCPを使ってページを取得する
- UTF-8のHTMLをストリーミング解析し、対応要素の文章を画面幅へ折り返して表示する
- キーボード、USBマウス、タッチでスクロールとリンク選択ができる
- 同一ページ内fragmentを除く相対リンクと絶対HTTPリンクへ移動できる
- 直前8ページまで戻れる
- 読み込み中でもEscapeでキャンセルでき、リンク切断時も画面と入力が固まらない
- リダイレクトを最大5回まで追跡し、HTTPSへの遷移は未対応であることを表示する
- ページが上限を超えた場合は、それまでの不完全な内容を成功扱いせず、理由を表示する
- ページ遷移を繰り返してもヒープ使用量が単調増加せず、表示DMA underrunを増やさない

## 初期版の非目標

- TLS／HTTPS
- IPv6、AAAA、mDNS、WebSocket、HTTP/2、HTTP/3
- CSS。`style`属性、`<style>`、外部stylesheetはいずれも適用しない
- JavaScript、DOM API、イベントスクリプト。`<script>`の内容は表示せず読み飛ばす
- PNG、JPEG、GIF、WebP、SVGなどの画像デコード。`img`は`alt`だけを本文として扱う
- form送信、password入力、cookie、認証、local storage、session storage
- 音声、動画、canvas、iframe、埋め込みオブジェクト
- ダウンロード、ページキャッシュ、閲覧履歴の永続化、bookmarkの永続化
- 日本語フォントと日本語入力。非ASCII文字は初期版では代替glyphで表示する
- HTML5の完全なtree construction、CSS box model、pixel単位での互換表示

HTTPは平文なので、初期版は認証情報を送る機能を持たない。アドレス欄には常に
`INSECURE HTTP`を表示し、HTTPSへ自動的にdowngradeしない。

## 対応するHTML

| 要素 | 初期版の扱い |
| --- | --- |
| `title` | タイトルバーへ表示。本文には入れない |
| `h1`〜`h6` | 前後改行を持つ見出し。初期版では2段階程度の固定文字倍率へ丸める |
| `p`、`div`、`section`、`article` | 前後を段落境界にする |
| `br` | 強制改行 |
| `pre` | ASCII空白と改行を保持し、画面幅では安全のため折り返す |
| `a href` | 本文とリンク先を保持し、選択・クリック対象にする |
| `ul`、`ol`、`li` | 固定のmarkerと字下げで表示する。深さは上限で打ち切る |
| `hr` | 1本の水平線として表示する |
| `img alt` | `alt`があれば`[alt text]`、無ければ`[image]`と表示する |
| `strong`、`b`、`em`、`i`、`code` | 色または固定の文字属性へ丸める。字体は増やさない |
| `script`、`style` | end tagまでraw textとして読み飛ばす |
| comment、doctype、未知の要素 | 表示せず、既知の子テキストは通常どおり処理する |

文字参照は数値参照（10進・16進）と、表示に必要な小さい固定表
（`amp`、`lt`、`gt`、`quot`、`apos`、`nbsp`）だけを扱う。一般のnamed entity表は
初期版に入れない。未対応の参照は入力を失わない形でそのまま表示する。

HTMLは壊れていることを通常ケースとして扱う。再帰下降で木を作らず、未終了タグ、
不正UTF-8、未知の属性を読み飛ばして次のテキストへ復帰する。ただし上限超過は
「読み飛ばして成功」にはせず、明示的なエラーにする。

## メモリ方針と上限

ページの生HTML、DOM、layout tree、画面全体のbitmapを同時に持つ方式は採らない。
HTTP本文は小さい入力chunkから直接tokenizerへ渡し、必要なテキスト、リンクURL、
block境界だけをcompactな文書モデルへ変換する。配置後も画面外をbitmap化せず、
viewportと交差する行だけ描く。

初期値は次で固定し、実機結果を変更理由とともにこの計画へ記録する。

| 項目 | 上限 |
| --- | ---: |
| HTTP応答ヘッダ | 4 KiB（既存値を維持） |
| URL 1件 | 2,048 byte |
| redirect | 5回 |
| 履歴 | 8ページ |
| decode済みHTML入力 | 2 MiB |
| 保持する表示テキスト | 1 MiB |
| 文書item | 16,384件 |
| link | 最大4,096件、解決済みURL合計2 MiB。超過分は非link化 |
| list／inline状態の深さ | 32 |
| 1要素で処理する属性 | 16個。必要な属性以外は値を保持しない |

`Vec`や`String`の通常の拡張でOOM abortへ入らないよう、入力依存の拡張は
`try_reserve`／`try_reserve_exact`を通す。文書、文字列、link、layout itemは
ページ単位の所有物へまとめ、遷移時に一括dropする。古いページは履歴にDOMを残さず、
URLとscroll位置だけを残すため、戻る操作は再取得になる。

各ページの統計として、受信byte、表示テキストbyte、item数、link数、推定所有byte、
最大token長、描画時間を保持し、UARTへ1行で出せるようにする。グローバルアロケータの
全使用量を推測値で報告せず、ブラウザ自身が所有するallocation capacityの合計を
独立して数える。

## URLの契約

`browser::url::Url`は、少なくともscheme、ASCII host、port、path＋query、fragmentを
分けて持つ。ネットワークへ渡すrequest targetからfragmentを外す。

- 初期版で接続できるschemeは`http`だけ
- `https`はURLとして認識するが、接続せず`HTTPS is not supported`を表示する
- その他のscheme、userinfo、IPv6 literalは未対応として拒否する
- hostはASCII DNS名またはIPv4。Unicode hostのIDNA変換は行わず、punycode表記は入力可
- pathが空なら`/`とする。queryは保持し、fragmentは画面内移動が未実装なら無視したと示す
- 絶対URL、scheme-relative、root-relative、path-relative、`?query`、`#fragment`を区別する
- `.`と`..`は相対参照の解決時に正規化し、percent-encodedな`%2e`を`.`と同一視しない
- host、request target、header valueにCTL、空白、CR、LFを許さない
- 表示URLと実際に接続するhost／portは同じ`Url`から生成し、別々の文字列を正としない

## HTTPの契約

既存の`net::http::get`は取得完了まで戻らないため、ブラウザ画面の主経路には使わない。
`src/net/http.rs`に、ソケットを`SocketSet`へ登録したまま短い`poll`を繰り返せる
`Transaction`を追加する。`Transaction`は`Stack`や`Rpc`を所有・長期borrowせず、各pollで
短時間借りる。ブラウザ終了、cancel、link切断の全経路でsocketをabortしてhandleを
`SocketSet`からremoveする。

既存`httpget`は回帰を避けるため残す。可能なら同期`get`を`Transaction`の薄いwrapperへ
移し、HTTPヘッダ解析と終了処理を二重実装しない。

最低限解釈する応答情報は次とする。

- status code
- `Content-Type`と`charset`
- `Content-Length`
- `Transfer-Encoding: chunked`
- `Location`
- `Content-Encoding`

`Content-Encoding`は初期版では`identity`だけを許可し、gzip等は未対応表示にする。
要求に`Accept-Encoding: identity`を明記する。chunkedはchunk size、拡張、terminal chunk、
trailer終端を増分解析し、decode後byte数へ2 MiB上限を適用する。`Content-Length`が上限を
超える場合は本文を読む前に拒否し、長さが無い応答も受信済み量が上限へ達した時点で
abortする。

2xxだけをHTML表示の成功とする。3xxは`Location`を現在URLに対して解決して最大5回まで
追い、循環または上限を明示する。4xx／5xxは本文を通常ページとして実行せず、statusと
短いASCII説明をブラウザ自身のエラーページにする。

## 所有関係とforeground loop

ブラウザは既存の全画面アプリと同じく`app::run`から呼ばれるが、ネットワークを使うため
次の所有関係にする。

```text
app::run
  ├─ Display / Framebuffer
  ├─ InputManager
  ├─ Option<wifi::Rpc>
  ├─ Option<net::Stack>
  └─ app::browser::run(
       &mut Framebuffer,
       &mut InputManager,
       &mut wifi::Rpc,
       &mut net::Stack,
     )
       ├─ BrowserState
       ├─ net::http::Transaction（実行中だけ）
       └─ Page（表示中の1ページだけ）
```

`browser`は既にstation接続とIPv4設定がある場合だけ開く。Wi-Fi credential入力やDHCP開始を
画面へ持ち込まず、未準備ならシェルで`wificonnect`と`ipconfig dhcp`を行うよう表示する。

ブラウザのloopは全interruptで目を覚ます。非frame wakeでは`input.service_fast`と小さい
ネットワーク処理だけを行い、I2C input保守、通常入力、layout、描画はframe境界で行う。
1回のHTTP／HTML処理量にbyte予算を持たせ、受信が多くても入力確認へ戻る。少なくとも
Escapeは読み込み中に1 frame以内で認識する。

画面を抜ける前に実行中transactionを必ず破棄する。C6 linkが死んだ場合はエラー画面にし、
戻った`app::run`側でも既存のdead-session処理を通して`Rpc`と`Stack`を同時に捨てる。

## 画面と入力

画面は上から固定toolbar、document viewport、固定status lineに分ける。

- toolbar: `INSECURE HTTP`、戻る、アドレス欄、読み込み中／停止表示
- viewport: 現在のscroll位置と交差する行だけを描画
- status line: 選択中リンクのURL、エラー、受信量

初期版の操作は次とする。

| 入力 | 動作 |
| --- | --- |
| Escape | 読み込み中はcancel。idle時はブラウザを終了 |
| Enter | アドレス編集の確定、または選択中linkへ移動 |
| Tab／Shift+Tab | link選択を前後へ移動。Shiftを表せない入力源では前方向だけでもよい |
| Up／Down | 小さい単位でscroll |
| Page Up／Page Down | viewport単位でscroll |
| Home／End | 文書の先頭／末尾 |
| Backspace | アドレス編集時は1文字削除。閲覧時は直前ページへ戻る |
| pointer click／touch | toolbar、link、scroll操作のhit test |
| pointer wheel | 利用可能ならscroll。Boot mouseでwheelが無い場合は必須にしない |

アドレス入力はASCIIだけとし、URLの最大長を越える文字を受け付けない。初期表示では
アドレス欄を選択状態にして、ポインタが無くてもキーボードだけで全操作できるようにする。

USBマウスのカーソルは`win`の実証済み方式（下地を退避して最上位へ描く）を共有する。
同じ実装をcopyせず、`src/app/pointer.rs`等へ汎用部分を分離する。描画順は必ず
「cursorを外す→dirty領域を描く→cursorを載せる→和集合をflush」とする。

文書全体のbitmapは作らない。layout結果は行のy範囲、文字列範囲、色、link IDのような
compact itemとして保持する。scroll位置が変わった場合はviewportを描き直してよいが、
toolbarとstatus lineは変化した矩形だけを更新する。単一フレームバッファなので更新途中が
見える可能性は許容し、display DMA underrunを起こすほど長い全画面CPU描画は許容しない。

## モジュール構成（案）

| 追加／変更 | 責務 |
| --- | --- |
| `src/browser.rs` | ブラウザ非UI部の再export、共通上限とエラー |
| `src/browser/url.rs` | URL解析、検証、相対参照解決、request target生成 |
| `src/browser/html.rs` | 増分UTF-8 decode、HTML tokenizer、対応要素の状態機械 |
| `src/browser/document.rs` | page所有arena、text／item／link、折返しlayout |
| `src/net/http.rs` | 中断可能な`Transaction`、応答head、chunked、body上限 |
| `src/app/browser.rs` | 全画面状態機械、address編集、navigation、描画、入力 |
| `src/app/pointer.rs` | `win`とbrowserで共有するcursorの退避・復元・hit位置 |
| `src/app.rs` | `Outcome::Browser`の遷移、network所有物の受け渡しと復帰 |
| `src/app/shell.rs` | `browser`コマンド、help、未接続時の案内 |
| `tools/browser_fixture_server.py` | LAN上で正常／異常HTTPとHTMLを再現する開発用server |

URL、HTML、documentはnetwork、framebuffer、inputへ依存させない。純粋なbyte列から結果を
作れる層にして、fixtureの分割位置を1 byteずつ変える試験を可能にする。

## 段階分け

### Stage 0: 対応範囲、上限値、試験素材、計測基準の固定

- 上記のHTML表、URL契約、メモリ上限を定数名とともに確定する
- `DESIGN.md`のPSRAMヒープ要約を約22.24 MiBへ直す
- LAN上で次を返す`tools/browser_fixture_server.py`を用意する
  - 短い正常HTML、長い段落、相対link、list、`pre`、entity、UTF-8
  - 1 byteずつ遅く送る応答、途中切断、長すぎるheader、長さ不一致
  - 301／302／307／308、redirect loop、HTTPS `Location`
  - chunk境界を毎回変えるchunked応答
  - 未終了tag、深いnest、巨大属性、`script`／`style`内の`<`と`>`
- 現行releaseのIROM、DROM、`.data`、`.bss`、stack、display underrun、HTTP速度を記録する

**完了条件**: 上限値とfixture endpointがこの文書とserver内で一致し、変更前baselineを
記録できること。README.mdは変更しない。

### Stage 1: HTTP URLの解析と相対参照の解決

`browser::url`だけを実装する。まだ通信と画面へ接続しない。正常・異常URLのtable testを
置き、base URLに対する相対参照の結果を文字列で比較する。

最低限、default port、空path、query、fragment、`/a/../b`、`../`、scheme-relative、
IPv4、DNS名、2,048 byte境界、CR/LF、userinfo、IPv6 literal、HTTPSを試す。

**完了条件**: link表示、DNSへ渡すhost、TCP endpoint、HTTP request targetが1つの`Url`から
得られ、CR/LFをrequestへ混入できないこと。release buildとXIP layout検査が通ること。

### Stage 2: 中断可能なHTTPトランザクション

`net::http::Transaction`を追加し、fixtureへ接続して1回のpollごとに処理を返す。head完了、
body chunk、完了、失敗、cancelを呼び出し側が区別できるAPIにする。Content-Length、chunked、
close-delimitedの3終端を実装し、既存`httpget`の成功・404・512 KiB取得を回帰確認する。

**実機確認**（`httpstream`診断で行う。frame loopが要る2項目はStage 5へ移した。
判断記録「Stage 2: 実機確認のうち2項目をStage 5へ送る」を参照）:

- 2 MiB超、4 KiB超header、途中切断、chunk破損がそれぞれ別のエラーになる
- cancel／失敗を100回繰り返してsocket handleとallocationが増え続けない
- `ipconfig`の`dropped`、`throttled`、`failed`に処理停止由来の増加がない
- `httpget`の成功・404・512 KiB取得が回帰していない

### Stage 3: ストリーミングHTML解析と文書モデル

通信から切り離して、任意位置で分割した入力chunkから同じ文書モデルを作る。生HTMLは
保持しない。UTF-8のmulti-byte列、comment終端、entity、tag名、属性値、`script`／`style`
終端がchunk境界をまたぐ場合を必ず試す。

`DocumentBuilder`は上限到達とallocation失敗を呼び出し側へ返し、途中まで作った文書を
成功としてpublishしない。未知のtagや属性は無視するが、そのサイズ自体は上限へ数える。

**完了条件**: Stage 0の全HTML fixtureが期待するtext、block、linkへなり、入力を1 byteずつ
渡しても結果が変わらないこと。2 MiB、16,384 item、4,096 link、深さ32の各境界を確認する。

fixtureはPython側から`--dump browser/tests/fixtures`で書き出し、
`browser/tests/fixtures.rs`が同じbyte列を解析する。chunk sizeは1、2、3、5、7、13、
31、64、127、512、4096で回して結果が一致することを確認する。

### Stage 4: ローカル文書による表示、スクロール、リンク操作

networkなしで埋め込みfixtureを`app::browser`へ表示する。toolbar、viewport、status line、
折返し、見出し、list、`pre`、linkの選択色を実装する。pointer共通部を`win`から分離し、
`win`のdrag／抜去／cursor回帰も同じStageで確認する。

**実機確認**:

- キーボードだけで全scroll、link選択、終了ができる
- touchとUSBマウスで同じlinkを選択できる
- 文書末尾、空文書、1行がURL上限まで続く文書で座標overflowやfreezeがない
- 連続Page Down／Page Upを1分行ってdisplay underrunとDMA errorが増えない
- `win`のpointer移動、window drag、時計更新、マウス抜去が退行しない

### Stage 5: ネットワーク統合、遷移、戻る、キャンセル

Stage 2のbodyをStage 3へ直接渡し、完成した文書だけStage 4の表示対象に切り替える。
読み込み中は直前ページを残してstatusだけ更新し、成功時に一括で置き換える。失敗時は
直前ページへ戻れるエラー画面にする。

redirect、絶対／相対link、address入力、履歴8件、戻る時の再取得とscroll位置復元をつなぐ。
HTTPSへのredirectは追わず、遷移先を表示して未対応と説明する。

**実機確認**:

- 読み込み中のEscapeで1 frame以内にsocketがremoveされる（Stage 2から移動）
- slow response中もUSBキーボードとtouchの保守が止まらない（Stage 2から移動）
- `browser`開始前に接続とDHCPが無い場合、シェルへ具体的な準備手順を表示する
- 正常HTML、相対link、redirect chainを辿れる
- 読み込み中のcancel後、別URLを即座に取得できる
- C6切断、DNS失敗、TCP timeout後にブラウザを終了でき、シェルの再接続も成功する
- 8件を越えた履歴が古い順に捨てられ、戻るたびに旧DOMを保持していない

### Stage 6: 上限検査、異常系、メモリ・表示・通信の実機受入

Stage 0の全fixtureを実機から巡回する`browsertest`診断を追加する。自動試験はpointer操作を
必要とせず、URLごとのstatus、text CRC、item／link数、browser-owned peak bytes、所要時間、
network counters、display countersをUARTへ出す。

コマンドは`bt [rounds]`（別名`browsertest`）。`hbase`が指すサーバから
（現在は`bt <url> [rounds]`で、引数に渡したサーバから——「完了後:
`hbase`を廃止し、schemeの無いアドレスに`http://`を補う」）
`/manifest.txt`を読み、その中の各pathを`app::fetch::Fetch`——ブラウザ画面と
同じもの——で取得して、manifestの期待値と突き合わせる。UARTへは1 endpoint
1行、コンソールへは失敗と要約だけを出す。

**最終受入条件**:

- 全正常fixtureのtext CRCとlink解決結果が期待値と一致する
- 全異常fixtureが定めたエラーで終了し、panic、OOM abort、5秒を越える無反応がない
- browser-owned peakが100回遷移後も最初と同じ規模へ戻る
- 100回の取得・cancel・戻るでsocket枯渇、RX queue破壊、C6 transport再同期不能がない
- 30分表示・scroll soakでdisplay underrun、DMA errorが0増分
- `httpget`、`ping`、`nslookup`、`win`、`paint`、USB keyboard／mouseを回帰確認する
- release build、`tools/check_elf_layout.py`、ESP image検査が通り、stack 128 KiB以上を維持する

上限内でも描画が入力を1 frame以上止める場合は、HTML機能を増やさずlayout／描画の処理量を
さらに分割する。peakが不自然に増えた場合は文書モデルの重複とcapacityを調べる。

### Stage 7: 現状文書の更新

実装と実機受入が済んだ内容だけを現状文書へ反映する。

- `docs/BROWSER.md`を追加し、対応HTML、操作、上限、エラー、HTTPのみであることを記載する
- `DESIGN.md`の現状文書表へ`BROWSER.md`を追加し、制約を実装結果へ合わせる
- `docs/FILE_LAYOUT.md`へbrowser、URL、HTML、document、pointerの責務を追加する
- `docs/NETWORK.md`へ非同期HTTP transactionと既存`httpget`との関係を追加する
- `docs/APPS.md`へブラウザ画面、cursor、部分再描画、入力順序を追加する
- 実測したメモリ、コードサイズ、速度、underrun、非目標をこの計画の判断記録へ残す

README.mdは人間管理なので、この依頼で明示されない限り変更しない。実装によりREADME.mdが
古くなった場合は、変更せず最終報告で不一致箇所を示す。

## Stageを越えて固定する中止条件

- ブラウザの都合でC6受信フレームの定期処理を止める設計になった場合
- HTML入力量、要素数、URL長、redirect数のいずれかが無制限になった場合
- 入力依存の通常allocation失敗がpanic／abortになる場合
- HTML、文書モデル、全ページbitmapの3重保持が必要になった場合
- 認証情報を平文HTTPで送る機能が必要になった場合
- CSSまたはJavaScriptが初期版の完了条件へ混入した場合
- 既存`httpget`、表示、USB inputの実機回帰を通せない場合

中止条件に当たったStageは完了扱いにせず、この文書へ実機症状と判断を追記して設計を
戻す。TLS、画像、日本語フォントを理由に初期版を止めず、それぞれ別計画へ分離する。

## 将来拡張の順序

初期版の受入後、必要性とメモリを実測して個別の計画書を作る。順序の目安は次とする。

1. 外部FlashまたはSDから必要glyphだけ読む日本語bitmap font
2. 同一文書内fragment移動と、小さい固定bookmark保存
3. scanline decodeできる画像形式を1つだけ追加
4. TLS 1.2／1.3

TLSは暗号algorithmだけでは完了しない。CSPRNG、SNI、hostname検証、CA root、証明書の
有効期間を判定できる壁時計、最大record／handshakeサイズ、失敗表示を一つの受入条件にする。
RTCが無効または未設定のときに証明書時刻検証を黙って省略してはならない。HTTPからHTTPSへ
redirectされたページを自動的にHTTPへ戻すdowngradeも行わない。

## 実装時の判断記録

各Stageで上限値、モジュール構成、実機結果を変更した場合は、現状文書だけでなく
この節へ「当初案」「観測」「採用理由」を残す。

### Stage 0: URL／HTML／documentを`src/`ではなく別クレートへ置いた

**当初案**: モジュール構成表のとおり`src/browser.rs`と`src/browser/url.rs`等。

**観測**: このリポジトリには試験が1件も無く、`cargo test`はホストターゲットで
ビルドされるためno_stdのbinクレートでは通らない。`src/`直下に置くと、URL解析や
HTML解析の境界試験を回す方法が「実機へ焼いて画面を見る」しか無くなる。
`[lib]`ターゲットを同じパッケージへ足す案は、パッケージ単位で依存が解決される
ため`riscv-rt`や`smoltcp`までホスト向けにビルドしようとして成立しない。

**採用理由**: 依存ゼロのワークスペースメンバ`browser/`（パッケージ名
`tab5-browser`）を作り、`src/browser.rs`はそこからの再exportだけにした。
firmware内のパス（`crate::browser::url::Url`）は当初案と同じで、呼び出し側は
クレート境界を意識しない。ホスト試験は
`cargo test -p tab5-browser --target x86_64-unknown-linux-gnu`（`mise run test`）。
`browser/src/memory.rs`の`try_reserve`ヘルパもこのクレートに置き、`net::http`も
そこを通す。

### Stage 0: fixture serverと上限値の二重定義を突き合わせで守る

`tools/browser_fixture_server.py`は上限値の写しを持ち、`--check-limits`で
`browser/src/limits.rs`の`pub const`を読んで比較する。起動時にも自動で比較し、
食い違ったら起動しない。`/limit/...`のfixtureは上限のすぐ外側に置いてあるので、
片側だけ動かすと「上限超過を検出できないのに通ってしまう」状態になるため。
endpointは51件で、`/manifest.txt`が「path→期待する結末」を返す。

### Stage 0: baseline（変更前）

`tools/check_elf_layout.py`の値。完了時との比較はStage 7の「実測値」にある。

```text
IRAM=10100  DRAM-rodata=1364  DROM=130776  IROM=617594  stack=178176
```

HTTP速度のbaselineは[`NETWORK.md`](NETWORK.md)に既にある512 KiBで約827 KiB/sを
そのまま基準とした（Stage 2で`httpget`を`Transaction`のwrapperへ移したあとも、
同じファイルの取得で回帰していないことを実機で確認）。display underrunは
Stage 4以降のscroll soakとStage 6の`bt`で「0増分」を都度確認する形にした。

### Stage 1: 非ASCIIはpercent-encodeし、URL長は符号化後で数える

**当初案**: URL契約は「hostはASCII」としか決めていなかった。

**観測**: `href`にUTF-8がそのまま入っているページは普通にある。拒否すると
リンクが死に、そのまま通すとrequest targetが非ASCIIになる。

**採用理由**: path／query／fragmentの0x80以上のbyteだけをpercent-encodeし、
既にASCIIのbyte（`%`を含む）は触らない。よって`%2e`は`.`にならず、
`..`の正規化対象にもならない（directory traversalを作らない）。
hostは非ASCIIなら`InvalidHost`（IDNAは実装しない）。2,048 byte上限は
符号化後の表示文字列に対して適用する。

### Stage 1: 数字とドットだけのhostは、IPv4として不正なら名前解決しない

`0177.0.0.1`や`999.1.1.1`は`inet_aton`系の実装では別の意味を持つ。
アドレス欄の表示と実際の接続先が読み手によって変わる余地を残さないため、
数字とドットだけで構成されたhostは厳格なdotted quad（先頭ゼロ禁止、各field
255以下）でなければ`InvalidHost`にし、DNSへは投げない。

### Stage 2: `Progress::HeadReady`は本文と同じpollで返さない

headと同じ読み出しに本文の先頭が入っていても、その分は`head_buffer`へ保持して
次のpollまで渡さない。redirectや表示しないstatusを、本文を1 byteも読まずに
判断できる状態を保証するため。`Content-Length: 0`だけはどの読み出しでも完了
しないので、pollの冒頭で明示的に完了させている。

### Stage 2: 実機確認のうち2項目をStage 5へ送る

**当初案**: Stage 2の実機確認に「読み込み中のEscapeで1 frame以内にsocketが
removeされる」「slow response中もUSBキーボードとtouchの保守が止まらない」を
含めていた。

**観測**: シェルの`execute`は`InputManager`を受け取らず（`usb_host`だけを
受け取り、shell.rs内に137箇所ある）、コマンド実行中はフレームループが回らない。
この2項目を今Stage 2で満たすには、シェルの引数構成を作り替えるか、
frame loopを持つ画面を先に作るかのどちらかになる。

**採用理由**: どちらもStage 4／5でframe loopを持つ`app::browser`が出来た時点で
自然に確認できる項目なので、Stage 5の実機確認へ移した。Stage 2では代わりに
`httpstream`診断で、head／body／完了／失敗／cancelの区別、3種類の本文終端、
異常系の区別、100回のcancelとfetchでsocketとヒープが戻ることを確認する。
`httpstream`は1回のpollを`DEFAULT_POLL_BUDGET`（4 KiB）で区切るループなので、
「pollが戻ってくること」自体は`polls`／`idle`カウンタで観測できる。

### Stage 2: `httpget`は`Transaction`の薄いwrapperにした

HTTPヘッダ解析・本文framing・socket後始末は1箇所だけになった。`httpget`側の
挙動で変えたのは次の3点。

- 要求に`Accept-Encoding: identity`を追加した
- `Content-Length`があればそこで本文を切る（従来はcloseまで読んでいた）
- エラー表示を`net::http::error_text`の1行へ統一した（種類が増えたため）

本文上限は`httpget`では無制限（`u64::MAX`）にしてある。512 KiBの回帰用
ダウンロードがブラウザのページ上限より大きいため。

### Stage 3: 文書モデルはDOMではなくflat arenaにした

**当初案**: 「compactな文書モデル」とだけ決めていた。

**採用**: ページ全体のテキストを1本の`String`に持ち、`Run`（textへのbyte範囲＋
style 1 byte＋link index）と`Block`（種別＋runの範囲）で構造を表す。node per
elementではないので、1 MiBの本文はおおよそ1 MiBのままで済む。

- block: Paragraph／Heading(1..6)／ListItem{depth, marker}／Preformatted／Rule
- 強制改行（`<br>`、`pre`内の改行）は本文中の`\n`。layoutが硬改行として扱う
- `MAX_ITEMS`はblock数とrun数の合計に対して適用する

### Stage 3: 未知の要素はinline扱いにする

blockとして扱う要素名は明示列挙にし、知らない要素はinlineとして中身のテキストだけ
通す。逆にすると`<span>`や`<font>`が文の途中で段落を割ってしまう。取り違えたときの
被害が小さい側を既定にした。

### Stage 3: 文字参照は`;`を必須にした

HTML本来の仕様は一部のnamed referenceを`;`なしでも解決するが、初期版は`;`必須。
`&amp`（`;`なし）は`&amp`とそのまま表示される。例外のない規則のほうが、1997年の
ページとのbug互換性より価値があると判断した。数値参照・named 6種はいずれも`;`必須。

### Stage 3: 本文バッファの伸長を128 KiBで頭打ちにする

`String`の通常の伸長は倍々なので、1 MiBの本文を持つと容量2 MiBになり、1 MiBが
ページ表示中ずっと遊ぶ。`browser::memory::reserve_capped`で、小さいうちは倍々、
128 KiBを超えたら128 KiB刻みにした。所有量の無駄は最大128 KiBで頭打ちになる。

### Stage 3: 不正UTF-8はU+FFFDへ置換する

tokenizerはbyte単位で動き（markupのdelimiterは全てASCII）、テキストrunを
flushする時点でUTF-8検証する。不正な列はU+FFFDにして前後のテキストを残す。
入力chunkの境界がmulti-byte列の途中に落ちる場合は、`safe_split`で完成している
ところまでしかflushしない。

### Stage 3: `httpstream <url> parse`を足した

Stage 5の統合を待たずに、実機で「socket→HTTP→tokenizer→文書」まで通す診断。
描画はしない。title、block数、run数、link数、text byte数、所有byte数、
最長token、本文の先頭を出す。上限に当たった場合は文書を返さずエラー名を出す
（途中まで作った文書をpublishしない実装であることの確認になる）。

### Stage 4: scrollはpixelではなく行単位にした

**当初案**: 「現在のscroll位置と交差する行だけを描画」とだけ決めていた。pixel単位の
scrollを想定していた。

**観測**: viewportの上端で行が半分だけ見える状態を作ると、glyph描画側に上下の
clipが必要になる。`draw_ascii_char`はパネル端に対するclipしか持っておらず、
2辺clipを足すのはbrowserのためだけにframebufferの中心部を触ることになる。

**採用理由**: viewport最上段は必ず行の先頭に揃え、下端で入りきらない行は描かない。
`scroll`はpixelではなく行indexで持つ。行高は見出しで変わるので「1行スクロール」の
移動量は場所によって変わるが、読んでいて不自然ではない。上下端に半端な行が
出ない代わりに、下端に最大1行分の余白が出る。

### Stage 4: viewportは全面描き直し、glyphは背景なしで描く

背景をPPAの矩形塗り（DMA、PSRAMへの読み出しを出さない）で1回置いてから、
glyphを`background: None`で重ねる。1文字あたりの書き込みが192 pixel（不透明セル）
ではなく実際にインクの乗る15 pixel程度になるので、1画面ぶんが数万回の散らばった
書き込みで済む。CPUによる全画面塗りは行わない。

### Stage 4: 組み込みページを常設にした

`browser`はネットワーク無しで開ける`http://built-in/`のページ群を持つ。Stage 4で
表示・scroll・リンク操作を通信抜きで確認するためだが、これは後段でも消さない。
Wi-Fiが落ちているときに表示側だけを確認できる唯一の文書であり、Stage 5以降の
ホーム画面でもある。ホスト名`built-in`はresolverに渡す前に判定するので、
LAN上の同名ホストへ問い合わせが飛ぶことはない。

- `/` ホーム（操作説明とリンク集）
- `/sample` 対応要素を一通り含む
- `/long` scroll用の長い文書
- `/wide` URL上限と同じ長さの1行
- `/empty` 空文書

### 完了後: 終了は`Ctrl+Q`、アドレス欄は`Ctrl+L`（利用者の指摘）

**観測**: `Escape`が終了なのは邪魔。読んでいたページを捨てる操作が、どの
キーボードでもいちばん単独で押しやすいキーに乗っている。アドレス欄も
`Enter`／`F2`より`Ctrl+L`で開きたい。

**前提が1つ足りなかった**: Ctrlは`input::Key`まで届いていなかった。
`key_from_hid_usage`が見ていたmodifierはshift（bit 1／bit 5）だけで、
`Ctrl+Q`は素の`q`と区別できない。ブラウザ側だけでは実装できず、入力層から
手を入れている。

**採用**: `Key::Control(u8)`を足し、持たせるのは小文字の英字そのものにした。
C0制御コードを`Ascii`で流す案は採らない。`Ctrl+H`＝0x08、`Ctrl+I`＝0x09、
`Ctrl+M`＝0x0Dは既にBackspace・Tab・Enterの席で、割り当てた画面が気づかずに
それらも割り当てることになる。別の型にしておけば、重なりの扱いはバイトを
変換する場所（`key_from_hid_usage`と`key_from_ascii`）だけで決まる。

| キー | 変更後 |
| --- | --- |
| `Ctrl+Q`、`q` | 終了。`Ctrl+Q`はアドレス欄が開いていても読み込み中でも効く |
| `Ctrl+L` | アドレス欄（`F2`とクリックと`Enter`未選択時は据え置き） |
| `Escape` | 中止／アドレス欄を閉じる／リンク選択とメッセージの解除。終了しない |

`Ctrl+Q`は`handle_key`の先頭、アドレス編集への分岐より前で処理する。
「出られない画面の状態」を作らないため。

**実機確認済み**: HID経路（Tab5 Keyboard／USB）でCtrlが届き、`Ctrl+Q`と
`Ctrl+L`が効く。あわせて**CardKB v1.1にCtrlキーが無い**ことも確認した。
`q`（終了）と`F2`（アドレス欄）は「Ctrlの無いキーボードのための保険」では
なく、CardKBで操作するときの唯一の経路である。割り当てられるのは、アドレス欄
以外に文字入力が無いからで、その前提はこの版でも変わっていない。

### 完了後: `hbase`を廃止し、schemeの無いアドレスに`http://`を補う（利用者の指摘）

**観測**: `hbase`は最初期の試験用で、もう要らない。代わりに、渡された
アドレスにschemeが無ければ`http://`を補ってほしい。

**採用**: `Url::parse_typed`を`browser/src/url.rs`に足し、`browser`・`hs`・
`bt`の引数とビューアのアドレス欄が全部そこを通る。`Url::parse`はscheme必須
のまま——`href`や`Location`のscheme無しは省略ではなく壊れた文書。

**判定を`classify`でやると外す**（ホスト側テストで確認）:

```text
example.com:8080/x  classify=Absolute  parse=Err(UnsupportedScheme)
localhost:8080      classify=Absolute  parse=Err(UnsupportedScheme)
192.168.0.2:8080/x  classify=Relative
```

URLの文法ではコロンの前がschemeなので、`host:port`はschemeを持つように
見える。fixture serverを叩くときにいちばん普通に打つ形がこれで、Stage 5で
入れたアドレス欄の`http://`補完（`classify == Relative`かつドットを含む）も
この形には効いていなかった。判定は`has_scheme`——**`scheme://`の形か、
`http:`／`https:`で始まるか**——に分けた。`ftp://x`は補完せずそのまま拒否、
`http:example.com`は「hostが無い」のまま。

`bt`は基準アドレスを失うので`bt <url> [rounds]`にした。scheme補完のおかげで
`bt 192.168.0.2:8080`で済み、`hbase http://…`＋`bt`より短い。覚えている値と
打った値の2つが基準になりうる状態も消えた。

アドレス欄では、ページの隣でしか意味を持たない参照（`/path`、`?q=1`、
`#part`、先頭がドットの相対参照）だけを今のページに`Url::resolve`で解決し、
それ以外は`parse_typed`へ回す。Stage 5で「組み込みページの上では`hbase`を
基準にする」としていた分岐は消えた。組み込みページで`/simple.html`と打つと
`http://built-in/simple.html`＝「そんな組み込みページはない」になる。
ホスト付きで打てば通るので、`hbase`が埋めていた穴はscheme補完が塞いでいる。

### 完了後: C6のバックログ溢れでリンクが死ぬ（実機で発覚）

**症状**: リンクをクリックして閲覧している途中で
`HOSTED: slave has more data than the staging buffer holds, len=0x0000254A`
が連続で出て、以後ブラウズできなくなる。

**原因は2つあった。**

1. **予防が足りていない**。C6は受信フレームをホストが読むまで溜め、その総量が
   staging buffer（8,704 byte）を超えると**バイトカウンタを再同期できず、
   リンクは二度と戻らない**。0x254A＝9,546 byteなので、読まない時間が
   1,500 byte級のフレーム7個ぶん続いたことになる。犯人はviewportの全面
   再描画で、画面の大半のPPA塗り＋数万回の散らばったglyph書き込み＋1.6 MBの
   writebackの間、ネットワークに一度も触っていなかった。`hs`や`bt`で
   100回回しても出なかったのは、あれらが描画しないため

2. **起きたあとの扱いが間違っている**。`fill_buffer`は溢れを検出しても
   `link_lost`を立てずに`false`を返していた。長さレジスタはホストが読むまで
   減らず、ホストは読めないので、以後すべてのpollがここへ来て延々とログを
   出し続ける。上位層は「リンクは生きている」と思ったまま。コメント自身が
   「ここから復旧はできない」と書いてあるのに、コードがそう振る舞っていなかった

**修正**:

1. フレームの先頭で必ず`Stack::poll`を呼ぶようにしたうえで、**描画の中でも
   呼ぶ**。`draw_dirty`に「リンクを読む」callbackを渡し、背景塗りの直後、
   glyph 8行ごと、writebackのband（10分割）ごとに呼ぶ。読むものが無ければ
   SDIOのレジスタ読み数回で済むので、細かく呼ぶ側に倒して困らない。
   writebackを**x方向で**分割しているのは、フレームバッファが回転している
   ため——論理行で割ると`flush_rect`に渡る範囲がほぼ全域のままで、同じ1.6 MBを
   10回に分けて書き戻すだけになる
2. `wifi/hosted.rs`で溢れ時に`link_lost = true`を立てる。`Rpc::is_alive`が
   falseになり、`Stack::poll`が失敗し、実行中の取得は`LinkLost`を報告し、
   シェルの`drop_dead_session`がセッションを捨て、`wificonnect`でやり直せる。
   ログ地獄が1行の報告になる
3. ビューアは毎フレーム`is_alive`を見て、死んでいたら一度だけstatus行へ出す。
   そうしないと「押すリンク全部が別々の理由で失敗する」ように見える
4. viewport再描画の所要時間を計測し、**最悪値を更新したときだけ**UARTへ出す
   （`BROWSER: slowest viewport repaint so far, ms=`）。計画の中止条件
   「描画が入力を1 frame以上止める場合はさらに分割する」を、見積もりでは
   なく実測で判断するため

これは中止条件「ブラウザの都合でC6受信フレームの定期処理を止める設計に
なった場合」に該当していた。設計を戻すのではなく、定期処理を描画の中へ
入れることで解いた。

### 完了後: viewport再描画の実測はおおむね1フレーム

**観測**: `BROWSER: slowest viewport repaint so far`は時々1フレーム
（17.5 ms）程度。

**内訳**（`docs/DISPLAY_BANDWIDTH.md`のPPA実測値から）: 1280×648の塗りは
PPAで約12 ms。ここが支配項で、1.66 MBをPSRAMへ書く帯域そのもの。残りは
glyphの散らばった書き込みと、汚れたキャッシュラインの書き戻し。**全面を
描き直す限りこれが床**で、アルゴリズムでは下げられない。

block copyでscrollする案は採らなかった。コピーは読み＋書きで、塗りの倍の
PSRAM往来になる。この基板では「消して描き直す」より高い。

**採用した削減**: 前回どこまで描いたかを覚えておき、**今回描く範囲と前回の
範囲の広い方**だけを塗って書き戻す。短いページ（ホーム、エラーページ、
fixtureの大半）は数百ピクセルで済み、5倍以上速くなる。全画面がテキストで
埋まるページ（`/long`）は変わらない。

**1フレームで許容と判断した理由**: 入力は`InputManager`が溜めるので取りこぼさ
ない。C6のリンクは描画の中で読んでいるので溢れない。display underrunは0増分。
残るのはscrollの滑らかさ（連打で30〜57 Hz）だけで、これは表示機能を削ってまで
買うものではない。計画の中止条件は「1 frame以上止める場合はさらに分割する」で
あって、その分割（描画中のリンク読み・帯分割・範囲の限定）は済んでいる。

### 完了後: アドレス欄は現在のアドレスを保持する（利用者の指摘）

**当初**: アドレス欄は開いた時点で全選択状態で、最初の1文字で全体が置き換わる。

**観測**: 「再入力する際に今のアドレスが消えるのは不便」。全選択が正しいのは
アドレス全体を貼り付けるのが主な操作であるデスクトップのブラウザで、Tab5では
よくやるのは末尾を変えることであり、親指キーボードで打ち直すのがいちばん
高くつく部分だった。

**採用**: 今出ているアドレスを保持したまま、カーソルを末尾に置いて開く。
あわせて普通のテキスト欄にした——`←`／`→`でカーソル移動、`Home`／`End`で両端、
`Backspace`と`Delete`で前後の削除。表示窓はカーソルに追従するので、長い
アドレスの途中も直せる。入力がASCIIだけなのでbyte位置と文字位置が一致し、
カーソルは`usize` 1つで足りる。

### 完了後: 行間を4分の1足した（利用者の指摘）

**観測**: 「フォントが窮屈」。5×7フォントは6×8の箱に入っているので、行間は
1ピクセル分（scale 2で2ピクセル、16ピクセルの活字に対して）しか無い。

**採用**: `layout::Metrics`を「glyphの箱」と「行の箱」に分け、行の箱＝
glyphの箱＋その割合（`LINE_GAP_PERCENT`）にした。空きは下に置く（半分を上に
振ると最初の行がviewport上端から下がるだけで、行間はどちらでも同じ）。

割合は最初2割にしたが、実機で見てまだ窮屈だったので4分の1へ上げた
（本文で4ピクセル、見出しで6ピクセル）。ピクセル数ではなく割合で持って
あるので、変更は定数1つで済み、見出しも一緒に動く。

割合で持つのは、3倍角の見出しが本文用の固定値で詰まらないようにするため。
副次的な効果として、リンクの下線をこの空きへ引けるようになった。従来は箱の
下端に引いていて`g`や`y`のdescenderに触れていた。

### Stage 7: 実測値

`tools/check_elf_layout.py`。左がStage 0のbaseline、右が完了時。

```text
          baseline    完了      差
IRAM        10100    10100      0
DRAM-rodata  1364     1364      0
DROM       130776   130776      0
IROM       617594   736396  +118802
stack      178176   178176      0
```

IROMは4 MiBのXIP窓に対して736 KiBで、余裕は十分。IRAM・DRAM・stackは
1 byteも動いていない。ブラウザは固定配列を`.bss`へ置かず、ページ依存の
データはすべてPSRAMヒープにあるため。

- ホスト側テスト186件（`browser`クレートのユニット161＋fixture統合25、
  ほかに`#[ignore]`の基準表出力1件）
- 追加した行数は約8,950行（`browser/src/`＋`src/app/`のブラウザ関連4ファイル。
  コメントと試験を含む）
- fixture端点52件。`bt`が巡回するのは`ok`と`error:NAME`の49件

### Stage 7: 現状文書へ反映したもの

- [`BROWSER.md`](BROWSER.md)を追加（対応HTML、操作、上限、エラー、URLの扱い、
  平文HTTPだけであること、診断コマンド）
- `DESIGN.md`のドキュメント構成表へ1行追加。制約欄にブラウザとHTTPの
  2つの顔を追記
- [`FILE_LAYOUT.md`](FILE_LAYOUT.md)へ`browser/`クレートの各モジュールと
  `src/app/`の4ファイル（`browser.rs`・`fetch.rs`・`pointer.rs`・
  `browsertest.rs`）を追加
- [`NETWORK.md`](NETWORK.md)のHTTP節を書き直し（`Transaction`と`get`の関係、
  本文終端3種、`HeadReady`を本文と同じpollで返さない理由）。DNS節へ`Query`を追記
- [`APPS.md`](APPS.md)へビューアの3帯・部分再描画・入力順序・文字の見分けを追加。
  `win`のポインタ節は`pointer.rs`への参照に書き換え

`README.md`は変更していない。実装により古くなった箇所は最終報告で示す。

### Stage 6: 取得の状態機械を`src/app/fetch.rs`へ切り出した

**当初案**: `browsertest`巡回はStage 6で追加する、とだけ決めていた。

**観測**: 巡回に独自の取得ループを書くと、redirect追跡・statusの判定・
「そもそもHTMLか」の判定がビューアと巡回で二重になる。それでは巡回が
「実際に使われるコードとは別のコード」を検査することになる。

**採用理由**: `app::fetch::Fetch`（`start`／`step`／`close`）へ切り出し、
ビューアと`bt`の両方が同じものを回す。`Fetch`はDocumentかFailureを返すだけで、
履歴やscroll位置は知らない（それはビューアの都合なので`app::browser`側に残した）。
redirectは`Fetch`の内部で完結する。

`Failure::name`は1語の安定した識別子で、fixture manifestの`error:NAME`と
突き合わせる対象。`status`だけは数字を含むので`status-404`のように合成する。

### Stage 6: 入力上限とテキスト上限のfixtureを分けた（実機で発覚）

**症状**: `bt`で`/limit/input-nolength`が`expected error:body-limit, got text-limit`。
`recv=1066088`。

**原因**: fixtureの本文が512文字の段落の繰り返し＝ほぼ全部テキストだったため、
入力上限（2 MiB）より先に表示テキスト上限（1 MiB）に当たっていた。`text-limit`は
正しい結末で、間違っていたのはfixtureの方。当初のdocstringは
「item・text上限に途中で当たらないよう段落の繰り返しにした」と書いてあったが、
テキスト主体の本文でテキスト上限を避けられるわけがなく、単に誤りだった。

**修正**: 2つに分けた。

- `/limit/input`・`/limit/input-nolength`: 中身の無い`div`の繰り返し（2,111,560 byte、
  文書テキストはほぼ0）。空blockは捨てられるのでitem上限にも当たらず、
  名前どおり入力上限だけに到達する
- `/limit/text`（新規）: 512文字の段落の繰り返し（1,098,322 byte、テキスト
  1,083,469 byte）。入力上限の内側に収まるので、テキスト上限だけに到達する。
  Python側で`assert len(body) < MAX_DECODED_HTML_BYTES`している

上限が複数あるとき「どれに先に当たるか」はfixtureの中身が決めるので、
上限1つにつきfixture 1つを対応させる必要があった。

### Stage 6: manifestに「巡回対象外」の印を足した

`/manifest.txt`自身と`/download/*.bin`はHTMLではないので、ページとしては
`not-html`になる。期待値の書式を4種類にして、巡回は前2つだけを実行する。

- `ok` — 取得できてページになる
- `error:NAME` — ちょうどその1語の理由で失敗する
- `download:crc32=...` — HTMLではない。`httpget`回帰用で巡回は飛ばす
- `text` — manifest自身。同じく飛ばす

### Stage 6: text CRCの期待値はホスト側に置く

**当初案**: 「全正常fixtureのtext CRCとlink解決結果が期待値と一致する」。

**観測**: 期待値をファームウェアに埋めると、fixtureを変えるたびに2箇所を
直すことになり、片方が古いまま通る状態を作る。上限値のミラーで既に一度
それに助けられている（`--check-limits`）が、CRCはPythonからは計算できない。

**採用理由**: 文書の中身そのものの期待値はホスト側テスト
（`browser/tests/fixtures.rs`、chunk sizeを変えても一致することを含む）で
byte単位に検査する。実機の`bt`は「各fixtureが正しい**結末**に達すること」を
検査し、CRC・block数・link数・text byte数・peak・所要時間を1行ずつUARTへ出す。
両者を突き合わせたいときのために、同じ数字をホスト側で出す
`reference_metrics`テスト（`#[ignore]`）を置いた。

```sh
cargo test -p tab5-browser --target x86_64-unknown-linux-gnu \
    --test fixtures -- --ignored --nocapture reference_metrics
```

link先の解決結果はbase URLに依存する（ホストとTab5でbaseが違う）ので、
実機側はlink数、ホスト側は解決後のURLそのものを検査する分担にした。

### Stage 5: fixture serverのredirect chainがN+1回だった（実機で発覚）

**症状**: `/redirect/chain/5`が`Too many redirects`になる。

**原因**: サーバ側の`/redirect/chain/N`が、`N == 0`のときも`/simple.html`へ
リダイレクトしていたため、実際には**N+1回**跳んでいた。`chain/5`は6回で、
上限5に対して正しく拒否されていた。ブラウザ側の数え方（初回navigationを
`redirects: 0`とし、6回目の3xxで`5 >= 5`により拒否）は仕様どおり。

**修正**: `chain/0`はリダイレクトせずページを返し、`chain/1`が`/simple.html`へ
1回だけ跳ぶようにした。これで`chain/N`はちょうどN回になり、`chain/5`は成功、
`chain/6`は上限エラーになる。この端点は「上限がoff-by-oneならここでしか
分からない」ためのものなので、端点自身がoff-by-oneでは意味が無かった。

### Stage 5: 上限の突き合わせがミラー漏れを捕まえた

上の修正のついでにサーバを起動したところ、Stage 4で足した`MAX_LAYOUT_LINES`が
Python側に無く、`--check-limits`が`MISMATCH`で起動を止めた。Stage 0で
「片側だけ動かすと上限超過を検出できないまま通ってしまう」ために入れた仕組みが
想定どおり働いた形なので、記録しておく。

### Stage 5: DNSにも中断可能なAPIを足した

**当初案**: 名前解決は既存の同期`net::dns::resolve`を使う。

**観測**: `resolve`はresolverが1台なら5秒、複数なら13秒ブロックする。その間
frame loopが回らないので、Escapeが効かない画面になる。しかもEscapeを押したくなる
のはまさにその状況である。

**採用理由**: `net::dns::Query`（`start`／`poll`／`cancel`）を足し、同期`resolve`を
その上のループに置き換えた。`net::http`で`get`を`Transaction`のwrapperにしたのと
同じ形で、timeoutと失敗の意味が二重定義にならない。`Query`はDNS socketのslotを
持つので、`cancel`を全経路で呼ぶ。

### Stage 5: `browser`はネットワークが無くても開く

**当初案**: 「`browser`は既にstation接続とIPv4設定がある場合だけ開く」。

**観測**: 組み込みページ（Stage 4）はflash上にあり通信を必要としない。Wi-Fiが
落ちているときこそ表示側だけを確認したい。

**採用理由**: 準備手順はシェル側（`wificonnect`と`ipconfig dhcp`が打てる場所）に
表示したうえで、画面自体は開く。ネットワークが要るリンクを踏んだときは
「No network」のエラーページを出す。計画の要求「開始前にシェルへ具体的な準備手順を
表示する」は満たしている。

### Stage 5: エラーはstatus行ではなくページにする

失敗した遷移は、viewer自身が組み立てたHTMLをいつもと同じtokenizer・document
builder・layoutに通してページとして表示する。手書きで描かないのは、エラー画面が
「折返しや描画のバグが唯一隠れる場所」になるのを避けるため。ページには失敗した
アドレス、理由、statusコード、`Home`リンクが載る。エラー表示時に直前のページを
履歴へ積むので、Backspaceで元居た場所へ戻れる。

### Stage 5: redirect中も`push_history`を引き継ぐ

最初は「履歴は連鎖の起点が1回だけ積む」つもりで各hopの`push_history`をfalseに
していたが、起点のnavigationは完了せずRestartになるため、誰も積まないことになる。
履歴を積むのは最終的に成功したhopで、積む対象は「読み手がリンクを踏んだ時点で
表示されていたページ」＝連鎖中に変わらないものなので、フラグをそのまま引き継ぐ
のが正しい。

### Stage 5: アドレス欄は組み込みページ上では`hbase`を基準にする

> この判断は「完了後: `hbase`を廃止し、schemeの無いアドレスに`http://`を
> 補う」で撤回済みです。以下は当時の記録です。

アドレス欄に打った文字列は現在のページのURLに対して解決する。ただし現在のページが
`http://built-in/`のときだけは`hbase`を基準にする。組み込みページは実在のサイトでは
ないので、`/simple.html`を`http://built-in/`に対して解決しても存在しないページに
なるだけで、打った人の意図では決してない。加えて、`example.com/page`のように
「ドットを含み先頭がドットでない相対参照」は`http://`を補う。

### Stage 5: キー割り当て

> `Escape`での終了は「完了後: 終了は`Ctrl+Q`、アドレス欄は`Ctrl+L`」で
> 変更済みです。以下は当時の記録です。

- `Enter`は「リンク選択中なら遷移、未選択ならアドレス欄を開く」。1つのキーで2つの
  意味だが、両方が同時に可能になることはない（Tabを押していない読み手には辿る先が
  無い）。新しいキーを増やさずにアドレス入力へ到達できる
- `Escape`は「読み込み中なら中止、アドレス欄が開いていれば閉じる、それ以外は終了」
- `Backspace`は戻る。アドレス編集中は1文字削除
- `F2`とアドレス欄のクリックでもアドレス編集に入る

### Stage 4: statusの優先順位を「メッセージ→リンク先」へ直した（実機で発覚）

**症状**: `https`リンクで`Enter`を押しても何も起きない。

**原因**: statusはリンクにfocusがある間はfocus先URLを優先して描いており、
`HTTPS is not supported`はfocusが乗ったまま設定されるので一度も表示されなかった。

**修正**: メッセージが設定されていればそちらを優先する。メッセージが立つのは
「読み手が今やった操作への返事」のときだけで、focusを動かす経路はすべて先に
メッセージを消すので、リンク先表示はTabを押した時点で戻る。メッセージは赤
（cleartext badgeと同じ色）で描く。どれも「できません」という返事なので。

### Stage 4: boldは色ではなく二度打ちにした（実機で発覚）

**症状**: `<i>`の灰色が黒と区別できない。

**観測**: 5×7フォント・scale 2では、黒と濃い灰色は「emphasis」ではなく
「描画がおかしい」に見える。明度差ではなく色相差が要る。さらに`<b>`は当初
黒のままだったので、そもそも何も起きていなかった。

**採用**:

- bold: 同じglyphを1 pixelずらして二度描く（合成bold）。フォントのweightは
  1つしかないので、合成するか捨てるかの二択で、捨てると`<b>`が無表示になる。
  ずらし量は1 physical pixel（`scale`ぶんずらすと隣の桁と混ざる）
- italic: 濃い赤（0x9000）
- 見出し: `layout::Line`に`heading`を足し、h3〜h6のような本文サイズの見出しも
  二度打ちで描く。これが無いと「上に隙間がある段落」と区別が付かない
- `/sample`のページ本文に「boldは二度打ち」「italicは赤」「codeは緑」と
  書いてある。確認する人が見比べる対象を持たずに済む

### Stage 4: 非ASCIIは中空の四角で描く

5×7フォントにglyphが無い文字を空白にすると日本語のページが白紙に見え、`?`に
すると著者が書いた`?`と区別が付かない。1文字1マスの中空四角にして「ここに
表示できない文字がある」ことを示す。折り返し幅の計算も1文字1マスなので、
行の長さは合う。

### Stage 4: pointerを`src/app/pointer.rs`へ分離した

`win`のカーソル実装（下地退避→描画→復元）をそのまま移した。`win`側は
`super::pointer`を使うだけになり、実装は1つ。描画順（cursorを外す→dirty領域を
描く→cursorを載せる→和集合をflush）はモジュールのドキュメントに明記した。

### Stage 4: 上限を1つ足した（`MAX_LAYOUT_LINES`）

文書の上限（text 1 MiB、item 16,384）だけでは折返し後の行数を縛れない。
`<br>`だけの1 MiBは100万行になり、`Line`が24 byteなので24 MB。32,768行で
打ち切る（1 MiBの散文でおよそ1万行なので3倍の余裕）。

### Stage 3: 診断コマンドを短く打てるようにした

> `hbase`は「完了後: `hbase`を廃止し、schemeの無いアドレスに`http://`を
> 補う」で廃止済みです。打鍵を短くする役目はscheme補完が引き継ぎました。
> 以下は当時の記録です。

**観測**: `httpstream http://192.168.0.159:8080/simple.html parse`は、Tab5の
親指キーボードで打つには長すぎる。長さ自体が「確認しない理由」になる。

**採用**: `hbase <url>`でベースURLをシェル状態（`shell::State`、`cwd`と同じ寿命）に
覚えさせ、部分指定を`Url::resolve`で補完する。補完はページ上の`href`の解決と
同じ処理なので、独自の短縮記法を増やしていない。あわせて`hs`別名と1文字の
モード語（`p`／`r`／`c`）を足した。

```text
hbase http://192.168.0.159:8080
hs /simple.html p
hs /slow c 100
```

Stage 5の`browser`コマンドも同じ`hbase`を使う。

### Stage 2: 追加したモジュール

モジュール構成表に無いものを1つ足した。

| 追加 | 責務 |
| --- | --- |
| `src/app/browsertest.rs` | `httpstream`診断。Stage 6の`browsertest`巡回もここへ足す |

`src/main.rs`には`heap_used()`（グローバルアロケータの払い出し量）を足した。
ブラウザ自身の所有量はこれとは別に数える方針は変えていない。leakの形を見る
ためだけの診断用。
