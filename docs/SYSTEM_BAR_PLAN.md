# 上部統合システムバー計画

> 索引: [`../DESIGN.md`](../DESIGN.md)
> この文書は作業計画です。現在の実装仕様は[`APPS.md`](APPS.md)、
> [`BROWSER.md`](BROWSER.md)、[`CONSOLE_SHELL.md`](CONSOLE_SHELL.md)とコードを
> 優先してください。

## 状態: **未実装**（Stage 0のみ完了）

| Stage | 内容 | 状態 |
| --- | --- | --- |
| 0 | 現状確認、用語、画面契約、段階分けの固定（この文書） | 完了 |
| 1 | 4つのアプリ区分と、座標・表示状態・hit testだけを持つ上部バー部品 | 未着手 |
| 2 | 時計・Wi-Fi・バッテリーの共有状態と部分更新 | 未着手 |
| 3 | 通常GUIアプリのhost、coordinator、ランチャー | 未着手 |
| 4 | Browserを通常GUIアプリとして上部バーへ統合 | 未着手 |
| 5 | Wi-Fi設定とバッテリー画面をミニアプリへ移行 | 未着手 |
| 6 | 専有GUIアプリとコンソールアプリの境界を固定 | 未着手 |
| 7 | 入力、ミニアプリ往復、画面遷移の回帰 | 未着手 |
| 8 | 実機受入、現状文書と本書の状態更新 | 未着手 |

Stageは原則として番号順に進める。Stage 1と2は既存画面へ接続せずにhost testと
診断用の静止表示で確認できる。Stage 3で通常GUIアプリとミニアプリの所有関係を作り、
Stage 4〜6で区分ごとに移す。Browserは自前toolbarの上へ別の帯を足さず、Stage 4の1回の変更で
既存toolbarを統合バーへ置き換える。実機でStage 5を受け入れるまでは通常buildの
起動経路を従来UIへ戻せる開発用featureを残し、Stage 8で削除する。

## 目的

通常GUIアプリと、そのアプリから開くミニアプリで、ランチャー、Wi-Fi、バッテリー、
将来の音量、時計を同じ上端の
48 pixelへ置く。アプリ固有の戻る、進む、アドレス、タイトルも別の段へ積まず、
同じ帯の中央を使う。

このfirmwareにはOSのtask、window、window managerが無い。したがって新しい帯は
実行中taskの一覧を持つ「taskbar」ではなく、1つだけ動く前景アプリの入口と本体状態を
示す「system bar」と呼ぶ。別の通常GUIアプリを選んだときは現在の通常GUIアプリを終了し、
次の1つを起動する。ミニアプリは呼出元の通常GUIアプリを停止した状態で一時的に画面を借り、
閉じると同じ状態へ戻す。隠れたアプリを実行し続けたり、windowを重ねたりしない。

## 今回の範囲

- 論理画面の上端48 pixelを、通常GUIアプリとミニアプリで共通のsystem barにする
- 左端をランチャー、右端をWi-Fi、バッテリー、将来の音量、時計に固定する
- 中央を現在のアプリが使うapp areaとし、Browserの操作を同じ一段へ収める
- system領域とapp領域の描画、hit test、dirty矩形の所有者を分ける
- Browser、Console、専有GUIの前景遷移を`src/app.rs`の1つのcoordinatorへ集め、
  Wi-Fi設定とバッテリー画面は通常GUI host内のミニアプリとして往復させる
- 通常GUI、ミニ、専有GUI、コンソールの4区分をコードと現状文書で明示する
- 起動画面、専有GUIアプリ、コンソールにはsystem barを表示しない
- Wi-Fiと時計の既存状態を再利用し、バッテリー測定を全画面モニターと共用する
- keyboard、touch、USB mouseのどれでもsystem barとBrowser固有操作を使えるようにする
- 単一framebuffer、部分書き戻し、表示DMA優先の現在の制約を維持する

## 今回は行わないこと

- preemptive multitasking、thread、async executor、別hartでのアプリ実行
- 複数アプリの同時保持、task一覧、window、重なり、最小化、dragによる配置
- アプリの画面bitmapを退避して即座に復元する仕組み
- 通知センター、toast履歴、常駐アプリ用のtray API
- 音声codec、speaker、音量制御そのものの実装
- 起動画面のUSB／Wi-Fi初期化順や接続policyの変更
- `win`診断画面を製品用desktopまたはsystem shellへ昇格すること
- Browserの取得、履歴、HTML、TLS、status行の意味の変更
- Consoleの上端余白、156列×44行、cell描画契約の変更
- READMEの変更

## アプリ区分

この改修で、画面を使う処理を次の4区分へ分ける。区分は名前や起動コマンドではなく、
system barの有無、画面の所有者、ミニアプリと協調できるかで決める。

