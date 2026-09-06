工作流说明
==========

两条流水线，以及它们为什么这么设计。

test.yml
--------

推送到 main、对 main 发 PR，或手动触发时运行。

| 任务 | 平台 | 是否必过 | 做什么 |
|---|---|---|---|
| windows | windows-latest | 是 | build / test / clippy |
| android | ubuntu-latest | 是 | 交叉编译 omy-core 到 aarch64 与 armv7，宿主上跑单测 |
| frontend | ubuntu-latest | 是 | 构建文档站、检查 i18n 键 |
| msrv | ubuntu-latest | 否 | 用 1.85 编一遍，核实声明的下限 |
| linux | ubuntu-latest | 否 | build / test / clippy |
| macos | macos-latest | 否 | build / test / clippy |
| result | ubuntu-latest | 是 | 汇总必过任务，供分支保护使用 |

**分支保护只需要勾 `测试结论` 这一项。** 它只看 windows / android /
frontend；未验证平台的红叉不会阻塞合并。

### 为什么 linux / macos 允许失败

项目只在 Windows 与 Android 上实测过。这两个平台的代码路径写了但没跑过，
设成必过会让 main 长期挂红，红叉也就不再有意义。它们现在的作用是持续
提供「还差什么」的信息。

某个平台连续绿且确实有人用过之后，删掉它的 `continue-on-error`，同时改掉
`site/index.md`、`site/en/index.md`、`site/guide/install.md` 里「尚未验证」
的说法。

### 为什么必过任务用 stable 而不是 MSRV

`Cargo.toml` 写着 `rust-version = "1.85"`，但这个下限没有验证过。把没验证过
的版本设成门禁，一旦代码用了 1.85 之后的特性，会在两个必过任务上同时挂，
而原因是假设错了、不是代码坏了。所以 stable 负责门禁，`msrv` 任务单独去
核实那个声明；等它稳定为绿再考虑提升。

### 为什么没有 cargo fmt --check

实测有 89 个文件、649 处不符合 rustfmt 默认风格——本仓库的中文注释和对齐
是手写的，rustfmt 会重排。加这个门禁会让流水线从第一次运行就红，唯一的
修法是全仓库重排格式，那是独立的决定。要引入的话，先单独提一次全仓库
fmt，再加这一步。

release.yml
-----------

两个动作，触发条件不同：

| 想做的事 | 怎么触发 |
|---|---|
| 出 GitHub Release（含各平台产物） | 推 `v*` 标签，例如 `git tag v0.0.1 && git push origin v0.0.1` |
| 发布到 crates.io | 提交信息里含 `[publish]` |
| 手动发布到 crates.io | Actions 页面手动触发并勾选 |

两者独立，可同时发生（推标签且该提交信息含 `[publish]`）。

### 为什么 crates.io 要单独的标记

**crates.io 上的版本发布后无法删除**，只能 yank，而且版本号不能复用。
GitHub Release 删了可以重发，registry 不行。所以上传 registry 必须由提交
信息里的显式标记触发，不能作为推标签的副作用顺带发生。

真正上传前会先跑一次 `cargo publish --workspace --dry-run`。

### 发布顺序

用 `cargo publish --workspace`，由 cargo 自己算依赖顺序并等每个包在 registry
上可见后再发下一个：

    omy-core → omy-media → omy-net → omy-cli

手写顺序加 `sleep` 是旧做法，cargo 1.98 已内置处理。

`omy-gui` 不发布到 crates.io，在它的 `Cargo.toml` 里标了 `publish = false`：
它的 `build.rs` 在前端产物缺失时会去跑包管理器，而 `dist/` 不入库，所以
registry 上的包对没有 Node 工具链的人是编不过的。GUI 通过 Release 里的
安装包与 APK 分发。

### 需要配置的 secret

| 名称 | 用途 |
|---|---|
| `CARGO_REGISTRY_TOKEN` | 发布到 crates.io。在 crates.io 的 Account Settings 生成 |

`GITHUB_TOKEN` 由 Actions 自动提供，不用配。

### APK 未签名

签名密钥不进仓库，也不由 CI 代持，所以流水线产出的是未签名 APK，安装时
需要允许未知来源。要正式签名版就本地签，或另配 secrets 后在
`build-android` 里加签名步骤。

首次发布到 crates.io
--------------------

四个包都还没上架。首次发布只要一次带 `[publish]` 的提交即可，`--workspace`
会按依赖顺序处理。已在本地用 `--dry-run` 验证过顺序可行。
