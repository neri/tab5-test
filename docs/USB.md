# USB-Aホスト

> 索引: [`../DESIGN.md`](../DESIGN.md) ／ 段階分けと実機で踏んだ罠:
> [`USB_HOST_PLAN.md`](USB_HOST_PLAN.md)、[`USB_REFACTOR_PLAN.md`](USB_REFACTOR_PLAN.md)、
> [`USB_INTERRUPT_REFACTOR_PLAN.md`](USB_INTERRUPT_REFACTOR_PLAN.md)、
> [`USB_MSC_PLAN.md`](USB_MSC_PLAN.md)、[`USB_FLOPPY_PLAN.md`](USB_FLOPPY_PLAN.md)、
> [`USB_MSC_BOOT_MARGIN_PLAN.md`](USB_MSC_BOOT_MARGIN_PLAN.md)、
> [`USB_WRITE_STABILITY_PLAN.md`](USB_WRITE_STABILITY_PLAN.md)

Tab5のUSB-Aコネクタに繋がるHigh-Speed USB-DWCコントローラーをホストとして
使用します。モジュールの層構成（`hcd`／`protocol`／`hid`／`hid_keyboard`／
`hid_mouse`／`bot`／`msc`／`registry`）は[`FILE_LAYOUT.md`](FILE_LAYOUT.md)を
参照してください。この文書は現在どこまで動くかを説明します。

USB-C側のFull-Speed OTGコントローラー（GPIO26/27）と、`uart.rs`が使う
USB Serial/JTAG（GPIO24/25）は対象外です。

## 実機で確認できている範囲

- HID Boot Protocolキーボードからのキー入力（`src/usb/hid_keyboard.rs`）。
  `InputManager`がCardKBと並列にポーリングします（[`INPUT.md`](INPUT.md)）。
- HID Boot Protocolマウスからのポインタ入力（`src/usb/hid_mouse.rs`）。動作確認は
  `win`コマンドの画面で行います（[`APPS.md`](APPS.md)）。root直結Full-Speedマウスの
  periodic channel 1経路も実機確認済みです。
- 1段のUSBハブ配下の複数デバイス列挙と逐次ポーリング（`src/usb/hub.rs`）。
- USB Mass Storageの読み出し（`src/usb/msc.rs`）。詳細は
  [`STORAGE.md`](STORAGE.md)。直結・ハブ経由のどちらでも動作します。
  書き込み（WRITE(10)、`usbwritetest`）も実装・実機受入済みです。かつては間欠故障の
  緩和として各READ 16回ごと／各WRITE直前に予防的BOT再同期を必要としましたが、
  HCD側の契約を整えた結果それ無しで通るようになり、撤去しました
  （[`USB_WRITE_STABILITY_PLAN.md`](USB_WRITE_STABILITY_PLAN.md)、
  [`USB_BOT_HCD_REFACTOR_PLAN.md`](USB_BOT_HCD_REFACTOR_PLAN.md)）。
  通常のWRITE(10)は最大8 block（4 KiB）で、Stage 7では`1234:5645`と`054C:0243`を
  High-Speed直結／Full-Speed固定ハブ＋HIDの両方で2／4／8 block各10回確認しました。
- High-Speedハブ配下にFull/Low-Speedデバイスを繋ぐ構成（Split Transaction）。

## 中断したFloppy実装

UFI/CBI USB Floppy用の試作クラスドライバは`src/usb/floppy.rs`に保持するが、現在の
ビルドには含めず、レジストリも選択しない。したがってUFI/CBIデバイスは未対応として
一度だけログに出力され、`usbfloppy`と`usbfloppyprobe`コマンドは存在しない。

直結実機（VID:PID `054C:002C`、interface `08/04/00`、Bulk IN `0x81`、Bulk OUT
`0x02`、status Interrupt IN `0x83`）でのCBI ADSC制御要求は、descriptor-DMAの
SETUP PID修正後もSETUP段階の`XCS_XACT_ERR`で失敗した。詳細と再開条件は
[`USB_FLOPPY_PLAN.md`](USB_FLOPPY_PLAN.md)を参照する。

## root portのconnect待ち

`probe_port`が「VBUSを入れてからデバイスがpull-upを上げるまで」を待つ上限は
runtimeに切り替えられます（`hcd::set_connect_wait_ms`）。定常値は500 msで、
フレームループが空ポートを定期再probeする間はこの時間ずっとブロックするため
長くできません。起動時は`InputManager::new`でスキャンせず、白い起動画面が
connect waitを1 msへ一時変更した通常の`rescan`を100 ms間隔で繰り返します。rootが
空の場合と、接続は見えるがenable／列挙できない場合のどちらも、campaign開始から最低
2,000 msは再試行します。列挙できればその時点で確定し、2,000 ms後も列挙できない場合は
警告終端として起動を先へ進めます。画面にはscan開始前から`Running`を表示します。

初回の成功／警告はstartup routeを決めるための有限な判定であり、USB lifecycleの停止では
ありません。inventoryが空なら通常fallback scanを次の`InputManager::service`へ前倒しし、
起動画面がWi-Fi待ちで残る間も通常の物理接続eventと再スキャン、自動マウントを処理します。
次画面へ移った後も同じ`InputManager`が処理を継続します。

このcampaign全体の所要時間は`UsbHost::finish_boot_scan_campaign`がboot診断へ記録します。
Mass Storageのready待ちは初回scanから分離され、起動画面中の`AutoMount`が250 ms間隔、
最大4,000 msの既存budgetで担当します。計測手順と従来の1,000 ms根拠は
[`USB_MSC_BOOT_MARGIN_PLAN.md`](USB_MSC_BOOT_MARGIN_PLAN.md)にあります。
`usbmargin`は計測中だけ5,000 msを使います。

## バスの所有とスキャン周期

`UsbHost`（`src/usb/registry.rs`）がUSBバスの単一所有者です。
`hcd::probe_port`と`hub::Hub::open`を呼ぶのはこの型だけで、`usbinfo`／`usbhub`／
`usbmsc`などのシェルコマンドも同じレジストリを引きます。コマンドごとに個別へ
列挙するとバスリセットが走り、フレームループが持っているキーボードセッションを
黙って無効化するためです。

`InputManager::service`がフレーム境界ごとに次を進めます（`src/input.rs`）。

- ルートポートの切断検出: 毎フレーム（レジスタ読み出しだけなので安価）
- ルートポートの再接続: DWCのconnection eventを前景でtakeし、検出しだい即時再スキャン
- セッションが古くなったデバイスの再スキャン: 検出しだい即時
- **ハブの使用中ポートの取り外し掃引: 60フレームごと**（下記）
- ハブの空きポートの増分スキャン: 60フレームごと
- ルートポートが空のときの再スキャン: 300フレームごと（ブロッキングの
  リセット・デバウンスを伴うため粗い間隔にしてある）

構成が変わり得るたびに`topology_epoch`を1つ進めます。VFSの自動マウント
（[FILESYSTEM.md](FILESYSTEM.md)）が毎フレーム見るのはこの整数1つだけで、
バスには触れません。**このepochが進むのはデバイスがbindされた時点で、
logical unitが読み出しに答えられるようになる時点ではありません。**
その差は自動マウント側がready待ちで吸収します。意図的に粗く、同じ構成を見つけただけの再スキャンでも進みます。
変化していないのに突き合わせても、マウント表を1周するだけでバスI/Oは発生しません。

