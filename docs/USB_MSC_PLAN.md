# USB Mass Storage対応 実装計画

> 索引: [`../DESIGN.md`](../DESIGN.md)
> この文書は作業計画と実機での判断記録です。現在の実装仕様は現状文書と
> コードを優先してください。

## 状態: Stage 1〜6・第10版120分複合試験完了

## 追補（2026-08-21）: 連続READ(10)のtimeoutとReset Recovery

表示・PSRAM・SD・USBを同時に動かす`mix 1`の実機試験で、USB MSCの4 KiB READ(10)が
37回完了した後にBulk IN timeoutとなった。`HPRT=0x00001005`はroot portのconnected、
enabled、poweredが維持され、1 ms間の`HFNUM`も進んでいた。表示は2,138 frame、
PSRAM heap検査は2,110回、DPI underrunとdisplay DMA errorはともに0だったため、
PSRAM 200 MHz化によるシステム停止やVBUS断ではなく、USB BOT transport単体の失敗と判断した。

従来は1パケットの待ち時間が固定20,000,000 iterationで、CPU 360 MHz時は約444 msだった。
さらにtimeout後はchannelをhaltするだけで、persistする`BulkOnlyTransport`のdevice側BOT phaseと
IN／OUT toggleを同期し直していなかった。このため1回の失敗後も同じsessionを使う個別commandが
不安定になり得た。HCDの`control transfer timed out`という表示も共通`run_packet`の固定文言で、
今回のログはcontrol transferではなくBulk IN packetのtimeoutだった。

最初の対策としてpacket timeoutをCPU周波数追従の約2秒へ変更し、command途中のtransport失敗時は
Mass Storage Reset class request、Bulk IN／OUT両方の`CLEAR_FEATURE(ENDPOINT_HALT)`、host側
toggleのDATA0復帰を順に行うBOT Reset Recoveryを追加した。READ(10)はread-onlyなのでRecovery後に
1回だけ再送し、将来のWRITE系commandは共通層で自動再送しない。短い実機試験用に、同じ4 KiBを
既定100回read・比較してRecovery再送数も表示する`ut [count]`を追加した。`ut`と`mix 1`による
実機再確認は未完了である。

追加後の`ut`既定100回では、最初のBulk IN timeout後にReset Recoveryが成功してREAD(10)を
再送できたが、92/100回で2回目のtimeoutとなった。2回目はMass Storage ResetのSETUP後、
EP0 IN status待ちがtimeoutし、その後の`CLEAR_FEATURE`もQTD status 1で失敗した。
`HPRT=0x00001005`と`HFNUM`進行は引き続き正常で、data mismatchは0件だった。

control層にも固定2,000,000 iterationが残っており、CPU 360 MHzでは約44 msしかなかった。
1回目はその時間内にResetが完了したが、2回目は完了前に打ち切られたと判断した。Bulk timeoutを
約5秒、control timeoutを約1秒のCPU周波数追従値へ変更し、Mass Storage Resetが完了しなかった
場合は未知のBOT phaseへ後続`CLEAR_FEATURE`を送らずRecovery失敗として終了するよう修正した。
第2版の`ut`は91/100回でQTD status 1を発生した。最初はCBW Bulk OUTでstatus 1となりBOT Resetは
成功したが、再送READ(10)のBulk INも同じstatusとなり、その後はReset Recoveryの
`CLEAR_FEATURE` SETUPもstatus 1で失敗した。command retry 1、failure 1、mismatch 0だった。

ESP-IDF v5.5.3の`usb_dwc_ll.h`ではQTD status 1はpacket errorで、CRC、transaction timeout、
stuff、false EOPに加えてexcessive NAKも含む。これはCBWをdeviceが受理したという意味ではなく、
同じDATA PIDでのUSB packet再送が可能な状態である。OUTのACKだけを失ってdeviceが既に受理して
いた場合も、同じPIDのduplicateは再消費されない。従来はこのstatusを即座にBOT phase破綻として
Resetへ昇格していたため、deviceの一時的なNAK／packet errorを重いRecoveryへ変換していた。

第3版ではQTD status 1を他のtransaction errorから分離し、toggleを進めず同一packetを50 ms間隔、
最大20回まで再送する。packet retryを使い切った場合だけ従来のBOT Reset Recoveryへ進む。
`ut`と`mix`はpacket retryとcommand retryを別々に計数する。

第3版の`ut`は91/100回で5秒のBulk IN timeoutを3回発生した。各timeout後のBOT Reset Recoveryは
成功し、最初の2回はREAD(10)も再送できたが、3回目でcommand再送を使い切った。結果は
failure 1、mismatch 0、packet retry 0、command retry 2で、その後deviceがroot portから切断され
再スキャンとなった。QTD status 1ではなくchannel halt自体が来ない症状で、発生位置が前版と同じ
約91 commandだったため、timeout値やpacket retryだけではない決定的な累積条件が残っていた。

