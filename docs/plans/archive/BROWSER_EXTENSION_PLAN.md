# Browser画像・フォーム・キャッシュ対応計画

> 索引: [`../../../DESIGN.md`](../../../DESIGN.md) ／ 現状仕様:
> [`BROWSER.md`](../../BROWSER.md)、[`NETWORK.md`](../../NETWORK.md)、[`INPUT.md`](../../INPUT.md)

## 目的と実装状況

単一文書のBrowserへ、画像、共通テキスト入力View、フォーム送信、HTTPキャッシュを
段階的に追加する。実装順は、表示基盤 → 画像 → 共通テキスト入力View・フォーム →
HTTPキャッシュとする。各機能は個別に受け入れ可能とし、キャッシュを画像の前提にしない。

2026-09-12時点の合意を記録した計画である。**Stage 0は実装済み（設計固定のみで
実機対象外）、Stage 1〜8は実装・実機受入済み**である。
既存の表・fragment対応の
受入記録は、それぞれの計画書にある。
本書を追加しただけでは現状仕様を変更しない。

## 確定したスコープ

| 項目 | 方針 |
| --- | --- |
| 文書 | 単一文書。frame、frameset、iframe対応は対象外 |
| スクロール | 全体で1つの縦スクロール。pixel位置で管理する |
| 画像 | 静止画JPEG・PNG。1形式ずつ実装する |
| 配置 | 本文中、link内、table cell内。文章の回り込みは対象外 |
| 寸法 | imgの指定を優先。不明な寸法は仮サイズとし、受信途中で判明した寸法から再計算 |
| 幅の制約 | 指定から寸法を決めた後、配置先の幅を超える場合だけ縦横比を保って縮小 |
| 取得 | HTML本文完成・表示後に画像を順次取得。初期の同時画像取得は1件 |
| 画像失敗 | 確保済み領域内にaltと失敗状態を表示。本文の閲覧を継続 |
| フォーム | text・hidden・submit、buttonから開始。GET → POST → control拡充 |
| POST本文 | application/x-www-form-urlencoded。file upload、multipartは対象外 |
| フォーム用途 | 検索と管理下サーバへの入力。ログイン用途、cookie、接続先認証の追加は別スコープ |
| テキスト入力 | 共通TextInputView相当を用意し、アドレス欄とフォームで共有 |
| 日本語入力 | 今回対象外。将来の入力方式を接続できる境界を準備 |
| キャッシュ | RAM内のHTTP GET応答。永続保存、オフライン閲覧、入力値の永続保存は対象外 |
| その他 | CSS、JavaScript、SVG、画像アニメーション、慣性スクロールの追加は対象外 |

## 調査時点の実装と再利用する部分

- `browser/src/layout.rs`はLineのpixel座標とCellBoxを持つ。表の配置、文字幅測定、
  link当たり判定を再利用する。DOMへの全面移行は行わない。
- Stage 1前の`Page`と`HistoryEntry`は先頭行を保持し、本文は上下に部分表示される
  文字を描かなかった。Stage 1でpixel offset、履歴、描画clipへ移行した。
- fragment対応では文書URLと訪問URLが分離され、Anchorはblock・textの論理位置を持つ。
  この仕組みを活かすが、idのない文章も再レイアウト後に追跡できる位置表現が必要。
- `src/app/fetch.rs`は取得とHTML/text Parserを結合している。画像受信先を追加する際に
  transport・redirect処理とbodyの消費を分ける。`net::http::Transaction`は再利用する。
- HTTP要求はGET専用。Wi-Fi復帰と`Browser::suspend()`からの復帰ではGETをやり直す。
  POST追加時にこの2経路を必ず見直す。
- table cellは文章中心の構造である。画像とcontrolを含む内容を扱えるよう拡張する。
- 以前のbrowser所有memory 4 MiB上限は現在の定義にはない。入力、text、item等の
  個別上限と失敗可能な確保はあるが、画像・履歴・cacheを含む合計予算は新たに必要。

## 共通の設計契約

### メモリと失敗

表示中文書、次の文書、旧新layout、受信buffer、decoder作業領域、展開画像、入力値、
履歴復元用の結果、cacheの同時保持量を計上する。画像の圧縮byte数と展開画素数は
別の上限を設ける。寸法の乗算・座標加算はoverflowを検出する。

数値はStage 0で暫定値を決め、各段階の実機結果で見直す。上限なしの実装を先行させない。
入力依存の確保は失敗を返し、allocation失敗で基板全体をabortさせない。

優先順位は、表示中本文と編集中の値を守り、cacheを先に破棄し、新たな画像取得・
decodeを断念する。入力値や送信本文を切り詰めて送らない。履歴用のPOST結果を
保持できない場合は、後述の再送確認へ縮退する。

### 位置とレイアウト

表示用のpixel offsetと、再レイアウト用の論理位置を分ける。論理位置はblock、text内の
位置、画像/controlの識別子などと画面内offsetで表し、HTMLのidの有無に依存させない。
文書再取得で論理位置が消えた場合のfallbackと末尾へのclampを定義する。

画像寸法変更は画面更新単位でまとめる。初期は上限付きの文書全体再layoutでもよく、
実測なしに複雑な差分layoutを導入しない。ただし処理中にも入力・networkへ戻れることを
確認する。仮画像枠自体を見ている場合の位置維持規則もfixtureで固定する。

HTML本体は従来どおり完成後に差し替える。未完のHTMLを正常な完成文書として表示する
方式へは変更しない。その完成文書に対して画像が後から更新される。

### 取得の所有

画像jobにはページ世代と画像IDを付け、遷移後の結果を新しいページへ反映しない。
遷移・終了では関連するjobとsocketを解放する。中断・Wi-Fi復帰ではGETを先頭から
やり直し、受信済みの異なる応答を連結しない。失敗や未対応画像は局所的な表示にする。

HTTPSからHTTPへの画像取得・redirect、およびnetwork文書からのfile画像参照は拒否する。
local文書からのlocal画像を扱い、remote画像には通常の取得規則を適用する。
TLS表示は既存の未認証TLS方針を維持する。

## Stage 0: 境界と暫定上限を固定する（実装済み）

2026-09-12に以下を`browser/src/limits.rs`へ固定した。数値は実機の速度や通常ページの
互換性を確認して決めた最終値ではなく、後続Stageを無上限で始めないための暫定値である。