| 区分 | system bar | ミニアプリとの協調 | 画面と実行の契約 | 例 |
| --- | --- | --- | --- | --- |
| 通常GUIアプリ | 表示する | する | 長く使う前景画面。自分の状態を保持したままミニアプリへ一時的に画面を渡せる | Browser |
| ミニアプリ | 表示する | 通常GUIアプリに従属する | system barのlauncherまたはindicatorから起動し、終了すると呼出元の同じ状態へ戻る | Network settings、Battery details、将来のVolume |
| 専有GUIアプリ | 表示しない | しない | 画面全体を専有する。system barの入力、描画、ミニアプリ起動を止める | `coordtest`、`paint`、`touchtest`、`fonttest`、`axistest`、`win` |
| コンソールアプリ | 表示しない | しない | Consoleのcell grid上で開始から終了まで動き、結果をConsoleへ返す | `ls`、`cat`、`mount`、多くの診断command |

### 通常GUIアプリ

通常GUIアプリは`NormalGuiHost`としてsystem bar、共有indicator、launcher、active appの
状態をまとめて所有する。初版ではBrowserだけである。Browserのpage、scroll、履歴、
編集中address、進行中の取得はBrowserの状態であり、ミニアプリへ渡さない。

ミニアプリを開く前に、Browserは進行中resourceを一時停止または安全な待機状態へ移す。
ミニアプリが閉じたらBrowserを全面再描画し、同じpageと履歴から再開する。別の通常GUI
アプリを選んだ場合だけBrowserを終了し、socketとVFS handleを返す。

### ミニアプリ

ミニアプリは独立したtaskでも通常GUI routeでもない。`NormalGuiHost`から同期的に呼ばれ、
画面と必要な共有管理器を一時的に借りる。呼出元の通常GUIアプリは裏でpollされず、停止した
値としてcall stack上に残る。

ミニアプリ自身もsystem barを表示するが、呼出元のapp固有buttonは消し、app areaには
ミニアプリのtitle、戻る操作、必要なら短い状態だけを描く。ミニアプリ内でlauncherが押された
場合は別のミニアプリを直接callせず、`MiniOutcome::Open(MiniId)`をhostへ返して同じ深さで
入れ替える。Cancel／完了は`MiniOutcome::Dismissed`を返す。

Wi-Fi、battery、将来のvolumeのindicatorは対応するミニアプリへの短い入口でもある。
対応するミニアプリ自身を表示中に同じindicatorを押しても再入しない。

### 専有GUIアプリ

専有GUIアプリへ入る前に通常GUIアプリとミニアプリは終了する。専有中はsystem barを
描かず、bar用のtouch hit testとindicator writebackも行わない。終了後の行き先は明示的な
routeで、前の通常GUIアプリが自動的に裏から復帰することはない。

`coordtest`の全pixel測定、`paint`の上端を含むcanvas、`win`固有の下部taskbarのように、
system barを重ねると目的自体が変わる画面をこの区分にする。

### コンソールアプリ

Consoleはsystem barを持たない156列×44行のhostを維持する。コンソールアプリは
`Console`へ文字を出し、shell commandのreturnまで同じcell grid上で動く。indicator更新の
ためにConsoleの上端を予約せず、ミニアプリを開くsystem actionも受け付けない。

shell command名だけでは区分を決めない。`browser`のように通常GUIアプリへ遷移するcommandや、
`coordtest`のように専有GUIアプリへ遷移するcommandは、GUIへの入口であってコンソールアプリ
ではない。反対に`wifi on|off|status|forget`のようにConsoleへ結果を返す処理は
コンソールアプリである。

現在の引数なし`wifi`と`battery`／`batinfo`は全画面GUIをConsoleから直接開くため、新しい
区分へそのまま残さない。Stage 5で、引数なし`wifi`はsystem barのNetwork settingsを案内する
Console上の応答へ変更し、`battery`／`batinfo`は共有sampleを1回Consoleへ表示するcommandへ
変更する。継続表示と操作はミニアプリ側だけに置く。

起動画面はアプリではなくsystem初期化画面なので4区分の外に置く。system barを表示せず、
初期化後はOnlineかどうかにかかわらず通常GUIアプリのBrowserへ進む。ネットワーク未設定時は
Browserの組み込みhomeとsystem barを表示し、利用者がlauncherまたはWi-Fi indicatorから
Network settingsミニアプリを開く。起動画面からミニアプリを直接開かない。

## 現状と変更が必要な理由

### 画面ごとに上端の契約が異なる

- Browserは`y=0..48`をtoolbar、`y=56..688`をviewport、最下部32 pixelを
  status行として使う。toolbar右端には既にWi-Fi indicatorがある
- Wi-Fiメニューは上端78 pixelをheaderとして使い、一覧は`y=94`から始まる
- Consoleは上8 pixelを余白とし、1280×720に156列×44行を固定している
- バッテリー、ペイント、各診断画面は全画面を自分で消去してから描く
- `win`は画面下に独自のWindows 95風taskbarを持つが、USB mouseとpointerの診断用である

この状態へ独立した48 pixelのsystem barを足すと、Browserだけ上端が96 pixelになり、
画面の13%を2本の操作帯が占める。問題は上か下かより、system操作とapp操作を別々の
全幅bandにすることである。本計画ではBrowserの既存48 pixelをsystem barの高さとして
再利用し、縦方向を増やさない。

