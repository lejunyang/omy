/** Tauri IPC 桥接。
 *
 * 不引 `@tauri-apps/api` 包：那需要 npm 与打包步骤，而我们只用到
 * `invoke` 一个函数。Tauri v2 会在页面上注入 `window.__TAURI_INTERNALS__`，
 * 直接用它即可。
 *
 * 代价是要自己处理版本差异——所以下面对两种注入形态都做了兼容，
 * 并在都不存在时明确抛错，而不是静默返回 undefined
 * （那会让「后端没响应」表现为「数据是空的」，很难查）。
 */

/** 调用后端命令。 */
export async function invoke(cmd, args) {
  const internals = window.__TAURI_INTERNALS__;
  if (internals && typeof internals.invoke === 'function') {
    return internals.invoke(cmd, args ?? {});
  }
  // 某些版本把 invoke 直接挂在 __TAURI__ 上
  const legacy = window.__TAURI__;
  if (legacy && typeof legacy.invoke === 'function') {
    return legacy.invoke(cmd, args ?? {});
  }
  throw new Error(
    `Tauri IPC 不可用：找不到 invoke（命令 ${cmd}）。` +
      '这个页面是否在浏览器里直接打开了？',
  );
}