| 対象 | 暫定上限 |
| --- | ---: |
| 1文書の画像数 | 64 |
| 1画像の圧縮入力 | 512 KiB |
| intrinsic寸法 | 幅・高さ各1280 px、かつ1,048,576 pixel |
| decoder作業memory | 1 MiB |
| 仮画像枠 | 160×90 px |
| form / control | 32 / 256 |
| 1入力値 / 全入力値 | 4 KiB / 32 KiB |
| encoded request | 48 KiB |
| HTTP cache | 16 entry、1 entry 512 KiB、合計2 MiB |
| 拡張機能の合計所有memory | 6 MiB |

合計6 MiBは「1件の圧縮入力＋decoder作業領域＋RGB565の最大展開画像＋cache全体＋
form値」が同時に存在できる値であり、host testでこの関係を固定した。実装は個別上限と
合計予約の両方を満たすものだけ確保する。cacheを先に破棄し、それでも不足する場合は
新規画像の取得・decodeを局所的に失敗させる。

寸法属性はASCII 10進数の1以上だけを指定とし、0、overflow、符号、単位付き、
空文字列は未指定と同じに扱う。列幅は文字内容だけで先に確定し、画像はその内容幅へ
縮小する。画像から列幅を拡大しないことで循環を避ける。論理位置が消えた場合は同じ
blockの先頭、blockも消えた場合は直前のpixel offsetを文書末尾へclampする。

decoder候補は`zune-jpeg 0.5`と`png 0.15`のsourceを確認した。前者はMIT OR Apache-2.0で
`no_std`/`alloc`に対応するが、baseline/progressiveを一度にdecodeするAPIなので、poll分割と作業領域の
上限をこのコード側で保証できない。後者はこのversionで`std`依存がある。したがってStage 3で
そのまま採用せず、まずPNGのnon-interlaced、8-bit RGB/RGBAに限った分割decoderを採用する。
interlaced PNG、palette/grayscale/16-bit PNG、アニメーションPNGは画像枠内に`unsupported PNG`と出す。
JPEGは続く形式とし、baseline 8-bit 1/3 componentから開始し、progressiveは作業memoryとpoll分割を
保証できるdecoderを採用するまで`unsupported JPEG`とする。

fixtureはpureなparser/layout用を`browser/tests/fixtures/extension/`、network応答を
`tools/browser_fixture_server.py`へ置く。`i`診断に将来`images compressed decoded work forms values
cache extension`の現在値とpeakを追加する。

1. 画像数、圧縮入力、寸法・画素数、decoder作業量、総所有memoryの暫定上限を決める。
2. form数、control数、1入力値・合計入力値、encoded要求、cache容量の上限を決める。
3. JPEG・PNG decoderをno_std適合、ライセンス、作業memory、処理分割、対応variantで
   調査する。1形式目と、非対応variantの明示方法を記録する。
4. 仮サイズ、寸法属性の不正値・0の扱い、表列幅の計算順序、位置fallbackを固定する。
5. fixture追加先と各memory項目の診断表示を決める。

**完了条件:** 未決の数値と互換範囲が実装可能な値・規則になり、本書へ記録されている。
decoderの性能・実機動作はこの段階では未確認とする。

## Stage 1: 単一文書の表示基盤（実装・実機受入済み）

2026-09-12に履歴、現在ページ、再読込みの保存位置を文書内pixel offsetへ移行した。
上下キーは20 px、wheel 1 detentは60 px、PageUp/Downはviewport高から20 px引いた
量を動かす。Homeは0、Endは文書高からviewport高を引いた位置へclampする。

fragmentはanchorの先頭行のpixel yへ変換し、link focusは対象行の上下がviewport内に
入る最小量だけscrollする。hit testも同じpixel変換を使う。table cell、選択背景、
下線、A4文字はframebufferの上下clip band内だけを書き、部分的に見える行も描画する。
clip bandはviewport描画後に元へ戻す。

hostのpure testとrelease buildは成功した。短かった`built-in/table`を18行の表へ拡張し、
2026-09-12に人間が実機で確認して続行指示を出したため受入済みとする。
寸法変更後の論理位置保持fixtureはStage 2の画像ID/block位置導入時に
追加する。

- 先頭行中心のscrollをpixel offsetへ変更する。矢印・wheel・PageUp/Downの移動量を
  明示し、Home/End、hit test、focus追従も同じ座標変換を使う。
- A4文字、linkの選択背景・下線、table背景・罫線をviewportでclipする。
  framebuffer端へのclipだけでtoolbar・statusを守れるとは扱わない。
- fragmentの論理位置を新scroll位置へ変換し、戻る・進む・reloadの保存位置を更新する。
- 寸法変更を模したfixtureで、画面より上の領域が伸縮しても読み位置を保持する。

**完了条件:** release buildが通り、実機でtable、fragment、上下端の部分文字、link操作、
戻る・進む、system bar往復に回帰がないとの報告を得る。

## Stage 2: 画像要素と寸法確定（実装・実機受入済み）

HTML tokenizerは`src`、`alt`、`width`、`height`を上限付きで取り出し、Documentは最大64件の
画像IDごとに解決済みURL、alt、有効な指定寸法、text位置、link IDを保持する。
`browser/src/image.rs`は確保なしの逐次header調査でPNG IHDRとJPEG SOFの寸法を返し、
可変長metadataの受信中は`NeedMore`、上限超過は`TooLarge`とする。

layoutは画像を回り込みなしの独立領域として配置し、指定なしの軸だけ160×90 pixelの
仮寸法を使う。配置幅を超えた場合は縦横比を保って縮小する。画像だけのtable cellは
希望画像幅を列の希望幅へ加え、全列がtable幅へ収まらない場合だけ他列とともに縮小する。
確定したcell内容幅を上限とし、画像高を含むようrowを伸ばす。link内画像は矩形全体をhit領域とし、
文書順のfocus対象になる。decode前の現在は薄い背景と枠、altまたは`image pending`を描く。

再layout前に`ReadingPosition`で先頭のtext offsetまたは画像IDとobject内pixel offsetを
保存し、新layoutで同じ論理位置をpixel yへ戻せる。画像高が上方で40から240 pixelへ
変わるpure fixtureで、下の本文内の読み位置が保たれることを確認している。Stage 3は
intrinsic寸法判明時にこのAPIを実際の再layout経路へ接続する。

