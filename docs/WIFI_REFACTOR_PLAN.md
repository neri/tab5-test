# Wi-Fi接続管理リファクタリング計画

> 索引: [`../DESIGN.md`](../DESIGN.md)
> この文書は作業計画と実機での判断記録です。現在の実装仕様は現状文書と
> コードを優先してください。

## 状態: 最小メニュー（Stage 0〜2）完了

| Stage | 内容 | 状態 |
| --- | --- | --- |
| 0 | 最小メニューの境界固定と既存動作のbaseline | 完了（実機baselineはStage 2へ統合） |
| 1 | キーボード操作の最小メニュー、接続、メニュー専用DHCP | 完了 |
| 2 | 最小メニューの実機受入とCLI回帰確認 | 完了 |
| 3 | Wi-FiセッションとIPスタックを接続管理器へ集約 | 未着手 |
| 4 | 初回接続失敗とreason 4の自動リトライ | 未着手 |
| 5 | 接続後の切断検出と自動再接続 | 未着手 |
| 6 | 永続化経路の調査、接続先保存、起動時自動接続 | 未着手 |
| 7 | Wi-Fi ON/OFF、保存情報の削除、タッチ操作、既存コマンド統合 | 未着手 |
| 8 | 長時間・異常系の実機受入と現状文書の更新 | 未着手 |

### Stage 1実装記録

2026-08-26に`src/app/wifi_menu.rs`を追加し、`wifi`コマンドから最小メニューを
開く経路を実装した。scan結果は受信順のまま15件ずつ表示し、上下キー、Enter、`R`、
Escapeで操作する。hidden SSIDは表示のみ、OPEN以外は最大64 byteのマスク入力とし、
入力bufferは接続・cancelの両経路でvolatile writeにより消去する。

接続要求の直前に古い`net::Stack`を破棄し、association成功後はメニュー経由に限って
STA MACから新しいstackを作成してDHCPを開始する。15秒でleaseを取れなくてもassociationと
DHCP clientは維持し、通常フレームループへ返す。画面でキーを待つ間もフレームごとに
`Stack::poll`、stackがなければ`Rpc::discard_station_frames`を呼ぶ。

静的確認では`cargo build --release`と`tools/check_elf_layout.py`が成功した。生成ELFは
IRAM 10,100 byte、DRAM rodata 1,364 byte、DROM 130,776 byte、IROM 752,832 byte、
stack 178,176 byteだった。workspace全体のstrict Clippyは既存コードの警告をerror扱いして
停止するため、本Stageの合否には用いていない。

2026-08-26に利用者が実機で最小メニューとCLI回帰を確認し、Stage 2の受入を完了した。
これにより今回の実装対象であるStage 0〜2を完了とする。自動リトライ、自動再接続、保存、
起動時接続、ON/OFF、タッチ操作は今回の残作業ではなく、Stage 3以降の将来対応として残す。

## 実装優先順位

最初にStage 0〜2だけを実装し、**AP一覧から選んで接続できる最小メニュー**を先に
実機へ載せる。この時点では全面的な接続管理リファクタリングを前提にせず、既存の
blockingな`scan`、`connect`、`wait_for_connection`、`Stack::start_dhcp`、
`pump_until`を再利用する。

最小メニューに含めるのは次だけである。

- `wifi`コマンドから全画面へ入り、開始時に1回スキャンする
- 上下キーとEnterでAPを選択する
- OPEN APはそのまま、暗号化APはマスク付きパスワード入力後に接続する
- メニュー経由の接続だけDHCPを開始し、結果を表示する
- Escapeでシェルへ戻り、得た`Rpc`と`Stack`を既存フレームループへ返す
- コマンドラインの`wificonnect`と`ipconfig dhcp`の従来動作を変えない

次は最小メニューの対象外とし、Stage 3以降へ送る。

- 常時動く接続状態機械と所有権の全面整理
- reason 4を含む自動リトライ、自動再接続、長時間切断診断
- タッチ／マウス操作、バックグラウンドスキャン、接続中のcancel
- AP／パスワード保存、起動時接続、Wi-Fi ON/OFF、forget
- 複数AP profile、hidden APの手入力

