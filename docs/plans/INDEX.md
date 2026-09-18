# 作業計画の一覧

> 索引: [`../../DESIGN.md`](../../DESIGN.md)

機能追加やリファクタリングの段階分けと、実機での判断記録です。現在の実装仕様は
`docs/`直下の現状文書とコードを優先してください。計画は状態ごとにディレクトリを分けます。

| ディレクトリ | 置くもの |
| --- | --- |
| [`active/`](active/) | 着手済みで未完了、または実装済みで実機受入待ち |
| [`proposed/`](proposed/) | 提案だけで未着手 |
| [`archive/`](archive/) | 完了、凍結、打ち切り。当時の記録で、現状と食い違う記述を含む |

状態が変わったら`git mv`で移し、この表と、移したファイルを指すリンク・パス
（`docs/`、`src/`、`Cargo.toml`、`tools/`など）を同じ作業で直してください。
状態欄は各計画の`## 状態`行の要約です。食い違ったら計画本文とコードを優先します。

## active

| 計画 | 内容 | 状態 |
| --- | --- | --- |
| [SCALABLE_PROPORTIONAL_FONT_PLAN.md](active/SCALABLE_PROPORTIONAL_FONT_PLAN.md) | 英字プロポーショナル／スケーラブルフォント表示 | 実装済み、実機受入待ち |
| [SYSTEM_BAR_PLAN.md](active/SYSTEM_BAR_PLAN.md) | 上部統合システムバー | 統合実装済み・実機未確認（Stage 8の受入待ち） |

## proposed

| 計画 | 内容 | 状態 |
| --- | --- | --- |
| [COMMAND_RETIREMENT_PLAN.md](proposed/COMMAND_RETIREMENT_PLAN.md) | コンソールコマンドの退役 | 未着手（案のみ） |
| [DEVICE_TREE_PLAN.md](proposed/DEVICE_TREE_PLAN.md) | DeviceTree導入 | 未着手 |
| [TLSF_ALLOCATOR_PLAN.md](proposed/TLSF_ALLOCATOR_PLAN.md) | TLSFグローバルアロケータ移行 | 未着手 |
| [USB_HID_REPORT_PLAN.md](proposed/USB_HID_REPORT_PLAN.md) | USB HID Report protocolと複数入力の調停 | 提案（全Stage未着手） |

## archive

