# USB 割り込み駆動リファクタリング計画

> 索引: [`../DESIGN.md`](../DESIGN.md) ／ 現在のUSB仕様: [`USB.md`](USB.md)
>
> この文書は作業計画と実機での判断記録です。現在の実装仕様は現状文書とコードを
> 優先します。

## 状態: Stage 0〜5とroot接続event完了、hub statusは低頻度fallbackを維持

2026-08-20時点で、ESP-IDF v5.5.3のローカル参照実装からHigh-Speed DWCの割り込み
ソースが`ETS_USB_OTG_INTR_SOURCE = 93`であること、DWC側のmask／ack手順を確認した。
共有CLIC入口、USB ISR、Atomicスナップショット、従来ポーリングを残した完了回収、
`usbhw`診断まで実装し、`cargo check`、releaseビルド、ELF配置検査に通過している。

同日の実機確認で、root直結LSキーボードの起動直後の入力、source 93からのchannel／port
割り込み、unknown cause 0を確認した。一方、切断後の再接続はISRが採取したport eventを
`InputManager`が消費しておらず、従来の300フレーム周期のfallback scanに残っていた。
port eventを前景へ渡し、物理接続変化で即時`rescan`する修正を実機で再確認した。最初の
接点bounceはdebounceで棄却し、次のconnection eventで直ちに再列挙してキー入力まで復帰した。
90秒のidle中にLCD underrunは発生せず、画面へ大量表示した`usbinfo`／`usbhw`時だけ増えたため、
USB ISR追加の回帰ではなく既知の表示帯域問題と判断した。

Stage 2の第一段として、通常のcontrol／bulkとsoftware Splitのchannel haltを割り込みで待つ
WFI互換ラッパーを実装した。直結HIDのidle NAKはdescriptor DMAがhaltを発生させないため、
短い従来pollを明示的なfallbackとして残している。MSCではsleep 18、WFI 28を実機確認した。
一方、control転送のquiet診断指定までHID fallbackと同一視していた不具合が見つかったため、
待機方式を`CompletionWait`でログ抑制から分離した。修正後はLS keyboard列挙でもsleep 30、
WFI 31を確認した。続いて固定長transfer slot、世代token、submit／reap分離を実装し、HIDと
MSCの実機回帰にも通過した。次は通常HIDをまだ切り替えず、channel 1と32-entry frame listを
一時的に使う`usbperiodic`診断もroot直結LS keyboardで成功した。現在はこの実証済み設定を
単一直結keyboardの常設経路へ昇格し、idle時にchannel 0 pollが完全に止まることも実機確認した。
その後channel 1固定を4-slot allocatorへ一般化し、複数keyboard／mouseを独立channelで扱う
実装まで完了した。root mouseもGoとなり、残るGo/No-GoはFull-Speedハブ配下の複数HIDである。

## 背景

現在のUSB-AホストはDWCコアのdescriptor DMAを使うが、転送の起動から完了までをCPUが
同期的に待つ。`src/usb/hcd.rs`の`run_packet`はチャネル0を起動し、`await_packet`が
`HCINT.CHHLTD`をビジーループで読む。DMAはデータを運んでも、完了待ち・NAK待ち・次の
転送の起動はCPUが占有する。

計画開始時、HID BootのInterrupt INエンドポイントもperiodic schedulerを初期化していないため
`HCCHAR.eptype=BULK`としてフレームごとに疑似ポーリングしていた。現在は対象routeを
periodic channel 1〜4へ昇格し、割当て不能／Split routeだけこのfallbackを使う。

一方DWCコアはホストチャネルの完了・エラー、ルートポート変化を`GINTSTS`／`HCINT`で
通知できる。CPU側のCLICは現状`src/interrupts.rs`で表示用DW-GDMAだけを処理している。
USBを同じトラップ入口に追加すれば、少なくとも完了するまで状態を読み続ける処理をなくせる。

periodic schedulerとframe listはroot直結LS keyboardで実証済みである。ただしHSハブ配下のFS/LS
デバイスは、現行でもdescriptor DMAを一時停止してbuffer DMAのSplit Transactionを実行
する。これはコントローラ全体のDMAモードを切り替える制約であり、単にチャネル数を増やす
だけでは安全にならない。

