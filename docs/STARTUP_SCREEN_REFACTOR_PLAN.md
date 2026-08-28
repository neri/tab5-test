# 起動画面リファクタリング計画

> 索引: [`../DESIGN.md`](../DESIGN.md)
> この文書は作業計画です。現在の実装仕様は現状文書とコードを優先してください。

## 状態: Stage 0〜8完了（画像差し替え後の実機視認待ち）

| Stage | 内容 | 状態 |
| --- | --- | --- |
| 0 | 現状確認、要求整理、境界の仮固定（この文書） | 完了 |
| 1 | 起動サブアプリと画面遷移の純粋な状態契約 | 実装完了 |
| 2 | 白背景の起動画面、タイトル、状態アイコン | 完了・画像差し替え後の実機待ち |
| 3 | USB初回スキャンの協調的な状態機械化 | 完了 |
| 4 | USB自動マウントと起動画面の統合 | 完了 |
| 5 | Wi-Fi起動処理の協調的な状態機械化と有限リトライ | 完了 |
| 6 | 起動画面完了後のブラウザ／Wi-Fiメニュー／コンソール遷移 | 完了 |
| 7 | Wi-Fiメニューの白ベース化 | 完了 |
| 8 | 回帰試験、実機受入、現状文書の更新 | 完了（基本経路） |

Stageは原則として番号順に進める。Stage 3と5は別々のサブアプリだが、どちらも
Stage 1のpoll契約とStage 2の表示契約へ載せる。Stage 6では両方を初めて通常起動経路へ
接続するため、それまでは既存のコンソール起動を維持し、各サブアプリを単独で診断できる
経路から確認する。

## 目的

画面初期化後に白背景の起動画面を表示し、USBとWi-Fiの初期化を互いに独立した
協調実行型のサブアプリとして進める。利用者には中央付近のタイトルとアイコンで進行状況を
見せ、全サブアプリが終端状態へ到達した時点でネットワーク状態に応じた次画面へ遷移する。
初回表示から5秒経っても全サブアプリが終端でない場合は、最下行より1行上に任意キャンセルの
案内を表示する。その後のEscapeは起動画面の待機を打ち切り、コンソールへ遷移する。

起動後の遷移は次の契約とする。

```text
起動画面
  ├─ 5秒後にEscape ────────────→ コンソール
  ├─ IPv4利用可能 ───────────────→ ブラウザ
  │                                  └─ 終了 → コンソール
  └─ ネットワーク利用不可 ───────→ Wi-Fiメニュー
                                     ├─ 接続＋DHCP完了 → ブラウザ
                                     └─ キャンセル     → コンソール
```

USBの失敗はネットワークの行き先を変えない。USBデバイスが無い、対応クラスが無い、
列挙できない、Mass Storageを読めない、マウントできない、のいずれもUSBサブアプリの
終端結果として記録し、警告アイコンとUART診断を残したうえで起動を続ける。

## 今回の範囲

- 画面初期化後、最初の全画面描画を白一色にする
- 初版タイトル`Tab5`を2倍角で中央付近に表示する
- USBとWi-Fiを独立した状態機械として毎フレームpollする
- USB接続の可能性がある間は初回列挙とMass Storageの準備完了判定を待つ
- 起動時に見つかったUSB Mass Storageを既存の規則で自動マウントする
- 保存済みWi-Fi profileがある場合はassociationとDHCP完了まで待つ
- 回復可能なWi-Fiエラーだけを500 ms間隔で有限回リトライする
- 5秒を超えた起動待ちにだけEscapeによるコンソール遷移を提供する
- 通常完了後はブラウザまたはWi-Fiメニュー、任意キャンセル時はコンソールへ分岐する
- Wi-Fiメニュー全体を白ベースの配色へ変更する
- UARTの既存診断能力と、起動後のUSB hot-plug／Wi-Fi自動再接続を維持する

## 今回は行わないこと

- bootloaderやLCD／PSRAM初期化前に起動画面を出すこと
- USB、Wi-Fi、ネットワークの低層プロトコルそのものの機能追加
- USB自動マウントの対象、命名、読み取り専用方針の変更
- SDカードの自動マウント
- Wi-Fiの保存先、資格情報の寿命、認証失敗分類の変更
- ブラウザの表示機能やホームページ内容の変更
- 汎用async runtime、executor、heap上のfuture、別hartの導入
- 起動中の操作でUSBまたはWi-Fiだけをskipする機能

## 現状と変更が必要な理由

### 現在の起動順

`src/app.rs::run`は現在、次の順に同期処理を行い、そのままコンソールのフレームループへ
入る。

