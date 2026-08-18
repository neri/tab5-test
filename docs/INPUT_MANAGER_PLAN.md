# 入力マネージャ計画: 複数キーボードの統合

> 索引: [`../DESIGN.md`](../DESIGN.md)
> この文書は作業計画と実機での判断記録です。現在の実装仕様は現状文書と
> コードを優先してください。

## 状態: 完了（Stage 1〜5実装・実機確認済み、特殊キー拡張も実機確認済み）

## 背景・目的（着手時）

着手時のコンソールは CardKB と USB HID Boot キーボードの双方から文字を受け取れた。
`app::run` が毎フレーム CardKB を先にポーリングし、キーが無い場合に限って
`UsbHost::poll_keyboards` を呼ぶためである。

しかし、入力源のライフサイクル管理は `app::run` に分散していた。

- CardKB の初期化・約60フレームごとの再接続試行は `app.rs` のローカル状態である
- USB-A の列挙、抜去検出、再列挙、ハブ空きポート走査も同じループのローカル状態である
- `paint` と `touchtest` は `Option<CardKb>` だけを受け取るため、通常のシェルとは
  異なり USB キーボードでは終了できない
- `u8` をそのままコンソールに渡しているため、新しいキーボードが矢印、修飾キー、
  Unicode などを提供する場合の入力契約が定義されていない

この計画の目的は、**複数の物理キーボードを一つの入力経路として扱い、入力が必要な
全画面モードも同じ経路を使えるようにする**ことである。既存の USB バス管理を一般の
「全デバイス管理器」へ吸収することは目的に含めない。

## 現在の前提

- `UsbHost` は USB-A バスの単一所有者である。ハブ、USB HID キーボード、MSC の
  レジストリと、列挙による状態変更を一つに閉じ込めている。
- USB のシェルコマンドは `&UsbHost` / `&mut UsbHost` を受け、必要な場合だけ
  `UsbHost::rescan` を実行する。この所有権を崩してはならない。
- プロジェクトは `no_std` であり、現在の対象台数・デバイス種別はコンパイル時に
  分かっている。今回の範囲ではヒープ確保や `dyn Keyboard` を導入しない。
- 1フレームにコンソールへ渡すキーは最大1件でよい。USB の転送も現在どおり逐次
  ポーリングであり、並列転送は対象外である。

## 設計方針

### 所有関係

`App`（当面は `app::run` のローカル構成）が `InputManager` を一つ所有する。
`InputManager` は入力に関する状態だけを所有し、USB-A の具体的なバス状態は
内部の `UsbHost` に委譲する。

```text
app::run
  ├─ Display / Console
  └─ InputManager
       ├─ CardKbInput
       │    ├─ Option<CardKb>
       │    └─ reconnect_frames
       └─ UsbHost
            ├─ USB バス・ハブ
            ├─ USB キーボード群
            └─ USB Mass Storage 群
```

ここでの `InputManager` は「キーボードを統合するアプリケーション層」であり、
`UsbHost` の代替ではない。USB MSC やハブはキーボードではないため、USB 固有機能の
所有者は引き続き `UsbHost` とする。

### 入力イベント

入力源の公開値を生の `u8` から、固定サイズで `Copy` 可能なイベントへ置き換える。
初期実装は既存の ASCII 動作を完全に保つ最小限の形でよい。

```rust
#[derive(Clone, Copy, Eq, PartialEq)]
pub enum Key {
    Ascii(u8),
    Escape,
    ArrowUp, ArrowDown, ArrowLeft, ArrowRight,
    Home, End, PageUp, PageDown, Insert, Delete, Function(u8),
}

#[derive(Clone, Copy, Eq, PartialEq)]
pub struct KeyEvent {
    pub source: KeySource,
    pub key: Key,
}

#[derive(Clone, Copy, Eq, PartialEq)]
pub enum KeySource {
    CardKb,
    Usb,
}
```

`KeySource` は当面は診断・公平性テスト用であり、コンソールが入力源ごとに分岐する
ためのものではない。コンソールは `Key::Ascii` を従来どおり `char` へ変換する。
CardKB v1.1のEscとカーソル（`0xB5`=↑、`0xB6`=↓、`0xB4`=←、`0xB7`=→）、およびUSB HID Boot
キーボードのEsc、カーソル、Home/End、Page Up/Down、Insert/Delete、F1〜F12を`Key`へ変換する。
コンソールは Esc（行消去）、Left/Right/Home/End（カーソル移動）、Delete
（削除）を現在の行編集として扱う。Up/Down、ページ、Insert、Fキーはイベントとして届くが、
コマンド履歴などの機能が未実装のため現時点では動作を割り当てない。