## 目的と完了像

USB転送を「CPUが完了を待つ関数呼び出し」から、**コントローラへ投入して割り込みで完了を
回収する状態機械**へ移す。

- 通常のcontrol／bulk／HID転送は、完了待ちのビジーループを使わない。
- HID Bootキーボード・マウスは、実機確認が取れた場合にのみ本来の`INTR`型とperiodic
  schedulerを使う。確認前に現行のBULK代用を削除しない。
- ISRは完了状態を記録して起床させるだけにし、列挙、クラス解析、UART出力、I2C、DMA
  モード切替、メモリ確保を実行しない。これらは前景の`UsbHost`が行う。
- 既存の直結、1段ハブ、HSハブ配下のFS/LS Split Transaction、MSC読み出しを回帰させない。
- ルートポートの接続変化は、検証後にポーリングではなくポート割り込みで検出する。

RTOSや並行タスクは導入しない。USBバスの唯一の所有者は引き続き`UsbHost`であり、前景は
表示フレーム境界とUSBイベントのどちらでも仕事を進める。

## 対象外

- USB-C側のFull-Speed OTG、USB Serial/JTAG、isochronous転送。
- HID非Bootレポート解析、複合デバイス、多段ハブ、MSCのWRITE(10)、ファイルシステム。
- ISRからクラスドライバを直接動かす設計、RTOS、動的な転送キュー。
- periodic frame listを使う前に、現行で動くBULK代用を破棄すること。

## 設計方針

### ISRは通知だけにする

`src/interrupts.rs`を表示DMAとUSBの両方をディスパッチできる共通入口へ拡張する。USB ISRは
DWCの有効なグローバル／チャネル割り込みを読み、該当ビットをW1Cし、チャネルごとの完了
スナップショットとポート変化フラグを`AtomicU32`へORするだけに留める。イベント連番も
加算し、`WFI`直前・直後の競合で通知を取り逃がさない。

前景の`usb::hcd`が状態をtakeし、QTDや`HCTSIZ`のDMA書き戻しをキャッシュ同期してから
成功・NAK・STALL・エラーを判定する。USB ISRは次のQTDを投入せず、クラス状態にも触れず、
ログも出さない。表示ISRと同じくIRAM常駐かつ短い処理に保つ。

### 固定長スロットの状態機械にする

`hcd`に、ハードウェアチャネル数以下の固定長`TransferSlot`配列を置く。各スロットは
`Idle`、`Armed`、`CompletionPending`、`Cancelled`などの状態、チャネル番号、宛先、
QTD／buffer DMA情報、開始フレーム、期限、結果を持つ。初期段階は**チャネル0のみ**を使い、
正しさを確認してから同時発行が安全な通常転送だけを複数チャネルへ広げる。

```rust
pub fn submit(packet: PacketRequest) -> Result<TransferToken, SubmitError>;
pub fn take_completed(token: TransferToken) -> Option<PacketOutcome>;
pub fn service_timeouts();
```

移行中は既存の列挙・BOTを一度に書き換えないため、これらの上に`run_packet`互換の待機
ラッパーを残す。ラッパーはスピンせず、対象転送または表示フレームのイベントまで`WFI`して
完了を回収する。`TransferToken`は世代番号を含め、再利用済みスロットの古い通知を読まない。
容量超過は`Busy`として返し、前景が次フレームへ繰り延べる。ヒープ、`dyn`、ISR内ロックは
使わない。

### Split Transactionを排他モードとして守る

HSハブ配下のFS/LS転送は、現行どおりbuffer DMAでSSPLIT/CSPLITを完結させる必要がある。
`HCFG.DescDMA`はコントローラ全体の設定なので、descriptor DMAの定期転送と同時には
切り替えられない。

`TransferArbiter`を導入し、Split要求時は新規通常転送の投入を止め、動作中チャネルを
完了または安全にhaltしてからSplit専用モードへ移る。CSPLITを安全境界まで回収してから
通常モードへ戻し、停止したperiodic転送を再投入する。ISRはモードを変えない。統合確認が
終わるまでは、複数チャネル化を直結・HS同速機器に限り、Split機器では安全な逐次方式へ
フォールバックする。

