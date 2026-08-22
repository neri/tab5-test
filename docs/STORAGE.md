# ストレージ（SDカードとUSBマスストレージ）

> 索引: [`../DESIGN.md`](../DESIGN.md) ／ 段階分けと実機で踏んだ罠:
> [`SD_CARD_PLAN.md`](SD_CARD_PLAN.md)、[`USB_MSC_PLAN.md`](USB_MSC_PLAN.md)

ブロック単位の読み書きまでを実装しており、ファイルシステムは扱いません。
SDカードとUSBメモリはLBA 0を読んだ後の扱いだけを共有します
（`src/app/mbr.rs`、`sdmbr`／`usbmbr`）。この共有部分はセクタが
どちらから来たかを知りません。

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

CPU/APBによる`SDHOST_BUFFIFO_REG`の直接読み出しと`IDSTS`のRIビットには実機固有の
制約があり、いずれも使用していません。詳細は
[`KNOWN_ISSUES.md`](KNOWN_ISSUES.md)を参照してください。

## USB Mass Storage（`src/usb/msc.rs`）

Bulk-Only Transport（BOT）でSCSIコマンドを送ります。実機確認済みなのは
INQUIRY、TEST UNIT READY、READ CAPACITY(10)、READ(10)です。Bulk転送は
コントロール転送ではないため`protocol.rs`を通らず、`hcd.rs`のパケット
プリミティブを直接使い、エンドポイントごとのデータトグルを自分で管理します。

Bulk QTDはCPU周波数から約1秒で一度区切り、同じBOT phaseのまま最大4回再投入するため、
合計待ち時間は約5秒です。BOT Reset Recoveryでも使うcontrol packetは約1秒です。CBW送信後に
Bulk転送がtimeout／transaction error、またはCSW不正になった場合は、BOT Reset
Recovery（Mass Storage Reset class request、Bulk IN／OUTそれぞれの
`CLEAR_FEATURE(ENDPOINT_HALT)`、host toggleのDATA0復帰）を実行します。
読み出し専用のREAD(10)だけはRecovery後に1回再送します。将来WRITE(10)を実装しても、
書き込み済みか不明なcommandを自動再送しないよう、再送処理はBOT共通層ではなく
`UsbMassStorage::read_blocks`に限定しています。

descriptor DMAのQTD status 1はESP-IDF 5.5.3と同じくpacket error
（CRC、transaction timeout、stuff、false EOP、excessive NAK）として扱います。
1 packetだけのQTDではendpoint toggleを進めず、同一packet／同一DATA PIDを50 ms間隔で
最大20回まで再送します。ACKを失ったOUTでもdevice側は同じPIDのduplicateを再消費しないため
安全です。timeoutまたは複数packetのBulk IN QTD errorではdescriptorの残量から正常受信済みの完全MPS packet数を
求め、そのbyteを保持し、DATA PIDをpacket数だけ進めて未受信suffixだけを再投入します。進捗が
MPS境界でない場合は安全に再開できないためBOT Reset Recoveryへ昇格します。

Bulk INのデータフェーズは、4 KiB READ(10)をendpoint MPSごとのQTDへ分割せず、descriptor
DMAが1 QTD内でpacketへ分割します。High-Speed MPS 512 byteの実機では`ut`既定100回で
約1,000回だったchannel 0の起動を約200回へ減らします。QTDのIN byte数はMPSの倍数にする必要があるため、
13 byte CSW、36 byte INQUIRY、8 byte READ CAPACITYはMPSサイズの内蔵SRAM stagingへ受け、
実受信byteだけを呼び出し側へコピーします。

USBハブのポートに挿したUSBメモリも同じレジストリに乗ります
（[`USB_REFACTOR_PLAN.md`](USB_REFACTOR_PLAN.md) Stage F）。`usbmsc`／
`usbread`／`usbmbr`はいずれもレジストリを引くので、直結とハブ経由を
区別しません（[`USB.md`](USB.md)）。

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
| `usbmbr` | LBA 0のMBRを`sdmbr`と同じ書式で表示 |
| `ut [count]` | 同じ4 KiBを反復read・比較するread-only試験（既定100回、Recovery再送数も表示） |

SD関連は起動シーケンスに含まれず、コマンド実行時にのみ`SDMMC: `接頭辞で
UARTへログを出します（[`DIAGNOSTICS.md`](DIAGNOSTICS.md)）。

## 未実装

- FAT/exFATファイルシステムの解析（[`SD_CARD_PLAN.md`](SD_CARD_PLAN.md)の
  Stage 4、保留）
- GPTの解析（MBRのみ。保護MBRは種別`0xEE`として表示されるだけ）
- USB MSCの書き込み（WRITE(10)）
- SDのUHS-Iモード（SDR50/SDR104等、100 MHz以上）
