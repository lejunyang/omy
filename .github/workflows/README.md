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
| msrv | ubuntu-latest | 否 | 用 1.85 编一遍，核实声明的下限 |
| linux | ubuntu-latest | 否 | build / test / clippy |
| macos | macos-latest | 否 | build / test / clippy |
| result | ubuntu-latest | 是 | 汇总必过任务，供分支保护使用 |

**分支保护只需要勾 `测试结论` 这一项。** 它只看 windows / android /
frontend；未验证平台的红叉不会阻塞合并。

### 每个编 omy-gui 的任务都要装 Node 与 pnpm

`omy-gui` 的 `build.rs` 在 `dist/` 缺失时会调包管理器构建前端，而 `dist/`
不入库——所以**凡是会编到 omy-gui 的任务**（windows / linux / macos / msrv，
以及 release 里的各构建任务）都必须先装 Node 与 pnpm。

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

### Android 交叉编译要显式指定 CC

只装 NDK 是不够的。`cc-rs` 会去找**不带 API 级别**的
`aarch64-linux-android-clang`，而 NDK 从 r19 起只提供带级别的 wrapper
（`aarch64-linux-android24-clang` 之类）。实测 NDK 29 里确实没有不带级别的
那个，于是 `zstd-sys` 的 build script 报
`failed to find tool "aarch64-linux-android-clang"`，整个任务挂掉。

所以两条流水线的 Android 任务都有一步「指定交叉编译用的 C 编译器」，
把 `CC_<target>` / `AR_<target>` 指到真实存在的 wrapper。两个易错点：

- API 级别要和 `gen/android/app/build.gradle.kts` 的 `minSdk`（当前 24）一致。
  用更高的级别会让 `.so` 在低版本系统上加载失败，而那只有真机能发现。
  **改 minSdk 时这里要跟着改。**
- armv7 的目标三元组与 clang 前缀不一致：Rust 叫 `armv7-linux-androideabi`，
  NDK 的 wrapper 叫 `armv7a-`（多一个 a）。少写那个 a 还是「找不到工具」，
  报错完全看不出差在哪。

注意本机（Windows + osdk）**不会**复现这个失败：PATH 上有别的 `clang`，
`cc-rs` 在找不到带前缀的 wrapper 后会退回它，加上 `--target=` 照样编得过。
所以「本地能编」不能证明 CI 能编，判断要看有没有显式设 `CC_<target>`。

### 为什么 linux / macos 允许失败

项目只在 Windows 与 Android 上实测过。这两个平台的代码路径写了但没跑过，
设成必过会让 main 长期挂红，红叉也就不再有意义。它们现在的作用是持续
提供「还差什么」的信息。

某个平台连续绿且确实有人用过之后，删掉它的 `continue-on-error`，同时改掉
`site/index.md`、`site/en/index.md`、`site/guide/install.md` 里「尚未验证」
的说法。

### 为什么必过任务用 stable 而不是 MSRV

`Cargo.toml` 写着 `rust-version = "1.85"`。这个下限**已在本机核实**：装上
1.85 工具链后 `cargo +1.85 check --workspace --all-targets` 通过。

但仍然由 stable 负责门禁、`msrv` 任务非阻塞。首次核实就抓到 5 处 let-chain
（`if let Some(x) = a && cond`，该语法 1.88 才稳定），说明这条线以前没人盯，
短期内还可能再冒出别的 1.85 不支持的写法。等它在 CI 上连续绿再提升为必过。

### 跑测试的任务都要装 FFmpeg

`omy-media` 有一批测试需要 FFmpeg：有损 WebP 编码走的是它的 libwebp，抽帧和
媒体探测也要它。runner 镜像**不带 ffmpeg**，不装的话这些测试只会跳过——
流水线是绿的，但那些路径根本没被覆盖。

windows 用 `choco`、linux 用 `apt`、macos 用 `brew`，都装在「测试」步骤之前。

android 任务只跑 `cargo test -p omy-core`，core 不碰 FFmpeg，所以不用装。

注意这不会掩盖「没装 FFmpeg 时怎么降级」那条路径：相关单元测试直接调降级
函数，不依赖 runner 上缺不缺 ffmpeg。

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
| 发布到 crates.io | 提交信息**首行**含 `[publish]` |
| 手动发布到 crates.io | Actions 页面手动触发并勾选 |

两者独立，可同时发生（推标签且该提交标题含 `[publish]`）。

### 为什么 crates.io 要单独的标记

