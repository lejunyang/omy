/** 应用状态。
 *
 * # 交互模型：文件管理器优先，密码按需
 *
 * 早先的模型是「先解锁才能进门」：启动就是一个密码框，输对了才看得到
 * 任何东西。这假设了用户已经是老用户——可新用户第一次打开时还没有
 * 任何加密文件，他要做的第一件事恰恰是**挑几个文件来加密**。
 *
 * 现在的模型：
 *
 * - 启动直接进文件管理器，能浏览任何目录，不需要密码
 * - 加密文件在列表里标出来，双击才问密码
 * - 顶栏有个统一密码框，输了就对当前目录批量试解锁
 *
 * 所以这里**没有** `unlocked` 这个全局开关。会话里有几个凭据是
 * `credentials`，那只影响「哪些加密文件能看见真名」，不影响能不能
 * 用这个应用。
 *
 * # 锁定态仍然是数据
 *
 * 文档 §10 那条结论没变：每个加密文件自己的 `unlocked` 决定它怎么显示，
 * 不能用「统一重设样式」实现。锁定时后端清空已识别列表，
 * 前端拿到的条目就都是锁定态。
 *
 * # 文件组织
 *
 * 实现按主题拆在 `store/` 下，这里只做汇总导出。**顺序有意义**：state 必须
 * 第一个求值——places 模块顶层的 watch 会立即读 state。模块之间可以互相引用
 * （顶层只有声明和惰性的 computed），但不要在模块顶层直接调用别的模块的函数。
 */

export * from './store/state';
export * from './store/local';
export * from './store/crypto-ops';
export * from './store/session';
export * from './store/remote-view';
export * from './store/virtual';
export * from './store/remote-browse';
export * from './store/grid';
export * from './store/remote-files';
export * from './store/places';