組み込み`http://built-in/images`は4種類の属性指定、本文幅への縮小、link画像、
table cell内画像をネットワークなしで並べる。hostでは228 unit testと29 fixture test、
release buildを通す。2026-09-12に人間が、4種類の寸法指定、本文幅への縮小、
link画像の操作、上下端clipとscrollを実機で確認した。table cell内画像は期待値の
説明が不足していたため未判定である。その確認で、tableに余裕があっても画像がalt文字幅まで
縮む問題が判明した。画像の希望幅も列幅計算へ入力し、余裕があれば指定300×120 pixelを維持、
全列が収まらない場合だけ比率を保って縮小するよう修正した。左右4 pixelのpadding内へ収め、
画像高と上下paddingをrowの外枠が完全に包むことは変わらない。修正後の300×120 pixel表示と
cell内配置も2026-09-12に人間が実機確認したため、Stage 2を受入済みとする。

imgのsrc、alt、width、heightを上限付きで取り出し、文書内の画像IDへ結ぶ。
本文・link内・cell内に画像領域を配置し、周囲の文章との順序を保つ。
回り込みは行わず、初期は画像を独立した領域として置く。

| 属性指定 | 初期領域 | intrinsic寸法判明後 |
| --- | --- | --- |
| 幅・高さ | 指定した幅・高さ | 領域を維持。指定比率で描画 |
| 幅のみ | 指定幅と仮高さ | intrinsic比率から高さを算出 |
| 高さのみ | 仮幅と指定高さ | intrinsic比率から幅を算出 |
| 指定なし | 仮幅・仮高さ | intrinsic幅・高さを採用 |

この決定後に配置先の幅へ縮小する。cell内では先に画像の希望幅を列の希望幅へ入力し、
全列の希望幅をtable幅へ一度だけfitした後、その確定内容幅を画像の上限にする。
画像を配置した結果から列幅を再計算しないため循環しない。CSS寸法は扱わない。

寸法解析は「追加byte待ち／寸法判明／不正・未対応」を返す。先頭の固定byte数で必ず
判明すると仮定せず、可変長metadataも上限内で逐次処理する。寸法が分かった時点で
再layoutを要求し、全画像のdecode完了は待たない。

**完了条件:** 仮領域、4種類の寸法指定、幅縮小、table cell内、link当たり判定、
遅い寸法判明時の読み位置保持を確認できるfixtureがあり、release buildが通る。

## Stage 3: JPEG・PNGの取得と表示（実装・実機受入済み）

PNGのpure decoderを先行実装した。対応範囲はnon-interlaced、8-bit RGB/RGBA、
filter 0〜4で、RGBAは白背景へalpha合成してRGB565へ変換する。圧縮入力512 KiBと
展開作業1 MiBをdecode前に検査し、palette、grayscale、16-bit、interlace、不正filter、
切断、展開上限超過は画像単位のエラーとして返す。`miniz_oxide 0.9.1`は
`default-features = false`と`with-alloc`でno_std利用する。PageはDocument画像IDと同じ添字で
decode結果を所有し、framebufferは最近傍で予約領域へ拡縮しながらviewport clipする。
組み込み`images`ページには2×2 RGBA PNGをdecodeして全寸法・link・cell領域へ描く経路を
接続し、2026-09-12に実機受入済みとなった。`file:`画像はHTML表示後に1件ずつ、
1 poll最大16 KiBで読み、512 KiB上限内の完成byte列を同じPNG decoderとPage画像IDへ
接続する。遷移・終了時はfile handleを閉じ、失敗はページ全体ではなく画像単位に留める。
2026-09-12にlocal PNGの取得・decode・通常表示を実機確認した。最初のfixtureは文書が
viewportより短く、画像をviewport境界へ通すclip確認ができなかったため、画像前12段落、
後18段落を追加し、更新後fixtureのscroll・clipも同日に実機受入済みとなった。

HTTP画像は文書と同じDNS、TLS、redirect、16 KiB/pollのFetchを画像modeで使い、成功した
`image/png`だけを最大512 KiBの完成byte列としてdecodeする。HTTPS文書からHTTP画像への
downgradeとnetwork文書から`file:`画像を拒否する。遷移、system barへのsuspend、回線断で
socketを閉じ、同じ画像IDから再開する。同一ページの同一解決済みURLは先にdecodeした
RGB565を`Rc`で共有し、再取得しない。fixture serverの`/images/stage3.html`は同じPNGを
384×256と192×128で2回参照する。市松模様より描画不良を見分けやすいgradient、半透明の
太陽、山、grid、色帯を含むfixtureへ更新し、2026-09-12に両表示寸法で実機受入済みとなった。

2形式目は`zune-jpeg 0.5.15`を`default-features = false`でno_std接続した。ライセンスは
MIT OR Apache-2.0 OR Zlibである。decoderへ渡す前にmarker列を検査し、SOF0、8-bit、
1または3 componentのbaseline JPEGだけを許可する。progressiveを含む他SOF、別precision、
4 componentは`unsupported`とし、圧縮入力512 KiB、幅・高さ各1280 px、1,048,576 pixel、
RGB作業領域1 MiBを先に検査する。decoder出力はRGBからRGB565へ変換し、PNGと同じ画像slot、
拡縮、clip、重複共有経路へ接続した。Fetch画像modeは`image/jpeg`を受理する。
hostではbaseline decodeとprogressive事前拒否をtestし、RISC-V release buildまで成功した。
LAN fixtureの同ページには16×12のgradient baseline JPEGを384×288で追加した。
このJPEG経路も2026-09-12に実機受入済みとなった。

PNGは全chunkのtypeとdataからCRC-32を検証し、不一致を破損画像として拒否するよう
hardeningした。取得失敗とdecoderの`unsupported`、`malformed`、上限超過、OOMは画像IDごとの
失敗状態へ保存し、予約領域内へ理由を表示する。別画像やページ全体の表示は継続する。
fixtureにはIHDRのCRCを1 bit壊した`/images/bad-crc.png`を追加した。この失敗表示経路は
host testとrelease buildを通し、2026-09-12に他画像と本文を維持した局所失敗表示を
実機受入済みとなった。