### periodic schedulerは独立したGo/No-Go判定にする

`HCFG.PERSCHEDENA`とframe listを有効にすれば、HIDの`bInterval`に従う`INTR`型チャネルを
コントローラ側で動かせる見込みがある。しかし現行ではframe list未設定の`INTR`が実機で
進まず、BULK代用にしている。従って「割り込みで完了を受ける」と「真のperiodic scheduling」
を別の完了条件にする。

まず非periodicのcontrol／bulkを割り込み化する。その後にframe listのアラインメント、
descriptor形式、`HFLBAddr`、周期、`HFNUM`との整合を小さな診断で測定する。成功した場合
だけHIDを`INTR`へ切り替える。失敗した場合も、割り込み完了の通常転送＋現行の周期投入は
残せるため、前段の成果を失わない。

## 段階的な実装・検証

### Stage 0: 割り込み経路とDWC完了条件の調査

**進捗: 完了。参照実装との照合と実機でのIRQ発生を確認済み。**

確認した値:

- ESP32-P4の割り込み表では`ETS_USB_OTG_INTR_SOURCE = 93`。`usb_dwc_periph.c`でも
  controller 0（High-Speed、UTMI PHY）にこのsourceを割り当てる。Full-Speed側は別source。
- DWCのglobal signalは`GAHBCFG.GlblIntrMsk`、channel集約は`GINTSTS.HChInt`→
  `HAINT`→各`HCINT`。root-portは`GINTSTS.PrtInt`から`HPRT`のchange bitを回収する。
- Stage 1では`GINTMSK=0x23000000`（disconnect、host channel、port）、
  `HAINTMSK=1`、channel 0の`HCINTMSK`へ`0x00003FFF`を書き込む。descriptor DMA
  動作中の実機read-backは`0x00002807`だったため、未実装／モード依存bitをハードウェアが
  落としている。Splitのbuffer DMAについてはStage 5まで直接読出しfallbackを維持する。

最初に通常動作を変えず、UART診断で以下を採取する。

- ESP32-P4のUSB OTG HS割り込みソース番号、割り込みルータ設定、CLIC番号・優先度。
  ESP-IDF v5.5.3のヘッダ／DWC実装と実機read-backを照合し、推測した番号を固定しない。
- `GAHBCFG`のグローバル割り込み有効、`GINTMSK`、`HAINTMSK`、各`HCINTMSK`の実装ビットと
  W1C順序。descriptor DMAとbuffer DMAの双方で完了・NAK・STALL・エラー・haltを採取する。
- root-port connect/disconnect／enable-changeの割り込みビットと、`HPRT`のW1Cが電源・
  リセット・速度を壊さない手順。
- 実機が報告するホストチャネル数（既存の読出しでは16）と、同時有効化時のDMA／FIFO制約。

完了条件は、root直結のHIDとMSCの各1転送について発生原因とDWC状態を再現可能に記録する
こと。不明点が残る場合は次Stageへ進めず、観測結果をこの文書へ追記する。

### Stage 1: 共有CLIC入口とUSB完了ISR

**進捗: 完了。割り込み経路、root再接続、LCDへの影響なしを実機確認済み。**

最初の実機結果（root直結LS HID Bootキーボード）:

- 起動直後の列挙とキー入力は成功。
- `source=93`、global enable 1、`GINTMSK=0x23000000`、`HAINTMSK=1`。
- 1回目の`usbhw`でtotal 684／channel 682／port 2／spurious 32、2回目は
  total 1349／channel 1347／port 2／spurious 56。unknown cause/countはいずれも0。
- port pendingは`0x0000000A`（connection detectとenable change）だった。切断は
  `USB: nothing connected to USB-A`として検出したが、再挿入を即時再列挙しなかった。
- 試験中にLCD FIFO underrunを3回記録した。大量の`usbhw`表示／UART出力に伴う既存の
  帯域競合かISR追加の回帰かは未判定であり、再試験でidle時の増加有無を確認する。

