# ストレージ（SDカードとUSBマスストレージ）

> 索引: [`../DESIGN.md`](../DESIGN.md) ／ 段階分けと実機で踏んだ罠:
> [`SD_CARD_PLAN.md`](plans/archive/SD_CARD_PLAN.md)、[`USB_MSC_PLAN.md`](plans/archive/USB_MSC_PLAN.md)、
> [`USB_WRITE_STABILITY_PLAN.md`](plans/archive/USB_WRITE_STABILITY_PLAN.md)、
> [`USB_MSC_BOOT_MARGIN_PLAN.md`](plans/archive/USB_MSC_BOOT_MARGIN_PLAN.md)

ブロック単位の読み書きと、その上の共通ブロックデバイス層・MBR判定までを
実装しており、ファイルシステムは扱いません。SDカードとUSBメモリは
`src/fs/`の`BlockDevice`として同じ形で見えます。

## SDカード（`src/sdmmc.rs`）

Tab5のmicroSDスロットはSDIO1にIOMUX経由（GPIOマトリクスを通さない）で
接続されています。GPIO39〜44がD0/D1/D2/D3/CLK/CMDです。カードVDDは
`SOC_3.3V`直結で電源制御GPIOはなく、スロットのDetectピンもSoCへ配線されて
いないため、カードの有無はコマンドのタイムアウトから推定します。

実機確認済みの範囲:

- 4bitバスモード
- カード対応時はHigh Speedモード（CMD6 SWITCH_FUNC、規格上限50 MHz、
  ホスト実クロック40 MHz）。複数枚のカードでHigh Speed対応と読み込み成功を確認。
  ただしESP32-C6（同じコントローラのカード1）が活性化済みの間は
  Default Speedの20 MHzに留めます。入力クロックが共有で、倍にするとC6を
  High Speed無効のまま40 MHzで駆動してしまうためです（[WIFI.md](WIFI.md)）
- カード活性化（CID/CSD取得）
- IDMAC（内蔵DMA）経由の単一・複数ブロック読み書き（CMD18/CMD25はハードウェア
  auto-stopを使い、手動のCMD12は使わない）
- SDから直接PSRAMへのDMA転送（`sdreadpsram`）
- MBRパーティションテーブルの表示

IDMACのディスクリプタと転送先バッファは内蔵SRAMに置きますが、ESP32-P4では
内蔵SRAMもPSRAMと同様にL1/L2キャッシュの背後にあるため、`psram.rs`と同じ
`Cache_WriteBack_Invalidate_Addr`が必要です（ディスクリプタを渡す前に
write-back、DMAが書いたバッファをCPUが読む前にinvalidate）。

このROM関数は**開始アドレスが64 byteのキャッシュライン境界にない範囲を拒否**し、
拒否されるとCPUは転送前の内容を読み続けます（エラーは出ません）。`sdmmc.rs`は
非整列のバッファを整列済みステージングバッファ経由で転送し、キャッシュ操作が
拒否された場合は転送失敗として扱うので、呼び出し側はアラインメントを意識する
必要がありません。経緯は[`KNOWN_ISSUES.md`](KNOWN_ISSUES.md)を参照してください。

1ブロックの読み出しはCMD17、複数ブロックはCMD18を使います。

CPU/APBによる`SDHOST_BUFFIFO_REG`の直接読み出しと`IDSTS`のRIビットには実機固有の
制約があり、いずれも使用していません。詳細は
[`KNOWN_ISSUES.md`](KNOWN_ISSUES.md)を参照してください。

## USB Mass Storage（`src/usb/msc.rs`）