これにより、メニュー表示のためだけにFlash書き込みや全ネットワーク利用箇所の借用変更まで
先行させない。Stage 1のコードは後の接続管理器から呼び直せるよう、画面描画・入力と
「既存sessionへscan/connect/DHCPを依頼する小さい操作」を分けるが、先に完成形の抽象化を
作り込まない。

## 背景

現在はシェルで`wifiscan`、`wificonnect <ssid> [password]`、`ipconfig dhcp`を
順に実行する必要がある。`src/app.rs`は`Option<wifi::Rpc>`と`Option<net::Stack>`を
別々に保持し、接続後は毎フレーム通信をポーリングするが、次の処理は持たない。

- スキャン結果から接続先を選ぶ画面
- パスワードを画面へ露出させずに入力する経路
- メニューから接続した場合の、アソシエーション成功後のDHCP自動開始
- 接続要求の失敗、接続後の切断、C6リンク喪失に対する再試行方針
- 接続先とON/OFF設定の不揮発保存

切断イベント自体は既にESP-Hosted RPCから受信できるが、通常のフレームループでは
再接続に使っていない。ネットワークコマンド終了時の`drop_dead_session`はイベントを
表示するだけで、アドレスを使える状態へ戻さない。また、DHCPリースを失った場合は
`net::Stack`がアドレスを外すが、Wi-Fiの再アソシエーションとは連動していない。

既知のreason 4（`DISASSOC_DUE_TO_INACTIVITY`）は[`WIFI.md`](WIFI.md)に記録済みである。
HP CPUだけを再起動し、アソシエートしたままのC6を後からリセットすると、AP側に残った
古いstation状態の影響で再起動後の最初の接続だけreason 4になり、2回目は成功した。
現在は再起動時のdeauthと起動時のC6電源断で原因を減らしているが、APや電波状況によって
同じreasonが出る可能性は残るため、接続管理側でも回復できるようにする。

## 到達目標

- `wifi`コマンドで全画面のWi-Fiメニューを開く
- APをスキャンし、SSID、RSSI、チャンネル、認証方式を一覧表示する
- キーボードの上下キーまたはタッチでAPを選び、必要ならパスワードを入力する
- パスワードを画面では`*`に置き換え、UART、通常ログ、状態画面へ出さない
- メニューからの接続操作を「アソシエーション→DHCP→利用可能」の1つの流れとして扱う
- コマンドラインの`wificonnect`は従来どおりアソシエーションだけを行い、
  `ipconfig dhcp`を自動実行しない
- reason 4など回復可能な接続失敗を自動でリトライする
- 接続後に切断した場合、画面やシェルを固めず、自動で再接続し、DHCP方針が有効なら取り直す
- 最後に接続成功したAPを1件保存し、次回起動時に自動接続する
- Wi-Fi全体をON/OFFでき、OFF中は自動接続も自動再接続も行わない
- `wifiscan`、`wificonnect`、`wifistatus`、`wifidisconnect`など既存の診断手段を残す
- 切断がAP／無線、C6／SDIOリンク、DHCPのどの層で観測されたかを診断できる

初版で保存するプロファイルは**最後に接続成功した1件だけ**とする。複数APの優先順位、
企業向け802.1X、WPS、SoftAP、5 GHz、ソフトウェアキーボードは対象外である。

## 利用者から見える動作

### AP選択画面

完成形では`wifi`を実行すると現在状態とON/OFFを上部に、スキャン結果を下部に表示する。
同じSSIDを複数BSSIDが広告している場合、現行の接続RPCはSSIDだけを指定するため、
一覧もSSID単位にまとめ、最も強いRSSIと検出BSSID数を表示する。BSSIDを選べるように
見せながら実際には選べない画面にはしない。空SSIDのhidden APは`(hidden)`として表示するが、
初版では選択不可とする。

Stage 1の最小メニューでは、上下キー、Enter、`R`、Escapeだけを実装し、scanが返した順に
APを表示する。SSID単位の重複統合、Page Up／Down、タッチ、ON/OFF、forgetはStage 7で
追加する。完成形の操作は次とする。