### 画面遷移はConsole loopに埋め込まれている

現在の`src/app.rs`はConsoleのframe loopを本体とし、shell commandの`Outcome`に応じて
各全画面`run`を同期呼出しする。Browser自身もWi-Fi indicatorが押されたときに
Wi-Fiメニューを同期呼出しし、戻るとBrowserを全面再描画する。

このまま各画面がランチャーから互いを直接呼ぶと、`Browser → Launcher → Battery →
Launcher → Browser`のたびにcall stackが深くなり、resourceを誰が返すかも画面ごとに
変わる。Stage 3でConsole、通常GUI、専有GUIの前景route選択を`src/app.rs`へ戻す。
ミニアプリだけは通常GUI hostの内側で常に同じ深さへ同期呼出しし、ミニアプリ同士や
別の通常GUIアプリを直接起動しない形にする。

### indicatorに必要な能力は揃っていない

- Wi-Fiは`wifi_manager::Manager`がassociation、DHCP、Online、再接続を保持している
- 時計はRX8130CEと`wall_clock::local_now`があり、UTC保存と既定JST表示の規則がある
- バッテリーはINA226を全画面`battery`アプリが初期化し、約1秒ごとに測定している
- 音声出力と音量状態は現在の実装に存在しない

初版で音量iconを有効にすると、実際には変更できない状態を変更できるように見せる。
右端には将来のslot幅だけを予約するが、音量driverが入るまでは絵もhit targetも持たせない。

## 固定する設計

### 1. 二段にせず、1本をsystemとappで共有する

論理画面1280×720に対し、通常GUIアプリとミニアプリの契約を次で固定する。

```text
y=0   ┌──────────────────────────────────────────────────────────────┐
       │≡│      app固有操作／title       │Wi-Fi│BAT│ VOL │  HH:MM │
y=48  ├──────────────────────────────────────────────────────────────┤
       │                     app content                              │
y=720 └──────────────────────────────────────────────────────────────┘
```

| 領域 | x | 幅 | 所有者 | 初版の意味 |
| --- | ---: | ---: | --- | --- |
| Launcher | 0 | 52 | system | ランチャーを開く |
| App | 52 | 1004 | active app | app固有操作またはtitle |
| Wi-Fi | 1056 | 48 | system | 接続段階。tapでNetwork settingsミニアプリ |
| Battery | 1104 | 48 | system | 電圧由来の目安。tapでBattery detailsミニアプリ |
| Volume reserve | 1152 | 48 | system | 初版は空白、hit targetなし |
| Clock | 1200 | 80 | system | `HH:MM`、読出し不能なら`--:--` |

数値はStage 1の最初の候補であり、実機で48 pixelの押し分けとBrowserのアドレス幅を
確認してから確定する。ただし次の条件は変えない。

- bar高はBrowserで実機比較済みの48 pixelを出発点とする
- system slotのhit targetは見えている絵だけでなくbar高全体を使う
- app areaとsystem slotは重ならない
- Browserの編集可能なアドレス本文は、全消去領域を除いて最低64半角cellを残す
- system slotを非表示にするときもapp areaの右端を動かさず、画面ごとの横揺れを作らない

BrowserではApp領域を、戻る52、進む52、再読込／中止52、12 pixelの間、南京錠24、
8 pixelの間、残りをアドレス欄として使う。候補値ではアドレス欄が約796 pixel残り、
全消去buttonと内側余白を除いても90半角cell前後を表示できる。現在より短くなるが、
既存の編集欄はcaretが見える位置へ横方向に追従するため、URL長の上限は変えない。

### 2. system barはhardwareを直接所有しない

`src/app/system_bar.rs`を追加し、次だけを担当させる。

- system領域の背景、icon、時計文字の描画
- appが中央へ描くための`AppRect`の提供
- system領域のtouch／mouse hit test
- 前回表示したsnapshotとの差分からdirty slotを求めること
- launcher、Wi-Fi、batteryの`SystemAction`を返すこと

hardwareとpolicyは既存の所有者へ残す。概念上の境界は次の形とする。実装時にはborrowと
code sizeに合わせて名前を変えてよいが、描画部品がI2C、C6、VFSを直接初期化しない契約は
固定する。

```rust
struct SystemSnapshot {
    wifi: WifiIndicator,
    battery: BatteryIndicator,
    clock: ClockIndicator,
    volume: Option<VolumeIndicator>,
}

enum SystemAction {
    Launcher,
    Wifi,
    Battery,
}

struct SystemBar {
    painted: Option<SystemSnapshot>,
}
```

active appは初回描画時にbar背景と自分のapp areaを描く。状態更新時は`SystemBar`が変更された
system slotだけを描き、active appの中央を塗り直さない。逆にBrowserがaddressを変えても
時計やbatteryを塗り直さない。

### 3. taskを作らず、前景区分だけを切り替える

Stage 3後の`src/app.rs`はhardwareと共有resourceを一度だけ初期化し、その内側で現在routeを
1つだけ回す。

