工作流说明
==========

三条流水线，以及它们为什么这么设计。

pages.yml
---------

把 `site/` 部署到 GitHub Pages。只在 main 上、且站点相关文件有改动时触发
（`site/**` 或本工作流自身），也可手动触发。

### 首次启用要手动配一次

仓库 **Settings → Pages → Source 选 `GitHub Actions`**。不配这一项，
部署任务会失败并报没有启用 Pages——报错原文是
`Get Pages site failed ... Not Found`，看起来像 action 坏了，实际只是缺这次设置。

工作流里刻意**不用** `configure-pages` 的 `enablement: true` 去自动启用：
那个参数明确要求「除 `GITHUB_TOKEN` 之外的令牌」（PAT 需 `repo` 或 Pages 写
权限，GitHub App 需 `administration:write`），而这条流水线只用自动提供的
`GITHUB_TOKEN`。加上它并不会生效，只会把「缺一次设置」变成「缺一个 secret」。

站点地址是 `https://lejunyang.github.io/omy/`，与 `site/.vitepress/config.js`
里的 `base: '/omy/'` 对应。**换成自定义域名时两处要一起改**：只改一处会
让页面能打开但 CSS 和 JS 全部 404。

### 为什么和 test.yml 里的站点构建分开

test.yml 的 `frontend` 任务也构建站点，但目的是「站点没坏」（死链、i18n
缺键），要跑在 PR 上；部署只该在 main 上发生。混在一条里得给部署步骤套
一堆条件，而且 PR 的构建也会带上 Pages 的写权限——那是不必要的权限扩大。

test.yml
--------

推送到 main、对 main 发 PR，或手动触发时运行。

| 任务 | 平台 | 是否必过 | 做什么 |
|---|---|---|---|
| windows | windows-latest | 是 | build / test / clippy |
| android | ubuntu-latest | 是 | 交叉编译 omy-core 到 aarch64 与 armv7，宿主上跑单测 |
| frontend | ubuntu-latest | 是 | 构建文档站、检查 i18n 键 |
| msrv | ubuntu-latest | 否 | 用 1.88 编一遍，核实声明的下限 |
| linux | ubuntu-latest | 否 | build / test / clippy |
| macos | macos-latest | 是 | build / test / clippy |
| result | ubuntu-latest | 是 | 汇总必过任务，供分支保护使用 |

**分支保护只需要勾 `测试结论` 这一项。** 它只看 windows / android /
macos / frontend；未验证平台（linux）的红叉不会阻塞合并。

`omy-gui` 的远程位置单元测试与 `omy-remote` 的 session 单元测试使用进程内
测试密钥，不访问系统凭据库。凭据库本身的真实读写仍由 `omy-secret` 的专门
测试覆盖。两层不能重复测同一件事：macOS runner 没有可交互桌面，并行访问
同一个 Keychain service/id 会等待授权窗口，表现为若干纯编排测试同时卡到
6 小时，而不是业务逻辑失败。

### 每个编 omy-gui 的任务都要装 Node 与 pnpm

`omy-gui` 的 `build.rs` 在 `dist/` 缺失时会调包管理器构建前端，而 `dist/`
不入库——所以**凡是会编到 omy-gui 的任务**（windows / linux / macos / msrv，
以及 release 里的各构建任务）都必须先装 Node 与 pnpm。

`cargo tauri android build` 是例外：Tauri CLI 会在 Rust `build.rs` 获得执行机会前
先校验 `frontendDist`。因此 `osdk.toml` 的 `android-release-build` task 把
`pnpm install --frozen-lockfile`、`pnpm build` 与 Tauri 构建按顺序封装在一起；发布
流水线只调用这个 task。只安装 pnpm 就直接运行 Tauri 仍会报
`Unable to find your web assets`。

runner 镜像自带 node 与 npm，但**不带 pnpm**。缺它时 `build.rs` 直接 panic，
而报错长得跟 Rust 毫无关系：

- Windows：`'pnpm' is not recognized as an internal or external command`（退出码 1）
- Linux / macOS：`无法执行 pnpm（No such file or directory）`（退出码 101）

`build.rs` 按 `frontend/` 下的 lockfile 选包管理器，看到 `pnpm-lock.yaml`
就只会调 pnpm，**不会**退回 npm 或 bun。所以装了 bun 也不能替代
（release 的 build-android 任务一度只装了 bun，就是这个坑）。

pnpm 版本固定为 `9.15.1`（与本机一致），不写 `latest`：
`frontend/pnpm-lock.yaml` 是 `lockfileVersion 9.0`，pnpm 跨大版本会改 lockfile
格式与默认行为，让 CI 装出一棵和本地不同的依赖树。另外仓库根目录没有
`package.json`，`pnpm/action-setup` 读不到 `packageManager` 字段，
**必须显式写 `version`**。