### Mass Storageの番号（`usbM`）

Mass Storageデバイスには接続時に番号を振り、**そのデバイスが繋がっている間は
取り上げません**。2本挿したうちの1本目を抜いても2本目の番号は動かないので、
触っていないデバイスのマウントが壊れることがありません。番号は順に増え、
16まで進むと先頭へ戻りますが、そのとき接続中のデバイスが持っている番号は
飛ばします。

番号はスロット（＝バス上の位置）ごとに`ConnectionEpoch`と一緒に予約します。
再スキャンは全スロットをいったん空にしてから作り直すので、番号の予約が
その間も残っていないと、戻ってきた同じデバイスが別番号になりマウントが全部
壊れます。逆に、ポートで接続変化のedgeが観測されていれば予約は無効になり、
同じポートに挿した別のデバイスは新しい番号を受け取ります。

`devices`はスロット順（直結ポート、次にハブポートの若い順）に並べます。
何かを抜いた後は番号順と一致しません。目の前のポートと突き合わせて読むための
一覧なので、バスの並びを優先しています。

### 再スキャンの理由（`RescanReason`）

`rescan`は理由を取ります。バス上の手順は同じですが、上の層が何を結論すべきかが
違うためです。転送失敗後にセッションを組み直す`Recovery`で同じ媒体が見つかったのは
「期待どおりの媒体が見つかった」ですが、利用者が抜いた後の
`PhysicalConnectionChange`で同じ媒体が見つかったのは「一度持ち去られた媒体が
戻ってきた」であり、開いているファイルを持つ側にとっては別物です。

- `Manual`: シェルからの要求、起動画面の初回探索、ルートが空のときの定期再スキャン
- `Recovery`: セッションが古くなったので組み直す
- `PowerRecovery`: こちらがVBUSを落として入れ直した後
- `PhysicalConnectionChange`: 接続変化のedgeを観測した

`rescan`は開始時に**未処理のedgeを先に採取**します。まだリセットしていない時点で
残っているedgeは物理的なもので、呼び出し元が指定した理由より優先します。
以前はこれを自分のリセットが生むedgeと一緒に捨てていたため、抜き挿しが
たまたま再スキャン直前に起きると痕跡が残らず、その上のマウントは媒体が
一度離れたことを知らないまま動き続けていました。

リセット**後**のedgeは引き続き捨てます。あれはこのファームウェア自身の
仕業であって、次のポーリングでホットプラグと取り違えてはいけません。

### ハブポートからの取り外し検出

ハブのポートからデバイスを抜いてもルートポートのHPRTは変化しません。ハブ自身は
まだ繋がっているためです。そしてセッションが死んだことを上へ伝えるのはHIDだけで、
Mass Storageは`needs_reinit`から意図的に外してあります（死んだストレージ
セッションがバス全体の再列挙を誘発して、動作中のキーボードを巻き添えにしない
ためです。[STORAGE.md](STORAGE.md)）。

その結果、**そこにもう無いデバイスがポートを占有したまま**になっていました。
`has_room`は「空きなし」と答え、空きポートスキャンは「既に駆動中」として
そのポートを飛ばし、挿し直しても何も起きません。

`detach_disconnected_hub_ports`が60フレームごとに使用中ポートを掃引し、
デバイス種別に関わらず切断されたスロットを外します。読むのはラッチされた
ステータスだけでデバウンスはしません。特定の失敗に反応するのではなく
タイマーで全ポートを見るので、`C_PORT_CONNECTION`がクリアされるまで
ラッチされる以上、待たなくても取りこぼしません。

この掃引は`has_room`の条件の**外**で走ります。`has_room`は「新しいデバイスが
入る余地があるか」であり、全ポート占有時に偽になります。それはまさに、抜かれた
デバイスがポートを掴んだままの状態でもあるので、掃引をそこに入れると
一番見るべき状態だけを見ないことになります。

切断を観測するたび`UsbHost::connection_epoch_at`の値が進みます。ファイルシステム層は
これをマウント時に記録し、進んでいたら媒体が持ち去られた可能性があるとして
扱います（[FILESYSTEM.md](FILESYSTEM.md)）。

数え方は**ポート単位**です。バス全体で1つにすると、ハブから何か1つ抜いただけで
同じハブ上の他のデバイスのマウントまで無効になります。それらは動いていないし
関係もありません。

ルートで起きた事象（USB-Aのケーブル自体が抜けた、ルートポートが接続変化を報告した）は
ハブごとバス全部を持ち去るので、こちらは別に数えて全ポートに効かせます。
`connection_epoch_at`はルートの数とポートの数を組で返し、両方を比較します。
和にしないのは、「和が同じなら何も起きていない」と言うために「どちらのカウンタも
減らない」という論証が要るからで、組で比べるなら論証が要りません。

## 電力問題の切り分け

デバイスが応答しなくなったとき、それが「プロトコル上の失敗」なのか「5Vが足りずに
デバイスが落ちた」のかは、コントローラーのステータスからある程度分かります。

HPRTの`prtconndet`（bit 1）・`prtenchng`（bit 3）・`prtovrcurrchng`（bit 5）は
**write-1-to-clearで、ISRが割り込みを確認する時点でクリアされます**。そのため失敗後に
前景でHPRTを読んでも痕跡は残りません。現在はISRがこれらを
`USB_PORT_EVENT_HISTORY`へ**ラッチ**し、`probe_port`がポートをenableできた時点だけが
クリアします（このドライバ自身のreset pulseが`prtenchng`を、場合によっては`prtconndet`も
立てるため、それより前でクリアすると以後のログが常に「デバイスが脱落した」と主張します）。転送失敗時のログと`usbhw`が同じ内容を表示します。

```text
USB:   port now: connected enabled powered
USB:   port since bus came up: OVER-CURRENT connect-change
```

読み方:

- **OVER-CURRENT** — コントローラーが過電流入力を見た。**これが出たら電力問題は確定**。
  ただしTab5のUSB-A 5Vスイッチのfault出力がSoCへ配線されていない場合、
  どれだけ電流制限がかかっても永遠に出ません。**出ないことは弱い証拠**です。
- **connect-change** — デバイスが一度バスから消えた。ブラウンアウトして再起動した
  デバイスはこうなります。「応答しないだけ」と「電源が落ちた」を分ける実用的な指標です。
- **enable-change** — コアがポートを無効化した（babble等）。

ハブ配下なら、より確実な指標があります。**USBハブはポートごとの過電流を
`wPortStatus`のbit 3で報告する**（規格）ので、`usbhub`の表示に`OVERCURRENT`が出ます。
バスパワーのハブは供給能力が低く、ここに出る可能性が高いです。

なお`ina226.rs`が測るのは**バッテリーパック**であってUSB-Aの5Vではないため、
VBUSの電圧降下は直接は見えません（システム全体の電流の跳ね上がりは見えます）。

## 転送失敗の巻き添え（FIFOの共有）

