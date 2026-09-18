# Browser画像キャッシュ改良・静止WebP対応計画

> 索引: [`../../../DESIGN.md`](../../../DESIGN.md)
> この文書は作業計画と実機での判断記録です。現在の実装仕様は
> [`../../BROWSER.md`](../../BROWSER.md)、[`../../NETWORK.md`](../../NETWORK.md)、
> [`../../FILESYSTEM.md`](../../FILESYSTEM.md)とコードを優先してください。

## 状態: 完了（8 MiB候補は棄却、実Wi-Fi切断回帰は見送り）

| Stage | 内容 | 状態 |
| --- | --- | --- |
| 0 | WebP decoder候補と現行画像memoryのbaseline調査 | 完了（候補採用・release／ELF検査。追加前比較値は取得不能として記録） |
| 1 | URL単位の画像entryと容量計上をPNG／JPEGで導入 | 完了（専用entry化はせず、共有`Rc`のURL alias単位で容量・LRUを一元管理） |
| 2 | viewport優先schedulerと全画像単位LRU | 1往復を実機受入済み |
| 3 | 圧縮入力・寸法・画素数・HTTP cache上限の緩和 | 第1〜4段を実機受入済み（8 MiBはPANICで棄却、安全版の4 MiBを再確認） |
| 4 | 静止WebPのdecodeと既存画像経路への接続 | 完了（基本経路とHTTP cacheからの再decodeを実機受入、大画像性能は未測定） |
| 5 | fixture、release build、回帰試験 | 完了（WebP・LRU統合fixture、293 unit test・29 fixture test・release／ELF検査通過） |
| 6 | Tab5実機受入、現状文書同期、計画の完了 | 完了（実Wi-Fi切断回帰だけを人間の判断で見送り） |

## 結論

実装順は次で固定する。

1. WebP decoder候補を小さく検証し、RISC-V `no_std` build、対応形式、作業memoryを把握する
2. 製品経路の変更はPNG／JPEGによる画像cache改良から始める
3. 全画像を文書順にdecodeする方式を、viewport優先のlazy decodeへ変える
4. decode済みRGB565を現在page内の解決済みURL単位で共有し、全画像単位のLRUで管理する
5. memory上限を実測に基づいて緩和する
6. 最後に静止WebPを同じcache・取得・失敗経路へ追加する

WebPを先に製品経路へ追加しない。現在の全件保持のまま形式と上限だけ増やすと、画像の多い
pageでmemory圧迫を悪化させ、直後のcache改良で所有構造とschedulerを再度変更するためである。
ただしcacheの数値を決める前にWebP候補の作業領域を調べ、WebPを後付けできない予算やAPIを
作らない。

初期版のeviction単位は**画像全体**とする。8〜16 scanlineの帯cache、codec途中状態の保持、
任意行からの再開は、全画像LRUを実機評価してなお必要な場合の別計画とする。

## 背景

現在のBrowserはPNGとbaseline JPEGを1件ずつ取得し、decode結果をRGB565へ変換して
`Page::decoded_images`へ保持する。同一page内で同じ解決済みURLが先にdecode済みなら`Rc`を
共有するが、異なる画像はviewport外へ出ても解放しない。Desktopやmini appへ移ってBrowserを
suspendした場合も現在pageは生存するため、decode結果は残る。別pageへの遷移では旧`Page`と
ともに解放され、履歴にはdecode結果を保持しない。

画像取得はDocumentの画像IDを`next_image`で先頭から順に進める。従ってLRUだけを追加しても、
まだ読者が到達していないpage末尾の画像が、現在見ているpage先頭の画像を追い出す。必要時だけ
decodeするschedulerへの変更をLRUと同じ作業で行う必要がある。

計画開始時の主な上限は次のとおりだった。現行値は後述の「上限の決め方」と
`browser/src/limits.rs`を参照する。

| 項目 | 現在値 |
| --- | ---: |
| 1文書の画像数 | 64 |
| 1画像の圧縮入力 | 512 KiB |
| intrinsic幅・高さ | 各1280 pixel |
| 1画像の画素数 | 1,048,576 pixel |
| decoder作業領域 | 1 MiB |
| HTTP cache 1 entry | 512 KiB |
| browser拡張機能の想定合計 | 6 MiB |

この6 MiBの静的検査は「圧縮入力1件＋作業領域1件＋RGB565画像1件＋cache body 1件＋form／
履歴」を足していたが、当時は現在pageに生存する複数の`DecodedImage`合計をruntimeで制限していなかった。
画像単体の確保は失敗可能でも、先にdecodeした画像がheapを占有し続けるため、後続の本文、layout、
TLS、cache処理に残すべき余裕を画像数に応じて失う。

HTTP cacheは圧縮された応答bodyを`/tmp/browser-cache`へ保存する。`/tmp`は8 MiBのRAM disk上に
あるので、heap上のdecode済みRGB565とは所有領域が別だが、PSRAM全体としてはどちらも有限である。
圧縮入力上限だけを数MiBへ増やすと、1 entryがRAM diskの大半を占め得る。圧縮入力、HTTP cache
entry、RAM disk上で共存させたいentry数を同時に決める。

