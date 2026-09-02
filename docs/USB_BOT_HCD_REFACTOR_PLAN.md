# USB BOT／HCD正常境界リファクタリング計画

> 索引: [`../DESIGN.md`](../DESIGN.md) ／ 現在のUSB仕様: [`USB.md`](USB.md) ／
> 現在のストレージ仕様: [`STORAGE.md`](STORAGE.md)
>
> この文書は作業計画と実機での判断記録です。現在の実装仕様は現状文書とコードを
> 優先します。既存の故障履歴は[`USB_WRITE_STABILITY_PLAN.md`](USB_WRITE_STABILITY_PLAN.md)を
> 参照してください。

## 状態: Stage 3完了、Stage 4着手前で中断

### 中断時点の要約（再開する人へ）

Stage 0〜2は完了し、実機で確認済み。Stage 3のdescriptor-DMA内の局所cleanup A/Bは
v25〜v29ですべてNo-Goになった。v30／v31のdirect buffer DMAも最初のCBWから失敗したため
撤回した。v32はdescriptor DMAへ戻して通常試験を回復したが、2-slot QTDでもB1 raw WRITEは
3/32で止まった。v33はzero-progressという不安定なdescriptor残量を発火条件にしたため実機では
一度も動かなかった。v34は反復errorでmode再始動を確実に発火させたがB1 rawは3/32のままだった。
v35で250 ms／1000 msのcommand間隔を与えてもB1は改善しなかった。公式実装がBulk data
transfer全体を1 QTDへ載せるのに対し、この実装が64 byteごとにchannelをhalt／rearmする差を、
WRITE dataだけ512 byte QTDへまとめたv36はB2をwrite 1/10へ悪化させ、複数packet QTDだけ
MC/EC=1にしたv37も1/10で改善しなかった。1 packet QTDの契約を保った8-entry listを
channel 1回で実行するv38では、B2の全WRITEが後続QTDへ進む前のQTD 0でstatus 1になった。
v39の安全なprefix再開でWRITE 10/10まで回復したが、復元は9/10でCSW timeoutが残った。
複数packet／list案をすべて撤回したv40はB2でREAD 100/100、WRITE／復元10/10へ戻ったが、
実レジスタは`512/1024/1024`でFIFO設定がroot-port resetに消されていた。
v41はESP-IDFと同じくreset成功後にFIFO設定を再適用し、B2 `usbcheck`で実レジスタ
`512/256/128`、packet error 0、READ／WRITE／復元の全PASSを確認した。
同じB2のREADを挟まないraw WRITE burstも32/32、pattern一致、復元成功でPASSした。
同じv41のB1も`usbcheck`とraw WRITEが全PASSし、従来3/32で止まったrawが32/32まで通った。
Full-Speed固定ハブ経路の旧故障は両媒体で解消した。v41の`usbcachefail`は[1/3]を正しく検出したが、
直後の自動rescanが最初のdevice descriptorで失敗し、残り2検査を実行できなかった。
v42は注入間の再列挙をbounded retryし、[1/3]〜[3/3]の全9 gateと手動rescanがPASSした。
故障注入は完了。C1 `usbcheck`もHigh-Speed／MPS 512でREAD・WRITE・復元が全PASSした。
C1 raw WRITEも32/32、pattern一致、復元成功でPASSした。
同じHigh-SpeedハブのC2（Sony）も`usbcheck`が全PASSした。
C2 raw WRITEも32/32、pattern一致、復元成功でPASSした。
A1／A2のHigh-Speed直結も通常試験とraw WRITEがすべてPASSした。
**6構成の実機matrixと故障注入を完了し、Stage 3を完了とする。Stage 4着手前で中断。**

確定したこと:

- 正常転送のDMA bufferはHCDが所有し64 byte整列する。cache同期の拒否は転送失敗になり、
  6構成すべてで拒否0（Stage 1）。
- `actual`は1箇所でだけ導出し、「0 byte」と「不明」を型で区別する。放棄されたpacketは
  1 byteも動いていないと証明できるときだけ再送する。`requested=64 actual=64`のまま
  4回再送する形は消えた（Stage 2）。
- **このcoreはpacket error時に不可能な残量を書き戻す**（64 byte要求へ100,489など）。
  0にできないので、byte数として使わないことだけを保証する。
- v29のB1 `usbrawcheck`は4本目でzero-progress packet errorが20回連続し、再送成功後cleanupへ
  一度も到達せず失敗した。snapshot復元は成功し、filesystem repairなしで再現できた。B2は32本と
  復元がPASSした。問題は「再送成功後の次packet」だけでなく、descriptor-DMA QTD自身が同じ
  packetを恒久的に処理できなくなるところまで狭まった。
- v32ではQTD addressが`0x4FF51400`／`0x4FF51600`へ実際に交互切替しても、B1 raw WRITEは
  同じ3/32で停止した。物理descriptorの即時再利用が根本原因という仮説は否定した。

Stage 3の最終結果:

- Full-Speed固定ハブ経路の恒久的なdata OUT status 1は、v41のbalanced FIFO再適用後に
  B1／B2とも通常WRITE 10/10、raw WRITE 32/32となり再現しなくなった。故障時に
  `CLEAR_FEATURE(ENDPOINT_HALT)`も失敗していた問題を含め、旧故障が消えたことを残りのmatrixで
  回帰確認した。
- topology非依存の`usbcachefail`はv42で全9 gateがPASSした。各注入間の自動rescanと最後の
  手動rescanも成功し、MSC／keyboard／mouseが復帰した。
- A1／A2／B1／B2／C1／C2の全構成で`usbcheck`と`usbrawcheck`がPASSした。Stage 3の
  実機残件はない。

v41のB1／B2は`usbcheck 100 <犠牲LBA>`と`usbrawcheck <犠牲LBA> 32 1`がすべてPASSし、
v42の`usbcachefail`も完了した。C1の`usbcheck 100 1`もPASSしたため、次は
`usbrawcheck 1 32 1`を実行し、これもPASSした。C2の`usbcheck 100 1`もPASSしたため、
同じC2で`usbrawcheck 1 32 1`を実行し、これもPASSした。A1／A2も同じ2 commandがPASSした。
Stage 3を完了し、Stage 4着手前で中断する。

**Stage 3のtransport受入には`fswritetest`を実行しない。**失敗したfilesystem metadataが
残ると別マシンでrepairするまで次回試験ができず、transportの1回のA/Bに不要な復旧負担を生む。
以下に残る`fswritetest`結果は過去の判断記録であり、現在の実機依頼ではない。新しい
`usbrawcheck`はfile／directoryを作らず、filesystem外と明示した犠牲LBAへREADを挟まない
単一block WRITE列を発行する。復元不能でも同じ犠牲範囲を`usbrescan`後に再利用できる。
filesystem層そのものの受入はtransportのStage 3と分け、必要な段階で再開する。

正常なBOT commandの境界でhost channel／FIFOを予防cleanupする現状を、転送完了条件、
DMA／cache契約、BOTの実転送長とCSW検証を正したうえで段階的に撤去する。

現在の予防処理は、連続READ(10)では成功16回ごと、WRITE(10)では各command直前に
`BulkOnlyTransport::maintain_command_boundary`から
`hcd::recover_channel_after_packet_failure`を呼ぶ。これはUSB Mass Storage Bulk-Only
Transportの通常command境界に必要な処理ではない。実機で古いCSWが次commandに見える故障を
抑えるための緩和策であり、根本原因を直した証拠ではない。

この計画ではcleanupを先に外さない。次の順で、正常転送がそれ自身の状態を完全に回収する
契約を作ってから、READとWRITEを別々のGo/No-Go判定で外す。

1. 既存の失敗を数値とraw状態で再現できるようにする。
2. DMA bufferの64 byte cache-line契約をHCD境界で強制する。
3. HCDが要求長、実転送長、QTD所有権、channel完了を一度だけ確定する。
4. BOTがCBW、data、CSWの長さ、tag、status、residueを整合させる。
5. 実失敗後のRecoveryと正常command前の予防処理を別APIへ分ける。
6. READ前cleanup、次にWRITE前cleanupを実機A/Bで撤去する。

## 背景

### 現在の実機事実

[`USB_WRITE_STABILITY_PLAN.md`](USB_WRITE_STABILITY_PLAN.md)で確定している事実は次のとおり。

- 予防処理なしの反復READ(10)は、Full-Speed構成で33〜40回、High-Speed直結で52回後に
  BulkとEP0が無応答になった。
- 成功16 READごとの予防処理を入れると、High-Speed直結、Full-Speedハブ＋HID、
  High-Speedハブ＋Low-Speed HID＋High-Speed MSCで`ut 100`を完走した。
- WRITE前cleanupを外すA/Bでは、expected tag `N`に対して直前commandの正常なCSW
  （tag `N-1`、residue 0、PASSED）が見え、2回目のWRITEで停止した。
- 正常境界でdevice-facing Mass Storage Reset／CLEAR_FEATUREを繰り返す必要はなく、
  host channel／FIFO cleanupだけで従来の受入試験は通った。
- 複数ブロックWRITE(10)は決定論的に失敗するため、現在は1 command 1 blockへ制限している。

これらは「cleanupに緩和効果がある」証拠であって、「cleanup不足が根本原因」という証拠では
ない。正常終了したcommandのCSWが次commandで再び見えるなら、前commandの完了公開、DMA buffer、
QTD再利用、実転送長、short packet、cache invalidateのいずれかが境界を越えている。

### 静的監査で残っている問題

計画策定時点のコードには、予防cleanupの必要性を正しく評価する前に直すべき問題がある。

| 層 | 現状 | 起こり得ること |
| --- | --- | --- |
| cache同期 | `hcd.rs`が`psram::writeback_invalidate`の`false`を記録するだけで転送を続ける | OUTは古いRAMを送信し、INはDMA前のcacheを読む |
| DMA buffer | BOTのshort IN staging、periodic report、制御／MSC／HIDの任意sliceが64 byte整列を保証しない | cache routineが開始addressを拒否する |
| Bulk OUT | `PacketOutcome::Ok(actual)`の`actual`を無視して要求chunk全体を送った扱いにする | 部分WRITEを成功扱いする |
| CSW | residueを上位へ反映せず、Phase Errorと未定義statusをtransport failureへしない | phase不一致のsessionを再利用する |
| cleanup | FIFO flush timeoutを戻り値で伝えない | 必須cleanup失敗後にもWRITEを開始する |
| 記述子 | descriptor実受信長とendpoint MPSの検証が弱い | softwareの分割長とhardware設定が食い違う |

### 他実装を比較基準にする

Linuxの`usb-storage`は正常command間で定期的なFIFO cleanupをしない。各bulk転送のactual
lengthを保存し、CSWのtag、status、signature、residueを検証し、Phase Errorをtransport
errorとしてRecoveryへ送る。DWC2のFIFO flushはhost core初期化／reset側にあり、READ回数と
結び付いていない。

- Linux BOT: <https://github.com/torvalds/linux/blob/master/drivers/usb/storage/transport.c>
- Linux DWC2 HCD: <https://github.com/torvalds/linux/blob/master/drivers/usb/dwc2/hcd.c>
- Linux usb-storageのDMA alignment: <https://github.com/torvalds/linux/blob/master/drivers/usb/storage/scsiglue.c>
- U-Boot BOT: <https://github.com/u-boot/u-boot/blob/master/common/usb_storage.c>

Linuxのdevice quirkには初回READ(10)再試行、command後の短いdelay、転送sector上限、壊れた
residueやtagの許容があるが、全deviceへ一律に「N回ごとのhost FIFO flush」を行うものはない。
この計画ではLinuxを移植せず、責務の境界だけを比較基準にする。

## 目的と完了像

- 正常なCBW→data→CSWが完了した時点で、そのcommandのQTD、DMA、割り込み、実転送長が
  一度だけ回収され、次commandへ古い状態を持ち越さない。
- DMAに渡るbufferはHCDが整列とcache範囲を保証する。上位の任意`&mut [u8]`へ暗黙の
  alignment契約を課さない。
- cache maintenance拒否、部分OUT、CSW不整合、FIFO flush timeoutを成功へ丸めない。
- BOT Reset Recoveryはtransport error、Phase Error、無効CSWなどの実失敗後だけに行う。
- READ(10)だけはRecovery成功後に1回再送できる。WRITE(10)は引き続き自動再送しない。
- 正常READ／WRITE前の`maintain_command_boundary`と`proactive host cleanups`ログを削除する。
- cleanup撤去後も既存の直結、ハブ、HID併用、ファイルシステム試験を同等以上の条件で通す。

## 守る不変条件

| 境界 | 不変条件 |
| --- | --- |
| CPU→DMA | hardwareが読む全byteを転送前にwritebackし、同期拒否ならchannelをarmしない |
| DMA→CPU | hardware所有権が解除された後にinvalidateし、同期拒否ならpayloadを公開しない |
| buffer配置 | cache maintenance開始addressは64 byte境界。QTD／frame list固有のより強いalignmentも維持する |
| channel投入 | 前世代のHCINT、QTD、完了tokenを消費してから新しい世代をarmする |
| HCD成功 | `HCINT.XferCompl`、`QTD.Active == 0`、妥当な残量を同じ世代で確認する |
| OUT成功 | `actual == requested`。部分OUTをPIDだけ進めて成功扱いしない |
| IN成功 | `actual <= requested`。short INは実長としてBOTへ渡し、勝手に要求長へ増やさない |
| CBW | 正確に31 byteをBulk OUTし、部分転送はtransport errorにする |
| CSW | 正確な13 byte、期待tag、妥当なsignature、status 0〜2、data residue整合を確認する |
| WRITE成功 | data OUT全量、CSW PASSED、書き込みに矛盾するresidue無しを満たす |
| Recovery | 実失敗後だけ。正常commandの回数を条件にしない |

## 対象範囲

- `src/usb/hcd.rs`のchannel 0 packet転送、DMA/cache同期、完了回収、FIFO flush。
- persistent periodic HID bufferのDMA alignment。scheduler全体は変更しない。
- `src/usb/bot.rs`のCBW、data IN/OUT、CSW、Reset Recovery。
- `src/usb/msc.rs`のREAD再送方針、WRITE非再送方針、予防cleanup count。
- `src/fs/usb_msc.rs`のread/write stagingと1 block WRITE制限の再評価。
- USB診断counterと、再現可能な実機A/B手順。

対象外:

- UAS、複数LUN、isochronous転送、多段ハブ。
- USBを非同期queue型APIへ全面変更すること。
- Split Transaction schedulerやHID report parserの機能追加。
- WRITE失敗後の自動再送、電断原子性、ファイルシステムjournal。
- cleanup撤去と同時に複数ブロックWRITEを既定へ戻すこと。

## 設計方針

### 上位sliceを直接DMA契約にしない

`run_packet(&mut [u8])`の呼び出し側すべてに64 byte alignmentを要求しても、制御転送の
8 byte packetやFull-Speed MPS 64未満のoffsetで部分sliceを作れば次packetで崩れる。
alignmentを型コメントだけで伝播させず、HCDが固定長のcache-aligned packet stagingを所有する。