1. `Display::new`でフレームバッファを得る
2. コンソールを黒ベースで描き、`Display::start`でscanoutを開始する
3. RAM diskをformatし、`Vfs`、shell state、`AutoMount`を作る
4. `InputManager::new`の中で入力機器を初期化し、USBの初回`rescan`を同期実行する
5. `/tmp`をmountする
6. `Manager::begin_startup_auto_connect`でC6初期化と保存profile確認を同期実行する
7. associationとDHCP、USB自動マウントをコンソールループから毎フレーム進める

この構造ではUSBの初回スキャン中に画面上の状態を更新できず、Wi-Fiの結果を待たずに
コンソールが出る。また`AutoMount::service`は結果を直接コンソールへ書くので、白い
起動画面の裏でそのまま使うと表示を壊す。

### 再利用する既存実装

- `wifi_manager::Manager`はassociation、再試行待ち、DHCP、Online、失敗をすでに
  状態として保持している
- associationは20秒、DHCPは15秒でtimeoutし、回復可能／不可能な失敗分類も
  `wifi_retry`に分離済みである
- `AutoMount`は新規USB Mass Storageを250 ms間隔、最大4,000 msまで待ち、MBR上の
  読めるFAT／exFATを既存の`files::attach`経路でmountする
- `UsbHost`はtopology、接続世代、初回スキャン時間、Mass Storage inventoryを保持する
- ブラウザは白い本文面を持ち、Wi-Fi管理器を毎フレームserviceできる
- Wi-Fiメニューは入力待ちとassociation／DHCP待ちの間も毎フレームserviceする

これらのpolicyは作り直さない。起動画面のために必要なのは、同期した開始処理を小さい
段階へ分けること、表示と処理結果を分離すること、最初の遷移先を決める薄いcoordinatorを
置くことである。

## 設計方針

### 1. 「非同期」はフレーム駆動の協調実行とする

このfirmwareにはasync runtimeが無く、USB host、Wi-Fi RPC、VFSはいずれも単一所有者を
前提にしている。新しいexecutorやthreadは入れず、既存の全画面アプリと同じく表示frameと
1 kHz tickのwakeごとに短い`poll`を呼ぶ。

概念上の契約は次の形とする。実装時には借用関係とcode sizeを見てtraitではなく具象型の
同名methodにしてよいが、状態と結果の意味は固定する。

```rust
enum AppPhase {
    Pending,
    Running,
    Succeeded,
    Warning,
    Failed,
}

enum PollResult<T> {
    Pending,
    Ready(T),
}

trait StartupSubApp {
    type Output;
    fn phase(&self) -> AppPhase;
    fn poll(&mut self, context: &mut StartupContext<'_>) -> PollResult<Self::Output>;
}
```

`Pending`以外を返したサブアプリは二度とhardware操作を行わない。coordinatorは毎frame、
未完了のUSBとWi-Fiを各1回pollし、phaseが変わったアイコン領域だけを書き戻す。1 kHz wakeでは
`InputManager::service_fast`と期限確認だけを行い、全画面を描き直さない。

1回のpollで長いbusy waitをしてはいけない。USBのcontrol／bulk transactionや短いRPCのように
低層API自身が同期である部分は1回分を許すが、接続待ち、retry待ち、複数port列挙、
Mass Storage ready待ちを1回のpollにまとめない。各pollの最長時間はUARTで計測し、通常frameを
1秒以上止める経路が残った場合はStage 3または5を完了にしない。

### 2. coordinatorは2つのサブアプリと描画だけを所有する

`src/app/startup_screen.rs`を追加し、次を担当させる。

- 起動画面の初回描画と部分更新
- `UsbStartup`と`WifiStartup`のpoll順序
- phase変化の検出とアイコン更新
- 5秒の案内表示timerとEscape入力
- 全サブアプリが終端へ到達したかの判定
- Wi-Fi結果から最初の`Route`を決めること

低層driver、VFS policy、Wi-Fi再試行理由をここへ書かない。coordinatorが返す値は次の3択で
足りる。

```rust
enum InitialRoute {
    Browser,
    WifiMenu,
    Console,
}
```

通常完了ではネットワーク不良時にいったんWi-Fiメニューを開く。`Console`は、5秒後に
表示した案内に対してEscapeが押された場合だけ起動画面から直接選ぶ。

### 3. 起動画面の描画契約

論理解像度1280×720の白背景を前提に、初版は次の配置とする。