- 上下キー、Page Up／Down、またはタッチ: 選択移動
- Enterまたは項目のタップ: 選択したAPへ進む
- `R`: 再スキャン
- `O`: Wi-Fi ON/OFF
- `F`: 保存済みAPを削除（確認画面を挟む）
- Escape: メニューを閉じる。接続管理は止めない

スキャンは数秒ブロックする既存RPCで始めてもよいが、開始前に画面を書き戻して
`Scanning...`を見せ、完了後に入力を再開する。接続中に再スキャンすると通信が一時停止する
可能性があるため、利用中は確認を求めるか、切断後に行う。

### パスワード入力

OPEN以外のAPでは最大64 byteの入力欄を開く。ASCIIの表示可能文字と空白、Backspace、
Enter、Escapeを受け付け、表示は入力byte数と同数の`*`だけにする。入力中の文字列を
コンソール履歴へ渡さず、ログへも渡さない。OPENのAPは入力画面を省略し、空パスワードで
接続する。

接続が認証失敗した場合は同じパスワードを無限に試さず、入力画面へ戻す。成功した資格情報は
アソシエーション成功時点で有効とみなす。メニュー接続でDHCPだけが失敗してもパスワードを
再入力させず、`Associated / DHCP retrying`として別の問題を表示する。この自動DHCPは
メニュー接続専用であり、シェルの`wificonnect`には適用しない。

### 状態表示

利用者向け状態は少なくとも次に分ける。

```text
Off
Idle (no saved network)
Starting C6 link
Associating (attempt N)
Associated / waiting for DHCP
Associated / IP unconfigured (run ipconfig dhcp)
Online (SSID, RSSI, IPv4)
Retry waiting (reason, next attempt)
Needs password (authentication failed)
Link failed (C6/SDIO recovery pending)
```

「接続済み」はアソシエーションだけを指す語として使わず、IPアドレス取得済みの`Online`と
区別する。Stage 1ではこのうちスキャン、入力、association、DHCP結果だけを画面ローカルの
状態として表示する。メニューを閉じても進むバックグラウンド状態機械はStage 3で導入する。

## 設計方針

### 所有権を接続管理器へまとめる

以下はStage 3以降の完成形であり、最小メニューの前提条件にはしない。Stage 1は現行の
`Option<wifi::Rpc>`と`Option<net::Stack>`を`wifi_menu::run`へ可変参照で渡し、終了時に
同じ所有者へ返す。メニューに必要な範囲だけ、既存シェル内のsession確立とStack生成処理を
副作用の小さい共通helperへ切り出す。

`src/app.rs`に並んでいる`Option<wifi::Rpc>`と`Option<net::Stack>`を、例えば
`wifi::Manager`（最終的な名前は実装時に決める）へまとめる。

```text
app frame loop / Wi-Fi menu / browser / shell
                    |
                    v
              Wi-Fi Manager
       policy + state + retry timer
          |                    |
     Option<Rpc>          Option<net::Stack>
          |                    |
      ESP32-C6             smoltcp/DHCP
```

管理器だけがセッション、IPスタック、現在プロファイル、ON/OFF、接続元、IP設定方針、
再試行回数と期限を変更する。
画面やシェルは`scan`、`connect`、`set_enabled`、`forget`、`status`のような意図を渡し、
内部の`Option`を直接捨てない。これにより「RPCだけ張り直して古いIPアドレスを残す」状態を
型とAPIの境界で防ぐ。

接続要求には少なくとも`MenuManaged`、`SavedProfile`、`ShellManual`の接続元を持たせる。
`MenuManaged`と`SavedProfile`はassociation成功後にDHCPを開始する。`ShellManual`は
associationまでで止まり、現行と同じく`run 'ipconfig dhcp' to get an address`を表示する。
利用者が実際に`ipconfig dhcp`を実行した後はIP設定方針をDHCPとして記録し、その接続が
後から切断・再associationした場合はDHCPをresetして取り直してよい。まだ一度も
`ipconfig dhcp`を実行していない`ShellManual`接続へ、管理器が勝手にDHCPを追加してはならない。

フレームループは毎フレーム`service`を呼ぶ。`service`は1回の処理量を制限し、次を行う。