- OUTは上位sliceからstagingへ要求packet分だけcopyし、cache同期成功後にDMAへ渡す。
- INはstagingをDMAへ渡し、完了とcache同期成功後に実受信byteだけ上位sliceへcopyする。
- CBW 31 byte、CSW 13 byte、control setup/data、HID report、MSC payloadを同じ入口へ通す。
- High-Speed bulk MPS 512までを1 packet stagingで扱う。より大きいMPSはdescriptor拒否とする。
- persistent periodic DMAは専用bufferを64 byte alignmentへ上げ、通常packet stagingとは共有しない。

copyを避けるzero-copy最適化は、alignment、cache span、lifetimeを型で証明できる場合だけ後から
追加する。本計画の完了条件にはしない。

### HCDは要求長と実転送長を分ける

HCDの完了値を単なる`Ok(usize)`から、少なくとも要求長、実転送長、HCINT、QTD最終word、
short packet有無を一つのsnapshotとして扱える形へする。上位へ返す公開値は実転送長であり、
再投入のために要求長へ書き換えない。

timeout／transaction error後の同一phase内packet retryは、完了済みbyteを二重送信しないことを
条件にする。転送済みbyteとDATA PIDを確定できないOUTはtransport errorへ上げ、WRITEを
command単位で再送しない。INでsuffixを再投入する場合も、既に公開したprefixを上書きしない。

### BOTはhost実長とdevice residueを突き合わせる

`CommandResult`は`transferred`と`status`だけでなく、CSW residueと期待data長を保持する。
data phaseとCSWが示す値が矛盾した場合はcommand failureではなくtransport errorとして
Reset Recoveryへ送る。

短いdata INに13 byteの`USBS`が先着した場合は、Linuxと同様に「deviceがdata phaseを省略して
CSWを返した」形として識別する。通常payloadへCSWを混ぜず、同じCSWをもう一度待ってtimeout
しない。signatureだけで決めず、長さ、tag、status、residueをまとめて検証する。

### 正常境界と失敗回復を別APIにする

現在の`recover_channel_after_packet_failure`を正常command前から呼ぶ構造を解消する。

- `finish_packet`は正常完了した1 packetの所有権と状態を閉じる。
- `recover_failed_packet`はtimeout、transaction error、halt異常後にchannelを止めて掃除する。
- `reset_bot_transport`はMass Storage Reset、Bulk IN/OUT halt clear、DATA0同期を行う。
- FIFO flushは`Result`を返し、timeoutをlogだけで成功扱いしない。

Stage 4までは比較のため予防cleanupの呼び出しを残してよいが、失敗回復APIを流用せず、
一時的な`proactive_cleanup`として明示する。最終StageではこのAPI自体を削除する。

## 段階的な実装・検証

### Stage 0: baselineと観測契約

**進捗: 完了。観測契約を実装し、6構成の実機baselineを採取した。**

このStageでは転送挙動を変えず、現行cleanupを維持したまま比較可能なbaselineを採る。

- cache同期拒否をphase、direction、address、要求長と一緒に数える。
- packetごとにrequested／actual、HCINT、QTD final、retry済みbyteを失敗時だけ記録する。
- CSW不一致時にsignature、tag、residue、statusと先頭13 byteを記録する。
- FIFO flush timeoutを方向別に数える。まだ戻り値へ反映しないが、発生を隠さない。
- proactive READ／WRITE cleanup、packet retry、command retry、BOT Reset Recoveryを別counterにする。
- 受入試験前後の`usbhw`値を同じ形式で採取する。

実機baseline（構成ごとに2コマンド）:

```
usbcheck 100 <犠牲にできるLBA>
fswritetest /vol/usb0pN 1 1
```

`usbcheck`はcounterを前後で自分で採り、read soakとwrite 10回を実行し、差分と
Go条件ごとのPASS/FAILを出す。接続はA（High-Speed直結）、B（Full-Speed固定ハブ＋HID）、
C（High-Speedハブ＋Low-Speed HID＋High-Speed MSC）の3通りを、最低2メーカーのMSCで。
媒体のVID/PID、接続経路、speed、Bulk MPSはこの文書のStage 0結果へ追記する。

Stage 0のbaselineは`usbcheck`を作る前に採ったので、`usbhw`を2回採って手で突き合わせた
記録になっている。以降のStageは`usbcheck`の差分行をそのまま貼る。

完了条件は、後続Stageの同じ試験とcounterを機械的に比較できることである。baselineが失敗しても
成功したように取り繕わず、どこで止まったかを記録して次Stageの入力にする。

#### 実装したもの

転送の挙動は変えていない。増えたのはcounterと、失敗時のログ内容だけである。

- `hcd`が転送phaseのラベル（control／CBW／data IN／data OUT／CSW／interrupt IN）を持ち、
  転送を所有する層が公開する。HCDは解釈も分岐もしない。cache拒否とpacket失敗はこの
  ラベルと一緒に記録する。
- `cache_writeback_invalidate`がDMA共有オブジェクト（channel 0のQTD／payload、split
  staging、常設periodic、frame list、periodic probe）と方向（out／in／descriptor）を
  受け取り、拒否をsite別・方向別・phase別に数える。直近の拒否のaddressと長さも残す。
  **戻り値の扱いは変えていない**。拒否を転送失敗にするのはStage 1である。
- packet失敗を原因別に数え、直近の失敗のrequested／actual／`HCINT`／QTD最終control wordを
  残す。同じ4つを失敗時のUARTログにも出す。従来のログは「失敗した」とHCINTしか
  言わず、要求byteのうち何byteが既に動いたのかを言わなかった。
- TX（非periodic）／TX（periodic）／RXのFIFO flush timeoutを方向別に数える。periodic
  channelがarm中でflushを省略した回数も別に数える。**戻り値へは反映していない**（Stage 4）。
- BOT層のretryをpacket error別／timeout別に分け、さらに「既にbyteが転送済みだった
  packetのretry」の回数と累計byte数を別に数える。
- BOT Reset Recoveryの実行回数と失敗回数を、予防cleanupの回数とは別に数える。
- CSWが期待と合わなかった場合（13 byte未満／signature不一致／tag不一致）を種類別に数え、
  受信byte数・期待tag・signature・tag・residue・statusをUARTへ出す。13 byteに満たない
  部分は0埋めするので、実際に届いたbyteだけが読める。
- `usbhw`がこれらを固定書式で表示し、同じ行をUARTへも出す。0の項目も必ず出す。
- `CommandStatus`から未使用の`residue`を外した。上位が期待data長と突き合わせないまま
  residueだけ持つのは、decodeしても比較しないfieldを増やすだけである。Stage 3で
  比較と一緒に`CommandResult`へ入れる。

#### 静的監査で分かったこと

実機を回す前に、release ELFの配置検査（`tools/check_elf_layout.py`）で次を確認した。

| DMA object | address | alignment |
| --- | --- | --- |
| periodic frame list | `0x4FF51800` | 512 byte境界 ✓ |
| periodic QTD bank | `0x4FF50C00` | 512 byte境界 ✓ |
| periodic report buffer | `0x4FF515D8` | **64 byte境界ではない**（+0x18） |

periodic HIDのreport bufferは`#[repr(C, align(4))]`のままで、cache maintenanceの開始
addressが64 byte境界にならない。Stage 1がこれを64 byte alignmentへ上げる対象である。
現状ではこのbufferに対するcache拒否は`usbhw`の`refusal site`の`periodic`として数えられる。

#### Stage 0結果（実機）

2媒体×3接続の6構成で採取した。番号内で連続実行した試験以外は、試験や媒体を変えるたびに
再起動している場合があるため、controller全体のcounterは構成をまたいで累積していない。

| ID | 接続 | 媒体 | speed／Bulk MPS | `ut 100` | `usbwritetest 1`×10 | `fswritetest /vol/usb0p1 1 1` |
| --- | --- | --- | --- | --- | --- | --- |
| A1 | HS直結 | メーカー不明 | High-Speed／512 | PASS 100/100、packet retry 0、proactive 6 | 10/10 match、復元10/10、collateral 0 | PASS 12検査 12,926 ms |
| A2 | HS直結 | Sony | High-Speed／512 | PASS 100/100、packet retry 0、proactive 6 | 10/10 match、復元10/10、collateral 0 | PASS 12検査 12,649 ms |
| B1 | FS固定ハブ＋HID | メーカー不明 | Full-Speed／64 | PASS 100/100、packet retry 0、proactive 6 | 10/10 match、**復元2/10**、collateral 0 | **FAIL 検査1** |
| B2 | FS固定ハブ＋HID | Sony | Full-Speed／64 | PASS 100/100、**packet retry 1**、proactive 6 | 10/10 match、復元10/10、collateral 0 | **FAIL 検査1** |
| C1 | HSハブ＋HID | メーカー不明 | High-Speed／512 | PASS 100/100、packet retry 0、proactive 6 | 未実施 | PASS 12検査 13,259 ms |
| C2 | HSハブ＋HID | Sony | High-Speed／512 | PASS 100/100、packet retry 0、proactive 6 | 11/11 match、復元11/11、collateral 0 | PASS 12検査 12,208 ms |

全構成で共通していた値:

- **cache同期拒否は全構成0**。site別・方向別・phase別のいずれも0。後述のとおり
  periodic report bufferは64 byte境界に無いが、ROM routineはこの構成を拒否しなかった。
  拒否が0であることは契約が満たされている証拠ではない——Stage 1は実測ではなく契約として
  整列を保証する。
- **FIFO flush timeoutは全構成0**（TX非periodic／TX periodic／RXとも）。periodic arm中の
  flush省略も0。
- CSW異常（13 byte未満／signature不一致／tag不一致）は**全構成0**。今回のbaselineでは
  「正常なCSWが次commandで再び見える」故障は再現しなかった。
- 予防cleanupは`ut 100`につきREAD前6回。WRITE前は`usbwritetest`／`fswritetest`の
  WRITE数に等しい。

#### 分かったこと1: 観測契約側の欠陥（修正済み）

baselineを採る過程で、Stage 0の実装自体に3つの欠陥が出た。いずれもこの記録の後に直した。

- **idle Interrupt INのpoll expiryをpacket失敗として数えていた。** `SET_IDLE(0)`のHIDは
  キーが動くまでNAKし続けるので、これは正常なbusの姿である。B1では`ut 100`の間だけで
  661件、C1では728件が`pkt-fail`に入り、A1の本物の失敗2件を埋めていた。`run_packet`の
  `CompletionWait::PollIdleNak`と、periodic splitのframe境界expiryを、失敗ではない
  別counter（`idle-poll`）へ分けた。
- **`usbhw`の行が80桁で切れていた。** `Line`が80 byteで打ち切るため、phase別内訳の
  最終列と、失敗kind別内訳の後半5列が読めなかった。列見出しを短縮し、内訳を
  複数行へ分けて、5桁の値でも収まるようにした。
- **同じ行が2回出ていた。** `Console::write_output_line`が既にUARTへmirrorしているのに、
  重ねてUARTへ書いていた。

このため今回の`pkt-fail`の数値は、次の内訳へ読み替える。

| ID | `ut 100`中の`pkt-fail`増分 | うちidle interrupt poll | 本物の失敗 |
| --- | --- | --- | --- |
| A1 | 0（起動時に2） | 0（HID無し） | 起動時のCSW halt-timeout 2、いずれもretryで回復 |
| A2 | 0 | 0（HID無し） | 0 |
| B1 | 661 | 661 | 0 |
| B2 | 417 | 416 | 1（QTD packet error） |
| C1 | 728 | 728 | 0 |
| C2 | 441 | 441 | 0 |

#### 分かったこと2: OUTの部分転送とtimeout後の再送（Stage 2の対象）

Full-Speed経路でのみ、**転送済みbyteがあるpacketの再送**が実際に起きた。HS直結と
HSハブでは1件も無く、[`USB_WRITE_STABILITY_PLAN.md`](USB_WRITE_STABILITY_PLAN.md)
25kが「DMA済みbyteをtimeout後に再投入する仮説は否定」と結論した根拠は、この経路を
含んでいなかった。

B2の`ut 100`（成功したrun）:

```text
USB BOT: retrying bulk packet after packet error during CBW
USB BOT:   DATA1=0
USB BOT:   bytes already transferred=10
USB BOT:   retry attempt=1
```

31 byteのCBWのうち10 byteが出た状態で、**同じDATA PIDでpacketの先頭から**再送している。
`run_bulk_packet`の「1 MPS以内なら再送は重複packetにしかならず、endpointは二重に
消費しない」という根拠は、packetが完全に出たか全く出なかったかのどちらかである場合にしか
成り立たない。10/31 byteはそのどちらでもない。

B2の`fswritetest`（失敗したrun）:

```text
USB BOT: retrying bulk packet after timeout during data OUT
USB BOT:   DATA1=0
USB BOT:   bytes already transferred=64
USB BOT:   retry attempt=1  ... 2 ... 3 ... 4
USB:   failure=halt-timeout phase=data-OUT
USB:   requested bytes=64
USB:   actual bytes=64
USB:   QTD final=0x00000000
```

64 byteのOUT packetが**要求全量転送済み**と読めるのにchannelがhaltせず、同じpacketを
4回再送してから失敗している。ただし`QTD final=0x00000000`は「残量0でhardware所有解除、
status成功」と「descriptorが書き戻されていない／cacheが古い」を区別できない値である。
`Channel0Transfer::cancel`はこのwordの妥当性を確かめずに`actual = requested - remaining`を
計算するので、**「全量転送済み」と「実転送長不明」を同じ数値にしている**。どちらであっても
再送は安全ではない。

これは守るべき不変条件の2つに正面から違反している。

- OUT成功: `actual == requested`。部分OUTをPIDだけ進めて成功扱いしない。
- HCD成功: `HCINT.XferCompl`、`QTD.Active == 0`、妥当な残量を同じ世代で確認する。

Stage 2の「QTD残量から`actual`を一度だけ計算し、残量underflowとhardware所有中を
transport errorにする」「安全性を証明できないOUT retryは行わない」が、この現象へ直接
対応する。Stage 0はこれを数値として固定した。

#### 分かったこと3: Full-Speedハブ経路のWRITEが2本目で止まる

B1／B2はどちらも`fswritetest`が検査1（scratch directoryの`mkdir`）で失敗した。
A1／A2／C1／C2は同じ試験を通っている。**Full-Speed固定ハブ経路のWRITEだけが落ちる。**

B1では`usbwritetest 1`も10回中8回で復元WRITEが失敗した。pattern WRITEは通り、
FUA read-backも一致し、その直後の**復元WRITEのCSWがtimeout**する。

```text
pattern write+read-back: match
USB BOT: retrying bulk packet after packet error during data OUT   (0 byte)
USB BOT: retrying bulk packet after timeout during CSW             (0 byte)
USB:   failure=halt-timeout phase=CSW  requested bytes=64  actual bytes=0
USB BOT: bulk IN timed out during CSW
USB MSC: WRITE(10) transport failed, not retrying
original data restored: NO -- see UART log, LBA may be corrupted
```