| 部位 | 位置／寸法 | 内容 |
| --- | --- | --- |
| 背景 | 全画面 | `WHITE`で1回全消去 |
| タイトル | y=216、画面幅の中央 | `Tab5`、2倍角、黒 |
| アイコン列 | y=320〜416、中央寄せ | USB、Wi-Fiの順 |
| アイコン | 64×64 | 黒輪郭、白地、状態色を一部に使用 |
| ラベル | アイコン下 | `USB`、`Wi-Fi`、1倍角、黒 |
| 状態説明 | y=456 | phaseに対応する短い1行、濃い灰色または警告色 |
| キャンセル案内 | y=`HEIGHT - 32`（最下行より1行上） | 5秒後だけ表示、1倍角、濃い灰色 |

タイトルのx座標は固定値にせず、`font::text_width("Tab5") * 2`から中央を求める。
「中央付近」は画面の幾何学中心より少し上とし、アイコンと状態行を含むまとまり全体が
上下中央に見える配置にする。

初回実装では`fill_rect`と細い線で64 px図形を描いたが、基本受入後の利用者指定により、
提供されたUSB／Wi-Fi画像を64×64へ縮小したbitmapへ差し替えた。firmwareはPNG decoderを
持たず、同じ縮小画像のalpha maskだけをリンクして白地へ合成する。レガシーMac OS／BeOSの
起動画面のように小さな独立アイコンが順に活性化する見せ方と、状態markerは維持する。

| phase | 表示 |
| --- | --- |
| `Pending` | 薄い灰色の輪郭 |
| `Running` | 黒い輪郭＋青い状態マーク |
| `Succeeded` | 黒い輪郭＋緑の小さなcheck |
| `Warning` | 黒い輪郭＋黄橙の`!` |
| `Failed` | 黒い輪郭＋赤い`!` |

`Succeeded`と`Warning`／`Failed`はすべて終端状態である。animationを必須にせず、phaseが
変わったときだけ64×96 px程度の領域を白で消して描き直す。これにより単一bufferの
全画面書き換えを増やさず、DSI underrunの既存制約を守る。

状態説明には秘密情報を出さない。SSID、password、BSSIDは表示せず、`Connecting Wi-Fi`、
`Waiting for DHCP`、`Mounting USB storage`のような処理名だけにする。詳細なfailure reasonと
所要時間はUARTへ残す。

#### 5秒後のキャンセル案内

表示開始間隔は`STARTUP_CANCEL_HINT_MS = 5_000`の1定数とする。
起動画面の初回flushが成功した時刻を起点とし、USBとWi-Fiのいずれかが未完了のまま
5,000 ms経ったとき、最下行より1行上へ`ESC  CANCEL STARTUP AND OPEN CONSOLE`を中央寄せで表示する。
これはtimeoutや失敗の通知ではなく、正常に時間のかかるassociationやDHCPを待たないための
任意操作である。

Escapeは案内を表示した後だけ有効とする。5秒より前のEscapeとその他のkeyはそのframeで捨て、
5秒後の操作として持ち越さない。USB keyboardは列挙済みの場合だけ使え、CardKBと内蔵keyboardは
それぞれの初期化後に使える。

Escapeは起動待ちと起動画面固有の新規retryを打ち切るが、共有resourceを破棄しない。
未commitのUSB boot scanは次のpollを発行せず破棄し、通常の`InputManager::service`が再scanできる状態を立てる。
完了済みのUSB列挙とmountは残し、未完了の`AutoMount` pendingは同じinstanceのままコンソールloopへ
引き継ぐ。Wi-Fiの進行中のassociationとDHCPも`Manager`へ残し、コンソールへ遷移した後は
起動画面固有の500 ms retryではなく既存policyでserviceする。これにより、Escapeからコンソール表示までを
ハードウェアの長いcleanup待ちで止めない。

### 4. USBサブアプリ

#### 状態

```text
Pending
  → DetectingRootPort
      ├─ 接続なし                → NoDevice（成功終端）
      └─ 接続の可能性あり        → Enumerating
  → Enumerating
      ├─ inventory確定           → ReconcilingMounts
      └─ 列挙不能／budget終了    → EnumerationWarning（警告終端）
  → ReconcilingMounts
      ├─ pendingなし             → Ready（成功終端）
      └─ 4,000 ms budget終了     → MountWarning（警告終端）
```

「USBに何か繋がっている可能性」は、root portのconnect状態、connect-change、hub配下の
既知port、初回scan中のpresenceのいずれかで判断する。単に対応classが無いことを
「何も繋がっていない」とは扱わない。未知classでも列挙が確定すればサブアプリは終われる。

#### `InputManager`から初回スキャンを分離する

