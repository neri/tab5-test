# USB-Aホスト

> 索引: [`../DESIGN.md`](../DESIGN.md) ／ 段階分けと実機で踏んだ罠:
> [`USB_HOST_PLAN.md`](USB_HOST_PLAN.md)、[`USB_REFACTOR_PLAN.md`](USB_REFACTOR_PLAN.md)、
> [`USB_INTERRUPT_REFACTOR_PLAN.md`](USB_INTERRUPT_REFACTOR_PLAN.md)、
> [`USB_MSC_PLAN.md`](USB_MSC_PLAN.md)、[`USB_FLOPPY_PLAN.md`](USB_FLOPPY_PLAN.md)

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
- High-Speedハブ配下にFull/Low-Speedデバイスを繋ぐ構成（Split Transaction）。

## 中断したFloppy実装

UFI/CBI USB Floppy用の試作クラスドライバは`src/usb/floppy.rs`に保持するが、現在の
ビルドには含めず、レジストリも選択しない。したがってUFI/CBIデバイスは未対応として
一度だけログに出力され、`usbfloppy`と`usbfloppyprobe`コマンドは存在しない。

直結実機（VID:PID `054C:002C`、interface `08/04/00`、Bulk IN `0x81`、Bulk OUT
`0x02`、status Interrupt IN `0x83`）でのCBI ADSC制御要求は、descriptor-DMAの
SETUP PID修正後もSETUP段階の`XCS_XACT_ERR`で失敗した。詳細と再開条件は
[`USB_FLOPPY_PLAN.md`](USB_FLOPPY_PLAN.md)を参照する。

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
- ハブの空きポートの増分スキャン: 60フレームごと
- ルートポートが空のときの再スキャン: 300フレームごと（ブロッキングの
  リセット・デバウンスを伴うため粗い間隔にしてある）

## 転送方式の現状

- HID BootのInterrupt INは、対象routeで空きがあればchannel 1〜4のいずれかを確保し、
  `HCCHAR.eptype=INTR`と32-entry periodic frame listで常時待機します。対象はrootへFS/LSで直結した
  keyboard／mouseと、Splitが生じないFull-Speedハブ配下のkeyboard／mouseです。最大4 endpointの
  channel bitを共有frame listへ`bInterval`ごとに合成し、QTD、64-byte report buffer、data toggle、
  世代token、IRQ pendingはchannelごとに独立しています。割当て不能時とHigh-Speedハブ配下は、
  controller-wide DMA modeとSplitの調停が未実装なため、従来の`BULK`分類＋frame pollへfallback
  します。root直結High-Speed HIDもinterval解釈の実機確認前なのでfallbackです。
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
- rootへ直接接続したHID Boot keyboardは、attach時にstaticな512-byte aligned
  32-entry frame list／QTD bankを割り当て、`HCCHAR.eptype=INTR`で常時待機します。report完了IRQを
  前景がtakeして次QTDをrearmするため、idle中にchannel 0をpollしません。この常設経路は
  root直結LS keyboardとFull-Speed mouseで実機確認済みです。keyboardの10秒idle比較で
  poll／submit／cancelは増えず、
  key reportだけchannel 1で完了・rearmしました。channel 1〜4 allocator、root直結mouse、
  Full-Speedハブ配下の複数HIDも実装済みです。後者はHigh-Speedハブを`usbfs on`でFull-Speed
  列挙する代替試験により、keyboard=channel 1、mouse=channel 2の同時動作を確認済みです。
- High-Speedハブ配下のFS/LS HIDは、Splitがbuffer DMA、periodicがdescriptor DMAという
  controller-wide制約のため、channel 0のserialized Split fallbackを使います。各SSPLIT／CSPLIT
  phaseはIRQ＋WFIで待ち、レジスタをspin pollしません。HCDはSplit modeを排他状態として管理し、
  periodic channelが残っていればDMA modeを切り替えずエラーにします。`usbhw`の`IRQ split`で
  packet／round／conflictとmode activeを確認できます。keyboard＋mouseのHigh-Speedハブ実機回帰では
  4745 packets／58727 roundsをIRQ＋WFIで処理し、poll／conflictはいずれも0でした。
  同じハブへHigh-Speed USBメモリを追加し、Split HIDを維持したままMSCの`INQUIRY`、
  `TEST UNIT READY`、`READ CAPACITY(10)`、`READ(10)`も実機成功しています。
- USB-Aの5V（VBUS）は2個目のPI4IOE5V6408（E2、I2Cアドレス`0x44`）のbit 3です。
  同じexpanderは電源断や充電制御とも共用するため、書き換えはビット単位の
  read-modify-write（`hcd::set_pi4ioe2_output_bit`）で行います
  （[`CONSOLE_SHELL.md`](CONSOLE_SHELL.md)の「全体電源断」も参照）。

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

## シェルコマンド

| コマンド | 内容 |
| --- | --- |
| `usbinfo` | 現在USB-Aに繋がっている全デバイス（直結・ハブ配下）の一覧 |
| `usbrescan` | ポートをリセットして再列挙 |
| `usbfs on\|off` | FS/LS-only host modeを切替えて即時再列挙（診断用、既定off） |
| `usbhub` | ハブの記述子と全ポートの状態 |
| `usbhw` | DWCコアの`GHWCFG`ダンプと`HCSPLT`の実在検査 |
| `usbperiodic` | 常設periodicが無効な最初のHIDでchannel 1＋frame listを1転送だけ試験（旧Go/No-Go診断） |
| `usbvbus <0-7> on\|off` | PI4IOE2（`0x44`）の出力ビット直接操作（bit 3がVBUS）。診断用 |
| `usbmsc`／`usbread`／`usbmbr` | USB Mass Storage（[`STORAGE.md`](STORAGE.md)） |

未対応デバイスが列挙まで成功した場合、UARTには各interfaceの
`number/class/subclass/protocol`（上位byteから順）を16進で出す。これは対応する
クラスドライバまたは転送方式を判断する診断情報であり、`usbrescan`を繰り返さずに
記述子の内容を確認するために使う。同じ未対応デバイスが接続されている間は、
定期再スキャンを続けてもこのログを繰り返さない。物理的に切断された後の次回接続では
再び1回出力する。

起動時のログと再接続時のログは[`DIAGNOSTICS.md`](DIAGNOSTICS.md)を参照して
ください。

## 未実装

- 文字列記述子（製品名）の取得
- 多段ハブ（ハブ配下のハブ）
- HIDの非Bootレポート解析、複合デバイス
- control／bulkの複数channel scheduler（channel 0の固定slotとperiodic HID channel 1〜4は実装済み）
- High-Speedハブ配下のperiodic HIDとSplit transferのDMA mode調停

ハブのstatus-change Interrupt IN endpointは未実装です。High-Speedハブではperiodic descriptor DMAが
Split HIDのbuffer DMAとcontroller-wideに競合するため、空きポート発見は安全な1秒周期の
`scan_empty_hub_ports`を維持します。root-portの挿抜はDWC port IRQで即時検出します。