Bulk-Only Transport（BOT）でSCSIコマンドを送ります。実機確認済みなのは
INQUIRY、TEST UNIT READY、READ CAPACITY(10)、READ(10)です。WRITE(10)は
実装・実機受入済みです。当初はbulk転送のpacket errorからcontrol転送まで巻き込んで
sessionが死ぬ間欠故障があり、予防的BOT再同期とMSC session隔離で緩和していました。
その後HCD側の契約（DMA cache同期、descriptor完了の検査、実転送長の単一化、cleanup失敗の
伝播）を整えた結果、**予防的BOT再同期は不要になり撤去しました**
（[`USB_BOT_HCD_REFACTOR_PLAN.md`](plans/archive/USB_BOT_HCD_REFACTOR_PLAN.md)）。High-Speed直結、
FS-onlyハブ＋HID併用、High-Speedハブ＋Low-Speed HID併用の3構成でREAD／WRITE試験を
完走しています。
確定した事実・否定した仮説・入れた緩和策は
[`USB_WRITE_STABILITY_PLAN.md`](plans/archive/USB_WRITE_STABILITY_PLAN.md)にまとめてあります。
Bulk転送は
コントロール転送ではないため`protocol.rs`を通らず、`hcd.rs`のパケット
プリミティブを直接使い、エンドポイントごとのデータトグルを自分で管理します。

BOT Reset Recoveryが失敗、または回復直後のcommandも続けて失敗した場合は、そのMSC
sessionだけを使用不能にして以後のcommandを即時に失敗させます。channel 0を正常にhalt
できている限り、別チャネルで動作中のHIDを巻き込む自動再列挙は行いません。再開には
`usbrescan`を明示的に実行します。channel 0自体をhaltできなかった場合だけはcontroller全体の
故障として次のフレーム境界で全バスを再列挙します。

Bulk QTDはCPU周波数から約1秒で一度区切り、同じBOT phaseのまま最大4回再投入するため、
合計待ち時間は約5秒です。BOT Reset Recoveryでも使うcontrol packetは約1秒です。CBW送信後に
Bulk転送がtimeout／transaction error、またはCSW不正になった場合は、BOT Reset
Recovery（Mass Storage Reset class request、Bulk IN／OUTそれぞれの
`CLEAR_FEATURE(ENDPOINT_HALT)`、host toggleのDATA0復帰）を実行します。Mass Storage Reset
の直後は150 ms待ってから最初のhalt解除を送ります。Full-Speed媒体がreset requestを処理中の
まま即時のSETUPを取りこぼす頻度を下げるためです。実機では同じ媒体でもRecoveryが完了する
runとCLEAR_FEATUREで失敗するrunがあり、この待機だけで回復を保証するものではありません。
読み出し専用のREAD(10)と、媒体確認に使うTEST UNIT READY、READ CAPACITY(10)、
INQUIRY／INQUIRY(EVPD)はRecovery後に1回再送します。**WRITE(10)は自動再送しません。**
途中で失敗した書き込みはメディアへ
一部だけ届いている可能性があり、再送すると不確かな
ブロックが増えるだけだからです。この方針を保つため、再送処理はBOT共通層ではなく
commandの意味を知るMSC class driverのREADと再送安全な照会に限定してあります。

正常なcommand境界ではhost cleanupを行いません。かつては反復READ(10)の16回ごとと
各WRITE(10)の直前にhost controllerのchannel／FIFO cleanupを行っていました——FS-onlyで
33〜40回、High-Speed直結でも52回後にBulkとEP0が無応答になる実測への緩和策です。

これを撤去できるかは実機A/Bで判定しました。READ側は間隔`16`／`32`／`disabled`の3設定、
WRITE側はon/offで、いずれもbuild時に選ぶ一時的な設定です。`disabled`のbinaryで
High-Speed直結、FS-onlyハブ＋HID＋MSC、High-Speedハブ＋Low-Speed HID＋High-Speed MSCの
3構成とも`usbcheck 1000`（READ 1000回）が`proactive read+0`で完走し、旧故障の最短33回の
30倍の距離を越えました。WRITE側も同じ3構成で各100回、`fswritetest`を2メーカーの媒体で
各10回連続実行して通りました。**両方とも撤去済みです。**

撤去できた理由は緩和策が不要になったからで、故障が消えたからではありません。
持ち越しの元だったcontroller側residueは、descriptor完了の検査（`HCINT.XferCompl`と
QTD Active解除を同じ世代で確認）、DMA bufferをHCDが所有して整列させる契約、
実転送長を1箇所で導出する契約が、そもそも次commandへ渡らないようにしています。
失敗後のcleanupは残っており、そこでFIFO flushがtimeoutした場合はBOT Reset Recoveryを
実行せずsessionを引退させます（[`DIAGNOSTICS.md`](DIAGNOSTICS.md)の`cleanup-failed`）。