現在の`InputManager::new`はCardKB、内蔵keyboard、touchとUSB host生成に加えて、最大1秒の
root接続待ちを含む`UsbHost::rescan`まで行う。これでは起動画面を先にflushできないため、
次の二段階へ分ける。

- `InputManager::new`は入力driverと空の`UsbHost`を作るだけにする
- USB起動サブアプリが表示開始後にboot scanを開始する

初回scanでだけ使う`BOOT_CONNECT_WAIT_MS = 1,000`、Mass Storage readyの
`BOOT_MASS_STORAGE_READY_MS = 4,000`、初回timing logは削らず、USB起動サブアプリ側へ
責務を移す。通常時の短いconnect waitと再接続backoffは変更しない。

#### `rescan`を中断可能な段階へ分ける

`UsbHost::rescan`の最終結果とregistry更新規則は維持したまま、起動時だけ
`begin_boot_scan`／`poll_boot_scan`相当のAPIを追加する。最低でも次をpoll境界にする。

1. VBUS／controller準備
2. root port接続待ち
3. port reset／enable待ち
4. root device descriptorとaddress設定
5. configuration読出しとclass bind
6. hubがあればportごとの列挙
7. periodic HID開始とinventory確定

途中で失敗してもregistryの世代、topology epoch、connection epoch、HIDのperiodic channel、
MSC sessionが半端な状態で公開されないよう、現在の`rescan`が行うcommit境界を維持する。
通常の`usbrescan`コマンドと障害回復は当面既存の同期APIを使用し、起動経路を実機受入してから
共通化を判断する。最初から全rescan用途を書き換えて回帰範囲を広げない。

### 5. USB自動マウントとの統合

`AutoMount::service`は現在、VFS変更とconsole表示を同時に行う。起動画面から同じmount policyを
使えるよう、処理結果を`AutoMountNotice`として返し、表示先を呼び出し側に分ける。

- 起動画面: iconの成功／警告へ反映し、詳細はUARTへ出す
- コンソール: 現在と同じ`automount: ...`行として表示する

mountは必ず既存の`files::attach`を通し、fingerprint、generation、読み取り専用、
`usbMpN`命名を維持する。起動時に既に挿さっているMass Storageも250 msごとの再試行と
4,000 msのbudgetを使う。1つでもmountできれば成功、読めるfilesystemが無い／全partitionが
失敗した場合は警告とするが、どちらも終端である。

起動画面が終わった後も同じ`AutoMount` instanceをコンソールへ渡す。`offered`、`pending`、
deadlineを作り直すと同じ媒体を二度mountしたり、利用者の`umount`を取り消したりするためである。

### 6. Wi-Fiサブアプリ

#### 正常なネットワークの定義

起動分岐における「正常」は、`wifi_manager::State::Online`であり、かつ生きているRPC sessionと
IPv4 addressを持つ`net::Stack`があることとする。associationだけ成功した
`AssociatedNoLease`は正常に含めず、Wi-Fiメニューへ送る。DNS疎通、gateway ping、外部HTTPへの
probeは行わない。外部serverの障害で毎回起動が止まることを避けるためである。

#### 状態

```text
Pending
  → ReadingPersistentState
      ├─ Wi-Fi OFF               → NeedsSetup（警告終端）
      ├─ 保存profileなし         → NeedsSetup（警告終端）
      ├─ C6初期化失敗            → retry判定
      └─ 保存profileあり         → Associating
  → Associating／RetryWaiting
      ├─ 認証系など回復不能      → NeedsSetup（失敗終端）
      ├─ 回復可能、試行回数内     → 500 ms後に再試行
      ├─ 3回目が失敗               → NeedsSetup（警告終端）
      └─ association成功         → RequestingDhcp
  → RequestingDhcp
      ├─ lease取得               → Online（成功終端）
      └─ timeout                 → NeedsSetup（警告終端）
```

`Manager`を別に作らず、起動後もブラウザ、Wi-Fiメニュー、コンソールが使う同じinstanceを
サブアプリへ貸す。これによりsession、stack、保存profileの有無、資格情報のzeroize、履歴、
再接続方針を引き継ぐ。

`begin_startup_auto_connect`のC6 bring-up、mode取得、config取得、station startは現在同期して
いるため、それぞれを開始状態機械のstepへ分ける。association以降は既存の`Manager::service`を
使い、起動画面固有の状態をWi-Fi管理器本体へ重複実装しない。

#### 起動画面だけの短い有限リトライ

現在の管理器は通常運用中の回復のため最大30秒のbackoffを継続する。このpolicy自体は維持し、
起動画面のcoordinatorにだけ短い固定間隔と有限の試行回数を置く。

