# 既知の問題

> 索引: [`../DESIGN.md`](../DESIGN.md)

調査中にDW-GDMAチャンネルの停止方法に関する別の不具合を発見し、修正済みです。チャンネルが
転送中の場合、`CHEN0`（`DW_GDMA+0x18`）の有効ビットをクリアするだけでは確実に停止せず、
その後の再始動が不安定になります。正しくはESP-IDFの`dw_gdma_ll_channel_abort`と同じく
`CHEN1`（`DW_GDMA+0x1C`）へアボート要求を書き込み、完了をポーリングする必要があります
（この停止方式自体は現在のコードでは使用していません）。

SDHOST（SDMMCコントローラー）にも、ESP-IDFの実ドライバが一度も踏んでいないと
思われる実機固有の制約が2つ見つかっています（詳細と切り分け過程は
[`SD_CARD_PLAN.md`](plans/archive/SD_CARD_PLAN.md)のStage 2/3を参照）。

- `SDHOST_BUFFIFO_REG`へのCPU/APB直接読み出しはポップ動作をしない。
  `STATUS.FIFO_COUNT`はカードからの実データ到着どおりに増え続けるのに、
  固定アドレス・FIFO窓内でのインクリメントアドレスのどちらで読んでも同じ
  ワードが返り続ける。ESP-IDFは常に内蔵DMA（IDMAC）を使っており、この
  CPU直接読み出し経路を検証していないため、ドライバの誤りというより
  この経路自体が実機で機能しないと考えられる。ブロック読み書きは
  すべてIDMAC経由（`sdmmc.rs`の`read_block`/`read_blocks`/`write_blocks`）。
- DMA転送が実際に成功していても（`STATUS.FIFO_EMPTY`が転送後に1へ戻る、
  `RINTSTS`のDTOビットも正しく立つ）、`SDHOST_IDSTS_REG`のRI
  （Receive Interrupt）ビットは実機で一度も立たない。`SDHOST_CTRL_REG`の
  `int_enable`を含め試したが変化しなかった。DMA完了判定は`IDSTS`ではなく
  `RINTSTS.DTO`のポーリングで行っている。

## IDMACのバッファは64 byte境界から始まらなければならない

ROMのキャッシュ操作関数（`Cache_WriteBack_Invalidate_Addr`）は、開始アドレスが
64 byteのキャッシュライン境界にない範囲を**拒否します**（長さ側は内部で切り上げ
られるため整列不要）。`psram::writeback_invalidate`はこの可否を戻り値で返します。

拒否された場合、CPUはそのバッファのキャッシュ上のコピーを持ち続けます。DMAは
RAMへ正しく書いているのに、CPUが読むのは転送前の内容です。**ゼロ初期化した
バッファなら512 byte全部がゼロで返り、コマンドもDMAもエラーを出しません。**

`sdio.rs`のCMD53と`wifi/hosted.rs`は最初からこの契約を守っていましたが、SDカード側の
`read_block`／`read_blocks`／`write_blocks`／CMD6の`switch_func`は、呼び出し側の
スタック上の`[u8; N]`（Rust上のアラインメントは1）をそのままIDMACへ渡していました。
スタックのどこに置かれるかで成否が変わるため、同じ関数が呼び出し場所によって
正しく読めたり全ゼロを返したりします。`fs`層のMBR判定を追加したときに、
`blkread sd0 0`は正しく読めるのに`devices`は全ゼロという形で顕在化しました。

現在は`sdmmc.rs`が非整列のバッファを整列済みステージングバッファ経由で転送するので、
呼び出し側はアラインメントを意識しなくてよくなっています。加えてキャッシュ操作が
拒否された場合は転送失敗として扱い、黙って誤ったデータを返さないようにしました。
生の入口である`data_transfer_on`だけはステージングせず、非整列を拒否します
（`sdio.rs`の呼び出し側が既に整列を保証しているため）。

`dma2d.rs`の`Descriptor`もこの理由で`align(64)`にしてあります。`usb/hcd.rs`は
同じROM関数を使いますが、DMAが触るバッファに明示的なアラインメントを宣言しており、
実機受入も済んでいるため今回は変更していません。

## `hadris-fat` 2.1.0の追記がクラスタ1つ分を上書きする（2.2.0で修正済み）

`FileWriter::new_append`が、ファイル長がクラスタサイズの整数倍のときに書き込み位置を
**最終クラスタの先頭**に置いていました。

```rust
// 2.1.0
let offset_in_last = file_size % cluster_size;   // 整数倍なら 0
```