再接続対策として、`UsbHost::rescan`自身が生むconnect/enable eventは終了時に捨て、
それ以後の物理connection/disconnection eventだけを`InputManager::service`がtakeする。
現在接続中なら即時に全バスを`rescan`し、割り込みを取り逃した場合の300フレームfallbackも
残す構成へ変更した。再試験では切断ログ後、挿入時の最初のedgeは
`connection bounced away during debounce`となったが、続くedgeで即時再列挙し、LS Boot
keyboardを`04D9:2020`として再認識した。`usbhw`ではport IRQ 141、pending 0、unknown 0で、
接点bounce中の複数通知を最終的に回収できている。

LCDは90秒のidle中にunderrunなし。`usbinfo`と`usbhw`の画面出力時に2回発生したが、これは
従来から大量描画時に再現する既知事象と一致するためStage 1の中止条件には該当しない。

`interrupts.rs`を表示DMA専用から複数ソース対応へ変更する。既存の表示DMA処理と
`FRAME_SEQUENCE`の意味は変えない。

- `install_usb`／`uninstall_usb`相当を追加し、DWCをリセットする前にマスクを落とし、
  stale状態をW1Cしてからルータ・CLIC・DWCの順に有効化する。
- USB ISR用のIRAMコード、チャネルごとの完了bit、ポート変化bit、USBイベント連番を置く。
- 未知外部割り込みで無限ループしないディスパッチにし、最小の診断フラグを前景へ渡す。
- 前景は表示フレーム、USB完了、他の有効割り込みのいずれでも起床後に状態を再確認する。

完了条件は、USB転送をまだポーリングで実行したままでも、USBマスク有効時に表示更新・入力・
MSCが回帰せず、意図的な1転送でUSB ISRカウンタだけが増えること。ISRからのUART出力は禁止する。

### Stage 2: チャネル0の非同期化（control／bulk）

**進捗: 完了。MSC bulkとkeyboard列挙controlのWFI、submit／reap分離を実機確認済み。**

第一段では既存の同期APIとstack上QTDの寿命を変えず、channel halt待ちだけを2経路に分けた。
通常のcontrol／bulkとsoftware Splitは、`mstatus.MIE`を一時maskしてAtomic／HCINTを再確認後に
`WFI`する。これによりcheck-before-sleep競合でも通知を失わない。期限は`rdcycle`で判定し、
USB以外の割り込みで起きても期限と完了を再確認する。`usbhw`にsleep wait、poll wait、WFI回数、
last／max wait cyclesを追加した。

直結HIDのBULK代用は、idle NAKをdescriptor DMAが内部再試行してCHHLTDを出さない。この経路を
WFIにすると次の表示割り込みまで最大1フレーム眠り、入力周期を半減させるため、50,000回の
短いpollを当面維持する。異常時のforce-halt確認も同じ短いpollを使う。periodic schedulerへ
移行した時点でこのHID fallbackを削除する。

最初の実機試験結果:

- 直結LS Boot keyboardはキー入力を維持し、sleep 0／poll 3646／WFI 0だった。channel IRQは
  1786回発生しており、キー報告などchannelがhaltした時のISRは動いている。ただしidle待ちは
  設計どおりpoll fallbackである。
- HS MSCはsleep 18／poll 7399／WFI 28、last wait 1,577 cycles、max 71,142,233 cyclesで、
  control／bulkの少なくとも一部がWFIからUSB IRQで復帰した。unknown causeは0。
- keyboard列挙のcontrolまでsleep 0だった原因は、`quiet_timeout`を待機方式の判定にも使った
  ことだった。control retryは成功する最初の試行でも診断をquietにするため、全列挙packetが
  pollへ流れていた。`CompletionWait::{Interrupt, PollIdleNak}`を追加し、ログ抑制とは独立して
  control／bulkは常にInterrupt、HID reportだけPollIdleNakと指定するよう修正した。
- 分類修正後のLS keyboardではsleep 30／poll 1196／WFI 31となり、controlはWFI、HID idleは
  bounded pollという意図した分離を確認した。channel IRQ 604、port IRQ 2、unknown 0である。

