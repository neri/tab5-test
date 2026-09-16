# DeviceTree 導入計画

> 索引: [`../../../DESIGN.md`](../../../DESIGN.md)
>
> この文書は作業計画です。現在の実装仕様は、実装後に追加する現状文書とコードを
> 優先してください。

## 状態: 未着手

## 目的

現在は Tab5 の配線、I2C アドレス、パネル寸法、I/O エキスパンダのビット番号、
接続される周辺チップが各ドライバーに直接書かれている。例えば `i2c.rs` が三つの
Tab5 用バスを static に持ち、`lcd.rs`、`power.rs`、`sdio.rs`、各センサードライバーが
それぞれ Tab5 固有のピンまたはアドレスを知っている。このままでは別の ESP32-P4
ボードを追加するたびに、共通ドライバーまで条件分岐または複製が必要になる。

本計画の到達点は次の三段階である。

1. Tab5 のボード記述をコードから分離し、ビルド時に一つの DeviceTree から生成する。
   動作・起動順・出力イメージは移行前と同一にする。
2. DeviceTree の `compatible` とノード関係から、有効なデバイスドライバーだけを
   構成し、各ドライバーへ配線・アドレス・設定を渡す。
3. ESP32-P4 共通の SoC 記述とボード記述を分け、別ボードは DeviceTree の選択だけで
   共通ドライバー群を再利用できるようにする。

ここでいう「切り替え」は**ファームウェアをビルドするときのボード選択**である。
この `no_std` ファームウェアに起動時 FDT パーサーや動的ドライバーローダーを載せる
意味ではない。生成物は静的な Rust 定数と型検査済みの構成コードだけにし、起動時の
ヒープ確保、文字列パース、デバイス探索を増やさない。

## 採用する仕組み