```text
StartupScreen
  └─ Browser
       ↓
coordinator loop
  ├─ ConsoleHost.run() ───────── FrontRoute ─┐
  ├─ NormalGuiHost(Browser).run() ─ FrontRoute ─┼─→ 次の1前景画面
  └─ ExclusiveGui(id).run() ──── FrontRoute ─┘

NormalGuiHost(Browser)
  └─ Launcher choice
       ├─ Mini(Network／Battery) ─→ 同期実行してBrowserへ戻る
       ├─ Normal(other) ─────────→ FrontRouteをcoordinatorへ返す
       ├─ Console ───────────────→ FrontRouteをcoordinatorへ返す
       └─ Cancel ────────────────→ Browserを同じ状態で再描画
```

`FrontRoute`は`Console`、`Normal(NormalAppId)`、`Exclusive(ExclusiveAppId)`を持つ。
Wi-Fi設定、battery詳細、将来のvolumeは`FrontRoute`へ入れず、`MiniId`として
`NormalGuiHost`の内側だけで扱う。
rebootとshutdownは画面routeではなく、現在どおり明示的な終端操作として別に扱う。

別の`FrontRoute`へ移ると現在のhostまたは専有GUIアプリのframe loopはreturnする。
network socket、VFS file handle、
pointerの退避画素など、その画面だけのresourceをreturn前に閉じる。アプリの処理を
backgroundで継続しない。`wifi_manager`、`InputManager`、VFS、automountのような
現在も画面間で共有している管理器だけをcoordinatorが保持する。

ミニアプリへ入る場合はBrowserのframe loopから同期呼出しし、Browser固有resourceを閉じない。
ただしBrowser loopはミニアプリ中にpollされず、ミニアプリが明示的に借りた共有管理器だけが
進む。Consoleのセル内容を保持して再描画することは許すが、Console loopを裏で回すことではない。
初版のBrowserは別の`FrontRoute`への変更でpage、scroll、履歴を破棄する。将来状態を保持する場合も、
実行中taskではなく停止した値として明示的に設計する。

### 4. ランチャーはoverlay windowにしない

初版ランチャーはbar直下のcontent全体を一時的に使うmodalな一覧とする。popupを下地の上へ重ねると、
閉じるために任意のappを矩形clip付きで再描画するか、下地bitmapを退避する必要がある。
どちらも単一framebufferで最初に導入する共通機能としては費用が大きい。

初版の項目は次とする。

- Browser（通常GUI。初版では選択中なのでそのまま閉じる）
- Network settings（ミニアプリ）
- Battery details（ミニアプリ）
- Volume（driver追加後だけ出すミニアプリ）
- Console（前景区分の切替）
- Cancel

専有GUIアプリは引き続きConsoleの入口commandから開く。項目はkeyboard、touch、USB mouseで選べる。
launcher自体も同じsystem barを表示し、左端は選択中として描く。launcherからlauncherを
再度開くactionは無視する。

launcherは選択した対象を自分で起動せず、`LaunchChoice::Mini(MiniId)`、
`LaunchChoice::Front(FrontRoute)`、`Cancelled`のいずれかを通常GUI hostへ返す。
CancelならhostがBrowserを全面再描画して同じframe loopを続けるため、page、scroll、履歴を
失わない。Miniならhostが同じ深さで同期実行し、FrontならBrowserが固有resourceを閉じて
coordinatorへreturnする。launcher内からappや別launcherを直接callしない。

### 5. indicatorは実際に分かる状態だけを表示する

#### Wi-Fi

現在のBrowser indicatorと同じく、RSSIの強さではなく接続の進行段階を示す。

| 状態 | 表示 |
| --- | --- |
| OFF／C6なし | 輪郭または0本 |
| association中／再接続待ち | 1本 |
| associated、leaseなし | 2本 |
| Online | 3本 |
| 認証停止／失敗 | 赤いmarkerを加える |

状態は`wifi_manager::Manager`からsnapshotへ写す。barからC6を直接pollしない。tapは
Network settingsミニアプリを開く。Browserで取得中の場合は、現在の`open_wifi_menu`と同じくsocketを
安全に返し、必要ならWi-Fi再接続待ちへ移してから画面を渡す。

#### Battery

`src/app/battery.rs`に閉じているINA226の初期化と約1秒周期のsample保持を、共有の
`BatteryMonitor`へ分ける。barとBattery detailsミニアプリは同じsampleを読む。二重にINA226を
初期化したり、別々の周期で同じI2C busを読む実装にしない。

iconは現在と同じ6.00 Vを空、8.23 Vを満として4段階程度に丸める。この値は正確なSoCでは
ないので、barに百分率や残り時間を出さない。初期化前／読出し失敗は空のoutlineと`?`等で
「不明」を示し、0%や満充電を捏造しない。tapで詳細画面を開き、電圧、電流、電力、
シャント電圧の現在値を表示する。

#### Clock

RTCの値を直接JSTだと解釈せず、`wall_clock::local_now`の既存規則で`HH:MM`にする。
RTCが`VLF`、`STOP`、不正BCD、I2C errorを返した場合は`--:--`とする。代替時刻を作らない。
1秒程度で状態を再確認しても、描画とwritebackは表示する分が変わったときだけ行う。