WRITE(10)は失敗後に安全な自動再送ができないという方針自体は変わりません。失敗した
WRITEは再送せず、呼び出し側へ失敗を返します。

High-Speed直結の`usbwritetest 2`は10/10回、pattern照合、原本復元、周辺LBA照合がすべて
成功しています。pattern書き込みと復元を合わせて計20回のWRITEを連続成功しました。
FS-onlyハブ＋HID併用でも10/10回成功しました。給電中のHID事前接続ハブも上流再接続を
5/5回認識し、初回列挙修正を含む実機受入を完了しています。

BOTのcommand resultはhost実転送長に加えて、期待data長とCSW residueを保持します。
READ(10)／WRITE(10)の固定長I/Oは、CSW PASSEDだけでなく全量転送かつresidue 0でなければ
成功にしません。特にWRITE(10)はhostが全dataを送れていてもdeviceが非zero residueを返した
場合があり得るため、これを成功へ丸めません。一方、INQUIRY VPDは可変長応答なので、host
実転送長とresidueが矛盾しないことをBOT層で確認した後、response headerが宣言する長さまでを
使います。data INの代わりにCSWが先着した場合はdata bufferへ混ぜず、current commandの
FAILED応答またはstale tagとして処理します。

`usbwritetest <lba>`は`sdwritetest`より検査が厚くなっています。対象LBAだけでなく
**前1・後2ブロックの窓**を事前に読んで保持し、パターン書き込み後に窓全体を読み直します。
対象以外が変化していれば`COLLATERAL DAMAGE`として表示し、保持した内容でそのブロックも
書き戻します。**対象LBAに書いて対象LBAから読み戻すだけでは、デバイスが別の場所へ書いても
必ず一致してしまう**ためです（実機で実際にデータ破損が起きました。
[`USB_MSC_PLAN.md`](plans/archive/USB_MSC_PLAN.md)の追補）。

書き込みのたびにSYNCHRONIZE CACHE(10)（`msc::synchronize_cache`）でフラッシュを試み、
照合の読み出しはFUA（Force Unit Access）付きREAD(10)（`read_blocks_from_medium`）で
**キャッシュではなく媒体**を読みます。デバイスがSYNCHRONIZE CACHE(10)を
ILLEGAL REQUEST（sense key 5）で断った場合は「非対応」と判定してそのsession中は送らず
（ASCは`0x20`とは限らず、実機には`0x24`を返す個体があります）、
FUAを断られた場合は通常のREAD(10)へ落として画面にその旨を出します。CHECK CONDITIONが
返った場合はREQUEST SENSEでsense key／ASC／ASCQをUARTへ出します（デバイスは誰かが
読むまでsenseを保持するため、回収の意味もあります）。フラッシュ後は
`wait_until_ready`でデバイスの内部処理完了を待ってから次のコマンドを出します。
WRITE(10)の成功はデバイスに届いたことしか意味せず、読み戻しも同じキャッシュから
返り得るためです。書き込み前にREAD CAPACITY(10)で論理ブロック長が512であることと
LBAが容量内であることも確認し、窓が読めなければ何も書かずに中止します。書き込みに
失敗した場合はREQUEST SENSEのsense key／ASC／ASCQを表示し、sense key 7
（DATA PROTECT）は「write protected」と明示します。`ut`と`mix`はread-onlyのままで、
書き込みは行いません。

`usbzero <lba> [count]`は`sdzero`のUSB版で、指定範囲をゼロで上書きします。
**`usbwritetest`が途中で失敗して残したパターンを消すためのコマンド**でもあります。
消去を`usbwritetest`の失敗処理に組み込んでいないのは、あのテストが諦める時点では
たいてい転送層ごと死んでいて、後始末の書き込みも失敗するからです。`usbrescan`で
sessionを作り直してから別コマンドとして実行します。1ブロックごとにフラッシュし、
FUA付きREAD(10)で**媒体から**ゼロを読み直して確認し、失敗したLBAで止めて
sense keyを表示します。**成否は読み戻しで判定し、フラッシュの失敗は注記として別に
出します**——デバイスが実行しないフラッシュは、直前の書き込みについて何も語らないため
です（`usbwritetest`も同じ）。