**定義ファイルは Zephyr DeviceTree と互換にする。** すなわち、Zephyr の
前処理済み DeviceTree 入力として受理できる DTS v1、C preprocessor の `#include`、
`.dtsi`、`.overlay`、ラベル/phandle、`/aliases`、`/chosen`、および Zephyr 形式の
YAML binding を正とする。DTS の独自サブセットを発明しない。Zephyr は DTS/DTSI/overlay
を C preprocessor で展開して binding と照合し、最終 DTS を生成するため、本計画も
同じ入力モデル・優先順を採る。[Zephyr の入力/出力仕様](https://docs.zephyrproject.org/latest/build/dts/intro-input-output.html)
と [binding 構文](https://docs.zephyrproject.org/latest/build/dts/bindings-syntax.html) を
基準にする。

DTS は `/dts-v1/;` を先頭に置き、SoC 記述を `#include` する。binding は
`compatible`、`properties`、`required`、`type`、`enum`、`default`、`bus`、`on-bus`、
`*-cells`、`child-binding` を Zephyr の YAML 構文どおりに用いる。既存の Zephyr
upstream binding と `compatible` が使える部品はそれを再利用し、Tab5/本プロジェクト
固有のものだけをローカル binding として追加する。独自 `compatible` の vendor prefix は
Zephyr の vendor-prefix 規則に従う。採用前に既存 prefix を確認し、未登録ならプロジェクト
側の `vendor-prefixes.txt` に名称と prefix を登録して、曖昧な `tab5,...` のような名前を
作らない。

Zephyr の DeviceTree と同じ考え方（SoC とボードの分離、階層、`compatible`、
`status`、`reg`、`gpios`、phandle）を使う。対象となる標準要素は次のとおりである。

| 要素 | 用途 |
| --- | --- |
| ノード階層と `/ {}` | SoC、バス、周辺、選択済み構成を表す |
| `compatible` | 対応する Rust ドライバーと必須プロパティを決める |
| `status = "okay"` / `"disabled"` | ビルド対象かどうかを決める |
| `reg` | I2C アドレス、MMIO 範囲、CS 番号などのバス上の番地を表す |
| `gpios`、`pinctrl-*` | GPIO 番号、方向、active-low、GPIO Matrix/IOMUX の接続を表す |
| phandle と `&label` | デバイスが使うバス、電源、リセット、パネルを参照する |
| `/aliases` と `/chosen` | 安定した別名、起動コンソール、表示、既定入力、ブート時に必須な電源制御を選ぶ |
| vendor prefix の独自プロパティ | ESP32-P4 固有のクロック、DMA、パネル初期化、既知の基板差を明示する |

`build.rs` は DTS を自前で解釈しない。固定した Zephyr toolchain revision の
`dtc`、前処理、`edtlib`/binding 検証を呼ぶ `tools/gen_dt_rust.py` を起動し、Zephyr が
解釈した EDT (Enhanced DeviceTree) から `OUT_DIR/devicetree_generated.rs` を出力する。
Rust への変換バックエンドだけがプロジェクト固有であり、DTS の構文・binding 解決・
overlay マージの意味を再実装しない。Zephyr 本体や Python はホスト側のビルド依存だけで、
生成済みのファームウェアには含めない。

Zephyr toolchain revision は `tools/zephyr-toolchain.lock` などで固定し、CI はその revision
で `dtc` と binding validator を実行する。ローカルの Cargo build も同じ toolchain を
見つけられない場合は、互換性の保証を装った代替パーサーへ黙ってフォールバックせず、
導入手順を案内して失敗させる。`dtc` が最終 DTS に対して出す警告もエラーとして扱う。

生成コードを使うインターフェースは `src/devicetree.rs` とする。そこでは生の文字列や
ノード検索 API を公開せず、例えば `BoardConfig`、`I2cBusConfig`、`GpioConfig`、
`DisplayConfig`、`DeviceConfig` のような型付き定数だけを再公開する。ドライバーが
`compatible` 文字列を比較したり GPIO 番号を再定義したりしないことを境界にする。

## ディレクトリとボード選択

導入時に想定する配置は次のとおりである。Zephyr の out-of-tree board/SoC/binding の
慣例に合わせ、他の Zephyr プロジェクトへ持ち込んでも構造を読み替えなくてよい形にする。

```text
dts/
  bindings/                 # Zephyr YAML binding と vendor-prefixes.txt
  riscv/espressif/esp32p4.dtsi
                             # P4 の MMIO、GPIO、SDMMC、USB、MSPI 等の共通記述
boards/
  m5stack/tab5/tab5.dts      # Tab5 の配線、搭載デバイス、chosen
  <vendor>/<new-p4-board>/<new-p4-board>.dts
build.rs                     # 固定 Zephyr toolchain を通して Rust 定数を生成
tools/gen_dt_rust.py         # EDT から Rust config へ変換する唯一の独自部分
src/devicetree.rs           # include!(OUT_DIR/...) の唯一の入口
src/board.rs                # 生成構成から起動段階・ドライバー集合を組み立てる
```

Cargo feature を選択入口にする。最初は `board-tab5` を default feature とし、別ボード
追加時は `board-<name>` を一つだけ有効にする。`build.rs` は「有効な board feature が
ちょうど一つであること」と、選択した DTS と全ての DTSI の変更を検査する。例:

```text
cargo build --release                         # board-tab5
cargo build --release --no-default-features --features board-example-p4
```

base board は Cargo feature だけで一意に選ぶ。一方、Zephyr と同じ `.overlay` は、明示した
ローカル配線差や shield 用に使えるようにする。`TAB5_DT_OVERLAY` は空白区切りの overlay
一覧として受け、base DTS の後に指定順でマージする（Zephyr の `DTC_OVERLAY_FILE` と同じ
優先順）。`build.rs` は各ファイルを `rerun-if-changed` 対象にし、最終 DTS、board feature、
overlay の絶対パスと内容ハッシュをビルドログ/成果物メタデータに残す。公式 release と CI
では overlay を使わない、または repository 内の固定 overlay だけを feature と対にする。

## Tab5 の記述範囲

`boards/m5stack/tab5/tab5.dts` は少なくとも下記を唯一のボード固有値の置き場にする。SoC のレジスタ
アドレス、ECO2 向け ROM API、CPU 起動・PSRAM 初期化のシリコン依存手順は
`esp32p4.dtsi` または SoC ドライバーに残し、ボード固有の配線と搭載品から分離する。

| 分類 | Tab5 から移す値・関係 |
| --- | --- |
| メモリ/起動 | 16 MiB flash、32 MiB Hex-DDR PSRAM、ECO2 ボードプロファイル、パーティションと chosen の対応 |
| GPIO/pinctrl | board I2C (31/32)、Ext.Port1 (0/1)、PORT.A (53/54)、バックライト、C6 reset、SDIO1 GPIO Matrix、SDMMC0 IOMUX |
| I2C デバイス | PI4IOE1/2、GT911 の候補アドレス、ST7121/ST7123、BMI270、INA226、RX8130CE、CardKB、Tab5 Keyboard |
| 表示 | DSI host、D-PHY、720×1280 native panel、回転、タイミング、バックライト、パネル reset/enable を行う expander 出力 |
| 電源 | USB-A VBUS (E2.P3)、C6 電源 (E2.P0)、全体電源断 pulse (E2.P4) と active level |
| 接続機器 | microSD、ESP32-C6 SDIO Wi-Fi、USB-A High-Speed host と利用する電源ノード |
| `chosen` | console/display/primary-input、早期に停止または電源断するデバイス |

Tab5 のタッチには GT911 搭載ロットと ST7121/ST7123 搭載ロットがある。これは
一つの実装固定値ではなく、実在する同一ボード名の組立差である。両方のノードを
記述し、登録済み vendor prefix の `exclusive-probe-group = "touch"` と優先順位を持たせる。生成した
構成はこのグループを「最初に識別できた一台だけを有効化する optional driver」として
扱う。通常の必須デバイスにこの probe-fallback を広げず、将来ロットを完全に分けられる
時点で DTS variant (`m5stack-tab5-gt911.dts` 等) に分離する。

概念例（`acme` は実装時に vendor-prefixes.txt で確定する prefix、プロパティ名は binding
で固定する）:

```dts
/dts-v1/;
#include <riscv/espressif/esp32p4.dtsi>
#include <zephyr/dt-bindings/gpio/gpio.h>

/ {
    model = "M5Stack Tab5";
    compatible = "acme,tab5", "espressif,esp32-p4-eco2";

    chosen {
        zephyr,display = &lcd_panel;
        acme,board-i2c = &i2c_board;
    };

    aliases {
        rtc0 = &rtc;
        imu0 = &imu;
    };

    i2c_board: i2c-board {
        compatible = "acme,soft-i2c";
        sda-gpios = <&gpio0 31 GPIO_OPEN_DRAIN>;
        scl-gpios = <&gpio0 32 GPIO_OPEN_DRAIN>;
        clock-frequency = <10000>;

        rtc: rtc@32 { compatible = "epson,rx8130ce"; reg = <0x32>; };
        imu: imu@68 { compatible = "bosch,bmi270"; reg = <0x68>; };
    };
};
```

`zephyr,display` は Zephyr で意味を持つ標準の chosen property なので、Zephyr にも渡す
board DTS ではそのまま使う。プロジェクト独自の chosen property は vendor prefix を
付ける。上の例は実際に Zephyr の前処理/validator を通す構文であり、Rust 側の API だけが
Zephyr API と異なる。

## ドライバー組み込みの設計

### 構成と所有権

`src/board.rs` は生成済み `BoardConfig` を一度だけ読み、現行 `main.rs` の起動順を
保つ次の段階に分けてドライバーを組み立てる。

| 段階 | 内容 | 制約 |
| --- | --- | --- |
| SoC early | watchdog/clock、XIP 検査、PSRAM、割り込み基盤 | board DT をパースしない。必要なボード値は生成済み定数だけ |
| board early | board I2C 初期化、前回起動から残る DMA の停止、C6 電源断 | PSRAM 再調整の前後関係を現行どおり守る |
| core devices | display、入力、USB host、SD/MMC、SDIO、電源制御 | bus、GPIO、電源を phandle で借用し、所有権を重複させない |
| optional devices | RTC、INA226、BMI270、外付けキーボード、タッチ候補 | `status` と probe 結果を `Option` に反映し、未接続を起動失敗にしない |

I2C エキスパンダの出力を複数機能が共有する点が最初の重要な境界である。E2 を
「USB の内部詳細」として `usb/hcd.rs` に置いたままにはしない。`pi4ioe5v6408` を
独立した DT ノード/ドライバーにし、VBUS、C6 電源、電源断 pulse はその GPIO
consumer として表す。read-modify-write の排他性は expander ドライバーが一元的に
守る。LCD 側 E1 についても同じ仕組みに揃える。

ソフト I2C も `BOARD_BUS` 等の三つの静的変数を名前で呼ぶ方式から、生成した
`I2cBusConfig` と `SoftI2c` インスタンスへ移す。デバイスドライバーのコンストラクタは
`&SoftI2c` と型付きアドレス/設定を受け、`i2c::board_bus()` や GPIO 番号を内部から
参照しない。バス初期化は `board.rs` が一度だけ行い、接続されていない外付け機器の
NACK は従来どおり optional device の不在として扱う。

### compatible と binding

各 `compatible` に対し、`dts/bindings/` に Zephyr 形式の binding を置く。最初に必要な例は
`acme,soft-i2c`、`nxp,pi4ioe5v6408`、`epson,rx8130ce`、`bosch,bmi270`、
`ti,ina226`、`goodix,gt911`、`sitronix,st7123`、`acme,tab5-keyboard`、
`acme,cardkb`、`acme,esp32p4-sdio-wifi`、`acme,dsi-panel` である。ここで `acme` は
例示であり、導入時に vendor-prefixes.txt で確定した実在の prefix に一括置換する。

binding は、必須プロパティ、型、範囲、参照先の compatible、`status` の扱いを定義する。
例えば RTC は `reg` と I2C 親を必須、INA226 は shunt 抵抗と較正値を必須、C6 は reset
GPIO と power GPIO と SDMMC slot を必須とする。既定値を driver の内部 `const` に隠さず、
ハードウェア特性として意味があるものは binding の既定値または DTS に表す。

生成器は次を検査する。

- 有効ノードの `compatible` が登録済みであること
- required property、値の範囲、GPIO/pinctrl の方向と重複しない排他的出力を満たすこと
- `reg` が親バスの番地空間で重複しないこと（同一アドレスの明示的 alias は除く）
- phandle が存在し、期待する型のノードを指すこと
- `chosen` の参照先が `okay` で一意であること
- shared SDMMC、I2C expander、DMA のように実行時排他が必要な関係を明示していること
- タッチのような exclusive probe group が一つの優先順を持ち、必須デバイスと混ざらないこと

既存ドライバーの「識別してから使用する」安全性は維持する。DT の `compatible` は
配線上の意図であり、I2C ACK だけでチップを信頼してよい根拠にはならない。RTC の暦値、
BMI270 の chip ID、INA226 の manufacturer/die ID、パネル/タッチの既存識別は残す。

## 段階別の実施計画

### Stage 0: 棚卸しと移行契約

- `rg` とコードレビューで、全ボード固有値を「SoC 固有」「ボード配線」「デバイス特性」
  「アプリ画面の論理寸法」に分類する一覧を作る。対象は少なくとも `main.rs`、`gpio.rs`、
  `i2c.rs`、`lcd.rs`、`touch.rs`、`power.rs`、`sdmmc.rs`、`sdio.rs`、`usb/hcd.rs`、
  `bmi270.rs`、`ina226.rs`、`rtc.rs`、入力ドライバーである。
- タイミングや PSRAM tuning のように「別ボードで再測定が必要な値」を generic driver
  の既定値にせず、DT property として明示するか、SoC profile に隔離する。
- 既存 Tab5 の起動 UART ログ、表示、入力、SD、USB、Wi-Fi、RTC/センサー診断を基準として
  保存する。以降の Stage はこの挙動を退行させない。
- 使用する Zephyr revision、DTS/binding/overlay の互換範囲、生成する Rust public API、
  feature 命名を短い ADR として固定する。DTS の解釈には常にその Zephyr toolchain を使い、
  独自パーサーを導入しない。

**完了条件:** 固定値に所有者と移行先が対応付けられ、Tab5 で許容する差分が「構成の
取得経路だけ」であると合意できる。

### Stage 1: Tab5 DeviceTree の分離

- `dts/riscv/espressif/esp32p4.dtsi` と `boards/m5stack/tab5/tab5.dts`、Zephyr 形式の binding、
  vendor-prefixes.txt、`build.rs`、`tools/gen_dt_rust.py`、`src/devicetree.rs` を追加する。
- まずは GPIO、I2C bus、I2C device、SDMMC/SDIO、USB VBUS/C6/poweroff、display の構成を
  生成する。ただしこの段階では既存 driver の公開 API を大きく変えず、生成済み定数から
  旧来の静的構成を供給してもよい。
- `board-tab5` default feature を導入し、feature 不在/複数指定、未解決 phandle、未知の
  compatible、property 型違いをビルドエラーにする。
- Tab5 の実効構成を `cargo` の build output または `tools/print_dt.py` 相当で人間が確認
  できるようにする。ソース DTS と生成後定数の両方をレビュー対象にする。

**受入条件:** `board-tab5` の release ELF/イメージ配置検査が既存どおり成功し、実機で
起動、PSRAM、LCD、入力、USB VBUS、C6 電源断が移行前と同じ順序で動く。ビルド済み
イメージが DT の文字列保持やランタイムパーサーのために不必要に肥大化していないことも
map/ELF で確認する。

### Stage 2: DeviceTree からドライバーを構成する

低リスクの独立デバイスから移し、共有資源を持つものを後にする。一括置換しない。

1. `SoftI2c`、GPIO、pinctrl を構成型へ変更し、CardKB、Tab5 Keyboard、RTC、BMI270、
   INA226 の各コンストラクタから Tab5 固定 bus/address を除く。
2. PI4IOE1/E2 を独立ドライバー化し、LCD 制御、USB VBUS、C6 電源、poweroff を consumer
   ノード経由にする。共有出力の read-modify-write と polarity を回帰試験する。
3. GT911/ST712x の exclusive probe group を `InputManager` へ組み込み、native resolution、
   回転、最大接触数を DT config から渡す。
4. microSD、C6 SDIO、USB host、LCD/DSI を、SoC ドライバーと board config に分離する。
   LCD/PSRAM/起動の順序は `chosen` と静的な phase 表で表し、実行時に任意順へ並べ替えない。
5. `board.rs` を唯一の組み立て地点にし、`main.rs` は SoC early init → board early init →
   board run の呼び出しだけに縮める。各アプリは `InputManager`、表示、ストレージ等の
   抽象を使い、DT ノードを直接検索しない。

**受入条件:** `status = "disabled"` の device driver は初期化も GPIO/I2C アクセスも行わず、
`compatible` の異なる有効ノードだけが対応 driver を構成する。Tab5 の既存診断コマンドと
実機マトリクス（表示・全入力経路・SD・USB host/MSC・Wi-Fi・電源断）を回帰する。

### Stage 3: 第2 ESP32-P4 ボードでの切り替え実証

- 実機、回路図、flash/PSRAM 種別、必要な周辺機器が確認できる別 ESP32-P4 ボードを一つ
  選ぶ。ボード名だけ作った未検証 DTS は「対応済み」としない。
- `boards/<vendor>/<name>/<name>.dts` を新設し、共有できる `esp32p4.dtsi` と Zephyr 形式の
  binding だけを使う。Tab5 固有の module、GPIO 番号、I2C アドレス、パネルコマンドを新ボードの
  driver へ持ち込まない。
- この時点で不足する表現だけを binding と共通 driver API に追加する。新ボード固有の
  `if board == ...` を共通 driver に加える場合は設計を見直す。
- `--no-default-features --features board-<name>` でコンパイル、リンク、イメージ検査、実機
  smoke test を実施する。機能を搭載しない場合はノードを `disabled` にし、ダミー driver や
  Tab5 の装置を要求しない。

**受入条件:** feature を切り替えるだけで同じソースツリーから両方のイメージが作れ、少なく
とも UART、PSRAM、表示（搭載する場合）、一つ以上の入力または I2C デバイスが各ボードで
動く。両ビルドを CI の matrix に追加する。

### Stage 4: 継続運用と拡張

- 新しい driver は、実装前に binding、対応 `compatible`、必須 property、初期化 phase、
  `status` 時の期待動作を追加する。
- 新しいボードは DTS、feature、実機確認済みの機能表、CI build を同一変更に含める。
- DTS から現在の接続図・デバイス一覧を生成し、設計資料と手作業の表が乖離しないようにする。
  ただし実機で確認した時系列・制約・障害記録は引き続き人間向け文書に残す。

## 検証と CI

各 Stage で次を機械的に実行する。

- 固定した Zephyr の `dtc`/`edtlib`/binding validator を通す host test: 正常な Tab5、
  overlay による上書き、未解決参照、重複 I2C address、未対応 compatible、複数 board feature、
  disabled node を含める。Zephyr の出力 EDT から Rust を生成できることも同時に確認する。
- `cargo check` と release build: 有効な各 board feature の matrix。生成コードは
  `rustfmt --check` 可能な安定した出力にする。
- 既存の `tools/check_elf_layout.py` と `tools/check_esp_image.py`: 全ボードで実行する。
  flash/partition が違うボードは、その差を board config と runner 設定に表し、Tab5 の
  16 MiB 前提を暗黙に流用しない。
- 実機 smoke: UART 起動点、PSRAM、表示、I2C probe、入力、USB/SD/SDIO を搭載機能に応じて
  実行する。共有 expander、PSRAM、LCD、C6 は CPU-only reset を含む試験を必須とする。
- 生成された Tab5 config と移行前の固定値を一時的な比較テストで照合し、全値の移行後に
  比較テストを削除する。二重の真実を恒久的に残さない。

## リスクと判断基準

| リスク | 対策・判断 |
| --- | --- |
| DTS 実装が独自仕様へ分岐する | DTS/binding の解釈は固定 Zephyr toolchain に委譲する。独自部分は EDT→Rust の型付き変換だけに限定する |
| 生成器の既定値が隠れた決め打ちになる | ハードウェア意味を持つ値は DTS/binding に現す。生成器の既定は構文上の値だけに限定する |
| 初期化順が変わり PSRAM/表示/C6 の reset 時に壊れる | phase を明文化し、Stage 1 は構成取得だけを置換、実機 reset 試験を各段階で行う |
| shared I2C expander を複数 driver が直接操作する | expander を provider として一元化し、consumer は GPIO handle だけを得る |
| DT を信頼して誤配線・誤搭載を見逃す | 既存の chip ID/内容検証を維持し、DT は物理的な意図、probe は実在確認として分ける |
| Tab5 のロット差を一つの固定ノードに押し込む | exclusive probe group は例外扱いとし、確認できた時点で DTS variant に分ける |
| 別ボード追加で `cfg` 分岐が共通 driver に漏れる | board 差は DTS/binding/config 型に吸収する。SoC errata だけを SoC profile に置く |

## 対象外

- 起動時に外部 DTB/FDT をロード・変更する仕組み、ユーザーが画面から配線を変更する仕組み
- Zephyr の Kconfig/CMake/west、C API、ランタイムの `struct device`、汎用 driver model の導入
  （ただし DTS v1、DTSI、overlay、binding は Zephyr 互換とする）
- ESP32-P4 以外の SoC 抽象化
- 未確認の別ボードを、DTS ファイルだけで「対応済み」と表明すること
- アプリ画面のレイアウトや機能有無を、単なる GPIO/バス定義だけで自動決定すること

## 実装後に更新する文書

Stage 1 が実装されたら、現状仕様として `docs/DEVICE_TREE.md` を追加し、`DESIGN.md` の
現状文書表へ載せる。そこで DTS の選択方法、生成物、supported property、起動 phase、
Tab5 の実効デバイス一覧を説明する。`FILE_LAYOUT.md` は `dts/`、`build.rs`、
`src/devicetree.rs`、`src/board.rs` の責務に更新する。各ドライバーの固定配線説明は
DISPLAY/INPUT/STORAGE/WIFI/USB/RTC 等の現状文書から DeviceTree の node/property 参照へ
置き換える。README.md は人間管理のため、この計画または実装に伴って変更しない。