`write()`は`offset_in_cluster >= cluster_size`のときだけ新しいクラスタを確保するので、
この0では確保せず、既に埋まっている最終クラスタへ先頭から書きます。**追記のはずが
直前のクラスタ1つ分を上書きします。**

RAMディスクは8 MiBで`choose_layout`が`sectors_per_cluster = 1`を選ぶため
クラスタサイズが512 byteです。`Vfs::write`をハンドルへ4096 byteずつ呼ぶと、
2回目以降の全てが該当します。`fswritetest /tmp`が検査5（`handle: write 64 KiB`）で
`content differs at byte 3584`——64 KiBの最初の4096 byte書き込みの、最後の512 byte——
として捕まえました。

影響したのは`FileWriter::new_append`を使う経路、すなわち`Vfs::write`の2回目以降の
書き込みと、既存ファイルへの`Vfs::write_stream(Append)`です。1回の
`write_stream`の内側は影響しません——writerを1つ作って`write`を繰り返すだけで、
`new_append`を通らないためです。

2.2.0が整数倍のときに`offset_in_last = cluster_size`を置くよう修正しています
（コメントに`#B2`とあります）。`Cargo.toml`の要求を2.2.0へ上げてあり、これは
好みではなく**下限**です。

## 解消済み: Full-Speed固定ハブ経路のWRITEでCSWが返らなかった

この問題はv41で解消しました。root-port resetより前に書いていたFIFO設定がresetで失われ、
意図しない既定値で動作していたことが根本原因です。reset成功後にESP-IDFと同じbalanced値
RX/NPTX/PTX=`512/256/128`を再適用するよう変更した結果、B1／B2とも`usbcheck`のWRITE／復元
10/10と`usbrawcheck`の32/32・復元がPASSしました。さらにA1／A2／C1／C2も同じ受入試験が
PASSし、2媒体×3接続構成のStage 3 matrixを完了しています。

以下は解消前の観測と切り分けの履歴です。

`usbfs on`でバスをFull-Speedに固定し、ハブ配下のUSBメモリへ書き込むと、WRITE(10)の
data phaseは通るのに**CSW（status wrapper）にデバイスが一切応答しない**ことがあります。
2メーカーの媒体で再現し、High-Speed直結およびHigh-Speedハブ経路では起きません。

```text
USB BOT: retrying bulk packet after packet error during data OUT
USB BOT:   reap QTD control=0x16000040   <- 残量64＝1 byteも出ていない、再送して成功
USB BOT: retrying bulk packet after timeout during CSW
USB BOT:   reap HCINT=0x00000000         <- channelがhaltもcompletionも上げない
USB BOT:   reap QTD control=0x06000040   <- 1 byteも受け取っていない
```

`HCINT`が`0x00000000`のままというのは、IN tokenに対してデバイスが何も返していない
ということです。**data phaseでpacket errorが起きて再送した直後にだけ現れます**——
data phaseの再送とCSWの無応答が対で出るのがこの経路の形です。

影響は書き込みだけです。同じ構成でも`ut`（反復read）は100/100を完走し、読み出しと
HID入力は正常に動きます。`usbwritetest`はpattern書き込みには成功し、その直後の**復元
書き込み**が10回中8回失敗します（周辺LBAへの波及は0回）。`fswritetest`は最初の`mkdir`で
止まります。失敗したWRITEは自動再送しないので、犠牲LBAにpatternが残ります
（`usbzero <LBA>`で消せます）。

この時点では原因は未解明でした。実転送長の扱いとretry安全性は
[`USB_BOT_HCD_REFACTOR_PLAN.md`](plans/archive/USB_BOT_HCD_REFACTOR_PLAN.md)のStage 2で確定させ、
この失敗がそれらとは無関係であることまでは切り分けました。Stage 3でBOT phase、CSWの
長さ／tag／status／residue、data INへのCSW先着を厳密に検証する実装と実機の正常系確認までは
完了しました。同じ系統として、この経路では
Reset Recovery自体が`CLEAR_FEATURE(ENDPOINT_HALT)`のSETUPで失敗する例も観測しています
——**転送失敗はデバイスを任意のBOT phaseに取り残し、当時のRecovery手順はそれを
片付けられていません。**

Stage 3第1回の実機再測定では、両媒体とも`usbcheck 100 1`はCSW異常0、WRITE／復元
10/10でPASSした一方、`fswritetest`は両方とも検査1で再現しました。B1はdata OUTの
packet error再送後にCSWが無応答、B2はpacket error再送後の次のdata OUTがhaltしませんでした。
共通していたのは、**既に`ChHltd`で終了しQTDをreapしたreported packet errorにも、timeoutと
同じchannel／FIFO cleanupを再送前に実行していたこと**です。Stage 3第2回でこのcleanupを
外したところ、同じstatus 1がretry上限まで続いて明確に悪化したため仮説を棄却し、元へ戻しました。