root-port reset成功後に再適用するFIFO分割はESP-IDF v5.5.3のHigh-Speed DWC既定balanced設定と同じ
RX/NPTX/PTX=`512/256/128` linesです。`usbcheck`のhost行は実レジスタ値を
`fifo=512/256/128`の順で表示します。

periodic TX FIFOとhost RX FIFOは**コントローラー全体で共有**で、失敗したチャネルの
持ち物ではありません。channel 0（control／bulk）の失敗回復で全FIFOをflushすると、
channel 1〜4で待機中のHIDのin-flightデータが道連れになり、**無関係なMSCの転送失敗が
キーボードのsessionを殺します**（実機では`usbfs on`でHIDを繋いだ後、`usbwritetest`で
HIDのポートが死にました）。

現在、channel 0のcleanupはperiodicチャネルがarmされている間は
**non-periodic TX FIFOだけをflushします**（channel 0が送信に使うのはそこだけです）。
skipした場合は`USB: periodic channels armed, flushed only the non-periodic FIFO`を
出します。代償は失敗した転送の残骸がRX FIFOに残り得ることですが、動作中のキーボードを
無関係なデバイスの失敗で壊すほうが実害が大きいと判断しています。Split転送の後始末は
従来どおり全FIFOをflushします（Splitとperiodicは`enter_split_mode`により排他なので、
巻き添えにするものが存在しません）。

## cleanupの3つの結果と失敗の伝播

FIFO flushの結果は`Fifo`（`nptx`／`ptx`／`rx`）ごとに**実行・timeout・skip**の3つを
別々に数えます（`usbhw`の`BOT: fifo-flush`／`fifo-timeout`／`fifo-skipped`）。
skipは失敗ではありません——上記のとおり動作中のHIDを守るための意図的な省略で、
cleanupはそのまま続行できます。timeoutだけが失敗です。1つのcleanupが返す
`CleanupOutcome`は、最初にtimeoutしたFIFOの名前を保持します。

cleanupの入口は用途ごとに分かれています。

| API | いつ | flushする範囲 |
| --- | --- | --- |
| `hcd::recover_failed_packet(FailureScope::Abandoned)` | timeout、halt不能、原因不明 | 全FIFO（periodic arm中はnon-periodic TXだけ） |
| `hcd::recover_failed_packet(FailureScope::ReportedPacketError)` | coreがpacket errorを報告済み | OUTはnon-periodic TXだけ、INは上と同じ |

この2つが channel 0のcleanupのすべてで、**正常に完了したpacketはどれも呼びません**。
かつては正常なBOT command境界でも同じ入口を呼んでいましたが、READ 16回ごと・WRITE毎回の
予防cleanupは3構成の実機A/B（各1000 read／100 write）を経て撤去しました。deviceへ
Mass Storage Resetを送るBOT Reset Recoveryは`bot.rs`側の別手順で、host側cleanupが
成功したときにだけ実行します。

flushがtimeoutした場合は、**そのcleanupが用意していたcommandを開始しません**。

- 実失敗後のrecoveryでtimeoutした場合、BOT Reset Recoveryは実行せずMSC sessionを
  使用不能にします。Reset Recoveryのcontrol転送とその後のbulkは、いま空にできなかった
  FIFOを通るので、実行しても「回復した」という誤った結論しか得られません。
- packet retryのcleanupでtimeoutした場合、同じpacketを再送しません。元の失敗outcomeを
  そのまま返し、上のcommand単位の処理へ委ねます。

いずれの場合もUARTへ`could not flush the <fifo> FIFO`とその後の判断を出し、
`MSC: cleanup-failed`へ加算します。

なお`fifo-skipped`は**HIDを繋いでいても0のことがあります**。skipはpersistent periodic
channelがarmされている間だけ起き、`enable_periodic_hid`はsplitが要る経路を拒否するので、
High-Speedハブ配下のLow-Speed HIDは構造上periodic channelを取れません。frame poll
fallbackで動いているHIDはchannel 1〜4を持たないため、cleanupは3 FIFOとも実行します。
A／B／C 3構成の実機確認では`fifo-skipped`は一度も発火していません。この経路は正常なhardwareでは到達できないため、
`usbcachefail`の[4/4]がflush timeoutを注入して確認します
（[`DIAGNOSTICS.md`](DIAGNOSTICS.md)）。

## periodic HIDの停止検出

Interrupt IN endpointは、descriptor DMAではNAKでhaltしません。**アイドル中の
キーボードと、コアが面倒を見なくなった死んだチャネルは、pendingマスクだけでは
区別できません**（どちらも「まだ完了していない」）。区別できるのはチャネル自身の
`HCCHAR.ChEna`で、コアがまだpollしているチャネルはこれが立っています。

`take_periodic_hid_report`は、pendingかつ`ChEna`が落ちている場合に割り込みを一時maskし、
atomic pendingとhardware HCINTを再確認します。正常な完了は`ChEna`を落としてからISRへ
公開されるため、この再確認がないと完了直前のreportを停止と誤認する競合が生じます。
再確認後も完了がなく`ChEna`が落ちている場合だけ「停止」と判定し、
`USB HID: periodic channel stalled, channel=N`とHCCHAR／HCINT／HCINTMSK／HAINTMSK／
HCFG／port状態を出したうえで、そのslotのgenerationを進めます。以後の読み出しは
errorになるので、HIDドライバの連続エラー閾値が再列挙を要求します。この検出が無いと、
**キーも来ず、ログも出ず、`usbinfo`にはデバイスが残ったまま**という状態になります
（実機で発生しました）。

## 転送失敗からの自動復帰

すべてのcontrol／bulk転送はチャネル0を共有します。したがって**チャネル0をhaltできない
まま放置すると、以後のcontrol転送まで失敗し、バス全体が死んだままになります**。実機では
1回のBulk timeoutからこの状態に入り、`usbrescan`でも復帰せず（HPRTはconnected／enabled
のまま、列挙の8 byte device descriptor読み出しがpacket errorで失敗）、本体をcold boot
するまで戻りませんでした。

復帰の範囲は、故障が確認できた層に限定します。

1. **チャネル0をhaltできなかった場合だけ**コントローラー全体を「バス使用不能」と記録し
   （`hcd::note_bus_unusable`）、`UsbHost::needs_reinit`経由で`InputManager`が次の
   フレーム境界に全バスを再列挙します
   （`USB: channel 0 did not halt; the bus needs re-enumeration`）。この状態では次のcontrol／
   bulk転送を安全に開始できないため、HIDを含む全sessionの作り直しが必要です。
2. **BOT Reset Recoveryが失敗した、または2回続けて効かなかった場合はMSC sessionだけを
   使用不能にします**（`this session needs re-enumeration`）。実機ではこのときもチャネル0は
   正常にhaltしており、別チャネルのHIDまで壊れた証拠はありません。したがって自動の全バス
   再列挙は行わず、以後のMSC commandを即時に失敗させます。`usbrescan`を明示的に実行すると
   sessionを作り直します。BOT Reset Recoveryの成功はcontrol転送が通ったことしか示さないため、
   直後のcommandも失敗したら「回復していない」と判定します（成功したcommandだけが連続回数を
   リセットします）。