`usbrawcheck <lba> [writes] [span] [gap_ms]`はUSB transportのfilesystem非依存raw burst試験です。
filesystem APIを通さず、指定した犠牲範囲（既定1 block、最大8 blocks）へ単一block WRITE(10)を
READ／flush／ready pollなしで連続発行します（既定32回、最大256回）。最後にだけ媒体から
最終patternを読み戻し、開始時snapshotの復元と再照合を試みます。**指定範囲は必ず、保持したい
filesystemの全partition外に置きます。**transport failureで復元できなくてもfilesystem objectは
残らず、sessionを`usbrescan`して同じ犠牲範囲を再利用できます。Stage 3では全6構成で安定を
確認し、`fswritetest`の代わりにtransport受入として採用しました。filesystem層の受入は
別段階で行います。

`usbmultiwrite <lba> <2|4|8>`は、複数block WRITE(10)を再評価したStage 7診断です。
同じ範囲へ10回発行し、各回をflush後のFUA READで照合します。
指定範囲の前後1 blockもguardとしてsnapshot・照合し、最後に受入済みのsingle-block WRITEで
原本へ戻します。成功packetのrequested／actual／DATA PIDと最終CSW residueはUARTへ出ます。
transport failureで復元不能になる可能性があるため、**指定範囲と前後guardのすべてを
filesystem外の失ってよいLBAに置きます**。`1234:5645`と`054C:0243`の2媒体について、
High-Speed直結とFull-Speed固定ハブ＋HIDの2 topologyで2／4／8 blockを各10回通したため、
filesystem adapterの`MAX_WRITE_BLOCKS`は8へ増やしました。

descriptor DMAを使うHigh-Speed／Split以外の既存経路では、QTD status 1をESP-IDF 5.5.3と同じくpacket error
（CRC、transaction timeout、stuff、false EOP、excessive NAK）として扱います。
各descriptorはendpoint MPS以下の1 packet QTDに限定し、非Split BOT Bulk OUTも1 packetごとに
channelをarmします。v36〜v39で試した複数packet QTD／複数QTD listはFull-Speed WRITEを悪化
させたため撤回しており、現行経路では使いません。HCCHAR MC/ECはCBW／CSW／Bulk INと同じ0を
維持します。

**再送の可否は、packetが「失敗を報告された」のか「放棄された」のかで分かれます**
（[`USB.md`](USB.md)の「実転送長とretry安全性の契約」）。

- **coreがpacket errorを報告したpacket**は、endpoint toggleを進めず同一packet／同一
  DATA PIDを50 ms間隔で最大20回まで再送します。USB packetは不可分で、ここでのQTDは
  1 packetしか運ばないため、失敗の報告は「deviceが受け取らなかった」か「受け取ったが
  handshakeが失われた」を意味します。ACKを失ったOUTでもdevice側は同じPIDのduplicateを
  再消費しないため安全です。
- **channelがhaltせず放棄したpacket**は、descriptorが「1 byteも動いていない」と
  証明できるときだけ再送します。completionが無いのでcoreがpacket途中だった可能性を
  否定できません。証明できない場合はtransport errorとして上げ、
  `USB BOT: refusing to resubmit after ...`を出します。実機では64 byteのOUTが
  `requested=64 actual=64`のまま4回再送されていました。