decode成功時はDocument画像IDへintrinsic幅・高さを設定してLayoutを再構築する。属性が
両方あれば指定値を維持し、片方だけならintrinsic縦横比で他方を算出し、両方なければ
intrinsic寸法を採用してから配置幅へ縮小する。再構築前のviewport先頭はtext offsetまたは
画像IDと内部offsetで保存し、新layout上の同じ論理位置へ戻す。focus中linkもlink IDから
新しい表示順へ結び直す。fixtureの`/images/slow.png`はHTTPの5秒idle timeoutを発生させないよう
2秒ごとに5分割して約8秒で送り、160×90の仮枠から
192×128のintrinsic寸法へ変化する。最初のfixtureは10秒無通信となりHTTPの5秒idle timeoutで
正しく切断されたため、2秒ごとの分割へ修正した。修正後は2026-09-12に画像表示と読み位置維持を
実機受入済みとなった。

画像開始時にも親文書の接続がPinned TLSなら、画像URLがHTTPSかつpin登録済みhostであることを
要求する。HTTPSからHTTP、Pinned TLSから未認証TLS、network文書からlocal fileへの移行は
画像枠内の拒否表示に留める。取得開始、local read、通信中の失敗も同じ画像単位の状態へ保存する。
大画像fixtureとして640×400 RGBA PNG（展開filter byte込み約1000 KiB、圧縮約107 KiB）を追加し、
上限近くのdecode・RGB565変換・描画を確認できるようにした。2026-09-12に大画像のdecode・描画・clip、
遅延中の遷移中止、Wi-Fi切断復帰、system bar往復を実機受入済みとなった。Pinned TLS文書から
未認証画像への拒否だけは実機fixture未実施である。この確認用に2026-09-13、LAN fixtureへ
`/images/pinned.html`を追加した。Pinned TLSで開いたページから、同じpin済みhostのHTTPS画像（表示）、
平文画像（`HTTPS downgrade refused`）、pinの無いhostの画像（`TLS identity downgrade refused`）、
pinと鍵が合わないEd25519 listenerの画像（接続失敗）を並べる。手順は`BROWSER.md`に記載した。
最初の確認では3が名前解決失敗になった。基板のpinがfixture serverのアドレスと一致せず、ページが
`TLS UNVERIFIED`で開かれていたためで、pinの設定を合わせた再確認で期待どおりとなり、2026-09-13に
実機受入済みとなった。

- local画像で1形式目のdecode・縮小・RGB565描画を接続し、network画像へ進む。
  PNG透明画素の背景合成も定義する。2形式目も同じ画像領域・状態遷移へ接続する。
- Fetchのbody消費をHTML/textと画像に分ける。寸法用の別要求を発行せず、同じGETで
  寸法を検出して受信を継続する。不要な圧縮入力の二重保持を避ける。
- HTML表示後、画像を1件ずつ取得する。同一ページ内の同一URLは取得・画素を共有し、
  各imgの表示寸法は独立させる。この共有はHTTP cacheとは別の寿命を持つ。
- 非対応形式・破損・上限超過・切断・OOMは画像領域の失敗表示にする。寸法判明済みなら
  その領域、未判明なら仮領域を維持する。
- decodeと描画の長い処理が入力・network処理を塞がないよう分割または上限を設ける。

**完了条件:** 両形式の採用したvariant、4種類の寸法指定、大画像、透明PNG、cell内、
重複URL、遅延・破損応答、遷移中止、Wi-Fi/system bar復帰を実機で確認した報告を得る。

完了後の実サイト確認で、画像が文章やnested tableと同じ外側cellにあると画像boxを作らない
制約が判明した。cell blockを画像だけの場合に限定せず、text offset順に「文章区間・画像・
文章区間」を積むlayoutへ修正した。画像代替文字列は描画対象から外し、全画像高をrowspanを
含むcell高へ加える。nested tableそのものの列・罫線対応はDocumentの単一table状態と
1 cell＝1 blockモデルの変更を伴うためスコープ外とし、内部文章の平坦化を維持する。
2026-09-12に実サイトで混在cell内画像を実機受入した。その際、nested tableの各開始・終了tagへ
改行を入れていたため内容が分解して見えた。内側tableは外側cellのblockに閉じ込めたまま、行末だけ
改行し、同じ行のcellを` | `で区切るcompact textへ変更した。nested tableを独立した罫線付きtable
として再帰layoutする対応は引き続きスコープ外である。このcompact表示も2026-09-12に
実サイトで実機受入済みとなった。

## Stage 4: 共通TextInputView（実装・実機受入済み）

Rustのstructとして共通のTextInputView相当を準備し、まずアドレス欄を移行する。
配置moduleは既存GUI構成を確認して決め、URLやHTTPに依存しない境界を保つ。

- 編集状態: UTF-8本文、caret、選択範囲、byte上限。位置はUTF-8境界を守る。
- 編集操作: 文字列挿入、選択範囲置換、削除、移動。Key::Ascii専用APIにしない。
- 表示: 共通font metricsで測定し、caret・選択・clip・入力位置へのscrollを描画する。
- 用途設定: 単一行／複数行、入力制限、Enterによる確定／改行。URL検証と送信は外へ置く。
- 入力方式の境界: 物理キーから編集操作へのadapterと、文字列確定操作を分離する。
  将来の未確定文字列・その選択範囲は確定本文と別に持てる契約を用意する。
  未確定文字列をURLやformへ送る構造にしない。

今回IME、かな漢字変換、候補表示、日本語入力手段は作らない。UTF-8の保持と表示は
日本語入力対応の完成を意味しない。結合文字等の移動・削除単位は明記し、将来差し替え
可能にする。全アプリの入力欄を今回一括移行することは要求しない。

pureな`browser::text_input::TextInput`へUTF-8本文、Unicode scalar境界のcaret・選択、
byte上限、単一行／複数行、文字列置換、未確定文字列を実装した。アドレス欄の旧ASCII専用
編集状態をこの部品へ移し、`Ctrl+A`全選択、選択置換、比例幅測定によるcaret追従と選択背景を
接続した。物理キーadapterは現時点ではASCII確定文字だけを渡し、IMEや日本語入力手段は
追加していない。編集操作は前後の表示位置からdamageを求め、caret移動は旧・新caret、選択変更は
旧・新選択範囲、文字変更は変更位置以降だけをwritebackする。横scrollが変わる場合だけアドレス欄の
32 pixel高矩形全体を更新する。framebuffer描画にもdamageと同じ横clipを掛け、直接scanoutが途中の
全体消去を表示しないようにした。ボタンと南京錠の不要なwritebackも止めた。host testとrelease
buildは通過し、2026-09-12に既存編集操作、選択、横scroll、damage限定描画を
実機受入済みとなった。

