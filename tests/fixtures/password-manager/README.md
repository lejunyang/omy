# KeePassXC 集成测试库

`keepassxc-sample.kdbx` 只用于自动化和人工联调，里面没有真实凭据。

- 数据库密码：`123456`
- 分组：`omy`
- 条目：`omy/样例同步密钥`
- 用户名：`测试主密钥`
- URL：`https://credentials.omy.app/`
- 条目密码：`omy1_AAECAwQFBgcICQoLDA0ODxAREhMUFRYXGBkaGxwdHh8`

这个弱数据库密码是刻意固定的测试输入，不能照搬到真实 KDBX。固定条目秘密由
字节 `0..31` 编码而来，也只用于验证 KeePassXC 返回值能否真正解锁 `.omy`。

修改 fixture 后至少执行：

```bash
keepassxc-cli show -a Password \
  tests/fixtures/password-manager/keepassxc-sample.kdbx \
  'omy/样例同步密钥'
```

输入 `123456` 后应得到上面的固定条目密码。

这个 fixture 只固定桌面 KeePassXC 可以查询的 URL。KeePassDX 的
`AndroidApp Signature` 必须来自实际安装包的签名，不能提交一个对所有设备都
有效的伪值；Android 真机测试应按站点指南给该条目写入当前安装的应用关联。
