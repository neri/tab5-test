# USB HID Report protocolと複数入力の調停計画

> 索引: [`../../../DESIGN.md`](../../../DESIGN.md)
> この文書は作業計画です。現在の実装仕様は[`USB.md`](../../USB.md)、
> [`INPUT.md`](../../INPUT.md)とコードを優先してください。

## 状態: 提案（全Stage未着手・実機未確認）

2026-09-06の作業ツリーを基準とする。今回は計画書だけを追加し、実装は変更しない。
以下の推奨案・数値・診断出力は決定済みの仕様や実測結果ではない。

## 目的と範囲

Report descriptorに従ってキーボードと相対マウスを読み、複数の物理機器、
複数interface、同一interface内の複数Report IDを共存させる。
既存のBoot入力、CardKB／Tab5 Keyboard、タッチ、MSCとの共存を維持する。
「Report protocol対応」は全HID Usageへの対応を意味しない。

初期対象はKeyboard/Keypad、修飾キー、相対X/Y、縦ホイール、左右・中央ボタン。
NKROのbitmap形式と従来のarray形式を対象とする。ゲームパッド、絶対座標ポインタ、
USBタッチ、Consumer/System Controlの操作、横スクロール、追加ボタン、独自Feature、
複数カーソル、多段ハブ、高帯域Interrupt転送は別段階とする。
未対応Usageが混在しても、解釈できるレポートの位置を壊さず、対応機能を利用できる設計にする。

## コードから見える現状と問題

| 箇所 | 現状 | 改修が必要な理由 |
| --- | --- | --- |
| `src/usb/hid.rs` | Boot subclass/protocolで最初のinterfaceを選び、Bootへ切替。Report descriptorは未取得 | subclass 0や可変レイアウトを扱えない |
| `src/usb/registry.rs` | 物理ポートのslotに単一`DeviceKind`。keyboard→mouse→MSCの最初の成功を採用 | キーボード＋マウスreceiver、HID＋MSCの機能を同時登録できない |
| `hid.rs`／`bot.rs` | class attachがそれぞれ`SET_CONFIGURATION`を発行 | 複数bindに拡張すると他interfaceの転送状態を再初期化する危険がある |
| `src/usb/protocol.rs` | Configuration取得bufferは256 byte | 複合機器の後方interfaceが切り落とされ得る |
| `src/usb/hid_keyboard.rs` | 8 byte固定、前回キー6個、pending 6件 | Report ID、NKRO、広いUsage、修飾状態の独立管理がない |
| `poll_keyboards`／`poll_split_keyboards` | キーを返すと開始slotを進める巡回は既存 | 受信とアプリへの配信が結合。Splitは最小intervalを共有し、endpoint個別の期限ではない |
| `src/usb/hid_mouse.rs` | 最低3 byte、最大8 byte。4 byte目をwheelとみなす | 記述子にない位置の推測。1 poll内の押下→解放は始点と終点の比較で消える |
| `UsbHost::poll_mice` | 今回updateを返したマウスの状態・edgeをORする | 無応答のマウスの保持状態が合成から消える。他機器が保持中でもreleaseを出し得る |
| `src/input.rs` | USBを一つの`KeySource`とし、pending満杯なら黙って捨てる | 機器単位の公平性、切断時の古いイベント除去、過負荷の観測が不足 |
| `src/usb/hcd.rs` | persistent periodicは4枠。Split stagingは64 byte。MSC併用時はchannel 0へ戻す | HID数とhardware枠数は別。転送量と待ち時間の総予算が必要 |

複数HIDが全く考慮されていないわけではない。既存の巡回とマウス移動量加算を土台に、
物理機器と入力機能の分離、および状態を持つ調停へ進める。

## 実装前に決める事項