1. ESP-Hostedをポーリングし、STA接続／切断イベントとSDIOリンク状態を読む
2. 接続中ならsmoltcpをポーリングし、DHCPイベントを反映する
3. 状態遷移に応じてアドレスと進行中ソケットを無効化する
4. 再試行期限が来たら1回だけ次の接続処理を開始する

ブラウザのような全画面アプリも生の`Rpc`と`Stack`を長期間所有せず、管理器から現在有効な
ネットワークを短時間借りる。切断時は通信世代番号を更新し、古いTCP／DNS処理が前のリンクを
使い続けないよう中断してハンドルを返す。

### 切断原因の分類

原因を1つの「Wi-Fiエラー」に潰さない。

| 観測点 | 例 | 回復処理 |
| --- | --- | --- |
| STA切断イベント | reason 4、200、201、202 | アドレスを外し、理由別に再アソシエーションまたは入力待ち |
| ESP-Hostedリンク死 | SDIO/RPCの連続失敗、C6無応答 | `Rpc`と`Stack`を破棄し、C6リンクから再構築 |
| DHCP deconfigured／timeout | APとのassociationは維持 | DHCP方針が有効な接続だけsocketをresetして再取得。直ちにWi-Fiを切らない |

直近16件程度の遷移履歴をRAMへ保持し、時刻、旧状態、新状態、reason/status、試行回数を
`wifistatus`または`wifilog`で表示できるようにする。SSIDは表示してよいがパスワードは
保持形式、長さ、成否を含めログへ出さない。この履歴により「C6側の仕様か、AP側か」を
推測だけで決めず、どの層が最初に異常を報告したかを実機で切り分ける。

### リトライ方針

再試行は理由別に扱い、永久的な認証失敗でAPへ接続要求を送り続けない。

| 条件 | 初期動作 | 継続動作 |
| --- | --- | --- |
| reason 4 `DISASSOC_DUE_TO_INACTIVITY` | 500 ms後に再試行 | 連続3回を超えたら一般backoffへ移行 |
| reason 200 `BEACON_TIMEOUT`、201 `NO_AP_FOUND`、203 `ASSOC_FAIL`、205 `CONNECTION_FAIL` | 1秒後に再試行 | 1、2、4、8、16、最大30秒の指数backoff |
| reason 15／204 handshake timeout、202 `AUTH_FAIL`、互換securityなし | 自動再試行を停止 | `Needs password`を表示し、利用者の再入力を待つ |
| 接続イベントtimeout | 1回は再試行 | 以後は一般backoff。RPC link healthも確認 |
| C6／SDIOリンク死 | C6を安全に停止してリンク再構築 | 最大30秒backoff。再構築後に保存プロファイルで接続 |
| DHCP timeout／lease loss | Wi-Fi接続を維持してDHCP reset | メニュー接続、保存profile、または手動でDHCPを開始済みの場合だけ最大30秒間隔で再取得。STA切断が来たらそちらを優先 |

接続がOnlineで10分以上安定したら連続失敗回数を0へ戻す。手動の`Retry now`はbackoffだけを
解除し、誤ったパスワードを自動で再利用する禁止までは解除しない。カウンタは飽和させ、
時刻計算は`tick::now_ms()`の単調時刻で行う。

### Wi-Fi OFFの契約

OFFは表示上の印ではなく、次をすべて満たした状態とする。

- 新しいスキャン、接続、DHCP、バックグラウンド再試行を開始しない
- 進行中のDNS／TCP処理をエラーで完了させ、IPアドレス、route、resolverを外す
- C6リンクが生きていればbest-effortで`esp_wifi_disconnect`を送り、イベントを短時間待つ
- `Rpc`と`net::Stack`を破棄する
- SDIO／microSD共存への影響を確認したうえで、可能ならC6を`power_down_c6`する
- 保存済みAPは削除しない。再びONにすれば同じAPへ接続できる

OFF中でも`wifi`メニューと状態表示は開ける。`wifiinfo`など明示的な低層診断コマンドは
一時的にC6を操作してよいが、終了後にOFFへ戻り、自動接続を有効にしてはならない。

## 資格情報と永続化

### 現状の制約

