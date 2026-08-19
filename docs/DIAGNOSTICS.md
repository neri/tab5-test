# 診断ログ

> 索引: [`../DESIGN.md`](../DESIGN.md)

正常時の主要な通過点は次のとおりです。

```text
RAM: L2 cache bytes=0x...
RAM: usable top=0x...
RAM: stack top=0x...
XIP: pre-PSRAM DROM probe start
XIP: pre-PSRAM IROM probe start
XIP: pre-PSRAM DROM+IROM ok
DMA2D: version=0x02304110
PPA: version=0x02304041
PPA: clocked, out of reset, registers verified
PSRAM: ready (framebuffer + heap)
XIP: post-PSRAM DROM probe start
XIP: post-PSRAM IROM probe start
XIP: post-PSRAM DROM+IROM ok
LCD: D-PHY 4/4 ready
LCD: DCS init complete
ICM: clk_en=0x...
ICM: master priority=0x...
ICM: master arqos=0x...
ICM: master awqos=0x...
LCD: DMA 3/3 full-frame interrupt installed
LCD: RGB565 framebuffer DMA active
```

`ICM: master priority`と`ICM: master arqos`は、DW-GDMAの2ポート分
（bit 12〜19）が`F`になっていれば書き込みが効いています。ここが`0`のままなら、
レジスタ自体が書けていません。`ICM: master awqos`は書き込み側の診断値です。
表示DMAはPSRAMを読み出すだけなので、DW-GDMAのbit 12〜19を変更しません。
`LCD: DPI FIFO underrun ...`が出た場合は、そのフレームのパネル表示が水色に
なっています（[`DISPLAY_BANDWIDTH.md`](DISPLAY_BANDWIDTH.md)を参照）。

SDカード関連は起動シーケンスに含まれず、シェルコマンド（`sdinfo`/`sdread`/
`sdreadn`/`sdwritetest`/`sdzero`）実行時にのみ`SDMMC: ...`という接頭辞で
UARTへ出ます。正常時は`SDMMC: card activated`の後にCID/CSDの生値が続きます。
対応範囲は[`STORAGE.md`](STORAGE.md)、失敗パターンの詳細は
[`SD_CARD_PLAN.md`](SD_CARD_PLAN.md)を参照してください。

USB-AホストはLCDとCardKBの初期化後に起動し、最初の`UsbHost::rescan`を実行します。
そのため、起動時にも列挙結果や`USB: initial scan complete`がUARTへ出ます。その後も、
ルートポートの切断・再接続、空いているハブポートの増分スキャン、トランザクションエラーからの
復帰時に`USB: ...`ログが出ます。`usbinfo`/`usbhub`/`usbmsc`等は共有レジストリを使い、
`usbrescan`だけがユーザー操作でバスの再列挙を行います。`usbvbus`はI/O expanderの出力ビットを
直接変更する診断用コマンドです。対応範囲は[`USB.md`](USB.md)、段階分けと未確定事項は
[`USB_HOST_PLAN.md`](USB_HOST_PLAN.md)と
[`USB_INTERRUPT_REFACTOR_PLAN.md`](USB_INTERRUPT_REFACTOR_PLAN.md)を参照してください。

`usbhw`はSplit Transactionのレジスタに加え、USB割り込みのsource、global enable、
総ISR回数、channel 0／periodic channel 1〜4／root-port／spurious回数、`GINTMSK`／`HAINTMSK`／`HCINTMSK0..4`、
最後に採取した`GINTSTS`／`HAINT`／`HCINT0`／`HPRT`を表示します。さらにStage 2の
sleep wait／poll wait／実行した`WFI`の回数とlast／max wait cyclesを表示します。通常の
control／bulkを使った後はsleepとWFIが増え、periodicへ昇格できないHIDのidle待ちではpollが増えるのが正常です。
`sleep`は待機したpacket数、`wfi`は途中で別割り込みに起こされて再度眠る場合も含む命令回数
なので、後者が前者より多くても異常ではありません。HIDでもキー報告を受信してchannelが
haltした時はchannel IRQが増えます。常設periodic HIDのidle NAKはcontrollerが処理するため、
前景のpoll／submit／cancelは増えません。
`IRQ slots`は原則として`submit = reap + cancel`です。直結HIDのidle NAK timeoutはcancelへ
入るため、キーを押さずに待つほどreapよりcancelが多くなります。観測瞬間に1件実行中なら
submitが右辺より1だけ多い場合があります。`stale-token`は常に0が正常です。Splitは別の
buffer DMA経路なのでこのslot集計には含みません。
Stage 1の設定値は
source 93、`GINTMSK=0x23000000`、`HAINTMSK=0x00000001`です。
`HCINTMSK0`へは`0x00003FFF`を書き込みますが、descriptor DMA中の実機read-backは
`0x00002807`でした。正常な転送後はchannel 0の回数が増え、
`unknown-cause`は0のままです。spuriousは前景のfallback読出しがISRより先に完了を
回収した競合でも増え得ますが、操作を止めても増え続ける場合は割り込み嵐です。