WRITEを再送しない方針どおりの挙動であり、collateral changesは全構成0、範囲外LBAの変化も
無い。犠牲LBAは意図どおり犠牲になっただけである。ただし**同一command列の2本目のWRITEが
落ちる**という形は25jが予防cleanup撤去A/Bで観測したものと同じで、今回はcleanupを
**入れたまま**Full-Speedハブ経路で再現している。予防cleanupはこの経路では緩和になって
いない。

Stage 5／6のGo条件はA〜Cの全構成である。**B1／B2のWRITEは現状でも通らないので、
cleanup撤去の判定材料にはできない。**

**この経路の修復をStage 2／3の完了条件に含める**と決めた。切り分けて既知問題へ逃がす案も
あったが、落ちている中身が「部分OUTを成功扱いしない」「実転送長を確定できないOUTを
再送しない」というStage 2／3が作る契約そのものであり、別の問題ではない。B1／B2の
`fswritetest`が通らないうちはStage 5／6へ進まない。

#### 分かったこと4: periodic report bufferの整列（Stage 1の対象）

release ELFの配置検査（`tools/check_elf_layout.py`）:

| DMA object | address | alignment |
| --- | --- | --- |
| periodic frame list | `0x4FF51800` | 512 byte境界 ✓ |
| periodic QTD bank | `0x4FF50C00` | 512 byte境界 ✓ |
| periodic report buffer | `0x4FF515D8` | **64 byte境界ではない**（+0x18） |

`PeriodicReportBuffer`は`#[repr(C, align(4))]`のままで、cache maintenanceの開始addressが
64 byte境界にならない。実機では拒否されなかったが、`psram::writeback_invalidate`の契約は
「開始addressは64 byte整列。ROM routineは行の途中から始まる範囲を拒否し、切り下げない」で
あり、拒否されなかったことはこの契約を満たした証拠ではない。Stage 1で整列させる。

#### 次Stageへ持ち越す観測

- `ut 100`は6構成すべてPASS。READ経路は現行cleanupの下で安定している。
- `usbhw`の出力書式を直したので、Stage 1の実機確認では`pkt-fail`と`idle-poll`が
  直接分かれて出る。Stage 0のbaselineと比較するときは上の読み替え表を使う。
- 本物の失敗はA1の起動時CSW timeout 2件、B2のCBW部分OUT 1件、B1の復元WRITE 8件、
  B1／B2の`fswritetest` WRITE各1件。すべてFull-Speed経路か起動直後である。

### Stage 1: DMA bufferとcache同期の契約化

**進捗: 完了。6構成で実機確認済み。**

- HCDに64 byte aligned、512 byte以上の固定packet staging型を置く。
- channel 0のcontrol／bulk／fallback HIDをstaging経由へ統一する。
- BOTの`BulkInStaging`、HCDの`PeriodicReportBuffer`／buffer bank、Split stagingを棚卸しし、
  DMAへ直接渡るものを64 byte境界へ置く。
- QTDの512 byte alignmentとperiodic frame listのhardware alignmentは弱めない。
- `cache_writeback_invalidate`を`Result`または`bool`として呼び出し元まで返し、拒否時は
  channelをarmせず`PacketOutcome::CacheSyncFailed`相当へする。
- cache対象範囲は開始addressを下げて他ownerのdirty dataを巻き込まず、staging自身の
  whole cache lineだけに限定する。
- release ELFのsymbol／stack layout検査でDMA bufferのaddressを確認する。

完了条件:

- baseline全構成でcache refusalが0。
- 故意に非整列sliceを上位から渡しても、HCD内部DMA addressは64 byte整列する。
- cache同期を診断用に失敗させたbuildでは転送開始前に失敗し、成功やzero-filled INにならない。
- `cargo check -p tab5-hello-world --release`とrelease ELF配置検査に通る。

このStageではproactive cleanupを外さない。cache修正だけで安定したように見えても、後続のBOT
検証を飛ばさない。

#### 実装したもの

- `PacketStaging`（64 byte整列、512 byte＝High-Speed Bulk MPS）をHCDが1 QTDごとに所有し、
  channel 0のcontrol／bulk／fallback HIDをすべてこれ経由にした。OUTは上位sliceからcopyし、
  INは実受信byteだけ上位sliceへ返す。512 byteを超える要求は転送せず拒否する。
  periodic probeも同じstagingを使う。
- `SplitStaging`を`align(4)`から`align(64)`（1 cache line）へ、`PeriodicReportBuffer`を
  `align(4)`から`align(64)`へ上げた。frame listとQTD bankの512 byte alignmentは維持した。
  `PeriodicBufferBank`はelement型からの継承ではなく自分で`align(64)`を宣言する——element側を
  弱めても気付かない形にしないため。
- `cache_writeback_invalidate`を`#[must_use]`の`bool`にし、20か所すべての呼び出し元へ
  結果を伝えた。開始addressが行境界でないことはROM routine任せにせず自分で検査し、長さは
  行単位へ切り上げる（切り下げない）。
- 拒否を成功へ丸めない経路を作った。`PacketOutcome::CacheSyncFailed`を追加し、
  arm前の拒否ではchannelをarmしない。受信後のinvalidate拒否では上位bufferへ1 byteも
  公開しない。同じbufferは次も拒否されるのでBOTは再送しない。periodic HIDは
  arm拒否・publish拒否ともslotのgenerationを無効化し、HID driverの再列挙へ回す。
  frame listのwriteback拒否ではperiodicを開始せずchannel 0 pollへ落ちる。
- `PacketFailureKind::CacheSyncRefused`（`usbhw`の`cache`列）を追加した。
- cache同期拒否のfault injectionを追加した。正常なhardwareは拒否しないので、注入以外に
  この経路へ到達する方法がない。**自分で減るcounter**であって、恒久的なmodeではない——
  1回の拒否で1消費されるので、armされたまま残る状態は作れない。
- `tools/check_elf_layout.py`の`PERIODIC_HID_BUFFER`要求alignmentを4から64へ上げた。

#### 実機確認コマンドを2つ追加した

Stage 0のbaselineは構成ごとに`usbhw`→`ut 100`→`usbwritetest`×10→`usbhw`と打ち、
2つの10行blockを目視で突き合わせる作業だった。6構成でこれを繰り返すのは、この計画で
一番高くつくうえ、人が間違える場所でもある。

- **`usbcheck [reads] [lba]`**: 1構成の受入試験1回分。counterを前後で自分で採り、
  read soakとwrite 10回を実行し、**差分**とGo条件ごとのPASS/FAILを出す。絶対値には
  起動時の列挙とidle HIDのpollが全部乗っているので、差分でないと比較にならない。
  LBAを省略するとread専用。`fswritetest`はmountされたvolumeが要るので別のままにした。
- **`usbcachefail`**: cache同期拒否を1回注入し、転送がarm前に失敗すること、宛先bufferへ
  1 byteも公開されないことを確認する。宛先は事前に`0x5A`で埋める——deviceのdataでも
  0埋めstagingでもない値なので、残っていれば何も上書きされていない証拠になる。
  ドライバ自身の論理の試験なので接続構成に依存せず、全体で1回でよい。

構成あたり14コマンド＋目視diffが、2コマンド＋PASS/FAILになった。

#### 静的確認の結果

| DMA object | Stage 0時点 | Stage 1後 |
| --- | --- | --- |
| periodic frame list | `0x4FF51800`（512 ✓） | `0x4FF51800`（512 ✓） |
| periodic QTD bank | `0x4FF50C00`（512 ✓） | `0x4FF50C00`（512 ✓） |
| periodic report buffer | `0x4FF515D8`（**64境界でない**） | `0x4FF51600`（64 ✓） |

配置検査に64 byte要求を入れたので、同じ後退は次からbuild時に落ちる（要求を1024へ上げた
検査で`0x4FF51600`が実際にerrorになることを確認済み）。fault injectionの経路は`usbcachefail`で実機確認する。

#### Stage 1実機確認の結果

Stage 0と同じ6構成で`usbcheck 100 1`＋`fswritetest`を実行した。

| ID | 接続 | 媒体 | cache sync | read soak | write rounds | `fswritetest` |
| --- | --- | --- | --- | --- | --- | --- |
| A1 | HS直結 | 不明 | PASS | PASS | 10/10 | PASS 12,920 ms |
| A2 | HS直結 | Sony | PASS | PASS | 10/10 | PASS 12,065 ms |
| B1 | FS固定ハブ＋HID | 不明 | PASS | PASS | 10/10 | **FAIL 検査1** |
| B2 | FS固定ハブ＋HID | Sony | PASS | PASS | 10/10 | **FAIL 検査1** |
| C1 | HSハブ＋HID | 不明 | PASS | PASS | 10/10 | PASS 12,302 ms |
| C2 | HSハブ＋HID | Sony | PASS | PASS | 10/10 | PASS 11,323 ms |

**cache同期拒否は全構成で`+0`、`pkt-fail cache`も`+0`。**FIFO flush timeoutも全構成`+0`、
CSW異常も`+0`。整列を強制した後も転送は成立しており、Stage 1の完了条件を満たす。

Stage 0で復元WRITEが10回中8回失敗したB1が、今回は`writes ok=10/10 restored=10/10`に
なった。**Stage 1の効果とは断定しない**——Stage 0のB1の失敗はCSW timeoutで、cache拒否は
当時も0だった。今回B1で観測されたのは34件のQTD packet error（すべて`bytes already
transferred=0`）で、全部retryで回復している。同じ経路の不安定さが別の形で出ただけの
可能性がある。

B1／B2の`fswritetest`は予定どおりまだ落ちる。Stage 2／3の完了条件である。B2では今回、
**BOT Reset Recovery自体が失敗**した（`CLEAR_FEATURE(ENDPOINT_HALT)`のcontrol IN status
stageがQTD packet errorで2回とも失敗し、`reset recovery failed; this session needs
re-enumeration`）。Stage 0ではrecoveryは完了していたので、これは新しい観測である。
先行するのはStage 0と同じ形——64 byte OUTが`requested=64 actual=64`のままhaltせず4回再送
——だった。

#### 観測契約の欠陥を2つ直した（`usbcachefail`と`usbcheck`）

実機で`usbcachefail`が2ゲート落ちたが、**ログはドライバが仕様どおり動いたことを示している**。
落ちたのは試験の仕様である。

```text
USB:   phase=CBW
USB:   the transfer is failed rather than started
USB BOT: bulk OUT DMA cache sync refused during CBW
USB BOT: reset recovery complete
USB MSC: READ(10) transport failed
USB MSC: retrying READ(10) after BOT recovery      <- 設計どおりの1回再送
```

- 注入が**CBW（OUT packet）**へ当たっていた。commandの最初のcache操作はdata INではなく
  CBWなので、宛先bufferのsentinel検査は何も検証していなかった。
- 1回だけの注入は、`msc.rs`がReset Recovery後にREAD(10)を1回だけ再送する方針で治って
  しまう。再送は完全に同期された転送なので成功し、bufferは正常なdataで埋まる。これは
  retry方針が働いた証拠であって、契約違反ではない。

修正: 注入を**転送phase指定**にした。操作回数で狙うのは成立しない——commandのdata phaseに
到達するまでのcache呼び出し数は後続Stageで変わる実装詳細であり、間にReset Recoveryの
control転送も入る。phaseで指定すれば回数に関係なく目的のpacketへ当たり、`Control`と
labelされたrecoveryは動ける。`usbcachefail`はdata INへ4回分armし、再送を上回らせる。

#### `usbcachefail`の実機結果（修正後）

4ゲートすべてPASS。Stage 1の完了条件を満たす。

```text
USB:   phase=data-IN
USB:   the transfer is failed rather than started
USB BOT: bulk IN DMA cache sync refused during data IN
usbcachefail: refusals=1 pkt-fail cache=1 armed-left=3
usbcachefail: PASS the injected refusal was seen
usbcachefail: PASS the packet failed as a cache-sync failure
usbcachefail: PASS the read failed rather than succeeding
usbcachefail: PASS no bytes were published over the destination
```

phase指定が意図どおり働いた。注入はdata INへ当たり、間に走ったReset Recoveryの
control転送は`Control`とlabelされているので消費されていない。宛先bufferはsentinelの
まま——**DMAが書いていないstagingから1 byteも公開されない**ことが実機で確定した。

armは4回分だったが消費は1回（`armed-left=3`）。再送のCBWがhalt timeoutで落ち、data IN
phaseまで到達しなかったためである。

```text
USB BOT: reset recovery complete
USB MSC: retrying READ(10) after BOT recovery
USB:   failure=halt-timeout phase=CBW
USB:   requested bytes=31
USB:   actual bytes=0
USB BOT: recovery is not holding; this session needs re-enumeration
```

**Reset Recoveryが「complete」を返した直後のCBWをdeviceが受け取らなかった。**注入で
data phaseを途中放棄したため、deviceは512 byteのdataとCSWを送る途中で取り残されている。
Mass Storage ResetとCLEAR_FEATURE×2はそれを片付けるはずで、control転送自体は成功して
いるのに、次のCBWが通らない。人工的な故障で誘発した状態なので通常動作の証拠にはならないが、
「transport failureはdeviceを任意のBOT phaseで待たせたまま残せる」というStage 3／4が扱う
問題そのものが出ている。session は使用不能として引退した——**data を握ったまま続行するより
正しい落ち方**である。

`usbcheck`の`no packet failures`ゲートも直した。B1の34件、B2の1件はすべてQTD packet errorで、
retryで回復している。計画のGo条件は「packet retryが発生した場合はHCINTとactualが説明でき、
同じbyteを二重公開しない」であって、retryの発生自体は失格条件ではない。ゲートを
**driver contract**（`cache`／`qtdbad`／`nocpl`／`stale`／`splitrej`——deviceやケーブルには
作れない、このドライバ自身の契約違反）だけに絞り、transport由来（`timeout`／`stall`／
`xact`／`qtderr`）は`NOTE`で報告するようにした。実際に何かを壊したかどうかはread／write
ゲートが答える。

### Stage 2: channel 0完了と実転送長の単一化

**進捗: 完了。実転送長とretry安全性の契約は成立。Full-Speedハブ経路の残る失敗はStage 3へ移した。**

- arm直前に前世代のsoftware snapshot、HCINT、QTD状態を既定順序で消費する。
- transfer generationを持ち、古いIRQ／完了snapshotを新しいpacketへ対応付けない。
- `HCINT.XferCompl`と`QTD.Active == 0`を同じ世代で確認し、ChHltdだけでは成功にしない。
- QTD残量から`actual`を一度だけ計算し、`actual > requested`、残量underflow、hardware所有中を
  transport errorにする。
- short INとshort OUTを別結果にする。short OUTはBOTへ届く前に失敗とする。
- 完了後のcache同期と上位bufferへのcopyが終わるまでslot／stagingを再利用しない。
- timeout後のretryでprefixを二重送受信しない。安全性を証明できないOUT retryは行わない。

完了条件:

