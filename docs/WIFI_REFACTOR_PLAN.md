# Wi-Fi接続管理リファクタリング計画

> 索引: [`../DESIGN.md`](../DESIGN.md)
> この文書は作業計画と実機での判断記録です。現在の実装仕様は現状文書と
> コードを優先してください。

## 状態: 全Stage未着手

| Stage | 内容 | 状態 |
| --- | --- | --- |
| 0 | 現象の再現、切断原因の分類、C6側NVS保持可否の確認 | 未着手 |
| 1 | Wi-FiセッションとIPスタックを接続管理器へ集約 | 未着手 |
| 2 | AP一覧とパスワード入力の全画面メニュー | 未着手 |
| 3 | 接続からDHCP取得までの一連化 | 未着手 |
| 4 | 初回接続失敗とreason 4の自動リトライ | 未着手 |
| 5 | 接続後の切断検出と自動再接続 | 未着手 |
| 6 | 接続先と資格情報の永続化、起動時自動接続 | 未着手 |
| 7 | Wi-Fi ON/OFF、保存情報の削除、既存コマンドとの統合 | 未着手 |
| 8 | 長時間・異常系の実機受入と現状文書の更新 | 未着手 |

## 背景

現在はシェルで`wifiscan`、`wificonnect <ssid> [password]`、`ipconfig dhcp`を
順に実行する必要がある。`src/app.rs`は`Option<wifi::Rpc>`と`Option<net::Stack>`を
別々に保持し、接続後は毎フレーム通信をポーリングするが、次の処理は持たない。

- スキャン結果から接続先を選ぶ画面
- パスワードを画面へ露出させずに入力する経路
- アソシエーション成功後のDHCP自動開始
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
- 接続操作を「アソシエーション→DHCP→利用可能」の1つの流れとして扱う
- reason 4など回復可能な接続失敗を自動でリトライする
- 接続後に切断した場合、画面やシェルを固めず、自動で再接続してDHCPを取り直す
- 最後に接続成功したAPを1件保存し、次回起動時に自動接続する
- Wi-Fi全体をON/OFFでき、OFF中は自動接続も自動再接続も行わない
- `wifiscan`、`wificonnect`、`wifistatus`、`wifidisconnect`など既存の診断手段を残す
- 切断がAP／無線、C6／SDIOリンク、DHCPのどの層で観測されたかを診断できる

初版で保存するプロファイルは**最後に接続成功した1件だけ**とする。複数APの優先順位、
企業向け802.1X、WPS、SoftAP、5 GHz、ソフトウェアキーボードは対象外である。

## 利用者から見える動作

### AP選択画面

`wifi`を実行すると現在状態とON/OFFを上部に、スキャン結果を下部に表示する。
同じSSIDを複数BSSIDが広告している場合、現行の接続RPCはSSIDだけを指定するため、
一覧もSSID単位にまとめ、最も強いRSSIと検出BSSID数を表示する。BSSIDを選べるように
見せながら実際には選べない画面にはしない。空SSIDのhidden APは`(hidden)`として表示するが、
初版では選択不可とする。

操作は次で固定する。

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
アソシエーション成功時点で有効とみなす。DHCPだけが失敗してもパスワードを再入力させず、
`Associated / DHCP retrying`として別の問題を表示する。

### 状態表示

利用者向け状態は少なくとも次に分ける。

```text
Off
Idle (no saved network)
Starting C6 link
Associating (attempt N)
Associated / waiting for DHCP
Online (SSID, RSSI, IPv4)
Retry waiting (reason, next attempt)
Needs password (authentication failed)
Link failed (C6/SDIO recovery pending)
```

「接続済み」はアソシエーションだけを指す語として使わず、IPアドレス取得済みの`Online`と
区別する。メニューを閉じても同じ状態機械がバックグラウンドで進む。

## 設計方針

### 所有権を接続管理器へまとめる

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

管理器だけがセッション、IPスタック、現在プロファイル、ON/OFF、再試行回数と期限を変更する。
画面やシェルは`scan`、`connect`、`set_enabled`、`forget`、`status`のような意図を渡し、
内部の`Option`を直接捨てない。これにより「RPCだけ張り直して古いIPアドレスを残す」状態を
型とAPIの境界で防ぐ。

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
| DHCP deconfigured／timeout | APとのassociationは維持 | DHCPソケットをresetして再取得。直ちにWi-Fiを切らない |

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
| DHCP timeout／lease loss | Wi-Fi接続を維持してDHCP reset | 最大30秒間隔で再取得。STA切断が来たらそちらを優先 |

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
`get_config`相当RPCの有無、C6電源断後の保持、明示disconnect後の保持をStage 0で実機確認する。

### 採用判断

Stage 0の結果で次の順に選ぶ。

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

### Stage 0: baselineと永続化経路の確認

- 現行コードで30分以上放置し、STA切断reason、RPC link health、DHCP状態を記録する
- AP側ログが取れる場合はstation timeout時刻と突き合わせる
- reason 4、beacon timeout、AP電源断、C6 reset、DHCP server停止を別々に再現する
- C6へ設定した資格情報がHP CPU reboot、C6 reset、C6 power cycleをまたぐか確認する
- ESP-Hostedに保存済みconfigを安全に再利用／削除するRPCがあるか確認する

**完了条件**: 少なくとも各層の切断を識別できるログが取れ、C6 NVS採用可否と根拠を
この文書へ記録していること。観測できない原因をreason 4と決め打ちしない。

### Stage 1: 接続管理器への集約