`/tmp`はPSRAM上のRAMディスクで起動ごとに再フォーマットされる。SDとUSBのVFSは
読み取り専用なので、どちらも設定保存先にできない。SPI Flashには`nvs`と`storage`
パーティションが予約されているが、現行ファームウェアはFlash書き込みもESP-IDF NVSも
実装していない。

また、C6へ送るWi-Fi初期化設定には`nvs_enable = 1`があるが、それだけでは
「P4を再起動しても、資格情報を読み戻せて接続できる」保証にならない。ESP-Hostedの
`get_config`相当RPCの有無、C6電源断後の保持、明示disconnect後の保持をStage 6で実機確認する。

### 採用判断

Stage 6の調査結果で次の順に選ぶ。

1. **C6側NVSを利用できる場合**: パスワードはC6だけに保持し、P4側にはON/OFF、SSID、
   レコードversionだけを保存する。起動時はC6の保存済みconfigでconnectする。P4側に
   パスワードを重複保存しない
2. **C6側から確実に再利用できない場合**: P4の専用settingsパーティションへSSIDと
   パスワードを保存し、起動時に`set_config`し直す

P4側の設定保存が必要な場合は、既存の12 MiB `storage`を直接アドレス決め打ちで使わず、
`partitions.csv`に消去単位へ揃えた小さい`settings`パーティションを追加し、`storage`を
その分だけ縮める。2セクタ以上のA/B journalとし、magic、format version、sequence、length、
CRC、payloadを持たせる。inactive側を消去・書き込み・読み戻し検査してからcommit markerを
最後に書き、電源断時は最後の完全なrecordへ戻る。再接続のたびには書かず、保存APの変更、
forget、ON/OFF変更時だけ書く。

ESP32-P4はFlash XIP中なので、消去／書き込み中に通常のFlashコードや定数へ触れてはならない。
ROM API、キャッシュ停止、割り込み、IRAM／DRAM閉包を含む実装は独立した実機Stageとして扱い、
既存の`tools/check_elf_layout.py`でFlash非依存性を検査する。失敗時は自動接続を無効にして
通常起動を続け、壊れた設定を推測して使わない。

Flash encryptionが有効でない個体でP4側へ保存する場合、パスワードは物理的なFlash読み出しに
対して秘匿されない。独自の固定鍵や難読化を暗号化とは呼ばない。保存前にこの制約を画面へ
一度表示し、`Save and auto-connect`と`Connect once`を選べるようにする。RAM上のパスワードは
置換、forget、OFF後に明示的にzeroizeし、panicや診断ダンプへ含めない。

## 段階分け

### Stage 0: 最小メニューの境界固定とbaseline

- `wifiscan`、`wificonnect`、`ipconfig dhcp`を現行コードで1回ずつ実行し、正常時の表示と
  UARTログをbaselineとして記録する
- `wifi`という新コマンド名、`Outcome::WifiMenu`相当の画面遷移、終了後に返す
  `Rpc`／`Stack`の所有権を決める
- Stage 1で再利用する既存関数と、シェルから切り出す最小helperを列挙する
- 最小メニューに入れない機能を上記「実装優先順位」の一覧で固定する

**完了条件**: Stage 1の変更対象が`app`の画面遷移、`wifi_menu`、必要最小限の共通helperに
限定され、Flash、partition、接続管理器、browser／fetchのAPI変更を含まないこと。

### Stage 1: キーボード操作の最小Wi-Fiメニュー

- `src/app/wifi_menu.rs`相当を追加し、`wifi`コマンドから全画面で開く
- 画面を書き戻して`Scanning...`を表示した後、既存のblocking scanを1回実行する
- scan結果を並べ替えたり統合したりせず、SSID、RSSI、channel、auth modeを表示する
- 上下キー、Enter、`R`、Escapeで一覧を操作し、選択が画面端を越えたら自動scrollする
- OPEN APは直接接続し、それ以外は最大64 byteのマスク付き入力欄を開く
- 既存のblocking `connect`と`wait_for_connection`でassociation結果を表示する
- メニュー接続が成功した場合だけ`net::Stack`を用意し、既存のDHCP開始／待機処理を呼ぶ
- メニューを閉じると、接続済み`Rpc`と取得済み`Stack`を`app`の通常ループへ返す
- 入力キャンセル、失敗、成功、画面終了の全経路でpassword bufferをzeroizeする

