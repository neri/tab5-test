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
PSRAM: profile MHz=0x000000C8
PSRAM: read latency cycles=0x0000000E
PSRAM: write latency cycles=0x00000007
PSRAM: DQS window start=0x...
PSRAM: DQS window length=0x...
PSRAM: DQS phase=0x...
PSRAM: DQS data delay=0x...
PSRAM: DQS delay=0x...
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

ログはUSB Serial/JTAG（GPIO24/25）のCDCシリアルへ出ます。ホストが接続されていない間は
SOF（1 msごとのフレーム開始パケット）が来ないので、`uart.rs`はTX FIFOが埋まった時点で
それを検出し、以降の出力を捨てます。起動時にUSBを繋いでいなくてもファームは待たされず、
後からケーブルを挿せばSOFの再開を検出して次の行から出力が戻ります。ただしFIFOに残った
最大64バイトは破棄できないため、接続直後の1行目が切断中の古い断片になることがあります。
ホスト側は`tools/monitor.py`を使うと再列挙をまたいで追従できます（DTR/RTSを操作しないので
チップをリセットしません）。

`ICM: master priority`と`ICM: master arqos`は、DW-GDMAの2ポート分
（bit 12〜19）が`F`になっていれば書き込みが効いています。ここが`0`のままなら、
レジスタ自体が書けていません。`ICM: master awqos`は書き込み側の診断値です。
表示DMAはPSRAMを読み出すだけなので、DW-GDMAのbit 12〜19を変更しません。
`LCD: DPI FIFO underrun ...`が出た場合は、そのフレームのパネル表示が水色に
なっています（[`DISPLAY_BANDWIDTH.md`](DISPLAY_BANDWIDTH.md)を参照）。

`displaybench`は表示負荷を経路別に測り、次の3行をUARTと画面へ出します。

```text
displaybench: ppa-safe count=100 phase=0ms
burst=128 completed=100 frames=...
mean=...us underrun operations=.../100
```

`completed`が指定値より小さい場合は、続けて`operation or display DMA failed`が出ます。
CPU/PPA/cache同期の切り分けと実機試験条件は
[`DISPLAY_UNDERRUN_REFACTOR_PLAN.md`](DISPLAY_UNDERRUN_REFACTOR_PLAN.md)を参照します。

同じ標準条件を一括実行する場合は`db`だけを入力します。`db 20`のように1 case当たりの
回数も指定できます。結果は`mode phase burst mean underruns frames`の1行1 caseで、測定中の
console再描画を避けるため全13 caseの完了後にまとめて表示します。正常な全画面試験色は
BLACK/REDです。通常のproduction設定だけを100回確認するときは`dp`を使います。
30分のidle走査だけを確認するときは`di`を使い、103,200 frameそれぞれのunderrunを回収します。
実際のconsole scroll 100回と全画面アプリ遷移は`ui`で一括実行し、最後に
`ui visual: underruns=... dma_error=...`を表示します。途中の各画面は任意キーで次へ進みます。
表示・PSRAM heap・microSD・USB MSCのread-only複合試験は`mix`で既定120分実行し、最後に
`mix: PASS (SD/USB were read-only)`を表示します。途中経過は10分ごとにUARTへframe数を出し、
結果の`usb retries: packet=... command=...`はそれぞれ同一BOT phase内のQTD再投入回数と、
BOT Reset Recovery後のREAD(10)再送回数です。複合試験前にUSBだけを短く確認する場合は`ut`を
実行します。既定で同じ4 KiBを100回read・比較し、`completed`、transport `failures`、data
`mismatch`に加え、`packet_retries`、`command_retries`、`proactive_resyncs`を表示します。
最後の値は、応答が止まる前にBOT command境界を再同期できた回数です。