- `Rpc`、`Stack`、接続状態、retry policyを1つの所有者へ移す
- 現行の毎フレームpollと受信フレーム背圧を維持する
- shell、browser、fetchの借用を新しい管理器経由へ変更する
- 既存コマンドの出力と手動接続動作を変えずにrelease buildを通す

**完了条件**: 手動の`wificonnect`→`ipconfig dhcp`→`ping`→`browser`が回帰せず、
`Rpc`だけまたは`Stack`だけが古いまま残る経路がないこと。

### Stage 2: 全画面Wi-Fiメニュー

- `src/app/wifi_menu.rs`相当を追加し、`wifi`コマンドから開く
- AP一覧のSSID単位集約、選択、scroll、再スキャンを実装する
- OPEN APの直接接続と、マスク付きパスワード入力を実装する
- タッチなし、マウスなしでもTab5 Keyboard／CardKB／USBキーボードだけで完結させる
- 入力キャンセル後と画面終了後にパスワードbufferをzeroizeする

**完了条件**: AP名やパスワードをコマンド行へ入力せず、一覧選択から接続要求まで進めること。

### Stage 3: 接続とDHCPの一連化

- 接続イベントを待つ処理をblockingな`wait_for_connection`から状態遷移としても使える形へ分ける
- association成功時にMACを用意して`net::Stack`を生成し、DHCPを自動開始する
- association成功、DHCP待ち、Onlineを別状態として表示する
- DHCP timeoutでもassociationを切らず、手動のstatic IP設定を診断用に残す

**完了条件**: AP選択とパスワード入力の後、追加コマンドなしでIPv4、router、DNSを取得し、
pingまたはHTTP GETが成功すること。

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
- AP停止／再開後に同じ資格情報で再associationし、DHCPを取り直す
- C6／SDIOリンク死では下層から再構築する
- ブラウザ表示中も管理器を毎フレームserviceし、切断時に操作不能にしない
- 安定期間後にbackoffをresetする

**完了条件**: APを5分停止してから戻した場合、再起動やコマンド入力なしでOnlineへ戻ること。
1時間放置を3回行い、切断しても再接続するか、少なくとも分類済みの停止理由を残すこと。

### Stage 6: 保存と起動時自動接続

- Stage 0で選んだC6 NVSまたはP4 settings方式を実装する
- format version、CRC、電源断時rollback、未知versionの安全な無視を実装する
- `Save and auto-connect`と`Connect once`を選べるようにする
- 起動時は保存profileがありONなら、UIを開かず接続とDHCPを開始する
- 新しいAPはassociation成功後にだけ旧profileと置き換える
- Flash書き込み回数と失敗を診断表示する。パスワードは表示しない

**完了条件**: 完全電源断をまたいで自動接続し、保存途中で電源断しても前のprofileまたは
未設定のどちらかとして起動すること。壊れたrecordでpanicしないこと。

### Stage 7: ON/OFFと既存コマンド統合

- メニューとシェルの`wifi on|off|status|forget`を同じ管理器へ接続する
- OFF処理を上記「Wi-Fi OFFの契約」に合わせる
- ON/OFFを永続化し、OFFで再起動した場合は自動接続しない
- `wificonnect`成功時も保存確認を経由できるようにし、裏で別sessionを作らない
- `wifidisconnect`は「今回だけ切断」と「Wi-FiをOFF」の違いを明示する
- `wifiinfo`／`wifiup`が通常sessionを壊す場合は確認または一時停止を入れる

**完了条件**: OFF後は10分放置しても接続要求が出ず、再起動後もOFFを維持すること。
ONに戻すと保存profileでOnlineへ戻り、forget後は自動接続しないこと。

### Stage 8: 実機受入と文書更新

次の試験を同じAPだけでなく、可能なら2種類以上の家庭用AP／テザリングで行う。

- OPEN、WPA2-PSK、WPA3またはWPA2/WPA3 mixedへの接続
- SSID 32 byte、password 64 byte、空白を含むpassword、重複SSID、hidden SSID
- reason 4、誤password、AP不在、AP再起動、DHCP不応答、C6 reset、SDIO link loss
- 接続操作中のEscape、OFF、reboot、shutdown
- `/tmp`、microSD、USB MSC、USB keyboard／mouse／touchとの同時利用
- 24時間放置、30回のAP停止／再開、100回のHP CPU reboot、10回の保存中電源断
- 再接続中のブラウザ／ping／DNSが固まらず、古いsocket handleを再利用しないこと
- パスワードが画面、UART、`wifilog`、panic前の通常診断へ一度も出ないこと

実装が確定したら[`WIFI.md`](WIFI.md)、[`NETWORK.md`](NETWORK.md)、
[`APPS.md`](APPS.md)、[`BOOT.md`](BOOT.md)、[`FILE_LAYOUT.md`](FILE_LAYOUT.md)、
[`DIAGNOSTICS.md`](DIAGNOSTICS.md)の該当箇所を同じ作業で更新する。partitionを変更した場合は
`partitions.csv`も現状文書と一致させる。

## 実装順の判断

Stage 1〜5を先に行えば、Flash書き込みに触れずに「選んで接続、DHCP、reason 4 retry、
放置後の再接続」まで実機評価できる。永続化に問題があっても、接続回復の中核を巻き戻さずに
済む。Stage 6は便利さの最後の要件だが、XIP中のFlash書き込みという別の故障領域を持つため、
接続状態機械が安定してから追加する。Stage 7のON/OFFは保存形式を確定した後に永続設定まで
含めて完成させる。

実装途中でStage 6を見送る場合も、Stage 1〜5の管理器は起動時に勝手に未保存の資格情報を
推測しない。電源断後は`Idle`から始まり、メニューで再接続する。