- 通常packetの成功はXferCompl、Active解除、妥当なactualを必ず満たす。
- syntheticな古いcompletion generationを注入しても新転送を成功にしない。
- 部分OUTを注入するとcommandが失敗し、要求長分の成功として上位へ返らない。
- baseline実機試験でdata mismatch、stale completionが0。
- **impossible lengthをbyte数として使わない。**当初この条件は「impossible lengthが0」
  だったが、実測で否定された。このcoreはpacket error時に実際に不可能な残量を書き戻す
  （後述）。0にできない値を0にする条件は、満たされないか、満たされたふりをされるかの
  どちらかにしかならない。
- ~~**Full-Speed固定ハブ経路（Stage 0のB1／B2）で`usbwritetest`の復元WRITEが10/10成功する。**~~
  **Stage 3へ移した。**実測の結果、この失敗は実転送長でもretry安全性でもなく、
  data OUT完了後のCSW INにdeviceが一切応答しない問題だった（後述）。BOT phaseの扱いと
  Recoveryの範囲なので、このStageでは直せない。

#### 実装したもの

- **`actual`を1箇所（`Channel0Transfer::progress_from`）でだけ導出する。**結果は
  `TransferProgress::Known(n)`か`Unknown`で、**「0 byte」と「不明」を型で区別する**。
  `Unknown`になるのは、hardwareがまだdescriptorを所有している／残量が要求長より大きい／
  `HCINT.XferCompl`が無いのにsubmit時の`QTD_EOL`が読み戻せない、の3つ。
- **`QTD_EOL`検査は完了経路には適用しない。**`XferCompl`で終わったpacketの計算は受入試験
  matrixの全構成で成立が確認済みで、そこへ新しい失敗条件を持ち込まない。この検査が効くのは
  timeout後の`cancel`——実機で`QTD final=0x00000000`が出た経路そのもの——である。
- **再送は`TransferProgress::Known(0)`のpacketにだけ許す**（`safe_to_retry`）。転送済みbyteが
  あるもの、`actual`が不明なものは即座にtransport errorへ上げ、`USB BOT: refusing to
  resubmit after ...`を出す。IN方向にも同じ規則を適用した。計画はOUTだけを要求しているが、
  部分受信したINを同じbufferへ再投入すると、deviceが次のpacketを送ってきた場合に公開済みの
  prefixを別のdataで上書きする。実機で観測された有用なretryはすべて`transferred=0`だった
  ので、この制限で失うものは無い。
- **短いOUTを`reap`で失敗にする**（`PacketFailureKind::ShortOut`）。上位層へ届かない。
- `PacketOutcome::Timeout`／`PacketError`のpayloadを`usize`から`TransferProgress`へ変えた。
  compilerが全呼び出し元を洗い出す。
- Split packetはbuffer DMAでdescriptorが無いので、放棄されたsplitのprogressは`Unknown`。
- **完了後のstaging再利用**は構造上すでに満たしている。`PacketStaging`は
  `Channel0Transfer`が所有し、`reap`が上位bufferへcopyし終えて`run_packet`が返るまで
  生存する。

#### 故障注入を`usbcachefail`へ2つ足した

Stage 2の完了条件のうち2つは、正常なhardwareでは起こせない状態を要求する。

- **古い世代の完了**: slotがもう持っていない世代でcompletionを渡す。同期APIでは1つの
  `Channel0Transfer`が1回の`run_packet`内で生成・submit・reapされるので、この状態は
  構造上起こらない。検査はこれを置き換えるqueue型schedulerのためにあり、
  **一度も発火を観測していない検査は、動くかどうか誰も知らない検査**である。
- **短いOUT**: OUT packetを要求より1 byte少なく報告させる。READ(10)を使うので注入が
  当たるのはcommand blockのOUT packetで、媒体には何も書かない。

`usbcachefail`は3つを順に実行し、各段階でsessionが引退したら自動で再列挙してから次へ進む。
接続構成には依存しないので全体で1回でよい。

#### Stage 2実機確認 第1回: 契約は3つとも成立、ただしB1を壊した

`usbcachefail`は9ゲートすべてPASS。3つの注入がいずれも検出され、吸収されなかった。

```text
usbcachefail: PASS [1/3] the refusal failed the packet as a cache-sync failure
usbcachefail: PASS [2/3] the stale completion was rejected and counted
usbcachefail: PASS [3/3] the short OUT was rejected and counted
usbcachefail: RESULT PASS
```

`usbcheck`は`driver contract`ゲートが全構成PASS。A1／A2／C1／C2は`RESULT PASS`、
`fswritetest`も通った。

**しかしB1（FS固定ハブ／不明媒体）が`writes ok=0/10 restored=0/10`になった。**
Stage 1では同じ構成が10/10だった。**これはこのStageが持ち込んだ後退である。**

```text
USB BOT: refusing to resubmit after packet error during data OUT
USB BOT:   bytes already transferred are UNKNOWN
USB BOT: bulk OUT packet retries exhausted during data OUT
USB MSC: WRITE(10) transport failed, not retrying
```

Stage 1の同じ構成の同じ箇所はこうだった。

```text
USB BOT: retrying bulk packet after packet error during data OUT
USB BOT:   bytes already transferred=0        <- 既知の0
USB BOT:   retry attempt=1
```

つまり、**これまで正常に再送できていたpacketが`Unknown`に分類されるようになった。**
原因は`progress_from`の信頼性判定を広く取りすぎたことである。「`XferCompl`が無いなら
`QTD_EOL`が読み戻せること」を要求したが、この経路のpacket errorはその条件を満たさない。

実機で確認できたcontrol wordは2つだけだった。

| 状況 | control word | 読み方 |
| --- | --- | --- |
| 完了したSETUP | `0x07000000` | EOL＋IOC＋IS_SETUP、残量0。**hardwareはEOLを保持する** |
| Full-Speedハブのtimeout | `0x00000000` | 全ビット0。残量0が「全部転送済み」を意味してしまう |

**判定を実際に観測した signature へ絞った。**`XferCompl`が無く、要求長が0でなく、
control wordが**丸ごと0**のときだけ`Unknown`とする。`0x00000000`は、EOLもIOCも無いのに
全量転送を主張しており、それ自体が矛盾している。一般的なEOL要求は取り下げた。

あわせて、判定の材料を必ず残すようにした。`refusing to resubmit`とretryのログへ
`reap HCINT=`と`reap QTD control=`を出し、`cancel`経路でもこのsnapshotを公開する。
**根拠にしたwordへ遡れない拒否は、規則のバグと区別が付かない**——第1回で実際にそうなった。

counterも直した。`retries_after_progress`が「転送済みbyteがある」と「不明」の両方を
数えていたため、B1は`progressed+10 bytes=0`という矛盾した行を出していた。
`refused`（契約が拒否した再送）と`progressed`（うちdescriptorがbyteを報告したもの）に
分けた。

#### Stage 2実機確認 第2回: descriptorの実測でretry規則を正した

`refusing to resubmit`へ`reap QTD control=`を出すようにした結果、判断材料が揃った。
観測できたcontrol wordは次のとおり。

| 状況 | control word | Active | status | EOL | 残量 | 要求 | |
| --- | --- | --- | --- | --- | --- | --- | --- |
| 完了したSETUP | `0x07000000` | 0 | 0 | 1 | 0 | 8 | 正常 |
| B2 CLEAR_FEATURE SETUP packet error | `0x17000008` | 0 | 1 | 1 | 8 | 8 | 正常 |
| B1 data OUT packet error | `0x16000040` | 0 | 1 | 1 | 64 | 64 | 正常 |
| **B1 data OUT packet error** | **`0x16018889`** | 0 | 1 | 1 | **100,489** | **64** | **不可能** |
| **A1 CBW packet error** | **`0x16000080`** | 0 | 1 | 1 | **128** | **31** | **不可能** |
| B2 data OUT timeout | `0x06000000` | 0 | 0 | 1 | 0 | 64 | 曖昧 |
| B1 CSW timeout | `0x00000000` | 0 | 0 | 0 | 0 | 64 | 曖昧 |

**このcoreはpacket error時に不可能な残量を書き戻す。**64 byteのOUTに対して100,489、
31 byteのCBWに対して128。いずれも`Active`解除・status 1・`EOL`／`IOC`保持の正規の
writebackで、byte数の欄だけが残量として成立していない。従来の
`buffer.len().saturating_sub(remaining)`はこれを黙って0へ丸めていた。

第1回の後退の本当の原因はここだった。`remaining > requested`を`Unknown`とし、
`Unknown`なら再送しないとしたため、**この経路のpacket errorがすべて再送不可になった**。

**規則を「放棄されたpacket」と「失敗が報告されたpacket」で分けた。**

- **packet error（QTD status 1）は残量に関係なく再送する。**USB packetは不可分で、
  deviceは丸ごと受け取るか全く受け取らないかのどちらかである。ここでのQTDは1 packetしか
  運ばないので、失敗の報告は「deviceが受け取らなかった」か「受け取ったがhandshakeが
  失われた」を意味する。同じDATA PIDでの再送は両方を覆う——toggleを進めた後のendpointは
  重複を捨てる。Stage 0／1でこの再送はFUA read-backにより10/10で正しさを確認済みである。
- **timeout（channelがhaltしなかった）は`Known(0)`のときだけ再送する。**completionが
  無いので、coreがpacketの途中だった可能性を否定できない。`0x06000000`は「status成功、
  残量0」を主張しながら`HCINT`は`0x00000000`で、force haltが必要だった。この値を
  byte数として読むと「64 byte全部出た」になり、実際に4回再送されていた。

第2回でこの分離が正しく働いていることも確認できた（B2、`fswritetest`）。

```text
USB BOT: retrying bulk packet after packet error during data OUT   <- 再送する
USB BOT:   reap QTD control=0x16000040
USB BOT: refusing to resubmit after timeout during data OUT        <- 再送しない
USB BOT:   bytes already transferred=64
USB BOT:   reap QTD control=0x06000000
```

**`requested=64 actual=64`のまま4回再送する形は消えた。**これがStage 2の本題だった。

不可能な残量は`Unknown`として弾くだけでなく数えるようにした（`usbcheck`の
`impossible-len`）。「起きない」ことにできない現象は、見えるようにしておく以外にない。

#### B1／B2の`fswritetest`について

Stage 2の完了条件に入れたが、第2回の時点でまだ通らない。ただし**落ちている場所が
変わった**。

- B2: WRITEのdata OUTがtimeoutし（`0x06000000`、既知の64 byte転送済み）、再送を正しく
  拒否してcommandを失敗させた。その後のReset Recoveryが`CLEAR_FEATURE(ENDPOINT_HALT)`の
  SETUPで失敗（`0x17000008`＝8 byte中0 byte）し、sessionが再列挙を要求した。
  **retryの安全性はStage 2で解決し、残っているのはRecoveryがdeviceを片付けられない問題**
  である。これはStage 3／4の「実失敗後のRecovery」の範囲にある。
- B1: 第2回では`usbcheck`のwrite roundsが0/10のまま採取された（上のretry規則修正より前の
  binary）。修正後の再確認が必要。

Stage 2の完了条件から「B1／B2の`fswritetest`が通る」を外すかどうかは、修正後の再測定を
見てから判断する。B1が10/10へ戻り、B2がRecovery失敗だけになるなら、その条件はStage 3／4へ
移すのが実態に合う。

#### Stage 2実機確認 第3回（B1のみ）: 後退は解消、残る失敗は別の問題

retry規則を分離した版でB1を再測定した。

| | pattern write | 復元WRITE | `refused` |
| --- | --- | --- | --- |
| Stage 0 | 10/10 | **2/10** | — |
| Stage 1 | 10/10 | 10/10 | — |
| Stage 2 第1・2回 | **0/10** | **0/10** | +10 |
| Stage 2 第3回（修正後） | 10/10 | **2/10** | **+0** |

**後退は解消した。**pattern writeが0/10から10/10へ戻り、`refused+0`——Stage 2のretry規則は
このrunで1件も再送を止めていない。packet errorを残量に関係なく再送する分離が効いている。

counterはすべて自己整合していた。`proactive write+20`＝10 pattern＋10復元、
`pkt-retry err+20`＝2 write×10 round、`timeout+32`＝4 retry×8失敗round、
`pkt-fail timeout+40`＝5試行×8失敗round、`recovery ok+8`＝8失敗、
`impossible-len+12`＝`0x16018889`×10＋`0x16014849`×2。

**復元WRITEの2/10はStage 0と同一である。**Stage 1の10/10は再現しなかった。1サンプルずつ
なので、Stage 1が効いたのか運だったのかは区別できない。**Stage 1もStage 2もこの失敗を
起こしてもいないし直してもいない**、というのが今言える全部である。

不可能な残量は2種類観測された。`0x16018889`（残量100,489）と`0x16014849`（残量84,041）、
いずれも64 byte要求に対して。どちらも正規のwritebackで、byte欄だけが成立していない。
12件すべて`Unknown`として弾かれ、**1件もbyte数として使われず、1件も正当な再送を妨げなかった**。
Stage 2の完了条件「impossible lengthをbyte数として使わない」は満たされている。

#### 残っている失敗: CSW INにdeviceが応答しない

8回の失敗はすべて同じ形だった。

```text
（復元WRITEのdata OUT）
USB BOT: retrying bulk packet after packet error during data OUT
USB BOT:   reap QTD control=0x16000040        <- 残量64＝1 byteも出ていない、再送して成功
（CSW IN）
USB BOT: retrying bulk packet after timeout during CSW
USB BOT:   reap HCINT=0x00000000              <- channelが何も上げない
USB BOT:   reap QTD control=0x06000040        <- 残量64＝1 byteも受け取っていない
  ... 4回再送 ...
USB:   failure=halt-timeout phase=CSW
USB:   requested bytes=64  actual bytes=0  QTD final=0x00000000
```

`HCINT=0x00000000`は、channelがhaltもcompletionも上げていないことを意味する。IN tokenに
対してdeviceが何も返していない。data phaseは通っているのにstatus wrapperだけが来ない。

これは実転送長の問題でもretry安全性の問題でもない。**data OUT完了後のBOT phaseで、
deviceがCSWを送れる状態になっていない**という問題であり、Stage 3（BOT phaseとCSW検証）と
Stage 4（Recovery）の範囲にある。Stage 2の完了条件からは外し、Stage 3へ移した。

B2で観測したReset Recovery自体の失敗（`CLEAR_FEATURE(ENDPOINT_HALT)`のSETUPが
`0x17000008`で失敗）も同じ系統だった。**deviceを任意のBOT phaseに取り残したまま、
当時のRecovery手順では片付けられていなかった。**

なお、失敗した8 roundは犠牲LBAへpatternを残したままである（`original data restored: NO`）。
`usbzero <LBA>`で消せる。周辺LBAへの波及は全roundで0だった。

### Stage 3: BOT phaseとCSW検証の厳密化

**進捗: 完了。host test、6構成の実機受入、故障注入がすべてPASS。**

- CBWはactual 31 byteだけを成功とする。
- data OUTは全量転送だけを成功とし、部分転送後にoffsetを要求長分進めない。
- data INはactualを保持し、short responseを要求長へ丸めない。
- CSWはactual 13 byte、期待tag、session中一貫したsignature、status 0〜2を検証する。
- CSW status 2のPhase Errorと未定義statusをtransport errorへし、BOT Reset Recoveryを行う。
- `dCSWDataResidue`を`CommandResult`へ残し、host actualと期待data長に矛盾する値を拒否する。
- PASSED＋非zero residueを無条件成功にしない。INQUIRY等の可変長応答とREAD／WRITEの固定長を
  MSC command層で区別する。