#### Volume

現在は音声出力driverもvolume stateも無い。Stage 1では48 pixelを予約するだけで、icon、
数値、tap actionを出さない。driver追加時に`Option<VolumeIndicator>`が`Some`になった場合だけ
表示する。system bar導入のために音声driverを同時実装しない。

### 6. 入力は最初に押した領域がgestureを最後まで所有する

通常GUIアプリとミニアプリでは`y < SYSTEM_BAR_HEIGHT`をsystem／app bar、
`y >= SYSTEM_BAR_HEIGHT`をcontentとする。
touchの`Pressed`がbar内なら、その接触が`Released`になるまでbarが所有し、contentへ移動しても
appへ渡さない。逆にcontentで始まったdragがbarへ入ってもlauncherやindicatorを発火しない。
離した位置が最初のhit target内にある場合だけactionを確定し、drag-outをcancelとして扱う。

同じ境界をUSB mouseのclickにも使う。Browserの戻る、address、Wi-Fi等の判定を別々の
座標式で持たず、system hit testを先、app areaのhit testを後にする。bar以外のpointer描画順は
現在の`pointer.rs`の「cursorを外す→下を描く→cursorを載せる→和集合をflush」を維持する。

keyboardの既存割当ては変えない。初版のlauncherには矢印、Page Up／Down、Enter、Escapeを
与える。全画面共通shortcutはCardKBにCtrlが無いこととBrowserの既存キーとの衝突を解決する
まで追加せず、launcherへの確実な入口はbar左端のtap／clickとする。

### 7. full-width barを毎回flushしない

CW回転ではlogical x幅がwritebackするnative連続範囲の大きさを決める。高さ48 pixelでも
`flush_rect(0, 0, WIDTH, 48)`を時計更新のたびに行えば、全幅分の高いcostを払う。

- 初回表示または画面遷移時だけbar全体を描く
- Wi-Fi、battery、clockは各slotだけを消して描き直す
- clockは分が変わらなければdirtyにしない
- app areaのdirtyとsystem slotのdirtyを別bitで持つ
- 複数の隣接slotが同frameで変わった場合だけ、矩形を結合した方が安いか実測する
- pointerがslot上にいる場合は、既存spriteの退避・復元順を守ってからslotを更新する

Stage 2のhost testではsnapshot差分が正しいslotだけをdirtyにすることを検査する。実機では
各indicatorを変化させ、LCD underrun countと最長writeback時間をUARTへ記録する。

### 8. 現在の画面を4区分へ割り当てる

| 画面／処理 | 区分 | 理由 |
| --- | --- | --- |
| Browser | 通常GUIアプリ | 現在のtoolbar 48 pixelを統合し、page状態を保ってミニアプリと往復する |
| Wi-Fi menu | ミニアプリ | Network settingsとしてBrowserのWi-Fi状態と協調する |
| Battery monitor UI | ミニアプリ | 共有sampleの詳細を一時表示する |
| 将来のVolume UI | ミニアプリ | driverが存在するときだけlauncherとindicatorから開く |
| Paint | 専有GUIアプリ | 上端を含むcanvasを維持する |
| Touch test | 専有GUIアプリ | 画面全体のtouch座標を診断する |
| Coordinate test | 専有GUIアプリ | 全pixelが測定対象で、上書きを許さない |
| Font test | 専有GUIアプリ | 1画面固定の収録・配置診断を維持する |
| Axis test | 専有GUIアプリ | sensor／表示更新の診断条件を変えない |
| `win` | 専有GUIアプリ | 独自taskbarを含むpointer診断で、system UIの前例にしない |
| Display系soak／chart | 専有GUIアプリ | 全画面転送量と色を測る試験を変えない |
| `ls`、`cat`、`mount`等 | コンソールアプリ | Consoleのcell gridだけで完結する |
| Console | コンソールhost | アプリそのものではなく、コンソールアプリとGUIへの入口commandを実行する |
| Launcher | system component | 通常GUI hostが所有し、選択対象の区分を決める |
| Startup screen | system screen | アプリ分類外。初期化状態を専有表示する |

専有GUIアプリとConsoleの表示中はsystem barを描かず、indicatorの画面更新もしない。ただし
C6受信やUSB HID Splitのように停止すると低層状態を壊すserviceは、現在必要としている範囲で
継続する。次に通常GUIアプリへ入った最初のframeでsnapshotを読み直し、bar全体を描く。

## 画面別の移行

### Consoleとコンソールアプリ

`Console`の`TOP=8`、列数156、行数44を変えない。system barのgeometryをConsoleへ持ち込まず、
scroll、cursor、行編集、automount通知の退避・復元も現在のcell gridだけで行う。

Console loopは`ConsoleHost`として1つの`FrontRoute`を実行するが、system actionをpollしない。
コンソールアプリは従来どおり同期実行してConsoleへ結果とpromptを返す。通常GUIまたは
専有GUIへの入口commandだけが`FrontRoute`を返し、その画面からConsoleへ戻ったときは
system barなしでConsole全体を再描画する。