**このcoreはpacket error時に不可能な残量を書き戻すことがあります**（64 byte要求に対して
100,489や84,041を実測）。いずれも正規のwritebackでbyte欄だけが成立していないため、
残量として使わず`usbcheck`の`impossible-len`で数えます。非Split BOT Bulkはdescriptor DMAを使い、
channel-0の固定2-slot QTD bankをpacketごとに交互使用します。retryと次packetが直前の物理QTD
addressを即時再利用しないための所有境界です。4 KiB READ(10)もMPS単位へ分割し、各packetの
完了後にsoftwareがDATA PIDを1回進めます。
reported Bulk OUT packet errorはnon-periodic TX FIFOをcleanupし、同じDATA PIDでretryします。
descriptor-DMA mode再始動はB1の停止packetで反復しても結果を変えなかったため使用しません。
複数QTDのWRITE listはB2で正常WRITEを後退させたため使用せず、非Split BOT OUTも1 packet
QTDずつ実行します。reported packet errorだけを同じDATA PIDで再送し、haltしなかったpacketは
descriptorが0 byte進行を証明できる場合以外再送しません。
QTDのIN byte数はMPSの倍数にする必要があるため、
13 byte CSW、36 byte INQUIRY、8 byte READ CAPACITYはMPSサイズの内蔵SRAM stagingへ受け、
実受信byteだけを呼び出し側へコピーします。
短い応答が要求長を超えた場合は、要求長、HCDが報告した実受信長、先頭16 byteを4つの
little-endian wordでUARTへ記録します。READ CAPACITYの8 byte要求に対して13 byteかつ先頭
`0x53425355`（ASCII `USBS`）なら、容量dataではなくCSWを受けたことになり、hostとdeviceの
BOT phase不一致をbyte数計上の誤りから区別できます。同じblockには、そのpacketを成功扱いした
channel-0のHCINTとQTD controlも記録します。通常のdescriptor-DMA成功はHCINTのXferComplと
QTD Active解除の両方を必要とし、ChHltdだけ、またはActiveの残ったdescriptorは成功として返しません。

起動時のスキャンは、USB MSCが見つかった場合にTEST UNIT READY／READ
CAPACITY(10)／READ(10) LBA 0までを実行し（ready待ちの上限は4,000 ms）、
各段階の所要時間をUARTへ出します。not readyのときはREQUEST SENSEで理由を確認し、
sense key 2かつASC `0x3A`（MEDIUM NOT PRESENT）であれば**待たずに即座に諦めます**。
空のカードリーダーは挿さっている限りこの答えを返し続けるためで、実測では上限の
10秒を使い切っていました。起動中（ASC `0x04`）などその他の理由は上限まで待ちます
（`USB BOOT:`、[`DIAGNOSTICS.md`](DIAGNOSTICS.md)）。将来ファイルシステム層が
起動時にUSB MSCを最優先で選ぶために必要な待ち時間を実測で決めるための計測で、
`usbmargin`はそれをVBUSの再投入で繰り返します。計測の目的と確定条件は
[`USB_MSC_BOOT_MARGIN_PLAN.md`](plans/archive/USB_MSC_BOOT_MARGIN_PLAN.md)を参照してください。

USBハブのポートに挿したUSBメモリも同じレジストリに乗ります
（[`USB_REFACTOR_PLAN.md`](plans/archive/USB_REFACTOR_PLAN.md) Stage F）。`usbmsc`／
`usbread`／`usbmbr`はいずれもレジストリを引くので、直結とハブ経由を
区別しません（[`USB.md`](USB.md)）。

## ブロックデバイス層（`src/fs/`）

[FILESYSTEM_PLAN.md](plans/archive/FILESYSTEM_PLAN.md)のStage 1にあたる層です。SDカード、
USB Mass Storage、PSRAM上のRAMディスクを、論理ブロック単位の共通interface
`fs::block::BlockDevice`（`geometry`／`read_blocks`／`write_blocks`／`flush`）
で扱えるようにします。ファイルシステムそのものはまだありません。

読み取り専用と読み書きでtraitを分けていません。書き込んで良いかはVFSの
mount policyが決めることで、転送層の性質ではないためです。SD・USB・RAMの
3つとも`write_blocks`は実転送を行います。かつては物理媒体のadapterが
コマンド発行前に無条件で失敗させていましたが、読み書き可能な媒体になった以上
その拒否は`MountMode`の1箇所に集約してあります
（[FILESYSTEM.md](FILESYSTEM.md)）。