### Android 交叉编译要显式指定 CC 与 LINKER

只装 NDK 是不够的。`cc-rs` 会去找**不带 API 级别**的
`aarch64-linux-android-clang`，而 NDK 从 r19 起只提供带级别的 wrapper
（`aarch64-linux-android24-clang` 之类）。实测 NDK 29 里确实没有不带级别的
那个，于是 `zstd-sys` 的 build script 报
`failed to find tool "aarch64-linux-android-clang"`，整个任务挂掉。

所以两条流水线的 Android 任务都有一步指定交叉工具链，把
`CC_<target>` / `AR_<target>` 指到真实存在的 wrapper。测试流水线安装 Rust 时
使用 `osdk install rust --force`：Action 缓存曾恢复出一份宿主编译器可运行、
但 rustup manifest 与 Android target 都缺失的半成品；普通 install 只看完成
标记会跳过，单独 target add 又无法在缺 manifest 时自修复。强制安装让 osdk
按 `osdk.toml` 一次重建版本、clippy 与全部 target，仍不绕过它的隔离目录和
镜像选择。

三个易错点：

- API 级别要和 `gen/android/app/build.gradle.kts` 的 `minSdk`（当前 24）一致。
  用更高的级别会让 `.so` 在低版本系统上加载失败，而那只有真机能发现。
  **改 minSdk 时这里要跟着改。**
- armv7 的目标三元组与 clang 前缀不一致：Rust 叫 `armv7-linux-androideabi`，
  NDK 的 wrapper 叫 `armv7a-`（多一个 a）。少写那个 a 还是「找不到工具」，
  报错完全看不出差在哪。
- **`CC_*` 不管链接。** 它只给 `cc-rs` 编 C 用（`zstd-sys` 那种）；链接可执行
  文件走的是 `CARGO_TARGET_<TARGET>_LINKER`，没设时 cargo 去找默认的 `cc`。
  test.yml 以前只编 `omy-core`（lib，不链接）所以没暴露，加上 `omy-cli` 这个
  bin 之后就会挂在 ``error: linker `cc` not found``——报错完全不提 Android。

注意本机（Windows + osdk）**不会**复现「找不到 CC」这个失败：PATH 上有别的
`clang`，`cc-rs` 在找不到带前缀的 wrapper 后会退回它，加上 `--target=` 照样
编得过。所以「本地能编」不能证明 CI 能编，判断要看有没有显式设 `CC_<target>`。

### Android 任务编四个 crate，不是只编 core

test.yml 的 Android 任务原先只 `cargo build -p omy-core`，于是「某个 crate 在
Android 上编不过」这类问题 CI 完全看不见——实测 `omy-cli` 曾无条件依赖
`trash`，而 `trash` 5.2 的 cfg 排除了 android，交叉编译在编 trash 自身时就
失败，但 CI 编不到 cli，一直显示绿。

现在四个 crate（core / media / net / cli）都编，两个架构各一遍，并额外跑一次
Android 目标的 clippy：平台专属分支只有在对应目标下才会被 lint，宿主 clippy
检查不到 `#[cfg(target_os = "android")]` 里的代码。

`omy-gui` 不在其中：它的 Android 版要走 gradle + tauri 打包，不是 cargo 单独
能编的，那件事在 release.yml 里做。

### 为什么只有 linux 允许失败

项目已在 Windows、Android 与 macOS（Apple Silicon）真机上实测过，这些平台
设成必过。Linux 的代码路径写了但没在真机上跑过，设成必过会让 main 长期挂红，
红叉也就不再有意义。它现在的作用是持续提供「还差什么」的信息。

Linux 连续绿且确实有人用过之后，删掉它的 `continue-on-error`，同时改掉
`site/index.md`、`site/en/index.md`、`site/guide/install.md` 里「尚未验证」
的说法。

### 为什么必过任务用 stable 而不是 MSRV

`Cargo.toml` 写着 `rust-version = "1.88"`。edition 2024 自身虽然只要求 1.85，
但 `grammers-mtsender 0.10` 依赖的 `hickory-net`、`hickory-proto`、
`hickory-resolver` 0.26.3 都明确要求 1.88；这条版本约束内没有 1.85 兼容候选，
resolver 3 也无法自动降级。因此流水线必须用 1.88 核实真实下限。

但仍然由 stable 负责门禁、`msrv` 任务非阻塞。首次核实就抓到 5 处 let-chain
（`if let Some(x) = a && cond`，该语法 1.88 才稳定），说明这条线以前没人盯，
短期内还可能再冒出别的不兼容写法。等它在 CI 上连续绿再提升为必过。

### 跑测试的任务都要装 FFmpeg

