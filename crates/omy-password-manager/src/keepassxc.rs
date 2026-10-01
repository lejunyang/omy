//! KeePassXC-Browser 本地协议 provider。
//!
//! 直接启动 KeePassXC 自带的 `keepassxc-proxy`，但不依赖浏览器扩展。proxy
//! 只负责把 Native Messaging 的 stdin/stdout 转到 KeePassXC 本地 socket；
//! 凭据内容在本模块与 KeePassXC 之间另有一层 NaCl box 加密。

use crate::{
    Capabilities, Credential, CredentialSecret, Error, PasswordManager, ProviderKind, Result,
};
use base64::Engine as _;
use crypto_box::aead::Aead as _;
use crypto_box::{PublicKey, SalsaBox, SecretKey};
use rand_core::RngCore as _;
use serde_json::{Map, Value, json};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};

/// omy 密钥条目使用的 URL 命名空间。
///
/// 这里不发生网络访问；KeePassXC 只取 host 做条目匹配。
pub const DEFAULT_NAMESPACE: &str = "https://credentials.omy.app/";
/// 建议给自动创建条目使用的分组。
pub const DEFAULT_GROUP: &str = "omy";

const ACTION_ASSOCIATE: &str = "associate";
const ACTION_CHANGE_PUBLIC_KEYS: &str = "change-public-keys";
const ACTION_CREATE_GROUP: &str = "create-new-group";
const ACTION_GET_DATABASE_HASH: &str = "get-databasehash";
const ACTION_GET_GROUPS: &str = "get-database-groups";
const ACTION_GET_LOGINS: &str = "get-logins";
const ACTION_SET_LOGIN: &str = "set-login";
const ACTION_TEST_ASSOCIATE: &str = "test-associate";
const EVENT_DATABASE_LOCKED: &str = "database-locked";
const EVENT_DATABASE_UNLOCKED: &str = "database-unlocked";

/// KeePassXC socket 的消息上限。超过它时必须在分配前拒绝。
// 与 KeePassXC BrowserShared::NATIVEMSG_MAX_LENGTH 完全一致。放宽到自定的
// 4 MiB 没有意义：proxy 会先拒绝，只会让本端多分配内存并报错得更晚。
const MAX_FRAME_LEN: usize = 655_360;
const NONCE_LEN: usize = 24;
const KEY_LEN: usize = 32;

/// 一个 KDBX 与 omy 客户端之间的长期关联。
///
/// `key` 是恢复授权的 bearer credential，不应进入普通配置或日志。
#[derive(Clone, serde::Serialize, serde::Deserialize)]
pub struct Association {
    /// 数据库 root UUID 的哈希。
    pub database_hash: String,
    /// 用户在 KeePassXC 关联窗口中确认的名字。
    pub id: String,
    /// KeePassXC 保存并在恢复关联时比对的 key。
    pub key: String,
}

impl core::fmt::Debug for Association {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("Association")
            .field("database_hash", &self.database_hash)
            .field("id", &self.id)
            .field("key", &"<redacted>")
            .finish()
    }
}

/// 当前 KeePassXC 数据库信息。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DatabaseInfo {
    /// 数据库稳定哈希。
    pub hash: String,
    /// KeePassXC 报告的版本。
    pub version: String,
}

/// KeePassXC 分组。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Group {
    /// 显示名。
    pub name: String,
    /// KeePassXC UUID（十六进制）。
    pub uuid: String,
    /// 子分组。
    pub children: Vec<Group>,
}

/// 可交换单个 JSON 请求的传输层。
///
/// 抽象这一层是为了让协议测试无需启动真实 KeePassXC，也给未来不经 proxy 的
/// 平台实现留边界。产品路径使用 [`ProxyTransport`]。
pub trait Transport: Send {
    /// 发送一个请求并等待同 action 的响应。
    ///
    /// # Errors
    ///
    /// 连接断开、帧过长或 JSON 无效时返回。
    fn exchange(&mut self, request: &Value) -> Result<Value>;
}