## 目的

- 画像の多いpageでもdecode済み画像がheapを無制限に占有しない
- 表示中の画像と直近に使った画像を優先し、画面外の古い画像を自動的に解放する
- eviction後もintrinsic寸法とlayoutを維持し、再表示時にlocal file、HTTP cache、networkの順で
  既存規則に従って復元する
- 画像取得をviewport優先にし、未到達の画像が表示中画像を追い出すcache churnを起こさない
- 圧縮入力、画素数、decoder作業領域、decode済み画像の現在量とpeakを同じ予算モデルで説明できる
- 現在値より大きい実用的なPNG／JPEGを、基板全体のOOMや再起動にせず表示または局所失敗させる
- 静止WebPのlossy、lossless、alphaをPNG／JPEGと同じ画像box、layout、cache、TLS規則へ接続する
- animated WebPは通信やpage全体を壊さず、画像単位の`unsupported image`として拒否する
- `cargo build --release`、host test、fixture試験と人間によるTab5実機受入を通す

## 対象外

- animated WebPのframe timer、loop、blend、dispose
- GIF、SVG、AVIF、JPEG progressive、PNG Adam7／16-bitなど、既存非対応形式の追加
- scanline／tile単位のdecode済みcache、codec checkpoint、任意行からのdecode再開
- decode済み画像のpage横断cache、履歴へのdecode結果保存
- CSS画像、background image、`picture`／`srcset`、device pixel ratio、画像の回り込み
- ICC profileによる色管理、Exif orientation、XMP／Exif表示
- 複数画像の同時network fetchまたは複数decoderの並列実行
- RAM disk容量そのものの変更
- HTTP cacheをflash、SD、USBへ永続化する変更

## 固定する設計方針

### 1. Document画像とdecode cacheを分ける

Documentの画像IDはHTMLの出現位置、`alt`、指定寸法、intrinsic寸法、link／buttonとの関係を持つ。
decode cacheは解決済みURLと画素を持つ。両者を同じ添字の`Vec<Option<Rc<DecodedImage>>>`として
扱わず、page内の画像IDからcache entry IDを参照する。

概念上のentry状態は次とする。実装時の名前はRustの所有関係に合わせてよいが、状態の意味を
混ぜない。

```text
Unrequested  URLはあるが、まだ取得対象でない
Queued       viewportまたは先読み範囲から要求された
Reading      local／HTTP cache／networkから圧縮byteを取得中
Decoded      RGB565を保持して描画可能
Evicted      寸法とURLは有効だがRGB565を解放済み
Failed       取得、形式、破損、上限、OOMの局所失敗
```

`Evicted`は失敗ではない。画像boxへ失敗文字列を固定せず、再び必要になれば`Queued`へ戻す。
`Failed`を自動で無限再試行しない。Reload、page再取得、cache validatorによる新応答など、入力が
変わり得る操作でだけ再試行する。

同じ解決済みURLを参照する複数画像IDは1 entryを共有する。いずれか1つがviewport内ならentry全体を
表示中とみなす。URLが同じでもpageをまたいで共有しない。redirect後の最終URLではなく、現在と同じ
解決済み要求URLを初期keyとし、redirect・cache・securityの既存挙動を変えない。最終URLへkeyを
統合する最適化は別作業とする。

### 2. viewport優先のlazy decode

画像jobはDocument順の単調な`next_image`ではなく、各frameでlayoutから必要度を求めて1件を選ぶ。
同時に動かす取得／decode jobは現在どおり最大1件とする。

優先順位は次とする。

1. viewportと交差する未decode画像
2. viewportの上下それぞれ1 viewport分にある未decode画像
3. focus中のlink／buttonに含まれる未decode画像
4. それ以外は取得しない

同順位ではdocument y、画像IDの順で決定し、毎frame順序が揺れないようにする。scroll、画像寸法確定、
再layout、focus変更で優先度を再計算する。画像が近傍を出ても進行中の1 jobは原則完了させるが、page
遷移、Stop、Wi-Fi切断、Browser suspendは現在の所有規則どおり中断または再開する。

遠方画像を取得しないため、幅・高さ属性が無い画像は近づくまで仮寸法を使う。decode後のintrinsic
寸法確定では既存の`ReadingPosition`により論理位置を維持する。遠方画像のためだけのHEADまたは寸法
専用GETは追加しない。

### 3. 全画像単位LRUとpin

LRUはpage内cache entryごとに単調なaccess generationを持つ。描画、decode成功、cache hitによる
復元をaccessとする。counterのwrap時に全entryを相対順へ詰め直し、時刻のwrapやRTC変更へ依存しない。

次のentryはevictionしない。

- viewportと交差する画像のentry
- 現在Reading／decode中のentry
- 新しい画像のinstallと再layoutが完了するまでの新旧対象entry

