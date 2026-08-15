# SoftI2CトランザクションAPI化計画

## 状態: 完了（実装・実機動作確認済み）

通常の`write`/`read`/`write_read`、可変長用の`transaction`、既存I2Cドライバの
移行を実装した。呼び出し元がSTART/STOPやアドレスのR/Wビットを直接操作する
通常転送は残していない。トランザクションAPIと、GPIO設定・バス復旧を物理バスの
初期化へ集約した変更の両方を実機で確認済みである。

USB MSCは長時間放置後に一度だけBOTセッション破綻（CSW tag mismatch）を観測したが、
再現せず、I2Cの電源操作は該当コマンド実行時に走らないことを確認した。これはI2C
リファクタとは独立した既知の低頻度USBセッション問題として保留する。

`cargo check`と`cargo build --release`は通過済み。ビルド時に出る未使用コードの
警告は、この変更以前からある`framebuffer.rs`のものだけである。

## 背景・目的

現在の`src/i2c.rs`の`SoftI2c`は、START/STOPとバイト単位の送受信だけを
公開している。そのため、各デバイスドライバがアドレス送信、repeated START、
最終バイトのNACK、エラー時のSTOPを個別に実装している。

同じ処理は少なくとも次の箇所に重複している。

| 利用箇所 | 現在の処理 |
|---|---|
| `cardkb.rs` | 7-bitアドレスを読出し方向で送信し、1バイトをNACKして読む |
| `lcd.rs` | PI4IOE1へレジスタ番号と値を書き込む |
| `usb/hcd.rs` | PI4IOE2のレジスタ読出し・書込み（読出しはrepeated START） |
| `touch.rs` | 16-bitレジスタの読出し・書込み（読出しはrepeated START） |
| `bmi270.rs` | 8-bitレジスタの読出し・書込み、16バイト単位の設定転送 |

この計画の目的は、通常のI2C操作を`SoftI2c`に集約し、プロトコル終端の
STOPを必ずバス層が実行するようにすることにある。同時に、先頭バイトの値で
後続の読出し長が決まる可変長プロトコルを表現できる逃げ道も残す。

## 対象としないこと

- 10-bit I2Cアドレス、SMBus固有プロトコル、マルチマスター調停の実装。
  現在使用する全デバイスは7-bitアドレスであり、現行のビットバン実装も
  これらをサポートしていない。
- I2Cバス共有の排他機構の新設。LCD、タッチ、USB VBUS、BMI270は同じ物理バスを
  使用するが、現行は単一スレッドで逐次実行される前提である。トランザクション
  APIは一操作内のSTART〜STOPを閉じるが、割り込みや並行実行を許可する設計には
  しない。
- マルチマスターの排他機構。バス復旧は`SoftI2c`へ集約済みだが、並行実行を
  許可する設計にはしない。

## 設計方針

### 公開する通常API

7-bitアドレスを受け、アドレスバイトの組立て、START/STOP、各バイトのACK確認、
読出し最終バイトのNACKを`SoftI2c`内部に閉じる。

```rust
pub enum I2cError {
    BusBusy,
    ClockStretchTimeout,
    AddressNack,
    DataNack,
    InvalidTransfer,
}

impl SoftI2c {
    pub fn write(&self, address: u8, bytes: &[u8]) -> Result<(), I2cError>;
    pub fn read(&self, address: u8, buffer: &mut [u8]) -> Result<(), I2cError>;
    pub fn write_read(
        &self,
        address: u8,
        write: &[u8],
        read: &mut [u8],
    ) -> Result<(), I2cError>;
}
```

- `write`はSTART → `address + W` → `bytes` → STOP。
- `read`はSTART → `address + R` → `buffer.len()`バイト → STOP。最後の1バイトだけ
  NACKする。
- `write_read`はSTART → `address + W` → `write` → repeated START → `address + R` →
  `read` → STOP。レジスタを指定して連続読出しする一般的なI2Cデバイスに使う。
- 空の書込み／読出しバッファは、曖昧なアドレスだけの操作を発生させず
  `InvalidTransfer`とする。アドレス応答の確認が必要になった場合は、用途を明示した
  `probe(address)`を別途追加する（今回の移行では不要）。
- 現行の`bool`/`Option`を単純に置き換えるのではなく、アドレスNACKとデータNACK、
  SCL待機タイムアウトを区別する。ログ表示の要否は呼び出し側ごとに判断する。

`stop`時にSCLが解放できない場合も観測可能にするかは、既存の`stop`がエラーを
返さない点を踏まえて実装時に決める。少なくとも主操作が失敗した場合は、その
エラーをSTOP失敗で上書きしない。

### 可変長用のクロージャAPI

I2Cにはスレーブが読出し終端を通知する仕組みがなく、マスターが各バイトの後に
ACK（継続）またはNACK（終了）を決める。従って、先頭の長さバイトを読んでから
後続長を決める操作は、固定長の`write_read`だけでは表せない。