| 判断事項 | 推奨案 | 代替案・影響 |
| --- | --- | --- |
| 別キーボードのShift/Ctrlを合成するか | 初期版は入力元の論理キーボード内に限定 | 全USBで合成するとAのShift＋Bのaで大文字になる。CardKB等は既に文字へ変換済みなので同じ契約にはできない |
| 同じキーを2台から押した場合 | 入力元ごとに押下差分を取り、2件の入力を配信 | 全体集合で差分を取ると2台目の同じキーが消える |
| 複数マウス | 移動量は加算、ボタンは全入力元の保持状態のOR | 操作中の1台へ排他的に所有させる方式もある。初期版は専有切替を設けない |
| ドラッグ中に保持元を抜く | cancelしてUI操作を成立させない。他機器が保持するボタンは残す | 通常releaseとして扱うと抜去でクリックやdropが成立する |
| Protocol選択 | Report優先、Boot対応interfaceに限って初期化失敗時に明示的Boot fallback | Boot優先は互換性重視だがNKROや標準wheelが使われない |
| キー配列・lock・repeat | 現行のキー変換を維持。lock状態、LED Output、ホストrepeatは今回追加しない | フルキーボード仕様まで含めるなら、lockの共有範囲と全機器へのLED同期を追加設計する |
| 必須実機 | Boot機器、Report専用/NKRO機器、複合receiver、キーボード2台、マウス2台を用意 | 用意できない形式は未確認として残す。対象機器のVID:PIDと記述子を先に集める |
| 受入台数と性能 | ハブ1段・8ポートの既存上限を維持。HID endpointは全体16枠を初期候補とする | 最大台数を保証するには実機と時間・メモリ見積りが必要。4 periodic枠を超える試験が必須 |

特に修飾キーの共有範囲とマウスの合成方針は、Stage 1前に利用者と合意する。
対応機器と性能上限はStage 0で決める。回答がまだ無い事項は推奨案を設計の仮定とし、
受入済みとは扱わない。

## 設計案

### 1. 所有と列挙

`UsbHost`によるバスの単一所有を維持し、次の階層へ分ける。

```text
UsbHost
  PhysicalDevice（port、address、connection/session generation、configuration）
    InterfaceBinding[]（interface number、alternate setting、class）
      HidInterface（選択protocol、解析済みlayout、InterruptIn）
        Report ID / Application Collection → 論理キーボード・マウス状態
      MassStorage（既存BOT/MSC）
InputManager
  入力元別状態・queue → 公平なキー配信／単一ポインタへの調停 → 各GUI
```

Configurationを完全に取得・検査して候補を選び、物理機器ごとに一度だけ設定する。
HIDとBOTのattachから設定要求を外す。初期版は選択済みconfigurationのalternate 0、
HID interfaceあたりInterrupt IN 1本を対象にし、その他は理由付きで未対応にする。
同じendpointをkeyboardとmouseが別々に読まず、1回受信した内容を機能へ振り分ける。
同一Report ID内の複数Collectionも区別する。Report IDだけを機器識別子にしない。

識別子は物理port＋接続世代＋session世代＋interface＋論理機能で構成する案とする。
USB addressやslotの再利用、Recoveryによる再生成で古いqueueを新機器へ渡さない。
既存MSC番号、`ConnectionEpoch`、自動マウントの意味は保ち、HID内部の再構成だけで
媒体の物理切断を捏造しない。診断用`DeviceSummary`も複数機能を表示できるよう変更する。

### 2. Report取得と解析

HID descriptorの従属descriptor一覧を走査してReport descriptorを取得する。
先頭の組がReportだという現在の仮定を外す。解析は列挙時に一度行い、受信時は
上限付きのfield tableを参照する。ConfigurationとReport descriptorの途中切断を
成功扱いしない。長さ超過はbindを拒否して理由を表示する。

parserはMain/Global/Local、Collection、Push/Pop、Usage範囲、Report ID、
bit単位のfield、符号拡張、Array/Variable、Constant、Relative/Absoluteを扱う。
Input/Output/Featureのoffsetは混ぜず、未対応Usageもfield幅に反映する。
Local状態の寿命、Report Size×Countのoverflow、stack深度、Usage範囲、
Report IDの不正値、欠損item、未知・未対応itemを明示的に検査する。
安全に位置を確定できない構文は推測せずそのinterfaceを拒否する。

