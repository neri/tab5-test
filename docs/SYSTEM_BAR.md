# 上部統合システムバー

> 索引: [`../DESIGN.md`](../DESIGN.md) ／ 計画・実機受入項目: [`SYSTEM_BAR_PLAN.md`](plans/active/SYSTEM_BAR_PLAN.md)

通常buildは上部48 pixelをsystem操作とBrowser操作で共有する。実装済みだが、
この統合経路の実機動作、押し分け、I2C／SDIOの最長停止時間、表示帯域は未確認。
旧UIと`legacy-ui` featureは削除した。`--features system-bar-static`は
起動後に固定snapshotのbarを表示し、キーでConsoleへ戻る診断用build。

## 区分と所有者

| 区分 | 対象 | barとミニアプリ |
| --- | --- | --- |
| 通常GUI | デスクトップ、Browser | barあり。Browserの状態を停止してミニへ渡す |
| ミニ | Network settings、Battery details | barあり。独自の入力待ちloopは持たない |
| 専有GUI | paint、touchtest、coordtest、fonttest、axistest、display診断 | barなし。終了後はデスクトップ |
| コンソール | ls、cat、mount等、wifiサブコマンド | barなし。156列×44行、TOP=8 |

Startupは分類外の初期化画面。完了時にWi-FiがOnlineかつIPv4取得済みならBrowser、それ以外はデスクトップへ進む。
USB初期化待ちの間に切断する可能性があるため、以前の成功判定ではなく完了時の状態を見る。
起動中の明示EscapeはConsoleへ進む。`app.rs`のcoordinatorが
`FrontRoute::Console`／`Desktop`／`Normal(start URL)`／`Exclusive(id)`／`Power(action)`の1つを実行する。
ミニはrouteに含めず、`system_bar`内の平坦な`Screen`値を入れ替える。
通常GUI host終了時にはBrowserのnetwork socketとlocal file handleを閉じる。
Browserの終了操作は同じhost内のデスクトップへ切り替え、本文と履歴は再選択まで保持する。

## 共有する一段の配置

| 領域 | x範囲（右端を含まない） | 高さ | 動作 |
| --- | --- | --- | --- |
| Launcher | 0..52 | 48 | ランチャー |
| App | 52..1056 | 48 | Browser操作、またはミニの戻る／タイトル |
| Wi-Fi | 1056..1104 | 48 | Network settings |
| Battery | 1104..1152 | 48 | Battery details |
| 音量予約 | 1152..1200 | 48 | 背景のみ。icon／actionなし |
| Clock | 1200..1280 | 48 | HH:MM、操作なし |

bar全体の背景はbutton faceのグレー（RGB565 `0xC618`）で統一する。
基本色は[`GUI_THEME.md`](GUI_THEME.md)の`theme.rs`を参照する。Launcherの選択行は青地に白文字、
Network／Batteryの`< Back`は太字とし、タイトルは通常の太さにする。titleとmenu labelは
A4比例幅Sans、ClockはA4 Sans Monoで実際のpixel幅を80 pixel slot内へ中央揃えする。
状態iconの`!`／`?`はLatin A4、日本語は16 pixelのNoto CJK A4 strikeへfallbackする。

Browser本文の開始y=56、下端y=688、status高32は維持する。
app領域の横配置は[`BROWSER.md`](BROWSER.md)。寸法とbar targetは`system-ui/`の
共通定数から導き、アドレス本文64 cell以上と1280 pixel内への収容をcompile-time assertする。

Wi-Fiは電波強度ではなく接続段階を示す。OFF／リンクなしは0本、Idle／association／retryは
1本、association済みでleaseなしは2本、アドレスを持つOnlineは3本。
`Failed`／`NeedsPassword`は赤い`!`を出す。
Batteryは6.00 V〜8.23 Vの電圧目安を0〜4段階へ丸め、百分率や残り時間をbarへ出さない。
未測定／測定失敗は`?`。詳細画面とConsoleは同じ`BatteryMonitor`の値を参照する。
時計はRTCのVLF／STOPを検査してから`wall_clock::local_now`のJSTを使い、失敗時は`--:--`。

## イベントとフォーカス

`system_bar::run`がUSB／Wi-Fi保守、入力、画面timer、描画とpointerを所有する。
非frame wakeでは`InputManager::service_fast`とWi-Fiのtransport保守を行う。
frameではinput service、Wi-Fi policy、GUI用silent automount、indicator更新、キーとpointer、
active screenのtimer、描画を進める。画面を停止しても共有管理器の保守は続く。
Browserのband描画中はtransportだけを保守し、画面遷移を再入させない。

キーは1 frame最大16件。既存のInputManagerの16件queueは満杯時に新しいキーを捨てる。
遷移要求が出た時点でそのbatchへの配信を終了し、取得済みキーqueueを破棄する。
USBキーボードの押下履歴は残して同じheld keyを新しい押下として再生しない。
Wi-Fiの画面向け完了通知は1件で、要求tokenが合う画面だけが消費する。
共有の接続回復処理は画面向け通知を生成しない。

- `M`（文字編集中以外）または`F3`でLauncher。CardKBは`M`を使う。
- Launcherの項目は左端8 pixel、bar直下8 pixelから幅400 pixelで左上に配置する。
  行の高さは48 pixel、行間は8 pixel。項目の外側や行間のtap／clickでは選択しない。