`mix`の結果には`rescans=...`も表示します。BOT ResetのEP0 recoveryまで失敗したときだけroot
portをreset・再列挙し、再接続したMSCから同じread-only 4 KiBを取得します。開始時の基準dataと
一致すれば`mix: USB rescan recovered matching read-only data`として継続し、不一致または3回連続で
再列挙／readできなければFAILです。また、試験開始時にMSCが未登録またはnot readyなら、`mix`が
最大3回同期的に再列挙します。ready確認後の基準4 KiB readも失敗した場合は同じく再列挙してから
再試行し、setupが完了するまでsoakの時間計測へ入りません。3回のroot rescanでも回復しなければ、
USB-A VBUSを1秒offにしてHub／MSCを一度だけ完全にpower-cycleします。再取得した4 KiBが基準dataと
一致した場合だけ継続し、結果には`power_cycles=...`も表示します。1試験あたり最大1回です。

再起動耐久試験は`rt`だけで既定20回を自動実行します。途中bootはUARTに次を出し、最終bootは
画面にも`REBOOT TEST PASS: 20/20`を表示してプロンプトへ戻ります。

```text
REBOOT TEST: completed=0x...
REBOOT TEST: remaining=0x...
REBOOT TEST: PASS total=0x00000014
```

途中bootが80 MHzへfallbackした場合は、そのbootを成功回数へ含めず
`REBOOT TEST: FAIL completed=...`を出して自動再起動を終了します。
実機の既定20回試験は`PASS total=0x00000014`、画面表示`PASS: 20/20`で完走済みです。

SDカード関連は起動シーケンスに含まれず、シェルコマンド（`sdinfo`/`sdread`/
`sdreadn`/`sdwritetest`/`sdzero`）実行時にのみ`SDMMC: ...`という接頭辞で
UARTへ出ます。正常時は`SDMMC: card activated`の後にCID/CSDの生値が続きます。
対応範囲は[`STORAGE.md`](STORAGE.md)、失敗パターンの詳細は
[`SD_CARD_PLAN.md`](SD_CARD_PLAN.md)を参照してください。

Wi-Fi（ESP32-C6）は保存profile確認のため対話ループ開始時に起動します。保存設定があれば
`WIFI: saved profile auto-connect started`、なければ`WIFI: no saved profile`、設定取得までに
失敗しても起動を続けて`WIFI: saved profile probe failed; continuing boot`と出します。
以後も層ごとに接頭辞が分かれており、どこで止まったかがそのまま分かります。

- `SDIO: ...` — C6をSDIOカードとして活性化する層。正常時は
  `SDIO: C6 activated`とRCA・CIS識別子が続きます。E2の各レジスタと
  6本のパッドレベルも毎回出るので、C6が給電されバスがプルアップされているかを
  ここで確認できます
- `HOSTED: ...` — ESP-Hostedのフレーム層。正常時は
  `HOSTED: opening the data path`のあと`HOSTED: link is up`です。
  `HOSTED: the C6 stopped answering, link lost`が出た場合は直後の
  `SDIO: C6 still answers CMD5`／`does not answer CMD5 either`が
  「C6がリセットされた」か「バスごと固まった」かを切り分けます
- `RPC: ...` — TLVとprotobufの層。応答が来ない場合は
  `RPC: timed out waiting for response id=...`が出ます
- `WIFI: ...` — `esp_wifi_*`に対応する層。スレーブがエラーを返したときに
  リクエストIDとステータスを出します
- `WIFI MENU: ...` — 全画面Wi-Fiメニュー。開始時の`opened`、scan成功時の
  `access points=...`、scan経路の失敗、associationとDHCPの完了を記録します。
  入力したパスワードは値・長さとも出しません。association／DHCP待ちは接続管理器が
  フレームごとに進め、初回接続と接続後切断の再試行も理由別backoffで進めます。切断通知は
  シェルやbrowserへ戻った後も`wifilog`へ残ります
- `NET: ...` — smoltcpによるIPv4の層。正常時は`NET: DHCP configured`だけで、
  それ以外は失敗の報告です。`NET: dropped an outgoing frame`はスレーブが
  スロットルを要求している間に送ろうとしたフレーム、`NET: DHCP lease lost`は
  保持していたリースが失効したことを示します

