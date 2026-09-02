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
`mix: PASS (nothing written to SD/USB)`を表示します。途中経過は10分ごとにUARTへframe数を出し、
結果の`usb retries: packet=... command=...`はそれぞれ同一BOT phase内のpacket再投入回数と、
BOT Reset Recovery後のREAD(10)再送回数です。複合試験前にUSBだけを短く確認する場合は`ut`を
実行します。既定で同じ4 KiBを100回read・比較し、`completed`、transport `failures`、data
`mismatch`に加え、`packet_retries`と`command_retries`を表示します。

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

- `TLS: ...` — TLS 1.3 transport。正常な接続では**何も出ません**。出るのは
  次の3つで、いずれも接続が成立しなかったことを意味します

| 行 | 意味 |
| --- | --- |
| `TLS: <理由>; not connecting` | 真性乱数の種を用意できず、**1 packetも送っていない**。`entropy`コマンドで切り分ける |
| `TLS: unsupported CertificateVerify signature scheme` | serverがこのfirmwareの実装していない署名方式を選んだ。Ed25519とECDSA P-384はClientHelloに載るが検証できない（[`NETWORK.md`](NETWORK.md)） |
| `TLS: the server sent alert <level> <name>` | serverがalertを送って断った（画面は`tls-alert`）。`handshake_failure`は共通の暗号方式／鍵交換群が無い、`protocol_version`はTLS 1.3を話さない、`unrecognized_name`はSNIで送った名前をserverが知らない |
| `TLS: aborting the handshake with alert <level> <name>` | serverのhandshakeをこちらが解釈できず中断した（画面は`tls-handshake`）。相手ではなくこちら側の制約である |
| `TLS: a transaction was dropped without close()` | socketが`SocketSet`に取り残された。実装のバグで、そのsocketはリセットまで戻らない |

  失敗の分類は画面とコマンド出力側に出ます（`tls-cert`、`tls-pin`、`tls-alert`など）。
  ASN.1の解析位置のような詳細は意図的に出しません。証明書の中身を無制限に
  画面へ出すのは、そこに攻撃者の書いた文字列が入り得るからです

対応範囲は[`WIFI.md`](WIFI.md)と[`NETWORK.md`](NETWORK.md)、実機で踏んだ罠は
[`WIFI_C6_PLAN.md`](WIFI_C6_PLAN.md)と[`TCPIP_PLAN.md`](TCPIP_PLAN.md)、
TLSは[`TLS_PLAN.md`](TLS_PLAN.md)を参照してください。

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
ESP-IDF既定のbalanced FIFO分割と周期SSPLITの位相合わせを組み合わせた修正版は
続けて`USB STABILITY: fault-rescan retry v42`を出します。両classが同じbusへ登録されると
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
`reset recovery failed`となり、READ(10)または再送安全な照会の1回再送も失敗した場合は
呼び出し元へ失敗を返します。媒体確認に使うTEST UNIT READY、READ CAPACITY(10)、
INQUIRY／INQUIRY(EVPD)は媒体を変更しないため、Recovery後に1回再送し、
`USB MSC: retrying <command> after BOT recovery`を出します。
失敗したcommandは`USB BOT: failed command opcode=`にCDB opcode、続く行にattach後の
command tag、data byte数、IN方向かを必ず出します。`opcode=0x00`はTEST UNIT READY、
`0x12`はINQUIRY、`0x25`はREAD CAPACITY(10)、`0x28`はREAD(10)、`0x2A`はWRITE(10)、
`0x35`はSYNCHRONIZE CACHE(10)です。
非SplitのCBW／CSW／Bulk INと短いBulk OUT QTDは1 packetに限定し、約1秒でhaltしなければ
同じDATA PIDで最大4回再投入します。v36〜v39で試したFull-Speed WRITE dataの複数packet
QTD／複数QTD listはB2を後退させたため撤回し、現在はOUTも1 packet QTDずつ実行します。
channel 0のQTDは固定2-slot bankをpacketごとに交互使用するため、retryの`channel HCDMA=`は直前の
失敗packetと別の512-byte境界になります。
合計約5秒で応答しなければBOT Resetへ進みます。BOT ResetのIN statusを含むcontrol packetは約1秒で、
いずれもCPU周波数からiteration数を算出します。`packet_retries`にはstatus 1とtimeoutの両方による
QTD再投入を数えます。
`USB: transfer QTD packet error, status=0x00000001`は、ESP-IDFと同じQTD定義でCRC、transaction
timeout、stuff、false EOP、excessive NAKのいずれかです。BOT層は1 packet QTDならtoggleを
進めず同一packetを50 ms間隔で最大20回まで再送します。4 KiB READ(10)もendpoint MPS単位の
QTDへ分割し、各完了後にsoftwareが次のDATA PIDを決めます。成功した再投入は
`packet_retries`へ数えます。