コードを再確認すると、4 KiB READ(10)をendpoint MPSごとに1 QTDへ分割し、High-Speed MPS
512 byteの実機ではデータフェーズだけでcommand当たり8回channel 0をhalt／restartしていた。
`ut` 91回ではCBW／CSWを含め約1,000回の起動となる。一方、ESP-IDF 5.5.3のdescriptor DMA契約では
1 QTDが複数MPS packetを自動的に処理し、非0 byteのBulk IN QTD長はMPSの倍数でなければならない。
従来は13 byte CSW等の短いbufferもQTDへ直接渡しており、この契約から外れていた。

第4版では4 KiB READ(10)を1 QTDへまとめ、High-Speedで`ut` 100回のchannel起動を約1,100回から約200回へ
削減する。13 byte CSW、36 byte INQUIRY、8 byte READ CAPACITYはMPSサイズの内蔵SRAM stagingで
受け、実受信長だけをコピーする。1 packet QTDだけは第3版の局所再送を維持するが、複数packet
QTDのstatus 1はdevice toggleが何packet進んだか不明なので全体再送せずBOT Resetへ昇格する。

第4版`ut`も91/100回でBulk INが5秒待ち切れとなり、最初のBOT Reset後のREAD(10)再送も
Bulk IN timeoutとなった。2回目のRecoveryでは両`CLEAR_FEATURE`のIN statusがQTD status 1で
失敗し、failure 1、mismatch 0、packet retry 0、command retry 1だった。集約前と同じcommand位置で
再現したため、channel再起動数を主因とした仮説は棄却する。ただしQTD集約とshort response stagingは
descriptor DMA契約への適合として維持する。次は既定High-Speedと`usbfs on`のFull-Speedを同じ
`ut`で比較し、High-Speed PHY／transaction固有か、速度に依存しないBOT／device側かを切り分ける。

Full-Speed切替を依頼した次の`ut`ログは36/100回で4 KiB Bulk IN QTDがstatus 1となったが、
当時の出力には強制modeとendpoint MPSがなく、実際にFull-Speed再列挙されたことをログ単体では
証明できない。このfailureでは`packet retries exhausted`と表示した一方、複数packet QTDを即Resetへ
送る第4版のため`packet_retries=0`だった。続くRecoveryは両`CLEAR_FEATURE`のSETUPでstatus 1となり、
failure 1、mismatch 0、command retry 0で終了した。

第5版は`ut`開始時に`host=High-Speed|FS-only`とBulk IN MPSを表示する。さらに複数packetのBulk IN
QTDがstatus 1になった場合、descriptorの残量から正常受信済みの完全MPS packet数を求め、そのbyteを
保持する。次のDATA PIDを成功packet数の偶奇で進め、未受信のMPS-multiple suffixだけを50 ms後に
再投入する。進捗がMPS境界でなければ曖昧な状態を再利用せずBOT Resetへ進む。

第5版をbuildし直してもmode/MPS行が出なかった原因は、診断行を`ut`ではなく直前の`mix`に
誤挿入した実装ミスだった。第5版相当の実機`ut`は引き続き91/100で5秒のBulk IN timeoutを3回
発生し、各BOT Resetは成功、failure 1、mismatch 0、packet retry 0、command retry 2だった。
status 1のpartial再投入はこのtimeout経路には入らないため、期待どおり結果を変えなかった。

第6版ではmode/MPS表示を共通helperにして`ut`と`mix`の両方から正しく呼ぶ。Bulk QTDの1回の
待ちを約1秒へ短縮し、timeout時にchannelをhaltした後のQTD残量をcache同期して回収する。
正常受信済みの完全MPS prefixを保持し、次DATA PIDと未受信suffixを復元して同じBOT phase内で
最大4回再投入する。合計待ちは従来と同じ約5秒で、それでも完了しない場合だけBOT Resetへ進む。

第6版をHigh-Speed、Bulk IN MPS 512 byteで実機確認した。`ut`既定100回は100/100、failure 0、
mismatch 0、QTD retry 2、command retry 0でPASSした。2回の一時停止をBOT Resetへ昇格せず同一
phase内のQTD再投入で回復しており、従来91/100で再現したfailureを解消した。直接原因はPSRAM
帯域やHigh-Speed PHYではなく、descriptor DMAが長時間haltしない場合に1個のQTDを5秒間放置し、
その後すぐBOT Resetしていた回復粒度だった。次の受入試験は`mix 1`、その後`mix`既定120分とする。

同じ第6版の`mix 1`はHigh-Speed／MPS 512 byteで、3,441 frame、SD/USB I/O 57回、PSRAM heap
検査3,138回を完走した。途中のBulk INはQTD再投入4回を使い切った後にBOT Reset Recoveryと
READ(10) command再送1回で回復した。外部mediaの不一致、DPI underrun、display DMA errorは
すべて0で、`mix: PASS`となった。短時間複合試験を合格とし、残る実機条件は120分の`mix`である。

第6版の120分`mix`はI/O 37回目でQTD再投入4回を使い切り、続くBOT Reset Recoveryの両endpoint
halt解除がEP0 IN statusのQTD status 1で失敗した。2,545 frame、heap検査2,110回までのDPI
underrunとdisplay DMA errorは0で、USBだけを理由に約44秒で終了した。BOT Resetが完了しないsessionを
再利用してはならないが、root port reset後の再列挙ならdevice address、configuration、endpoint
toggleをすべて作り直せる。