先読み範囲だけにあるentryは優先度を上げるがpinしない。soft limitを超えたら、pinされていない
`Decoded`を最終accessが古い順に解放する。同じURLの全参照は同時に`Evicted`となる。

visible entryだけでsoft limitを超えることは許すが、hard limitを超えて確保しない。非pin entryを
すべて解放しても新規decodeの予約がhard limitへ収まらない場合、既に表示できている画像を捨てて
入れ替え続けず、新規画像を`image memory limit`として局所失敗させる。1画像のRGB565上限はhard
limit以下に固定する。

### 4. capacityと一時領域を含む予算

decode済み量は`width * height * 2`だけでなく、実際に所有する`Vec<u16>::capacity() * 2`とentryの
付随allocationを数える。URLはDocument所有分と二重計上しない構造を優先する。診断値とeviction判定が
異なる定義を使わない。

decode開始前に少なくとも次を予約量として検査する。

```text
現在resident decode容量
+ 完成後RGB565容量
+ 圧縮入力capacity
+ format別decoder最大作業領域
+ cache書込みcaptureなど同時に生存する画像関連buffer
```

必要なら非pin entryを先にevictする。decoderを呼んだ後のallocation failureにだけ頼らない。候補crateが
入力依存の通常`Vec::push`でOOM abortし得る場合は、寸法・圧縮長・作業量をdecoder前に上限検査し、
必要量のheap余裕を確保できることを採用条件にする。内部最大量を上から制限または見積もれないdecoderは
採用しない。

soft／hard limitはdecode済みresident量に対する値と、active decodeを含む同時peakに対する値を分けて
名前付けする。従来の`MAX_EXTENSION_OWNED_BYTES`を、複数の生存画像を数えない説明のまま数値だけ増やさない。

### 5. eviction後の再取得

再表示時のsource選択は新しい独自cacheを作らず、現在の経路を再利用する。

| 元画像 | eviction後の復元 |
| --- | --- |
| `file:` | VFSから同じfileを再読込 |
| fresh HTTP cache | `/tmp/browser-cache`から読込 |
| stale HTTP cache＋ETag | 条件付きGET後、304ならcache bodyを読込 |
| cache miss／`no-store` | networkから再取得 |
| network利用不能 | 画像boxを維持して取得失敗を表示。page本文は残す |

`no-store`応答の圧縮byteをLRU用に別途保持しない。decode済み画像がevictされた後は再取得となる。
Pinned TLS pageからの画像、HTTPS downgrade、network pageから`file:`への参照は既存規則を維持する。

### 6. 保持解像度

初期実装は現在と同じくintrinsic解像度のRGB565をentryへ保持し、描画時に最近傍拡縮する。同じURLを
異なる表示寸法で共有でき、PNG／JPEGの変更とLRUの効果を分離できるためである。

大きい元画像を小さいboxで多数表示するpageでは、intrinsic解像度保持がsteady memoryを浪費する。
Stage 3の実測でこれが支配的なら、次の追加最適化を同Stage内で採否判断する。

- page内の同一URLが要求する最大表示寸法を求め、その寸法のRGB565だけを保持する
- intrinsic寸法は別に保持し、layout比率には元寸法を使う
- より大きい表示boxが後から確定した場合だけ再decodeまたは再sampleする

これは保持量を減らすが、codec内部の一時RGBA／RGB領域を必ず減らすものではない。実機結果なしに
WebP追加の必須条件にはしない。

## 静止WebPの対応範囲

受理するHTTP media typeは`image/webp`とし、本文のsignatureも`RIFF`、RIFF size、`WEBP`、chunk境界を
検査する。拡張containerを含め、初期対応範囲は次とする。

| WebP構成 | 方針 |
| --- | --- |
| `VP8 ` | 静止lossyとしてdecode |
| `VP8L` | 静止losslessとしてdecode |
| `VP8X`＋`VP8 ` | 静止lossy。`ALPH`があればalphaを白へ合成 |
| `VP8X`＋`VP8L` | 静止losslessとしてdecode |
| `ANIM`、`ANMF`、VP8X animation flag | decoderへ入れる前に`Unsupported` |
| ICCP、EXIF、XMP | chunk境界は検証するが内容を解釈せず、色と向きを変えない |
| unknown ancillary chunk | RIFF境界内ならskip |

alpha合成はPNGと同じ白背景とし、最終形式はRGB565である。animationの先頭frameだけを静止画として
見せない。動いて見えるべき画像を黙って別内容として表示するより、画像box内で非対応を明示する。

decoder候補は実装開始時にrelease sourceとlockfileを再確認する。第一調査候補はpure Rustで
`no_std + alloc`を掲げ、静止VP8／VP8L／alphaと画素数制限を持つ`webpkit`系とする。ただし新しい
0.x実装であることを考慮し、名称だけで採用しない。次を満たすものだけを使う。

