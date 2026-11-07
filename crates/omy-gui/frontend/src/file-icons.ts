/** 文件类型 → 图标与配色类别。
 *
 * 网格卡片与列表行共用这一份判定。原来网格（EntryCard）有完整的扩展名映射，
 * 列表行（MainScreen）另写了一个只分「文件夹 / 锁 / 文件」的版本：同一张
 * 图片在网格里是图片图标、切到列表就变成通用文件图标。
 *
 * 类别同时决定颜色（app.css 的 --c-* 令牌），一眼分得出图片、视频、音频、
 * 文档与压缩包。锁定的加密文件只给「锁」：它的类型本身就是要保护的信息。
 */

export type FileKind =
  | 'folder'
  | 'folder-locked'
  | 'locked'
  | 'unlocked'
  | 'image'
  | 'video'
  | 'audio'
  | 'doc'
  | 'sheet'
  | 'archive'
  | 'other';

const EXT_KIND: Record<string, FileKind> = {
  jpg: 'image', jpeg: 'image', png: 'image', gif: 'image', webp: 'image', bmp: 'image',
  svg: 'image', heic: 'image', heif: 'image', avif: 'image', apng: 'image', tif: 'image', tiff: 'image',
  mp4: 'video', mkv: 'video', mov: 'video', avi: 'video', webm: 'video', flv: 'video', wmv: 'video', m4v: 'video',
  mp3: 'audio', flac: 'audio', wav: 'audio', m4a: 'audio', aac: 'audio', ogg: 'audio', opus: 'audio', m4b: 'audio',
  txt: 'doc', md: 'doc', log: 'doc', json: 'doc', xml: 'doc', csv: 'sheet',
  pdf: 'doc', doc: 'doc', docx: 'doc', ppt: 'doc', pptx: 'doc', xls: 'sheet', xlsx: 'sheet',
  zip: 'archive', rar: 'archive', '7z': 'archive', gz: 'archive', tar: 'archive', xz: 'archive',
};

/** 后端按内容判出的预览类别（容器内条目常有不在映射里的后缀）。 */
const PREVIEW_KIND: Record<string, FileKind> = {
  image: 'image', video: 'video', audio: 'audio', text: 'doc',
};

/** 条目的类型类别。字段名与后端 Entry / 容器条目一致。 */
export function fileKind(e: {
  is_dir?: boolean;
  is_encrypted?: boolean;
  is_encrypted_dir?: boolean;
  unlocked?: boolean;
  ext?: string | null;
  preview?: string | null;
  real_name?: string | null;
  name?: string;
}): FileKind {
  if (e.is_dir) return e.is_encrypted_dir ? 'folder-locked' : 'folder';
  if (e.is_encrypted && !e.unlocked) return 'locked';
  // 已解锁的加密文件按真名的后缀判：磁盘名是 .omy，看不出类型
  const name = e.is_encrypted ? e.real_name || '' : e.name || '';
  const ext = (e.is_encrypted ? name.split('.').pop() : e.ext || name.split('.').pop() || '')
    ?.toLowerCase() || '';
  const byExt = EXT_KIND[ext];
  if (byExt) return byExt;
  const byPreview = e.preview ? PREVIEW_KIND[e.preview] : undefined;
  if (byPreview) return byPreview;
  return e.is_encrypted ? 'unlocked' : 'other';
}

/** 类别 → AppIcon 语义名。 */
export const KIND_ICON: Record<FileKind, string> = {
  folder: 'folder',
  'folder-locked': 'folder-lock',
  locked: 'lock',
  unlocked: 'unlock',
  image: 'image',
  video: 'video',
  audio: 'music',
  doc: 'file',
  sheet: 'spreadsheet',
  archive: 'archiveFile',
  other: 'package',
};