- Launcherは矢印／Page Up・Down、Enter、Escape、行のtap／mouse clickで操作する。
- Browser、Console、Desktop、Power...の4項目。Power...は最下段。
  Network settingsとBattery detailsはそれぞれbarのWi-Fi／Battery slotから開く。
- Power...は同じ左上位置のサブメニュー（Reboot、Shutdown、Back）を開く。
  Back／Escapeで親のPower...選択へ戻る。ハンバーガー再クリックはメニュー全体を閉じて呼出元へ戻る。
- Launcher表示中にハンバーガーアイコンを再クリック／tapすると呼出元へ戻る。
- Escapeは呼出元へ戻る。Browser項目はBrowserへ戻る。ミニ同士の選択は同じ深さで入れ替える。
- ミニのEscapeまたは中央の戻る領域は呼出元のデスクトップ／Browserへ戻る。同じindicatorを押しても再入しない。
- Launcher／ミニ中のキーをBrowserへ二重配信しない。BrowserのURL編集状態は保持する。

barのgestureは押下開始位置が所有し、release時に確定する。targetから一度でも出ると、
元へ戻っても取消。contentからbarへ移ってもsystem actionは発火しない。
touchのtap／drag分類は[`INPUT.md`](INPUT.md)を正本とする。Browser本文から始まったdragは
指の移動24 pixelごとに1行scrollへ変換し、接触終了までlinkを発火しない。
touchをmouseより優先し、touch中のmouse actionは捨てる。wheelはBrowser content上だけで処理する。
遷移時は全接触が離れるまで新しいtouchを受け付けず、USB topology変化時も押下を取り消す。マウスがなくtouchも離れていればcursorを消す。

## タイマー、描画、資源

`Timer`は未消費を最大1件とし、handlerから戻った後にconsumeする。停止中は配信しない。
復帰時、定時型は元の周期の次の期限へ飛ばし、非定時型は復帰時から通常間隔で再開する。
値を破棄すれば保留も消える。Browserは17 msの非定時timerを使い、ミニ中はpollしない。
通信GETはミニ／Launcherへ入る前にsocketを閉じ、戻ったとき再取得する。local readは停止した
handleとして保持し、復帰時のVFS検査へ任せる。通信の実時間deadlineを画面停止時間で延ばさない。

INA226は初期化と測定を別回のpollで行い、約1秒ごとにsampleを更新する。初期化失敗は5秒後に
再試行し、読み出し失敗は直前値を正常表示として保持しない。時計の初回を500 msずらし、
同一pollでbattery測定群とRTC読出し群を重ねない。これはtouch等を含めたI2C全体の排他schedulerではない。

system slotはsnapshot差分が出たものだけを描画／flushする。秒だけの変化ではclockを描かない。
Browserのaddress更新はapp領域だけで、system slotを含まない。遷移時は全slotを無効化して
描き直し、Browserのcontent全面を再描画する。cursorを外す→dirty描画→cursorを載せる→
旧新cursor矩形の和集合flushの順序を保つ。画面bitmapの退避は行わない。
bar／Browserのflush失敗はConsoleへのreturnとUARTログにし、古い表示のまま操作を継続しない。

`SYSTEM BAR: max service/handler ms=`はserviceと入力・画面処理の最長値、
`BROWSER: slowest viewport repaint so far, ms=`は本文再描画の最長値を更新時だけ記録する。
各indicatorの`SYSTEM BAR: ... slot max draw/flush us=`は描画とslot書き戻しの最大時間を
360 MHzのcycle counterで測る。handlerの実機許容時間、slot flushの帯域とunderrun増分は受入時に判断する。
Wi-Fi操作の段階実行と保存規則は[`WIFI.md`](WIFI.md)を参照。

## デスクトップ

`desktop.rs`はbarより下をテーマのデスクトップ背景（ティール、RGB565 `0x0410`）で塗るだけで、独自の入力処理はない。
`win`コマンド実行時と専有GUI終了後は`FrontRoute::Desktop`、Browserの終了操作は`Screen::Desktop`へ進む。
Consoleへの明示遷移と描画失敗時の退避は引き続きConsoleへ戻る。
Wi-Fi OFF、保存接続先なし、接続失敗、DHCP失敗時の起動フォールバック先もデスクトップとする。

## 電源メニュー

Power...を開くだけでは電源操作を実行しない。Reboot／Shutdownの選択確定で実行し、
追加の確認画面は設けない。項目の配色・入力方法・左上配置は親Launcherと共通。
遷移時に取得済みキーとtouchの押下を破棄するため、親項目を開いた入力を子項目へ再配信しない。

電源要求は`LaunchChoice::Power`から`FrontRoute::Power`へ渡す。通常GUI hostはcursorを消し、
Browserのsocket／file handleを閉じてからcoordinatorへ戻る。Networkのpassword bufferも破棄される。
coordinatorは受付メッセージをConsoleへ描画し、300 ms待って既存の`shell::reboot`／`shutdown`を呼ぶ。
再起動前はManagerの保持する認証情報を消去する。機器側の処理は
[`CONSOLE_SHELL.md`](CONSOLE_SHELL.md)の再起動・全体電源断と共通。

Shutdownが戻った場合は電源断完了とみなさず、I2C失敗なら
`shutdown request failed; device is still running`、パルス送信済みなら
`shutdown pulses sent; device is still running`を表示する。キー入力後にConsoleへ戻る。
GUIからの再起動・電源断は実機未確認。