### Browser

`TOOLBAR_HEIGHT=48`、`VIEWPORT_TOP=56`、`VIEWPORT_BOTTOM=688`、status 32 pixelは維持する。
変えるのはtoolbar内部の横配置と所有者だけで、縦の本文行数を減らさない。

- 左端へlauncher slotを追加する
- 戻る／進む／再読込をapp area左へ移す
- 南京錠とaddressをその右へ置く
- Browser固有のWi-Fi描画と`WIFI_LEFT`判定を削り、system slotへ一本化する
- address editingの全消去、caret追従、security文言、各keyの意味を維持する
- status行は選択link、error、loading、security説明のためにBrowserが専有する
- toolbar dirtyをapp部分とsystem slotへ分け、page navigationで時計まで描き直さない

Network settingsミニアプリから戻ったときにBrowserのpage状態を保つ現在の動作を維持する。
launcherで別の`FrontRoute`を選んだ場合はBrowserを終了し、socketとVFS handleを必ず返す。

### Network settingsミニアプリ

現在のheader 78 pixelのうち上48 pixelをsystem barに置き換える。残る30 pixelと
`y=78..94`を列見出し／余白に使い、`LIST_TOP=94`、row高32、footer位置は初版で維持する。
タイトルまたは現在状態はapp areaへ置き、右のWi-Fi slotは選択中の詳細先として描く。
自分自身を開くtapは無視し、ON/OFFや再scanは画面内の既存操作だけで行う。

### Battery detailsミニアプリ

共有`BatteryMonitor`のsampleを表示し、自分で別のINA226 instanceを初期化しない。
contentの幾何学中心は`y=48..720`の中央から求め、固定値を単純に48 pixel足して下端を
はみ出させない。barのbattery slotを押しても自分自身を再度開かない。

## Stage詳細

### Stage 0: 現状確認と計画

- `DESIGN.md`から関連する現状文書を選ぶ
- Browser、Wi-Fi menu、Console、battery、診断画面の座標と所有resourceを確認する
- 音量driverが未実装であることを範囲へ反映する
- 通常GUI、ミニ、専有GUI、コンソールの4区分と協調可否を固定する
- 1段共有、48 pixel、非task routeを固定する
- 本書を`DESIGN.md`の作業計画リストへ登録する

### Stage 1: アプリ区分と純粋なsystem bar部品

- `AppClass`または同等の列挙で4区分をコード上に表す
- system barを使えるのは通常GUIとミニだけ、という判定を1か所に置く
- `src/app/system_bar.rs`へgeometry、snapshot、dirty、system hit testを実装する
- system slotとapp areaの境界を1か所の定数から導く
- launcher、Wi-Fi、battery、clockの静止描画を作る
- volume reserveは背景だけで、actionを返さない
- appが中央へ描くための矩形とclip条件を提供する
- 境界pixel、押下開始／drag-out／releaseのhost testを追加する
- 全slotが1280 pixel内に収まり、app areaが0にならないcompile-time assertを置く
- 診断用featureで静止barを実機表示し、48 pixelの押し分けと文字のbaselineを確認する

### Stage 2: 共有indicator状態

- `wifi_manager::State`から`WifiIndicator`への副作用のない変換を追加する
- `wall_clock::local_now`から`ClockIndicator`を作り、不正RTCを`--:--`にする
- INA226の所有とsample保持を`BatteryMonitor`へ分ける
- battery全画面画面をまだ変更せず、同じ換算結果になるhost testを先に置く
- snapshot差分からWi-Fi／battery／clockの個別dirtyを作る
- 各pollにdeadlineを持たせ、1回のI2C失敗でframe loopを終了しない
- indicator serviceは1 frameに複数のboard I2C transaction群を重ねず、周期を分散する
- clockの秒が変わっても`HH:MM`が同じなら描画しないことをtestする

### Stage 3: 通常GUI host、coordinator、ランチャー

- 初回route後の`src/app.rs`を`FrontRoute`のcoordinator loopへ分ける
- `NormalGuiHost`、`MiniId`、`MiniOutcome`と、各区分が借りる共有contextを定義する
- appからappを直接起動する経路を増やさない
- launcherを通常GUI host内のmodal system componentとして追加する
- Network、Battery、条件付きVolume、Console、Cancelを区分付きで返す
- CancelではBrowserの状態を保って再描画し、`FrontRoute`選択時だけBrowserがreturnする
- Mini選択ではhostが同期実行し、mini自身は選択先を直接起動しない
- route終了時にBrowser socket、VFS handle、pointer下地が残らない検査点を置く
- reboot／shutdownとBrowser終了後Consoleを維持する
- Startup完了後はOnlineでなくてもBrowserへ進み、ミニアプリを直接開かない

### Stage 4: Browserを通常GUIアプリへ移行