第7版の`mix`はUSB transport failure時に現在sessionのretry数を集計してborrowを解放し、root portを
最大3回rescanする。新しくattachしたMSCをreadyにしてLBA 0の4 KiBを再読出しし、試験開始時の
referenceと完全一致した場合だけsoakを継続する。不一致は即FAIL、3回連続で再列挙またはreadに
失敗した場合は`USB transport failed after rescan`で終了する。結果には`rescans`も表示する。

第7版`mix 2`は、起動時のHigh-Speed Hub配下の列挙に失敗してMSCが未登録だったため、soak開始前の
`no USB Mass Storage`で終了した。第7版はsoak中のBOT failureからだけ再列挙するため、setup時の
未登録を回復できなかった。第8版はsetupでMSCが未登録またはnot readyの場合にもroot portを最大3回
同期的にrescanする。readyになった後の基準4 KiB readが失敗した場合も同様に再列挙し、基準dataを
取得できてから時間計測を開始する。実機識別markerは`MIX TEST: recovery v8`とする。
第8版はformat/check/release build、ELF/ESP image検査に合格し、app imageは379,632 byte、
XIP 2 segment＋RAM 2 segmentを維持した。

第8版実機ではHub port 2/3の列挙失敗後、空slotの増分scanが同じ接続を約1秒ごとにreset・再列挙し、
ログが連続した。第9版は列挙失敗した物理接続を保留し、背景処理はconnection/change bitのquiet監視
だけを行う。抜き差しまたは明示的full rescanまでdescriptor取得を再試行しない。起動markerは
`USB ENUM: bounded retry v9`、`mix` markerは`MIX TEST: recovery v9`とする。
第9版はformat/check/release build、ELF/ESP image検査に合格し、app imageは379,728 byte、
XIP 2 segment＋RAM 2 segmentを維持した。

第9版`mix 2`では連続background logは止まったが、3回のfull rescanすべてでHub port 2のHS deviceが
最初のdevice descriptor IN data stageでQTD status 1となり、MSCを取得できなかった。port 1のLS
keyboardは毎回Split列挙でき、port 3のFS deviceはSplit transaction errorだった。root resetを跨いで
同じ失敗位置のため、Hub／device側の状態がVBUS維持中に残る可能性がある。

第10版は通常rescanを3回使い切った場合にUSB-A VBUSを1秒offにし、registryを破棄してHubと全deviceを
電源投入状態から再列挙する。setup、基準read、soak中のtransport failureのいずれでも同じ最終回復を
使うが、1試験あたり最大1回とする。再列挙後のLBA 0 4 KiBが開始時referenceと一致した場合だけsoakを
継続し、結果に`power_cycles`を表示する。実機markerは`MIX TEST: recovery v10`とする。
第10版はformat/check/release build、ELF/ESP image検査に合格し、app imageは381,696 byte、
XIP 2 segment＋RAM 2 segmentを維持した。

第10版`mix 2`ではsetupのroot rescan 3回後にVBUS power-cycleを1回実行し、LS keyboard、HS MSC
（Bulk IN MPS 512）、FS mouseを正常列挙できた。soak中にはBulk IN QTD retry exhaustedとBOT Reset
failureが1回発生したが、root rescan後のread-only 4 KiBがreferenceと完全一致し、試験を継続した。
6,881 frame、I/O 118回、heap検査6,647回、packet retry 22、command retry 0、rescan 5、
power-cycle 1、DPI underrun 0、display DMA error 0で`mix: PASS`となった。第10版の短時間複合試験を
合格とし、残る受入条件は既定120分の`mix`である。

第10版`mix`既定120分は412,801 frame、I/O 7,228回、PSRAM heap検査411,508回を完走した。
packet retry 132、command retry 0、root rescan 4、VBUS power-cycle 1でUSB read-only sessionを
回復し、data mismatch、DPI underrun、display DMA errorはいずれも0だった。`mix: PASS`を確認し、
長時間複合受入条件を完了とする。

USB-A**直結**のUSBメモリで、列挙・Bulk-Only Transport・SCSI
（INQUIRY/TEST UNIT READY/READ CAPACITY(10)/READ(10)）・SDカードと共通の
MBRパース（`mbr::show`、`usbmbr`/`sdmbr`）まで実機確認済み。

**USBハブ経由の接続は本計画では意図的に後回しにしたが、その後
[`USB_REFACTOR_PLAN.md`](USB_REFACTOR_PLAN.md) Stage Fで対応済みである。**
以下の「未実装」の記述は本計画完了時点のもので、現在は当てはまらない。
`connect_mass_storage`のような関数を書く代わりに、`usb::registry::UsbHost`が
ハブの全ポートを走査してMSCもレジストリに載せ、`usbmsc`/`usbread`/`usbmbr`は
`mass_storage_mut()`でレジストリを引く（直結・ハブ経由を区別しない）形に
なった。実機ではハブのポートに挿したUSBメモリが認識されるところまで
確認している。

以下、本計画完了時点の記録:
`usb::connect_keyboard`が持つ直結/ハブ経由の自動判定に相当する
`connect_mass_storage`は書いておらず、`usbmsc`/`usbread`/`usbmbr`は
すべてUSB-A直結デバイスのみを対象にする。技術的な障害があるわけではなく
（`USB_HOST_PLAN.md` Stage 4-5でHID Bootキーボードに対して同じパターンを
実装済み）、単に本計画のスコープに含めなかっただけ。詳細は「範囲外
（将来検討）」を参照。