正常なcommand境界ではcleanupを行いません。かつては成功したREAD(10) 16回ごとと
WRITE(10)の直前にhost channel／FIFO cleanupを実行していました——実機で最短33 READ後に
BulkとEP0が応答しなくなったため、EP0がまだ応答する半分の間隔でBOT境界を再確立する
予防策です。HCD側の契約を整えたうえで3構成の実機A/Bを行い、READ 1000回・WRITE 100回とも
cleanup無しで通ったため撤去しました（[`USB_BOT_HCD_REFACTOR_PLAN.md`](USB_BOT_HCD_REFACTOR_PLAN.md)の
Stage 5・6）。`proactive_resyncs=`と`USB MSC: proactive ...`のログはこの撤去で消えています。
`ut`開始ログの`USB TEST: fault-rescan retry v42`は残ります。

cleanupが残っているのは**失敗後だけ**です。そこでFIFO flushがtimeoutした場合は、
packetを再送せず、BOT Reset Recoveryも実行せずにsessionを引退させます。回数は
`MSC: cleanup-failed`で見ます。

READ／WRITE転送が失敗した場合はLBA、block数、READのFUA有無を続けて出します。

受入試験1回分をまとめて実行するのは`usbcheck [reads] [lba]`です。前後のcounterを自分で
採り、read soakと（LBAを指定した場合は）write 10回を実行し、**差分**とGo条件ごとの
PASS/FAILを出します。`usbhw`を2回採って目視で突き合わせる必要はありません——絶対値には
起動時の列挙とidle HIDのpollが全部乗っているので、差分でないと比較になりませんでした。
raw WRITEが不安定な段階ではfilesystem側の`fswritetest`を実行しません。代わりに
`usbrawcheck <犠牲LBA> [writes] [span] [gap_ms]`で、filesystem外の範囲へREADを挟まない
単一block WRITE列を発行します。gapは成功したWRITE commandと次WRITEの間だけ待つ0〜2000 msで、
既定0は連続burstです。失敗してsessionが使用不能になっても`usbrescan`後に同じ犠牲範囲を再利用でき、
filesystem repairを試験の前提にしません。
DMA cache同期の拒否経路は`usbcachefail`で確認します（下記）。

`usbhw`は上記に続けて、[`USB_BOT_HCD_REFACTOR_PLAN.md`](USB_BOT_HCD_REFACTOR_PLAN.md)
Stage 0のbaseline counterを固定書式で表示します。0の項目も必ず表示します——「counterが
無い」と「counterが0」を区別できない書式では比較になりません。行は`Line`の80 byteで
打ち切られるため、内訳は5桁の値でも収まる単位へ分割し、列見出しを短縮しています。

`BOT:`で始まる行はcontroller全体のもので、再列挙しても続きます。

- `cache-refusals=`は合計と方向別（`out`／`in`／descriptor`desc`）です。拒否は転送の失敗
  なので、`pkt-fail`の`cache`列と対になって増えます（periodic HIDのarm拒否など、
  channel 0を通らない経路では`cache-refusals`だけが増えます）。