次の内部構造では、unsplit channel 0用の`Channel0Transfer`に512-byte aligned QTD、状態
（Idle／Armed／CompletionPending／Reaped）、世代付き`TransferToken`をまとめた。`submit`が
cache同期とレジスタ投入を行い、IRQ待機後に`note_completion`、`reap`がQTD write-backと
`PacketOutcome`分類を行う。同期`run_packet`はこの境界を使う互換ラッパーになった。
`usbhw`のsubmit／reap／cancel／stale-tokenで世代不整合と回収漏れを確認する。

分離後の実機結果は、HIDでsubmit 550／reap 42／cancel 508、MSC試験までの累計で
submit 1426／reap 64／cancel 1362だった。いずれも`submit = reap + cancel`、stale-token 0で、
LS keyboard入力とHS MSC読出しも成功したためStage 2を完了とする。

`run_packet`を`submit`と完了回収へ分離する。最初は既存チャネル0、descriptor DMA、
非Split packetだけに限定する。

- QTD・`HCCHAR`・`HCTSIZ`・`HCDMA`の設定、cache write-backを`submit`へ移す。
- 前景がQTDをcache invalidateし、残量・QTD状態・`HCINT`から既存と同じ`PacketOutcome`を
  作る。
- 期限切れは前景で判定し、halt→halt確認→W1C→必要時FIFO flushの現行復旧順序を保つ。
- 同期API利用者はWFI型の互換ラッパーで動かし、列挙、BOTのCBW/data/CSWの順に移行する。

完了条件は、直結キーボードの列挙、直結USBメモリの`usbmsc`／`usbread 0`／`usbmbr`を
連続実行して同じ結果を得ること。`rdcycle`で旧`await_packet`のスピン回数を計測し、通常成功
経路でゼロになったことも記録する。

### Stage 3: スケジューラと複数チャネル

**進捗: channel 1〜4の複数slot allocatorまで実装・実機確認済み。root keyboard／mouseに加え、
FS強制したHigh-Speedハブ配下でkeyboard＋mouseの2 slot同時動作を確認した。**

`UsbHost::service`を、クラスドライバが直接同期転送する構成から、要求をHCDへ渡して完了を
受け取る構成へ変える。フレームごとの仕事量に上限を設け、キー・ポインタ・ハブ保守・MSCを
公平に扱う。

- キーボードとマウスに「要求中」「完了待ち」「報告消費」の状態を持たせ、同一endpointへ
  二重投入しない。
- control/BOT/hub制御は非periodic、HID候補はperiodic要求として区別する。
- 通常モードで複数の空きチャネルを使う場合、割当て・完了・timeout・cancelをスロット単位に
  する。並行数はFIFOと実機測定に基づく小さい上限から始める。
- MSCの長い処理がHIDを無期限に飢餓させない優先度を定める。ただしBOTのCBW/data/CSWを
  並列化しない。

完了条件は、キーボード＋マウス＋USBメモリでキー・マウスを継続しながらMSC読出しを行い、
完了、timeout、再初期化がデバイスごとに正しく記録されること。

### Stage 4: periodic frame listの実証とHIDの本来の`INTR`化

**進捗: root直結LS keyboard、root直結Full-Speed mouse、FS強制hub配下のkeyboard＋mouseを完了。
最大4 HID endpointへの一般化はGo。**

ESP-IDF v5.5.3のESP32-P4 HALと同じ手順を小さい診断へ移植した。512-byte alignedの32-entry
frame listへchannel 1 bitを`bInterval`から丸めた2の冪間隔で置き、`HFLBAddr`、
`HCFG.FrListEn=32`、`PerSchedEna`、`HCCHAR.eptype=INTR`、FS/LS用`SCHED_INFO=0xFF`を設定する。
channel 1のQTDはendpoint MPSちょうど1 packetで、ISRはchannel 0と独立したAtomicへ完了を
保存する。診断は最大5秒WFIし、完了・timeoutのどちらでもchannelをhaltしてperiodic設定と
frame-list addressを復元してからstack上DMA領域を破棄する。

`usbperiodic`実行中にキーを押す／離すかマウスを動かし、channel 1 IRQ、HCINT、QTD write-back、
転送byte数を表示する。ESP-IDF HALには「LS endpointはperiodic transfer非対応」という注記も
あるため、今回のroot LS keyboardでtimeoutする可能性がある。その場合は通常経路を変更せず、
FS／HS HIDで再判定する。