Report ID付き／無しを区別し、受信実長とIDごとの期待長を照合する。
別IDのレポートが来ても他IDのキー・ボタン状態を消さない。
短いレポート、未知ID、不正値では直前の正常状態を上書きせず診断に数える。
rollover/error usageは正常な全解放とみなさない。繰り返す不正入力は当該入力元を
隔離・cancelし、永続した押下を残さない。無通信・通常NAKを故障扱いしない。

初期候補はConfiguration 4 KiB、Report descriptor 4 KiB/interface、Input report
64 byte（ID込み）かつendpointの1 packet以内、field 128/interface、ID 16/interface、
Collection/Global stack各8段、Usageはpage付きで保持する。
これは有限な対応範囲であり、特にLow-Speedのpacketをまたぐreportは初期対象外となる。
NKROの検出状態を6件queueへ縮めず、Keyboard pageの対象Usage集合で持つ。
memoryの全体上限を別に設け、16 endpoint×最大descriptorを無制限に常駐させない。
Stage 0で静的領域・stack・一時buffer・解析済みtableの最悪値を算出して確定する。

### 3. Protocolと既存経路の移行

Boot subclassには`SET_PROTOCOL(Report)`を明示する。非Bootにはその要求を必須にせず
Report descriptorを使用する。Bootへ戻すときは切替成功を確認して専用decoderを選ぶ。
切替失敗後の内容をBootだと推測しない。`SET_IDLE`の失敗はSTALLとバス故障を分け、
機能・規格上の要否に沿って継続可否を決める。idle再送をキーrepeatにはしない。

Boot decoderも共通の「入力元の状態更新」へ変換する。Report経路のwheel位置を
固定byteで推測しない。従来Bootの4 byte目wheel拡張は互換経路として明記し、
維持する機器と挙動を実機記録に残す。
通常運用中の不正reportでprotocolを往復させない。fallbackは初期化時に限定する。

### 4. 入力状態とイベント順序

受信・状態更新とアプリが1件ずつ取り出すAPIを分離する。アプリがキーを取り出さなくても
USB保守と受信は予算内で進める。入力元別の順序付きqueueを持ち、キー配信を巡回する。
USB内部の公平性とCardKB／Tab5 Keyboardとの公平性を両方維持する。

キーは論理キーボードごとの正常な集合の差分で押下を作る。同一論理機能の複数IDを
合成してから比較する。修飾キーだけが別IDでも失わない。同時押下の順序は観測順、
同一report内はUsage順などの決定的な規則とし、物理的な先後を保証しない。
既存`Key`への変換は状態更新後に行い、未対応Usageを8 bitへ切り詰めない。

マウスは応答の有無に関係なく全入力元の最新ボタン状態を保持する。
レポートを1件適用するたびに合成前後のボタン集合からedgeを作る。
AとBが左を押しているときAだけが離してもreleaseを出さない。
短いpress→releaseを保存するため、`MouseUpdate`の最終状態だけでは足りない。
順序付きpointer event APIを追加し、GUI consumerを移行する。移動の併合は
ボタンedgeを跨がない範囲とし、押下位置と解放位置を保持する。加算overflowも防ぐ。

画面遷移の`discard_queued_keys`は取得済みイベントだけを捨て、held履歴は保つ。
切断・session無効化ではその入力元のqueueと状態だけを捨て、pointer cancelを通知する。
system barやtouch優先規則は[`SYSTEM_BAR.md`](../../SYSTEM_BAR.md)と整合させる。

queue候補はkey 32件/入力元、pointer edge 64件/全体。超過は黙って捨てず数える。
状態追跡は継続し、キーは新規イベント欠落を記録、pointerはgestureをcancelして
解放まで新規操作を抑止する。有限queueで過負荷時の全入力保存は保証しない。

### 5. 転送の公平性と故障の範囲

endpointごとに速度に応じた`bInterval`と次回期限を保持する。Split keyboardだけの
最小interval共有をやめ、mouseも同じ期限管理へ入れる。
periodic枠はendpointに割り当て、満杯なら明示的にframe pollへ戻す。
全endpoint数とperiodic枠数を混同しない。High-Speedのinterval表現は別途検査する。