再列挙自体もバス全体をリセットするため、**取り付けた直後に必ず失敗するデバイスがあると
「attach→失敗→リセット→attach」のループになり、同じバス上の正常なデバイスまで
毎秒何度も落とされます**（実機ではSplit経由のHIDがこれを起こし、MSCが巻き添えになりました）。
そのため`InputManager`は、stale sessionによる再列挙が連続する場合にバックオフします。
1回目は即座（ケーブルを抜いた通常のケースで待たされないため）、2回目以降は
60フレーム×連続回数（最大600フレーム＝約10秒）空け、600フレーム無事に経過したら
連続回数をリセットします。`USB: repeated stale sessions, next rescan in frames=`が
そのログです。

`usbwritetest`と`usbzero`は、MSC sessionまたはバスが使用不能と記録された時点で残りの
手順を打ち切り、`usbrescan`を案内します。どのみち全部同じ失敗をするうえ、1手順あたり
数秒かかるためです。復帰後に残骸を消すには`usbzero <lba>`を使います。

3. 再列挙で「HPRTはconnectedなのにデバイスが応答しない」（ポートがenableしない、または
   列挙が失敗する）と判定した場合、
   `USB: device unreachable after a port reset; power-cycling USB-A`を出して**VBUSを
   1秒切って入れ直し**、もう一度スキャンします。port resetは`probe_port`が既に行っている
   ため、ソフトウェアに残された手段はこれだけです。
4. **ハブ配下のデバイスが列挙できない場合は、そのハブポートの電源を切ります**
   （`USB: power-cycling hub port N`）。**セルフパワーハブではroot側のVBUSを切っても
   下流ポートの電源は落ちない**ので、1と2では何も起きません。ハブがper-port電源
   スイッチングに対応している場合だけ実行します（gangedのハブは他のポート＝
   キーボード等まで巻き添えにするため）。電源復帰後にdebounceして1回だけ再列挙し、
   それでも駄目なら`USB: still unreachable after a port power cycle; hub port N`を出して
   そのポートを保留にします。

自動power cycleは（root VBUS・ハブポートのどちらも）前回から30秒以上経過している場合
だけ実行します。

BOT層は、MSC sessionまたはコントローラーが使用不能と記録されている間**コマンドを送らず
即座に失敗します**（`USB BOT: session is unusable, skipping commands until re-enumeration`）。
再列挙までに投げたcommandは、1本ごとにtimeoutとReset Recovery失敗を積み上げるだけで、
実機ではこれが「1回の書き込み失敗が数分のログとシェルの無応答」になっていました。

VBUSの自動power cycleは1回の再列挙につき最大1回で、さらに前回から30秒以上経過している
場合に限ります（`POWER_RECOVERY_INTERVAL_MS`。ハブポートの電源断とも共有します）。power cycleは1秒以上ブロックしバス上の
全デバイスを落とすため、応答しないデバイスが刺さったままフレームループが数秒ごとに
電源を切り続ける状態を避けるためです。`mix`の明示的なpower cycleも同じ間隔を共有します。

増分スキャンで列挙に失敗したポート、または対応class driverが無いポートは、その物理接続を
保留状態として記録します。以後は約1秒ごとにHubの接続／change bitだけをquietに読み、同じdeviceを
reset・再列挙し続けません。抜き差しを検出した場合、または`usbrescan`／`mix`が明示的にfull rescan
した場合だけ列挙を再試行します。これにより列挙失敗ログがconsole操作を妨げる連続出力になりません。
device descriptorまたは`SET_ADDRESS`までに失敗したポートは、次のoccupied portをresetする前に
`CLEAR_FEATURE(PORT_ENABLE)`で無効化します。失敗deviceをDefault state（address 0）のままenableして
おくと、後続portのaddress 0列挙と同時に応答して、先に失敗したMSC一台が後続のkeyboard／mouseまで
列挙不能にするためです。無効化しても接続statusは残り、次のPORT_RESETで再enableできるので、物理的な
抜き差しまたはfull rescanによる再試行は妨げません。

セルフパワーハブはupstreamを抜いても下流deviceへ給電し続けるため、再接続時に古いdevice
address／configurationや`C_PORT_RESET`が残り得ます。下流port列挙では古い`C_PORT_RESET`を
先にclearし、新しい`SET_FEATURE(PORT_RESET)`後のreset完了change、または実際に観測した
RESET assert→deassertを待ってからaddress 0へアクセスします。reset要求直後の最初のstatusが
まだRESET=0でも完了とは扱いません。給電中ハブへHIDとMSCを事前接続した実機では両方を初回認識し、
FS-onlyの`ut 100`をretry 0で完走しています。給電したままの上流再接続も5/5回、HIDの
抜き直しなしで認識しました。

## 転送方式の現状

- HID BootのInterrupt INは、対象routeで空きがあればchannel 1〜4のいずれかを確保し、
  `HCCHAR.eptype=INTR`と32-entry periodic frame listで常時待機します。対象はrootへFS/LSで直結した
  keyboard／mouseと、Splitが生じないFull-Speedハブ配下のkeyboard／mouseです。最大4 endpointの
  channel bitを共有frame listへ`bInterval`ごとに合成し、QTD、64-byte report buffer、data toggle、
  世代token、IRQ pendingはchannelごとに独立しています。割当て不能時とHigh-Speedハブ配下は、
  controller-wide DMA modeとSplitの調停が未実装なため、従来の`BULK`分類＋frame pollへfallback
  します。root直結High-Speed HIDもinterval解釈の実機確認前なのでfallbackです。
  ハブの初回／増分スキャンでは、occupied portをすべて列挙し終えるまでperiodic channelを
  開始しません。低い番号のportに事前接続されたHIDが先にperiodic DMAを開始すると、後続portの
  channel 0列挙controlと競合し、HID後挿し時だけ成功する状態になったためです。全port処理後に
  MSC併用ならchannel 0逐次化、HIDだけならperiodic開始を一度だけ選択します。
- 転送はチャネル0を使った逐次・同期方式で、真の並列転送はしません。
  [`USB_INTERRUPT_REFACTOR_PLAN.md`](USB_INTERRUPT_REFACTOR_PLAN.md) Stage 1として、
  High-Speed DWCのsource 93をCLICへルーティングし、channel／root-port状態を短いISRで
  Atomicスナップショットへ保存します。通常のcontrol／bulkとsoftware SplitはAtomic／HCINTを
  再確認してから`WFI`し、USB完了割り込みで起床します。直結HIDのidle NAKはdescriptor DMAが
  haltしないため、`CompletionWait::PollIdleNak`を明示した短いbounded pollを移行中のfallback
  として残します。診断ログの抑制指定は待機方式に影響しません。source 93からの
  channel／root-port割り込み、LSキーボード入力、切断後の即時再列挙は実機確認済みです。
  MSCとLS keyboard列挙controlのWFI完了待ちは実機確認済みです。通常のunsplit転送は
  世代付き固定`Channel0Transfer`へsubmitし、IRQ後に同じtokenでreapします。この内部構造の
  HID／MSC実機回帰も確認済みです。