この用途には、START〜STOPの所有権を保ったまま逐次操作を許可するAPIを追加する。

```rust
pub struct I2cTransaction<'a> { /* SoftI2cへの一時的な借用と状態 */ }

impl SoftI2c {
    pub fn transaction<T>(
        &self,
        operation: impl FnOnce(&mut I2cTransaction<'_>) -> Result<T, I2cError>,
    ) -> Result<T, I2cError>;
}

impl I2cTransaction<'_> {
    pub fn start_write(&mut self, address: u8) -> Result<(), I2cError>;
    pub fn start_read(&mut self, address: u8) -> Result<(), I2cError>;
    pub fn restart_write(&mut self, address: u8) -> Result<(), I2cError>;
    pub fn restart_read(&mut self, address: u8) -> Result<(), I2cError>;
    pub fn write_byte(&mut self, byte: u8) -> Result<(), I2cError>;
    pub fn write_all(&mut self, bytes: &[u8]) -> Result<(), I2cError>;
    pub fn read_byte(&mut self, acknowledge: bool) -> Result<u8, I2cError>;
}
```

`transaction`はクロージャの`Ok`/`Err`を問わず、開始済みであれば最後に一度だけ
STOPを送る。`I2cTransaction`の寿命をクロージャ内に限定することで、呼び出し元が
STOPを忘れたり、途中状態を次の処理へ持ち出したりできないようにする。

想定例は次のとおりである。

```rust
bus.transaction(|tx| {
    tx.start_write(address)?;
    tx.write_byte(command)?;
    tx.restart_read(address)?;

    let length = tx.read_byte(true)?; // 後続を読むのでACK
    for index in 0..length {
        let last = index + 1 == length;
        let byte = tx.read_byte(!last)?;
        // byteを利用する
    }
    Ok(())
})?;
```

今回の既存デバイスに可変長読出しは無い。したがって通常APIが主経路であり、
クロージャAPIは将来のFIFOやブロック転送を安全に実装するための限定的な低レベル
入口とする。`start`/`stop`/素のアドレスバイト送信を再び`SoftI2c`の公開APIにはしない。

### 内部実装の責務

- 現在のビット操作は、非公開の`start_condition`、`stop_condition`、
  `write_raw_byte`、`read_raw_byte`などへ移す。信号タイミングは変更しない。
- `start_write`は未開始トランザクションでのみSTARTを送る。
  `restart_*`は開始済みでのみrepeated STARTを送る。誤った順序は
  `InvalidTransfer`で失敗させる。
- アドレス送信時のNACKを`AddressNack`、以後の書込みデータのNACKを`DataNack`に
  対応付ける。読出し中のSCL待機失敗は`ClockStretchTimeout`とする。
- バス開始時にSDAがHighにならない場合は`BusBusy`、SCL待機のタイムアウトは
  `ClockStretchTimeout`とする。現在の`start() -> false`より原因を細分化するが、
  調停検出を新たに主張しない。
- GPIOのオープンドレイン設定と9クロック＋STOPの復旧は`SoftI2c::initialize()`／
  `recover()`に閉じる。`BOARD_BUS`と`CARDKB_BUS`は物理バスごとに一つだけ持ち、
  初期化関数が成功時に一度だけこれを実行する。デバイスドライバはGPIO設定や
  復旧パルスを直接操作しない。

## 実装ステージ

### Stage 0: 現状動作の固定 ✅

- `SoftI2c`の現在の波形・遅延値を変更しないことを確認する。特にrepeated START、
  読出し最終バイトのNACK、SCLクロックストレッチ待機の順序を記録する。
- 各呼び出し箇所を、単純書込み・単純読出し・書込み後読出し・低レベル連続書込みに
  分類する。
- 変更前に利用可能な実機で、LCD起動、CardKB入力、タッチ、USB VBUS操作、
  `axistest`を確認し、変更後の比較対象にする。

### Stage 1: エラー型と非公開プリミティブを導入 ✅

- `src/i2c.rs`に`I2cError`を追加する。
- 既存の公開`start`/`stop`/`write_byte`/`read_byte`を内部プリミティブへ改名して
  非公開化する。この段階では全呼び出し側を同じコミットで移行し、部分的に古いAPIを
  残さない。
- SCL待機失敗、開始不能、書込みACK不在を`I2cError`へ変換するヘルパーを作る。
- GPIO設定・バス復旧の呼び出し元を`i2c.rs`へ一本化する。

**完了条件:** `src/i2c.rs`外から、START/STOPやR/Wビットを直接組み立てる通常の
データ転送コードが残っていない状態にできる基盤が整う。

### Stage 2: 固定長の通常APIを実装 ✅

- `write`、`read`、`write_read`を実装する。
- 各メソッドが、途中のNACKやタイムアウトを含むすべての戻り経路でSTOPを実行することを
  コードレビューで確認する。