- data INへCSWが先着する13 byte short responseを専用経路で識別する。
- parserと整合判定をMMIOから分離し、hostでtable-driven testできる小さい`no_std` workspace
  memberへ置く。firmware側は検証済み結果だけを受け取る。

table-driven testには少なくとも次を含める。

- 正常IN／OUT／data無しcommand。
- CBW short、data OUT short、CSW 0／12／13／14 byte。
- signature／tag不一致、status 0／1／2／3。
- residue 0、期待長以内、期待長超過、host actualとの矛盾。
- data INへ前commandのCSWが来る形と、current commandのCSWが先着する形。

完了条件はhost testとbaseline実機試験が通り、古いCSWをpayloadまたはcurrent CSWとして成功扱い
しないことである。実機でPhase Errorを観測した場合はReset Recoveryの完了まで記録する。

**あわせて、Full-Speed固定ハブ経路（Stage 0のB1／B2）で`usbcheck`のWRITE／復元が10/10、
`usbrawcheck 1 32 1`がWRITE 32/32、pattern一致、snapshot復元となることを完了条件とする。**
Stage 2で移した条件をfilesystem非依存の再現試験へ置き換えた。Stage 2第3回の実測で、この経路の
失敗は実転送長でもretry安全性でもなく**data OUT完了後のCSW INにdeviceが応答しない**ことだと
確定し、raw burstでも同じ旧故障を3/32停止として再現できていた。v41は両媒体でこの条件を満たした。

```text
USB BOT: retrying bulk packet after timeout during CSW
USB BOT:   reap HCINT=0x00000000       <- channelが何も上げない
USB BOT:   reap QTD control=0x06000040 <- 1 byteも受け取っていない
```

先行するdata OUTには必ずpacket error（残量64＝1 byteも出ていない）と再送が1回ある。
**data phaseの再送とCSWの無応答が対で現れる**のがこの経路の形であり、まずここを説明する。

#### 実装したもの

- CBW OUTとdata OUTは実転送長が要求長と一致した場合だけ次phaseへ進む。
- CSW parserとhost actual／residue整合判定を依存なし`no_std` crate
  `tab5-bot-protocol`へ分離した。13 byteちょうど、`USBS`、current tag、status 0／1、
  residue上限と方向別の実転送長を1回の検証で確定する。Phase Errorと未定義statusは
  transport errorであり、`execute_command`の既存経路からReset Recoveryへ進む。
- data INはpacketを上位bufferへcopyする前に13 byte CSWか検査する。current commandのCSWが
  先着した場合は同じCSWをもう一度待たず、前commandのtagならstale CSWとして失敗させる。
  先着前に受信済みのdata prefixはhost actualとして保持する。
- `CommandResult`へ`expected`と`residue`を追加した。READ／WRITE／READ CAPACITY／REQUEST SENSE／
  標準INQUIRYはMSC層で全量・residue 0を要求し、可変長INQUIRY VPDはheaderと実受信長で扱う。
- `usbhw`／`usbcheck`へPhase Error、未定義status、residue矛盾、CSW先着counterを追加した。
  `usbcheck`はinvalid CSWが1件でもあればStage 3 gateをFAILにする。

host testは正常IN／OUT／data無し、CSW 0／12／13／14 byte、signature／tag、status 0〜3、
residue 0／範囲内／超過／host actual矛盾、current／前command CSW先着を含む7 table-driven testを
通過した。`cargo check -p tab5-hello-world --release`も通過した。

実機受入はStage 2と同じ6構成で`usbcheck 100 <犠牲LBA>`と
`usbrawcheck <犠牲LBA> 32 1`を実行する。
特にB1／B2では`delta csw`と`delta csw-detail`、data OUT packet retry直後のCSW応答、
復元WRITE 10/10を記録する。これが通るまでStage 3完了およびStage 4着手とはしない。

#### Stage 3実機確認 第1回: CSW契約はPASS、filesystem WRITEの旧故障は再現

B1／B2で`usbcheck 100 1`と`fswritetest`を実行した。`usbcheck`は両媒体ともread 100/100、
WRITE 10/10、復元10/10、collateral 0でPASSした。`delta csw short／sig／tag`と
`phase／status／residue／early`はすべて0で、Stage 3の新しい検証は正常trafficを拒否して
いない。B1ではQTD packet error 34件と不可能残量34件を数えたが、すべて同一PID retryで
回復し、refused／progressedは0だった。B2のpacket errorは0だった。

ただし`fswritetest`は両媒体とも検査1でFAILしたため、Stage 3全体はNo-Goのままである。

- B1: CBWとdata OUTのpacket errorを各1回retry後、CSW INが`HCINT=0`、
  `QTD=0x06000040`のままhaltせず、Reset Recoveryのcontrol転送だけは完了した。
- B2: data OUTのpacket errorを1回retry後、次のdata OUTが`HCINT=0`、
  `QTD=0x06000000`（64 byte進んだ主張）のままhaltしなかった。危険な再送は拒否した。
  続くReset RecoveryもCLEAR_FEATUREのSETUP packet errorで失敗した。

2つに共通する直前操作を監査すると、`run_packet`が`ChHltd`を確認しQTDをreapして
`PacketError`を返したあとにも、BOT retry側が`recover_channel_after_packet_failure`を呼び、
channel／FIFOをflushしていた。fresh QTDは次のsubmitがHCINTをclearして再armするため、reported
packet errorにこのcleanupは不要である。Stage 3第2回向けにこの呼び出しを外した。channelが
haltしなかったtimeoutのforce halt／cleanup、正常WRITE前の予防cleanupは変更していない。

#### Stage 3実機確認 第2回: packet error後cleanup撤去はNo-Go

B1で第1回と同じ`usbcheck 100 1`を実行したところ、readは100/100、CSW契約counterは全0の
ままだったが、WRITEは前回の10/10から**0/10**へ後退した。各WRITEでdata OUTのQTD status 1が
retry上限まで繰り返され、packet failureは34から220、packet retryは34から210、Reset Recoveryは
0から10へ増えた。全WRITEがpattern投入前に失敗したため復元対象とcollateral changeは0だった。
続く`fswritetest`も従来どおり検査1で失敗した。

したがって「reported packet errorはChHltd／reap済みなのでcleanup不要」という仮説は棄却する。
このcoreのFull-Speed経路は、software上のchannel完了だけでは次のQTDを受け付ける状態へ戻らず、
packet error後のchannel／FIFO cleanupが同じstatus 1の反復を止めていた。呼び出しを第1回の状態へ
戻した。Stage 3の残件は、cleanupを維持した状態でまれにdata OUT後のCSWまたは次packetが
無応答になる理由である。

#### Stage 3実機確認 第3回準備: OUT cleanupを送信FIFOへ限定

第2回で分かったのは「何らかのcleanupが必要」であって、「RX／periodic TXを含む全FIFOを
毎回flushする必要がある」ではない。失敗したWRITEに先行するのはOUT packet errorであり、
そのpacketの残量を持ち得るのはchannel 0のnon-periodic TX FIFOである。従来の共通APIは方向を
受け取らないため、OUTの再送前にもRX／periodic TXまでflushしていた。

`recover_reported_packet_error(is_in)`を追加し、reported OUT packet errorではchannel状態と
HCINTを既定状態へ戻したうえでnon-periodic TXだけをflushする。IN packet errorはRX residueを
否定できないので従来の保守的範囲を維持する。timeout、command failure後、正常境界の予防cleanupも
変更しない。適用回数はcontroller-wide `out-nptx` counterへ記録し、`usbcheck`の
`delta packet-cleanup`へ出す。これはcleanup全撤去の再試行ではなく、必要だと確定した送信側
cleanupを残して無関係なFIFO操作だけを外すA/Bである。

B2でMass Storage Reset成功直後のCLEAR_FEATURE SETUPがpacket errorになった点も、Recovery手順の
待機不足として切り分ける。Linuxはclass reset後に回復待ちを置き、U-Bootは各手順間に150 ms置く。
この版は正常command経路を変えず、Mass Storage Reset成功後から最初のCLEAR_FEATUREまで150 ms
待つ。第3回でWRITE自体の成否と、WRITEが失敗した場合のReset Recovery成否を別々に記録する。

第3回の判定:

- 起動ログが`USB TEST: directional packet cleanup v25`であること。
- B1／B2で`usbcheck 100 1`を実行し、`delta packet-cleanup out-nptx`がOUT packet retryと
  対応し、write／restoreが10/10、CSW契約counterが全0であること。
- 続けて`fswritetest`を実行する。失敗した場合も、CSW無応答または次data OUT無応答が再現したか、
  Reset Recoveryが150 ms待機後にcompleteしたかを区別する。
- status 1が20回反復してWRITE 0/10へ戻った場合は、non-periodic TXだけでは不足なので即No-Goとし、
  第1回の全cleanupへ戻す。

#### Stage 3実機確認 第3回: 方向別cleanupとReset待機はGo、filesystem WRITEは未解決

B1／B2とも`USB TEST: directional packet cleanup v25`で確認した。`usbcheck 100 1`はread
100/100、WRITE／復元10/10、collateral 0、CSW契約counter全0でPASSした。B1のreported OUT
packet error 34件に対して`out-nptx+34`、B2の1件に対して`out-nptx+1`となり、方向別cleanupは
意図したpacketだけへ適用された。FIFO flush timeoutは0で、cleanup全撤去時のstatus 1連続再発も
なかった。したがってOUTのnon-periodic TX cleanup限定はGoとして維持する。

`fswritetest`は両媒体とも検査1でFAILし、Stage 3全体は引き続きNo-Goである。

- B1: data OUTのpacket errorを1回retryした後、CSW INが`HCINT=0`、
  `QTD=0x06000040`からhaltせず、最終的に`QTD=0`となった。Reset Recoveryはcompleteした。
- B2: CBWとdata OUTのpacket errorを各1回retryした後、次のdata OUTが`HCINT=0`、
  `QTD=0x06000000`のままhaltしなかった。64 byte進んだ主張なので再送は拒否した。
  Reset Recoveryはcompleteした。

B2では第1回にMass Storage Reset直後の最初のCLEAR_FEATUREが失敗したが、第3回は同じWRITE
失敗後にもRecoveryを完了した。150 ms待機はReset手順の改善としてGoとし、WRITE無応答とは
別に維持する。

#### Stage 3実機確認 第4回準備: direct channelのMC/ECを1へ初期化

channel設定を監査すると、split transferはHCCHARのMC/EC fieldへ1を設定していた一方、direct
descriptor-DMA transferはreset値0のままarmしていた。Linux DWC2は非周期を含む全channelへ
`multi_count = 1`を設定してからHCCHARへ書く。通常のBulk／controlでこのfieldが無視されるcoreも
あるが、error／retry時のchannel状態差を1 fieldずつ切り分けるため同じ初期値へ合わせる。

第4回の変更はdirect channelのHCCHARへ`MC/EC=1`を加える1点だけである。第3回でGoとした
方向別OUT cleanupとMass Storage Reset後150 ms待機、retry／CSW契約は維持する。

第4回の判定:

- 起動ログが`USB TEST: channel MC one v26`であること。
- B1／B2で`usbcheck 100 1`を実行し、write／restore 10/10、CSW契約counter全0、
  `out-nptx`とOUT packet retryの対応を確認する。
- 続けて`fswritetest`を実行し、検査1のmkdirが通るか確認する。失敗時はCSWまたは次data OUTの
  無応答、Reset Recovery成否、HCINT／QTDを第3回と比較する。
- `usbcheck`が後退した場合はMC/EC=1を即No-Goとして戻す。`usbcheck`が同等でも
  `fswritetest`の故障形が変わらなければ、効果なしとして次のHCD差分監査へ進む。

#### Stage 3実機確認 第4回: direct channelのMC/EC=1はNo-Go

B1は`usbcheck 100 1`がread 100/100、WRITE／復元10/10、CSW契約counter全0でPASSした。
reported OUT packet errorは20件で`out-nptx+20`と一致したが、`fswritetest`は第3回と同じく
data OUTのpacket errorをretryした後、CSW INが`HCINT=0`、`QTD=0x06000040`からhaltせず失敗した。

B2は正常系が明確に後退した。v25ではread 100/100だった`usbcheck`が4/100で停止し、data INの
`HCINT=0`、`QTD=0x06000040` timeoutをpacket retry上限まで続けた。Reset Recoveryはcompleteしたが、
Recovery後のREAD再送も同じ形で失敗し、session unusableになった。差分counterはtimeout 10、
packet retry 8、command retry 1、Recovery成功2である。続く`fswritetest`も第3回と同じく、data
OUT packet errorのretry後に次のdata OUTが`HCINT=0`、`QTD=0x06000000`のままhaltしなかった。

通常の非周期channelでMC/EC=1は採用しない。direct descriptor-DMA transferからこのbitを除去して
v25の0へ戻し、split transferだけ1を維持する。B1のpacket errorが34から20へ減ったことは1 run間の
変動と区別できず、B2の正常系後退を覆す採用理由にはならない。

#### Stage 3実機確認 第5回準備: 失敗channelのregister snapshot

第4回は「Linuxと同じ値」がこのcoreのdirect channelに正しいとは限らないことを実機で確定した。
次は推測したregister値を変えず、v25の転送動作へ戻して失敗時の事実を追加採取する。

- reported packet errorをreapした直後、方向別cleanupを行う前にchannel 0のHCCHAR／HCTSIZ／HCDMAを
  表示する。
- channel halt timeoutでは、force haltでregisterを変える前に同じ3値を表示する。
- HCCHARのChEna／ChDisとendpoint設定、HCTSIZのPID／schedule情報、HCDMAのdescriptor list addressを
  packet errorと、その後にhaltしないpacketで比較する。

第5回は観測追加だけでretry、cleanup、timeout、BOT Reset Recoveryの挙動を変えない。起動ログは
`USB TEST: channel snapshot v27`とする。B2で`usbcheck 100 1`が100/100へ戻ることをrollback確認とし、
B1／B2の`fswritetest`で`channel HCCHAR/HCTSIZ/HCDMA`と`before halt`を採取する。

#### Stage 3実機確認 第5回: 次packetは正しくarm済みだがcoreがhaltしない

B1／B2とも`usbcheck 100 1`はread 100/100、WRITE／復元10/10、CSW契約counter全0でPASSした。
B1のpacket error 20件はすべて`out-nptx`と対応し、B2はpacket failure 0だった。v26で4/100へ
後退したB2 readが元へ戻ったため、direct channelのMC/EC=0 rollbackは確認済みとする。

B1の`fswritetest`は、data OUTのpacket errorを同一PIDで再送した後、CSW INがhaltしなかった。