- 初回を含め最大3回（retryは最大2回）
- 回復可能な失敗から次の試行までは500 ms固定とし、指数backoffを使わない
- 認証失敗など`wifi_retry::Decision::Stop`は直ちに終了
- 3回を使い切ればWi-Fiメニューへ進む
- associationの1試行20秒とDHCPの15秒timeoutは既存値を維持する

この打ち切りは接続管理器の状態や資格情報を破壊しない。Wi-Fiメニューへ進んだ時点で利用者が
別APを選べるようにする。ブラウザ、Wi-Fiメニュー、コンソールへ移った後は500 ms固定の
起動時policyを持ち越さず、現在の理由別分類と最大30秒の指数backoffをそのまま使う。
起動画面を閉じるためだけに保存profileを削除したりWi-FiをOFFにしたりしない。

### 7. 画面遷移と各画面の戻り値

`app::run`のコンソールループを巨大な汎用window managerへ変えない。初回起動の前段に次の
限定的な遷移を置き、以後は現在のコンソール中心の構造へ戻す。

```rust
match startup_screen::run(...) {
    InitialRoute::Browser => browser::run(..., None),
    InitialRoute::WifiMenu => match wifi_menu::run(..., Entry::Startup) {
        WifiMenuOutcome::Online => browser::run(..., None),
        WifiMenuOutcome::Cancelled => {}
    },
    InitialRoute::Console => {}
}
run_console_loop(...);
```

起動から開くブラウザは`start = None`、つまり既存の内蔵home pageとする。ブラウザ終了理由に
かかわらず次はコンソールである。

Wi-Fiメニューは現在`()`を返すため、次の結果を返すようにする。

| 結果 | 意味 | 起動経路での遷移 |
| --- | --- | --- |
| `Online` | associationとDHCPが完了し、`Manager`がOnline | ブラウザ |
| `Cancelled` | Escape、OFF画面からの終了、scan error画面からの終了 | コンソール |

通常のshellコマンド`wifi`から開いた場合はどちらの結果でも従来どおりコンソールへ戻る。
接続処理中のEscapeは画面だけを閉じ、現在どおり管理器に処理を引き継ぐ。資格情報を消すための
暗黙disconnectにはしない。

### 8. Wi-Fiメニューを白ベースへ変更する

`wifi_menu.rs`の暗い背景色と、各描画箇所へ直接書かれた`WHITE`文字を一緒に変更する。
背景だけ白にすると本文が消えるため、先に用途別のsemantic colorを固定する。

| 用途 | 方針 |
| --- | --- |
| 画面背景 | 白 |
| header／footer | ごく薄い灰色 |
| panel | 薄い灰色、黒い境界 |
| 通常文字 | 黒 |
| 補助文字 | 濃い灰色 |
| 選択行 | 淡い青、黒文字 |
| 主操作 | 濃い青、白文字 |
| 成功 | 濃い緑 |
| 警告 | 白地でも読める濃い黄土色 |
| エラー／危険操作 | 濃い赤。塗りボタン内だけ白文字 |

対象はAP一覧だけでなく、Wi-Fi OFF、scan中、password入力、profile選択、association、DHCP、
結果、forget確認の全画面とする。選択状態を色だけに依存させず、現在の枠や位置も残す。
タッチ当たり判定と座標は配色変更では動かさない。

## Stage別作業

### Stage 0: 現状確認と方針の仮固定

- `DESIGN.md`から関連する現状文書と既存planを確認する
- `app::run`、`InputManager`、`UsbHost`、`AutoMount`、`wifi_manager`、`wifi_menu`、
  `browser`の所有関係と同期区間を記録する
- 利用者の回答を本書の「確認済み事項」に記録する

完了条件は本書と`DESIGN.md`の索引が追加され、READMEが変更されていないこと。

### Stage 1: 状態契約とhost test

- `AppPhase`、USB結果、Wi-Fi結果、`InitialRoute`を純粋なenumとして追加する
- サブアプリ完了条件とroute選択をhardware非依存関数へ分離する
- 5,000 ms未満で案内とEscapeが無効、5,000 ms以後は案内表示と`Console`遷移が有効に
  なることを境界値testで固定する
- USB警告がWi-Fi routeへ影響しないことをtestする
- `Online`だけがBrowserを選び、Off、profileなし、NoLease、FailedはWifiMenuを選ぶことを
  testする
- 完了済みサブアプリを再pollしてもhardware操作しない契約をtest doubleで固定する

完了条件はhost testで全状態組み合わせが通り、まだ通常起動経路を変更していないこと。

### Stage 2: 起動画面renderer