| 計画 | 内容 | 状態 |
| --- | --- | --- |
| [BROWSER_IMAGE_CACHE_WEBP_PLAN.md](archive/BROWSER_IMAGE_CACHE_WEBP_PLAN.md) | Browser画像LRU・上限緩和・静止WebP | 完了（8 MiB候補棄却、実Wi-Fi切断回帰は見送り） |
| [BROWSER_EXTENSION_PLAN.md](archive/BROWSER_EXTENSION_PLAN.md) | Browser画像・フォーム・キャッシュ | 完了（Stage 0〜8） |
| [BROWSER_FRAGMENT_NAVIGATION_PLAN.md](archive/BROWSER_FRAGMENT_NAVIGATION_PLAN.md) | ブラウザfragment navigation | 完了 |
| [BROWSER_INLINE_CONTROL_PLAN.md](archive/BROWSER_INLINE_CONTROL_PLAN.md) | Browser inline control・装飾button | 完了（Stage 0〜6） |
| [BROWSER_TABLE_PLAN.md](archive/BROWSER_TABLE_PLAN.md) | Browser table | 完了（Stage 0〜7） |
| [BROWSER_UI_PLAN.md](archive/BROWSER_UI_PLAN.md) | ブラウザUI改修 | 完了（Stage 0〜10） |
| [DISPLAY_UNDERRUN_REFACTOR_PLAN.md](archive/DISPLAY_UNDERRUN_REFACTOR_PLAN.md) | 表示アンダーラン対策 | 完了（Stage 0〜4。Stage 5〜8は実施しない） |
| [DNS_PLAN.md](archive/DNS_PLAN.md) | DNSクライアント | 完了（Stage 0〜5。任意のStage 6 mDNSは未着手） |
| [FILESYSTEM_PLAN.md](archive/FILESYSTEM_PLAN.md) | ファイルシステム | 完了（Stage 1〜5） |
| [FILESYSTEM_WORKFLOW_PLAN.md](archive/FILESYSTEM_WORKFLOW_PLAN.md) | ファイルシステム運用機能 | 完了（機能1〜3） |
| [FILESYSTEM_WRITE_REFACTOR_PLAN.md](archive/FILESYSTEM_WRITE_REFACTOR_PLAN.md) | 外部FAT書き込みリファクタリング | 完了（Stage 0〜5） |
| [FLASH_XIP_MIGRATION_PLAN.md](archive/FLASH_XIP_MIGRATION_PLAN.md) | FLASH XIP移行 | 完了（全Stage） |
| [FONT_MIGRATION_PLAN.md](archive/FONT_MIGRATION_PLAN.md) | 16 pixel Unicodeビットマップフォント移行 | 完了 |
| [INPUT_MANAGER_PLAN.md](archive/INPUT_MANAGER_PLAN.md) | 入力マネージャ（複数キーボードの統合） | 完了（Stage 1〜5） |
| [PPA_FILL_PLAN.md](archive/PPA_FILL_PLAN.md) | PPA/2D-DMAによる塗りつぶし | 完了（Stage 1〜6） |
| [ROOT_FILESYSTEM_PLAN.md](archive/ROOT_FILESYSTEM_PLAN.md) | RAMディスクのルートファイルシステム化 | 完了（Stage 0〜4） |
| [SD_CARD_PLAN.md](archive/SD_CARD_PLAN.md) | SDカードアクセス | 完了（Stage 0〜3・4a。以降は別計画へ移管） |
| [SOFT_I2C_REFACTOR_PLAN.md](archive/SOFT_I2C_REFACTOR_PLAN.md) | SoftI2CトランザクションAPI化 | 完了 |
| [STARTUP_SCREEN_REFACTOR_PLAN.md](archive/STARTUP_SCREEN_REFACTOR_PLAN.md) | 起動画面リファクタリング | 完了（Stage 0〜8） |
| [TCPIP_PLAN.md](archive/TCPIP_PLAN.md) | TCP/IPスタック（smoltcp） | 完了（全Stage） |
| [TLS_PLAN.md](archive/TLS_PLAN.md) | TLS／HTTPS | 初期範囲完了（Stage 0〜8。Stage 9 Web PKI検証は条件成立時に別途判断） |
| [USB_BOT_HCD_REFACTOR_PLAN.md](archive/USB_BOT_HCD_REFACTOR_PLAN.md) | USB BOT／HCD正常境界リファクタリング | 完了（Stage 0〜8） |
| [USB_FLOPPY_PLAN.md](archive/USB_FLOPPY_PLAN.md) | USB Floppy | 凍結（Stage 2で未解決、再開予定なし） |
| [USB_HOST_PLAN.md](archive/USB_HOST_PLAN.md) | USB-Aホスト機能 | 完了（Stage 5は別計画で実施） |
| [USB_INTERRUPT_REFACTOR_PLAN.md](archive/USB_INTERRUPT_REFACTOR_PLAN.md) | USB割り込み駆動リファクタリング | 完了（hub statusの割り込み化は実施しない） |
| [USB_MSC_BOOT_MARGIN_PLAN.md](archive/USB_MSC_BOOT_MARGIN_PLAN.md) | 起動時USB MSC認識マージンの計測 | 完了 |
| [USB_MSC_PLAN.md](archive/USB_MSC_PLAN.md) | USB Mass Storage | 完了（読み出し。WRITE(10)は別計画で受入） |
| [USB_REFACTOR_PLAN.md](archive/USB_REFACTOR_PLAN.md) | USBマルチデバイス管理とMSCのハブ対応 | 完了（Stage E不要、多段ハブのStage Gはスコープ外） |
| [USB_WRITE_STABILITY_PLAN.md](archive/USB_WRITE_STABILITY_PLAN.md) | USB MSC書き込みの不安定さ（調査記録） | 完了（根本原因は未特定、緩和策で封じ込め） |
| [WEB_BROWSER_PLAN.md](archive/WEB_BROWSER_PLAN.md) | 簡易Webブラウザ | 完了 |
| [WIFI_C6_PLAN.md](archive/WIFI_C6_PLAN.md) | ESP32-C6経由Wi-Fi | 完了（全Stage） |
| [WIFI_REFACTOR_PLAN.md](archive/WIFI_REFACTOR_PLAN.md) | Wi-Fi接続管理リファクタリング | 完了（Stage 0〜7。Stage 8は実施しない） |