/// 通过 `keepassxc-proxy` 子进程通信。
pub struct ProxyTransport {
    child: Child,
    stdin: Option<ChildStdin>,
    stdout: ChildStdout,
}

impl ProxyTransport {
    /// 启动指定的 `keepassxc-proxy`。
    ///
    /// # Errors
    ///
    /// 路径不是文件、进程无法启动或拿不到管道时返回。
    pub fn spawn(path: &Path) -> Result<Self> {
        if !path.is_file() {
            return Err(Error::Unavailable(format!(
                "找不到 keepassxc-proxy：{}",
                path.display()
            )));
        }
        let mut child = Command::new(path)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            // proxy 的诊断不能混进协议 stdout；stderr 也不应把内部路径带进
            // omy 日志。状态错误由协议和进程退出码返回。
            .stderr(Stdio::null())
            .spawn()
            .map_err(|e| Error::Unavailable(format!("无法启动 {}：{e}", path.display())))?;
        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| Error::Transport(String::from("keepassxc-proxy 没有 stdin")))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| Error::Transport(String::from("keepassxc-proxy 没有 stdout")))?;
        Ok(Self {
            child,
            stdin: Some(stdin),
            stdout,
        })
    }

    fn write_frame(&mut self, value: &Value) -> Result<()> {
        let stdin = self
            .stdin
            .as_mut()
            .ok_or_else(|| Error::Transport(String::from("keepassxc-proxy 已关闭")))?;
        write_native_message(stdin, value)
    }

    fn read_frame(&mut self) -> Result<Value> {
        read_native_message(&mut self.stdout)
    }
}

fn write_native_message(writer: &mut impl std::io::Write, value: &Value) -> Result<()> {
    let bytes = serde_json::to_vec(value)
        .map_err(|e| Error::Protocol(format!("请求 JSON 无法编码：{e}")))?;
    if bytes.len() > MAX_FRAME_LEN {
        return Err(Error::Protocol(format!(
            "请求超过 {MAX_FRAME_LEN} 字节上限"
        )));
    }
    let len = u32::try_from(bytes.len())
        .map_err(|_| Error::Protocol(String::from("请求长度无法表示")))?;
    writer
        .write_all(&len.to_le_bytes())
        .and_then(|()| writer.write_all(&bytes))
        .and_then(|()| writer.flush())
        .map_err(|e| Error::Transport(format!("写入 keepassxc-proxy 失败：{e}")))
}

fn read_native_message(reader: &mut impl std::io::Read) -> Result<Value> {
    let mut len = [0u8; 4];
    reader
        .read_exact(&mut len)
        .map_err(|e| Error::Transport(format!("读取 keepassxc-proxy 长度失败：{e}")))?;
    let len = u32::from_le_bytes(len) as usize;
    if len == 0 || len > MAX_FRAME_LEN {
        return Err(Error::Protocol(format!("响应长度 {len} 不在允许范围内")));
    }
    let mut bytes = vec![0u8; len];
    reader
        .read_exact(&mut bytes)
        .map_err(|e| Error::Transport(format!("读取 keepassxc-proxy 响应失败：{e}")))?;
    serde_json::from_slice(&bytes)
        .map_err(|e| Error::Protocol(format!("KeePassXC 返回的 JSON 无效：{e}")))
}

impl Transport for ProxyTransport {
    fn exchange(&mut self, request: &Value) -> Result<Value> {
        let want = string_field(request, "action")?;
        self.write_frame(request)?;
        // KeePassXC 可能在响应前插入 database-locked/unlocked 事件。串行客户端
        // 只需跳过这两种事件；其它 action 串台必须拒绝，不能把旧响应配给新请求。
        for _ in 0..4 {
            let response = self.read_frame()?;
            let action = string_field(&response, "action")?;
            if action == EVENT_DATABASE_LOCKED || action == EVENT_DATABASE_UNLOCKED {
                continue;
            }
            if action != want {
                return Err(Error::Protocol(format!(
                    "响应 action 不匹配：请求 {want}，收到 {action}"
                )));
            }
            return Ok(response);
        }
        Err(Error::Protocol(String::from(
            "状态事件过多，未收到请求响应",
        )))
    }
}