## 方針

本計画ではESP-IDF/RTOSをリンクせずレジスタ操作で実装し、1機能を1モジュール・
実機確認可能な単位でコミットする。[`USB_HOST_PLAN.md`](USB_HOST_PLAN.md)の
「将来検討」に挙げていた項目の着手であり、`hcd.rs`（チャネル/パケットプリミティブ）・
`protocol.rs`（コントロール転送・標準列挙）は変更せず、その上にクラスドライバ
`src/usb/msc.rs`を追加する形で進める（`hid_keyboard.rs`・`hub.rs`と同じ位置付け）。

**ゴールは`SD_CARD_PLAN.md`のブロックI/O層（`sdmmc::read_block`/`read_blocks`）と
同じ抽象でUSBメモリの生ブロックが読め、`sdmbr`と同じMBRパース処理を両方の
デバイスで共有できることまで**とする。ファイルシステム（FAT/exFAT）の解釈は
`SD_CARD_PLAN.md`のStage 4bと同様に完全に範囲外とし、別タスクとして後回しにする。
書き込み（WRITE(10)）も範囲外とし、まず読み込みだけを対象にする。

参考仕様はUSB Mass Storage Class仕様（Bulk-Only Transport、通称BOT）と、対象を
USBメモリに絞ることで実質必要になるSCSIコマンドサブセット（INQUIRY、
TEST UNIT READY、REQUEST SENSE、READ CAPACITY(10)、READ(10)）。今回はレジスタ
レベルの新規実装が無く（`hcd.rs`のBulk転送経路は既にHIDキーボードの箇所で実機
経由済み）、クラスプロトコルの組み立てが主眼のため、ESP-IDFのコンポーネントを
照合先にする必要は無い。

## Stage 0: 前提の棚卸し（実装前メモ）

`USB_HOST_PLAN.md`のStage 1〜4で以下は既に実機確認済みであり、MSC対応が
新規に踏み込む部分ではない。

- VBUS ON、コア/ポート初期化、接続検出・リセット・速度判定（`hcd::probe_port`）
- コントロール転送によるデバイス列挙（`protocol::enumerate_device`）。MSCデバイスも
  `EnumeratedDevice`として同じ経路で列挙できる（デバイスクラスがHIDでなくMass
  Storageなだけ）
- ハブ経由での列挙・アドレス管理（`usb::connect_keyboard`の直結/ハブ分岐と
  同じ構造をMSC版として複製できる想定）

一方、以下はMSC対応で初めて踏み込む領域であり、`USB_HOST_PLAN.md`の想定通り
「実機でしか踏めない罠」が残っている可能性が高い。

- **Bulk OUT方向のデータフェーズ**: これまでBulk分類（`HCCHAR_EPTYPE_BULK`）で
  実際に使われてきたのはHIDキーボードのInterrupt IN代替（`hcd.rs`の
  `HCCHAR_EPTYPE_BULK`のコメント参照）だけで、方向は常にIN。コントロール転送の
  OUT ステータスステージ（0byte）も実行しているが、これはBOTのCBW送信のような
  実データを伴うBulk OUTとは別物。実機での初検証になる
- **データトグルの持続**: `protocol.rs`のコントロール転送は毎回SETUPで
  トグルがリセットされる前提で書かれている（`data_stage_in`が常にDATA1から
  開始）。Bulkエンドポイントのトグルは転送をまたいで持続するため、
  `hcd::run_packet`を直接呼ぶ側（`msc.rs`）がトグル状態を自分で保持する
  必要がある。`hid_keyboard.rs`のInterrupt INポーリングは毎回DATA0/DATA1を
  固定引数で渡しており、これまでこの問題を踏んでいない
- **STALL回復**: BOT仕様はSTALL発生時に`CLEAR_FEATURE(ENDPOINT_HALT)`
  （標準リクエスト、recipient=endpoint）でエンドポイントのトグルとSTALL状態を
  リセットすることを要求する。未実装のままだと1回のSTALLでデバイスが
  以降ずっと応答しなくなる可能性がある

## Stage 1: MSCデバイスの列挙とBulkエンドポイント検出 ✅ 完了（実機確認済み）

新規モジュール`src/usb/msc.rs`。

- `protocol::enumerate_device`で列挙したMSCデバイスの`config_bytes()`を
  `hid_keyboard::find_hid_keyboard`と同じ要領で走査し、
  `bInterfaceClass=0x08`（Mass Storage）・`bInterfaceSubClass=0x06`（SCSI
  Transparent Command Set）・`bInterfaceProtocol=0x50`（Bulk-Only
  Transport）のインターフェースを探す
- 見つけたインターフェースに続くBulk IN/Bulk OUTの2エンドポイント記述子
  （`bEndpointAddress`のbit7で方向判定）からアドレスと`wMaxPacketSize`を
  控える
- `SET_CONFIGURATION`を発行する（`hub::Hub::open`と同じ理由。Addressステートの
  デバイスがBulk転送に応答する保証はConfigured後のみ）
