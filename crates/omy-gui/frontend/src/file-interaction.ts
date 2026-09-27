/**
 * 文件条目单击的统一决策。
 *
 * 移动端必须把“打开”和“选择”分成两个互不切换的手势：单击打开，
 * 长按选择。若让现有选择态改变单击含义，用户打开文件返回后再点其它文件
 * 就会误加选。桌面端仍保留单击选择、双击打开的文件管理器习惯。
 */
export type FileClickAction = 'open' | 'select' | 'ignore';

export function fileClickAction(mobile: boolean, suppressAfterLongPress: boolean): FileClickAction {
  if (suppressAfterLongPress) return 'ignore';
  return mobile ? 'open' : 'select';
}