impl Drop for ProxyTransport {
    fn drop(&mut self) {
        // 先关 stdin，让 proxy 自己因 EOF 退出；若它没有及时退出，只结束我们
        // 启动的这个子进程，不碰用户正在运行的 KeePassXC 主程序。
        let _ = self.stdin.take();
        if matches!(self.child.try_wait(), Ok(None)) {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }
}

/// 查找 `keepassxc-proxy`。
///
/// 显式路径优先；否则查 PATH 和各桌面平台的原生安装位置。Snap/Flatpak 不在
/// 这里猜命令包装方式，找不到时让用户选择路径。
#[must_use]
pub fn discover_proxy(explicit: Option<&Path>) -> Option<PathBuf> {
    if let Some(path) = explicit.filter(|p| p.is_file()) {
        return Some(path.to_path_buf());
    }

    let binary = if cfg!(target_os = "windows") {
        "keepassxc-proxy.exe"
    } else {
        "keepassxc-proxy"
    };
    if let Some(path_var) = std::env::var_os("PATH") {
        for dir in std::env::split_paths(&path_var) {
            let candidate = dir.join(binary);
            if candidate.is_file() {
                return Some(candidate);
            }
        }
    }

    let candidates: &[&str] = if cfg!(target_os = "windows") {
        &[
            r"C:\Program Files\KeePassXC\keepassxc-proxy.exe",
            r"C:\Program Files (x86)\KeePassXC\keepassxc-proxy.exe",
        ]
    } else if cfg!(target_os = "macos") {
        &[
            "/Applications/KeePassXC.app/Contents/MacOS/keepassxc-proxy",
            "/usr/local/bin/keepassxc-proxy",
            "/opt/homebrew/bin/keepassxc-proxy",
        ]
    } else {
        &["/usr/bin/keepassxc-proxy", "/usr/local/bin/keepassxc-proxy"]
    };
    candidates.iter().map(PathBuf::from).find(|p| p.is_file())
}

struct SessionCrypto {
    cipher: SalsaBox,
    public_key: String,
}

impl SessionCrypto {
    fn start() -> (SecretKey, String) {
        let secret = SecretKey::generate(&mut rand_core::OsRng);
        let public =
            base64::engine::general_purpose::STANDARD.encode(secret.public_key().as_bytes());
        (secret, public)
    }

    fn finish(secret: SecretKey, public_key: String, server_key: &str) -> Result<Self> {
        let raw = decode_fixed::<KEY_LEN>(server_key, "KeePassXC 公钥")?;
        let server = PublicKey::from(raw);
        Ok(Self {
            cipher: SalsaBox::new(&server, &secret),
            public_key,
        })
    }

    fn encrypt(&self, message: &Value, nonce: &[u8; NONCE_LEN]) -> Result<String> {
        let plain = serde_json::to_vec(message)
            .map_err(|e| Error::Protocol(format!("内部 JSON 无法编码：{e}")))?;
        let encrypted = self
            .cipher
            .encrypt(nonce.into(), plain.as_ref())
            .map_err(|_| Error::Protocol(String::from("无法加密 KeePassXC 请求")))?;
        Ok(base64::engine::general_purpose::STANDARD.encode(encrypted))
    }