**完了条件:** アドレス欄の既存操作が保たれ、文字列単位の挿入・置換でUTF-8を壊さず、
単一行／複数行をフォームから再利用できる。実機で既存keyboardと表示を確認する。

## Stage 5: フォームの入力とGET送信（実装・実機受入済み）

- form定義とcontrol IDをDocumentに持ち、編集値とfocusを別状態で保持する。
- input text・hidden・submit、buttonを実装する。labelとcontrolの関係を扱い、
  table cell内も通常本文と同じ入力Viewを配置する。
- name/value、disabled、初期値、押したsubmit buttonを処理する。送信値は順序付きの
  名前・値の組とし、同名fieldを辞書で潰さない。
- action、methodの既定と相対URL、query構築、percent encodingを一箇所で処理する。
  送信符号化はUTF-8とし、Shift_JIS文書からの送信互換範囲も明記する。
- Tabはlinkとcontrolを文書順に辿る。編集中のq/r/[/]等は文字として扱い、Browser操作と
  衝突させない。Enterによる送信条件とEscapeによる編集解除を定義する。
- 未対応の送信形式や必須のcontrolがある場合、不完全な値を黙って送らず送信前に示す。
  frameや別windowへのtargetは実装せず、単一文書での扱いを明示する。

**完了条件:** echo fixtureで実際のquery、同名field、空値、記号、hidden、button、
disabled、入力上限、table内操作を人間が確認し、意図しない送信がない。

送信処理の最初のpure境界として`browser::form::encode_query`を追加した。入力は辞書ではなく
順序付きの名前・値pair列で、同名fieldと空値を保持する。UTF-8 byte列、spaceの`+`、記号の
uppercase percent encodingを一箇所で行い、生成途中でもURL上限を超えた時点で失敗する。
GET actionへ適用すると既存queryを置換しfragmentを除去し、完成URL全体の上限も再検査する。
HTML tokenizerは`action`、`method`、`type`、`name`、`value`、`disabled`、labelの`for`を
bounded属性として保持する。Documentには安定したform/control ID、解決済みaction、method、
文書順のcontrol、text・hidden・submit・unsupported種別、初期値、disabled、要素IDを追加した。
編集値とfocusはDocumentへ入れず、表示側のpage stateで持つ境界を維持する。
GET送信対象の収集はdisabledと空nameを除外し、hiddenを含め、実際に押されたsubmitだけを
含める。未対応method/controlはURL生成前に拒否する。visible controlは専用Blockと
`Layout::ControlBox`を持ち、hiddenは場所を取らず、後続本文との重なりを避ける。
firmware rendererへ初期値、disabled配色、text field、submit buttonを接続し、組み込み
`http://built-in/forms`を表示専用の実機受入fixtureとして追加した。fixtureは複数viewport分の
本文をcontrol群の前後に置き、スクロール時の上下端clipも確認できる。この時点ではfocus・編集・
送信操作は未接続である。上端を越えたcontrolはviewport上端へbox全体を押し戻さず、元の
screen位置からframebufferのvertical clipを通して描画する。
この上端clipは2026-09-13に実機受入済みとなった。Tab対象をlinkと有効なvisible controlの
layout位置順へ統合し、hiddenとdisabledを除外した。control focusはtextでは青枠、submitでは
青い面で表示し、Tab移動時は対象全体が入るようscrollする。touch hitもcontrolをlinkより先に
判定してfocusする。focus順序、表示、disabled/hiddenのskip、自動scrollは2026-09-13に
実機受入済みとなった。focus移動は旧対象と新対象の矩形だけをclipして通常のviewport rendererを
再利用し、viewport全消去と全体writebackを避ける。このfocus局所再描画は2026-09-13に
実機受入済みとなった。その後、描画前に複数のfocus変更が
届くと最後の旧新対象がそれ以前のdamageを上書きし、古いfocus表示が残る場合が確認された。
最大4対象を重複なしで蓄積し、それを超えた場合だけviewport全体の再描画へ退避するよう修正した。
この複数focus damage処理は2026-09-13に実機受入済みとなった。text controlはTabでfocusした時点、
またはtouchで共通`TextInput`の単一行編集を開始し、追加のEnterを要求しない。Escape/Enterで
値を保持して終了し、Tabで値を保持して次へ進む。
編集中は印字可能ASCIIをBrowser shortcutより優先し、UTF-8境界、選択、最大4096 byte、caretを
共通実装へ委ねる。再描画はcaretの旧新位置、選択の旧新範囲、または文字変更位置以右だけを
horizontal clipし、横scrollが変わった場合のみtext内側全幅へ広げる。Tabから直接編集へ入る操作、
その他の編集キー、編集時の局所再描画は2026-09-13に実機受入済みとなった。
単一行text編集中のEnterは
現在値を保持してsubmit controlなしで暗黙送信し、submitはEnter/touchで起動してそのcontrolだけを
送信pairへ含める。組み込みfixtureのactionは`/forms`自身とし、同名hidden `q`、空のhidden
`empty`、submit `go`を送信後のaddress欄で観察できる。このGET UI接続は2026-09-13に
実機受入済みとなった。`button`は既定でsubmitとしてDocument controlを
作り、`value`属性の送信値と、要素内空白を畳んだ表示ラベルを分けて保持する。button内部のinline
markupは文字だけをlabelへ取り込み、通常本文へ漏らさない。button内タグのstyleと構造はすべて
無視し、複雑な子要素の描画はスコープ外とする。組み込みfixtureへ`Apply changes`を追加し、起動時は
`mode=advanced`だけをsubmit pairとして加える。このbutton接続は2026-09-13に実機受入済みとなった。
明示的な`label for=id`はparse完了時にcontrol IDへ解決し、layout上のlabel文字をtapすると対応する
有効controlへfocusする。text controlなら直接編集へ入る。後方参照を扱い、対応先なしとdisabledは
操作しない。このlabel操作は2026-09-13に実機受入済みとなった。
TableCellは自身のtext・image・control範囲を保持し、layoutはこれらを出現位置順に縦へ積む。
visible controlの高さをrow測定へ含め、boxをcell padding内の利用可能幅へ収める。controlを含む
columnは最大320 pixelを希望し、table全体のviewport fitで必要な場合だけ縮める。cell内controlも
通常と同じfocus、label hit、編集、GET送信経路を使う。組み込みfixtureへ`Cell query`を追加した。
このtable cell内controlは2026-09-13に実機受入済みとなった。
省略、未知、または専用UI未実装の`input type`はText stateへfallbackし、通常の単一行textとして
編集・GET送信する。組み込みfixtureのmain GET formへ`type=date`の`Date fallback`を追加した。
このinput type fallbackは2026-09-13に実機受入済みとなった。
GETでないmethodはURL生成前に拒否し、現在pageと入力値を保持してstatusへ理由を表示する。
組み込みfixtureのPOST formで`UnsupportedMethod`を確認できる。`target`は保持・適用せず、単一文書
navigationだけを扱う。このPOST拒否は2026-09-13に実機受入済みとなった。