`write_blocks`の共通条件は次のとおりです。

- `check_range`を転送前に通し、パーティション相対LBAから物理LBAへの変換は
  `PartitionBlockDevice`だけが行います
- 長い転送は媒体ごとの上限へ分割します
- **部分成功は返しません。** 途中まで転送してから失敗した場合も`Err`です
- **失敗した書き込みを自動再送しません**
- 下位ドライバが可変スライスを要求する箇所へ、`&[u8]`から`unsafe`で可変性を
  戻すことはしません。整列済みのstaging bufferへcopyします

LBAと容量は`u64`、論理ブロック長は`BlockGeometry`が持ちます。ただしMBRと
ファイルシステムの経路が受理する論理ブロック長は**512 byteだけ**で、それ以外は
`UnsupportedBlockSize`として解析前に拒否します。

- `src/fs/ramdisk.rs`: PSRAMの固定8 MiB領域（[PSRAM.md](PSRAM.md)）。唯一の
  読み書き可能な媒体で、起動ごとにFAT16でformatし直します。内容はリセットで
  消え、`flush()`は永続化を保証しません
- `src/fs/sd.rs`・`src/fs/usb_msc.rs`: 既存ドライバの上に載る薄いadapterです。
  IDMACのディスクリプタ制約、BOT recovery、packet単位の再送といった媒体固有の処理は
  下層に残し、ここは結果の変換と転送分割だけを行います。SDはCSD version 1.0の
  カード（容量を復号していない）をこの層では扱いません

### 媒体ごとの書き込み完了境界とflush

| 媒体 | 完了境界 | `flush()` |
| --- | --- | --- |
| RAMディスク | メモリへの書き込みそのもの | 何もしない。永続化は保証しない |
| SDカード | CMD25の完了後、DAT0のbusy解除まで確認した時点 | 何もしない（上の境界で既に達成済み） |
| USB MSC | WRITE(10)がdeviceに受理された時点 | SYNCHRONIZE CACHE(10)とready待ち |

**USBのWRITE(10)は最大8ブロック（4 KiB）です**（`MAX_WRITE_BLOCKS = 8`）。READ(10)も
4 KiBまでまとめるため、filesystem adapterの転送上限は方向で同じになりました。かつては
複数ブロックを1回のdata OUTフェーズに入れるとCSWが戻らず、1ブロックへ制限していました
（[USB_WRITE_STABILITY_PLAN.md](plans/archive/USB_WRITE_STABILITY_PLAN.md)の「決定論的な再現手順」）。
HCD／BOT契約とroot-port reset後のFIFO再適用を修正した後、Stage 7で2媒体×2 topologyの
2／4／8 blockを各10回通したため上限を増やしました。8ブロックを超える呼び出しはadapterで
4 KiBごとのWRITE(10)へ分割します。WRITE失敗を自動再送しない方針は変わりません。

SDの`flush()`が何もしないのは、USBより弱い保証だからではありません。`sdmmc.rs`の
`write_blocks`はCMD25が完了し、さらにカードがDAT0を離す（フラッシュへの書き込みを
終えた）まで戻りません。ホスト側に遅延write cacheが無いので、押し出すものが
ありません。`wait_data_not_busy`はtimeoutを成否として返し、busyのまま予算を
使い切ったカードは転送の失敗として上位へ届きます。

USBは`CacheSync::Flushed`が成功で、そのあとTEST UNIT READYでデバイスが戻るのを
待ちます（100 ms間隔で最大10回）。キャッシュのコミット中はデバイスが一時的に
応答しなくなることがあり、待たずに戻ると次のコマンド——多くは呼び出し側の
読み戻し——が「ただ忙しいだけ」のデバイスで失敗します。`CacheSync::Failed`と
ready待ちの失敗はI/O失敗です。

**コマンドが失敗したときにどちらへ分類するかは、デバイスが何と言ったかで決めます。**