- `startup_screen.rs`に白背景、中央タイトル、USB／Wi-Fiアイコン、状態行を実装する
- 静的な診断入口から各phaseの見本を順に表示できるようにする
- 初回だけ全画面flushし、phase変更はiconと状態行の矩形だけflushする
- 5秒後の案内は最下行より1行上だけ追加flushし、それ以前は空白のままにする
- framebuffer境界、文字中央寄せ、未収録glyph不使用をhost testまたは定数assertで確認する

実機では白の色むら、タイトル位置、アイコンの識別性、警告色の可読性、部分更新の残像、
LCD underrun増加が無いことを確認する。

### Stage 3: USB初回スキャン

- `InputManager::new`から同期初回`rescan`を分離する
- 起動用scanを開始／poll可能な状態へ分ける
- no-device／列挙失敗の2,000 ms待ち中もWi-Fiサブアプリと画面更新が進むことを確認する
- hub配下を含む列挙中も入力のfast serviceとWi-Fi frame受信を飢餓させない
- キャンセル時は未commitのscan状態を捨て、通常serviceで再scanできる状態へ戻す
- 既存のboot timing logとregistry commit規則を維持する
- 同期`usbrescan`と通常のhot-plug回復に回帰が無いことを確認する

実機対象はUSBなし、HID直結、MSC直結、給電hub、hub配下HID＋MSC、未知class、列挙失敗する
deviceである。

### Stage 4: 起動時自動マウント

- `AutoMount`のVFS処理とconsole表示を分離する
- 起動時に既接続のUSB Mass Storageをmount完了／budget終了まで進める
- 起動後も同じ`AutoMount` instanceを通常loopへ引き継ぐ
- キャンセル時の`pending`とdeadlineも同じinstanceでコンソールloopへ引き継ぐ
- superfloppy、読めないpartition、空card reader、遅いMSCを警告終端にする
- `/tmp` mountとUSB mountの順序を固定し、mount point競合を起こさない

実機ではFAT、exFAT、複数partition、空card reader、4秒近く準備にかかる媒体を確認する。
起動後の抜去で自動unmount、再挿入で新generationになることも回帰確認する。

### Stage 5: Wi-Fi起動サブアプリ

- 保存状態読出しまでの同期処理をpoll可能なstepへ分ける
- 既存の`Manager::service`からphaseを読み、起動画面phaseへ写す
- Online、profileなし、永続OFF、認証失敗、association timeout、DHCP timeoutを終端化する
- 回復可能エラーに起動画面だけの最大3回、500 ms固定間隔を適用する
- 起動画面を出た後は現在の理由別retryと指数backoffへ戻ることをtestする
- キャンセルは進行中のassociation／DHCPを破棄せず、通常policyへ所有権を戻す
- 起動画面終了後も同じmanagerと通常の自動再接続policyを維持する
- SSID／passwordを画面、UARTの新規log、test failureへ出さない

実機では保存profile成功、AP不在、誤password、DHCP不在、C6 link failure、Wi-Fi OFF、
profileなしを確認する。AP不在から3回の試行中に復帰した場合は起動画面からBrowserへ進むことも
確認する。

### Stage 6: 初回画面遷移

- display開始後、既存コンソール描画より先に白い起動画面を出す
- RAM disk、VFS、InputManager、AutoMount、WifiManagerを一度だけ生成し、全画面間で引き継ぐ
- USBとWi-Fiが両方終端になるまで起動画面を閉じない。ただし5秒後の明示的なEscapeは例外とする
- `InitialRoute`に従ってBrowser、Wi-Fiメニュー、Consoleのいずれかを開く
- 5秒後のEscapeは未完了処理を安全に引き継いでConsoleを直接開く
- Wi-Fiメニュー接続成功からBrowser、キャンセルからConsoleへ進む
- Browserを終了したらConsoleへ進む
- shellから開く`wifi`と`browser`の従来動作を維持する

起動分岐はroute tableのhost testに加えて実機で一巡させる。

### Stage 7: Wi-Fiメニュー白テーマ

- semantic color定数を導入し、全draw関数を用途別の色へ移す
- 全画面を白系背景へ変更する
- 通常、選択、disabled、成功、警告、error、危険確認のcontrastを実機で確認する
- keyboardとtouchの操作、選択行、password mask、footer instructionが消えていないことを
  確認する
- 起動画面から入った場合とshellから入った場合を両方確認する

### Stage 8: 総合受入と文書同期