対応範囲は[`WIFI.md`](WIFI.md)と[`NETWORK.md`](NETWORK.md)、実機で踏んだ罠は
[`WIFI_C6_PLAN.md`](WIFI_C6_PLAN.md)と[`TCPIP_PLAN.md`](TCPIP_PLAN.md)を
参照してください。

IP層が答えない場合の切り分けはUARTログよりコマンドの出力を見ます。
`netdump`が何も出さなければフレームがそもそも届いておらず（APへアソシエート
できていない）、宛先・送信元MACとethertypeが出るならリンク層までは動いています。
`ipconfig`の`rx queued`／`dropped`は受信キューの深さと、キューが溢れて捨てた
フレーム数です。`dropped`が増え続ける場合はポンプが追いついていません。
`uptime`はSYSTIMERティック秒とフレーム数からの概算秒を並べて出すので、
2つが離れていればティックを取りこぼしています（smoltcpのタイマの基準が
狂うので、ネットワークの不調がここに出ることがあります）。

`wifilog`は接続管理器の直近16件を古い順に表示します。各行は単調時刻、接続世代`g`、
試行番号`a`、旧状態→新状態、reason、RPC status、または`retry-ms`を持ちます。
同じ状態が続く行は、再試行を決めた元のreasonや期限前に届いて拒否した古いイベントの記録です。
`stable-reset`はassociationが10分安定してbackoffの失敗回数を0へ戻した記録です。入力した
パスワードは値・長さとも履歴へ入りません。`startup-profile`はC6 NVSからの起動時接続、
`profile-saved`／`profile-save-failed`はassociation後の保存結果、`profile-forgotten`は
永続設定削除を表します。`enabled`／`disabled`は永続ON/OFF操作、状態`off`はsession、stack、
自動接続timerがなくC6をpower downした状態です。メニューまたはCLIで接続先を入れ替える際の`replace-disconnect`は、古い
associationの切断完了を待ってから新しい接続へ進んだ記録です。切断イベントが3秒以内に
来なければ`disconnect-timeout`で停止します。例:

```text
1234ms g2 a1 associating->associating reason=4
1234ms g2 a1 associating->retry-wait retry-ms=500
1734ms g3 a2 retry-wait->associating retry-timer
```

接続後の自動再接続では`online->online reason=...`、`online->retry-wait retry-ms=...`、
`retry-wait->associating retry-timer`、`associating->associated connected`、
`associated->dhcp dhcp-start`の順が基本です。通常の起動時自動接続でも`connected`を独立した
遷移として記録した後にDHCPへ進みます。
C6リンク喪失では最初のreasonが`link-lost`になり、再構築成功時に`link-ready`が入ります。

`wifisaved`はC6が現在読み込んでいるSTA設定を調べ、SSIDと資格情報の有無だけを表示します。
RPC応答に含まれるpasswordは表示せず、長さも診断情報へ残しません。これはRAMの一回接続設定を
表示する場合もあるため、C6 NVSだけを確認するにはC6 reset直後に実行します。OFF中はC6を
起動せず、起動時に読んだ保存profile有無だけを表示します。`wififorget`成功後は現在のassociationが
残る場合がありますが、管理器のRAM資格情報を消去し、次回起動時接続を止めます。OFF中のforgetは
flash操作の間だけC6を起動し、空profileのOFF markerを書き直してから再びpower downします。
同コマンド末尾の`profile writes this boot`、`failed`、`forgets`は起動後に管理器が要求した
C6 NVS操作の回数で、C6内部NVSの生涯write回数ではありません。

