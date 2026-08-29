/**
 * 最小 WebSocket 客户端，专为驱动 CDP。
 *
 * 为什么自己写：Node 20 的全局 WebSocket 需要 --experimental-websocket，
 * 而 `ws` 包需要联网安装。CDP 场景下我们只需要：
 *   - 客户端发文本帧（带掩码，协议要求）
 *   - 接收文本帧（服务端不掩码）
 *   - 处理分片与 ping
 * 这点功能用 net 模块几十行就能写完，零依赖且不受 Node 版本影响。
 *
 * 不实现的部分：permessage-deflate（CDP 不强制）、二进制帧
 * （CDP 只用文本）、TLS（本地调试是明文 ws://）。
 */
import net from 'node:net';
import crypto from 'node:crypto';
import { EventEmitter } from 'node:events';

export class MiniWs extends EventEmitter {
  constructor(url) {
    super();
    const u = new URL(url);
    this.buf = Buffer.alloc(0);
    this.frag = [];      // 分片累积
    this.fragOp = 0;
    this.open = false;

    const key = crypto.randomBytes(16).toString('base64');
    this.sock = net.connect(
      { host: u.hostname, port: Number(u.port || 80) },
      () => {
        const path = u.pathname + (u.search || '');
        const req =
          `GET ${path} HTTP/1.1\r\n` +
          `Host: ${u.host}\r\n` +
          `Upgrade: websocket\r\n` +
          `Connection: Upgrade\r\n` +
          `Sec-WebSocket-Key: ${key}\r\n` +
          `Sec-WebSocket-Version: 13\r\n` +
          `Origin: http://127.0.0.1\r\n` +
          `\r\n`;
        this.sock.write(req);
      }
    );
    this.sock.on('error', (e) => this.emit('error', e));
    this.sock.on('close', () => this.emit('close'));
    this.sock.on('data', (d) => this._onData(d));
  }

  _onData(chunk) {
    this.buf = Buffer.concat([this.buf, chunk]);
    if (!this.open) {
      const i = this.buf.indexOf('\r\n\r\n');
      if (i < 0) return;
      const head = this.buf.subarray(0, i).toString('latin1');
      if (!/HTTP\/1\.1 101/.test(head)) {
        this.emit('error', new Error('握手失败: ' + head.split('\r\n')[0]));
        return;
      }
      this.buf = this.buf.subarray(i + 4);
      this.open = true;
      this.emit('open');
    }
    this._parseFrames();
  }

  _parseFrames() {
    for (;;) {
      if (this.buf.length < 2) return;
      const b0 = this.buf[0];
      const b1 = this.buf[1];
      const fin = (b0 & 0x80) !== 0;
      const op = b0 & 0x0f;
      const masked = (b1 & 0x80) !== 0;
      let len = b1 & 0x7f;
      let off = 2;

      if (len === 126) {
        if (this.buf.length < off + 2) return;
        len = this.buf.readUInt16BE(off);
        off += 2;
      } else if (len === 127) {
        if (this.buf.length < off + 8) return;
        const big = this.buf.readBigUInt64BE(off);
        if (big > 64n * 1024n * 1024n) { this.emit('error', new Error('帧过大')); return; }
        len = Number(big);
        off += 8;
      }
      // 服务端不应掩码，但容错处理
      let mask = null;
      if (masked) {
        if (this.buf.length < off + 4) return;
        mask = this.buf.subarray(off, off + 4);
        off += 4;
      }
      if (this.buf.length < off + len) return;

      let payload = Buffer.from(this.buf.subarray(off, off + len));
      if (mask) for (let i = 0; i < payload.length; i++) payload[i] ^= mask[i % 4];
      this.buf = this.buf.subarray(off + len);

      if (op === 0x8) { this.close(); return; }            // close
      if (op === 0x9) { this._send(0xa, payload); continue; } // ping → pong
      if (op === 0xa) continue;                             // pong

      if (op === 0x0) {
        this.frag.push(payload);
      } else {
        this.frag = [payload];
        this.fragOp = op;
      }
      if (fin) {
        const full = Buffer.concat(this.frag);
        this.frag = [];
        if (this.fragOp === 0x1) this.emit('message', full.toString('utf8'));
      }
    }
  }

  _send(op, data) {
    if (this.sock.destroyed) return;
    const mask = crypto.randomBytes(4);
    const len = data.length;
    let head;
    if (len < 126) {
      head = Buffer.alloc(2);
      head[1] = 0x80 | len;
    } else if (len < 65536) {
      head = Buffer.alloc(4);
      head[1] = 0x80 | 126;
      head.writeUInt16BE(len, 2);
    } else {
      head = Buffer.alloc(10);
      head[1] = 0x80 | 127;
      head.writeBigUInt64BE(BigInt(len), 2);
    }
    head[0] = 0x80 | op;
    const masked = Buffer.from(data);
    for (let i = 0; i < masked.length; i++) masked[i] ^= mask[i % 4];
    this.sock.write(Buffer.concat([head, mask, masked]));
  }

  send(text) { this._send(0x1, Buffer.from(text, 'utf8')); }
  close() { if (!this.sock.destroyed) { try { this._send(0x8, Buffer.alloc(0)); } catch {} this.sock.destroy(); } }
}