第3回はcleanupの有無ではなく範囲を分け、OUT packet errorでは送信残量を持ち得る
non-periodic TX FIFOだけをflushしました。B1／B2の`usbcheck`は再び全PASSし、`out-nptx`は
packet errorとそれぞれ34／1件で一致しました。全cleanup撤去時のstatus 1連続再発はなく、
方向別cleanupは採用します。またMass Storage Reset直後に150 ms待つ変更により、前回B2で
失敗したReset Recoveryは完了しました。ただし`fswritetest`はB1がCSW無応答、B2が次の
data OUT無応答で検査1のまま失敗しており、この時点ではStage 3全体は未完了でした。

direct channelをarmするときのHCCHAR MC/ECをLinux DWC2と同じ1へ設定するA/Bも行いましたが、
B2の`usbcheck`が従来のread 100/100から4/100で停止し、Reset Recovery後のREAD再送も失敗する
明確な後退になりました。通常の非周期channelでは0へ戻し、split transferだけ1を維持します。
次版は転送動作を変えず、reported packet errorのcleanup前とtimeoutの強制halt前にHCCHAR／HCTSIZ／
HCDMAを記録し、channelのarm状態とdescriptor addressを観測します。

この観測ではB2の`usbcheck`が100/100へ戻り、MC/EC rollbackを確認しました。filesystem WRITEの
故障時は、packet error側のchannelが既にhaltしている一方、その再送に続くB1のCSW INは
`HCCHAR=0x80C88840`で正しくarmされながら`HCINT=0`のままhaltしません。B2の次data OUTも
同じ形です。共通点は「packet errorの再送成功直後の次packet」です。次のA/Bは、再送前の
50 msに加えて再送成功後にも50 ms待ちましたが、B1／B2とも故障形はregister値まで変わらず
No-Goでした。さらにv29で`QTD=0x16000040`の再送成功後だけchannel／NPTX cleanupするA/Bを
行いましたが、B1のraw burstはこのpacket error自体が20回連続して再送成功へ到達せず、3/32で
停止しました。snapshot復元は成功し、B2は32/32と復元がPASSしました。次は局所cleanupを撤去し、
MPS 64以下の非Split BOT BulkだけQTDを使わないbuffer DMAへ切り替えています。これでも同じraw
故障が残るかを`usbrawcheck`で判定し、filesystem testは使いません。

最初のv30はdirect buffer DMAの`HCCHAR.MC/EC`をdescriptor DMAと同じ0にしていたため、B2の
31-byte CBWが`HCINT=0x92`（ChHltd＋NAK＋XactErr）、HCTSIZ未進行で即時失敗しました。これは
累積後のQTD故障ではなくchannel programming failureです。v31はbuffer DMAだけMC/EC=1にし、
v26で回帰したdescriptor DMAは0のまま維持します。

v31でdirect buffer DMAだけMC/EC=1にしましたが、B2はHCCHARの変更を確認できた一方、同じ
`HCINT=0x92`と未進行HCTSIZで失敗しました。B1もCBWでtimeout／short OUTとなったため、非Split
buffer DMA案は撤回しました。現在はdescriptor DMAへ戻し、従来すべての同期packetが同じ
HCDMA baseを即時再利用していた構造を固定2-slot QTD ping-pongへ変更しています。v32の実機では
B2のusbcheck／rawが全PASS、B1もusbcheckは全PASSし、HCDMAが2 slotへ交互に切り替わることを
確認しました。しかしB1 rawは同じ3/32でzero-progress errorを20回反復したため、同一descriptor
addressの即時再利用説は否定しました。v33はzero-progress Bulk OUT error後だけ
`HCFG.DescDMA`をoff→onする設計でしたが、停止packetのretryable 20 errorはすべて不可能残量で、
zero-progressになったのは上限後だけだったため一度も発火しませんでした。v34は同一OUT packetの
2回目以降に条件を変え、B1 rawで約19回確実にmode再始動しても同じ3/32で停止しました。
descriptor内部状態説も否定し、controller-wide切替は撤去しています。