実機結果はrequested／scheduled interval 1、frame list address/read-back
`0x4FF7F200`一致、`HCFG=0x06800200`、complete、halted 1、channel 1 IRQ 1、WFI 5、8 bytes、
`HCINT=0x00000003`、成功QTD `0x06000000`だった。診断終了後もHAINTMSKはchannel 0のみ、
HCINTMSK1とpending channel 1は0へ戻り、通常入力も継続した。root LSでもGoと判定する。

常設経路ではstaticな512-byte aligned frame listと4-entry QTD bank、4個の64-byte report bufferを
channel 1〜4へ割り当てる。rootへFS/LSで直結したkeyboard／mouseとFull-Speedハブ配下のHIDを登録し、activeな
channel bitを`bInterval`ごとに共有frame listへORする。各slotはQTD／buffer、data toggle、世代token、
IRQ pendingを独立して持つ。QTDはidle NAK中もactiveのままコントローラーが処理する。ISR完了後、
次の表示フレームで`InterruptIn::read_report`が該当slotのAtomicをtakeし、QTD／bufferをcache同期して
reportを返し、data toggleを進めて次QTDを即時rearmする。registry clear／rescanは全active channelを
haltしてperiodic設定とDMA addressを消してからdriverを破棄する。High-Speedハブ配下はSplitと
controller-wide DMA modeの調停がStage 5まで未実装なので従来fallbackのままである。

4-slot化後のrelease ELFではQTD bankは2048 byte、report buffer bankは256 byteである。
`check_elf_layout.py`はframe list／QTD bank／buffer bankの存在・内部RAM範囲・alignmentに加え、
QTDが4×512 byte、bufferが4×64 byteであることも検査し、各QTDの512-byte strideを保証する。

常設経路の実機結果では、1回目がchannel 1／complete／rearm = 26／26／27、10秒後の2回目が
38／38／39となった。一方、両時点でpoll 0、submit／reap／cancel = 30／30／0のまま、
periodic errors、spurious、stale-token、unknown causeはいずれも0だった。`HAINTMSK=3`、
`HCINTMSK1=0x2807`、再接続後の入力も成功した。よってidle CPU poll除去とrearm継続を確認し、
単一直結keyboardについてStage 4を完了とする。

4-slot化後、root直結Full-Speed mouseでもchannel mask 2、channel 1 IRQ 254、periodic
complete／rearm 253／256、poll 0、submit／reap 90／90、cancel 0を確認した。periodic errors、
spurious、stale-token、unknown causeはいずれも0であり、mouseもGoと判定する。LS keyboardの
同版回帰もchannel 1 IRQ 34、complete／rearm 33／35、poll 0、errors 0で通過した。
Full-Speed only hubは試験機材の入手が困難なため、既存のFS/LS-only host設定をruntimeで切替える
`usbfs on|off`診断を追加した。`usbfs on`は設定後に即時root reset／再列挙するため、手元の
High-Speed hubをFull-Speedで接続し、Splitなしで複数channelを同時にactiveにする代替試験に使う。
既定はoffであり、試験後は`usbfs off`でHigh-Speedへ戻して再列挙する。

代替試験ではhub port 1のLS keyboardをchannel 1、port 3のFS mouseをchannel 2へ割り当て、
periodic channel mask 6、`HAINTMSK=7`、両channelの`HCINTMSK=0x2807`を確認した。periodic IRQ／
complete／rearmは340／340／342、poll 0、errors 0、全pending 0だった。`usbfs off`後はroot hubが
High-Speedへ戻り、同じkeyboard／mouseをそれぞれLow／Full-Speed Split routeで再列挙できた。

専用の診断endpointから始め、frame listと`HCFG.PERSCHEDENA`を段階的に有効化する。

- frame listとperiodic QTDを必要なアラインメントで内部SRAMに確保し、DMA可視性を
  `check_elf_layout.py`とcache同期で検証する。