- MSCのBulk QTDはCPU周波数から約1秒で区切って最大4回再投入し、合計約5秒、control packetは
  約1秒になるよう算出します。CPU 360 MHz化後も固定iteration値の実時間が短縮されないための設定です。BOT commandが
  transport途中で失敗した場合は、チャネル0をidleへ戻した後、Mass Storage Reset、
  Bulk IN／OUT両endpointのhalt解除、DATA0へのtoggle同期からなるBOT Reset Recoveryを
  実行します。Mass Storage Reset直後はdeviceが回復するまで150 ms待ってから最初の
  `CLEAR_FEATURE(ENDPOINT_HALT)`を送ります。この待機で実機のRecovery成功率は改善しましたが、
  同じ媒体でもCLEAR_FEATUREが失敗するrunは残ります。安全に再送できるREAD(10)だけはRecovery後に1回再試行し、再試行数を
  session内で計数します。WRITE系commandの自動再送は行いません。
  descriptor DMAのQTD status 1はpacket errorとして扱います。CBW、CSW、Bulk IN、Splitと
  MPS以下のBulk OUTは1 endpoint MPS以下に限定し、toggleを進めず同一DATA PIDを50 ms間隔・
  最大20回の範囲で再送します。ACKだけを
  失ってdeviceがpacketを受理済みでも、同じPIDのduplicateは再消費されません。4 KiB Bulk INも
  MPS単位に分割し、各packetの完了とDATA PIDをsoftwareが確定してから次へ進みます。
  非Split BOT Bulk OUTも1 packet QTDごとにchannelをarmします。v36〜v39で試した複数packet
  QTD／複数QTD listはFull-Speed WRITEを悪化させたため撤回しており、現行経路では使いません。
  HCCHAR MC/ECは0です。
  reported packet error後の再投入にはcontroller cleanupが必要です。全cleanupを外した実機では
  同じstatus 1が20回続いてWRITEが10/10から0/10へ後退しました。現在はOUT packet errorでは
  そのpacketの送信残量を持ち得るnon-periodic TX FIFOだけをflushし、無関係なRX／periodic TX
  FIFOは触りません。IN packet errorとchannelがhaltしないtimeoutは従来の保守的cleanupです。
  reported OUT packet errorは、channel halt後にnon-periodic TX FIFOだけをflushし、同じDATA PIDを
  再送します。descriptor-DMA modeをoff→onするv33／v34の実機A/Bは、B1 raw停止packetで確実に
  反復実行しても3/32の結果を変えなかったため撤去しました。IN errorとchannelがhaltしないtimeoutは
  RX residueも考慮した従来の保守的cleanupです。
  HCCHARのMC/ECは通常の非周期descriptor-DMA Bulk／controlでは0、split transactionでは1にします。
  全direct channelを1にした実機A/BではB2 READが100/100から4/100へ後退し、長いWRITE QTDだけ1に
  限定してもWRITE 1/10のまま改善しなかったため採用しませんでした。
  13/36/8 byteの短いIN応答は、QTD長をMPS倍数に保つ内蔵SRAM staging経由で受信します。
  channel 0のdescriptor完了はQTD statusだけでなくHCINT.XferComplとQTD Active解除も検査します。
  ChHltdだけの古い完了snapshotや、hardwareがまだ所有するQTDを新しいpacketの成功として回収しません。
  正常なcommand境界ではcleanupを行いません。かつては連続READ(10)の16回ごとと各WRITE(10)の
  直前にchannel／FIFO cleanupを行っていました——FS-onlyで最短33回、High-Speed直結でも52回後に
  Bulk INからEP0まで無応答になる故障の緩和策です。上のdescriptor完了検査とcache同期契約を
  入れたうえで実機A/Bしたところ、3構成すべてで予防cleanup無しの`usbcheck 1000`（read）と
  100回のWRITEが通ったため撤去しました。なお当時、channel回復とDWC FIFO flushを外した実機A/Bでは、
  直前commandのCSWが次commandへ残って2回目のWRITEで停止したため、controller側residueのcleanupも
  維持します。実際のtransport failureでは従来どおりcleanup後に完全なBOT Reset Recoveryを実行します。
### 実転送長とretry安全性の契約

**`actual`は1箇所でだけ導出します。**QTDの残量が byte 数として意味を持つのは、その
descriptorをhardwareが実際に書き戻した場合だけです。次の3つは`actual`を「不明」とし、
0とは区別します。

- hardwareがまだdescriptorを所有している（`QTD.Active`が立っている）
- 残量が要求長より大きい
- **channelが`HCINT.XferCompl`を報告していないのに、control wordが丸ごと0**——実機の
  Full-Speedハブ経路で、timeoutしたOUTのcontrol wordが`0x00000000`で残っていました。
  ここから読んだ残量0は「要求byteが全部動いた」を意味し、同時に「descriptorが
  書き戻されていない」も意味します。区別が付かない値をbyte数として使えません。
  全ビット0はそれ自体が矛盾していて、submit時に書いた`QTD_EOL`／`QTD_INTR_CPLT`を
  1つも持たないまま全量転送を主張しています（hardwareはこれらを保持します——完了した
  SETUPは`0x07000000`、packet errorは`0x16000200`）。

`XferCompl`で完了したpacketにはこの検査を適用しません。完了経路の計算は受入試験matrixの
全構成で成立が確認済みで、そこへ新しい失敗条件を持ち込みません。

判定はこの2つの実測signatureまで狭めてあります。最初は「`XferCompl`が無いなら`QTD_EOL`が
必要」という広い規則にしたところ、**それまで正常に再送できていたFull-Speedハブ経路の
packet errorが`Unknown`になり、10回中10回成功していた書き込みが0回になりました。**
安全側に倒れる規則でも、どの読み値が「あり得ない」のかは正しくなければいけません。

**再送の可否は、packetが「放棄された」のか「失敗を報告された」のかで分けます。**

- **channelがhaltせず放棄したpacket**は、`actual`が`Known(0)`のときだけ再送します。
  completionが無いのでcoreがpacketの途中だった可能性を否定できません。実機では
  64 byteのOUTのdescriptorが`0x06000000`（status成功・残量0）で、`HCINT`は
  `0x00000000`でした。この残量をbyte数として読むと「全部出た」になり、4回再送されて
  いました。現在はtransport errorとして即座に上げ、
  `USB BOT: refusing to resubmit after ...`を出します。
- **coreがpacket error（QTD status 1）を報告した1 packet QTD**は、残量に関係なく再送します。
  USB packetは不可分で、deviceは丸ごと受け取るか全く受け取らないかのどちらかです。
  ここでのQTDは1 packetしか運ばないので、失敗の報告は「受け取らなかった」か「受け取った
  がhandshakeが失われた」を意味し、同じDATA PIDでの再送が両方を覆います——toggleを
  進めた後のendpointは重複を捨てます。Full-Speed経路ではpacket errorが`ChHltd`とQTD reapまで
  完了していても、再送前のchannel／FIFO cleanupが必要です。これを外す実機A/Bでは同じ
  status 1が20回続いてWRITE 10/10が全滅したため、同一PIDを保ったままcleanupして再armします。
- v36〜v39で試した複数packet QTD／複数QTD listはB2の正常WRITEを後退させたため使用しません。
  現在の非Split BOT OUTはCBWとdataを含め、すべて1 packet QTD／1 channel activationです。

