/** 内存时间轴精灵图生成器。
 *
 * 复用同一个 omystream URL 在隐藏 video 中按时间采样，产物是 data URL，只存在于
 * WebView 内存，不写临时文件。独立解码器不会打断用户正在播放的主 video。
 */

export type TimelineThumbnail = {
  urls: string[];
  pic_num: number;
  col: number;
  row: number;
  width: number;
  height: number;
};

export type TimelineLayout = {
  count: number;
  col: number;
  row: number;
  width: number;
  height: number;
  times: number[];
};

const MIN_FRAMES = 12;
const MAX_FRAMES = 36;
const SECONDS_PER_FRAME = 3;
const LONG_EDGE = 160;

export function timelineLayout(
  duration: number,
  videoWidth: number,
  videoHeight: number,
): TimelineLayout {
  const safeDuration = Number.isFinite(duration) && duration > 0 ? duration : 0;
  const count = Math.min(MAX_FRAMES, Math.max(MIN_FRAMES, Math.ceil(safeDuration / SECONDS_PER_FRAME)));
  const aspect = videoWidth > 0 && videoHeight > 0 ? videoWidth / videoHeight : 16 / 9;
  const width = aspect >= 1 ? LONG_EDGE : Math.max(1, Math.round(LONG_EDGE * aspect));
  const height = aspect >= 1 ? Math.max(1, Math.round(LONG_EDGE / aspect)) : LONG_EDGE;
  const col = Math.ceil(Math.sqrt(count));
  const row = Math.ceil(count / col);
  // 取每个时间桶的中点，避免第 0 秒或结尾恰好落在黑帧/不可 seek 边界。
  const times = Array.from({ length: count }, (_, index) =>
    safeDuration > 0 ? safeDuration * (index + 0.5) / count : 0,
  );
  return { count, col, row, width, height, times };
}

function abortError(): DOMException {
  return new DOMException('timeline thumbnail generation aborted', 'AbortError');
}

function waitForEvent(
  target: EventTarget,
  success: string,
  failure: string,
  signal: AbortSignal,
  timeoutMs: number,
): Promise<void> {
  return new Promise((resolve, reject) => {
    if (signal.aborted) {
      reject(abortError());
      return;
    }
    let timer = 0;
    const cleanup = () => {
      target.removeEventListener(success, onSuccess);
      target.removeEventListener(failure, onFailure);
      signal.removeEventListener('abort', onAbort);
      window.clearTimeout(timer);
    };
    const onSuccess = () => {
      cleanup();
      resolve();
    };
    const onFailure = () => {
      cleanup();
      reject(new Error(`video ${failure}`));
    };
    const onAbort = () => {
      cleanup();
      reject(abortError());
    };
    target.addEventListener(success, onSuccess, { once: true });
    target.addEventListener(failure, onFailure, { once: true });
    signal.addEventListener('abort', onAbort, { once: true });
    timer = window.setTimeout(() => {
      cleanup();
      reject(new Error(`video ${success} timeout`));
    }, timeoutMs);
  });
}

async function waitForDecodedFrame(video: HTMLVideoElement, signal: AbortSignal) {
  if (signal.aborted) throw abortError();
  await new Promise<void>((resolve, reject) => {
    let timer = 0;
    let frameHandle: number | null = null;
    let settled = false;
    const cleanup = () => {
      window.clearTimeout(timer);
      signal.removeEventListener('abort', onAbort);
      if (frameHandle !== null && typeof video.cancelVideoFrameCallback === 'function') {
        video.cancelVideoFrameCallback(frameHandle);
      }
    };
    const finish = () => {
      if (settled) return;
      settled = true;
      cleanup();
      resolve();
    };
    const onAbort = () => {
      if (settled) return;
      settled = true;
      cleanup();
      reject(abortError());
    };
    signal.addEventListener('abort', onAbort, { once: true });
    if (typeof video.requestVideoFrameCallback === 'function') {
      frameHandle = video.requestVideoFrameCallback(finish);
      // 部分 WebView 在 paused video 上不触发回调，超时后仍可使用 seeked 对应帧。
      timer = window.setTimeout(finish, 1000);
    } else {
      timer = window.setTimeout(finish, 40);
    }
  });
}

export async function generateTimelineThumbnail(
  url: string,
  signal: AbortSignal,
): Promise<TimelineThumbnail | null> {
  const video = document.createElement('video');
  video.crossOrigin = 'anonymous';
  video.preload = 'metadata';
  video.muted = true;
  video.playsInline = true;
  video.src = url;

  try {
    if (video.readyState < HTMLMediaElement.HAVE_METADATA) {
      video.load();
      await waitForEvent(video, 'loadedmetadata', 'error', signal, 15_000);
    }
    const layout = timelineLayout(video.duration, video.videoWidth, video.videoHeight);
    if (!Number.isFinite(video.duration) || video.duration <= 0 || !video.videoWidth || !video.videoHeight) {
      return null;
    }

    const canvas = document.createElement('canvas');
    canvas.width = layout.width * layout.col;
    canvas.height = layout.height * layout.row;
    const context = canvas.getContext('2d', { alpha: false });
    if (!context) return null;
    context.fillStyle = '#000';
    context.fillRect(0, 0, canvas.width, canvas.height);

    for (let index = 0; index < layout.count; index += 1) {
      if (signal.aborted) throw abortError();
      const target = layout.times[index];
      if (Math.abs(video.currentTime - target) > 0.01) {
        const seeked = waitForEvent(video, 'seeked', 'error', signal, 12_000);
        video.currentTime = target;
        await seeked;
      }
      await waitForDecodedFrame(video, signal);
      const x = (index % layout.col) * layout.width;
      const y = Math.floor(index / layout.col) * layout.height;
      context.drawImage(video, x, y, layout.width, layout.height);
    }

    return {
      urls: [canvas.toDataURL('image/jpeg', 0.72)],
      pic_num: layout.count,
      col: layout.col,
      row: layout.row,
      width: layout.width,
      height: layout.height,
    };
  } finally {
    video.pause();
    video.removeAttribute('src');
    video.load();
  }
}