- `HFLBAddr`、periodic FIFO、`bInterval`、`HFNUM`、完了割り込みの対応を記録する。
  idleのNAKをエラーや再列挙と誤認しない。
- root直結キーボード、マウス、ハブ配下の同速機器の順に`HCCHAR.eptype=INTR`を有効化する。
- `INTR`が進まない、または入力が不安定ならperiodic schedulerを無効へ戻す。その場合は
  Stage 1〜3を採用し、HIDはBULK代用＋前景の周期投入を維持する。判断と生ログを残す。

完了条件は、`INTR`型HIDで取りこぼしなし、idle時の不要CPU消費なし、表示57 Hzに依存しない
記述子間隔での動作を実機確認すること。

### Stage 5: Split Transactionとの統合

**進捗: 完了。High-Speedハブ配下のHIDはserialized Split fallbackを維持し、各phaseのIRQ＋WFI待ち、
controller-wide DMA modeの排他guard、keyboard＋mouse＋HS USBメモリの同時回帰を実機確認済み。**

排他guard追加後のHigh-Speed hub実機結果は、Split packets 4745、rounds 58727、sleep 59188、
WFI 59149、poll 0、conflicts 0、snapshot時mode active 0だった。periodic channel mask／IRQ／errorsは
すべて0、`HAINTMSK=1`であり、descriptor periodic DMAとbuffer Split DMAが混在していない。
channel 0 submit／reapは461／461、cancel／stale-token／unknown causeは0だった。round数がpacket数を
上回るのはTTのSSPLIT／CSPLITおよびidle NAKの安全境界回収であり、CPU register pollではない。

同じHigh-Speed hubのport 2へHigh-Speed Mass Storage（Sony `054C:0243`）を加え、keyboard／mouseを
接続したまま`INQUIRY`、`TEST UNIT READY`、`READ CAPACITY(10)`、LBA 0の`READ(10)`を完了した。
capacityは7831552×512 bytes、読出しは512 bytesと末尾`55 AA`を確認した。最終snapshotはSplit
packets／rounds 4756／61423、conflicts 0、mode active 0、poll 0、submit／reap 353／353、
cancel／stale-token／unknown cause 0だった。descriptor DMAのMSCとbuffer DMAのSplit HIDを交互に
使用してもハブ／TT resetなしで継続したため、Stage 5を完了とする。

Stage 3/4の通常モードと現行Split Transactionを`TransferArbiter`で統合する。

- HSハブ配下のFS/LSキーボード・マウスについて、通常時とMSC同時要求時の両方で
  SSPLIT/CSPLITを安全境界まで回収できることを確認する。
- DMAモード切替前後で全有効チャネルの停止、FIFO、ISRに残る通知を整合させる。旧転送を
  黙って捨てず、必ず回収または明示cancelする。
- Split中にperiodic再投入が走らず、復帰後にHIDが再開することを診断カウンタで確認する。

完了条件は、HSハブ配下のLS/FSキーボードとHS USBメモリを同時使用し、連続入力中の
`usbread`を複数回成功させ、ハブやTTのリセットなしに入力が続くこと。

### Stage 6: 接続変化と回復のイベント化

**進捗: root-portのconnection eventと再接続は実装・実機確認済み。ハブ配下はcontroller-wide
DMA mode競合を避け、既存の低頻度scanを意図的に維持。**

最後にルートポートの`PRTCONNDET`等をDWC割り込みで受ける。ISRは変化を記録し、
デバウンス、リセット、列挙、レジストリ更新は前景が行う。これでroot直結の挿抜を
`ROOT_RESCAN_FRAMES`待ちなしに検出する。

ハブ配下のstatus-change Interrupt INはHigh-Speed periodic descriptor DMAになる一方、同じハブ配下の
FS/LS HIDはbuffer DMAのSplitを使う。DWCのDMA modeはcontroller-wideであり、status endpointを
常設すると毎フレームのSplit HIDと競合する。この構成で無理に実装せず、既存の低頻度
`scan_empty_hub_ports`を安全なfallbackとして残す。ハブ状態を根拠なくルートポート割り込みへ
統合しない。

## リスクと中止条件