- host test、release build、Clippyの既知状況、ELF layout検査、ESP image検査を実行する
- 下記の実機matrixを完走する
- `APPS.md`へ起動画面と画面遷移の現状を記録する
- `INPUT.md`と`USB.md`へ初回scanの新しい所有者とpoll境界を記録する
- `FILESYSTEM.md`へ起動画面中のautomountと通知先を記録する
- `WIFI.md`へ起動時有限待ちと起動後再接続の違い、白いメニューを記録する
- `CONSOLE_SHELL.md`と`BROWSER.md`は遷移の説明が必要な場合だけ更新する
- `FILE_LAYOUT.md`へ`startup_screen.rs`と責務を追加する
- 実装と本書のStage状態、判断記録を同期する
- READMEがこの作業で変更されていないことを確認する

## 実機受入matrix

| USB | 保存Wi-Fi | 期待する起動結果 |
| --- | --- | --- |
| なし | Onlineになる | USBなしを成功終端、Browser |
| HIDのみ | Onlineになる | HID利用可能、Browser |
| FAT／exFAT MSC | Onlineになる | 自動mount後にBrowser |
| 空card reader | Onlineになる | USB警告終端、Browser |
| 列挙失敗device | Onlineになる | USB警告終端、Browser |
| 任意 | profileなし | Wi-Fiメニュー |
| 任意 | Wi-Fi OFF | OFF画面を白ベースで表示 |
| 任意 | 誤password | 有限回で停止しWi-Fiメニュー |
| 任意 | AP不在後に復帰 | 3回の試行中ならBrowser |
| 任意 | DHCP timeout | Wi-Fiメニュー |
| 初期化が5秒超 | 任意 | 最下行より1行上にEscapeの案内 |

画面遷移はさらに次を確認する。

1. 起動から5秒未満は案内を出さず、Escapeを押しても画面遷移しない
2. 5秒経過後は最下行より1行上に案内が出て、EscapeでConsoleが開く
3. 起動画面をキャンセルした後もUSBとWi-Fiの共有resourceが通常loopから使える
4. Wi-Fiメニューで接続とDHCPが完了するとBrowserが開く
5. Wi-FiメニューでEscapeを押すとConsoleが開く
6. Browserで`q`またはCtrl+Qを押すとConsoleが開く
7. Consoleから`wifi`を開いて閉じるとConsoleへ戻る
8. Consoleから`browser`を開いて閉じるとConsoleへ戻る
9. Browser／Console中のUSB挿抜で従来どおり自動mount／unmountする
10. Browser中にWi-Fiが切れても管理器が再接続を続け、古いstackを使用しない

各caseでUARTに秘密情報が出ていないこと、DMA errorが0であること、LCD underrun countが
起動描画によって増えていないことも確認する。

## 失敗時の扱い

- display開始またはflush失敗: 現在どおりUARTへ記録し、描画不能な状態で処理を続けない
- RAM disk初期化失敗: `/tmp`無しで起動を続け、USB／Wi-Fi結果とは分けてUARTへ記録する
- USB警告／失敗: 起動を続ける。console到達後の`usbinfo`／`lsusb`／`usbrescan`で診断可能にする
- USB mount失敗: device registryは残し、console到達後の明示mount／再挿入を妨げない
- Wi-Fi失敗: Wi-Fiメニューへ進み、AP選択、再scan、ON操作、profile削除を利用者へ委ねる
- Wi-Fiメニューのscan失敗: retryまたはcancelを選べる現在の操作を維持する
- Browser初期化失敗: UARTへ記録して戻る既存挙動に従い、Consoleへ進む
- 5秒後の起動キャンセル: 未完了resourceを破棄せず通常loopへ引き継ぎ、Consoleを開く

起動画面を永久に閉じない失敗を作らないことが重要である。すべてのサブアプリは成功、警告、
失敗のいずれかの終端を持ち、hardwareの無応答には必ず既存または起動画面固有のbudgetを置く。

## 確認済み事項

2026-08-28に次を利用者へ確認した。

1. 初版タイトルは`Tab5`とする
2. 起動画面のWi-Fiは失敗後に一瞬だけ待ってすぐ再試行する。初版では既存の短い間隔に
   合わせて500 ms、初回を含め最大3回とする
3. 起動直後のBrowserは現在の内蔵home pageから始める
4. 起動から5秒経っても初期化中なら最下行より1行上にキャンセル案内を出し、その後の
   Escapeでコンソールへ遷移する

ブラウザ、Wi-Fiメニュー、コンソールなど起動画面以外では、現在のWi-Fi retry policyを
変更しない。アイコンの細部と白テーマの具体的なRGB565値は、機能契約を変えないため
Stage 2／7の実機視認性で調整する。

## 判断記録

### 2026-08-28: plan作成時