- packet error reap: HCCHAR `0x00C81040`（ChEna=0、Bulk OUT）、HCTSIZ `0x000000FF`
- CSW retry後の最終timeout前: HCCHAR `0x80C88840`（ChEna=1、Bulk IN）、
  HCTSIZ `0x400000FF`（DATA1、schedule `0xFF`）、HCDMA `0x4FF77800`
- HCINTは0、portはconnected／enabled／powered、HFNUMは進行

したがって次CSWはendpoint、direction、PID、descriptor listを設定してChEnaまで立てており、古い
HCCHARを持ち越した失敗ではない。coreはframeを生成し続ける一方、このchannelをhaltさせていない。

B2も`fswritetest`はdata OUT packet errorの再送後、次のdata OUTが`HCINT=0`、
`QTD=0x06000000`のままhaltしなかった。Reset Recoveryは今回CLEAR_FEATUREのSETUP packet errorで
失敗した。v25／v26では同じ媒体のRecoveryがcompleteしているため、150 ms待機は改善ではあるが
成功保証ではない。

#### Stage 3実機確認 第6回準備: 再送成功後だけsettle

B1／B2の共通境界は「reported packet error」そのものではなく、cleanupと同一PID再送を終えた直後の
次packetである。現在はpacket error後にcleanupし50 ms待ってから再送するが、再送が成功すると通常の
packetと同じく即座に次へ進む。第6回は再送成功後にも50 ms待ち、core／deviceが次packetへ移る前の
settle時間を対称にする。

変更は`run_bulk_packet`内でpacket error retryが1回以上あったpacketの成功return直前に50 ms待つ
1点だけである。packet errorのない通常traffic、timeout retry、toggle、cleanup、Reset Recoveryは
変更しない。起動ログは`USB TEST: post-retry settle v28`とする。

第6回の判定:

- B1／B2の`usbcheck 100 1`がread 100/100、WRITE／復元10/10、CSW契約counter全0を維持する。
- `fswritetest`検査1が通るか、少なくとも従来の「再送成功直後の次packet無応答」が消えるかを確認する。
- `usbcheck`が後退した場合、または次packetが同じregister値でhaltしない場合はNo-Goとしてsettleを戻す。

#### Stage 3実機確認 第6回: 再送成功後50 ms settleはNo-Go

B1／B2とも`usbcheck 100 1`はread 100/100、WRITE／復元10/10、CSW契約counter全0を維持した。
B1のreported packet error 20件と`out-nptx+20`も従来どおり対応したため、正常系の後退はない。

しかし`fswritetest`は両媒体とも検査1でFAILし、待機前と故障形が変わらなかった。

- B1: data OUTの`QTD=0x16000040` packet errorをretry後、CSW INが強制halt前
  HCCHAR `0x80C88840`、HCTSIZ `0x400000FF`、HCINT 0のまま停止した。
- B2: CBW packet errorとdata OUTの`QTD=0x16000040` packet errorをretry後、次data OUTが
  `QTD=0x06000000`のまま停止した。Reset Recoveryも両CLEAR_FEATUREのIN status stageで
  packet errorとなり失敗した。

再送成功後50 msは結果にもregister snapshotにも影響しないためNo-Goとして撤去する。問題はdeviceの
処理待ちではなく、reported zero-progress errorから復帰したchannelの次のactivationに残る。

#### Stage 3実機確認 第7回準備: zero-progress retry後のchannel／NPTX cleanup

filesystem故障に先行するpacket errorはB1／B2とも`QTD=0x16000040`であり、descriptorの残量64、
つまり0 byte進行を報告する。一方、B1の`usbcheck`で20件発生してすべて回復したpacket errorは
`0x16018889`／`0x16014849`のような不可能残量型である。reported errorとしての再送規則は同じでも、
次channelが停止する形はzero-progress型へ絞れる。

第7回はOUTのzero-progress packet errorを再送して成功した直後だけ、channel interrupt baselineを
clearしnon-periodic TX FIFOをflushしてから呼び出し側へ成功を返す。error直後の既存cleanupは残すため、
再送の前後をcleanupで挟む形になる。不可能残量型のerror、通常packet、timeout retry、toggle、
Reset Recoveryは変更しない。適用時は
`USB BOT: post-retry OUT cleanup after reported zero progress`を表示する。起動ログは
`USB TEST: post-retry cleanup v29`とする。

第7回の実機結果（v29、No-Go）:

- B1／B2の`usbcheck 100 1`は従来の全PASSを維持した。
- B1の`usbrawcheck 1 32 1`は3/32後、`QTD=0x16000040`のzero-progress errorが再送20回を
  使い切ってFAILした。再送は一度も成功しないためpost-success cleanupログは出ていない。
  Reset Recovery後のsnapshot復元は成功した。
- B2の`usbrawcheck 1 32 1`は32/32、pattern、復元がすべてPASSした。
- v29 cleanupは到達不能な故障を直せないため撤去した。次のA/Bはdelay／flushを足さず、
  descriptor DMAそのものを外す。

#### 第8回: Full-Speed BOT Bulkのbuffer DMA化（v30）

MPS 64以下、`Route::split == None`のBOT Bulk packetだけをbuffer DMAで実行する。control、HID、
High-Speed MPS 512、Splitは従来経路を維持する。`HCFG.DescDMA`はcontroller-wideなので、periodic
channelがarm中なら切替を拒否する。MSCとHIDの併用時は既存registryがpersistent periodicを停止し、
全classをchannel 0へ逐次化済みである。

buffer DMAはQTDを使わず、`HCDMA`へ64-byte aligned `PacketStaging`のdata addressを直接設定する。
完了長は`HCTSIZ.XferSize`の減算で決める。非周期buffer DMAはNAKでchannelがhaltするため、同じ
HCTSIZ、HCDMA、DATA PIDを累積timeout内でsoftware rearmする。timeoutしたOUTの進捗は引き続き
`Unknown`とし、再送しない。`usbhw`の`IRQ direct buffer DMA`でpacket数とNAK rearm数を確認する。
起動ログは`USB TEST: FS Bulk buffer DMA v30`とする。

第8回の判定:

- B1／B2で`usbcheck 100 1`が全PASSし、`usbrawcheck 1 32 1`が32/32、pattern match、restored yes。
- `usbcheck`／`usbrawcheck`中にQTD packet error／`impossible-len`が増えない。control等の既存
  descriptor経路のcounter増加は別に扱う。
- `usbhw`でdirect buffer packetが増加し、periodic conflictが増えない。
- B1で初回packetから失敗する場合はbuffer DMAのprogramming／NAK handlingを修正し、descriptor
  DMAへ黙ってfallbackしない。raw burstだけで従来故障形が残る場合はHCDより下のPHY／FIFO／device
  interactionを次候補にする。

第8回の実機結果（v30、programming No-Go）:

- B1は起動中のMSC commandですでにsession unusableとなり、`usbcheck`開始時にはcommandを
  送れなかった。
- B2は最初の31-byte CBWから失敗した。再送時の`HCINT=0x00000092`は
  ChHltd＋NAK＋XactErr、`HCTSIZ=0x0008001F`は31 byte全量が未転送で、従来の累積後QTD故障とは
  別の即時programming failureである。Reset Recoveryは2回とも完了したが同じCBWが20 retryを
  使い切った。
- `HCCHAR=0x00C81040`はMC/EC=0だった。Split buffer DMAは同fieldを1にしており、DWCのbuffer-DMA
  channelは1 transactionを明示する必要がある。v26でNo-Goだったのは**descriptor DMA**へ1を
  適用した結果なので、v31はdirect buffer DMAだけ`HCCHAR_MC_ONE`を設定する。descriptor DMAは
  0、Splitは従来どおり1を維持する。

第9回の判定（v31）:

- 起動markerが`USB TEST: FS Bulk buffer DMA MC1 v31`であること。
- まずB2で`usbcheck 100 1`を実行し、CBWの即時`HCINT=0x92`が消えること。失敗したらraw WRITEへ
  進まない。
- B2のread／writeがPASSした場合だけ`usbrawcheck 1 32 1`、次にB1で同じ2 commandを実行する。
- buffer-DMA channelのHCCHARにはMC/EC=1が入り、descriptor-DMA回帰の`read 4/100`を再発させない。

第9回の実機結果（v31、No-Go）:

- B2は`HCCHAR=0x00D81040`となりMC/EC=1のprogrammingを確認したが、`HCINT=0x92`、未進行
  `HCTSIZ=0x0008001F`はv30と同一で、CBWを20 retryしても通らなかった。MC/EC不足説は否定。
- B1も最初のTEST UNIT READY CBWで2回失敗した。B2の即時XactErrと異なり、timeout 2件と
  short OUT 2件を数えたため、direct buffer DMAは一律無反応ではないが安全なpacket境界を作れない。
- Split buffer DMAは維持するが、非Split BOTへbuffer DMAを使うv30／v31経路は撤回する。

#### 第10回: channel-0 QTDの固定2-slot ping-pong（v32）

descriptor DMAへ戻す。従来の`Channel0Transfer`はQTDをstack fieldとして所有し、同期呼び出しの
stack frameが毎回同じ位置になるため、正常packet、reported error、そのretry、次packetのすべてが
同じHCDMA base（実測`0x4FF77C00`）を即時再利用していた。softwareのgeneration tokenは変わっても、
hardwareが見るdescriptor addressには世代境界が無かった。

v32はinternal RAMへ512-byte strideのQTDを2個固定確保する。新しい`Channel0Transfer`ごとにslotを
交互選択し、submit／cache sync／reap／cancelは選んだ物理slotだけを使う。channelは従来どおり
同期channel 0が1本なので、2 slotが同時にhardware ownershipを持つことはない。retryは失敗した
QTDと別address、成功後の次packetも直前成功QTDと別addressになる。payload staging、DATA PID、
packet retry規則、cleanupは変更しない。release ELF検査はbankがinternal RAM、512-byte aligned、
2×512 byteであることを必須にする。起動markerは`USB TEST: channel-0 QTD ping-pong v32`。

第10回の判定:

- まずB2 `usbcheck 100 1`でdescriptor DMAの通常動作へ戻り、HCDMAが2つの512-byte境界addressを
  交互に取ること。失敗時ログだけでは片側しか見えないため、結果がPASSならpacket error時のretry
  addressを確認する。
- B2がPASSした場合だけ`usbrawcheck 1 32 1`、続いてB1の同じ2 commandへ進む。
- B1 raw burstでzero-progress packet errorが出ても、同じQTD addressの20回反復にならず回復するかを
  判定する。失敗する場合は2-slotのaddress列を採り、descriptor address再利用説をNo-Goにする。

第10回の実機結果（v32、通常回帰Go／根本対策No-Go）:

- B2は`usbcheck 100 1`がread 100/100、write／restore 10/10でPASSし、続く
  `usbrawcheck 1 32 1`も32/32、pattern match、restored yesでPASSした。
- B1も`usbcheck 100 1`は全PASS。packet error時の`channel HCDMA=`は
  `0x4FF51400`／`0x4FF51600`へ交互に切り替わり、固定bankがhardwareへ反映された。
- それでもB1 raw burstは3/32後にzero-progress `QTD=0x16000040`を20回連続しFAILした。
  最終retryのHCDMAは`0x4FF51600`で、Reset Recovery後のsnapshot復元は成功した。
  よって同一QTD addressの即時再利用説はNo-Go。2-slot bank自体は通常系を壊さず、descriptorの
  software世代を物理addressにも反映する所有境界として維持する。

#### 第11回: zero-progress OUT error後のdescriptor-DMA mode再始動（v33）

v30／v31は非Split Bulk packetそのものをbuffer DMAで実行しようとして最初のCBWから失敗した。
今回は転送方式を変えず、descriptor DMAでreported zero-progressとなったBulk OUTだけを対象にする。
channel 0がhalt済みでperiodic channelもarmされていないことを確認し、`HCFG.DescDMA`をoff→onして
同じDATA PIDを再送する。non-periodic TX FIFO cleanupと50 ms retry間隔は維持する。periodic HIDが
arm中ならcontroller-wide modeを変えず、従来のOUT cleanupだけへfallbackする。

`usbcheck`／`usbhw`の`descdma-restart`で実行回数を観測する。起動markerは
`USB TEST: descriptor DMA restart v33`。

第11回の判定:

- B2で`usbcheck 100 1`、`usbrawcheck 1 32 1`の全PASSを維持する。
- B1で`usbcheck 100 1`を先に実行し、zero-progress OUTが出た場合だけ
  `descdma-restart`が増えること。通常packetやIN errorでは増えないこと。
- B1 `usbrawcheck 1 32 1`が32/32、pattern match、restored yesならGo。失敗時も復元結果を確認し、
  restart後のHCINT／QTD／HCDMAを次の切り分けに使う。rawが安定するまで`fswritetest`は行わない。

第11回の実機結果（v33、発火条件No-Go）:

- B2は`usbcheck 100 1`とraw 32/32が全PASS。B1も`usbcheck`はread 100/100、write／restore
  10/10でPASSし、通常回帰は維持した。
- B1 rawは従来どおり3/32でFAILし、Reset Recovery後のsnapshot復元は成功した。
- `descdma-restart`はB1／B2の`usbcheck`でともに0。B1 rawの停止packetでは、再送可能な最初の
  20 errorがすべて不可能なdescriptor残量で、retry上限後の最後だけ`QTD=0x16000040`だった。
  `Known(0)`はretry branchへ一度も入らず、v33のmode再始動は実行されていない。
- 同一packet errorの途中でもdescriptor残量が揺れるため、zero-progress値をcontroller recoveryの
  発火条件にする設計を撤回する。descriptor-DMA再始動仮説そのものは未判定。

#### 第12回: 同一packetのreported OUT error反復でdescriptor DMA再始動（v34）

同じpacketの最初のreported OUT errorは従来どおりNPTX cleanupだけで再送する。この再送もerrorに
なった場合、2回目以降はdescriptor残量に依存せず`HCFG.DescDMA`をoff→onして同じDATA PIDを再送する。
B1 `usbcheck`の33件の一過性errorはすべて最初のretryで回復しており、通常系ではmodeを変更しない。
停止系列だけを選ぶ条件として「同一packetで2回連続」を使う。最初の発火時は
`USB BOT: restarting descriptor DMA after repeated OUT packet error`を出す。起動markerは
`USB TEST: repeated-error DMA restart v34`。

第12回の判定:

- B2の`usbcheck 100 1`と`usbrawcheck 1 32 1`が全PASSし、一過性errorだけなら
  `descdma-restart`が増えないこと。
- B1 `usbcheck 100 1`の全PASSを維持すること。続くraw burstの反復errorでrestartログが出て、
  32/32、pattern match、restored yesまで到達すればGo。
- rawがFAILしてもfilesystem試験へ進まず、restartログ、最後のQTD／HCDMA、snapshot復元結果を採る。

第12回の実機結果（v34、No-Go）:

- B2は`usbcheck 100 1`とraw 32/32が全PASS。B1も`usbcheck`は全PASSし、33 packet retryの
  うち13回でdescriptor-DMA再始動が実際に発火した。
- B1 rawでは停止packetの2回目以降にrestartログが出て、約19回modeをoff→onしても3/32のまま
  retry exhaustedとなった。Reset Recoveryとsnapshot復元は成功した。