    fn decrypt(&self, message: &str, nonce: &[u8; NONCE_LEN]) -> Result<Value> {
        let encrypted = base64::engine::general_purpose::STANDARD
            .decode(message)
            .map_err(|_| Error::Protocol(String::from("KeePassXC 密文不是合法 base64")))?;
        let plain = self
            .cipher
            .decrypt(nonce.into(), encrypted.as_ref())
            .map_err(|_| Error::Protocol(String::from("KeePassXC 响应认证失败")))?;
        serde_json::from_slice(&plain)
            .map_err(|e| Error::Protocol(format!("KeePassXC 内层 JSON 无效：{e}")))
    }
}

/// KeePassXC-Browser 协议客户端。
pub struct Client<T: Transport> {
    transport: T,
    crypto: SessionCrypto,
    client_id: String,
    database: Option<DatabaseInfo>,
    association: Option<Association>,
}

impl<T: Transport> Client<T> {
    /// 建立传输并完成临时公钥交换。
    ///
    /// # Errors
    ///
    /// proxy 不可用、响应 nonce/公钥无效时返回。
    pub fn connect(mut transport: T) -> Result<Self> {
        let (secret_key, public_key) = SessionCrypto::start();
        let nonce = random_nonce();
        let client_id = random_id();
        let request = json!({
            "action": ACTION_CHANGE_PUBLIC_KEYS,
            "publicKey": public_key,
            "nonce": encode(&nonce),
            "clientID": client_id,
        });
        let response = transport.exchange(&request)?;
        check_plain_success(&response)?;
        let expected = increment_nonce(nonce);
        if string_field(&response, "nonce")? != encode(&expected) {
            return Err(Error::Protocol(String::from("公钥交换响应 nonce 不匹配")));
        }
        let server_key = string_field(&response, "publicKey")?;
        let crypto = SessionCrypto::finish(secret_key, public_key, server_key)?;
        Ok(Self {
            transport,
            crypto,
            client_id,
            database: None,
            association: None,
        })
    }

    /// 查询当前数据库。`known` 用于 KeePassXC 在“搜索全部数据库”时识别已关联库。
    ///
    /// `trigger_unlock` 为 true 时，若设置允许，KeePassXC 会把解锁窗口带到前台；
    /// 当前请求仍可能返回 [`Error::Locked`]，调用方应在用户完成后重试。
    pub fn database_info(
        &mut self,
        known: &[Association],
        trigger_unlock: bool,
    ) -> Result<DatabaseInfo> {
        let keys = known
            .iter()
            .map(|a| json!({ "id": a.id, "key": a.key }))
            .collect::<Vec<_>>();
        let response = self.send(
            ACTION_GET_DATABASE_HASH,
            json!({ "connectedKeys": keys }),
            trigger_unlock,
        )?;
        let info = DatabaseInfo {
            hash: string_field(&response, "hash")?.to_owned(),
            version: response
                .get("version")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned(),
        };
        if info.hash.is_empty() {
            return Err(Error::Protocol(String::from(
                "KeePassXC 没有返回数据库 hash",
            )));
        }
        self.database = Some(info.clone());
        Ok(info)
    }

    /// 请求用户授权当前 omy 与当前数据库关联。
    ///
    /// # Errors
    ///
    /// 数据库未查询、用户拒绝或协议失败时返回。
    pub fn associate(&mut self) -> Result<Association> {
        let database_hash = self
            .database
            .as_ref()
            .map(|d| d.hash.clone())
            .ok_or(Error::Locked)?;
        let id_secret = SecretKey::generate(&mut rand_core::OsRng);
        let id_key =
            base64::engine::general_purpose::STANDARD.encode(id_secret.public_key().as_bytes());
        let public_key = self.crypto.public_key.clone();
        let response = self.send(
            ACTION_ASSOCIATE,
            json!({ "key": public_key, "idKey": id_key }),
            true,
        )?;
        let association = Association {
            database_hash,
            id: string_field(&response, "id")?.to_owned(),
            key: id_key,
        };
        if association.id.is_empty() {
            return Err(Error::Protocol(String::from("KeePassXC 没有返回关联 id")));
        }
        self.association = Some(association.clone());
        Ok(association)
    }

    /// 恢复并验证已有关联。
    ///
    /// # Errors
    ///
    /// 当前数据库不匹配或 KeePassXC 已撤销关联时返回。
    pub fn resume(&mut self, association: Association) -> Result<DatabaseInfo> {
        let info = self.database_info(core::slice::from_ref(&association), true)?;
        if info.hash != association.database_hash {
            return Err(Error::NotAssociated);
        }
        let response = self.send(
            ACTION_TEST_ASSOCIATE,
            json!({ "id": association.id, "key": association.key }),
            false,
        )?;
        if string_field(&response, "id")? != association.id {
            return Err(Error::NotAssociated);
        }
        self.association = Some(association);
        Ok(info)
    }