最小実装ではスキャン中、接続イベント待ち、DHCP待ちの途中キャンセルは行わない。各処理前に
進行表示をflushし、既存timeoutで必ず戻る。非blocking化はStage 3で行う。

**完了条件**: キーボードだけでAP一覧から接続し、追加コマンドなしにIPv4、router、DNSを
取得できること。Escapeでシェルへ戻った後に`ping`が成功すること。

### Stage 2: 最小メニューの実機受入とCLI回帰

- OPENとWPA2-PSKのAPで、scan、scroll、password入力、association、DHCPを確認する
- scan 0件、scan失敗、誤password、接続timeout、DHCP timeout、Escapeを確認する
- パスワードが画面、UART、コマンド履歴へ出ないことを確認する
- `wificonnect`直後にはDHCP packetが出ず、`ipconfig dhcp`後にだけ取得することを確認する
- `wificonnect`→`ipconfig dhcp`→`ping`→`browser`の既存経路を回帰確認する
- release build、ELF layout検査、既存の実機起動を確認する

**完了条件**: メニュー経由だけDHCPが自動実行され、CLIの接続契約を変えず、Stage 3以降を
未実装のまま日常利用できること。この時点を最初の実装マイルストーンとする。

### Stage 3: 接続管理器への集約と非blocking化

- `Rpc`、`Stack`、接続元、IP設定方針、接続状態を1つの所有者へ移す
- 現行の毎フレームpollと受信フレーム背圧を維持する
- 接続イベント待ちとDHCP待ちを、画面を固めない状態遷移へ分ける
- shell、Wi-Fi menu、browser、fetchの借用を新しい管理器経由へ変更する
- Stage 1のメニュー描画と入力処理は維持し、接続操作部分だけ管理器呼び出しへ差し替える
- `ShellManual`ではDHCPを開始せず、`MenuManaged`だけ自動開始する契約を型で保持する

**完了条件**: Stage 1の見た目と操作、CLIの手動DHCP、`ping`、`browser`が回帰せず、
`Rpc`だけまたは`Stack`だけが古いまま残る経路がないこと。

### Stage 4: 初回接続リトライ

- reason分類とbackoffを純粋なpolicy部分へ分離する
- reason 4は短い待ちの後に自動再試行する
- 認証失敗は再試行せずパスワード入力へ戻す
- timeout、RPC status、イベント順序逆転を診断履歴へ残す
- 二重connect、期限切れtimer、古い接続イベントを世代番号で拒否する

**完了条件**: 1回目にreason 4を受け、2回目が成功する試験で利用者操作を要求しないこと。
誤ったパスワードでは接続要求を連打しないこと。

### Stage 5: 接続後の自動再接続

- Online中のSTA切断でIP設定と進行中通信を直ちに無効化する
- AP停止／再開後に同じ資格情報で再associationし、DHCP方針が有効だった接続だけDHCPを取り直す
- C6／SDIOリンク死では下層から再構築する
- ブラウザ表示中も管理器を毎フレームserviceし、切断時に操作不能にしない
- 安定期間後にbackoffをresetする

**完了条件**: メニュー接続したAPを5分停止してから戻した場合、再起動やコマンド入力なしで
Onlineへ戻ること。
1時間放置を3回行い、切断しても再接続するか、少なくとも分類済みの停止理由を残すこと。

### Stage 6: 永続化経路の調査、保存、起動時自動接続

- C6へ設定した資格情報がHP CPU reboot、C6 reset、C6 power cycleをまたぐか確認する
- ESP-Hostedに保存済みconfigを安全に再利用／削除するRPCがあるか確認する
- 上記「採用判断」に従ってC6 NVSまたはP4 settings方式を選び、根拠をこの文書へ記録する
- 選択した保存方式を実装する
- format version、CRC、電源断時rollback、未知versionの安全な無視を実装する
- `Save and auto-connect`と`Connect once`を選べるようにする
- 起動時は保存profileがありONなら、UIを開かず接続とDHCPを開始する
- 新しいAPはassociation成功後にだけ旧profileと置き換える
- Flash書き込み回数と失敗を診断表示する。パスワードは表示しない