### 最小 API

API の詳細名は実装時に調整してよいが、責務は次に固定する。

```rust
pub struct InputManager { /* CardKB の再接続状態と UsbHost */ }

impl InputManager {
    pub fn new() -> Self;
    pub fn service(&mut self);              // 抜去検出・再接続・USB の定期走査
    pub fn poll_key(&mut self) -> Option<KeyEvent>;
    pub fn usb_host_mut(&mut self) -> &mut UsbHost;
}
```

`service` はキーの取得をしない。副作用の大きい USB 再列挙と CardKB の再初期化を
明示的に分け、コンソール・全画面モードが同じ順序で `service` と `poll_key` を
使えるようにする。

### ポーリングの公平性

現状の `CardKB.or_else(USB)` は CardKB を固定優先にする。また USB 内の複数
キーボードもスロット順で最初にキーを返す。今回の目的に「同時打鍵の完全な時系列化」は
含めないが、常に入力可能な一台が他方を恒久的に飢餓状態にしないようにする。

- `InputManager` は前回キーを返したトップレベル入力源の次から調べる
  ラウンドロビンを行う
- `UsbHost::poll_keyboards` も、USB キーボードスロットの走査開始位置を回転させる
- 1回の `poll_key` は最初の1イベントだけを返す。残りのイベントをキューへ貯めない
- どちらもイベントが無い時のポーリング順は実装詳細であり、I/O エラーをキーイベント
  として表現しない（既存の再初期化・ログ方針を維持する）

## 「全デバイス管理器」を作らない理由

`UsbHost` はバスのリセット、USB アドレス、ハブ配下の経路、クラスドライバのセッション
を管理する。CardKB は I2C 上の単一機器であり、SD、タッチ、LCD もそれぞれ初期化・
排他・失敗回復の規則が異なる。これらを共通の `DeviceManager` に格納しても、現時点では
共有する操作は少なく、巨大な enum と条件分岐が増えるだけになる。

従って本計画では次の境界を採用する。

| 対象 | 所有者 | 理由 |
|---|---|---|
| CardKB と将来の非USBキーボード | `InputManager` | 統一されたキーイベントを提供するため |
| USB-A バス、USBキーボード、ハブ、MSC | `UsbHost` | バス状態と列挙セッションを一箇所で保つため |
| LCD、タッチ、SD | 各アプリケーション／専用モジュール | 現時点でキーボードと共通のライフサイクルが無いため |

将来、複数デバイスにまたがる起動順、電源管理、状態表示が実際に必要になった場合だけ、
`App` がそれらをフィールドとして所有する `System` / `AppDevices` を導入する。その場合も
個々のバス所有者を一つの汎用レジストリに落とし込まず、集約するだけに留める。

## 実装ステージ

### Stage 0: 現状の振る舞いを固定する

- `app.rs` の CardKB 再接続、USB の再列挙／空きポート走査、キー選択を行番号付きで
  記録する
- 通常コンソールは CardKB と USB の両方でコマンド実行できることを確認する
- `paint` と `touchtest` が CardKB でしか終了できないことを再現して、変更後の比較対象にする

**完了条件:** 変更前の実機確認手順と UART の期待ログをこの節またはテスト記録へ残す。

### Stage 1: `input` モジュールと ASCII 互換イベントを導入する ✅ 完了

- `src/input.rs` を追加し、`Key`、`KeyEvent`、`KeySource` を定義する
- CardKB の `Option<CardKb>` と再接続フレーム数を `InputManager` 内へ移す
- `UsbHost` の生成、起動時 `rescan`、USB の保守タイマーも `InputManager` 内へ移す
- `app::run` は `InputManager::new()`、各フレームの `service()`、`poll_key()` だけを呼ぶ
- キーは当面 `Key::Ascii` だけをコンソールへ渡し、既存の ASCII・Enter・Backspace 動作を
  変えない

**完了条件:** CardKB だけ、USB キーボードだけ、両方接続の各ケースで、現行と同じ
コンソール入力・USB の起動ログ・抜去後の再接続が動く。

### Stage 2: 全画面モードを統合入力へ切り替える ✅ 完了