`omy-media` 有一批测试需要 FFmpeg：有损 WebP 编码走的是它的 libwebp，抽帧和
媒体探测也要它。runner 镜像**不带 ffmpeg**，不装的话这些测试只会跳过——
流水线是绿的，但那些路径根本没被覆盖。

windows 用 `choco`、linux 用 `apt`、macos 用 `brew`，都装在「测试」步骤之前。

不同 FFmpeg 版本的列表表头分隔线并不固定：有的是 `--`，有的是 `------`。
能力解析只把「至少两个连字符且没有其他字符」当作分隔线；若写死成三个以上，
Ubuntu runner 上的 FFmpeg 虽然安装成功，最终仍会被误报成 `muxers=[]`。

android 任务只跑 `cargo test -p omy-core`，core 不碰 FFmpeg，所以不用装。

注意这不会掩盖「没装 FFmpeg 时怎么降级」那条路径：相关单元测试直接调降级
函数，不依赖 runner 上缺不缺 ffmpeg。

### FFmpeg 任务只安装所需的 osdk 工具

Windows 的 FFmpeg 测试与发布任务、Linux 发布任务都通过 one-sdk 仓库的
composite Action 安装 osdk，并缓存 Action 管理的 data/cache 目录。Action 固定到包含
该能力的完整提交 SHA，CLI 版本显式固定为 0.0.4；这样既不跟随可变分支，也不会因 SHA
引用而额外查询 latest Release。

Action 在 `runner.temp` 下执行且关闭自动物化，随后回到仓库根目录信任 `osdk.toml`，
按任务显式安装工具。Windows 只装 FFmpeg 配方使用的五个 conda 工具，Linux 只装
`conda:nasm`。Windows 解压源码时脚本优先找 `bsdtar`，但 `m2-base` 的实际
安装内容并不保证含它；缺失时改用 runner 自带、同样基于 libarchive 的
`System32\\tar.exe`，不能只因为 shim 配置声明了 `bsdtar` 就假定命令存在。
不要把工具安装改回无参数的 `osdk install`：根配置还声明了本地 Android E2E
所需的 emulator 与 `android-35;google_apis;x86_64` system image；FFmpeg 任务不使用它们，
但全量安装会下载约 1 GiB 的 Google 镜像。首次发布曾在这里连续 6 小时没有新输出，
最终被 GitHub 取消。Windows FFmpeg job 另设 90 分钟上限，保证类似的上游静默阻塞能
尽早暴露。

### 为什么没有 cargo fmt --check

实测有 89 个文件、649 处不符合 rustfmt 默认风格——本仓库的中文注释和对齐
是手写的，rustfmt 会重排。加这个门禁会让流水线从第一次运行就红，唯一的
修法是全仓库重排格式，那是独立的决定。要引入的话，先单独提一次全仓库
fmt，再加这一步。

release.yml
-----------

### 触发方式：提交标题含 `[publish]`

推送到 main 时，只看**本次推送最后一个提交**的标题（提交信息首行），
含 `[publish]` 就触发一次完整发布；也可在 Actions 页面手动触发
（workflow_dispatch）。

```
chore: 准备 0.1.0 发布 [publish]
```

只看最后一个提交、不遍历整批提交：发布意图必须由推送链**顶端**的那个提交
明确表达，不从一串中间提交里翻标记——否则一次普通推送若带上了历史中残留
`[publish]` 的提交（rebase / 合并时很常见），会在毫不知情时触发发布。
只认标题（首行）：正文里讨论「`[publish]` 怎么用」不会触发。提交信息用
jq 从事件 JSON（`GITHUB_EVENT_PATH`）读出，不插值进 shell，避免反引号 /
`$()` 注入。

一次触发依次做两件事，且共享同一组构建门禁：

1. 构建四个平台产物并创建 **GitHub Release**（自动打 `v{version}` 标签，
   版本取自根 `Cargo.toml` 的 workspace 版本）；
2. 发布到 **crates.io**（在所有平台构建全绿、且 `crates-io` environment
   放行之后）。

旧设计是「推 `v*` 标签出 Release、`[publish]` 发 crates.io」两套入口。
现在统一成一个入口：发布必须同时产出全部平台产物与 registry 包，不允许
只做一半；标签由流水线自动创建，不再手工打。

### 构建门禁：四个任务全部必过

| 任务 | runner | 产物 |
|---|---|---|
| build-windows | windows-latest | Windows x86_64 zip（内置自建 FFmpeg） |
| build-android | ubuntu-latest | 四个 ABI 的未签名 APK |
| build-linux | ubuntu-latest | Linux x86_64 tar.gz |
| build-macos | macos-latest | macOS aarch64 与 x86_64 两个 tar.gz |