**完了条件**: 完全電源断をまたいで自動接続し、保存途中で電源断しても前のprofileまたは
未設定のどちらかとして起動すること。壊れたrecordでpanicしないこと。

### Stage 7: ON/OFF、追加メニュー操作、既存コマンド統合

- 同じSSIDのscan結果を1項目へまとめ、最も強いRSSIと検出BSSID数を表示する
- Page Up／Downによる一覧移動を追加する
- AP一覧のタッチ選択、項目のtap、必要ならマウス操作を追加する
- メニューとシェルの`wifi on|off|status|forget`を同じ管理器へ接続する
- OFF処理を上記「Wi-Fi OFFの契約」に合わせる
- ON/OFFを永続化し、OFFで再起動した場合は自動接続しない
- `wificonnect`は従来どおりassociationだけを行い、資格情報保存やDHCPを暗黙に開始しない。
  保存が必要なら別の明示操作を使い、裏で別sessionを作らない
- `wifidisconnect`は「今回だけ切断」と「Wi-FiをOFF」の違いを明示する
- `wifiinfo`／`wifiup`が通常sessionを壊す場合は確認または一時停止を入れる

**完了条件**: OFF後は10分放置しても接続要求が出ず、再起動後もOFFを維持すること。
ONに戻すと保存profileでOnlineへ戻り、forget後は自動接続しないこと。キーボードだけの
Stage 1操作を壊さず、タッチでもAPを選択できること。

### Stage 8: 実機受入と文書更新

次の試験を同じAPだけでなく、可能なら2種類以上の家庭用AP／テザリングで行う。

- OPEN、WPA2-PSK、WPA3またはWPA2/WPA3 mixedへの接続
- SSID 32 byte、password 64 byte、空白を含むpassword、重複SSID、hidden SSID
- reason 4、誤password、AP不在、AP再起動、DHCP不応答、C6 reset、SDIO link loss
- 接続操作中のEscape、OFF、reboot、shutdown
- `/tmp`、microSD、USB MSC、USB keyboard／mouse／touchとの同時利用
- 24時間放置、30回のAP停止／再開、100回のHP CPU reboot、10回の保存中電源断
- 再接続中のブラウザ／ping／DNSが固まらず、古いsocket handleを再利用しないこと
- メニュー接続ではDHCPが自動開始し、コマンドライン接続では`ipconfig dhcp`まで
  DHCP packetを送らないこと
- パスワードが画面、UART、`wifilog`、panic前の通常診断へ一度も出ないこと

実装が確定したら[`WIFI.md`](WIFI.md)、[`NETWORK.md`](NETWORK.md)、
[`APPS.md`](APPS.md)、[`BOOT.md`](BOOT.md)、[`FILE_LAYOUT.md`](FILE_LAYOUT.md)、
[`DIAGNOSTICS.md`](DIAGNOSTICS.md)の該当箇所を同じ作業で更新する。partitionを変更した場合は
`partitions.csv`も現状文書と一致させる。

## 実装順の判断

最初の作業単位はStage 0〜2で打ち切る。ここでは既存の接続関数を画面から順に呼ぶだけで、
利用者が最も頻繁に行う「scan結果を見て選び、passwordを入れ、DHCPまで通す」を先に短縮する。
この段階でreason 4 retry、接続管理器、保存、ON/OFFを同時実装しない。最小メニューが単独で
実機受入できてから、次のStageへ進む。

Stage 3は、Stage 1で確認済みの画面を捨てず、blockingな接続操作だけを状態機械へ置き換える。
その上でStage 4〜5のretry／自動再接続を追加する。こうすれば、メニュー描画の不具合と
バックグラウンド接続管理の不具合を同時に追わずに済む。

Stage 6はXIP中のFlash書き込みという別の故障領域を持つため、接続状態機械が安定してから
追加する。Stage 7のON/OFFは保存形式を確定した後に永続設定まで含めて完成させ、タッチ操作も
この段階で足す。

Stage 6以降を見送る場合も、Stage 1の最小メニューはそのまま利用できる。電源断後は未保存の
資格情報を推測せず、`wifi`メニューを開いて接続し直す。
