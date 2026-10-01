//! 对真实 KeePassXC 的人工验证探针。
//!
//! 只接受仓库测试库，固定秘密也是公开测试数据。关联文件写到调用者指定的
//! 临时路径，不得提交；真实产品通过系统凭据库保存 association key。

use omy_password_manager::PasswordManager as _;
use omy_password_manager::keepassxc::{Association, Client, DEFAULT_NAMESPACE, ProxyTransport};
use std::fs::OpenOptions;
use std::io::Write as _;
use std::path::Path;
use zeroize::Zeroizing;

const SAMPLE_LABEL: &str = "测试主密钥";
const SAMPLE_SECRET: &str = "omy1_AAECAwQFBgcICQoLDA0ODxAREhMUFRYXGBkaGxwdHh8";

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args().skip(1);
    let command = args
        .next()
        .ok_or("用法：keepassxc_probe pair|verify|create <association.json>")?;
    let association_path = args
        .next()
        .ok_or("缺少 association.json 路径（应放在临时目录）")?;

    let transport = ProxyTransport::spawn(Path::new("/usr/bin/keepassxc-proxy"))?;
    let mut client = Client::connect(transport)?;

    match command.as_str() {
        "pair" => {
            let info = client.database_info(&[], true)?;
            println!("database={} keepassxc={}", info.hash, info.version);
            let association = client.associate()?;
            let mut options = OpenOptions::new();
            options.write(true).create(true).truncate(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt as _;
                options.mode(0o600);
            }
            let mut file = options.open(&association_path)?;
            let serialized = Zeroizing::new(serde_json::to_vec_pretty(&association)?);
            file.write_all(&serialized)?;
            println!("paired={}", association.id);
        }
        "verify" => {
            let serialized = Zeroizing::new(std::fs::read(&association_path)?);
            let association: Association = serde_json::from_slice(&serialized)?;
            client.resume(association)?;
            let entries = client.list(DEFAULT_NAMESPACE)?;
            let entry = entries
                .iter()
                .find(|entry| entry.login == SAMPLE_LABEL)
                .ok_or("没有找到样例同步密钥")?;
            if entry.secret.expose() != SAMPLE_SECRET {
                return Err("样例条目密码与 fixture 说明不一致".into());
            }

            // 不只比较字符串：让取回的秘密真正走一次 omy 加解密链路，防止
            // “协议字段取对了、业务接线却用错值”的假通过。
            let salt = core::array::from_fn(|i| (i as u8).wrapping_mul(11) ^ 0x31);
            let params = omy_core::crypto::Argon2Params::TEST_WEAK;
            let kek = omy_core::crypto::Kek::from_password(entry.secret.as_bytes(), &salt, params)?;
            let options = omy_core::file::EncryptOptions {
                filename: Some(String::from("keepassxc-probe.txt")),
                argon2: params,
                ..Default::default()
            };
            let encrypted = omy_core::file::encrypt(
                b"KeePassXC bridge reached omy-core",
                &[kek.duplicate()],
                &salt,
                &options,
                &omy_core::file::RandomMaterial::generate(),
            )?;
            let opened = omy_core::file::open(&encrypted.bytes, &[kek])?;
            let plaintext = opened.decrypt_all(&encrypted.bytes)?;
            if plaintext != b"KeePassXC bridge reached omy-core" {
                return Err("真实加解密往返内容不一致".into());
            }
            println!(
                "verified={} encrypted_bytes={}",
                entry.login,
                encrypted.bytes.len()
            );
        }
        "create" => {
            let serialized = Zeroizing::new(std::fs::read(&association_path)?);
            let association: Association = serde_json::from_slice(&serialized)?;
            client.resume(association)?;
            let label = args.next().unwrap_or_else(|| String::from("探针生成密钥"));
            let secret = omy_password_manager::generate_sync_secret();
            let id = client.create(DEFAULT_NAMESPACE, &label, &secret)?;
            println!("created={}", id.unwrap_or_default());
        }
        _ => return Err("未知命令，只能是 pair、verify 或 create".into()),
    }
    Ok(())
}