**このcoreはpacket error時に不可能な残量を書き戻します。**64 byteのOUTに対して100,489、
31 byteのCBWに対して128という値を実測しました。いずれも`Active`解除・status 1・
`EOL`／`IOC`保持の正規のwritebackで、byte数の欄だけが残量として成立していません。
これらは`Unknown`として弾き、`usbhw`／`usbcheck`の`impossible-len`で数えます。
**「起きない」ことにはできない現象なので、byte数として使わないことだけを保証します。**

**短いOUTは上位層へ届きません。**呼び出し側はMPS単位で分割しているので、要求より少ない
byteで完了したOUTはdeviceがpacketの一部だけ受け取ったことを意味します。これを成功と
すると、data toggleと呼び出し側のoffsetが、届いていないbyteの分だけ進みます。

### BOT phaseとCSWの検証契約

CBWは31 byte、CSWは13 byteを**ちょうど**転送した場合だけ成立します。CSWはsignature
`USBS`、現在commandのtag、status 0（PASSED）または1（FAILED）を要求し、status 2
（Phase Error）と未定義statusはtransport errorとしてBOT Reset Recoveryへ送ります。
`dCSWDataResidue`は破棄せず、commandの期待data長とhost実転送長に突き合わせます。

- data INは`host actual == expected - residue`でなければ矛盾です。
- data OUTはhostが全量を転送していることを要求します。deviceが未処理byteをresidueで
  返すことはあるため、wrapperの検証後にMSC command層が固定長commandかどうかを判定します。
- READ(10)、WRITE(10)、READ CAPACITY(10)、REQUEST SENSE、標準INQUIRYはPASSEDに加えて
  全量・residue 0を要求します。可変長のINQUIRY VPDは実受信長とresponse headerから
  有効範囲を決めます。

data INを省略して13 byteのCSWが先に届いた場合は、payloadへcopyする前にwrapperとして
識別します。current tagなら同じCSWをもう一度待たずにcommand resultへ使い、前commandのtag
ならstale CSWとしてtransport errorにします。signatureだけではpayloadと決め分けず、13 byte
ちょうどでstatusが定義範囲にあることまで確認します。解析と長さ／residue判定は
`tab5-bot-protocol`へ分離し、MMIOなしのhost testで検証します。

### DMA bufferとcache同期の契約

**controllerが読み書きするのは、HCDが所有する整列済みbufferだけです。**上位が
`run_packet`へ渡す`&mut [u8]`にDMAは触れません。13 byteのCSW、8 byteのSETUP packet、
4 KiB読み出しの途中から始まるsliceのいずれも、呼び出し側の努力ではcache line境界から
始められないためです。alignmentを上位へ伝播させる方法も成立しません——MPS単位の部分slice
を作った時点で次のpacketで崩れます。

- channel 0は`PacketStaging`（64 byte整列、512 byte＝High-Speed Bulk MPS）を1 QTDごとに
  所有します。OUTは上位sliceからstagingへcopyしてから転送し、INはstagingで受けて実受信
  byte数だけ上位sliceへcopyします。512 byteを超える要求は転送せず拒否します。
- 非Split channel 0のdescriptorはinternal RAMに固定した2-slot QTD bankから交互に選びます。
  各slotは512 byte境界・512 byte strideで、retryが直前に失敗した物理QTD addressを即時再利用せず、
  次packetも直前成功QTDとは別addressになります。channel 0は同期実行なので同時ownershipは1 slotだけ
  です。release ELF検査がbankのaddress、alignment、2×512 byteのsizeを検証します。
- Split packetは`SplitStaging`（64 byte整列、64 byte＝1 cache line）を使います。Splitは
  buffer DMAなので、`HCDMA`にはこのbufferのaddressを直接書きます。
- 常設periodic HIDのQTD bank（512 byte整列）、frame list（512 byte整列）、report buffer
  （64 byte整列、1 slotが1 cache line）はstaticに確保します。
- cache maintenanceの範囲は、開始addressをcache lineへ整列させ、長さを行単位へ切り上げます。
  切り下げは行いません——他のownerのdirty lineを巻き込むためです。各objectは自身の
  alignment以上の大きさへpaddingされるので、切り上げた範囲がobjectの外へ出ることはありません。

**cache同期の拒否は転送の失敗です。**開始addressが行境界でない場合、またはROM routineが
拒否した場合、channelをarmせずに`PacketOutcome::CacheSyncFailed`を返します。受信後の
invalidateが拒否された場合も、上位bufferへは1 byteも公開しません。DMAが書く前のCPUの
copy——0埋めのstaging——を渡すと、成功した短いpacketと見分けがつかなくなるためです。
同じbufferは次も拒否されるので、この結果は再送しません。

release ELFの配置検査（`tools/check_elf_layout.py`）がstaticなDMA objectのaddressと
alignmentを検証します。拒否経路そのものは`usbcachefail`コマンドで実機確認します——正常な
hardwareは拒否しないので、注入する以外にこの経路へ到達する方法がありません。注入は
自分で減っていくcounterで、1回の拒否ごとに1消費されます。恒久的なmodeではありません。

- rootへ直接接続したHID Boot keyboardは、attach時にstaticな512-byte aligned
  32-entry frame list／QTD bankを割り当て、`HCCHAR.eptype=INTR`で常時待機します。report完了IRQを
  前景がtakeして次QTDをrearmするため、idle中にchannel 0をpollしません。この常設経路は
  root直結LS keyboardとFull-Speed mouseで実機確認済みです。keyboardの10秒idle比較で
  poll／submit／cancelは増えず、
  key reportだけchannel 1で完了・rearmしました。channel 1〜4 allocator、root直結mouse、
  Full-Speedハブ配下の複数HIDも実装済みです。後者はHigh-Speedハブを`usbfs on`でFull-Speed
  列挙する代替試験により、keyboard=channel 1、mouse=channel 2の同時動作を確認済みです。
- **MSCとHIDが同じバスに存在する場合は、HIDのpersistent periodic DMAを停止し、control／
  bulk／HIDをchannel 0で逐次実行します。** Full-Speed固定の実機試験ではperiodic QTDをarm
  したままのMSC READ(10)が33回成功後にtimeoutし、Recovery後の再送も同じ形で失敗しました。
  RX FIFOを共有する複数DMA channelの同時稼働を避けるための安定性優先policyです。切替時は
  periodic channelをhaltし、完了済みreportの有無をQTD／HCINTから回収して次のDATA PIDを
  frame pollへ引き継ぎます。device resetや再列挙は行わず、`USB: MSC present, serializing HID
  and bulk on channel 0`を出します。MSCが無い構成では従来どおりperiodic channelを使います。