ESP-IDF v5.5.3のESP32-P4 LL／HALとも再照合し、QTD bit配置、HCDMA list base、HCTSIZのNTDと
SCHED_INFOは一致しました。公式定義でQTD status 1はexcessive NAKも含みます。B1だけがREADなしの
3 WRITE後に止まり、各WRITE後にreadbackする試験は完走しました。しかしv35でWRITE間を250 ms、
1000 ms空けても0/32、3/32で改善せず、device busy説はNo-Goでした。公式Bulk経路との残る大きな差は、
公式がdata transferを1 QTDへ載せるのに対し、こちらが64 byteごとにchannelをhalt／rearmする点です。
v36では非Split Full-SpeedのWRITE data 512 byteだけを1 QTDへまとめました。複数packet QTDは
失敗時の受理済みprefixが不明なので再送せず、BOT Reset Recoveryへ進みます。B2実機ではREAD
100/100を維持した一方、WRITE／復元が1/10へ悪化し、data OUT status 1、XferComplなし、CSW
timeoutが発生しました。multi-QTDは12回発火しましたがHCCHARはMC/EC=0の`0x00C81040`でした。
v37は複数packet QTDだけMC/EC=1にしましたが、B2はREAD 100/100、WRITE 1/10、復元0/10で
改善せずNo-Goでした。ESP-IDFのdescriptor DMAもMC/EC=0のため、この変更は撤回しました。
v38は長い512-byte QTDを使わず、64-byte QTDを8個のdescriptor listとしてMC/EC=0のchannel
1回で実行しました。B2では10回すべてQTD 0がstatus 1、不可能残量100,489で停止しましたが、
HCDMAは次descriptorを示す`base+8`、後続QTDは未実行でした。v39は検証済みQTD境界から再開し、
WRITE 10/10まで回復しましたが、QTD 1から再開した復元WRITEのCSWがtimeoutして復元9/10でした。
B2だけでpacket errorが33件へ増えたためlist方式全体をNo-Goとして撤回し、1 packet QTDへ戻しました。
v40のB2はREAD 100/100、WRITE／復元10/10でしたが、実レジスタはRX/NPTX/PTX=
512/1024/1024のreset既定値で、FIFO変更はroot-port resetに消されていました。v41はESP-IDFと同様、
reset成功後に公式balanced値512/256/128 linesを再適用します。B2 `usbcheck`はpacket errorが
v40の34件から0件へ減り、READ 100/100、WRITE／復元10/10で全PASSしました。変更前にコードが
意図していた値は448/224/224でした。B2のREADを挟まないraw WRITEも32/32と復元がPASSしました。
B1もpacket error 0、READ 100/100、WRITE／復元10/10、raw WRITE 32/32と復元がすべてPASSしました。
以前3/32で止まったB1 rawまで安定したため、この経路の旧故障は解消したと判断しています。
故障注入も全9 gateと注入間／終了後の再列挙がPASSしました。C1も通常試験とraw WRITEがPASSし、
C2も通常試験とraw WRITEがPASSしました。High-Speed直結A1／A2も同じ試験がPASSし、
2媒体×3接続のStage 3 matrixを完了しました。

Stage 3のtransport判定には`fswritetest`を使いません。失敗したmkdir metadataが媒体へ残ると、
別マシンでfilesystem repairするまで次のA/Bを開始できず、rawで同じ旧故障を再現・判定できる
ためです。代わりに
`usbrawcheck`がfilesystem外の犠牲LBAへREADを挟まないWRITE列を発行し、最後に照合と復元を
試みます。復元不能でもfilesystem objectは残らず、`usbrescan`後に同じ範囲を再利用できます。

## 解消済み: BOT command境界の予防cleanupが必要だった

連続READ(10)を16回ごと、WRITE(10)を毎回、host controllerのchannel／FIFO cleanupで
挟まないとBulkとEP0が無応答になる問題がありました（FS-onlyで最短33 READ、
High-Speed直結でも52回）。これは緩和策であって原因の解消ではなく、正常な転送のたびに
channelをhaltしFIFOをflushするという、実装としても診断としても筋の悪い形でした。

原因側をHCDの契約として順に固めた結果、この緩和策は不要になりました。

- DMA bufferはHCDが所有して64 byte整列し、cache同期の拒否は転送を開始させない。
  呼び出し側の任意の`&mut [u8]`へalignment契約を課さない。
- channel 0のdescriptor完了は`HCINT.XferCompl`・QTD Active解除・妥当な残量を
  **同じ世代で**確認する。古い完了snapshotやhardware所有中のQTDを成功として回収しない。
- 実転送長は1箇所でだけ導出し、「0 byte」と「不明」を型で区別する。
- root-port reset後にFIFO分割を再適用する（resetで失われていた。これが
  Full-Speed固定ハブ経路のWRITE故障の根本原因でもありました）。