## Stage 6: POSTと再送・履歴（実装・実機受入済み）

HTTP要求をmethod、URL、限定したheader、上限付きbodyで表現する。Content-Lengthを
正確に作り、headerへの改行混入を拒否し、既存GET利用側の契約を維持する。
送信処理もpollごとに戻り、受信側と同様に中断できるようにする。

- POSTはurlencodedのみ。301/302でPOSTをGETへ変更、303もGETへ変更しbodyを破棄する。
  307/308はmethod/bodyを保持する。別originへのbody再送は送信先を示して確認する。
  HTTPS降格拒否とredirect回数上限を維持する。
- Wi-Fi断・system bar中断・timeoutからPOSTを自動再送しない。送信後の結果が
  分からない場合は「結果不明」と表示し、再送には明示操作を要求する。
- reloadや履歴でPOSTをGETへ黙って変換しない。保持中のPOST結果は通信せず再表示し、
  失われた結果は再送確認へ進む。確認後に再送するための要求もなければ入力元へ戻す。
- POST結果の復元用保持はHTTP cacheと分け、有限の予算を設ける。保持禁止の応答や
  memory不足では履歴用複製を残さず、再送確認へ縮退する。
- 送信成功・結果不明の表示を分け、上限やallocation失敗で入力値を捨てない。

**完了条件:** fixtureのPOST受信回数で二重送信がないことを確認する。切断、app中断、
reload、戻る・進む、各redirectと再送取消を実機で確認した報告を得る。

method・URL・bodyを一体で保持するpureな`browser::request::Request`を追加した。GETのwire形式を
維持し、urlencoded POSTには固定`Content-Type`とbody byte数そのものの`Content-Length`を付ける。
host、request target、User-Agentに改行やcontrol byteを許さず、encoded bodyを48 KiBへ制限する。
form側はGET queryとPOST bodyで同じsuccessful control収集・UTF-8符号化を使い、POSTではactionの
既存queryを維持してfragmentだけを除く。HTTP transactionはheadとbodyを一続きの要求byte列として
pollごとに部分送信する。これらはhost testとrelease buildを通過した。

Browser UIからnetwork HTTP(S) actionのPOSTを開始する経路を接続した。301/302/303はGETへ変更して
bodyを破棄し、307/308は同一originならmethod/bodyを維持する。別originの307/308は確認UIができるまで
`post-redirect-confirm`で未送信停止する。送信開始後のWi-Fi断、timeout、中止、system bar中断では
GETのように自動再開せず、応答head前なら結果不明、head後なら応答中断としてstatusへ残す。
送信前または送信失敗では元pageと入力値を保持する。POST結果の文書は表示するが、結果文書の保持は
未実装なのでreloadと、その項目へ戻るback/forwardは再送せず理由を表示する。LAN fixtureへ
`/forms/post.html`とmanifest walk対象外の`/forms/post/echo`を追加し、method、POST受信回数、action
query、Content-Type、Content-Length、bodyを応答本文へ表示する。この通常POST UI経路は
2026-09-13に実機受入済みとなった。

301/302/303/307/308を1画面から個別に送る`/forms/post-redirects.html`も追加した。各statusの
source POST回数と最終要求回数を独立して数え、最終method、query、Content-Type、
Content-Length、bodyを結果pageへ表示する。カウンタを持つsource/result endpointはmanifest
walk対象外とした。host上のhandler応答検査は通過し、このredirect UI経路は2026-09-13に実機受入済みとなった。

続いて、再送確認、別origin redirectの確認、POST結果の履歴保持を実装した。status行の確認は
送信先のscheme・host・portを示し、`y`／Enter／`Send (y)`でだけ送る。`n`／Escape／
`Cancel (n)`・他の場所のタップでは送らない。確認中は`Ctrl+Q`以外のキーを確認の答えにだけ使う。
Wi-Fi断、timeout等、system bar中断で止まったPOSTは、未送信なら`not sent`、送信開始後なら
`result unknown`として確認へ進む。読み手の中止と、応答を受信したが表示できなかった失敗は確認を
出さない。別originの307/308は、`file:`、HTTPS降格、Pinned TLS降格の拒否を先に適用した後で
redirect後の要求を保持して確認し、承諾時はredirect回数を引き継いで送る。

POST結果の文書は、離れる時に作る履歴項目へ保持する。HTTP cacheとは別に
`MAX_RETAINED_POST_RESULTS = 2`、`MAX_RETAINED_POST_RESULT_BYTES = 320 KiB`、再送用body
`MAX_RETAINED_POST_REQUEST_BYTES = 96 KiB`を`limits.rs`へ追加し、既存の6 MiB合計予算のhost
testへ加えた。保持するのはparse済みDocumentだけで、layoutと画像は復元時に作り直す。
1件で上限を超える結果と`Cache-Control: no-store`の結果は保持せず、予算超過時は表示中pageから
遠い項目の文書、次に要求bodyを捨てる。back/forwardは、文書があれば通信せず再表示し、なければ
再送確認、要求もなければform元pageへGETで戻る。POST結果上のreloadは常に再送確認とする。
確認の取消、送信前失敗、通信断では、back/forwardのために取り出した履歴項目を元の場所へ戻す。
`i`診断へ保持件数と文書・body量を`post`として追加した。

