# GUIの基本色

> 索引: [`../DESIGN.md`](../DESIGN.md)

通常GUIの基本色は`src/app/theme.rs`にRGB565定数として定義する。
デスクトップ、Browser、システムバー、Launcher、Network settings、Battery detailsが参照し、
基本色を変えるときは画面ごとの色値ではなく、このモジュールを変更する。
Console、診断画面の独自配色は対象外。実行時のテーマ切替は設けていない。

| 用途 | 定数 | RGB565 |
| --- | --- | --- |
| デスクトップ背景（ティール） | `DESKTOP_BACKGROUND` | `0x0410` |
| 白背景／通常文字 | `BACKGROUND`／`TEXT` | `0xFFFF`／`0x0000` |
| バー・ボタン地 | `BUTTON_FACE` | `0xC618` |
| 青アクセント／選択上の文字 | `ACCENT`／`ON_ACCENT` | `0x001F`／`0xFFFF` |
| パネル／見出し／淡い面 | `PANEL`／`HEADER`／`SUBTLE` | `0xDEFB`／`0xE71C`／`0xEF7D` |
| 補助文字／境界／非点灯 | `MUTED`／`BORDER`／`INACTIVE` | `0x632C`／`0x8410`／`0x8C51` |
| 正常／警告／エラー | `SUCCESS`／`WARNING`／`ERROR` | `0x0400`／`0xA500`／`0xF800` |
| Browser強調／コード文字 | `EMPHASIS`／`CODE` | `0x9000`／`0x0320` |

LauncherとWi-Fi一覧の選択行は青地に白文字とする。Wi-Fi一覧は選択中の電波の点灯部と
接続済みマークも白にし、非点灯部はグレーで区別する。接続済みSSIDの太字は維持する。
Battery detailsは白地、黒い文字と輪郭、青い電圧表示、暗い緑／黄土色／赤の残量表示とする。
待機・エラー時も白背景を維持する。

Network settingsとBattery detailsのバー内`< Back`は、既存のBrowserと同じ
1 pixel右への重ね描きで太字にする。タイトルは通常の太さのまま。
Network settings下部の`Esc Back`も太字にする。
これらの配色・文字の実機での視認性は未確認。

GUIの英語文言は文頭と固有名詞・略語だけを大文字にする。初期フォントの制約に由来する
全大文字表記は使わず、`Wi-Fi`、`USB`、`DHCP`、`UART`、IC名などは通常の表記を保つ。

通常GUIのEnglish LatinはA4 coverageを各theme色と描画先背景のRGB565 channelでblendする。
選択行の青やDesktop barのtealでも二値化せず、太字の二度打ちも各passでblendする。