- Browser toolbarをsystem barのapp areaへ移す
- 既存Wi-Fi icon、描画、hit testをsystem側へ一本化する
- system/app dirtyを分離する
- Browserの状態を保持したままlauncherとミニアプリを同期実行するhost loopを追加する
- URL本文64 cell以上、全消去、caret、3button、南京錠の非重なりassertを置く
- touch／mouseでlauncherとBrowser buttonを境界まで押し分ける
- Network settings往復、取得中cancel、再接続待ち、local file handleのcleanupを回帰する
- 48 pixel toolbar＋8 pixel gap＋32 pixel statusの縦配置が変わらないことを確認する

### Stage 5: ミニアプリ

- Wi-Fi menuをNetwork settingsミニアプリへ変え、headerをbar＋既存listへ分ける
- Wi-Fi状態変化をbarと一覧へ同じmanagerから反映する
- battery UIをBattery detailsミニアプリへ変え、共有monitorから表示する
- ミニアプリ終了で`Dismissed`、別ミニ選択で`Open(MiniId)`を返す
- Wi-Fi／batteryの自分自身を開くsystem actionを無視する
- 引数なし`wifi`はsystem barを案内するConsole応答へ変更する
- `battery`／`batinfo`は共有sampleを1回Consoleへ出し、継続UIは開かない
- StartupからWi-Fi menuを直接開くrouteを削り、offlineでもBrowserへ進める
- Browserと各ミニアプリを往復し、page、scroll、history、共有resourceが継続することを確認する

### Stage 6: 専有GUIアプリとコンソールアプリ

- 各画面／commandの区分をコード上で列挙し、暗黙の「全画面だからbarなし」を残さない
- Startup、paint、touchtest、coordtest、fonttest、axistest、`win`、display診断を
  system screenまたは専有GUIとして確認する
- Consoleの`TOP=8`、156列×44行、scroll矩形を変更しない
- コンソールアプリ実行中にbar input、indicator paint、ミニアプリ入口が無いことを確認する
- GUIへの入口commandと、Console内で完結するcommandを区別する
- 専有GUI中にindicator矩形が書き戻されないようserviceとpaintを分ける
- 専有GUI終了後にConsoleへ戻る場合はbarを描かない
- `win`の下部taskbarとsystem barが同時に出ないことを確認する
- `run_visual_qa`の専有画面へbarが混入しないことを確認する

### Stage 7: 入力と画面遷移の回帰

- 通常GUI、launcher、ミニアプリのbar gesture所有を回帰する
- mini→miniの入替えでcall stackが深くならないことをtestする
- launcher Cancel、mini Dismiss、Normal／Console切替を区別する
- 専有GUIとコンソールでは同じ座標のtapがsystem actionにならないことを確認する
- Console→Browser→mini→Browser→Consoleを一巡し、各resourceの所有を確認する
- 次の通常GUI開始時にbar全体を最新snapshotで描き直す
- keyboard、touch、mouse抜去時に押下中gestureとpointer stateをcancelする

### Stage 8: 総合受入と文書同期

- host test、release build、ELF layout、image検査、既存display soakを実行する
- 下記の実機受入matrixを完走する
- [`APPS.md`](APPS.md)へ4区分、system bar、mini協調、routeの現状を記録する
- [`BROWSER.md`](BROWSER.md)へ統合後の横配置とsystem／app所有境界を記録する
- [`CONSOLE_SHELL.md`](CONSOLE_SHELL.md)へ44行維持、bar非表示、`wifi`／`battery`の
  Console上の挙動を記録する
- [`INPUT.md`](INPUT.md)へbar gestureの所有規則を記録する
- [`RTC.md`](RTC.md)へsystem clockの表示元とinvalid表示を記録する
- [`WIFI.md`](WIFI.md)へNetwork settingsの入口と通常GUI hostとの協調を記録する
- [`FILE_LAYOUT.md`](FILE_LAYOUT.md)へ`system_bar.rs`、launcher、battery monitor、
  coordinatorの責務を記録する
- `DESIGN.md`の起動分岐とsystem barの現状説明を実装へ同期する
- batteryの現状説明が変わるため[`APPS.md`](APPS.md)の該当節を同じ作業で更新する
- 実装と本書のStage状態、判断記録を同期する
- READMEがこの作業で変更されていないことを確認する

## 実機受入matrix