USB-AホストはLCDとCardKBの初期化後に起動し、最初の`UsbHost::rescan`を実行します。
そのため、起動時にも列挙結果や`USB: initial scan complete`がUARTへ出ます。その後も、
ルートポートの切断・再接続、空いているハブポートの増分スキャン、トランザクションエラーからの
復帰時に`USB: ...`ログが出ます。`usbinfo`/`usbhub`/`usbmsc`等は共有レジストリを使い、
`usbrescan`だけがユーザー操作でバスの再列挙を行います。`usbvbus`はI/O expanderの出力ビットを
直接変更する診断用コマンドです。対応範囲は[`USB.md`](USB.md)、段階分けと未確定事項は
[`USB_HOST_PLAN.md`](USB_HOST_PLAN.md)と
[`USB_INTERRUPT_REFACTOR_PLAN.md`](USB_INTERRUPT_REFACTOR_PLAN.md)を参照してください。
Hub portのdevice descriptor取得に失敗した場合、通常の増分スキャンは同じ物理接続を保留して
接続状態だけquietに監視します。約1秒ごとにport reset／列挙エラーを出し続けることはなく、抜き差し
または明示的なfull rescanでだけ再試行します。第9版は起動時に`USB ENUM: bounded retry v9`を出します。
1 packet QTD、BOT予防再同期、周期SSPLITの位相合わせを組み合わせた修正版は続けて
`USB STABILITY: phase-aligned split HID v24`を出します。両classが同じbusへ登録されると
`USB: MSC present, serializing HID and bulk on channel 0`が続きます。

起動時の初回スキャンは、各段階の所要時間を10進ミリ秒で出します。`USB BOOT: root connect ms=`
以降`scan total ms=`まではVBUSを入れた時点が起点で、USB MSCが見つかった場合は続けて
`USB BOOT: unit ready ms=`／`unit ready attempts=`／`read capacity ms=`／`first LBA 0 read ms=`
（最初のSCSIコマンドが起点）と、両者を足した`USB BOOT: usable from VBUS on, total ms=`が出ます。
読めなかった場合は、メディアが無いと分かったときだけ
`USB BOOT: mass storage has no medium, not usable`（REQUEST SENSEのASC `0x3A`による即断）、
それ以外は`USB BOOT: mass storage did not become readable`になります。
デバイスが無い起動では`USB BOOT: scan total ms=`と
`USB BOOT: no device on USB-A during the initial scan`の2行になります（この経路だけが
connect待ちの上限を使い切るので、その長さがUSBストレージを使わない起動のコストです）。
これは起動時にUSB MSCを最優先のファイルシステムにするための待ち時間を実測で決めるためのもので、
`usbmargin`が同じ計測をVBUSの再投入で繰り返します
（[`USB_MSC_BOOT_MARGIN_PLAN.md`](USB_MSC_BOOT_MARGIN_PLAN.md)）。

転送失敗時のpacket errorログには、`USB:   HCINT=`／`HCCHAR=`／`HCTSIZ=`／転送済みbyte数と
HPRT／HFNUMが続きます。QTD status 1はCRC・transaction timeout・stuffing・false EOP・
excessive NAKをまとめた値なので、HCINTのXactErr（bit 7）やChHltd（bit 1）で
「デバイスが無応答」と「トランザクションが壊れた」を区別するために使います。BOT層の失敗は
`USB BOT: bulk IN packet retries exhausted during CSW`のように**どの段階**
（`CBW`／`data OUT`／`data IN`／`CSW`）で落ちたかを出します。

packet errorのログには`USB:   port now:`（connected／enabled／powered／OVER-CURRENT）と
`USB:   port since bus came up:`（OVER-CURRENT／connect-change／enable-change）が続きます。
後者は**ISRがクリアしてしまうHPRTの変化ビットをラッチしたもの**で、
ポートがenableされた時点から次にenableされるまで保持されます。`usbhw`でも同じ内容を確認できます。
デバイスが電力不足で落ちたのかプロトコル上応答しないだけなのかは、ここで切り分けます
（[`USB.md`](USB.md)の「電力問題の切り分け」）。

`USB HID: periodic channel stalled, channel=N`は、Interrupt INのチャネルを
コアが面倒を見なくなった（`HCCHAR.ChEna`が落ちた）状態の検出です。続けてHCCHAR／HCINT／
HCINTMSK／HAINTMSK／HCFGとport状態を出し、そのHIDを再列挙へ送ります。これが無いと
キーも来ずログも出ずデバイスだけ`usbinfo`に残ります。