- controller内部descriptor fetch状態の再始動は結果を変えないためNo-Go。controller-wide modeを
  触る処理とcounterを撤去し、v29の方向別NPTX cleanupへ戻す。

#### 第13回: raw WRITE command間隔によるexcessive NAK切り分け（v35）

ESP-IDF v5.5.3のESP32-P4 `usb_dwc_ll.h`／HALと現在実装を再照合した。QTDのxfer size、setup、IOC、
EOL、status、activeのbit位置、HCDMAの512-byte list base、HCTSIZのNTD=0／SCHED_INFO=0xFFは一致し、
descriptor programmingの根本的なfield違いは見つからなかった。公式LLでもQTD status 1は
CRC／timeout／stuff／false EOPに加えて**excessive NAK**を含み、失敗descriptorの残量は上位HCDで
成功長としてparseしていない。

B1だけがREADやready pollを挟まないraw WRITEを3回通した後に同じBulk OUTをNAK相当で拒み続け、
WRITEごとにreadbackする`usbcheck`は10/10を完走する。deviceが前WRITEの内部処理中に次commandの
data OUTをNAKし、50 ms×20回の現retry budgetを超える仮説を、transportを変える前に測る。

`usbrawcheck <lba> [writes] [span] [gap_ms]`へ0〜2000 msのcommand間隔を追加する。gapは成功した
WRITEと次WRITEの間だけで、burst後の照合／復元やpacket retry処理は変えない。既定0はv34以前と
同じ故障再現条件。起動markerは`USB TEST: raw WRITE pacing probe v35`。

第13回の判定:

- B1でまず`usbrawcheck 1 32 1 250`を実行する。32/32ならdevice busy仮説をGoとし、次版で
  OUT packet retryをbounded backoff化する。
- 250 msでFAILし復元成功なら`usbrescan`後に`usbrawcheck 1 32 1 1000`を実行する。1000 msでも
  同じ3/32ならcommand間busy仮説をNo-Goにする。
- filesystem testはまだ使わない。B2は既定gap 0で既に32/32のため、v35はB1だけで判定する。

第13回の実機結果（v35、No-Go）:

- B1 `usbrawcheck 1 32 1 250`は最初のWRITE dataで失敗し、0/32。Reset Recovery後のsnapshot
  復元は成功した。
- `usbrescan`後の`usbrawcheck 1 32 1 1000`も従来と同じ3/32で停止し、復元は成功した。
- command間のdevice busy／excessive NAKが50 ms×20 retryを超える仮説はNo-Go。gap引数は
  再診断用として残すが、既定0のtransport動作は変えない。

#### 第14回: Full-Speed WRITE dataを1つのdescriptor-DMA QTDへ集約（v36）

ESP-IDF v5.5.3のBulk descriptor-DMA経路は、endpoint MPSごとにchannelをhalt／rearmせず、Bulk
transfer全体を1 QTDへ載せてhardwareにpacket列とDATA PIDの進行を任せる。現在実装はCBW、data、
CSWをすべてMPS単位のQTDへ分け、B1の1 block WRITEでは64 byte QTDを8回armする。この差は
QTD field値ではなく、同一channel activation内で何packetを進めるかにある。

v36では範囲を非Split・MPS 512未満のWRITE data phaseだけに限定し、最大512 byteを1 QTDへ載せる。
CBW、CSW、Bulk IN、Splitは従来の1 packet QTDを維持する。複数packet OUT QTDが失敗した場合、
descriptor残量からdeviceが受理したprefixを確定できないため同じQTDを再送しない。commandを失敗させ、
通常のBOT Reset Recoveryで両endpointをDATA0へ戻す。`usbcheck`は`delta write-data-qtd multi+N`、
`usbrawcheck`は`write-data multi-QTD=N`で実際に新経路を使った回数を出す。起動markerは
`USB TEST: WRITE data single-QTD v36`。

第14回の判定:

- 先にB2で`usbcheck 100 1`と`usbrawcheck 1 32 1`を実行し、read 100/100、write／restore、
  raw 32/32と復元を維持すること。両commandでmulti-QTDが0より大きいこと。
- 次にB1で同じ2 commandを実行する。rawが32/32、pattern match、restored yesなら、64 byteごとの
  channel halt／rearmが停止条件だったとしてGo。
- 複数packet QTDでfailureになった場合にpacket retryが増えず、unsafe replay拒否ログからReset
  Recoveryへ進むこと。FAILでもfilesystem testへ進まず、snapshot復元結果を採る。

第14回の実機結果（v36、programming No-Go）:

- B2 `usbcheck 100 1`はREAD 100/100を維持したが、WRITE／復元は1/10へ悪化してRESULT FAIL。
  `delta write-data-qtd multi+12`で新経路が確実に発火した。
- 512-byte data OUTはQTD status 1が5回、XferComplなしが2回、CSW timeoutも発生した。
  HCCHARは従来の`0x00C81040`、つまりMC/EC=0のままだった。複数packet QTDの失敗は再送せず
  Reset Recoveryへ進み、10回ともRecovery自体はcompleteした。
- 途中のrestore失敗があるため犠牲LBAの内容は保証しないが、collateral changeは0。filesystem
  外の犠牲範囲なのでrepairは不要。B1とrawへは進まない。

#### 第15回: 複数packet descriptor-DMA QTDだけMC/EC=1（v37）

v26はdirect descriptor-DMAの全transferへHCCHAR MC/EC=1を設定し、B2 READを100/100から4/100へ
悪化させたためNo-Goだった。v37ではその結果を覆さず、`QTD bytes > endpoint MPS`のchannelだけへ
MC/EC=1を設定する。CBW、CSW、全Bulk IN、Split以外の1 packet OUTは0のままで、v26が壊したREAD
経路には一切適用しない。Full-Speed 512-byte WRITE dataの失敗時HCCHARは`0x00D81040`になる。
起動markerは`USB TEST: WRITE data single-QTD MC1 v37`。

第15回の判定:

- B2でまず`usbcheck 100 1`だけを実行する。READ 100/100、WRITE／復元10/10、multi-QTD>0を
  すべて満たせば、続けて`usbrawcheck 1 32 1`を実行する。
- failure時HCCHARが`0x00D81040`なら限定MC/EC=1のprogramming発火を確認できる。同じFAILなら
  v37もNo-Goとしてmulti-QTD自体を撤回し、B1へは進まない。
- filesystem testは使わない。

第15回の実機結果（v37、No-Go）:

- B2 READは100/100を維持したが、WRITEは1/10、復元0/10でRESULT FAIL。v36から改善しなかった。
- data OUT失敗時HCCHARは狙いどおり`0x00D81040`で、限定MC/EC=1は確実に発火した。QTD status 1が
  7件、CSW timeoutを含むtimeoutが20件、Recoveryは11回すべてcompleteした。
- ESP-IDF v5.5.3の実ソースを取得して再確認すると、direct descriptor-DMAの
  `usb_dwc_ll_hcchar_init`はMC/ECを設定せず0のまま。公式との差ではなかったためv37を撤回する。
  B1／rawへは進まない。

#### 第16回: 1 packet QTD×8を1 descriptor listで実行（v38）

公式Bulkは512 byteを1 QTDへ載せ、hardwareにMPS packetへ分割させる。一方v36／v37は、この
forced-FS hub構成では長いQTD自体がstatus 1／不可能残量になった。従来の64-byte QTDはB2通常試験で
安定しているが、各QTDのたびにchannelをhalt／rearmする。v38は両者の中間として、512-byte WRITE
dataを64-byte QTD 8個へ分け、1つの512-byte-aligned listへ連続配置する。最後のQTDだけHOC／EOL、
HCTSIZはSCHED_INFO=0xFF、NTD=7、HCCHAR MC/EC=0とし、channel activationはdata phase全体で1回。

これにより各descriptorのxfer sizeとfailure境界は1 packetのまま、packet間のchannel再起動だけを
除去できる。list途中で失敗した場合は受理済みprefixを安全に再現できないためlist全体を再送せず、
BOT Reset Recoveryへ進む。失敗ログはQTD index／control、HCINT／HCCHAR／HCTSIZ／HCDMAを出す。
起動markerは`USB TEST: WRITE data packet-list v38`、counterは`write-data-qtd list+N`。

第16回の判定:

- B2で`usbcheck 100 1`だけを実行し、READ 100/100、WRITE／復元10/10、list>0を確認する。
- failure時HCTSIZが`0x000007FF`またはPID込み`0x400007FF`なら8-entry listのprogramming発火を
  確認できる。同じFAILならlist案もNo-Goとして従来1 packet／1 activationへ戻す。
- B2が全PASSした場合だけraw、次にB1へ進む。filesystem testは使わない。

第16回の実機結果（v38、No-Go）:

- B2 READは100/100だが、WRITE／復元は0/10。10回すべてdata OUTのQTD 0がstatus 1で停止した。
- QTD 0 controlは毎回`0x10018889`（status 1、不可能残量100,489）、HCINTはChHltdだけの
  `0x00000002`。HCTSIZは`0x000007FF`／`0x400007FF`でNTD=7、HCCHARはMC/EC=0の
  `0x00C81040`となり、狙ったlist設定は確実に発火した。
- HCDMAはQTD 1を指す`base+8`で停止した。v38はlistを再送しない契約なので、B2で通常は回復する
  最初のpacket errorを一度もretryせず全WRITEを失敗させた。raw／B1へは進まない。

#### 第17回: 検証済みQTD境界からの安全なlist再開（v39）

v38で長いQTDとは異なり、失敗したpacketのdescriptor indexと未実行suffixを分離できた。
status 1になったQTD自身の残量は引き続き信用しない。一方、失敗QTDより前の各controlが
Active=0・status=0・remaining=0で、それより後のcontrolがsubmit時とbit単位で一致する場合は、
完了prefixの境界だけはdescriptor列から証明できる。

v39はこの条件を満たす場合だけ、完了prefixを除いた新しいlistを別の512-byte slotへ作り直す。
開始DATA PIDは完了したpacket数だけ進め、失敗packetは同じPIDで再送する。これはACK喪失時も
device側toggleが重複packetを捨てる1 packet retry契約と同じで、完了prefixをbusへ再送しない。
後続QTDが1 bitでも変更されている、prefixが全量完了でない、またはchannel timeoutの場合は
安全な境界が無いものとして従来どおりcommandを失敗させる。retryはpacketごとに最大20回、
各回の前にreported OUT error用NPTX cleanupと50 ms待機を行う。

起動markerは`USB TEST: QTD-list safe resume v39`。B2で`usbcheck 100 1`だけを実行し、
READ 100/100、WRITE／復元10/10、`pkt-retry err`と`write-data-qtd list`が増えることを確認する。
曖昧境界拒否、timeout、BOT Recoveryが出た場合はNo-Go。全PASSの場合だけB2 rawへ進む。
filesystem testは使わない。

第17回の実機結果（v39、No-Go）:

- B2 READ 100/100、pattern WRITE 10/10。QTD 0のstatus 1は検証済みprefix 0からの同一PID再送で
  回復し、書いたpatternもFUA readで一致した。安全な境界判定と再送は意図どおり発火した。
- 1回だけQTD 1がstatus 1となり、完了prefix 64 byteを除いてDATA PIDを進めた448-byte listから
  再開した。data OUT自体は完了したが、続くCSW INが5試行ともhaltせず、BOT Recovery後の原本復元は
  失敗した。最終結果はWRITE 10/10、復元9/10、RESULT FAIL。
- packet error 33件、timeout 5件、list activation 53回で、従来1 packet QTDのB2正常系より
  明確に悪化した。list方式は根本対策にならないためv36〜v39をまとめて撤回し、raw／B1へ進まない。

#### 第18回: ESP-IDF既定のbalanced FIFO分割へ一致（v40）

packet／QTD単位の差を撤回したうえで、ESP-IDF v5.5.3のcore初期化とレジスタ単位で比較した。
DMA mode、AHB SINGLE、UTMI+ 16-bit、timeout calibration、descriptor DMA、SCHED_INFOは一致している。
残る明確なglobal設定差はFIFO分割だった。

ESP32-P4の`GHWCFG3`はusable FIFOを896 linesと報告する。従来コードは独自の半分＋残り二分割で
RX/NPTX/PTX=`448/224/224`としていた。ESP-IDF既定のbalanced設定はHigh-Speed DWCのsynthesis
depth 1024を基準にNPTX=1/4、PTX=1/8を確保し、usable remainderをRXへ割り当てるため
`512/256/128`となる。v40はこの値へ合わせ、複数QTD listは使わず従来の1 packet QTDへ戻す。

起動markerは`USB TEST: ESP-IDF balanced FIFO v40`。`usbcheck`のhost行には実レジスタから読んだ
`fifo=512/256/128`（RX/NPTX/PTX）を追加する。まずB2で`usbcheck 100 1`だけを実行し、READ 100/100、
WRITE／復元10/10へのrollbackとFIFO値を確認する。全PASSならB2 raw、次にB1 rawへ進む。
filesystem testは使わない。

第18回の実機結果（v40、FIFO A/B未成立）:

- B2はREAD 100/100、WRITE／復元10/10、collateral 0でRESULT PASS。QTD list撤回後の
  1 packet QTD経路へ正常にrollbackできた。packet error 34件は全件同一PID retryで回復し、
  timeout／BOT Recoveryは0だった。
- 一方、host行は`fifo=512/1024/1024`だった。field位置は公式register定義と一致しており、
  `configure_fifos`の書込み後に行うroot-port resetがFIFO size registerをhardware既定値へ戻していた。
  したがってv40のPASSはFIFO変更の効果ではなく、FIFO仮説はまだ未試験。raw／B1へ進まない。

#### 第19回: root-port reset後にbalanced FIFOを再適用（v41）

ESP-IDF v5.5.3の`hcd_port_command(RESET)`経路はreset recovery後、channel／periodic設定の前に
`usb_dwc_hal_set_fifo_config`を再実行する。v41も同じ順序にし、core初期化直後のFIFO書込みを削除して、
root portがenableされた直後かつchannelが1本もactiveでない`finish_port_enable`で
RX/NPTX/PTX=`512/256/128`を書き、全FIFOをflushしてからdescriptor DMAを有効にする。

起動markerは`USB TEST: post-reset balanced FIFO v41`。まずB2で`usbcheck 100 1`だけを実行し、
host行が`fifo=512/256/128`であること、READ 100/100、WRITE／復元10/10、collateral 0を確認する。
FIFO値が違うかRESULT FAILならNo-Go。両方を満たした場合だけB2 rawへ進む。filesystem testは使わない。

第19回のB2 `usbcheck`結果（v41、Go）:

- host行は`fifo=512/256/128`で、reset後の再適用が実レジスタに反映された。
- READ 100/100、WRITE／復元10/10、collateral 0、RESULT PASS。cache拒否、packet failure、
  retry、impossible remainder、timeout、BOT Recovery、CSW違反はすべて0だった。
- v40では同じB2でpacket error 34件をretryしていたが、v41では0件になった。balanced FIFOの
  実適用は少なくともB2の反復WRITEを明確に改善した。