- `shell.rs`に`usbmsc`コマンドを追加（`usbinfo`/`usbhub`と同じ構成）。
  `hcd::probe_port`→`protocol::enumerate_device`→上記の走査を行い、VID/PID・
  Bulk IN/OUTのエンドポイントアドレスと`wMaxPacketSize`を表示する
- ゴール: 実機でUSBメモリ（`USB_HOST_PLAN.md`のStage 4-4で使用したSony
  054C:0243等）をUSB-Aに直結し、`usbmsc`が正しいBulk IN/OUT
  エンドポイントアドレスを表示することを確認する。ハブ経由接続は
  Stage 0で述べた通り本計画の範囲外（「範囲外（将来検討）」参照）
  → **完了。実機のUSBメモリで`usbmsc`が
  `MSC (BOT) interface 0: bulk IN 0x81 (MPS 64), bulk OUT 0x02 (MPS 64)`
  を表示することを確認した（インターフェース0、Bulk IN/OUTとも
  MPS 64byteのFull-Speed機器）**

## Stage 2: Bulk転送プリミティブとトグル管理 ✅ 完了（実機確認済み、Stage 3と合わせて確認）

- `msc.rs`に`bulk_transfer_out`/`bulk_transfer_in`を実装する。中身は
  `hcd::run_packet`をBulk分類・対象エンドポイント番号で呼ぶだけだが、
  Stage 0で触れたトグル持続の問題があるため、`UsbMassStorage`構造体に
  `in_toggle`/`out_toggle`（`bool`、次に送るPIDがDATA1かどうか）を持たせ、
  転送のたびに読み書きする
- MPSより大きいデータは複数パケットに分割する（`protocol.rs`の
  `data_stage_in`と同じパターンをBulk版として実装する）
- STALL（`PacketOutcome::Error`かつ`HCINT_STALL`）を検出した場合の
  `CLEAR_FEATURE(ENDPOINT_HALT)`実装。標準リクエストなので
  `protocol::control_transfer_out_no_data`と
  `protocol::build_standard_out_setup`をそのまま使える見込み
  （`wValue=0`ENDPOINT_HALT、`wIndex=`対象エンドポイントアドレス）
- 単体では検証しづらいため、ゴールはStage 3のBOT実データ転送（INQUIRY）と
  合わせて確認する（このStage単体の実機確認コマンドは用意しない）
  → **完了。`UsbMassStorage::bulk_transfer_in`/`bulk_transfer_out`として
  実装し、`hcd::run_packet`をBulk分類・対象エンドポイント番号で呼ぶ形にした。
  `in_toggle`/`out_toggle`フィールドで転送をまたいだトグル持続を管理し、
  エラー時は`CLEAR_FEATURE(ENDPOINT_HALT)`（標準リクエスト、endpoint
  recipient）で回復を試みる。実機での検証結果はStage 3参照（初のBulk OUT
  実データ送信であるCBW送信を含め、STALLを一度も踏まずに成功した）**

## Stage 3: BOT (Bulk-Only Transport) CBW/CSW と SCSI INQUIRY ✅ 完了（実機確認済み）

- CBW（Command Block Wrapper、31byte固定）: signature `"USBC"`、tag（呼び出し
  ごとにインクリメントし、対応するCSWのtagと一致するか検証する）、
  data transfer length、flags（bit7=方向、IN=1）、LUN（0固定）、CB length、
  CDB（最大16byte、SCSIコマンド本体）
- CSW（Command Status Wrapper、13byte固定）: signature `"USBS"`、tag、
  residue、status（0=Passed、1=Failed、2=Phase Error）
- 最初のSCSIコマンドとしてINQUIRY（opcode `0x12`、6byte CDB）を実装する:
  CBWをBulk OUT → 36byteのINQUIRYデータをBulk IN → CSWをBulk IN →
  signature/tag/statusを検証する、という一連の流れを`msc::inquiry`として
  実装する
- `usbmsc`コマンドを拡張し、INQUIRYレスポンスのVendor
  Identification/Product Identification/Product Revision Level
  （ASCII、末尾スペースパディング）を表示する
- ゴール: 実機のUSBメモリで`usbmsc`がVendor/Product文字列を正しく表示し、
  CSW statusがPassed（0）であることを確認する
  → **完了。実機のUSBメモリ（Stage 1で確認したSony 054C:0243）で`usbmsc`が
  `Vendor: Sony      Product: Storage Media     Rev: 1.00`を表示した。
  Bulk OUT（CBW送信）・Bulk IN（INQUIRYデータ＋CSW受信）とも初回から成功し、
  STALL回復（`CLEAR_FEATURE(ENDPOINT_HALT)`）の実機経路は今回踏んでいない
  （Stage 0で想定した罠のうち、Bulk OUT自体の初検証は成功で通過。STALL
  回復ロジックは未検証のまま残る）**

## Stage 4: TEST UNIT READY / READ CAPACITY(10) ✅ 完了（実機確認済み）

- TEST UNIT READY（opcode `0x00`、6byte CDB、データフェーズ無し）でメディア
  準備状態を確認する。CSWがFailedを返した場合はREQUEST SENSE（opcode
  `0x03`）でセンスキーを読む処理を用意するが、対象をUSBメモリに絞るため
  詳細なリトライ・エラー分類は行わず、ログ出力にとどめる