- `refusal envelope`／`refusal data`は転送phase別の内訳です。列は
  `none`（未ラベル）／`ctl`（control）／`cbw`／`csw`と、`din`（data IN）／`dout`（data OUT）／
  `int`（Interrupt IN）です。phaseは転送を所有する層が公開するラベルで、HCDは解釈も
  分岐もしません。
- `refusal ch0`／`refusal periodic`はどのDMA共有オブジェクトかです。channel 0側は
  `qtd`／`buf`（payload）／`split`（split staging）、periodic側は`per`（常設periodicの
  descriptorとreport buffer）／`flist`（frame list）／`probe`です。`refusal last`は
  直近の拒否のaddress／長さ／phaseです。
- `pkt-fail`は2行で、合計と原因別内訳です。`timeout`（channelがhaltしなかった）、
  `stall`、`xact`（CRC／babble等）はデバイスやケーブルが起こし得るもの、
  `shortout`（OUTが要求より少ないbyteで完了した）、
  `cache`（DMA bufferまたはdescriptorのcache同期が拒否され、armする前に失敗した）と
  `qtderr`（QTD status 1）以降はこのドライバ自身の契約に関わるもの
  （`qtdbad`＝未定義QTD status、`nocpl`＝XferComplなしでhalt、`stale`＝世代の合わない完了、
  `splitrej`＝splitを開始できなかった）です。`cache`が0でない場合は転送が始まっていない
  ので、古いRAMが送られたりDMA前のcacheが公開されたりはしていません
  （[`USB.md`](USB.md)の「DMA bufferとcache同期の契約」）。
- `idle-poll`は**失敗ではありません**。`SET_IDLE(0)`のHIDはキーが動くまでNAKし続けるので、
  Interrupt INのpollが何も持たずに budget を使い切るのは正常なbusの姿です。`pkt-fail`へは
  加算しません。静止したキーボードを繋いだまま`ut 100`を回すと数百件出ます。
- `last-fail`は直近の失敗packetのkind、phase、方向、要求byte数、実転送byte数、
  `HCINT`、QTD最終control wordです。要求と実転送の差は、再送が同じbyteを二重に
  公開し得るかどうかを判断する数値です。`QTD=0x00000000`は「残量0で所有権解除」と
  「descriptorが書き戻されていない」を区別できない値なので、`act`をそのまま
  「全量転送済み」と読んではいけません。
- `fifo-flush`／`fifo-timeout`／`fifo-skipped`はTX（非periodic、`nptx`）／TX（periodic、`ptx`）／
  RX（`rx`）それぞれについて、flushが完了した回数、timeoutした回数、periodic channelが
  arm中で実行しなかった回数です。3つは別の状態で、まとめて「cleanup成功」と数えません。
  skipは失敗ではなく、動作中のHIDを守るための意図的な省略です（下の「転送失敗の巻き添え」）。
  timeoutだけが失敗で、その回のcleanupは対象のcommandを開始させません。
- `packet-cleanup out-nptx`はreported OUT packet error後にnon-periodic TX FIFOだけをflushした
  回数です。`usbcheck`では`delta pkt-retry err`のうちOUT方向だった回数と対応します。