| senseの内容 | 分類 |
| --- | --- |
| MEDIUM ERROR、HARDWARE ERROR、NOT READYなど具体的な故障 | `Failed`（I/O失敗） |
| ILLEGAL REQUEST | `Unsupported`（実装していない） |
| NO SENSE、またはresponse codeが`0x70`〜`0x73`でない無効なsense | `Unsupported` |
| senseを取る途中でBOT sessionが死んだ | `Failed` |

3行目は実機で踏みました。CSW status `0x01`で失敗しながらsenseを18 byteのゼロで
返す個体があります。**故障を1つも報告していない応答**なので、渡せる故障もありません。
`msc.rs`自身の規定どおり「デバイスが実行しないフラッシュは直前の書き込みについて
何も語らない」——書き込み自体はCSW PASSEDで受理済みで、ここで全書き込みを失敗させても
データが安全になるわけではなく、このデバイスが使えなくなるだけです。senseのresponse
codeは検証するようになっており、無効なら`invalid sense response code=`を出して
診断には使いません。
`CacheSync::Unsupported`は**best effortの成功**として扱います。実行しない
フラッシュはデバイスの言い分であって、それを理由に書き込み自体を失敗させると
そういうスティックが書けなくなるだけで安全にはなりません。ただしsession中
最初の1回だけ`flush unsupported; removal durability is not guaranteed`を
UARTへ出します。WRITEがdeviceへ受理されたこと以上の永続化は保証しません。

`usb_msc.rs`の`write_blocks`は`WriteOutcome::WriteProtected`（sense key 0x07
DATA PROTECT）を`BlockError::WriteProtected`へ写します。転送は何も壊れておらず、
再試行しても答えは変わらないので、`DeviceError`とは分けてあります。
- `src/fs/partition.rs`: `PartitionRange`（開始LBAと長さのデータ）と、I/Oの間
  だけデバイスを借りる`PartitionBlockDevice`に分けてあります。マウントが
  デバイスを所有しないので、同じディスクの`p1`と`p2`を同時に扱えます

### MBRの判定

`55 AA`だけではMBRと、パーティションテーブルを持たないFAT/exFATボリューム
（superfloppy）を区別できません。そこで`src/fs/mbr.rs`は両方の読み方を試し、
**ちょうど一方だけが成立したときにだけ**媒体を使います。

- MBRとして成立する条件: `55 AA`があり、4つのboot indicatorがすべて`0x00`か
  `0x80`で、少なくとも1つのentryが使用可能であること
- entryの検査: 長さ0、開始LBA 0、媒体範囲外を除外します。重なり合うentryは
  **両方**除外します。どちらが誤りかテーブルからは判断できないためです
- ブートセクタとして成立する条件: exFATは`EXFAT   `と`MustBeZero`領域、FATは
  jump命令・セクタ長・クラスタサイズ・FAT数・メディア記述子・ルート構成・
  総セクタ数フィールドをそれぞれ規格が許す値かで判定します（`src/fs/bootsector.rs`）。
  **判定に落ちた場合は理由をUARTへ出します**（`no jump instruction at the start`、
  `cluster larger than 64 KiB`など）。パーティション表には見えているものを
  マウントできないとき、媒体が壊れているのか検査が厳しすぎるのかを一語では
  区別できないためです
- **判定できることと、マウントできることは別です。** この検査が受け付けるクラスタサイズの
  上限は64 KiBですが、`hadris-fat`は32 KiBを超えるクラスタを開きません
  （[FILESYSTEM.md](FILESYSTEM.md)の「クラスタサイズの上限」）。64 KiBクラスタの
  ボリュームはここで「FATのブートセクタである」と正しく判定され、そのあとドライバに
  断られます。この順序は意図的で、ブートセクタでないと答えると`mbr.rs`の
  superfloppy判定にも嘘をつくことになるためです
- 両方成立した場合は`AmbiguousLayout`として拒否します。推測して外すと、
  生きているボリュームの途中をパーティションとして切り出すことになるためです

対応するのはclassic MBRのprimary entry 4個だけで、extended partition、GPT、
複数LUNは対象外です。

## シェルコマンド