- READ CAPACITY(10)（opcode `0x25`、10byte CDB）で最終LBAとブロック長
  （通常512）を取得する。`sdmmc.rs`の`SdCard.capacity_bytes`と対になる
  フィールドとして`UsbMassStorage`に持たせる
- ゴール: `usbmsc`が実機USBメモリの容量（バイト数、既知の実容量と一致する
  ことを目視確認）を表示する
  → **完了。実機のUSBメモリで`usbmsc`が`media ready`の後、
  `capacity: ... blocks x 512 bytes = ... MiB`を表示することを確認した。
  正確な公称容量との厳密な突き合わせは行っていないが、概ね妥当な値
  （ユーザー確認）。TEST UNIT READY／READ CAPACITY(10)ともCSW Passedで
  完走しており、REQUEST SENSE経路（media not ready時）は今回実機で
  踏んでいない**

## Stage 5: READ(10) によるブロック読み込み ✅ 完了（実機確認済み）

- SCSI READ(10)（opcode `0x28`、10byte CDB、LBAと転送ブロック数を指定）で
  1ブロック以上を読み込む。関数シグネチャは`sdmmc::read_block`/
  `read_blocks`に合わせ、`msc::read_blocks(&device, lba, buffer) -> bool`
  とする（呼び出し側からSD/USBの違いを吸収しやすくするため、Stage 6の
  前準備を兼ねる）
- `shell.rs`に`usbread <lba>`コマンドを追加（`sdread`と同じ構成:
  `sdmmc::dump_block`と同じダンプ関数をそのまま流用してUARTへ32行の
  16進ダンプを出す）
- この方式は`sdmmc.rs`と同じく「シェルコマンドのたびに`init()`から
  やり直す（永続状態を持たない）」パターンを踏襲する。Bulkトグルの
  持続はコマンド1回の実行内で閉じるため、複数コマンドをまたいだ
  トグル管理は考えない
- ゴール: 実機USBメモリの`usbread 0`が末尾510-511byteに`55 AA`の
  ブートシグネチャを含む、妥当なMBR/ブートセクタ内容を表示することを
  確認する（`sdread`のLBA0確認と同じ検証方法）

**踏んだ罠（実機、修正済み）**: `usbread`単体では読み込みに失敗する（Bulk IN
タイムアウト）が、先に`usbmsc`（INQUIRY→TEST UNIT READY→READ CAPACITY(10)を
実行）を叩いた直後だと成功することがある、という不安定な壊れ方が実機で
確認された。UARTログでは`USB MSC: bulk IN timed out`が、CBW送信（Bulk OUT）
ではなくREAD(10)のデータフェーズ（Bulk IN）で発生していた。

原因は2点と判断した。

- **`SET_CONFIGURATION`直後にいきなりREAD(10)を投げていた**こと。実際の
  BOTクラスドライバは初回READ/WRITE前にTEST UNIT READYで準備完了を
  ポーリングするのが通例だが、`usbread`はStage 5実装時点でこれを省略して
  いた。ドライブのファームウェアが列挙直後まだ内部処理中で、TEST UNIT
  READYを1往復以上要求するタイプだった可能性が高い（`usbmsc`が先に
  INQUIRY/TEST UNIT READY/READ CAPACITYの3コマンドを叩くことで、結果的に
  この「慣らし運転」を済ませてしまっていたために不安定に再現していたと
  考えられる）
- **`BULK_TIMEOUT_ITERATIONS`（`CONTROL_TIMEOUT_ITERATIONS`と同じ
  2,000,000）がREAD(10)のデータフェーズには短すぎた**可能性。INQUIRY/TEST
  UNIT READY/READ CAPACITYはファームウェアが即答できる小さな応答だが、
  READ(10)は実際にフラッシュへアクセスするため、同じタイムアウト予算では
  足りないことがある

対策として、`UsbMassStorage::wait_until_ready`（TEST UNIT READYを最大10回・
100ms間隔でポーリング）を追加して`usbread`が`read_blocks`の前に必ず呼ぶ
ようにし、`BULK_TIMEOUT_ITERATIONS`も2,000,000→20,000,000（10倍）へ拡大した。
→ **修正後、実機で`usbread 0`を`usbmsc`を挟まず複数回単体実行し、安定して
読み込めることを確認した。**

## Stage 6: ブロックデバイス抽象化とMBRパースの共通化（本計画のゴール） ✅ 完了（実機確認済み）

- `shell.rs`の`cmd_sdmbr`からMBRパース処理（446byte目からの4パーティション
  エントリ走査、`55 AA`シグネチャ確認、`partition_type_name`によるタイプ名
  変換）を、デバイスに依存しない形で新規`src/mbr.rs`へ抽出する。
  受け取るのは読み込み済みの`&[u8; 512]`のみとし、SD/USBのどちらから
  読んだかを一切知らない関数にする（例:
  `mbr::show(console: &mut Console, sector: &[u8; 512])`）