- USB初回scanは`InputManager::new`内の同期処理であり、そのままサブアプリで包むだけでは
  「状態を表示しながら非同期に進める」要求を満たさないため、Stage 3でscan自体を段階化する
- USB Mass Storageの準備待ちとmount retryは既存`AutoMount`の4,000 ms policyを再利用し、
  起動画面専用の別mount実装を作らない
- Wi-Fi association／DHCPはすでに管理器の状態機械であるため、policyを複製せず開始処理と
  起動画面の終了条件だけを追加する
- 正常なネットワークは外部疎通ではなくOnline＋IPv4とする。外部依存でbootを失敗させない
- USBの問題はWi-Fiによる画面遷移を妨げず、診断可能な警告終端として扱う
- Wi-Fiメニューは背景色だけでなくsemantic color全体を変更し、白地に白文字が残る事故を防ぐ
- 利用者回答により、起動画面の回復可能なWi-Fi失敗は500 ms後に最大2回再試行する。
  起動画面以外の理由別分類と指数backoffは変更しない
- 5秒はUSBの最低2秒探索後、Mass Storage準備待ちや最大20秒のassociation、15秒のDHCPを
  待たない選択肢を早めに出せるため採用する。案内表示は処理を自動中断しない

### 2026-08-28: 初回実装

- `src/app/startup_screen.rs`を追加し、初回の白画面、部分更新するUSB／Wi-Fiアイコン、
  5秒後のEscape案内、3つの初回routeを実装した
- USB rootが空、または接続を検出しても列挙できないときはconnect wait 1 msの短い`rescan`を
  100 ms間隔で最低2,000 ms続ける。有限なstartup判定後も通常の接続event／fallback scanへ
  引き継ぎ、起動画面中と次画面のどちらでも接続処理を止めない。接続を検出した回の列挙処理は
  既存`UsbHost::rescan`の同期境界を維持するため、実機で1 pollの所要時間を受入確認する
- USB Mass Storageのready／mount待ちは既存`AutoMount`へ一本化し、起動画面中は結果を
  UARTだけへ出す。Escape後は同じpending stateをConsole loopへ引き継ぐ
- Wi-Fi管理器へ起動画面専用の500 ms・最大3試行policyを追加した。起動画面を出ると既存の
  reason別policyへ戻し、進行中のassociation／DHCPとOnline後のlink監視は継続する
- Wi-Fiメニューは起動経路とshell経路を区別して結果を返す。起動経路でOnlineになれば
  profile保存結果が警告でもBrowserへ進み、OFFからONへ戻した自動接続もOnlineを待つ
- `cargo build --release`、`tools/check_elf_layout.py`、hostで実行可能なworkspace libraryの
  275 testを通過した（別途、表示・USB・C6を使う実機受入が必要）

### 2026-08-28: 基本経路の実機受入

利用者が次の依頼済み確認項目について実機動作を確認した。

1. USB-Aが空で接続可能な保存済みWi-Fiを使う通常起動。白い起動画面からBrowserへ進み、
   Browser終了後にConsoleへ進む経路
2. 保存済みAPへ接続できず初期化が5秒を超える起動。5秒未満のEscapeを受け付けず、
   最下行より1行上の案内表示後にEscapeでConsoleへ進む経路

この確認を初版の基本受入とする。FAT／exFAT MSC、空card reader、列挙失敗device、Wi-Fi OFF、
誤password、DHCP timeoutなど実機受入matrixの個別組み合わせは、該当ハードウェアや障害条件を
用意できるときの拡張回帰項目として残す。

### 2026-08-28: 提供画像へのアイコン差し替え

利用者提供のUSB／Wi-Fi PNG（各240×240、RGBA）をLanczos filterで64×64へ縮小し、起動画面の
手続き描画アイコンと差し替えた。firmwareにはPNG decoderを追加せず、縮小PNGから得た8-bit
alpha maskを`include_bytes!`でリンクし、白背景上へ状態に応じた黒／灰色で合成する。
Running／Succeeded／Warning／Failedの状態markerは従来どおり右下へ重ねる。

### 2026-08-28: USB初回判定後の継続処理

実機で起動時USBを認識しない場合があり、初回判定後の起動画面中に再接続処理が止まっていた。
原因は、rootがconnectedなら列挙失敗でもUSBサブアプリを終端にし、起動画面が既存Split HID用の
`service_fast`しか呼ばなかったことだった。未接続／列挙失敗の初回再試行を最低2秒へ延長し、
有限な判定を終えた後は通常の`InputManager::service`と`AutoMount`を起動画面からも継続する。
inventoryが空の場合は最初の300-frame fallbackを次frameへ前倒しする。