一巡の開始点を進め、endpoint当たりの受信件数とサービス全体の時間の両方を制限する。
仮の通常目標は全HIDサービス2 ms/frame、平常時キー配信100 ms以内とするが、
Splitの待ち時間を含む実測で確定する。1転送自身が残り予算を超え得るため、
既存timeoutとの関係も測る。高速mouseがkeyboardや画面描画を飢餓させないことを優先する。

MSC登録時のchannel 0直列化、Splitとの排他、DMA cache同期、PID引継ぎ、
列挙終了後のperiodic開始を維持する。MSCの同期処理中はHIDの遅延が増え得る。
今回の調停だけでhard real-timeを約束せず、通常時とMSC負荷中の遅延を別に測る。
MSC負荷時の目標を満たせない場合は転送分割／協調serviceを別Stageとして見積もる。

不正descriptorとresource不足は該当interfaceに閉じ込める。endpoint故障はまず
当該機能を停止する。局所回復できずバス全体のRecoveryが必要な場合は、理由と
全HIDのcancelを通知して再列挙する。安全性を証明せずFIFO共有の既存保護を緩めない。

## 作業段階と受入条件

| Stage | 作業 | 次へ進む条件 |
| --- | --- | --- |
| 0 未着手 | 対象実機・descriptor採取、仕様判断、メモリと時間予算、既存Bootの基準ログ | 修飾共有・合成方式・上限・性能目標と試験機器を記録 |
| 1 未着手 | 物理deviceとinterface binding分離、configuration設定の一元化、複合Boot対応 | Boot keyboard＋mouse複合機器と別ポートMSCが共存。MSC番号・挿抜を回帰確認 |
| 2 未着手 | descriptor取得・parser・layout診断。入力切替はまだ行わない | 対象descriptorと異常fixtureを実機診断で照合。上限超過を安全に拒否 |
| 3 未着手 | Report keyboard／mouse decoder、ID分配、Boot fallback | Report専用・NKRO・wheel・非byte境界・複数IDを確認。Boot回帰も確認 |
| 4 未着手 | 入力元別状態・queue、mouse edge順序、切断cancel、GUI移行 | 2 keyboard＋2 mouseで保持・同時入力・短いclick・抜去の期待値が一致 |
| 5 未着手 | endpoint別期限、公平な予算、periodic枠不足、MSC/Split共存 | 4枠超のHIDが応答し、通常時／MSC負荷時の遅延・overflowを測定 |
| 6 未着手 | 総合実機回帰と現状文書更新、制限確定 | 以下の必須試験を人間が確認。未確認機器・形式は明示して残す |

Stage 4のevent型と状態契約はStage 1前に定義し、Stage 3から利用する。
各Stageのコード変更時に`cargo build --release`を実行する。
実機書き込み・実行・UART確認は人間に依頼し、エージェントは実行しない。
parserのfixtureは実機上の診断入口で検査する案とし、host test環境の新設は別判断とする。

## 実機で確認する手順（実装後、人間が実施）

書き込み後、まず`lsusb`、`usbhub`、`usbhw`で構成を保存する。
Stage 2で`lsusb`詳細へinterfaceごとのprotocol、Report ID・長さ、対応Usage、
fallback／拒否理由を追加する。入力traceは新設予定の`hidtrace`に出す案とし、
入力元ID、受信連番、ボタン集合、press/release/cancel、queue overflow、最大service時間、
endpointの遅延を記録する。これは現在存在するコマンドではない。
traceは明示実行中だけ有効にし、通常ログへ打鍵内容を常時保存しない。