`USB BOT: session is unusable, skipping commands until re-enumeration`は、壊れたMSC sessionへ
BOTコマンドを送らずに失敗させている状態です。HIDはそのまま維持し、`usbrescan`を明示的に
実行したときだけ全バスを再列挙します。
ハブ配下のデバイスが列挙できない場合は`USB: power-cycling hub port N`が出ます。
セルフパワーハブでは`power-cycling USB-A`が下流ポートに届かないためです。

stale sessionによる再列挙が連続する場合は
`USB: repeated stale sessions, next rescan in frames=`を出してバックオフします。
再列挙はバス全体をリセットするため、attach直後に必ず失敗するデバイスがあると
正常なデバイスまで巻き添えで落とし続けるためです。

チャネル0をhaltできなかった場合の`USB: channel 0 did not halt; the bus needs re-enumeration`だけが
controller全体の「バス使用不能」を記録し、次のフレーム境界で自動的に再列挙します。
class driverの復帰手順が失敗した場合の`USB BOT: reset recovery failed; this session needs
re-enumeration`と、回復が2回続けて効かなかった場合の`USB BOT: recovery is not holding; this
session needs re-enumeration`はMSC sessionだけを停止し、HIDを巻き込む自動再列挙は行いません。
`reset recovery complete`が出ていても回復したとは限りません（control転送が通っただけで
bulkが動かない状態があり、その検出が2回連続判定です）。それでもデバイスが応答しない場合は
`USB: device unreachable after a port reset; power-cycling USB-A`を出してVBUSを1秒切ります
（30秒に1回まで）。詳細は[`USB.md`](USB.md)の「転送失敗からの自動復帰」を参照してください。

Bulk転送が応答しなかった場合、HCD共通ログは`USB: packet timed out waiting for channel halt`、
BOT層は方向別に`USB BOT: bulk IN timed out`等を出します。以前のHCDログはBulk失敗でも
`control transfer timed out`と誤表示していました。portが接続・有効・給電されたままで
Recoveryできれば、続けて`USB BOT: reset recovery complete`と
`USB MSC: retrying READ(10) after BOT recovery`が出ます。Recovery自体が完了しなければ
`reset recovery failed`となり、READ(10)の1回再送も失敗した場合は呼び出し元へ失敗を返します。
Bulk QTDは1 packetに限定し、約1秒でhaltしなければ同じDATA PIDで最大4回再投入します。
合計約5秒で応答しなければBOT Resetへ進みます。BOT ResetのIN statusを含むcontrol packetは約1秒で、
いずれもCPU周波数からiteration数を算出します。`packet_retries`にはstatus 1とtimeoutの両方による
QTD再投入を数えます。
`USB: transfer QTD packet error, status=0x00000001`は、ESP-IDFと同じQTD定義でCRC、transaction
timeout、stuff、false EOP、excessive NAKのいずれかです。BOT層は1 packet QTDならtoggleを
進めず同一packetを50 ms間隔で最大20回まで再送します。4 KiB READ(10)もendpoint MPS単位の
QTDへ分割し、各完了後にsoftwareが次のDATA PIDを決めます。成功した再投入は
`packet_retries`へ数えます。