- `write_read`のrepeated STARTは、STOPを挟まない現在と同じ信号列にする。
- 空バッファと範囲外アドレス（`address >= 0x80`）を`InvalidTransfer`として扱う。
- ハードウェア非依存部分（アドレス検証、空バッファの拒否、最終バイトだけNACKにする
  分岐）は、可能なら副作用を記録する小さなテスト用バックエンドに切り出して単体テスト
  する。GPIO直結部の実機検証をこのテストで代替しない。

**完了条件:** 固定長のI2C操作に対し、呼び出し側がSTART/STOP/RWビット/最終NACKを
記述する必要がない。

### Stage 3: `I2cTransaction`と後始末保証を実装 ✅

- `SoftI2c::transaction`と`I2cTransaction`を追加する。
- `start_write`、`restart_write`、`restart_read`、`write_byte`、`write_all`、
  `read_byte(acknowledge)`を実装する。
- 未開始での`restart_*`、開始済みでの`start_write`、7-bit範囲外アドレスなどの
  不正順序を`InvalidTransfer`として扱う。
- クロージャが`Err`を返した場合でもSTOPが一回だけ送られることを、テスト用
  バックエンドまたは操作ログで確認する。操作成功後のSTOPも同じ経路で確認する。
- 既存コードに可変長読出し対象は無いため、この段階で無理に呼び出し側を
  クロージャAPIへ移行しない。

**完了条件:** 先頭データを見てACK/NACKと後続長を決めるI2C読出しを、STOP漏れなく
一つのトランザクションで記述できる。

### Stage 4: 既存デバイスドライバを移行 ✅

移行時は外部に見える戻り値（例: `Option<u8>`、`bool`、既存の初期化エラー）を
維持し、内部でのみ`I2cError`を変換する。詳細エラーのログ利用は回帰を避けるため
後続の小変更としてよい。

| ファイル | 置換内容 |
|---|---|
| `src/cardkb.rs` | `read(CARDKB_ADDRESS, &mut [0; 1])`へ移行する。アイドル値`0`と通信エラーの双方を既存どおり`None`へ変換する。 |
| `src/lcd.rs` | PI4IOE1のレジスタ書込みを`write(0x43, &[register, value])`へ移行する。 |
| `src/usb/hcd.rs` | PI4IOE2書込みを`write`、1バイト読出しを`write_read`へ移行する。通常のVBUS操作から`recover_bus`を除去し、共有バスの起動時初期化だけを使う。 |
| `src/touch.rs` | 16-bitレジスタ番号を2バイト配列にし、読出しを`write_read`、書込みを`write`へ移行する。 |
| `src/bmi270.rs` | 通常のレジスタ読出しを`write_read`、固定長書込みを`write`へ移行する。ファームウェアの`register + chunk`は最大17バイトのスタック配列を使うか、`transaction`の`write_byte` + `write_all`を使い、ヒープ確保を導入しない。 |

共有ボードI2Cは`main`の起動時に、CardKB I2Cは`InputManager::new`で初期化する。
再接続試行や通常のVBUS操作でGPIO設定・復旧パルスを繰り返さない。

**完了条件:** `rg`で`src/i2c.rs`以外の`\.start(`、`\.stop(`、`\.write_byte(`、
`\.read_byte(`が通常I2C転送として残っていない。残る場合は`I2cTransaction`内の
可変長用途だけであり、理由をコメントする。

### Stage 5: ドキュメントと実機回帰確認 ✅

- `src/i2c.rs`のモジュールdocに、通常は`write`/`read`/`write_read`を使い、
  可変長だけ`transaction`を使う契約を記載する。
- 必要なら`DESIGN.md`のI2C構成説明を、バイト操作ではなくトランザクションAPIを
  提供する記述へ更新する。
- `cargo check`と`cargo build --release`を通す。
- 実機で次を確認する。

| ケース | 期待結果 |
|---|---|
| 通常起動 | LCD初期化、タッチ初期化、USB VBUS有効化が従来どおり成功する |
| CardKB | 未接続時は害なく、接続時は連続キー入力できる |
| タッチ | `paint`と`touchtest`でGT911/ST7121/ST7123の該当機種が読める |
| USB-A | USBキーボードとMSCの列挙・操作中にVBUS制御が回帰しない |
| BMI270 | `axistest`でchip ID、設定転送、連続モーション読出しが成功する |
| エラー系 | CardKB未接続、または対象I2C機器が応答しない場合に、以後のI2C操作が停止しない |

## 受け入れ条件

- 標準的なI2C読出し・書込みを、デバイスドライバがアドレスのR/Wビット、
  START/STOP、最終NACKを意識せず呼べる。
- 書込み後読出しはSTOPを挟まずrepeated STARTを使う。
- 可変長読出しを、先頭バイトを見た後に同一トランザクション内で継続できる。
- どの失敗経路でもバス層がSTOPを試行し、呼び出し側がSTOPを書く必要がない。
- `no_std`を維持し、ヒープ確保やデバイスドライバ間の新しい共有状態を導入しない。