- SD/USB間でディスパッチする最小限の抽象を用意する。このプロジェクトは
  現状`dyn`/トレイトオブジェクトを一切使っておらず（`usb::connect_keyboard`の
  直結/ハブ分岐も`if`で完結する具体型のみ）、本計画の「最小限の抽象化」という
  方針にも合わせ、trait objectではなく列挙型で表現することを推奨する:

  ```rust
  enum BlockDevice {
      Sd(sdmmc::SdCard),
      Usb(usb::UsbMassStorage),
  }

  impl BlockDevice {
      fn read_block(&self, lba: u32, buffer: &mut [u8; 512]) -> bool {
          match self {
              BlockDevice::Sd(card) => sdmmc::read_block(card, lba, buffer),
              BlockDevice::Usb(dev) => msc::read_blocks(dev, lba, buffer),
          }
      }
  }
  ```

  これにより将来デバイス種別が増えても`match`を1箇所足すだけで済み、動的
  ディスパッチのオーバーヘッドも発生しない
- `shell.rs`の`sdmbr`/新設`usbmbr`は、それぞれ対応する`BlockDevice`を組み
  立てて共通の`mbr::show`を呼ぶだけの薄いコマンドにする。1コマンド+
  引数（例: `mbr sd`/`mbr usb`）へ統合する案もあるが、既存の`sdmbr`が
  無引数コマンドとして定着していることと、このプロジェクトの「新しい
  引数解析surfaceを増やすより薄いコマンドを並べる」既存の傾向
  （`sdread`/`sdreadn`/`sdwritetest`/`sdzero`が別コマンドなのと同様）に
  合わせ、**`sdmbr`/`usbmbr`を別コマンドのまま維持する**ことを推奨する
- ゴール（この計画全体のゴール）: `usbmbr`が実機USBメモリのパーティション
  テーブルを`sdmbr`と同じ表示形式で表示し、`sdmbr`が新しい共通コード
  （`mbr::show`）へ差し替えた後も既存のSDカードで従来通り動作すること
  （リグレッション確認）を実機で確認する
  → **完了。実機で`usbmbr`が`sdmbr`と同じ表示形式でパーティション
  テーブルを表示し、`mbr::show`への差し替え後も`sdmbr`が従来通り動作する
  （リグレッションなし）ことを確認した。これで本計画のゴールを達成した**

**実装時の変更点（計画からの差分）**: `BlockDevice`列挙型は実装しなかった。
`sdmbr`/`usbmbr`を別コマンドのまま維持する方針にした結果、各コマンドは
最初から自分がどちらのデバイスを相手にするか確定しており（`sdmbr`は
常に`sdmmc::read_block`、`usbmbr`は常に`UsbMassStorage::read_blocks`）、
実行時にSD/USBを切り替える呼び出し元が1つも無い。この状態で
`BlockDevice`を導入すると、一度も`match`されない列挙型と一度も呼ばれない
メソッドが残るだけになり（`dead_code`警告の対象）、本計画の
「タスクが要求する以上の抽象化をしない」という方針に反する。そのため
Stage 6は「`mbr::show`によるパース処理の共通化」だけを実装し、
デバイス選択の抽象化はまだ導入していない。将来Stage 4b（ファイル
システム層）が「SD/USBのどちらが挿さっていても同じコードで読む」という
実行時ディスパッチを本当に必要とした時点で、`BlockDevice`（または同等の
抽象）を導入するのが適切と判断する。

- `src/mbr.rs`（新規）: `mbr::show(console, sector: &[u8; 512])`。
  `cmd_sdmbr`から抽出したパーティション走査・`55 AA`シグネチャ確認・
  `partition_type_name`をそのまま移設。デバイスについて一切知らない
- `shell.rs`: `Line`（行フォーマッタ）を`pub(crate)`にして`mbr.rs`と共有。
  `cmd_sdmbr`は`sdmmc::read_block`でLBA 0を読んで`mbr::show`を呼ぶだけの
  薄い実装に置き換えた。新設`cmd_usbmbr`（`usbread`と同じ機器起動手順:
  `probe_port`→`enumerate_device`→`UsbMassStorage::attach`→
  `wait_until_ready`）が`read_blocks(0, ...)`で読んだLBA 0を同じ
  `mbr::show`へ渡す

## 範囲外（将来検討）

- FAT/exFAT等ファイルシステム本体の解析。`SD_CARD_PLAN.md`のStage 4b以降と
  共通のタスクとしてまとめて着手する想定（`BlockDevice`抽象はそのまま
  ファイルシステム層の下敷きにできる見込み）
- WRITE(10)による書き込み。SD側で`sdwritetest`実装時に踏んだ「書き込み後
  カードがビジー状態になる」に類する罠がUSB側にもある可能性が高く、
  読み込みが安定してから別途着手する
- 複数LUN対応、複数パーティションを跨いだGPT解析（`sdmbr`と同じくGPT
  保護MBRの検出止まりとする）
- MSCデバイスの複数同時使用、抜き差し耐性（`UsbKeyboard`の
  `needs_reinit`相当の自己回復）の作り込み。まずは1回のシェルコマンド
  実行内で完結する動作を優先し、`sdmmc.rs`/既存USBコマンドと同じ
  「コマンドのたびに再初期化」で十分とする