- `paint::run` と `touch_test::run` の引数を `&mut InputManager` に変更する
- 待機ループでもフレーム境界ごとに `service()` と `poll_key()` を呼ぶ
- 画面文言を「Press any key to exit」に統一し、CardKB 固定の説明と doc comment を更新する
- 全画面モード中の USB 再接続・USB キーボードの抜去後再列挙が、通常コンソールと同じ規則で
  動くことを確認する

**完了条件:** CardKB・USB キーボードのいずれでも `paint` と `touchtest` を終了できる。
キーボード未接続時のタッチテスト待機も、後から接続したどちらのキーボードで終了できる。

### Stage 3: 入力源間と USB 内の公平性を実装する ✅ 完了

- `InputManager` にトップレベル入力源の次回走査位置を保持する
- `UsbHost` に USB キーボードスロットの次回走査位置を保持する。スロット消滅、再列挙、
  ハブ有無の切り替え時に範囲外にならないようリセットする
- 同一フレームに複数キーが届いた場合は、現在の走査開始位置から最初の一つだけを採用する
- ログに各キーを常時出す必要はないが、テストビルドまたは限定ログで入力源と走査順を
  確認できるようにする

**完了条件:** CardKB と USB キーボードを連続入力しても一方が恒久的に取りこぼされない。
USB キーボードを2台同時に扱える環境では、両方からの入力が交互に採用されることを確認する。

### Stage 4: USB コマンドへの参照を整理する ✅ 完了

- `shell::execute` が必要とする USB 参照を `input_manager.usb_host_mut()` 経由で渡す
- `shell` に入力源統合の判断や CardKB 型を露出させない
- 既存の `usbrescan`、MSC 操作、ハブ状態取得が、引き続き一つの `UsbHost` インスタンスを
  操作することを確認する

**完了条件:** USB コマンド実行後も USB キーボードのセッションが壊れず、InputManager が
USB ホストを複製・再生成していないことがコードレビューで確認できる。

### Stage 5: ドキュメント・回帰確認 ✅ 完了

- [`CONSOLE_SHELL.md`](CONSOLE_SHELL.md)、[`FILE_LAYOUT.md`](FILE_LAYOUT.md)、各モジュール
  doc commentの`CardKB`固定表現と、古い`lcd.rs`所有の説明を`InputManager`／`app.rs`の
  実態に更新する。`README.md`との不一致は人間管理ルールに従って報告だけにとどめる
- 少なくとも次の実機マトリクスを実行する

| ケース | 期待結果 |
|---|---|
| CardKB のみ | 起動、入力、抜去・再接続、全画面モード終了が動く |
| USBキーボードのみ | 起動、入力、抜去・再列挙、全画面モード終了が動く |
| CardKB + USBキーボード | 両方で入力・全画面モード終了でき、片方が飢餓化しない |
| USBハブ + USBキーボード + MSC | キー入力を維持したまま `usbinfo` / `usbmsc` 系を実行できる |
| キーボード未接続で全画面モードへ入る | 後から接続した CardKB または USB キーボードで終了できる |

**完了条件:** 上表の利用可能な実機ケースを確認し、未保有機材（例: USBキーボード2台）の
項目は未確認理由を明記する。`cargo check` とリリースビルドを通す。

## 互換性と移行上の注意

- 最初の変更では ASCII 以外のキーを勝手にコンソール動作へ割り当てず、USB HID の
  既存変換結果を `Key::Ascii` に包んだ。その後の特殊キー拡張で Esc、Left/Right、
  Home/End、Delete にだけ行編集の動作を割り当てた。
- CardKB の `poll()` は現状どおり `Option<u8>` のままでもよい。入力源共通の型へ変換する
  境界は `InputManager` とする。これにより既存ドライバを無関係に複雑化しない。
- USB の再列挙はブロッキングであり、全画面モードでも既存と同程度の一時停止が起き得る。
  本計画はそれを非同期化するものではない。
- 将来の BLE、UART、GPIO ボタンなどの入力源は `InputManager` に追加できる。ただし
  初期化・再接続・ポーリングを同じ `KeyEvent` 契約へ変換できる場合に限る。

## 対象外

- USB 転送の並列化、割り込み駆動化、複数 USB ホストコントローラへの対応
- HID Boot Keyboard 以外の USB HID レポートパーサ実装
- Unicode、IME、キーリピート、キーバッファリング、複数キー同時押しの意味付け
- SD、タッチ、LCD、I2C 全体を登録する汎用 `DeviceManager` の実装