| リスク | 対策・判断 |
| --- | --- |
| 割り込みソースやW1C順序の誤りで割り込み嵐になる | Stage 0で最小マスクを実測する。異常時はDWC側マスクを落とし、前景の診断フラグで停止する。 |
| 表示ISRとUSB ISRの共存で表示が乱れる | sourceごとに分岐し、優先度・実行時間を測る。USB処理で表示DMA再armを遅らせない。 |
| periodic設定がこのDWC実装で動かない | Stage 4を独立したGo/No-Goにする。失敗時も割り込み完了化は採用し、BULK代用を維持する。 |
| Split中のDMAモード切替が別チャネルを壊す | Splitを排他モードにし、停止確認なしの切替を禁止する。逐次フォールバックを残す。 |
| 完了通知の取り逃がし／二重処理 | Atomicイベント連番、チャネルごとのW1C、トークン世代、前景でのtake-and-clearを使う。 |
| ISRがXIP／キャッシュ依存や再入を起こす | ISRをMMIO W1CとAtomic更新だけに絞り、IRAM配置をELF検査する。 |

次のいずれかが起きた場合は当該Stageを中止し、ポーリング実装を戻してログとレジスタ値を
本文へ記録する。

- USB ISRを有効にすると表示DMAのframe sequenceが止まる、またはFIFO underrunが増える。
- 同じ転送の完了が二重に回収される、またはhalt後もDMAが続く。
- Split実行後に無関係なhub/control転送が恒常的に失敗する。
- periodic `INTR`が進まない、またはHID入力の取りこぼしが現行より増える。

## 変更予定箇所

| ファイル | 変更内容 |
| --- | --- |
| `src/interrupts.rs` | 表示DMAとUSBを扱う共通CLIC入口、USB ISR登録、Atomicイベント状態。 |
| `src/usb/hcd.rs` | DWC割り込みmask/ack、固定長スロット、submit/reap/timeout、割当て、Split排他制御。 |
| `src/usb/registry.rs` | 前景USBサービス、クラスドライバへの完了配布、接続変化、仕事量・公平性の管理。 |
| `src/usb/hid.rs`、`hid_keyboard.rs`、`hid_mouse.rs` | 同期pollから要求／完了状態機械へ移行。Stage 4成功時のみ`INTR`を使用。 |
| `src/usb/protocol.rs`、`bot.rs`、`msc.rs`、`hub.rs` | 同期転送を互換待機または非同期継続状態へ段階移行。BOT順序とhub制御の直列性を維持。 |
| `src/input.rs`、`src/app.rs` | フレームだけでなくUSBイベントでも前景サービスを進める。単一所有者モデルは維持。 |
| `src/app/shell.rs` | 割り込み・チャネル・periodic・Splitの読み取り専用診断。 |
| `docs/USB.md`、`docs/INPUT.md`、`docs/FILE_LAYOUT.md`、`docs/DIAGNOSTICS.md` | 完了Stageに合わせて現状仕様、待機モデル、責務、診断を更新。 |

## 実機試験マトリクス

| 構成 | 確認内容 |
| --- | --- |
| root直結 HID Bootキーボード | idle、連続キー、長押し、挿抜、再列挙、consoleと全画面モードのキー待ち。 |
| root直結 HID Bootマウス | 連続移動、ボタン、`win`画面での取りこぼし・加算量。 |
| root直結 USBメモリ | `usbmsc`、`usbread 0`、`usbmbr`、連続実行、BOT失敗後の回復。 |
| HSハブ + 同速複数機器 | 複数キーボード／マウス、USBメモリを同時に使う際のチャネル割当てと公平性。 |
| HSハブ + FS/LS HID | Splitのidle／連続入力、MSC同時処理、抜き差し後のTTとhub制御の健全性。 |
| 長時間動作 | 少なくとも90秒の入力・ポインタ・表示更新を継続し、USB完了、timeout、再列挙、LCD underrunを集計。 |

実装ごとに`cargo fmt --check`、`cargo check`、`cargo build --release`、
`tools/check_elf_layout.py`を実行する。USB ISRを追加したStageでは、ISRと静的状態が
IRAM/DRAMの許容範囲にあり、XIP依存関数を呼んでいないことをELFから確認する。