`usbperiodic`は最初のHIDに対してchannel 1のperiodic Interrupt INを最大5秒だけ試します。
実行直後にキーを押す／離すかマウスを動かします。`frame list addr/readback`は一致し、HCFGは
`PerSchedEna`と32-entry指定を含むこと、成功時は`result=complete`、`halted=1`、`ch1-irqs>0`、
HCINTにXFERCOMPL／CHHLTD、bytesがendpoint MPS以下であることを確認します。timeoutでも
`halted=1`なら診断は安全に終了しており、通常のHID fallbackを維持します。

`usbfs on`はhostをFS/LS-onlyへ切り替えてその場でroot reset／再列挙します。High-Speed hubも
root側が`speed: Full-Speed`になり、下流FS/LSデバイスをSplitなしで列挙します。通常設定へ
戻すときは`usbfs off`を実行し、同じく再列挙後に`host: High-Speed capable`を確認します。
代替実機試験ではkeyboard＋mouseに対してperiodic channel mask 6、`HAINTMSK=7`、channel 1／2の
`HCINTMSK=0x2807`、complete／rearm 340／342、poll 0、errors 0を確認しました。`usbfs off`後は
root hubがHigh-Speedへ戻り、同じ2台をSplit routeで再列挙できました。

`IRQ split`はHigh-Speedハブ配下のserialized Split fallbackについて、開始packet数、完了IRQを
受けたSSPLIT／CSPLIT round数、periodic DMAとの排他conflict数、現在mode中かを表示します。
通常の静止時snapshotは`conflicts=0 active=0`です。
実機のkeyboard＋mouse回帰ではpackets 4745、rounds 58727、sleep／WFI 59188／59149、poll 0、
conflicts 0、snapshot時active 0でした。roundがpacketより多いのはTTが各packetをSSPLIT／
CSPLITの複数phaseで処理し、idle HIDのNAKも安全境界まで回収するためです。
さらに同じHigh-Speed hubの別portへSony USBメモリ（`054C:0243`）を接続し、3824 MiBのcapacity
取得とLBA 0の512-byte読出し、末尾`55 AA`を確認しました。転送後もsplit conflicts 0、active 0、
poll／cancel／stale-token／unknown cause 0で、submit／reapは353／353でした。

常設periodic HIDが有効なら起動ログに`USB HID: periodic channel enabled: N`、`usbhw`に
`IRQ periodic: channels=0x..`が出ます。bit Nがchannel Nの割当てを表します。reportごとに
`complete`と`rearm`が同数ずつ増え、
`errors=0`が正常です。起動後にキーを押さず数秒待って2回`usbhw`を比較したとき、`IRQ waits`の
`poll`と`IRQ slots`のsubmit／cancelが増えないことがidle CPU poll除去の判定です。
root直結LS keyboardの実機確認値はchannel mask 2、complete／rearm 26／27から38／39へ進み、その間poll 0、
submit／reap／cancel 30／30／0が不変、errors 0でした。
4-slot化後の再確認でもLS keyboardはchannel 1 IRQ 34、complete／rearm 33／35、Full-Speed mouseは
channel 1 IRQ 254、complete／rearm 253／256でした。いずれもchannel mask 2、poll 0、
submit=reap、cancel／errors／spurious／stale-token／unknown causeは0でした。rearmがcompleteより
複数多いのは、同じ起動中に接続・再接続した各HIDの初回armも累積カウンタへ含むためです。

`battery`実行時は、検出したINA226のI2Cアドレスを`Battery: INA226 found at I2C address=0x...`
として出力します。初期化できない場合は`Battery: INA226 identity read failed`または
`Battery: INA226 configuration write failed`、動作中の一時読出し失敗は
`Battery: INA226 read failed; retaining last reading`を出力します。

主な失敗ログ:

- `CPU: unexpected boot clock source, staying at 90 MHz`: ブートローダーがCPLL/4以外の経路でCPUを構成した（分周比を書き換えず90 MHzのまま継続）
- `RAM: stack top is inside the L2 cache area`: `memory.x`の`RAM`範囲が広すぎる
- `MEM: .data/.bss initialization failed`: bootloaderのRAMロードまたはゼロ初期化が不正
- pre側の`XIP: DROM probe failed`／`XIP: IROM probe actual=...`: bootloaderによる初期FLASHマッピングが不正
- post側の同ログ、またはpost probe途中の停止: PSRAM/MSPI初期化後にFLASHデータまたは命令取得へ復帰できない
- `PSRAM: mode-register transaction failed`: MSPI3コマンド経路
- `PSRAM: no valid DQS phase`: DQS位相調整
- `PSRAM: mapped memory test failed`: MMUまたはキャッシュ経路
- `LCD: PI4IOE1 reset control failed`: ソフトウェアI2CまたはI/O expander
- `LCD: D-PHY lock timeout`: D-PHY電源、クロック、PLL
- `LCD: DCS FIFO timeout`: パネルコマンド経路
- `LCD: DMA interrupt error`: DW-GDMA転送