- RISC-V `riscv32imafc-unknown-none-elf`のrelease buildが通る
- decoderだけをfeature選択でき、encoder、animation全frame、`std`、thread、C toolchainを入れない
- licenseが本repositoryで再配布可能で、依存crateを含めて記録できる
- hostileな寸法、chunk長、bitstreamでpanicせず`Result`を返す
- decode前に画素数と最大作業memoryを制限できるか、安全側に見積もれる
- lossy、lossless、alpha、不正、切断のfixtureでhost testを通す
- IROM／DROM増分がapp partitionとXIP配置に収まる

条件を満たすdecoderが無ければStage 4だけを保留し、cache改良とPNG／JPEG上限緩和を先に完了してよい。
WebPを理由にcache改良を元へ戻さない。

### 2026-09-18 候補採用記録

`webpkit 0.1.0`（MIT OR Apache-2.0）を暫定採用した。pure Rustで、`default-features = false`と
`alloc` featureだけでRISC-V release buildが通り、C toolchainと`std`を要求しない。`DecodeOptions`の
decode前pixel上限とmetadata無効化を利用し、one-shot APIがanimationの先頭frameを返す仕様は
`is_animated`による事前拒否で覆う。crateにdecoder専用featureはなくencoder APIも同じ`alloc` featureへ
含まれるため、最終binaryでは未参照codeの除去に依存する。この点とIROM／DROM増分、実機heap peakは
Stage 0の未完了項目として残す。採用した4 MiB作業上限からWebPは1,048,576 pixelに制限する。
追加後のELFはIROM 1,655,434 bytes、DROM 1,113,816 bytesでlayout検査を通過した。
追加前との増分比較値は残っていないため、候補採用の最終判断前にbaselineとの差を測る。

## 上限の決め方

次は実装開始時の**評価候補**であり、現状仕様でも確定値でもない。Stage 0〜3でhostと実機のpeak、
decode時間、通常pageの互換性を比較して確定し、`browser/src/limits.rs`、本書、`BROWSER.md`を一致させる。

| 項目 | 現在値 | 評価候補 |
| --- | ---: | ---: |
| 圧縮入力／HTTP cache 1 entry | 1 MiB | 2 MiB |
| intrinsic幅・高さ | 各2048 px | 実機結果を見て維持または各4096 px |
| 1画像画素数 | 2,097,152 | 実機結果を見て維持 |
| 1画像RGB565 | 最大4 MiB | 実機結果を見て維持 |
| decode済みresident soft limit | 6 MiB | 実機結果を見て維持または再調整 |
| decode済みresident hard limit | 8 MiB | 実機結果を見て維持または再調整 |
| active decodeを含む拡張機能予算 | 16 MiB | 4 MiB作業領域で維持 |
| 先読み範囲 | viewport上下各1画面 | 維持 |

幅・高さ上限を4096へ上げても、画素数上限を同時に満たす必要がある。4096×4096を許す表ではない。
1920×1080は2,073,600 pixelなので2,097,152候補内に収まる。WebPがRGBA8全体を一時出力する場合、
この寸法だけで約8 MiB、最終RGB565と合わせて約12 MiBになる。これが他のpage、TLS、font、cacheと
共存できなければ、WebPだけ画素数を下げる、出力先指定APIを使う、または候補decoderを不採用にする。

HTTP cache 1 entryを2 MiBへ上げる場合、8 MiB RAM diskで大entryを4件保持できるとは限らない。
metadata、root filesystemの他file、書込み中の旧新entryを含め、full volume時の既存LRU purgeで
正常に縮退することを確認する。cache可能な画像の互換性を理由にRAM diskを無断で拡張しない。

## 診断

Browserの`i`診断へ少なくとも次を追加する。数値はsession開始からの累計と現在値／peakを区別する。

- image entry数、`Decoded`数、`Evicted`数、`Failed`数
- decode済みresident bytesの現在値、peak、soft／hard limit
- active圧縮入力capacity、decoder予約量、画像関連同時peak
- decoded cache hit、miss、eviction、圧縮cacheからのredecode、network refetch
- 同一URL共有hit
- PNG、JPEG、WebPごとのdecode成功、unsupported、malformed、too-large、OOM
- schedulerが選んだvisible、prefetch、focused job数

診断表示自体で大きいallocationを行わず、固定長counterと既存の上限付きstatus文字列を使う。
evictionされた画像を`Failed`へ数えず、再decode成功後に`Evicted`数を減らすか累計と現在値を別名にする。

## Stage 0: baselineとWebP decoder候補の検証

製品の画像所有構造を変える前に次を行う。

- 現行PNG／JPEG fixtureで、画像ごとの圧縮byte、decoder作業量、RGB565容量、heap before／peak／afterを
  記録できる診断を用意する