    /// 列出与 URL 命名空间匹配的凭据。
    pub fn get_logins(&mut self, namespace: &str) -> Result<Vec<Credential>> {
        let association = self.association()?.clone();
        let response = self.send(
            ACTION_GET_LOGINS,
            json!({
                "id": association.id,
                "url": namespace,
                "submitUrl": namespace,
                "keys": [{ "id": association.id, "key": association.key }],
            }),
            true,
        )?;
        let raw = response
            .get("entries")
            .and_then(Value::as_array)
            .ok_or_else(|| Error::Protocol(String::from("get-logins 缺少 entries")))?;
        let mut out = Vec::with_capacity(raw.len());
        for item in raw {
            let id = string_field(item, "uuid")?.to_owned();
            let login = string_field(item, "login")?.to_owned();
            let name = item
                .get("name")
                .and_then(Value::as_str)
                .filter(|v| !v.is_empty())
                .unwrap_or(&login)
                .to_owned();
            let group = item
                .get("group")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned();
            let password = string_field(item, "password")?.to_owned();
            out.push(Credential {
                id,
                name,
                login,
                group,
                secret: CredentialSecret::new(password),
            });
        }
        if out.is_empty() {
            return Err(Error::NotFound);
        }
        Ok(out)
    }

    /// 创建或更新 KeePassXC 条目。
    ///
    /// `entry_id` 为空时创建；有值时 KeePassXC 默认会弹出更新确认。
    pub fn set_login(
        &mut self,
        namespace: &str,
        label: &str,
        secret: &CredentialSecret,
        entry_id: Option<&str>,
        group: Option<&Group>,
    ) -> Result<()> {
        let association = self.association()?.clone();
        let mut fields = Map::new();
        fields.insert(String::from("id"), Value::String(association.id));
        fields.insert(String::from("login"), Value::String(label.to_owned()));
        fields.insert(
            String::from("password"),
            Value::String(secret.expose().to_owned()),
        );
        fields.insert(String::from("url"), Value::String(namespace.to_owned()));
        fields.insert(
            String::from("submitUrl"),
            Value::String(namespace.to_owned()),
        );
        if let Some(id) = entry_id {
            fields.insert(String::from("uuid"), Value::String(id.to_owned()));
        }
        if let Some(group) = group {
            fields.insert(String::from("group"), Value::String(group.name.clone()));
            fields.insert(String::from("groupUuid"), Value::String(group.uuid.clone()));
        }
        let response = self.send(ACTION_SET_LOGIN, Value::Object(fields), true)?;
        if response
            .get("error")
            .and_then(Value::as_str)
            .is_some_and(|v| v == "success")
        {
            Ok(())
        } else {
            Err(Error::Protocol(String::from(
                "KeePassXC 未确认条目写入成功",
            )))
        }
    }

    /// 读取分组树。
    pub fn groups(&mut self) -> Result<Vec<Group>> {
        let response = self.send(ACTION_GET_GROUPS, json!({}), true)?;
        let raw = response
            .get("groups")
            .and_then(|v| v.get("groups"))
            .and_then(Value::as_array)
            .or_else(|| response.get("groups").and_then(Value::as_array))
            .ok_or_else(|| Error::Protocol(String::from("KeePassXC 分组响应格式无效")))?;
        raw.iter().map(parse_group).collect()
    }

    /// 创建分组；若已存在，KeePassXC 直接返回现有分组。
    pub fn create_group(&mut self, name: &str) -> Result<Group> {
        let response = self.send(ACTION_CREATE_GROUP, json!({ "groupName": name }), true)?;
        Ok(Group {
            name: string_field(&response, "name")?.to_owned(),
            uuid: string_field(&response, "uuid")?.to_owned(),
            children: Vec::new(),
        })
    }

    fn association(&self) -> Result<&Association> {
        self.association.as_ref().ok_or(Error::NotAssociated)
    }

