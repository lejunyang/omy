/** 应用内的确认 / 输入 / 密码对话框，替代 `window.confirm` 与 `window.prompt`。
 *
 * # 为什么不用浏览器原生对话框
 *
 * - `prompt` 的输入框**不遮蔽**：拿它收密码，密码就明文摆在屏幕上。
 * - Android WebView 弹的是系统旧式对话框，样式与应用完全不搭；而且它会同步阻塞
 *   JS，调用前刚关掉的弹窗来不及重绘，原生框底下还叠着那个「已关闭」的弹窗。
 * - 原生框不走应用的返回键栈与主题，也无法标出「这是不可撤销的操作」。
 *
 * 调用方式与原生一样是「问一句、等答案」，只是变成 Promise：
 * `if (!(await askConfirm({ ... }))) return;`
 */
import { reactive } from 'vue';

export type AskKind = 'confirm' | 'text' | 'password';

export interface AskOptions {
  /** 标题，一句话说清要做什么。 */
  title: string;
  /** 补充说明（后果、范围）。 */
  message?: string;
  /** 输入框初始值（text 模式）。 */
  initial?: string;
  /** 输入框占位提示。 */
  placeholder?: string;
  /** 确认按钮文案，默认「确定」。 */
  confirmLabel?: string;
  /** 不可撤销的操作：确认按钮用危险色。 */
  danger?: boolean;
}

export interface AskRequest extends AskOptions {
  kind: AskKind;
  resolve: (value: string | boolean | null) => void;
}

/** 当前正在问的那一个。同一时间只问一件事。 */
export const askState = reactive<{ current: AskRequest | null }>({ current: null });

function open(kind: AskKind, opts: AskOptions): Promise<string | boolean | null> {
  // 新的问题顶掉旧的：旧的当作取消。不排队——两个问题叠着弹，
  // 用户分不清当前回答的是哪一个
  // 注意按**旧问题**的类型给取消值：确认框取消是 false，输入框取消是 null
  const prev = askState.current;
  prev?.resolve(prev.kind === 'confirm' ? false : null);
  return new Promise((resolve) => {
    askState.current = { ...opts, kind, resolve };
  });
}

/** 是/否确认。取消、点遮罩、按返回都算「否」。 */
export function askConfirm(opts: AskOptions): Promise<boolean> {
  return open('confirm', opts) as Promise<boolean>;
}

/** 让用户输入一行文本；取消返回 null，确定返回去掉首尾空白后的文本。 */
export function askText(opts: AskOptions): Promise<string | null> {
  return open('text', opts) as Promise<string | null>;
}

/** 让用户输入密码（遮蔽显示）；取消返回 null，原样返回输入（密码可以有空格）。 */
export function askPassword(opts: AskOptions): Promise<string | null> {
  return open('password', opts) as Promise<string | null>;
}

/** 由对话框组件调用：给出答案并关闭。 */
export function answer(value: string | boolean | null) {
  const cur = askState.current;
  askState.current = null;
  cur?.resolve(value);
}