**crates.io 上的版本发布后无法删除**，只能 yank，而且版本号不能复用。
GitHub Release 删了可以重发，registry 不行。所以上传 registry 必须由提交
信息里的显式标记触发，不能作为推标签的副作用顺带发生。

标记只认**提交信息首行**（标题）。原先是全文子串匹配，已经误触发过一次：
某次提交在正文里解释「`[publish]` 这个标记怎么用」，推上去就把发布流水线
拉起来了。讨论、引用、revert 说明都会命中，而这是个删不掉的动作，不能靠
「注意别提它」来避免。写进标题才算数。

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

| 名称 | 配在哪 | 用途 |
|---|---|---|
| `CARGO_REGISTRY_TOKEN` | environment `crates-io` | 发布到 crates.io。在 crates.io 的 Account Settings 生成 |

`GITHUB_TOKEN` 由 Actions 自动提供，不用配。Pages 部署用的是工作流里声明
的 `pages: write` / `id-token: write` 权限，也不需要额外 secret。

### 为什么 publish 走 environment

`publish` 任务绑定了名为 `crates-io` 的 environment。配置位置：
**Settings → Environments → New environment**，名字必须正好是 `crates-io`，
然后把 `CARGO_REGISTRY_TOKEN` 加到该环境的 Environment secrets 里。

配在仓库级 Secrets 里工作流一样读得到，但绑 environment 多两个好处：
可以给它加 **required reviewers**，让上传前停下来等人点确认；也能在
Actions 页面看到这个环境的部署历史。对一个**发出去就删不掉**的动作，
这道人工闸门值得。

注意 environment 名字写错不会报错，只会创建一个新的空环境——于是
`secrets.CARGO_REGISTRY_TOKEN` 变成空字符串。所以 `publish` 的第一步就
显式检查 token 非空：`--dry-run` 不需要 token 也能过，缺 token 时会一路
绿灯直到真上传那步才失败，而那时 cargo 可能已经发出去一部分包了。

### 为什么 publish 要挂构建门禁

crates.io 的版本发布后无法删除，所以这个不可撤销的动作不能在构建红着的
时候发生。`publish` 因此依赖 `build-windows` 与 `build-android` 成功——
只挂这两个已验证平台，linux/macos 是 `continue-on-error`，挂上去等于把
未验证平台变成了发布门禁。

这里有个反直觉的连带改动：**被跳过的依赖会让下游任务一起跳过**。两个构建
任务原先只在推标签时跑，如果不动它们，一个不带标签、只含 `[publish]` 的
提交会让构建 skip、publish 跟着 skip——表现是「写了 `[publish]` 却什么都
没发布」，而且没有任何报错。所以它们的条件加上了 `publish == 'true'`。

同理，`publish` 的 `if` 必须判 `result == 'success'` 而不是
`!= 'failure'`：skipped 不是 failure，用后者写门禁等于没设。

### 工具链版本与 osdk.toml 保持一致

Android 相关任务里的 JDK 与 NDK 版本必须和仓库根 `osdk.toml` 一致
（当前 JDK 21、NDK 29.0.14206865）。两边不一致会出现「本地能编过 CI 编不过」
或反过来，而排查方向会完全跑偏——JDK 版本不对时 gradle 在配置期崩溃，
报错只有一行版本号，看不出是 JDK 的问题。

JDK 不能超过 21：Gradle 8.14 上限是 24，Kotlin 1.9.25 的 JVM target 上限
是 21，两条约束叠加后 21 就是上限。

### APK 要装四个 Rust 目标

APK 默认打四个 ABI（`gen/android/buildSrc` 里的 `targetList` 是
aarch64 / armv7 / i686 / x86_64），所以 `build-android` 要把这四个 Rust 目标
都装上，`CC_<target>` 也要配齐四个。只配前两个时，gradle 会在编 x86 那一档
才报错，而前面几档已经编了十几分钟。

### APK 未签名

签名密钥不进仓库，也不由 CI 代持，所以流水线产出的是未签名 APK，安装时
需要允许未知来源。要正式签名版就本地签，或另配 secrets 后在
`build-android` 里加签名步骤。

首次发布到 crates.io
--------------------

四个包都还没上架。首次发布只要一次带 `[publish]` 的提交即可，`--workspace`
会按依赖顺序处理。已在本地用 `--dry-run` 验证过顺序可行，四个名字在
crates.io 上也都还没被占用（名字归属 `--dry-run` 查不出来，它不联网校验）。

发之前要先建好 `crates-io` environment 并配上 token，见上面那一节。
注意带 `[publish]` 的提交推上去后，会先跑 Windows 与 Android 的构建，
两者都绿了才会走到上传。
