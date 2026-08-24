# ストレージ（SDカードとUSBマスストレージ）

> 索引: [`../DESIGN.md`](../DESIGN.md) ／ 段階分けと実機で踏んだ罠:
> [`SD_CARD_PLAN.md`](SD_CARD_PLAN.md)、[`USB_MSC_PLAN.md`](USB_MSC_PLAN.md)、
> [`USB_WRITE_STABILITY_PLAN.md`](USB_WRITE_STABILITY_PLAN.md)、
> [`USB_MSC_BOOT_MARGIN_PLAN.md`](USB_MSC_BOOT_MARGIN_PLAN.md)

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
sessionが死ぬ間欠故障がありましたが、予防的BOT再同期とMSC session隔離後、High-Speed直結と
FS-onlyハブ＋HID併用でREAD／WRITE試験を完走しています。High-Speedハブ上のLow-Speed HIDとの
併用READ回帰も完走しています。根本原因は未特定です。
確定した事実・否定した仮説・入れた緩和策は
[`USB_WRITE_STABILITY_PLAN.md`](USB_WRITE_STABILITY_PLAN.md)にまとめてあります。
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
`CLEAR_FEATURE(ENDPOINT_HALT)`、host toggleのDATA0復帰）を実行します。
読み出し専用のREAD(10)だけはRecovery後に1回再送します。**WRITE(10)は自動再送しません。**
途中で失敗した書き込みはメディアへ一部だけ届いている可能性があり、再送すると不確かな
ブロックが増えるだけだからです。この方針を保つため、再送処理はBOT共通層ではなく
`UsbMassStorage::read_blocks`に限定してあります。

反復READ(10)では、成功16回ごとにcommand間で予防的BOT再同期を行います。Mass Storage
Resetと両Bulk endpointのhalt解除でhost／deviceのDATA toggleをDATA0へ揃える処理で、root
portはresetしません。FS-onlyでは33〜40回、High-Speed直結でも52回後にBulkとEP0が無応答に
なった実測に対し、EP0が応答している間にBOT境界を再確立する緩和策です。実行回数は`ut`の
`proactive_resyncs`に表示します。High-Speed直結の`ut 100`は予防再同期6回、retry 0で
100/100を完走しています。FS-onlyハブ＋HID併用でも同条件で100/100を完走し、試験後も
HIDは動作しました。High-Speedハブ＋Low-Speed HID＋High-Speed MSCの第24版最終回帰も
100/100、failure／mismatch 0、予防再同期6回、packet／command retry 0でPASSしています。

WRITE(10)は失敗後に安全な自動再送ができないため、各WRITEの直前にもMass Storage Resetと
両Bulk endpointのhalt解除を行います。蓄積したtransport状態を持ち込まず、DATA0へ揃えた
BOT command境界から開始するための予防再同期であり、WRITE失敗時に再送するものではありません。
High-Speed直結の`usbwritetest 2`は10/10回、pattern照合、原本復元、周辺LBA照合がすべて
成功しています。pattern書き込みと復元を合わせて計20回のWRITEを連続成功しました。
FS-onlyハブ＋HID併用でも10/10回成功しました。給電中のHID事前接続ハブも上流再接続を
5/5回認識し、初回列挙修正を含む実機受入を完了しています。

`usbwritetest <lba>`は`sdwritetest`より検査が厚くなっています。対象LBAだけでなく
**前1・後2ブロックの窓**を事前に読んで保持し、パターン書き込み後に窓全体を読み直します。
対象以外が変化していれば`COLLATERAL DAMAGE`として表示し、保持した内容でそのブロックも
書き戻します。**対象LBAに書いて対象LBAから読み戻すだけでは、デバイスが別の場所へ書いても
必ず一致してしまう**ためです（実機で実際にデータ破損が起きました。
[`USB_MSC_PLAN.md`](USB_MSC_PLAN.md)の追補）。

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

descriptor DMAのQTD status 1はESP-IDF 5.5.3と同じくpacket error
（CRC、transaction timeout、stuff、false EOP、excessive NAK）として扱います。
すべてのBulk QTDをendpoint MPS以下の1 packetに限定し、endpoint toggleを進めず
同一packet／同一DATA PIDを50 ms間隔で最大20回まで再送します。ACKを失ったOUTでもdevice側は
同じPIDのduplicateを再消費しないため安全です。4 KiB READ(10)もMPS単位へ分割し、各QTDの
完了後にsoftwareがDATA PIDを1回進めます。複数packet QTDのdescriptor残量から進捗を推定する
経路は使用しません。QTDのIN byte数はMPSの倍数にする必要があるため、
13 byte CSW、36 byte INQUIRY、8 byte READ CAPACITYはMPSサイズの内蔵SRAM stagingへ受け、
実受信byteだけを呼び出し側へコピーします。