LAN fixtureへ`Cache-Control: no-store`の`/forms/post/echo-no-store`、約400 KiBの
`/forms/post/echo-large`、plaintextからTLS listenerへ307/308を返す
`/forms/post/redirect/cross-307`・`cross-308`を追加した。host testとrelease buildは通過し、
fixture serverのheader・redirect先・POST回数はhost上で確認した。保持からの再表示、reload・
no-store・予算超過での再送確認と取消、別origin 307/308の確認、通信断・system bar中断からの
確認、`i`の`post`上限は2026-09-13に実機受入済みとなった。

## Stage 7: フォームcontrol拡充（実装・実機受入済み）

checkbox、radio、textarea、selectを順に追加する。未選択checkboxの除外、radio group、
選択値、複数行改行の送信を扱う。textareaはStage 4の共通Viewを利用する。
各controlの初期値、disabled、focus、pointer操作、送信値をfixtureで比較する。
file upload、passwordを使ったログイン、cookie、JavaScript依存formは追加しない。

**完了条件:** 追加controlを含むフォームで表示値と実際のGET/POST内容が一致し、
アドレス欄・既存controlに回帰がないとの実機報告を得る。

最初にcheckboxとradioを実装した。tokenizerは`checked`をboolean属性として保持し、Documentの
controlへ`Checkbox`／`Radio`種別と初期checkednessを持たせる。同じform ownerと同じ空でない
nameのradio groupでは文書順で最後の`checked`だけを残し、`value`省略時は`on`とする。
page stateは編集値と別に`control_checked`を持ち、`form::activate_checkable`がcheckboxの反転と
radio groupの排他選択を行い、状態が変わったcontrol IDを返す。送信はchecked、enabled、
name非空のものだけを文書順のpairへ含める。layoutは1行高の正方形boxとし、table列の希望幅を
320 pixelへ広げない。Tab、Enter、focus中のSpace、box・label touchで操作し、再描画は状態が
変わったcontrolの矩形だけとする（5件以上ならviewport全体）。組み込み`/forms`と
LAN `/forms/post.html`へfixtureを追加した。このcheckbox／radio対応は2026-09-13に実機受入済みとなった。

続いてtextareaを実装した。tokenizerは`textarea`をraw text要素とし、script・styleと違って内容を
捨てずtextとして渡し、文字参照を復号する。終了tag候補が一致しなかった`<`・`</`と名前の
一部も内容へ戻す。Documentは開始tag直後の改行1つを除き、改行をLFへ揃えた内容を初期値にし、
本文へは出さない。`rows`は既定2、最大8とする。4 KiBを超える初期値は`value_overflow`とし、
編集とformの送信を拒否して切り詰めた値を送らない。送信時の符号化はすべての改行をCRLFへ
揃える。`TextInput`へ文字単位で折り返す`wrap_rows`、caret行を求める`row_of`、表示行間を
移動する`move_row`を追加した。Browserは複数行編集でEnterを改行、上下キーを表示行移動とし、
caret行が見えるようbox内を行単位でscrollし、変更ごとにtext領域全体を再描画する。layoutは
行数分の高さのboxを置き、table cellでも同じ高さを使う。組み込み`/forms`とLAN
`/forms/post.html`へfixtureを追加した。このtextarea対応は2026-09-13に実機受入済みとなった。

最後にselectを実装した。tokenizerは`selected`と`multiple`をboolean属性として保持する。Documentは
optionを`SelectOption`（label、value、初期selectedness、optgroupから継承したdisabled）として
controlの範囲へ持ち、`MAX_SELECT_OPTIONS = 4096`を`limits.rs`へ追加した。select内では
option/optgroup以外を無視し、input・textarea・formはselectを閉じる。単一選択は最後の`selected`、
なければ最初の有効optionを選ぶ。page stateは`option_selected`を持ち、`form::choose_option`が単一
選択の置換とmultipleの切替を行う。送信は選択中かつ有効なoptionごとのpairとする。Browserは
boxの下／上へ最大10行の一覧を重ね、キー・touchで選び、一覧矩形だけを再描画する。一覧はscroll、
再layout、page置換、確認表示で閉じる。組み込み`/forms`とLAN `/forms/post.html`へfixtureを
追加した。実機確認で、disabled optionを選ぼうとした時の`this option is disabled`が一覧を閉じる
処理でstatusから消えることが判明したため、選択できなかった場合は一覧を開いたままにするよう
修正した。修正を含むselect対応は2026-09-13に実機受入済みとなり、Stage 7を受入済みとする。

## Stage 8: HTTPキャッシュ（実装・実機受入済み）

GET成功応答の有限byte数LRUから開始する。HTTP本文の再利用と、履歴・入力状態の復元を
混ぜない。最初にETag条件付き要求と304、その後に鮮度期間内の通信省略を実装する。

- 304用にはvalidatorだけでなく再利用できる本文または対応する表現が必要。
  Stage開始時に保存表現を決め、生HTMLを保持しない現状との費用差を記録する。
- URLのfragmentをkeyに含めずqueryは保持する。Varyを扱えない応答とVary:*は保存しない。
  no-storeは保存禁止、no-cacheは再利用前に検証する。Date/Age/max-age等を含む鮮度計算と
  304時のmetadata更新を定義し、曖昧な場合は通信する。
- reloadは再検証、強制再取得はcacheを使わない操作としてUIへ追加する。
- POST自体は保存しない。成功した更新要求に関係するGET cacheを無効化する。
- cache確保失敗は閲覧失敗にしない。表示中に参照されるデータをLRUから破棄しても
  不正参照にならない所有規則を設ける。decoded画像cacheは独立の追加最適化とし必須にしない。
- 永続化、offline fallback、cookie付き応答の共有は対象外。

**完了条件:** fixtureの要求回数・headerで200/304、鮮度期間内の通信省略、no-store、
no-cache、Vary、強制再取得、LRU追い出し、POST後の無効化を実機確認した報告を得る。

保存表現は、転送符号を外した応答本文と`ETag`・media type・charsetとした。文書モデルは生HTMLを
持たないため、304後の再表示は本文をparserへ再投入する。

最初の段階として、ETag条件付き要求と304をRAM内LRU（16 entry・2 MiB）で実装したが、実機確認前の
2026-09-13に人間から仕様変更の指示があった。変更後の仕様は次のとおりで、RAM内LRUは置き換えた。