| 構成・操作 | 期待結果 | 失敗症状 |
| --- | --- | --- |
| Boot機器を直結、FSハブ、HSハブ経由で操作 | 文字・移動の従来挙動を維持し、選択protocolが見える | 列挙ループ、無入力、文字化け |
| 非Boot/NKRO keyboardで7キー以上を押し離す | 対応Usageの新規押下が一度ずつ出る | 6キーで頭打ち、離した後も履歴が残る |
| keyboard＋mouse receiverを1個接続 | 同一address配下の両機能が使える | keyboardだけ使え、mouseが見えない |
| 同一interfaceの複数ID、同一IDの複数機能 | 各状態が独立して更新される | mouse reportでheld keyが消える |
| keyboard AでShift保持、Bでa。同じaを両方で押す | 推奨案ならBは小文字、同じaは2件 | 他機器の修飾混入、2台目の押下欠落 |
| mouse A左保持中、Bだけ移動。その後Bも左を押しAを離す | ボタン保持は継続し、最後の解放だけrelease | ドラッグが途中で切れる、余分なclick |
| 同一frame内でpress→release、移動を挟む | edge順序と位置を保持し、clickが1回 | click消失、二重click、押下位置のずれ |
| ドラッグ元を抜去、別機器が保持する場合も試す | 消えた入力元だけcancel、残りの保持は継続 | 抜去でdrop成立、押しっぱなし、他機器の状態消失 |
| キー保持中の抜去、同ポートへ別機器、`usbrescan`、画面遷移 | 古いqueueが流れず、新入力を受ける | 遷移先へ文字漏れ、再接続で架空のrepeat |
| HIDを5 endpoint以上接続し、MSCも追加して`ut 100` | 枠不足でもfallbackで全機器が応答。既存read試験を維持 | 5個目以降が無入力、枠漏れ、MSC retry増大 |
| 全機器を操作しながらMSC read、CardKB入力、タッチ | traceで公平性・遅延を測れ、タッチ優先と描画を維持 | 入力元の飢餓、長時間停止、queue overflow |
| 不正descriptor／短いreport／未知ID／queue超過fixture | 理由が見え、当該機能に限定した拒否またはcancel | panic、誤入力、無関係な機器の再列挙 |

FS/HSハブは1段とし、給電条件も記録する。各重要挿抜シナリオを10回行う案とする。
対象機器のVID:PID、firmware版、interface、descriptor、protocol、接続順、測定遅延を残す。
MSC write回帰が必要な場合は専用の消去可能媒体を用意し、既存の書込み試験手順に従う。
今回の計画では書込み試験の実行や対象LBAを指定しない。

## 更新対象と残るリスク

実装時の更新対象は`USB.md`（対応範囲・所有・転送）、`INPUT.md`（入力契約）、
`FILE_LAYOUT.md`（新parser等）、`DIAGNOSTICS.md`と`CONSOLE_COMMAND_REVIEW.md`
（診断）、`SYSTEM_BAR.md`／`APPS.md`（pointer eventとcancel）とする。
`DESIGN.md`の現状欄は実装・受入に合わせて更新する。`README.md`は編集しない。

最大の技術リスクは、parserそのものに加え、configuration設定の移動がBOTや
複合機器へ与える影響、短いクリックを保存するためのGUI契約変更、Split/MSCとの
共有時間である。規格どおりでない機器はVID:PID別quirkを増やす前に記述子と実通信を
確認し、一般decoderへ無条件の推測を入れない。BootとNKROの重複出力を持つ機器も
実機で識別し、別の正当なキーボード入力まで時間窓で重複除去しない。

## 参照資料

- [USB-IF HID 1.11](https://www.usb.org/sites/default/files/documents/hid1_11.pdf):
  descriptor、Reportの形式、protocol切替の一次資料。実装時は6章、7章、8章と付録Bを照合する。
- [USB-IF HID仕様・Usage Tables](https://www.usb.org/hid):
  Usage定義の入口。採用する版をStage 0で固定して記録する。
- [`USB_INTERRUPT_REFACTOR_PLAN.md`](../archive/USB_INTERRUPT_REFACTOR_PLAN.md)、
  [`USB_BOT_HCD_REFACTOR_PLAN.md`](../archive/USB_BOT_HCD_REFACTOR_PLAN.md): 既存HCD経路の判断記録。
- [`INPUT_MANAGER_PLAN.md`](../archive/INPUT_MANAGER_PLAN.md): 既存の入力源巡回の経緯。
