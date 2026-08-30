/**
 * 判定 pick_folder 在未设 OMY_GUI_PICK_FOLDER 时是否走真实对话框。
 *
 * 手段：调用它并只等 4 秒。
 *   - 超时（没返回）→ 说明真的弹了窗在等用户 → 正确
 *   - 立刻返回字符串 → 说明旁路无条件生效 → 错误
 *   - 立刻报错      → 命令本身有问题
 *
 * 只用 Node 内置模块，与 cdp-gui.mjs 保持一致（不引第三方依赖）。
 */
import http from 'node:http';
import net from 'node:net';
import crypto from 'node:crypto';

const port = process.argv[2] || '9455';

function getJson(url) {
  return new Promise((resolve, reject) => {
    http.get(url, (res) => {
      let b = '';
      res.on('data', (c) => (b += c));
      res.on('end', () => {
        try { resolve(JSON.parse(b)); } catch (e) { reject(e); }
      });
    }).on('error', reject);
  });
}

const targets = await getJson(`http://127.0.0.1:${port}/json/list`);
const page = targets.find((t) => t.type === 'page' && t.webSocketDebuggerUrl);
if (!page) { console.log('NO_TARGET'); process.exit(0); }

// 极简 WebSocket 客户端：只需发一条 Runtime.evaluate 再读回复
const u = new URL(page.webSocketDebuggerUrl);
const sock = net.connect(Number(u.port), u.hostname);
const key = crypto.randomBytes(16).toString('base64');

sock.on('connect', () => {
  sock.write(
    `GET ${u.pathname} HTTP/1.1\r\nHost: ${u.host}\r\nUpgrade: websocket\r\n` +
    `Connection: Upgrade\r\nSec-WebSocket-Key: ${key}\r\nSec-WebSocket-Version: 13\r\n` +
    `Origin: http://127.0.0.1:${port}\r\n\r\n`,
  );
});

let handshook = false;
let answered = false;

function frame(payload) {
  const data = Buffer.from(payload, 'utf8');
  const mask = crypto.randomBytes(4);
  const masked = Buffer.alloc(data.length);
  for (let i = 0; i < data.length; i++) masked[i] = data[i] ^ mask[i % 4];
  let head;
  if (data.length < 126) {
    head = Buffer.from([0x81, 0x80 | data.length]);
  } else {
    head = Buffer.alloc(4);
    head[0] = 0x81; head[1] = 0xfe;
    head.writeUInt16BE(data.length, 2);
  }
  return Buffer.concat([head, mask, masked]);
}

sock.on('data', (buf) => {
  if (!handshook) {
    const s = buf.toString('latin1');
    if (s.includes('101')) {
      handshook = true;
      // awaitPromise:true —— 若命令挂起，这里就不会有回复
      sock.write(frame(JSON.stringify({
        id: 1,
        method: 'Runtime.evaluate',
        params: {
          expression: `window.__TAURI_INTERNALS__.invoke('pick_folder', { title: 'probe' })`,
          returnByValue: true,
          awaitPromise: true,
        },
      })));
    }
    return;
  }
  // 收到任何回复都意味着命令**没有**挂起
  answered = true;
  const txt = buf.toString('utf8');
  if (txt.includes('"result"') || txt.includes('"error"')) {
    console.log('RETURNED_IMMEDIATELY: ' + txt.slice(0, 200));
    try { sock.destroy(); } catch {}
    process.exit(0);
  }
});

setTimeout(() => {
  if (!answered) {
    // 没有回复 = 对话框正等着用户，正是我们要的
    console.log('CORRECT: 命令挂起等待用户操作，未走旁路');
  }
  try { sock.destroy(); } catch {}
  process.exit(0);
}, 4000);