| 項目 | 変更後 |
| --- | --- |
| 保存先 | ファイルシステム。既定かつ現在は固定で`/tmp/browser-cache/`以下を構造化して保存 |
| 期限 | HTTP由来（`max-age`、`Expires`−`Date`、`Age`）、指定がなければ既定1時間。期限内は通信を省略し、期限切れは再検証に使わずpurge |
| 容量 | 1 entry 512 KiBだけ。全体上限は設けず、書き込み時の容量エラーでpurgeして再試行 |
| purge | 期限切れによる削除に加え、容量エラー時は期限切れ、それがなければ最終利用の古い順 |
| 履歴 | 従来どおりメモリ |

pureな`browser::cache`はURLのkeyとFNV-1a hash、`<bucket>/<hash>.body`・`.meta`の配置、metaの
テキスト形式、鮮度計算とIMF-fixdateの解析、purge順を持つ。時刻はRTCでなく起動からのミリ秒で、
`/tmp`がリセットで消えるため常に有効である。`http::Head`は`no-cache`、`max-age`、`Age`、`Date`、
`Expires`も読む。Fetchは期限内の`200`だけをmeta（鮮度秒、ETag、type、charset、no-cache）と本文に
して返し、検証付き要求の`304`は`NotModified`として鮮度の更新情報とともに返す。
`app::cache_store`は検索（期限切れはその場で削除）、段階的な読み出し`CacheRead`と書き込み
`CacheWrite`、bucket単位の定期sweep、容量不足時のpurgeを持つ。Browserは期限内なら通信せずに
ファイルから表示し、`r`は期限内でも再検証、`R`はcacheを見ない。書き込みは表示後に進め、書き込み中は
次の画像取得を待たせ、次の遷移前に書き終える。RAMに持つ本文は常に1件以下で、6 MiB合計予算の
host testはcache全体ではなくこの1件を数える。`MAX_HTTP_CACHE_ENTRIES`・`MAX_HTTP_CACHE_BYTES`は廃止し、
`DEFAULT_CACHE_FRESHNESS_SECS`と`MAX_CACHE_META_BYTES`を追加した。`i`は回数（利用、304、保存、purge）
を表示する。LAN `/cache/`以下へ`max-age`、`Expires`、headerなし、`no-cache`、`max-age=0`、容量を
埋める`sized.html`のfixtureを追加した。このファイル保存cacheは2026-09-13に実機受入済みとなった。
確認時に、cacheから読む間もstatus行にloadingが出ることを質問されたが、表示中ページを完成まで
差し替えない設計どおりの挙動として変更しなかった。

続いて、POST後のGET cache無効化を実装した。RFC 9111 4.4に従い、POSTの各hopが`2xx`・`3xx`の応答
headを返した時点で、送信先URLと同じoriginのredirect先URLをFetchが記録する。Browserはfetchの各
stepの後にこれを受け取り、cache保守処理の最初に該当entryを削除し、同じURLの書き込み中本文も捨てる。
中止や失敗で終わったPOSTでも応答head後なら無効化される。`i`へ無効化数`x`を追加し、LAN fixtureへ
1時間cacheされる`/cache/counter.html`と、同じURLへのPOST・303で戻るPOSTを追加した。host testと
release buildは通過し、2026-09-13に実機受入済みとなった。`Content-Location`による無効化は扱わない。
これでStage 8の完了条件に挙げた項目が揃い、Stage 8を受入済みとする。

## 各Stageの検証と実機依頼

エージェントは変更後に`cargo build --release`を実行してビルド成否を報告する。
`cargo run --release`、`espflash`、書き込み、serial取得、実機操作は実行しない。
build成功だけでは表示・通信・入力の動作を確認済みにしない。

各Stageで必要なfixtureと診断を用意し、正確な追加URLと期待値をその段階の報告へ書く。
network fixtureは`tools/browser_fixture_server.py`へ追加する。既存の起動方法は
人間がLAN上のPCで次を実行し、同じLANのTab5から開く形である。

```sh
python3 tools/browser_fixture_server.py
# firmware書き込み後、Tab5のConsoleから（PCの実IPへ置換）
browser http://<PCのIPv4>:8080/
```

既存回帰確認は`browser http://built-in/table`と
`browser http://built-in/fragments`を使う。必要な周辺機器はCardKBまたはTab5 Keyboard／
USB keyboard、touch、USB mouse。local画像確認にはRAMルートへ置いたfixture、または
対応filesystemのSD/USB媒体を使い、その配置手順を実装時に明記する。

人間には以下の結果を依頼する。

| 段階 | 期待する挙動 | 失敗時の症状 |
| --- | --- | --- |
| 表示基盤 | 端の文字・罫線がclipされ、読み位置・fragment履歴を維持 | barへの描画漏れ、残像、誤jump、hit位置ずれ |
| 画像 | 仮領域から寸法確定し、失敗画像があっても本文を読める | 大きな位置飛び、cell崩れ、停止・再起動、古いpageの画像混入 |
| 入力View | caret・選択・編集が正しく、app往復でも編集中の値を保持 | UTF-8破損、caretずれ、文字入力による終了・reload |
| GET/control | echo内容が画面と一致 | 値欠落、同名値消失、disabled混入、上限で切れた送信 |
| POST | 1操作に1送信。結果不明から勝手に再送しない | serverの受信counter増加、無断再送、誤った成功表示 |
| cache | 指示どおり再検証・再利用・破棄 | 古い応答再利用、禁止応答保持、強制取得でも通信なし |

全段階で反復操作後のheap・socket、入力応答、display underrunの有無を人間に確認して
もらう。回数や測定値は報告されたものだけを記録する。結果待ちのStageは未確認のまま
残し、推測で受入済みにしない。

## 文書の更新

実装を変更したStageで`BROWSER.md`を現状へ更新し、HTTP変更は`NETWORK.md`、共通入力は
`INPUT.md`、描画API変更は`GRAPHICS.md`、責務追加は`FILE_LAYOUT.md`等も必要に応じて
同期する。実機未確認であることと、実装済みであることを分けて記載する。
新規docsを増やす場合はDESIGN.mdの索引へ追加する。

README.mdは変更しない。実装により古くなる記述があれば、該当箇所と不一致を最終報告し、
詳細をdocsへ委ねる案を示す。本書の実機結果は人間からの報告後にのみ追記する。