    fn send(&mut self, action: &str, fields: Value, trigger_unlock: bool) -> Result<Value> {
        let mut inner = match fields {
            Value::Object(map) => map,
            _ => return Err(Error::Protocol(String::from("请求字段必须是对象"))),
        };
        inner.insert(String::from("action"), Value::String(action.to_owned()));
        let nonce = random_nonce();
        let encrypted = self.crypto.encrypt(&Value::Object(inner), &nonce)?;
        let mut outer = json!({
            "action": action,
            "message": encrypted,
            "nonce": encode(&nonce),
            "clientID": self.client_id,
        });
        if trigger_unlock && let Some(map) = outer.as_object_mut() {
            map.insert(
                String::from("triggerUnlock"),
                Value::String(String::from("true")),
            );
        }
        let response = self.transport.exchange(&outer)?;
        if response.get("errorCode").is_some() {
            return Err(map_provider_error(&response));
        }
        let response_nonce =
            decode_fixed::<NONCE_LEN>(string_field(&response, "nonce")?, "响应 nonce")?;
        if response_nonce != increment_nonce(nonce) {
            return Err(Error::Protocol(String::from("响应 nonce 不匹配")));
        }
        let encrypted_response = string_field(&response, "message")?;
        let inner = self.crypto.decrypt(encrypted_response, &response_nonce)?;
        if string_field(&inner, "nonce")? != encode(&response_nonce) {
            return Err(Error::Protocol(String::from("内层响应 nonce 不匹配")));
        }
        check_plain_success(&inner)?;
        Ok(inner)
    }
}

impl Client<ProxyTransport> {
    /// 查找并启动 proxy，再完成协议握手。
    pub fn connect_proxy(explicit: Option<&Path>) -> Result<Self> {
        let path = discover_proxy(explicit).ok_or_else(|| {
            Error::Unavailable(String::from(
                "未找到 keepassxc-proxy，请安装 KeePassXC 或指定路径",
            ))
        })?;
        Self::connect(ProxyTransport::spawn(&path)?)
    }
}

impl<T: Transport> PasswordManager for Client<T> {
    fn kind(&self) -> ProviderKind {
        ProviderKind::KeePassXc
    }

    fn capabilities(&self) -> Capabilities {
        Capabilities {
            select: true,
            create: true,
            update: true,
            passkey_prf: false,
        }
    }

    fn list(&mut self, namespace: &str) -> Result<Vec<Credential>> {
        self.get_logins(namespace)
    }

    fn create(
        &mut self,
        namespace: &str,
        label: &str,
        secret: &CredentialSecret,
    ) -> Result<Option<String>> {
        self.set_login(namespace, label, secret, None, None)?;
        // set-login 的创建响应不含 UUID。回读不仅拿到 UUID，也防止“响应成功但
        // 实际没有写入”的上游回归；没有精确匹配就不能拿这把钥匙去加密。
        let entries = self.get_logins(namespace)?;
        entries
            .iter()
            .find(|e| e.login == label && e.secret.expose() == secret.expose())
            .map(|e| Some(e.id.clone()))
            .ok_or_else(|| {
                Error::Protocol(String::from("KeePassXC 报告写入成功，但无法回读新条目"))
            })
    }
}

fn parse_group(value: &Value) -> Result<Group> {
    let children = value
        .get("children")
        .and_then(Value::as_array)
        .map(|items| items.iter().map(parse_group).collect())
        .transpose()?
        .unwrap_or_default();
    Ok(Group {
        name: string_field(value, "name")?.to_owned(),
        uuid: string_field(value, "uuid")?.to_owned(),
        children,
    })
}

fn random_nonce() -> [u8; NONCE_LEN] {
    let mut nonce = [0u8; NONCE_LEN];
    rand_core::OsRng.fill_bytes(&mut nonce);
    nonce
}

fn random_id() -> String {
    let mut id = [0u8; NONCE_LEN];
    rand_core::OsRng.fill_bytes(&mut id);
    encode(&id)
}

fn increment_nonce(mut nonce: [u8; NONCE_LEN]) -> [u8; NONCE_LEN] {
    let mut carry = 1u16;
    for byte in &mut nonce {
        let value = u16::from(*byte).saturating_add(carry);
        *byte = value as u8;
        carry = value >> 8;
    }
    nonce
}

fn encode(bytes: &[u8]) -> String {
    base64::engine::general_purpose::STANDARD.encode(bytes)
}

fn decode_fixed<const N: usize>(encoded: &str, what: &str) -> Result<[u8; N]> {
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(encoded)
        .map_err(|_| Error::Protocol(format!("{what} 不是合法 base64")))?;
    bytes
        .try_into()
        .map_err(|_| Error::Protocol(format!("{what} 长度必须为 {N} 字节")))
}

fn string_field<'a>(value: &'a Value, name: &str) -> Result<&'a str> {
    value
        .get(name)
        .and_then(Value::as_str)
        .ok_or_else(|| Error::Protocol(format!("响应缺少字符串字段 {name}")))
}