- 640×400、上限近傍、同一URL複数参照、多数の異なる画像で現在pageの生存量を確認する
- WebP候補をhost上の独立testまたはfeatureでbuildし、lossy、lossless、alpha、animated、不正入力を試す
- 候補の全featureと依存tree、license、`std`／C toolchain混入、入力依存allocationをsourceから監査する
- `cargo build --release`でRISC-V buildを確認し、`tools/check_elf_layout.py`でIROM／DROM増分を記録する
- 候補decoderの出力形式、画素数制限、最大作業領域、decodeをpoll分割できるかを記録する

hostで候補を比較するためのfixture生成toolは追加してよいが、通常buildが外部networkやhost decoderを
必要とする構成にしない。生成済みの小さいfixtureをcommitし、由来と生成条件を記す。

**完了条件:** 採用候補または「現時点で採用可能な候補なし」が理由付きで決まり、cache予算が候補decoderの
peakを含めて設計できる。数値が得られる前に評価候補を確定値へ書き換えない。

## Stage 1: URL単位cache entryと容量計上

PNG／JPEGだけで所有構造を変更する。

- Document画像IDとdecode cache entry IDの対応を作る
- 解決済みURLが同じ画像IDを同じentryへ結ぶ
- `Rc<DecodedImage>`がpage内の各slotへ分散してevictionを妨げる構造をやめ、cache entryが画素の
  生存を一元所有する
- `Unrequested`から`Failed`までの状態を実装する
- decode済みcapacity、現在値、peak、access generationを更新する
- layoutと描画はentry IDから`Decoded`画素を借り、未decode／evicted／failedを区別して描く
- intrinsic寸法はDocumentへ残し、RGB565 evictionで再layoutしない

pureなcache policyを`browser` workspace crateへ置ける場合は、URLやFramebufferへ依存しない部分を
host unit test可能にする。少なくとも共有、capacity計上、access更新、state遷移、counter wrapをtestする。

**完了条件:** LRU evictionをまだ有効にしなくても、既存PNG／JPEGの表示、同一URL共有、layout、失敗表示が
従来どおりで、decode済み総量を正しく報告できる。

## Stage 2: viewport schedulerとLRU eviction

- layoutの画像boxからviewport交差、上下先読み範囲、focus対象を計算する
- `next_image`による全件順次decodeをpriority選択へ置き換える
- 同時job 1件、page世代、Stop、suspend、Wi-Fi復帰の所有契約を維持する
- soft limit超過時に非pinの最古entryを画像全体でevictする
- hard limit予約に収まらないdecodeを開始前に局所失敗させる
- evictionされたlocal、fresh cache、304再検証、cache miss、`no-store`をそれぞれ既存取得経路へ戻す
- scrollを小刻みに往復しても境界付近の同じ画像を毎frame evict／redecodeしないよう、上下1 viewportの
  先読みとLRU accessによりhysteresisを持たせる

host testでは次を固定する。

- 表示中entryは非表示entryより後まで残る
- 同じURLの複数boxの1つがvisibleなら共有entryはpinされる
- non-visible LRU順が正しい
- soft超過はevictionし、hard超過は既存visible画像を破壊せず新規画像を失敗させる
- eviction後もintrinsic寸法とlayout高が変わらない
- scrollで再び近づくと`Evicted`から1回だけjobが作られる

**実機確認:** 多画像fixtureを上端から下端までscrollして戻り、heapがhard limit内、入力が応答し続け、
表示中画像が無用に点滅せず、UARTのeviction／redecodeが想定順に増えることを人間に確認してもらう。

## Stage 3: 画像上限の緩和

Stage 2の予算管理が機能してから、評価候補を一段ずつ上げる。一度にすべて変えず、次の順で比較する。

1. 圧縮入力とHTTP cache 1 entry
2. intrinsic幅・高さ。ただし画素数上限を独立に維持する
3. 1画像画素数とRGB565出力
4. PNG展開量、JPEG RGB作業量などformat別作業上限
5. decoded resident soft／hardとactive decode同時peak

上限未満、ちょうど、1 byte／1 pixel超過のhost testを各境界に持つ。巨大な寸法と小さい圧縮入力、
RIFF／chunk長overflow、乗算overflowをdecoder前に拒否する。上限超過は画像単位の`image too large`で、
page本文と既存画像を維持する。

**採用条件:** `cargo build --release`が通り、実機で上限近傍PNG／JPEGのdecode中もsystem bar、Stop、
Wi-Fi serviceが失われず、heap peakが決定した同時peak内に収まる。decodeが長時間frame loopを塞ぐ場合は
上限を下げるかdecoder分割を先に行い、見かけの互換性のために入力応答を犠牲にしない。

## Stage 4: 静止WebP対応

- 採用decoderを必要最小featureで`browser/Cargo.toml`へ追加する
- `Format::WebP`、RIFF／chunk検査、寸法取得、format別作業量見積りを追加する
- `decode()`へWebP signature dispatchを追加する
- HTTP fetchとfile-backed HTTP cacheへ`image/webp`を追加する
- lossy、lossless、alphaを白背景へ合成し、既存`DecodedImage`のRGB565へ変換する
- VP8X animation flag、`ANIM`、`ANMF`をdecoder前に`Unsupported`とする
- ICCP、EXIF、XMPを解釈しないことを現状仕様へ明記する
- WebPも同じURL共有、LRU、eviction、redecode、TLS downgrade拒否、局所失敗を使う