`MSC:`で始まる行は接続中デバイスの現在のBOT sessionのもので、session再構築で0へ戻ります。
`cmd-retry`はBOT Reset Recovery後のREAD(10)再送、`cleanup-failed`は失敗後のcleanupで
FIFO flushがtimeoutした回数、`pkt-retry`はpacket error／timeout別の再送回数です。`MSC: commands`はCBW送信まで進んだ
command数で、`cleanup-failed`のぶんはここに入りません——「commandが失敗した」と
「commandが始まらなかった」を区別するための対です。`resubmit`は
転送長の契約が拒否した再送回数（`refused`）、そのうちdescriptorが実際にbyteを報告して
いたもの（`progressed`）、そして**要求長より大きい残量**を書き戻されたdescriptorの数
（`impossible-len`）です。最後のものは0になりません——このcoreはpacket error時に
64 byteのOUTへ100,489という残量を返すことがあります。異常ではなく、
**そのbyte数を信用しない**ことがこのdriverの契約です。拒否されるのはchannelが
haltしなかったpacketだけで、coreがpacket errorを報告したpacketは残量に関係なく
1 packet QTDなら再送します（[`USB.md`](USB.md)の「実転送長とretry安全性の契約」）。
拒否は転送が失敗に終わることを意味しますが、**同じbyteを
busへ二度出すよりは失敗させるほうが正しい**という判断の結果です。拒否の根拠にした
`reap HCINT=`と`reap QTD control=`はUARTログに出ます。`recovery`は実失敗後のBOT Reset Recovery回数と
その失敗数、`csw`は13 byte以外（`short`。名前はbaseline互換）、signature不一致（`sig`）、
tag不一致（`tag`）の回数です。次の`MSC: csw`行はPhase Error（`phase`）、未定義status、
host実転送長とresidueの矛盾、data INにCSWが先着した回数（`early`）です。`early`自体は
current commandの正当なFAILED応答になり得ますが、他の3つはtransport errorです。
CSWが期待と合わなかった場合はUARTへ受信byte数、期待tag、signature、tag、residue、statusを
出します。13 byteに満たない分は0埋めなので、`signature=0x53425355`は`USBS`が実際に届いた
ことを意味します。`usbcheck`はこれらを`delta csw`／`delta csw-detail`で差分表示し、invalid
CSWが1件でもあれば`BOT CSW contract (stage 3)`をFAILにします。

reported packet errorを再送するときは、reap値に続けてcleanup前のchannel 0を
`channel HCCHAR=`／`HCTSIZ=`／`HCDMA=`で表示します。channelがhaltしないtimeoutでは、強制haltで
状態を変える前の同じ3 registerを`before halt`として表示します。HCCHARのChEna／ChDis、HCTSIZの
PID／schedule情報、descriptor list addressが、直前の失敗から次のpacketへ持ち越されていないかを
比較するためのStage 3診断です。

転送が失敗したときのHCDログには`USB:   failure=`（原因とphase）、`requested bytes=`、
`actual bytes=`、`QTD final=`が続きます。従来のログは失敗したことと`HCINT`しか言わず、
要求byteのうち何byteが既に動いたのかを言いませんでした。

`usbcachefail`はHCDの契約が守られていることを、正常なhardwareでは起こせない4つの故障を
注入して確認します。1回の実行で4つとも試し、各段階でsessionが引退したら自動で
再列挙してから次へ進みます。ドライバ自身の論理の試験なので接続構成ごとに繰り返す必要は
なく、全体で1回で足ります。媒体へ新しいdataは書きません。

- **[1/4] cache同期拒否**（Stage 1）: DMA cache同期の拒否を**data IN phaseへ**注入し、その転送がchannelをarmする
**前に**失敗すること、および宛先bufferへ1 byteも公開されないことを確認します。宛先は事前に
`0x5A`で埋めます——deviceのdataでも、0埋めstagingでもない値なので、そのまま残っていれば
何も上書きされていない証拠になります。正常なhardwareは拒否しないため、注入以外に
この経路へ到達する方法はありません。ドライバ自身の論理の試験なので、接続構成ごとに
繰り返す必要はなく1回で足ります。

- **[2/4] 古い世代の完了**（Stage 2）: slotがもう持っていない世代でcompletionを渡し、
  それが新しいpacketの結果として回収されないことを確認します。同期APIでは1つの
  `Channel0Transfer`が1回の`run_packet`内で生成・submit・reapされるので、この状態は
  構造上起こりません。検査はこれを置き換えるqueue型schedulerのためにあり、
  **一度も発火を観測していない検査は、動くかどうか誰も知らない検査**です。