- cleanupの失敗はcommandの中止として伝播する。

実機A/Bは3構成（High-Speed直結、FS-onlyハブ＋HID＋MSC、High-Speedハブ＋Low-Speed HID
＋High-Speed MSC）で、READ側は`usbcheck 1000`が`proactive read+0`で完走、WRITE側は
各構成100回と2メーカー媒体の`fswritetest`各10回が通りました。**予防cleanupは
コードから撤去済みです**（[`USB_BOT_HCD_REFACTOR_PLAN.md`](plans/archive/USB_BOT_HCD_REFACTOR_PLAN.md)の
Stage 5・6）。cleanupが残るのは失敗後だけで、そこでFIFO flushがtimeoutした場合は
Reset Recoveryを実行せずsessionを引退させます。

Stage 7では`usbmultiwrite`を使い、`1234:5645`と`054C:0243`の2媒体について
High-Speed直結とFull-Speed固定ハブ＋HIDの2 topologyで2／4／8 blockを各10回通しました。
この結果を受け、1回のWRITE(10)上限は8 block（4 KiB）へ増やしています。旧1 block制限は
既知問題ではなくなりました。

残件:

- **失敗cleanupがshared FIFOをskipする経路は実機で一度も発火していません。** skipは
  persistent periodic channelがarm中のときだけ起き、`enable_periodic_hid`はsplitが要る
  経路を拒否するため、High-Speedハブ配下のLow-Speed HIDは構造上そこへ到達しません。
  `usbhw`の`fifo-skipped`が0のままなのはこのためです。

## VPDページ0x80の要求に標準INQUIRYを返すUSBメモリがある

実機のUSBメモリで確認しました。EVPDビットを立ててページ`0x80`
（Unit Serial Number）を要求すると、CHECK CONDITIONではなく**標準INQUIRYの応答**が
返ってきます。

これが厄介なのは、**応答からは区別できない**ことです。VPD応答のbyte 1はページ
コードを echo する規定なので`data[1] != page`で検査していましたが、標準INQUIRYの
byte 1はRMB（リムーバブル媒体ビット）で、USBメモリでは`0x80`です。要求した
ページコードと同じ値なので検査を素通りします。

さらに、要求したallocation length（64 byte）まで**デバイスが自分の内部バッファの
残りで埋めて**返します。そこには直前に読んだブロックが入っているため、返ってくる
バイト列が呼び出しのたびに変わります。実際に観測した2つは、後半がそれぞれMBRの
ブートコードと、FAT16ブートセクタの拡張BPBでした。

結果として、シリアル番号ではないものが媒体fingerprintに混ざり、**動いていない
USBメモリの同一性が読むたびに変わる**状態になっていました。`fsverify`が
`MEDIA CHANGED; mount dropped`を出し、書き込み前の媒体照合が全ての変更操作を
`media changed`で拒否します。

現在は**ページ`0x00`（サポートページ一覧）を先に取り、そこに載っているページだけ**を
要求します。一覧が取れない、または一覧として筋が通らない（先頭がページ`0x00`で
昇順になっていない）場合はVPDを使いません。このデバイスでは`inquiry`＋`mbr`＋`boot`
だけが情報源になり、いずれも安定しています。

切り分けには、fingerprintのソース別digest（`differing=`）と、接続中にunit serialが
変わったときのUARTログが効きました。どちらも残してあります——接続が続いている
あいだシリアルが変わるのは、デバイスが答えを作っているか転送層が壊しているかの
どちらかで、黙って指紋へ畳み込んでよいものではありません。

## 2 GiBを超えるFAT16はマウントできない

`hadris-fat`が1クラスタ32 KiBを超えるボリュームを開きません
（`BPB cluster size must not exceed 32 KiB`）。FAT16は最大65,524クラスタなので、
約2 GiBを超えるFAT16は64 KiBクラスタでしか作れず、そこで引っかかります。

実機では3823 MiB・MBRタイプ`0x06`のUSBメモリで踏みました。`usbmbr`と`devices`は
タイプバイトを表示するので`FAT16`と出ますが、マウント判定はブートセクタの中身を見るので
`not a readable filesystem`になります。**媒体は壊れていません。** 2 GiBを超える媒体は
FAT32でフォーマットしてください。

`src/fs/bootsector.rs`側の上限は64 KiBにしてあります。ドライバの制限に合わせて
「ブートセクタではない」と答えると、`mbr.rs`のsuperfloppy判定にも嘘をつくことに
なるためです。ドライバが断った場合はその理由をUARTへ出します。