- High-Speedハブ配下のFS/LS HIDは、Splitがbuffer DMA、periodicがdescriptor DMAという
  controller-wide制約のため、channel 0のserialized Split fallbackを使います。各SSPLIT／CSPLIT
  phaseはIRQ＋WFIで待ち、レジスタをspin pollしません。HCDはSplit modeを排他状態として管理し、
  periodic channelが残っていればDMA modeを切り替えずエラーにします。`usbhw`の`IRQ split`で
  packet／round／conflictとmode activeを確認できます。SSPLITはmicroframe 0〜5に限定し、TTが
  downstream transactionを処理する時間として最初のCSPLITを2 microframe後、NYET後の再CSPLITを
  1 microframe後に投入します。keyboard＋mouseの旧High-Speedハブ実機回帰では
  4745 packets／58727 roundsをIRQ＋WFIで処理し、poll／conflictはいずれも0でした。
  同じハブへHigh-Speed USBメモリを追加し、Split HIDを維持したままMSCの`INQUIRY`、
  `TEST UNIT READY`、`READ CAPACITY(10)`、`READ(10)`も実機成功しています。一方、今回の
  High-Speedハブ＋Low-Speed HIDではmicroframe間隔なしの実装が`HCINT=0x82`で列挙失敗しており、
  第19版で列挙・class attachまでは成功しました。続く最初のInterrupt INがCSPLITで`0x82`に
  なった原因は、直結fallback用の`HCCHAR.EPType=BULK`をSplit tokenにも流用していたことです。
  第20版はSplit HIDだけをdescriptorどおり`EPType=INTR`にして文字入力まで成功しましたが、idle時の
  CSPLIT NYETを最大5000 round追跡して`giving up mid-split`と長いfreezeを繰り返しました。
  第21版は周期InterruptだけSSPLIT＋最大3回のCSPLITを同じHigh-Speed full frame内で試し、NYETのまま
  scheduling windowが終わればfull-frame境界を待って通常の`Timeout`（reportなし）として終了します。
  Control／BulkのNYET回収規則とhard capは変更しません。第21版の実機ではエラーとfreezeが消えました。
  ただしSplit packet数が約57回/秒で描画周期に縛られ、descriptorの`bInterval`より遅いことが次の
  入力遅延になりました。第22版は既存1 kHz tickの非描画wakeからSplit keyboardだけを
  `bInterval` msごとにpollし、受信keyを16 event queueへ保持します。通常のframe境界ではqueueを
  消費するだけなので、USBのsampling周期と表示の更新周期を分離します。起動時に実際の値を
  `USB HID: Split foreground poll interval ms=N`で表示します。新しいHigh-Speedハブ＋Low-Speed
  keyboardの実機で50,661 packet／202,076 roundを処理し、入力遅延・エラー・freezeなし、
  mode conflict 0、stale token 0、port event 0を確認しました。この第22版時点ではMSC併用回帰は
  未確認でした。
  第23版はSplit HIDの転送がstaleになったとき、まず所有する下流portのcurrent connection／changeを
  hub EP0で確認します。抜去または差し替えなら該当slotだけを破棄し、root busをresetしません。
  同じハブ上のMSC address／BOT sessionは維持され、HID再挿入は既存の増分port scanが処理します。
  物理抜去と同時に走っていたSplit packetの最初の`XACTERR`は検出契機として1回だけ出ます。
  第23版の抜去・再挿入動作は実機確認済みです。
  第24版はperiodic SSPLITを次のHigh-Speed microframe 0へ位相合わせします。`bInterval=1ms`の
  system tickとUSB SOFが固定位相でも、遅いslotから開始してCSPLIT窓を失い続けません。また周期転送の
  NAKは通常の「reportなし」としてそのpollを終了し、Control／Bulk用の「新しいSSPLITから再試行」へ
  入りません。第23版で145,394 packet／436,167 round（ほぼ3 round/poll）だった取りこぼしへの修正で、
  Low-Speed Interruptの規格上の最小interval 10msも適用します。この実機のdescriptor値1msをそのまま
  使った第23版はTTを毎full frame占有して58万IRQまで増えました。第24版は
  `USB HID: invalid Low-Speed bInterval, descriptor=1`を出して10msへ補正し、起動時の採用値も
  `Split foreground poll interval ms=10`で確認できます。High-Speedハブ＋Low-Speed keyboardの
  実機では入力が安定し、エラーログなし、10秒静止時のSplit packet増加が約1,000回
  （約100 packet/秒）であることを確認済みです。同じハブへHigh-Speed MSCを追加した最終回帰も、
  `ut 100`が100/100、failure／mismatch 0、packet／command retry 0でPASSしました
  （当時は予防再同期6回。現在は予防cleanup自体がありません）。
  直後の`usbhw`はSplit 1,126 packet／2,370 round、conflict 0、active 0、stale token 0、
  port eventなしでした。
- USB-Aの5V（VBUS）は2個目のPI4IOE5V6408（E2、I2Cアドレス`0x44`）のbit 3です。
  同じexpanderは電源断や充電制御とも共用するため、書き換えはビット単位の
  read-modify-write（`hcd::set_pi4ioe2_output_bit`）で行います
  （[`CONSOLE_SHELL.md`](CONSOLE_SHELL.md)の「全体電源断」も参照）。
  `mix`はroot resetを3回使い切ってもMSCを取得できない場合に限り、このbitを1秒offにして
  Hubとdownstream deviceを完全resetします。registryの全sessionを先に破棄し、再投入後にfull
  rescanするため、電源断前のdevice addressやtoggleを再利用しません。1試験中の自動実行は最大1回です。

## Split Transaction

High-Speedホストの下にFull/Low-Speedデバイスを繋ぐには、ハブが代理で低速の
転送を行うSplit Transaction（`HCSPLT`のSSPLIT/CSPLIT）が必要です。
Espressifの資料はESP32-P4を非対応（`OTG_SINGLE_POINT=1`）としていますが、
実機のシリコンは`GHWCFG2.SingPnt=0`を報告し`HCSPLT`も実在するため、資料の側が
誤りです。`usbhw`コマンドがこの検査（`hcd::probe_split_support`）を実行します。

[`USB_HOST_PLAN.md`](USB_HOST_PLAN.md) Stage 6でSplit Transactionを実装したため、
Stage 4の回避策だったバス全体のFull-Speed固定（`FORCE_FS_LS_ONLY_HOST`）は
既定で`false`です。診断時だけ`usbfs on`で同じ設定をruntimeに有効化して即時再列挙でき、
`usbfs off`でHigh-Speedへ戻せます。High-SpeedハブをFull-Speedで列挙し、Splitのない
複数periodic HID構成を再現するために使います。

## デバイス情報の表示（`lsusb`）

`lsusb`はバスに何が繋がっているかだけを見るコマンドです。周りの`usb*`コマンドが
このプロジェクトのUSBスタック自体（ポートのレジスタ、Split対応、BOTセッション、
スキャン所要時間）を診断するためのものであるのに対し、`lsusb`はデバイスが自分に
ついて何と言っているかしか表示しません。表示処理は`src/app/lsusb.rs`にあり、
`sdmbr`／`usbmbr`が`src/app/mbr.rs`へ出力整形を渡しているのと同じ形です。

引数なしではハブを介したツリーを表示します。読むのは`UsbHost`が列挙時に保存した
デバイスレコードだけなので、**バスへの転送は一切発生しません**。したがって表示は
最後のスキャン時点の状態です（挿したばかりのデバイスは、1秒周期の空きポート
スキャンが拾うか`usbrescan`後に現れます）。