host testには最低限、bare VP8、bare VP8L、VP8X＋VP8＋ALPH、透明VP8L、animated、切断RIFF、
不正chunk length、寸法超過、圧縮長超過を置く。外部decoderで正解画素を生成する場合も、test時に
外部programを要求せず、期待RGB565またはhashをrepositoryへ固定する。

**完了条件:** 対応静止WebPがhostで期待RGB565となり、animatedと破損がpanicせず局所失敗し、
RISC-V release buildとELF layout検査が通る。

## Stage 5: 統合fixtureと回帰

`tools/browser_fixture_server.py`へ少なくとも次を追加する。

- 新上限内で旧512 KiBまたは旧画素数上限を超えるPNG／JPEG
- 新上限を1段超える画像
- 1画面では収まらず、LRU soft limitを確実に超える異なる画像列
- 上下へscrollするとevictionとredecodeが起こる長いpage
- 同じURLを離れた位置と異なる表示寸法で再利用するpage
- cacheable、ETag再検証、`no-store`の画像
- WebP lossy、lossless、alpha、animated、破損、遅延応答
- cache可能なWebPをLRU evictionし、HTTP cacheから再decodeする長いpage
- PNG、JPEG、WebPの一部だけが失敗し、本文と他画像が残る混在page

host test、fixture manifest、release buildを通し、`README.md`は変更しない。現状文書は実機受入前に
未確認の挙動を受入済みと書かず、実装済み・実機未確認として更新する。

**完了条件:** hostで状態遷移、境界、RGB565、security、cache再利用を再現でき、release buildが通る。

## Stage 6: Tab5実機受入と完了

人間がTab5へ書き込み、同じLAN上のPCでfixture serverを起動して確認する。エージェントは
`cargo run --release`または`espflash`を実行しない。

### 実機手順

1. PCで`python3 tools/browser_fixture_server.py`を起動する
2. 人間が`cargo run --release`でTab5へ書き込む
3. Browserで追加した画像cache／WebP fixtureを開く
4. 上端で画像表示と`i`診断を記録し、page末尾までゆっくりscrollする
5. 末尾の画像表示後に`i`を記録し、上端へ戻る
6. 同一URL画像、cacheable画像、`no-store`画像の再表示とcounter差を確認する
7. lossy、lossless、alpha WebPを確認し、animatedと破損WebPが画像枠内だけで失敗することを確認する
8. decodeまたは再取得中にStop、Wi-Fi切断／復帰、system barからDesktop往復を行う
9. PNG／JPEGの既存fixture、form、table、TLS画像拒否を回帰確認する

### 期待結果

- decode済みresident量はsoft付近でevictionされ、visibleだけで超える場合もhard limitを超えない
- page下端の画像を先にdecodeせず、viewportと上下先読み範囲の画像から表示される
- 上下往復でevictionとredecodeは起こるが、同じ位置で停止中にcounterが増え続けない
- eviction後も画像box高、本文位置、focus、touch範囲が変わらない
- cacheable画像は圧縮cacheから戻り、`no-store`は必要時だけnetwork refetchとなる
- WebPの色、alpha白背景、縦横比がPNG／JPEGと同じ規則で表示される
- animated／破損／上限超過は局所失敗で、他画像、本文、Back、system barが使える
- panic、再起動、heap破損、Wi-Fi starvation、display underrun増加がない

### 失敗時に見える症状

- 画面外画像を読んだだけで表示中画像が消え、停止中もdecodeを繰り返す
- 上下scrollのたびに全画像をnetworkから取得する
- eviction後に画像boxが仮寸法へ戻り、本文が跳ぶ
- 同一URLが別々にmemoryを占有する
- residentまたはheap使用量がhard limitを超えて増え続ける
- 大画像decode中に入力、C6 service、system barが止まる
- animated WebPの先頭frameだけを成功として表示する
- 画像1件の失敗でpage全体がerror画面になる、panicする、または再起動する

人間から結果を受け取った後だけ、本書へ回数、counter、採用した上限、decode時間、失敗の有無を記録し、
`BROWSER.md`と必要な`NETWORK.md`／`FILESYSTEM.md`を現状へ同期する。全Stage受入後に
`git mv`で本書を`docs/plans/archive/`へ移し、`docs/plans/INDEX.md`と全参照を更新する。

### 2026-09-18 実機診断（初回）

人間から次の`i`診断値を受領した。

```text
BROWSER: heap 1006K sockets 2 back 1 fwd 0 post 0/0K+0K cache h0 r0 s5 p0 x0 img 1/1K e0 page 2K peak 2K
```