fn check_plain_success(response: &Value) -> Result<()> {
    if response
        .get("success")
        .and_then(Value::as_str)
        .is_some_and(|success| success == "true")
    {
        return Ok(());
    }
    if response.get("errorCode").is_some() {
        return Err(map_provider_error(response));
    }
    Err(Error::Protocol(String::from("KeePassXC 响应没有成功标记")))
}

fn map_provider_error(response: &Value) -> Error {
    let code = response
        .get("errorCode")
        .and_then(|v| {
            v.as_i64()
                .or_else(|| v.as_str().and_then(|s| s.parse().ok()))
        })
        .and_then(|v| i32::try_from(v).ok())
        .unwrap_or_default();
    let message = response
        .get("error")
        .and_then(Value::as_str)
        .unwrap_or("unknown error")
        .to_owned();
    match code {
        1 => Error::Locked,
        6 => Error::UserCancelled,
        8 | 10 | 11 => Error::NotAssociated,
        15 => Error::NotFound,
        19 => Error::Unsupported(message),
        _ => Error::Provider { code, message },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::VecDeque;

    struct QueueTransport {
        responses: VecDeque<Value>,
    }

    impl Transport for QueueTransport {
        fn exchange(&mut self, _request: &Value) -> Result<Value> {
            self.responses
                .pop_front()
                .ok_or_else(|| Error::Transport(String::from("没有测试响应")))
        }
    }

    #[test]
    fn nonce_increment_matches_libsodium_little_endian_rule() {
        let mut start = [0u8; NONCE_LEN];
        start[0] = 0xff;
        start[1] = 0xff;
        let result = increment_nonce(start);
        // 不这样会怎样：与 KeePassXC 计算出不同 nonce，所有响应都会被当成
        // 篡改；只测普通 +1 抓不到跨字节进位写反的问题。
        assert_eq!(result[0], 0);
        assert_eq!(result[1], 0);
        assert_eq!(result[2], 1);
    }

    #[test]
    fn secret_box_roundtrip_and_tamper_detection() {
        let (left_secret, left_public) = SessionCrypto::start();
        let (right_secret, right_public) = SessionCrypto::start();
        let left = SessionCrypto::finish(left_secret, left_public, &right_public).expect("left");
        let right =
            SessionCrypto::finish(right_secret, right_public, &left.public_key).expect("right");
        let nonce = [7u8; NONCE_LEN];
        let encrypted = left
            .encrypt(&json!({"password":"secret"}), &nonce)
            .expect("encrypt");
        let plain = right.decrypt(&encrypted, &nonce).expect("decrypt");
        // 不这样会怎样：协议层看似连通，但解出的密码字段已损坏，用户只会
        // 得到误导性的“omy 密钥不匹配”。
        assert_eq!(
            plain.get("password").and_then(Value::as_str),
            Some("secret")
        );

        let mut bytes = base64::engine::general_purpose::STANDARD
            .decode(encrypted)
            .expect("base64");
        if let Some(first) = bytes.first_mut() {
            *first ^= 1;
        }
        let tampered = encode(&bytes);
        // 不这样会怎样：本机 socket 上被改过的密码会进入 KDF，而不是在边界拒绝。
        assert!(
            right.decrypt(&tampered, &nonce).is_err(),
            "篡改必须被认证失败抓住"
        );
    }

    #[test]
    fn association_debug_hides_bearer_key() {
        let association = Association {
            database_hash: String::from("db"),
            id: String::from("omy-test"),
            key: String::from("bearer-must-not-leak"),
        };
        let rendered = format!("{association:?}");
        // 不这样会怎样：诊断关联失败时会顺手把可恢复授权的 bearer 写进日志。
        assert!(!rendered.contains("bearer-must-not-leak"));
        assert!(rendered.contains("redacted"));
    }

    #[test]
    fn provider_errors_keep_user_actions_distinct() {
        // 不这样会怎样：数据库锁定、用户取消和无条目都会显示成通信失败，
        // 界面无法决定该重试、安静退出还是提示创建。
        assert!(matches!(
            map_provider_error(&json!({"errorCode":"1","error":"locked"})),
            Error::Locked
        ));
        assert!(matches!(
            map_provider_error(&json!({"errorCode":"6","error":"cancel"})),
            Error::UserCancelled
        ));
        assert!(matches!(
            map_provider_error(&json!({"errorCode":"15","error":"none"})),
            Error::NotFound
        ));
    }

    #[test]
    fn malformed_handshake_is_rejected_before_session_exists() {
        let transport = QueueTransport {
            responses: VecDeque::from([json!({
                "action": ACTION_CHANGE_PUBLIC_KEYS,
                "success": "true",
                "nonce": encode(&[0u8; NONCE_LEN]),
                "publicKey": encode(&[0u8; KEY_LEN]),
            })]),
        };
        // 随机请求 nonce 不可能恰好等于全零响应 nonce。不这样会怎样：中间人
        // 可以重放旧握手响应，让客户端在错误的会话密钥下继续。
        assert!(
            Client::connect(transport).is_err(),
            "错误 nonce 的握手必须失败"
        );
    }

    #[test]
    fn group_tree_parser_preserves_nesting() {
        let group = parse_group(&json!({
            "name":"root",
            "uuid":"aa",
            "children":[{"name":"omy","uuid":"bb","children":[]}]
        }))
        .expect("group");
        // 不这样会怎样：创建条目时会把 group 名和 UUID 配错，密钥落到用户
        // 没有选择的分组里。
        assert_eq!(group.name, "root");
        assert_eq!(group.children.first().map(|g| g.name.as_str()), Some("omy"));
        assert_eq!(group.children.first().map(|g| g.uuid.as_str()), Some("bb"));
    }

    #[test]
    fn native_message_reader_handles_partial_reads() {
        struct OneByteReader(std::io::Cursor<Vec<u8>>);
        impl std::io::Read for OneByteReader {
            fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
                let Some(first) = buf.first_mut() else {
                    return Ok(0);
                };
                self.0.read(core::slice::from_mut(first))
            }
        }

        let expected = json!({"action":"get-logins","success":"true"});
        let mut framed = Vec::new();
        write_native_message(&mut framed, &expected).expect("frame");
        let mut reader = OneByteReader(std::io::Cursor::new(framed));
        let actual = read_native_message(&mut reader).expect("partial read");
        // 不这样会怎样：pipe 把一个 JSON 拆成多次 read 时，客户端会随机报
        // “JSON 损坏”；真实 IPC 不承诺一次 read 返回整帧。
        assert_eq!(actual, expected);
    }

    #[test]
    fn native_message_reader_rejects_oversize_before_allocating() {
        let claimed = u32::try_from(MAX_FRAME_LEN.saturating_add(1)).expect("test bound fits u32");
        let mut reader = std::io::Cursor::new(claimed.to_le_bytes().to_vec());
        let error = read_native_message(&mut reader).expect_err("oversize must fail");
        // 不这样会怎样：同用户恶意进程可只发一个长度头，让 omy 立即申请巨量内存。
        assert!(matches!(error, Error::Protocol(_)));
    }
}