- **[3/4] 短いOUT**（Stage 2）: OUT packetを要求より1 byte少なく報告させ、それが
  要求長分の成功として上位へ返らないことを確認します。READ(10)を使うので、
  注入が当たるのはcommand blockのOUT packetで、媒体には何も書きません。
- **[4/4] FIFO flush timeout**（Stage 4）: **2つ同時に注入します**——data IN cache同期の
  拒否でREAD(10)を失敗させ、その失敗が起こすcleanupのFIFO flushをtimeoutさせます。
  試験対象のcleanupは何かが失敗した後にしか動かないためです。gateは4つ——timeoutが
  cleanup失敗として数えられたこと、readが失敗したこと、**BOT Reset Recoveryが試行されて
  いないこと**、sessionが引退したことです。3つ目が要点で、Reset Recoveryはcontrol転送と
  bulkをいま空にできなかったFIFOへ通すため、実行すれば「回復した」という誤った結論しか
  得られません。flush要求自体はhardwareへ発行してから結果だけを偽るので、注入がFIFOの
  中身を実機の状態からずらすことはありません。Stage 6より前はWRITE(10)前の予防cleanupを
  狙っていましたが、そのcleanupは撤去されたのでこちらへ移しました。

[1/4]の注入は**操作回数ではなくphaseで狙います**。commandのdata phaseへ到達するまでのcache呼び出し
回数は実装詳細で、間にReset Recoveryのcontrol転送も入るためです。phase指定なら回数に
関係なく目的のpacketへ当たり、`Control`とlabelされたrecoveryは動けます。`msc.rs`は
Reset Recovery後にREAD(10)を1回だけ再送するので、注入は再送を上回る回数armします——
1回だけだと再送（完全に同期された転送）が成功して治ってしまい、retry方針が働いた
だけの結果を契約違反と取り違えます。残った分は終了時に必ず解除します。

2回連続の失敗はBOT層が「recoveryが効いていない」と判定する形なので、[1/4]〜[3/4]では
**MSC sessionは設計どおり使用不能になります**。その旨を表示するので`usbrescan`して
ください。[4/4]も同じく引退します——host側cleanupが完了できなかった以上、
そのsessionで次のcommandを始める根拠がないためです。

`usbhw`はSplit Transactionのレジスタに加え、USB割り込みのsource、global enable、
総ISR回数、channel 0／periodic channel 1〜4／root-port／spurious回数、`GINTMSK`／`HAINTMSK`／`HCINTMSK0..4`、
最後に採取した`GINTSTS`／`HAINT`／`HCINT0`／`HPRT`を表示します。さらにStage 2の
sleep wait／poll wait／実行した`WFI`の回数とlast／max wait cyclesを表示します。通常の
control／bulkを使った後はsleepとWFIが増え、periodicへ昇格できないHIDのidle待ちではpollが増えるのが正常です。
`sleep`は待機したpacket数、`wfi`は途中で別割り込みに起こされて再度眠る場合も含む命令回数
なので、後者が前者より多くても異常ではありません。HIDでもキー報告を受信してchannelが
haltした時はchannel IRQが増えます。常設periodic HIDのidle NAKはcontrollerが処理するため、
前景のpoll／submit／cancelは増えません。
`IRQ slots`はdescriptor-DMA transferについて原則として`submit = reap + cancel`です。Splitは
別集計で、ここには含みません。直結HIDのidle NAK timeoutはcancelへ
入るため、キーを押さずに待つほどreapよりcancelが多くなります。観測瞬間に1件実行中なら
submitが右辺より1だけ多い場合があります。`stale-token`は常に0が正常です。
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
100/100、failure／mismatch 0、packet／command retry 0でPASSしました（当時は予防再同期6回。
現在は予防cleanup自体がありません）。
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