`heap`は空きではなくグローバルallocatorが現在渡している総量である。decode済み画像は現在／peakとも
1 KiB、eviction 0で、報告時点では画像residentの増加やLRU作動は見られない。cacheは保存累計5回、
socketは2件である。この値だけでは静止2画像の目視、animated／brokenの局所失敗と、多画像ページを
上下往復した結果までは判定しない。

その後、同じ実機試験で静止lossless／lossy＋alphaの2画像が表示され、animatedとbrokenだけが画像枠内で
失敗し、本文と静止画像が残ることを人間が確認した。従ってWebPの基本表示と局所失敗は受入済みとする。
多画像ページの上下往復、eviction／再decode、上限近傍画像、Stop／復帰は引き続き未確認である。

### 2026-09-18 LRU実機受入

`/images/lru.html`の640×400 RGBA画像16枚（各512,000 bytesのRGB565、すべて異なる`no-store` URL）を
上端から末尾までscrollし、上端へ戻った。診断値は次の順だった。

```text
BROWSER: heap 1641K sockets 3 back 1 fwd 0 post 0/0K+0K cache h0 r0 s1 p0 x0 img 500/500K e0 page 8K peak 7K
BROWSER: heap 7011K sockets 2 back 1 fwd 0 post 0/0K+0K cache h0 r0 s1 p0 x0 img 6000/6000K e4 page 8K peak 7K
BROWSER: heap 7011K sockets 2 back 1 fwd 0 post 0/0K+0K cache h0 r0 s1 p0 x0 img 6000/6000K e9 page 8K peak 7K
```

residentはsoft limitの6,000 KiBで止まり、末尾までに4回、上端への再取得までに累計9回evictionした。
heapはresident増加分に沿って7,011 KiBで止まり、取得完了後のsocket数は2へ戻った。画像boxと操作応答も
人間が確認したため、viewport scheduler、全画像LRU、`no-store`再取得の1往復を実機受入済みとする。

### 2026-09-18 WebP eviction・再decode実機受入

`/images/webp-lru.html`のcache可能なlossless WebPと、後続する640×400 RGBA `no-store` PNG 16枚を
上端から末尾までscrollし、上端へ戻った。診断値は次の順だった。

```text
BROWSER: heap 2514K sockets 2 back 1 fwd 0 post 0/0K+0K cache h0 r0 s2 p0 x0 img 1500/1500K e0 page 11K peak 9K
BROWSER: heap 7014K sockets 2 back 1 fwd 0 post 0/0K+0K cache h0 r0 s2 p0 x0 img 6000/6000K e5 page 11K peak 9K
BROWSER: heap 7014K sockets 2 back 1 fwd 0 post 0/0K+0K cache h1 r0 s2 p0 x0 img 6000/6000K e9 page 11K peak 9K
```

residentは6,000 KiBで止まり、evictionは末尾で5回、上端へ戻るまでに累計9回となった。戻り時に
HTTP cache hitが0から1へ増え、WebPが同じboxへ再表示されたことを人間が確認した。従って静止WebPの
LRU evictionとHTTP cacheからの再decodeを実機受入済みとする。

### 2026-09-18 Launcher往復中断・復帰実機受入

`/images/stage3.html`の遅延PNG取得中にF3でLauncherへ移り、数秒後にBrowserへ戻った。同じpageと
scroll位置へ戻り、画像取得が再開して遅延PNGが表示され、先に表示されていたPNG／JPEGと本文も維持され、
`PANIC`しないことを人間が確認した。画像取得中のBrowser suspendと復帰を実機受入済みとする。

### 2026-09-19 画像取得のStop経路修正

残りのStop回帰前に所有経路を再点検し、本文表示後の`local_image`、`cache_image`、`network_image`が
page本体の`pending`とは別管理なのに、Escapeとtoolbarの停止判定が`pending`だけを見ていたことを確認した。
この状態では遅延画像の取得中もtoolbarは再読込表示のままで、画像だけを中止できなかった。

画像読出し中もtoolbarを停止表示にし、Escape／停止buttonで該当handleまたはsocketを閉じ、画像枠を
`stopped`として局所停止するよう修正した。失敗状態を付けるのはviewport schedulerによる即時再取得を
防ぐためで、page再読込では通常どおり再試行する。release buildとhost回帰を通過した後、遅延PNGで
停止表示、Escape／停止buttonによる局所停止、即時再取得しないこと、本文と既存PNG／JPEGの維持、
再読込による取得成功、`PANIC`しないことを人間が確認した。画像取得のStop経路を実機受入済みとする。

### 2026-09-19 Wi-Fi切断・復帰試験の扱い

画像取得中にWi-Fiを切断し、管理対象接続の復帰後に再取得する回帰試験は、人間の判断で今回の検証から
除外した。成功済みとは扱わない。Launcher往復によるsocket破棄と画像再取得は実機受入済みだが、
画像cache・viewport scheduler変更後の実Wi-Fi切断経路は未確認事項として残す。

## 完了判断