起動時のスキャンは、USB MSCが見つかった場合にTEST UNIT READY／READ
CAPACITY(10)／READ(10) LBA 0までを実行し（ready待ちの上限は4,000 ms）、
各段階の所要時間をUARTへ出します。not readyのときはREQUEST SENSEで理由を確認し、
sense key 2かつASC `0x3A`（MEDIUM NOT PRESENT）であれば**待たずに即座に諦めます**。
空のカードリーダーは挿さっている限りこの答えを返し続けるためで、実測では上限の
10秒を使い切っていました。起動中（ASC `0x04`）などその他の理由は上限まで待ちます
（`USB BOOT:`、[`DIAGNOSTICS.md`](DIAGNOSTICS.md)）。将来ファイルシステム層が
起動時にUSB MSCを最優先で選ぶために必要な待ち時間を実測で決めるための計測で、
`usbmargin`はそれをVBUSの再投入で繰り返します。計測の目的と確定条件は
[`USB_MSC_BOOT_MARGIN_PLAN.md`](USB_MSC_BOOT_MARGIN_PLAN.md)を参照してください。

USBハブのポートに挿したUSBメモリも同じレジストリに乗ります
（[`USB_REFACTOR_PLAN.md`](USB_REFACTOR_PLAN.md) Stage F）。`usbmsc`／
`usbread`／`usbmbr`はいずれもレジストリを引くので、直結とハブ経由を
区別しません（[`USB.md`](USB.md)）。

## ブロックデバイス層（`src/fs/`）

[FILESYSTEM_PLAN.md](FILESYSTEM_PLAN.md)のStage 1にあたる層です。SDカード、
USB Mass Storage、PSRAM上のRAMディスクを、論理ブロック単位の共通interface
`fs::block::BlockDevice`（`geometry`／`read_blocks`／`write_blocks`／`flush`）
で扱えるようにします。ファイルシステムそのものはまだありません。

読み取り専用と読み書きでtraitを分けていません。書き込んで良いかはVFSの
mount policyが決めることで、転送層の性質ではないためです。物理媒体（SD・USB）の
adapterは`write_blocks`をコマンド発行前に`WriteSuppressed`で失敗させ、LBAと
ブロック数をUARTへ記録します。成功を偽装すると、上位層がメタデータ更新は
届いたものとして続きを組み立ててしまいます。既存の`sdwritetest`／`usbzero`
などのraw書き込みコマンドは`sdmmc.rs`／`usb/msc.rs`を直接呼ぶので、この方針の
対象外のまま残ります。

LBAと容量は`u64`、論理ブロック長は`BlockGeometry`が持ちます。ただしMBRと
ファイルシステムの経路が受理する論理ブロック長は**512 byteだけ**で、それ以外は
`UnsupportedBlockSize`として解析前に拒否します。

- `src/fs/ramdisk.rs`: PSRAMの固定8 MiB領域（[PSRAM.md](PSRAM.md)）。唯一の
  読み書き可能な媒体で、起動ごとにFAT16でformatし直します。内容はリセットで
  消え、`flush()`は永続化を保証しません
- `src/fs/sd.rs`・`src/fs/usb_msc.rs`: 既存ドライバの上に載る薄いadapterです。
  IDMACのディスクリプタ制約、BOT recovery、予防的再同期といった媒体固有の処理は
  下層に残し、ここは結果の変換と転送分割だけを行います。SDはCSD version 1.0の
  カード（容量を復号していない）をこの層では扱いません
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
  総セクタ数フィールドをそれぞれ規格が許す値かで判定します（`src/fs/bootsector.rs`）
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
| `usbzero <lba> [count]` | 1〜8ブロックをゼロで上書きし、媒体から読み直して照合（破壊的） |
| `usbmbr` | LBA 0のMBRを`sdmbr`と同じ書式で表示 |
| `ut [count]` | 同じ4 KiBを反復read・比較するread-only試験（既定100回、Recovery再送数・予防再同期数も表示） |
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

- exFATの解析（[FILESYSTEM_PLAN.md](FILESYSTEM_PLAN.md)のStage 5）
- SD／USBへのファイルシステム経由の書き込み。全Stageで読み取り専用です
- GPTの解析（MBRのみ。保護MBRは種別`0xEE`として表示されるだけ）
- SDのUHS-Iモード（SDR50/SDR104等、100 MHz以上）