```
USB-A: High-Speed
[1] 05E3:0608 Hub, High-Speed
    driver: hub, 4 ports; usbhub shows their status
    if0: 09/00/01 Hub
  port 1: [2] 046D:C31C per-interface, Low-Speed via split
          driver: HID Boot keyboard
          if0: 03/01/01 HID Boot keyboard
          if1: 03/00/00 HID
  port 3: [4] 0781:5567 per-interface, High-Speed
          driver: Mass Storage (Bulk-Only Transport)
          if0: 08/06/50 Mass Storage SCSI, Bulk-Only
  port 5: device present, enumeration failed
3 devices; 'lsusb <address>' for one device's descriptors
```

複合デバイスは`bDeviceClass`が00（per-interface）で、デバイス記述子だけでは何を
するデバイスか分かりません。そのため各interfaceを1行ずつ、`class/subclass/protocol`と
その意味付きで下にぶら下げます。角括弧の番号はUSBアドレス（直結・ハブ自身が1、
ハブ配下はポート番号+1）で、詳細表示の引数になります。

クラスドライバが無いデバイスも`driver: none for this class`として表示します。
レジストリはドライバの配列（`slots`）とは別に列挙できた全デバイスのレコード
（`records`）を持っており、後者にはハブ自身と未対応クラスのデバイスも入ります。
列挙自体に失敗したポートは`device present, enumeration failed`として、そこに何か
挿さっている事実だけを出します。

`lsusb <address>`は指定デバイスの主要な記述子を表示します。デバイス記述子、
コンフィグレーション記述子のヘッダ、各interfaceとそのendpoint、HIDデバイスなら
HID記述子です。文字列記述子（`iManufacturer`／`iProduct`／`iSerialNumber`／
`iInterface`）はこのときだけ取得します。**列挙時には取りません**——起動時スキャンは
ストレージ選択の判断時間に直結しており（[`USB_MSC_BOOT_MARGIN_PLAN.md`](USB_MSC_BOOT_MARGIN_PLAN.md)）、
そこへ制御転送を増やさないためです。文字列を持たないデバイスは`(none)`、LANGIDを
返さないデバイスは`strings: device reports none`と表示します。取得した文字列は
表示先はコンソールの半角固定セルなのでASCIIへ畳み、非ASCIIは`?`にします。

```
device 2 on hub port 1, Low-Speed
  reached by split transactions through the TT of hub 1 port 1, Low-Speed
  driver: HID Boot keyboard
Device Descriptor:
  bcdUSB 1.10  bMaxPacketSize0 8  bNumConfigurations 1
  idVendor 0x046D  idProduct 0xC31C  bcdDevice 64.00
  bDeviceClass 00/00/00 per-interface
  iManufacturer 1: Logitech
  iProduct 2: USB Keyboard
  iSerialNumber 0: (none)
Configuration Descriptor:
  bConfigurationValue 1  bNumInterfaces 2  wTotalLength 59
  bmAttributes 0xA0 bus-powered, remote wakeup  bMaxPower 100 mA
  Interface 0 alt 0: 03/01/01 HID Boot keyboard, 1 endpoint
    iInterface 4: Keyboard
    HID Descriptor: bcdHID 1.11  country 0  report descriptor 65 bytes
    Endpoint 0x81: Interrupt IN, mps 8, interval 10
  Interface 1 alt 0: 03/00/00 HID, 1 endpoint
    HID Descriptor: bcdHID 1.11  country 0  report descriptor 159 bytes
    Endpoint 0x82: Interrupt IN, mps 4, interval 10
```

コンフィグレーション記述子は1デバイスあたり256 byteまで保存します
（`protocol::CONFIG_BUFFER_MAX`）。これを超えるデバイスは末尾のinterfaceを読めて
おらず、`lsusb`はその旨を1行出します。読めていない部分にはクラスドライバも
アタッチできません。

## シェルコマンド

| コマンド | 内容 |
| --- | --- |
| `lsusb [address]` | デバイス情報の表示（上記）。診断ではなくデバイスを見るためのコマンド |
| `usbinfo` | 現在USB-Aに繋がっている全デバイス（直結・ハブ配下）の一覧 |
| `usbrescan` | ポートをリセットして再列挙 |
| `usbfs on\|off` | FS/LS-only host modeを切替えて即時再列挙（診断用、既定off） |
| `usbhub` | ハブの記述子と全ポートの状態 |
| `usbhw` | DWCコアの`GHWCFG`ダンプと`HCSPLT`の実在検査 |
| `usbperiodic` | 常設periodicが無効な最初のHIDでchannel 1＋frame listを1転送だけ試験（旧Go/No-Go診断） |
| `usbvbus <0-7> on\|off` | PI4IOE2（`0x44`）の出力ビット直接操作（bit 3がVBUS）。診断用 |
| `usbmsc`／`usbread`／`usbmbr` | USB Mass Storage（[`STORAGE.md`](STORAGE.md)） |
| `usbwritetest <lba>` | USB MSCの1ブロック書き込み・照合・復元 |
| `usbrawcheck <lba> [writes] [span] [gap_ms]` | filesystem外の犠牲範囲でraw WRITE・照合・復元。gapはWRITE command間の0〜2000 ms（既定0） |
| `usbmultiwrite <lba> <2\|4\|8>` | Stage 7の複数block WRITE(10)診断。10回の書き込み・媒体照合・前後guard検査・原本復元を行い、packet actual／PIDとCSW residueをUARTへ記録。2媒体×2 topologyの受入後も回帰手段として残す |
| `usbzero <lba> [count]` | USB MSCの1〜8ブロックをゼロで上書き（破壊的）。テスト失敗後の後始末 |
| `ut [count]` | USB MSCの同一4 KiBをread・比較（read-only、既定100回、Recovery再送数を表示） |
| `usbmargin [rounds]` | VBUSを切って入れ直し、LBA 0が読めるまでの時間を計測（read-only、既定5回、最大20回） |

未対応デバイスが列挙まで成功した場合、UARTには各interfaceの
`number/class/subclass/protocol`（上位byteから順）を16進で出す。これは対応する
クラスドライバまたは転送方式を判断する診断情報であり、`usbrescan`を繰り返さずに
記述子の内容を確認するために使う。同じ未対応デバイスが接続されている間は、
定期再スキャンを続けてもこのログを繰り返さない。物理的に切断された後の次回接続では
再び1回出力する。

起動時のログと再接続時のログは[`DIAGNOSTICS.md`](DIAGNOSTICS.md)を参照して
ください。

## 未実装

- 多段ハブ（ハブ配下のハブ）
- HIDの非Bootレポート解析、複合デバイスの複数interface同時ドライブ
  （1デバイスにつきクラスドライバは1つで、`attach_class_driver`の順に最初に
  一致したものが担当する。`lsusb`は全interfaceを表示するので、駆動されて
  いないinterfaceがあることは一覧から分かる）
- control／bulkの複数channel scheduler（channel 0の固定slotとperiodic HID channel 1〜4は実装済み）
- High-Speedハブ配下のperiodic HIDとSplit transferのDMA mode調停

ハブのstatus-change Interrupt IN endpointは未実装です。High-Speedハブではperiodic descriptor DMAが
Split HIDのbuffer DMAとcontroller-wideに競合するため、空きポート発見は安全な1秒周期の
`scan_empty_hub_ports`を維持します。root-portの挿抜はDWC port IRQで即時検出します。