初期案の専用URL entry型は導入せず、既存の画像ID slotを保ったまま、同じ解決済みURLの`Rc` aliasを
同一groupとして容量計上、pin、access generation、evictionする構成を採用した。専用状態型を追加するより
変更範囲が小さく、同一URLの二重計上防止、全alias同時eviction、layout維持、再取得という目的を満たす。
LRUとWebP再decodeの実機結果がこの構成を通過したため、Stage 1の実装差分として確定する。

WebP追加前のELF比較値は保存されておらず、後から同条件で復元できないため取得不能として閉じる。追加後の
release ELFはIROM 1,657,506 bytes、DROM 1,113,816 bytesで配置検査を通過した。大WebPの性能は未測定だが、
4 MiB作業上限と1,048,576画素の事前拒否を持ち、対応形式、局所失敗、LRU再decodeを実機受入済みであるため、
機能完了の阻害事項とはしない。

実Wi-Fi切断回帰は上記のとおり未確認のまま見送る。Launcher suspendによる進行中socketの破棄と復帰、
画像Stop、PNG／JPEG維持は変更後の実機で確認済みであり、この既知の未確認事項を残して計画を完了する。

### 2026-09-18 圧縮入力1 MiB・初回実機試験

`/images/limit-expanded.html`の新上限を1 byte超える2枚目は局所失敗したが、画像枠の表示が期待した
`image too large`ではなくページ用の汎用見出し`Cannot show this page`だった。HTTP層がdecoderより先に
`body-limit`を返す経路で、画像boxが`Failure::headline`をそのまま使っていたためである。画像取得時だけ
`body-limit`を`image too large`、`out-of-memory`を`image out of memory`へ変換する修正を追加した。
修正版と、1枚目の768,698 byte PNG表示・cache保存は再実機確認待ちである。

修正版を再試験し、1枚目の表示・cache保存、2枚目だけの`image too large`表示、本文と1枚目の維持を
人間が確認した。圧縮入力とHTTP cache entryの1 MiB化を第1段として実機受入済みとする。

### 2026-09-18 辺寸法2048実機受入

`/images/limit-dimensions.html`で1600×400の1-bit PNGが表示され、幅2049の画像だけが
`image too large`として局所失敗し、本文と正常画像が残ることを人間が確認した。幅・高さ各2048 pixelへの
拡張を第2段として実機受入済みとする。

### 2026-09-18 総画素数2,097,152実機受入

`/images/limit-pixels.html`で1920×1080の1-bit PNGが表示され、2048×1025の画像だけが
`image too large`として局所失敗し、本文と正常画像が残ることを人間が確認した。総画素数2,097,152と
最大4 MiBのRGB565保持を第3段として実機受入済みとする。

### 2026-09-18 decoder作業領域4 MiB実機受入

`/images/limit-work.html`で1024×768 RGBA PNG（展開3,146,496 bytes）が表示され、1024×1024 RGBA
PNG（4,195,328 bytes）だけが`image too large`として局所失敗し、本文と正常画像が残ることを人間が
確認した。作業領域4 MiBとWebP 1,048,576画素までの事前上限を第4段として実機受入済みとする。

### 2026-09-18 decoder作業領域8 MiB棄却

最終候補として作業領域を8 MiBへ上げ、`/images/limit-work.html`の1920×1080 RGBA PNG
（展開8,295,480 bytes）を実機でdecodeしたところ、UARTには`PANIC`だけが表示された。PNG decoderは
展開buffer約8 MiBと完成RGB565約4 MiBを変換中に同時保持するため、page、network、allocatorの既存確保と
合わせたpeakまたは連続領域不足が原因と推定する。詳細なpanic位置は得られていないため断定しない。

8 MiBと拡張機能予算20 MiBは不採用とし、実機受入済みの作業領域4 MiB、拡張機能予算16 MiBへ戻した。
WebPの事前上限も1,048,576画素へ戻る。8 MiB以上を再検討する条件は、PNGの走査線展開またはbuffer再利用、
WebP decoder内部peakの計測、allocation failureをpanicにしない予約検査のいずれかを先に実装することである。

同日、4 MiBへ戻したrelease版を書き込み直し、`/images/limit-work.html`で1024×768 RGBA PNGの表示、
1024×1024 RGBA PNGだけの`image too large`、本文と正常画像の維持、`PANIC`しないことを人間が再確認した。
従って8 MiB試験からのrollbackは実機受入済みとする。

## 完了条件

- PNG／JPEG／静止WebPが共通のURL単位cacheとviewport schedulerを使う
- decode済みRGB565の現在量、peak、soft／hard limitが診断でき、hard limitを超えない
- evictionと再decodeがlayout、focus、touch、securityを変えない
- 旧上限より大きい採用範囲が明文化され、境界testと実機結果がある
- animated WebPは明示的な非対応として局所失敗する
- host test、`cargo build --release`、ELF layout検査が成功する
- 人間が多画像scroll、再decode、WebP、Stop／Launcher復帰、既存画像回帰を実機で受け入れる
- `BROWSER.md`などの現状文書が実装と一致し、`README.md`が変更されていない
