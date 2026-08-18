# USB-Aホスト

> 索引: [`../DESIGN.md`](../DESIGN.md) ／ 段階分けと実機で踏んだ罠:
> [`USB_HOST_PLAN.md`](USB_HOST_PLAN.md)、[`USB_REFACTOR_PLAN.md`](USB_REFACTOR_PLAN.md)、
> [`USB_MSC_PLAN.md`](USB_MSC_PLAN.md)

Tab5のUSB-Aコネクタに繋がるHigh-Speed USB-DWCコントローラーをホストとして
使用します。モジュールの層構成（`hcd`／`protocol`／`hid`／`hid_keyboard`／
`hid_mouse`／`hub`／`msc`／`registry`）は[`FILE_LAYOUT.md`](FILE_LAYOUT.md)を
参照してください。この文書は現在どこまで動くかを説明します。

USB-C側のFull-Speed OTGコントローラー（GPIO26/27）と、`uart.rs`が使う
USB Serial/JTAG（GPIO24/25）は対象外です。

## 実機で確認できている範囲

- HID Boot Protocolキーボードからのキー入力（`src/usb/hid_keyboard.rs`）。
  `InputManager`がCardKBと並列にポーリングします（[`INPUT.md`](INPUT.md)）。
- HID Boot Protocolマウスからのポインタ入力（`src/usb/hid_mouse.rs`）。動作確認は
  `win`コマンドの画面で行います（[`APPS.md`](APPS.md)）。
- 1段のUSBハブ配下の複数デバイス列挙と逐次ポーリング（`src/usb/hub.rs`）。
- USB Mass Storageの読み出し（`src/usb/msc.rs`）。詳細は
  [`STORAGE.md`](STORAGE.md)。直結・ハブ経由のどちらでも動作します。
- High-Speedハブ配下にFull/Low-Speedデバイスを繋ぐ構成（Split Transaction）。

## バスの所有とスキャン周期

`UsbHost`（`src/usb/registry.rs`）がUSBバスの単一所有者です。
`hcd::probe_port`と`hub::Hub::open`を呼ぶのはこの型だけで、`usbinfo`／`usbhub`／
`usbmsc`などのシェルコマンドも同じレジストリを引きます。コマンドごとに個別へ
列挙するとバスリセットが走り、フレームループが持っているキーボードセッションを
黙って無効化するためです。

`InputManager::service`がフレーム境界ごとに次を進めます（`src/input.rs`）。

- ルートポートの切断検出: 毎フレーム（レジスタ読み出しだけなので安価）
- セッションが古くなったデバイスの再スキャン: 検出しだい即時
- ハブの空きポートの増分スキャン: 60フレームごと
- ルートポートが空のときの再スキャン: 300フレームごと（ブロッキングの
  リセット・デバウンスを伴うため粗い間隔にしてある）

## 転送方式の現状

- Interrupt INエンドポイントのポーリングは`HCCHAR.eptype`に`INTR`ではなく
  `BULK`を使っています。periodic scheduler／frame list基盤が未実装のため、
  `INTR`のままではポーリングが一切完了しない不具合を実機で確認し、回避策として
  変更しました（[`USB_HOST_PLAN.md`](USB_HOST_PLAN.md) Stage 3）。`HCFG`の
  `PERSCHEDENA`は無効のままです。
- 転送はチャネル0を使った逐次ポーリングです。割り込みルーティングは行わず、
  真の並列転送もしません。
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
既定で`false`です。

## シェルコマンド

| コマンド | 内容 |
| --- | --- |
| `usbinfo` | 現在USB-Aに繋がっている全デバイス（直結・ハブ配下）の一覧 |
| `usbrescan` | ポートをリセットして再列挙 |
| `usbhub` | ハブの記述子と全ポートの状態 |
| `usbhw` | DWCコアの`GHWCFG`ダンプと`HCSPLT`の実在検査 |
| `usbvbus <0-7> on\|off` | PI4IOE2（`0x44`）の出力ビット直接操作（bit 3がVBUS）。診断用 |
| `usbmsc`／`usbread`／`usbmbr` | USB Mass Storage（[`STORAGE.md`](STORAGE.md)） |

起動時のログと再接続時のログは[`DIAGNOSTICS.md`](DIAGNOSTICS.md)を参照して
ください。

## 未実装

- 文字列記述子（製品名）の取得
- periodic scheduler／frame list基盤（上記のとおりBULKで代用中）
- 多段ハブ（ハブ配下のハブ）
- HIDの非Bootレポート解析、複合デバイス
- 割り込み駆動の転送（現状は全てポーリング）