- 続くB2 `usbrawcheck 1 32 1`もWRITE 32/32、pattern match、snapshot restoredでRESULT PASS。
  READを挟まない連続WRITEでも安定したため、次は同じbinaryのB1 `usbcheck 100 1`へ進む。

第19回のB1結果（v41、Go）:

- host行はB1でも`fifo=512/256/128`。`usbcheck`はREAD 100/100、WRITE／復元10/10、
  collateral 0でRESULT PASSだった。
- cache拒否、packet failure、retry、impossible remainder、timeout、BOT Recovery、CSW違反は
  すべて0。続く`usbrawcheck 1 32 1`もWRITE 32/32、pattern match、snapshot restoredでPASSした。
- 同じB1 rawはv29〜v35で3/32（v35の250 ms条件では0/32）から先へ進めなかった。
  packet処理を1 packet QTDへ戻したまま、root-port reset後のFIFOだけを公式値へ直して
  32/32になったため、旧故障の根本原因は不正なFIFO設定だったと判断する。
- B1／B2のrawが安定する条件を満たした。filesystem試験は全接続matrixのraw確認後まで保留し、
  先にtopology非依存の`usbcachefail`を1回実行する。

第19回の故障注入結果（v41、再列挙harness No-Go）:

- [1/3] cache同期拒否は転送開始前に検出され、READは失敗し、宛先512 byteはsentinelのままだった。
  3 gateはすべてPASSし、故障注入対象の契約自体は成立した。
- 意図したcommand failure後のBOT Resetはcontrol IN statusのQTD status 1で失敗し、sessionを正しく
  引退させた。続く自動`rescan (recovery)`ではdownstream MSCの最初の8-byte device descriptorが
  QTD status 1となり、MSCを再取得できなかった。このため[2/3]と[3/3]は未実行でRESULT FAIL。
- 失敗はmedia I/Oではなく注入間のtest harnessにある。実機で一過性の再列挙失敗が起き得るのに
  `usb_fault_reset`がrescanを1回しか行わず、直ちに残りをskipしていた。

#### 第20回: 故障注入間のbounded rescan retry（v42）

`usb_fault_reset`はreadyなMSCが無い場合、full recovery rescanを最大3回行う。2回目以降は500 ms
待ってから再試行し、各回でroot port／hub／downstream deviceを最初から組み直す。healthy sessionで
開始する最初の注入や、注入内容、BOT／HCDの通常転送・回復経路は変更しない。

起動markerは`USB TEST: fault-rescan retry v42`。B1またはB2のどちらかで`usbcachefail`を1回実行し、
[1/3]〜[3/3]の全gateと最終RESULTがPASSすることを確認する。再列挙retryログは許容する。
全注入後のsessionは設計どおり引退するため、結果採取後に手動`usbrescan`を1回実行する。

第20回の実機結果（v42、Go）:

- [1/3] cache同期拒否はcache failureとして数え、READを失敗させ、宛先bufferへ1 byteも公開しなかった。
- [2/3] stale completionはtokenとpacket failureの両方で数え、READを失敗させ、宛先を変更しなかった。
- [3/3] short OUTは8 byte要求に対する7 byteとして検出し、commandを成功扱いしなかった。
  全9 gateと最終RESULTがPASSした。
- [1/3]後と[2/3]後の自動rescanはいずれも1回目でMSC／keyboard／mouseを再取得した。
  最後の手動`usbrescan`も同じ3 deviceを取得し、故障注入後のsession引退から正常復帰した。
- topology非依存の故障注入は完了。次はv42のままC1（High-Speedハブ＋HID＋媒体1）で
  `usbcheck 100 1`から接続matrixを再開する。filesystem testはまだ使わない。

第20回のC1 `usbcheck`結果（v42、Go）:

- host行は`High-Speed bulk-in-mps=512 fifo=512/256/128`。reset後のbalanced FIFOは
  High-Speed構成でも維持された。
- READ 100/100、WRITE／復元10/10、collateral 0、CSW契約違反0でRESULT PASS。
- 開始付近のCSW INが2回halt timeoutになったが、各packetは同一PIDの1回再送で回復した。
  command retry、BOT Recovery、data mismatch、unsafe resubmit、impossible remainderは0で、
  `usbcheck`の分類どおり許容するtransport NOTEである。
- 次は同じC1で`usbrawcheck 1 32 1`を実行する。filesystem testは使わない。
- C1 `usbrawcheck 1 32 1`はWRITE 32/32、pattern match、snapshot restoredでRESULT PASS。
  C1を完了し、次は同じHigh-Speedハブ構成で媒体だけSonyへ替えたC2の`usbcheck 100 1`へ進む。

第20回のC2 `usbcheck`結果（v42、Go）:

- host行は`High-Speed bulk-in-mps=512 fifo=512/256/128`。
- READ 100/100、WRITE／復元10/10、collateral 0でRESULT PASS。cache拒否、packet failure、retry、
  unsafe resubmit、impossible remainder、timeout、BOT Recovery、CSW違反はすべて0だった。
- 次は同じC2で`usbrawcheck 1 32 1`を実行する。filesystem testはまだ使わない。
- C2 `usbrawcheck 1 32 1`はWRITE 32/32、pattern match、snapshot restoredでRESULT PASS。
  C2を完了し、次はHigh-Speed直結A1の`usbcheck 100 1`へ進む。

第20回のC構成まとめ:

- C1／C2とも`usbcheck 100 1`と`usbrawcheck 1 32 1`がPASSした。High-Speed bulk MPS 512、
  FIFO 512/256/128、WRITE／復元10/10、raw 32/32、pattern一致、snapshot復元を両媒体で確認した。
- `fswritetest`はtransport故障の再現・判定をrawへ移したため、Stage 3完了条件から外す。

第20回のA構成まとめ:

- A1／A2ともv42の`usbcheck 100 1`と`usbrawcheck 1 32 1`がすべてPASSした。
- これで2媒体×High-Speed直結、Full-Speed固定ハブ＋HID、High-Speedハブ＋HIDの6構成が完了した。
  `tab5-bot-protocol`のhost test 7件、実機のBOT／CSW gate、raw WRITE、故障注入を含むStage 3の
  完了条件を満たした。
- 正常READ 16回ごと／WRITE直前の予防cleanupはまだ有効である。次回はStage 4のRecovery APIと
  cleanup失敗伝播から着手し、Stage 5／6で予防cleanupを段階的に外す。Stage 4着手前で中断する。

### Stage 4: Recovery APIとcleanup失敗の伝播

**進捗: 未着手。**

- 正常packet完了、packet failure回復、BOT Reset Recovery、controller／port resetを別APIへ分ける。
- TX／RX FIFO flushを`Result`化し、timeoutしたFIFO名を保持する。
- 実失敗後のRecoveryでflushが失敗した場合はMSC sessionを使用不能にする。
- 暫定的なproactive cleanupでflushが失敗した場合は後続READ／WRITEを開始しない。
- periodic HIDがarm中でRX FIFOをflushできない場合を「全cleanup成功」と表示しない。
  実行したFIFOとskip理由をcounterへ分ける。
- `consecutive_recoveries`を正常な予防処理で0へ戻さず、実失敗と実回復だけで更新する。

完了条件:

- FIFO timeout注入時にWRITEを開始せず、成功を返さない。
- 実失敗後だけBOT Reset Recoveryが動き、正常command数だけではdevice-facing resetしない。
- HID併用時のfailure recoveryが無関係なHID sessionを再列挙しない。

### Stage 5: READ前cleanupの撤去

**進捗: 未着手。Stage 1〜4完了前に実施しない。**

一時的な診断build設定でREAD cleanup間隔を`16`、`32`、`disabled`から選べるようにし、同じ
binary系列・同じ媒体・同じ接続順でA/Bする。恒久的なuser optionにはしない。

試験順（各行は`usbcheck <reads>`1コマンド。書き込みを伴う行だけLBAを付ける）:

1. `16`でStage 0 baselineが維持されることを確認する。
2. `32`で最短33回故障の旧境界を越える。
3. `disabled`でHigh-Speed直結`usbcheck 1000`。
4. `disabled`でFull-Speed固定ハブ＋HID＋MSCの`usbcheck 1000`。
5. `disabled`でHigh-Speedハブ＋Low-Speed HID＋High-Speed MSCの`usbcheck 1000`。
6. `mix`既定120分と、ファイルシステムの大きな連続read。

Go条件:

- failure／mismatch／cache refusal／stale completion／CSW tag mismatchが0。
- proactive READ cleanupが0。
- command retryが0。packet retryが発生した場合はHCINTとactualが説明でき、同じbyteを二重公開しない。
- 試験後もHID入力、`usbinfo`、TEST UNIT READY、READ CAPACITY(10)が通る。

No-Goなら単に16へ戻して完了扱いしない。最初の失敗snapshotをStage 2または3の契約違反として
分類し、根本原因を直して同じStageを再実行する。

### Stage 6: WRITE前cleanupの撤去

**進捗: 未着手。Stage 5完了前に実施しない。**

WRITEは失敗後に安全に再送できないため、READより後に行う。`MAX_WRITE_BLOCKS = 1`を維持し、
cleanup有効／無効だけをA/Bする。

- 犠牲にできるLBAでHigh-Speed直結`usbcheck 100 <LBA>`を10回（write 100回）。
- Full-Speed固定ハブ＋HID＋MSCで同じ試験。
- High-Speedハブ＋Low-Speed HID＋High-Speed MSCで同じ試験。
- 最低2メーカーの犠牲にできるFAT媒体で`fswritetest /vol/usb0pN 1 1`を10回連続実行する。
- 各実行後にFUA照合、周辺LBA照合、原本復元を確認する。
- 最終的にPCで全ファイルを読み、`fsck.fat -n`で致命的な不整合が無いことを確認する。

Go条件:

- WRITE前cleanup 0で、部分転送、tag mismatch、residue矛盾、transport failure、collateral changeが0。
- 失敗したWRITEを一度も自動再送していない。
- SYNCHRONIZE CACHE非対応deviceの既存best-effort方針とFUA照合が維持される。
- 試験後も同一バスのHIDが動作する。

Goなら`maintain_command_boundary`、`reads_since_resync`、方向別proactive counterと成功ログを削除する。
`usbcheck`の`delta proactive`行はcounter削除に合わせて落とす。
No-Goなら古いCSWが再び見えたphaseとHCD snapshotを保存し、cleanupを戻しただけで計画を完了に
しない。

### Stage 7: 複数ブロックWRITEの再評価

**進捗: 未着手。cleanup撤去の完了条件には含めない。**

Stage 6完了後にだけ、決定論的だった複数ブロックWRITEを診断buildで再評価する。

1. 2 block WRITE(10)を同じLBA窓で10回。
2. 4 block、8 blockへ段階的に増やす。
3. data OUT各packetのactual、PID、最終CSW residueを記録する。
4. 失敗したcommandは再送せず、媒体全体を別ホストで検査する。

8 blockまで複数media／複数topologyで通った場合だけ`MAX_WRITE_BLOCKS`を増やす。失敗する場合は
1 block制限をdevice相互運用上の独立した既知問題として残す。cleanup撤去に成功していても、
このStageのNo-Goは前Stageを自動的に取り消さない。

### Stage 8: 診断整理、現状文書、回帰

**進捗: 未着手。**

- 正常系のproactive cleanupログとcounterを削除する。
- `hcd.rs`に`#[cfg(any())]`で無効化されたまま残っている旧failure snapshot（約200行、
  存在しないstaticを参照している）を削除する。Stage 0で似た名前のcounterを新設したため、
  読む人が取り違える形になっている。
- cache refusal、impossible actual、invalid CSW、Recovery失敗のcounterは異常診断として残す。
- `recover_channel_after_packet_failure`を実際のfailure pathだけを表す名前と責務へ整理する。
- [`USB.md`](USB.md): 正常BOT境界、actual length、Recovery条件、HCD buffer契約。
- [`STORAGE.md`](STORAGE.md): proactive cleanup撤去、READ再送／WRITE非再送、複数block上限。
- [`DIAGNOSTICS.md`](DIAGNOSTICS.md): 新しいfailure counterとログの読み方。
- [`KNOWN_ISSUES.md`](KNOWN_ISSUES.md): cache alignmentとcleanup回避の解消／残件。
- [`USB_WRITE_STABILITY_PLAN.md`](USB_WRITE_STABILITY_PLAN.md): 各A/Bの実機結果と否定した仮説。
- [`../DESIGN.md`](../DESIGN.md): 各WRITE前cleanupを必要とする制約を、実装結果に合わせて更新する。

静的確認:

- 変更ファイルの`rustfmt --check`。
- host-testable BOT workspace memberの`cargo test`。
- `cargo check -p tab5-hello-world --release`。
- release ELFでQTD、frame list、DMA stagingの配置とalignmentを検査する。
- `cargo clippy`のUSB correctnessに関係する警告を確認する。
- `git diff --check`。
- `README.md`にこの作業による差分が無いこと。

## 実機試験matrix

| ID | 接続 | 主な目的 |
| --- | --- | --- |
| A | High-Speed MSC直結 | hub／Splitを除いたchannel 0、MPS 512 |
| B | `usbfs on`、Full-Speedハブ＋HID＋MSC | MPS 64、serialized HID併用 |
| C | High-Speedハブ＋Low-Speed HID＋High-Speed MSC | Split HIDとHigh-Speed bulkの共存 |
| D | A〜Cを別メーカーMSCで反復 | device固有quirkとの切り分け |

各Stageで変更していない条件も省略せず記録する。特にspeed、MPS、FUA、LBA、block数、媒体VID/PID、
HID periodic／fallback、cleanup設定が違う試験を同じ結果としてまとめない。

## 中止条件とロールバック

- data mismatch、範囲外LBA変化、部分WRITE成功扱いを1回でも観測したら、そのStageをNo-Goにする。
- WRITE transport failure後は自動再送せず、MSC sessionと該当mountを使用不能にする。
- channel halt不能、FIFO flush timeout、EP0を含むdevice無応答はcontroller／session故障として
  既存の隔離・明示rescan方針へ戻す。
- HID停止や他portの列挙失敗をMSC安定化の代償として受け入れない。
- rollbackは直前Stageのcleanup設定へ戻すだけにし、採取したraw snapshotとcounterを消さない。
- cleanupを戻せば通る場合も、根本原因を解消したとは記録しない。

## 完了条件

- 全DMA転送がHCD所有の整列済みbufferまたは同等に証明されたbufferを使う。
- cache maintenance拒否が転送開始前の失敗として上位へ届く。
- HCDが部分OUT、古いcompletion、hardware所有中QTDを成功扱いしない。
- BOTがCBW／data／CSWのactual、tag、status、residueを整合させる。
- Phase Errorとinvalid CSWでReset Recoveryを行い、正常command回数では行わない。
- READ前とWRITE前のproactive host cleanupがコードと正常ログから無くなる。
- Stage 5、6の全実機matrixをcleanup無しで通る。
- READだけを安全に1回再送し、WRITEを自動再送しない方針を維持する。
- 実装変更と同じ作業で現状文書を更新し、調査履歴は計画書へ残す。