連続READでは、成功したREAD(10)を16回処理するごとにcommand間でMass Storage Resetと
Bulk IN／OUTの`CLEAR_FEATURE(ENDPOINT_HALT)`を実行し、両toggleをDATA0へ戻します。
実機で最短33 READ後にBulkとEP0が応答しなくなったため、EP0がまだ応答する半分の間隔で
BOT境界を再確立する予防策です。root portやHIDはresetしません。`ut`開始ログは
`USB TEST: phase-aligned split HID v24`、実行回数は`proactive_resyncs=`で確認します。
WRITE(10)の直前にも同じ再同期を行い、
`USB MSC: proactive BOT resync before WRITE(10)`を出します。

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
ただし別のHigh-Speedハブ＋Low-Speed HIDでは列挙時に`HCINT=0x82`で失敗しました。第19版は
SSPLITをmicroframe 0〜5に置き、最初のCSPLITを2 microframe後、NYET後を1 microframe後へ
配置します。transaction error時は`split failed during SSPLIT|CSPLIT`も出します。
第19版ではkeyboardの列挙とattachは成功し、最初のInterrupt INだけがCSPLITで`0x82`になりました。
第20版はSplit HIDの`HCCHAR.EPType`を実descriptorどおりInterruptへ設定し、文字入力まで通りました。
ただしidle時のCSPLIT NYETをControl／Bulkと同じ安全境界待ちで最大5000 round追い、
`USB: giving up mid-split; the hub's TT never answered`の連続と入力freezeを起こしました。
第21版はInterrupt Splitを1 full frame内のSSPLIT＋最大3 CSPLITに制限します。最後までNYETなら
full-frame境界でTTの周期transactionが失効するのを待ち、ログを出さない通常のidle timeoutとして
次のpollへ戻します。実機ではエラーとfreezeが消えましたが、poll自体は描画と同じ約57 Hzで反応が
鈍い状態でした。第22版は1 kHz tickの非描画wakeからdescriptorの`bInterval`ごとに実行します。
起動ログの`USB HID: Split foreground poll interval ms=N`が採用周期です。Control／Bulkで
`giving up mid-split`が出た場合は引き続きTT異常です。
Split HIDを抜いた直後は実行中packetが`split failed during CSPLIT`／`HCINT=0x82`を1回出し得ます。
続く`USB: HID disconnected from hub port N`が局所切断の成功で、
`USB: a device session went stale, rescanning...`が出た場合は下流切断を判定できずroot rescanへ
fallbackしたことを示します。
第24版のSplit keyboardは各pollのSSPLITをmicroframe 0へ揃えます。idle NAKならSSPLIT＋CSPLITの
scheduled resultが確定した時点で終了し、同じ窓でSSPLITを再開しません。Low-Speed descriptorが
10ms未満なら`USB HID: invalid Low-Speed bInterval, descriptor=N`を出して10msへ補正します。
今回のdeviceは起動ログが`Split foreground poll interval ms=10`になり、静止時のSplit packet増加率は
約100 packet/秒が期待値です。実機でも10秒間に約1,000 packet増加し、入力安定・エラーログなしを
確認済みです。rounds/packetはTTが最初のCSPLITへNYETを返す回数で変わります。
同じHigh-Speedハブ上でLow-Speed keyboardとHigh-Speed MSCを併用した第24版の`ut 100`は
100/100、failure／mismatch 0、packet／command retry 0、予防再同期6回でPASSしました。
直後のsnapshotはSplit 1,126 packet／2,370 round、conflict 0、active 0、stale token 0、
port eventなしです。

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
- `PSRAM: MPLL calibration timed out`: 400 MHz MPLLの自己調整が完了しない
- `PSRAM: 200 MHz failed stage=0x...`: 200 MHz初期化の失敗段階。直後に80 MHzへ再初期化する
- `PSRAM: falling back to 80 MHz`: 同じbootでMSPI、mode register、DQSを80 MHz profileから再設定中
- `PSRAM: direct memory test failed`: DQS選定後の複数物理アドレス検査で不一致
- `PSRAM: diagnostic forced tuning failure`: `pf`による1回限りのfallback試験。故障ログではない
- `PSRAM: no valid DQS phase`: DQS位相調整
- `PSRAM: mapped memory test failed`: MMUまたはキャッシュ経路
- `LCD: PI4IOE1 reset control failed`: ソフトウェアI2CまたはI/O expander
- `LCD: D-PHY lock timeout`: D-PHY電源、クロック、PLL
- `LCD: DCS FIFO timeout`: パネルコマンド経路
- `LCD: DMA interrupt error`: DW-GDMA転送
