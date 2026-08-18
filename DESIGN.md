# 設計資料

## 対象と方針

このプロジェクトはM5Stack Tab5のESP32-P4 ECO2（chip revision v1.3）を対象に
しています。ESP-IDFやRTOSをリンクせず、`riscv-rt`とレジスタ操作だけで起動、
PSRAM、MIPI-DSI、GDMAを初期化します。

実機で確認した構成は次のとおりです。

- ESP32-P4 ECO2、eFuse block revision v0.3
- 16 MiB SPI Flash
- Hex-DDR PSRAM（32 MiB）
- ネイティブ走査720×1280のMIPI-DSI LCD
- USB Serial/JTAG

## ドキュメント構成

本文は`docs/`以下に分割してあります。各文書は実装の現状を説明するもので、
`*_PLAN.md`は機能追加時の作業計画と実機での判断記録です。

| 文書 | 内容 |
| --- | --- |
| [BOOT.md](docs/BOOT.md) | イメージ配置（XIP／IRAM／DRAM）、RAMの範囲、起動シーケンス |
| [PSRAM.md](docs/PSRAM.md) | PSRAM初期化、DQS調整、MMU割り当て、キャッシュ同期、グローバルアロケータ |
| [DISPLAY.md](docs/DISPLAY.md) | LCDとパネル初期化、映像データ経路、フレーム割り込み |
| [DISPLAY_BANDWIDTH.md](docs/DISPLAY_BANDWIDTH.md) | 表示帯域とFIFOアンダーラン、PSRAMの実測値、PPA／2D-DMAへの移行、試して駄目だった方法 |
| [GRAPHICS.md](docs/GRAPHICS.md) | `Framebuffer`の描画API、CW回転による論理↔ネイティブ座標変換 |
| [CONSOLE_SHELL.md](docs/CONSOLE_SHELL.md) | コンソールのセル管理と部分書き戻し、シェル、再起動、全体電源断 |
| [INPUT.md](docs/INPUT.md) | ソフトI2C、CardKB／USBキーボード、`Key`正規化、`InputManager`、ポインタ、タッチコントローラー |
| [APPS.md](docs/APPS.md) | ペイント／タッチ診断、座標チャート、BMI270軸テスト、バッテリー、`win`デスクトップ |
| [USB.md](docs/USB.md) | USB-Aホストの対応範囲、バス所有とスキャン、転送方式、Split Transaction |
| [STORAGE.md](docs/STORAGE.md) | SDカードとUSBマスストレージのブロックI/O、MBR、シェルコマンド |
| [RTC.md](docs/RTC.md) | RX8130CEのカレンダー読み書きと`rtc test`の検査内容 |
| [FILE_LAYOUT.md](docs/FILE_LAYOUT.md) | モジュールごとの責務一覧、コーディング方針（コメントの言語、`unsafe`の粒度） |
| [DIAGNOSTICS.md](docs/DIAGNOSTICS.md) | 正常時のUARTログ通過点と主な失敗ログ |
| [KNOWN_ISSUES.md](docs/KNOWN_ISSUES.md) | 実機で見つかったDW-GDMA／SDHOSTの制約 |

作業計画（実装済みの機能について、段階分けと実機で踏んだ罠を残したもの）:
[FLASH_XIP_MIGRATION_PLAN.md](docs/FLASH_XIP_MIGRATION_PLAN.md)、
[INPUT_MANAGER_PLAN.md](docs/INPUT_MANAGER_PLAN.md)、
[PPA_FILL_PLAN.md](docs/PPA_FILL_PLAN.md)、
[SD_CARD_PLAN.md](docs/SD_CARD_PLAN.md)、
[SOFT_I2C_REFACTOR_PLAN.md](docs/SOFT_I2C_REFACTOR_PLAN.md)、
[USB_FLOPPY_PLAN.md](docs/USB_FLOPPY_PLAN.md)、
[USB_HOST_PLAN.md](docs/USB_HOST_PLAN.md)、
[USB_MSC_PLAN.md](docs/USB_MSC_PLAN.md)、
[USB_REFACTOR_PLAN.md](docs/USB_REFACTOR_PLAN.md)。

## 制約

- ECO2で確認したレジスタ値とROM APIアドレスを使用しています。
- PSRAMは32 MiB全体を固定アドレスへMMU割り当てします。フレームバッファ以外
  （約30.24 MiB）は`linked_list_allocator`によるグローバルアロケータのヒープです。
- DSIタイミングとパネルシーケンスは確認したTab5個体向けです。
- 日本語フォント、省電力制御は未実装です。
- バッテリー表示はINA226による瞬時測定と電圧ベースの目安だけです。充電状態、USB-Cの
  接続状態、正確なSoC／残り時間、電池の健全性は取得しません。
- ストレージはブロック単位の読み書きとMBR表示までです。FAT/exFATの解析、
  USB MSCの書き込み、SDのUHS-Iモードは未実装です（[STORAGE.md](docs/STORAGE.md)）。
- USB-AホストはHID Bootキーボード、HID Bootマウス、1段のハブ、Mass Storageの
  読み出しまで実機確認済みです。文字列記述子の取得、periodic scheduler基盤、
  多段ハブは未実装です（[USB.md](docs/USB.md)）。