四个任务**都是硬门禁**，不再有 `continue-on-error`：既然发布承诺三系统
桌面目标加 APK，哪个平台编不过就不该出 Release。但「CI 编过」不等于
「真机用过」——目前 Windows、Android 与 macOS（Apple Silicon）真机实测过，
Linux 与 macOS（Intel）仅保证编译、打包与 FFmpeg 产物校验成功，
这一点明确写在 Release 说明的状态表里，不混淆。

macOS 在 arm64 runner 上由 `osdk install rust` 按 osdk.toml
`[tools].rust.targets` 装好 aarch64/x86_64-apple-darwin，再交叉编译一份
Intel 产物；GitHub runner 的系统框架是 universal 的，不需要额外 SDK。

Android 发布任务由 one-sdk Action 恢复并保存 JDK、Rust、Node/pnpm、NDK、SDK
平台与 Build Tools；随后执行 `osdk run --no-deps android-release-build`。安装列表
刻意不含 emulator 和 system image：生成 APK 不需要启动虚拟设备，把本地 E2E 的
大镜像带进发布任务只会增加下载超时面。

### 发布到 crates.io 的顺序、范围与限流

发布由 `scripts/publish-crates.sh` 按依赖顺序逐个执行：

    omy-core → omy-config → omy-media → omy-net → omy-secret → omy-cli

`omy-gui`（build.rs 需要前端产物）、`omy-remote`（应用层远程驱动）与
`spikes/*` 在各自 Cargo.toml 里标了 `publish = false`，脚本清单里没有它们。

`omy-config` 与 `omy-secret` 是可发布的库：CLI 依赖它们，不发布则
`cargo publish -p omy-cli` 会因为找不到 registry 版本而失败。
所有 path 依赖都同时写了 `version`，否则打包阶段直接报错。

#### 为什么不用 `cargo publish --workspace`

crates.io 官方限流（https://crates.io/docs/rate-limits）：

- **新 crate**：单账号 burst **5** 个，之后每 **10 分钟**只准 1 个；
- 已有 crate 的**新版本**：burst 30 个，之后每分钟 1 个。

首次发布有 **6 个全新包**，`--workspace` 会连续打出去，第 6 个必然 429，
而前 5 个此时已经发出去删不掉。脚本因此：

1. 发每个包前查 `crates.io/api/v1/crates/<name>` 区分「全新包」还是
   「新版本」；
2. 按依赖顺序逐个 `cargo publish -p`；包之间留 30s 让 sparse 索引（CDN）
   传播，好让下一个包能解析到刚发的依赖；
3. 满 5 个新包后、下一个仍是新包时，等待 **610s**（略大于 10 分钟）再发；
4. 单个包失败最多重试 3 次，每次重试也等满一个冷却窗口。

所以**首次发布会在第 5 个包后暂停约 10 分钟，全程约 13 分钟**，publish
任务的 `timeout-minutes` 设为 90。以后都是发新版本，6 个包远在 30 的
burst 内，只受 30s 的索引传播等待影响。

真正上传前仍会先跑一次 `cargo publish --workspace --dry-run` 整体校验。

### 需要在 GitHub 上配置的环境（environment）

crates.io 发布走名为 **`crates-io`** 的 environment，首次发布前需要在
网页上手动建一次（YAML 只能引用环境，不能创建它）：

1. 仓库 **Settings → Environments → New environment**，名字必须正好是
   `crates-io`；
2. （可选但推荐）勾选 **Required reviewers**，把自己加为审批人——上传前
   流水线会暂停等人确认，对一个发出去删不掉的动作值得有这道闸门；
3. 在该环境的 **Environment secrets** 里加：

| Secret 名 | 值 |
|---|---|
| `CARGO_REGISTRY_TOKEN` | crates.io → Account Settings → API Tokens 生成的 token（publish-new 权限） |

配在仓库级 Secrets 工作流也能读到，但放在 environment 里才能配合
required reviewers。环境名拼错不会报错，只会读到空 secret，所以
publish 的第一步就显式检查 token 非空（`--dry-run` 不查 token，缺它时
会一路绿灯到真上传才失败，而那时可能已发出一部分包）。

`GITHUB_TOKEN` 由 Actions 自动提供（Release 的 `contents: write` 权限），
不需要额外配置。

### 发布前还要确认的事

- workspace 版本（根 `Cargo.toml`）已经按 semver 递增；crates.io 上
  **同一个版本号不能重复使用**，发版后再触发会在上传对应包时失败。
- **首次发布会持续约 13 分钟**（6 个新包撞上新 crate 限流，第 5 个后要等
  10 分钟冷却，见上节），不要因为任务停在 publish 步骤就以为它挂了。
- APK 始终是**未签名**的；签名密钥不进仓库，需要正式签名版时本地签或
  另配 secret 加签名步骤。