| 場面 | 操作／条件 | 期待結果 |
| --- | --- | --- |
| Console | 起動、長い出力、最下行scroll | system barが無く、156列×44行を維持する |
| Console | 入力途中にUSB automount通知 | 入力行とcursorが従来位置へ復元される |
| Console | 引数なし`wifi` | GUIを開かず、system barから開く案内を出す |
| Console | `battery`／`batinfo` | 共有sampleを1回文字で出し、継続UIを開かない |
| Browser | 戻る／進む／再読込／南京錠／address | launcherとsystem slotを誤発火しない |
| Browser | 長いURLを編集 | 64 cell以上見え、caretへ追従し、URL上限は変わらない |
| Browser | Wi-Fi slotをtap | Network settings往復後に同じpageを再描画し、通信を安全に再開する |
| Browser | launcherを開いてCancel | 同じpage、scroll、履歴へ戻る |
| Browser | launcherからBatteryへ | Battery detailsを閉じると同じpage、scroll、履歴へ戻る |
| Browser | launcherからConsoleへ | Browserを終了し、socketとfile handleを保持せずConsoleへ移る |
| Mini | NetworkからBatteryへ切替 | 一度hostへ戻して同じ深さで入れ替え、Browser状態を保つ |
| Wi-Fi | OFF→ON→association→DHCP | 同じslotが0→1→2→3段階へ進む |
| Wi-Fi | 認証停止／C6 link loss | error markerまたは再接続段階になり、古いOnlineを残さない |
| Battery | INA226あり | 約1秒周期のsampleでicon段階と詳細値が同じになる |
| Battery | INA226なし／一時I2C失敗 | 不明表示になり、0%を捏造せず、他入力が止まらない |
| Clock | 正常RTCで分境界を通過 | 該当slotだけが1回更新される |
| Clock | VLF／STOP／不正値 | `--:--`を表示し、代替時刻を作らない |
| Touch | barからcontentへdrag | content操作は発火せず、drag-outならbar actionもcancelする |
| Touch | contentからbarへdrag | launcher／indicatorを発火しない |
| Mouse | indicator上で抜去 | 押下をcancelし、古いpointer下地を書き戻さない |
| 専有GUI | coordtest | chartの全pixelにbarが重ならず、ミニアプリを開けない |
| 専有GUI | `win` | 下部の診断taskbarだけが表示される |
| Startup | Wi-Fi未設定／接続失敗 | ミニアプリを直接開かずBrowserへ進む |
| 復帰 | 専有GUIからConsoleへ | barを描かず44行Consoleへ戻る |
| 復帰 | ConsoleからBrowserへ | 最新snapshotのbarを欠けなく描く |

各caseでDMA error 0、意図しないLCD underrun増加なし、秘密情報のUART出力なしを確認する。
時計、battery、Wi-Fiの個別更新で全幅barをflushしていないことも、instrumentした矩形と時間で
確認する。

## 失敗時の扱い

- system bar初回draw／flush失敗: UARTへ記録し、その画面の開始を中止してConsoleへ戻す。
  部分的に古いbarを操作可能として残さない
- RTC read失敗: `--:--`。起動やapp操作を止めない
- INA226初期化／read失敗: battery不明。直前値を永久に正常値として残さず、詳細画面で
  errorを説明する
- Wi-Fi管理器未初期化／OFF: 0段階またはOFF。Onlineを推測しない
- launcher描画失敗: `Cancelled`として呼出元appを再描画する。別appを半端に開始しない
- route先の初期化失敗: resourceを返してConsoleへ遷移し、UARTへroute名と理由を残す
- system action処理中の入力device抜去: 押下をcancelし、再接続後のreleaseを前のgestureへ
  結び付けない
- bar更新がframe budgetを超える: 更新頻度を落とす。表示DMAの優先度やPSRAM配置を
  system UIのために変えない

## 未決事項と実機で決める条件

次は計画時点で数値を断定せず、Stageごとの実機比較で決める。

1. launcherの52 pixel幅で、左端とBrowserの戻るを指で押し分けられるか
2. Wi-Fi／battery各48 pixelと時計80 pixelが、bitmap fontとiconの見た目に足りるか
3. 音量reserveの空白が不自然なら、slot境界を描かず単なる余白として見せるか
4. batteryとRTCのI2C pollを同じ秒へ置くか、500 msずらす方がframe停止を減らすか
5. Browserの約90 cellのaddress表示が実用上足りるか。64 cell未満にはしない
6. 複数system slotが同時に変わったとき、別々のflushと結合flushのどちらが速いか

どの比較でも、押しやすさのために2本目のbandを足す選択はしない。必要なら横幅、icon、
文言、更新頻度を調整する。

## 判断記録

### 2026-08-31: plan作成時

- 上端へsystem barを追加するのではなく、Browserが既に使う48 pixelのtoolbarを
  system／app共有の1本へ置き換える
- 下部はBrowserのstatus行などapp固有の文脈へ残し、共通indicatorを上下へ分散しない
- Windows CEのtaskbarを再現せず、Command Barとsystem statusを1段へまとめた構造とする
- 通常GUI、ミニ、専有GUI、コンソールの4区分を、barの有無と協調可否の契約にする
- Consoleはsystem bar対象にせず、上端8 pixelと156列×44行を維持する
- task、window manager、background appを導入せず、coordinatorが1つの前景routeだけを実行する
- launcherは初版でoverlay popupにせず、bar下のcontentを一時的に使うmodal画面にする。
  Cancelなら通常GUIアプリを状態ごと継続し、ミニアプリ選択なら同期実行する
- Network settings、Battery details、将来のVolumeをミニアプリとし、Browserの状態を保って往復する
- 専有GUIアプリとコンソールアプリはsystem barもミニアプリ入口も持たない
- Startupは区分外のsystem screenとし、完了後はofflineでもBrowserへ進む
- 音量状態は未実装なので表示を捏造せず、将来の横幅だけを予約する
- indicator更新はslot単位のdirty／flushとし、48 pixel高を理由に全幅更新しない