| コマンド | 内容 |
| --- | --- |
| `sdinfo` | カードを活性化し、CID/CSDの要約を表示 |
| `sdread <lba>` | 1ブロック（512 byte）読み出してUARTへダンプ |
| `sdreadn <lba> <n>` | nブロック（n≦8）をDMAで読み出してUARTへダンプ |
| `sdwritetest <lba>` | 1ブロックの書き込み・照合・復元 |
| `sdzero <lba>` | ゼロクリアした1ブロックを書き込む |
| `sdreadpsram <lba> <n>` | nブロック（n≦8）をPSRAMへDMAし、SRAM経由と照合 |
| `sdmbr` | LBA 0のMBRパーティションテーブルを表示 |
| `usbmsc` | INQUIRY／TEST UNIT READY／READ CAPACITY(10)の結果を表示 |
| `usbread <lba>` | SCSI READ(10)で1ブロック読み出してUARTへダンプ |
| `usbwritetest <lba>` | SCSI WRITE(10)で1ブロックの書き込み・照合・復元（`sdwritetest`のUSB版） |
| `usbrawcheck <lba> [writes] [span] [gap_ms]` | filesystem外の犠牲範囲へ単一block WRITEを発行し、最後に照合・復元するraw試験。gapは成功したWRITE command間の0〜2000 ms、既定0 |
| `usbmultiwrite <lba> <2\|4\|8>` | 複数block WRITE(10)を同一範囲へ10回発行するStage 7診断。各回の媒体照合、前後guard検査、原本復元、packet／CSW trace付き |
| `usbzero <lba> [count]` | 1〜8ブロックをゼロで上書きし、媒体から読み直して照合（破壊的） |
| `usbmbr` | LBA 0のMBRを`sdmbr`と同じ書式で表示 |
| `ut [count]` | 同じ4 KiBを反復read・比較するread-only試験（既定100回、Recovery再送数も表示） |
| `usbcheck [reads] [lba]` | 1構成ぶんの受入試験。read soakと、LBA指定時はwrite 10回を実行し、counterの**差分**とGo条件ごとのPASS/FAILを表示（LBA省略でread-only） |
| `usbcachefail` | DMA cache同期拒否・古い世代の完了・短いOUT・FIFO flush timeoutの4つを注入し、いずれも吸収されず失敗になることを確認（接続構成に依存せず全体で1回。read-onlyで媒体には書かない。実行後は`usbrescan`） |
| `usbmargin [rounds]` | VBUS offから再投入し、LBA 0が読めるまでの各段階を計測（read-only、既定5回、最大20回） |
| `devices` | ブロックデバイス（`ram`／`sd0`／`usb0`…）の容量と、LBA 0の判定結果（MBRの各entry、superfloppy、ambiguous、判定不能）を表示。USBは接続中の台数ぶん列挙し、接続位置とVID:PIDも出す |
| `blkread <dev> [pN] <lba>` | ブロック層経由で1ブロック読み出してUARTへダンプ。`pN`を付けるとLBAはそのパーティション相対になり、末尾を越える指定は媒体へ届く前に拒否される |

`sdmbr`／`usbmbr`は読み終えたセクタをそのまま整形する古い表示のままです
（`src/app/mbr.rs`）。`devices`との違いは解析の場所で、`devices`は
`fs::mbr`がそもそもMBRかどうかを判定してからentryを出します。

SD関連は起動シーケンスに含まれず、コマンド実行時にのみ`SDMMC: `接頭辞で
UARTへログを出します（[`DIAGNOSTICS.md`](DIAGNOSTICS.md)）。

この層の上のVFS、FAT読み出し、マウント規則は[FILESYSTEM.md](FILESYSTEM.md)に
あります。計画では本文書へ追記する想定でしたが、ブロックI/Oとファイルシステムを
1つの文書に混ぜると両方が読みにくくなるため分けました。

## 未実装

- exFATの解析（[FILESYSTEM_PLAN.md](plans/archive/FILESYSTEM_PLAN.md)のStage 5）
- GPTの解析（MBRのみ。保護MBRは種別`0xEE`として表示されるだけ）
- SDのUHS-Iモード（SDR50/SDR104等、100 MHz以上）