- リムーバブルメディア（USBカードリーダー等）のメディア差し替え検出
  （UNIT ATTENTION、SCSIのメディア変更通知）
- **USB MSCのハブ経由接続**（→ `USB_REFACTOR_PLAN.md` Stage Fで対応済み。
  以下は本計画完了時点の記録）。`usb::connect_keyboard`が持つ「直結／ハブ経由の
  判定」（`enumerate_device`をハブのポート越しに呼ぶ分岐、
  `USB_HOST_PLAN.md`Stage 4-5）に相当する`connect_mass_storage`のような
  関数は未実装。`usbmsc`/`usbread`/`usbmbr`はいずれも`usbinfo`と同じく
  `usb::enumerate_device(usb::ROOT_DEVICE_ADDRESS, false)`でUSB-A直結
  デバイスのみを対象としており、ハブの下に挿したUSBメモリは扱えない
  （Stage 1完了時点のゴール文言が「直結でもハブ経由でも可」としていたのは
  誤りで、実装・実機確認とも直結のみ。修正済み）。Low-Speedデバイス自体も
  MSC用途では通常存在しないため、`hcd::Endpoint::low_speed_via_hub`絡みの
  検証もしていない

## 想定される罠（実装前メモ、実機で要検証）

`SD_CARD_PLAN.md`・`USB_HOST_PLAN.md`同様、以下は実機でしか踏めない可能性が
高い項目として着手前に注意しておく。

- **Bulk OUTの初検証**: これまでOUT方向のBulkデータフェーズを一度も実機で
  送っていない。コントロール転送のOUTステータスステージ（0byte、
  トグル固定）とは別物であり、CBW送信（31byte、Bulk OUT）で初めて
  実データを伴うOUT転送を行うことになる
- **トグルの持続とチャネル0共有**: `hcd.rs`は「チャネル0を1パケットごとに
  全部書き切る、呼び出し間の状態を持たない」設計（`USB_HOST_PLAN.md`の
  モジュール構成参照）。Bulkのトグルだけは例外的に呼び出し側
  （`msc.rs`）が持続管理する必要があり、既存の設計方針と初めて
  部分的に食い違う。トグルの初期値（Configured直後はDATA0）を
  取り違えると、STALLではなく「データ化けだけして見た目は成功する」
  という気づきにくい壊れ方をする可能性がある
- **STALL回復の要否**: `CLEAR_FEATURE(ENDPOINT_HALT)`を実装しないまま
  STALLを踏むと、`UsbKeyboard`のような自己回復（`needs_reinit`）が
  無い限りそのセッションは詰む。最初のINQUIRY実装時点でSTALLが
  起きるかどうかは実機依存
- **CBWのtag管理**: CSWのtagがCBWのtagと一致することを確認しないと、
  古い応答を新しい応答として誤読する可能性がある（BOT仕様が要求する
  検証）
- **32bit LBA上限**: READ CAPACITY(10)は32bit LBAまで（2TiB相当）。
  対象はUSBメモリのため通常問題にならない想定だが、大容量デバイスで
  容量が異常値になった場合はREAD CAPACITY(16)への切り替えが必要になる
  可能性がある

## モジュール構成（実際）

- `src/usb/msc.rs`: BOT CBW/CSW組み立て、Bulk転送プリミティブとトグル
  管理（`bulk_transfer_in`/`bulk_transfer_out`）、STALL回復
  （`CLEAR_FEATURE(ENDPOINT_HALT)`）、SCSIコマンド（INQUIRY/
  TEST UNIT READY/`wait_until_ready`/REQUEST SENSE/READ CAPACITY(10)/
  READ(10)）、`UsbMassStorage`構造体。`find_msc_interface`（Stage 1）も
  同じファイル
- `src/usb.rs`: `msc`のサブモジュール宣言・`UsbMassStorage`/
  `find_msc_interface`の再エクスポートを追加。直結/ハブ経由の判定
  （`connect_keyboard`相当の`connect_mass_storage`）は未実装のまま
  （`usbmsc`/`usbread`/`usbmbr`は`usbinfo`と同じく直結デバイスのみを
  対象にしている。ハブ経由対応は範囲外のまま）
- `src/mbr.rs`（新規）: `cmd_sdmbr`から抽出したMBRパース処理
  （`&[u8; 512]`のみを受け取り、デバイスについて何も知らない）。
  `shell.rs`の`Line`（`pub(crate)`化）を使って出力行を組み立てる
- `src/shell.rs`:
  - `usbmsc`コマンド追加（Stage 1〜4の確認用: 列挙・エンドポイント・
    INQUIRY・TEST UNIT READY・READ CAPACITY(10)）
  - `usbread <lba>`コマンド追加（Stage 5の確認用。`read_blocks`前に
    `wait_until_ready`を挟む）
  - `usbmbr`コマンド追加、既存`sdmbr`は共通の`mbr::show`呼び出しへ
    差し替え（Stage 6）

## 各段階の完了条件（実機確認）

`SD_CARD_PLAN.md`・`USB_HOST_PLAN.md`と同じく、各StageはUARTシェル経由で
コマンドを叩いて目視確認できることをもって完了とし、次のStageへ進む前に
必ず実機でログを確認する。
