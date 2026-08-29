#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""
CryptoVault (.cvlt) v1.0 参考实现 —— 用于验证格式设计与生成测试向量。

本实现刻意追求"可读、可对照规范"，而非性能。Rust 生产实现应遵循同一份规范。

覆盖：
  - 96 字节定长头 + 8 个不可区分 key slot + TLV 扩展区 + HMAC
  - 两级 KDF：Argon2id(密码, vault_salt) -> KEK；HKDF(KEK, file_uuid) -> slot 包裹密钥
  - 分块 AEAD 载荷，counter-based nonce，支持 O(1) 随机访问
  - 可选 zstd 分块压缩 + 加密索引表
  - 分片（含可选冗余头）
  - 往返一致性 / 随机访问 / 篡改检测 / 多密码 / 可否认性 自测
"""

import os
import io
import struct
import hmac
import hashlib
import json
import zlib
import binascii
from dataclasses import dataclass, field
from typing import List, Optional, Tuple, Dict

import zstandard as zstd
from argon2.low_level import hash_secret_raw, Type as Argon2Type
from cryptography.hazmat.primitives.ciphers.aead import ChaCha20Poly1305, AESGCM
from cryptography.hazmat.primitives.kdf.hkdf import HKDF
from cryptography.hazmat.primitives import hashes

# ---------------------------------------------------------------- 常量

MAGIC = b"CVAULT\x01\x00"          # 8 bytes
SHARD_MAGIC = b"CVSHARD\x01"        # 8 bytes
VERSION_MAJOR = 1
VERSION_MINOR = 0

FIXED_HEADER_LEN = 96
SLOT_SIZE = 48                      # 32B FEK + 16B AEAD tag
SLOT_COUNT = 8                      # 固定，空位填随机 -> 可否认性
SLOT_AREA_LEN = SLOT_SIZE * SLOT_COUNT   # 384
HEADER_MAC_LEN = 32
TAG_LEN = 16
SHARD_HEADER_LEN = 56

# cipher_id
CIPHER_CHACHA20POLY1305 = 1
CIPHER_AES256GCM = 2
# kdf_id
KDF_ARGON2ID = 1
# compress_id
COMPRESS_NONE = 0
COMPRESS_ZSTD = 1

# flags 位
FLAG_FILENAME_ENCRYPTED = 1 << 0
FLAG_EXT_PRESERVED = 1 << 1
FLAG_COMPRESSED = 1 << 2
FLAG_SHARDED = 1 << 3
FLAG_HAS_THUMBNAIL = 1 << 4
FLAG_CONTAINER = 1 << 5      # 文件夹容器模式
FLAG_DISGUISED = 1 << 6
FLAG_TRANSCODED = 1 << 7     # 载荷非原始字节（转码过）

# TLV 类型
TLV_FILENAME = 0x0001
TLV_PLAIN_EXT = 0x0002
TLV_THUMBNAIL = 0x0003
TLV_MEDIA_META = 0x0004
TLV_COMPRESSION_INDEX = 0x0005
TLV_MOOV_CACHE = 0x0006
TLV_SHARD_INFO = 0x0007
TLV_ORIGINAL_META = 0x0008
TLV_CONTENT_HASH = 0x0009
TLV_ZSTD_DICT = 0x000A
TLV_PORTABLE_SLOT = 0x000B
TLV_SUBTITLES = 0x000C
TLV_TRANSCODE_INFO = 0x000D
TLV_FOLDER_INDEX = 0x000E
TLV_DISGUISE_INFO = 0x000F

TLV_FLAG_CRITICAL = 1 << 0
TLV_FLAG_ENCRYPTED = 1 << 1

# HKDF info 标签（域分隔）
INFO_SLOT = b"cvault/v1/slot"
INFO_PAYLOAD = b"cvault/v1/payload"
INFO_HEADER_MAC = b"cvault/v1/header-mac"
INFO_TLV = b"cvault/v1/tlv"

# 生产环境 Argon2 档位
ARGON2_PROFILES = {
    "interactive": dict(m_kib=64 * 1024, t=3, p=1),
    "moderate":    dict(m_kib=256 * 1024, t=4, p=1),
    "sensitive":   dict(m_kib=1024 * 1024, t=4, p=1),
    "mobile":      dict(m_kib=32 * 1024, t=4, p=1),
    "test":        dict(m_kib=64, t=1, p=1),   # 仅供测试向量
}


# ---------------------------------------------------------------- 原语封装

def hkdf(key: bytes, salt: bytes, info: bytes, length: int = 32) -> bytes:
    return HKDF(algorithm=hashes.SHA256(), length=length,
                salt=salt, info=info).derive(key)


def argon2id(password: bytes, salt: bytes, m_kib: int, t: int, p: int) -> bytes:
    return hash_secret_raw(secret=password, salt=salt, time_cost=t,
                           memory_cost=m_kib, parallelism=p,
                           hash_len=32, type=Argon2Type.ID)


def aead_new(cipher_id: int, key: bytes):
    if cipher_id == CIPHER_CHACHA20POLY1305:
        return ChaCha20Poly1305(key)
    if cipher_id == CIPHER_AES256GCM:
        return AESGCM(key)
    raise ValueError(f"unknown cipher_id {cipher_id}")


def chunk_nonce(base_nonce7: bytes, index: int, is_final: bool) -> bytes:
    """12 字节 counter-based nonce = base(7) || index(u32 BE) || final(1)"""
    assert len(base_nonce7) == 7
    assert 0 <= index < 2 ** 32
    return base_nonce7 + struct.pack(">I", index) + (b"\x01" if is_final else b"\x00")


def chunk_aad(file_uuid: bytes, index: int) -> bytes:
    """AAD 绑定文件身份与块序号，防止跨文件移植 / 块重排"""
    return file_uuid + struct.pack(">I", index)


# ---------------------------------------------------------------- Key slot

@dataclass
class SlotCandidate:
    """一个候选解锁凭据（已派生出 KEK）"""
    name: str
    kek: bytes


def derive_kek_from_password(password: str, vault_salt: bytes,
                             m_kib: int, t: int, p: int) -> bytes:
    """慢速：每个 (密码, vault_salt) 组合每会话只做一次，结果应缓存"""
    return argon2id(password.encode("utf-8"), vault_salt, m_kib, t, p)


def slot_wrap_key(kek: bytes, file_uuid: bytes, slot_index: int) -> bytes:
    """快速：每文件每 slot 一次 HKDF（微秒级）"""
    return hkdf(kek, salt=file_uuid,
                info=INFO_SLOT + struct.pack("<H", slot_index))


# ---------------------------------------------------------------- TLV

def tlv_encode(entries: List[Tuple[int, int, bytes]]) -> bytes:
    out = bytearray()
    for t, flags, value in entries:
        out += struct.pack("<HHI", t, flags, len(value))
        out += value
    return bytes(out)


def tlv_decode(blob: bytes) -> List[Tuple[int, int, bytes]]:
    out, off = [], 0
    while off < len(blob):
        if off + 8 > len(blob):
            raise ValueError("TLV truncated header")
        t, flags, ln = struct.unpack_from("<HHI", blob, off)
        off += 8
        if off + ln > len(blob):
            raise ValueError("TLV truncated value")
        out.append((t, flags, blob[off:off + ln]))
        off += ln
    return out


def tlv_encrypt_value(fek: bytes, cipher_id: int, tlv_type: int, plain: bytes) -> bytes:
    """每个加密 TLV 用独立派生密钥 + 零 nonce（key 唯一即可安全）"""
    k = hkdf(fek, salt=b"", info=INFO_TLV + struct.pack("<H", tlv_type))
    return aead_new(cipher_id, k).encrypt(b"\x00" * 12, plain, None)


def tlv_decrypt_value(fek: bytes, cipher_id: int, tlv_type: int, ct: bytes) -> bytes:
    k = hkdf(fek, salt=b"", info=INFO_TLV + struct.pack("<H", tlv_type))
    return aead_new(cipher_id, k).decrypt(b"\x00" * 12, ct, None)


# ---------------------------------------------------------------- 写入

@dataclass
class EncryptParams:
    passwords: List[str] = field(default_factory=list)
    # 预派生 KEK（(名称, kek) 列表）。生产实现在批量加密时必须走这条路，
    # 避免对每个文件重复执行 Argon2。与 passwords 二选一或并用。
    keks: List[Tuple[str, bytes]] = field(default_factory=list)
    vault_salt: Optional[bytes] = None
    argon2_profile: str = "test"
    cipher_id: int = CIPHER_CHACHA20POLY1305
    chunk_size: int = 256 * 1024
    compress: bool = False
    zstd_level: int = 3
    encrypt_filename: bool = True
    preserve_ext: bool = False
    thumbnail: Optional[bytes] = None
    media_meta: Optional[dict] = None
    original_meta: Optional[dict] = None
    extra_tlv: List[Tuple[int, int, bytes]] = field(default_factory=list)
    # 仅测试：注入确定性随机源
    rng: Optional[object] = None

    def randbytes(self, n: int) -> bytes:
        if self.rng is not None:
            return self.rng(n)
        return os.urandom(n)


def encrypt(plaintext: bytes, filename: str, params: EncryptParams) -> bytes:
    prof = ARGON2_PROFILES[params.argon2_profile]
    vault_salt = params.vault_salt or params.randbytes(16)
    file_uuid = params.randbytes(16)
    base_nonce = params.randbytes(7)
    fek = params.randbytes(32)

    flags = 0
    if params.encrypt_filename:
        flags |= FLAG_FILENAME_ENCRYPTED
    if params.preserve_ext:
        flags |= FLAG_EXT_PRESERVED
    if params.compress:
        flags |= FLAG_COMPRESSED
    if params.thumbnail:
        flags |= FLAG_HAS_THUMBNAIL

    # ---- key slots：真实 slot 打散，空位填随机
    slots = [params.randbytes(SLOT_SIZE) for _ in range(SLOT_COUNT)]
    # 密码与预派生 KEK 统一成一个列表
    kek_list: List[bytes] = []
    for pw in params.passwords:
        kek_list.append(derive_kek_from_password(
            pw, vault_salt, prof["m_kib"], prof["t"], prof["p"]))
    for _name, k in params.keks:
        kek_list.append(k)

    for i, kek in enumerate(kek_list):
        if i >= SLOT_COUNT:
            raise ValueError("too many credentials for slot area")
        wk = slot_wrap_key(kek, file_uuid, i)
        slots[i] = aead_new(params.cipher_id, wk).encrypt(b"\x00" * 12, fek, None)
        assert len(slots[i]) == SLOT_SIZE
    slot_area = b"".join(slots)

    # ---- 载荷分块
    payload_key = hkdf(fek, salt=file_uuid, info=INFO_PAYLOAD)
    aead = aead_new(params.cipher_id, payload_key)
    cs = params.chunk_size
    n_chunks = max(1, (len(plaintext) + cs - 1) // cs)

    cctx = zstd.ZstdCompressor(level=params.zstd_level) if params.compress else None
    payload = bytearray()
    index_entries = []          # (密文相对偏移, 密文长度, 明文长度)
    for i in range(n_chunks):
        raw = plaintext[i * cs:(i + 1) * cs]
        data = cctx.compress(raw) if cctx else raw
        is_final = (i == n_chunks - 1)
        ct = aead.encrypt(chunk_nonce(base_nonce, i, is_final), data,
                          chunk_aad(file_uuid, i))
        index_entries.append((len(payload), len(ct), len(raw)))
        payload += ct

    # ---- TLV
    tlvs: List[Tuple[int, int, bytes]] = []
    if params.encrypt_filename:
        name_for_store = filename
        if params.preserve_ext:
            stem, dot, ext = filename.rpartition(".")
            if dot:
                name_for_store = stem
                tlvs.append((TLV_PLAIN_EXT, 0, ext.encode()))
        # padding 到 64 字节桶，隐藏文件名长度。
        # 注意：桶必须基于 (2 字节长度前缀 + 名字) 的总长计算，否则
        # len=63 这类边界会算出负填充，padded 长度越出桶边界并泄露长度特征。
        nb = name_for_store.encode("utf-8")
        need = 2 + len(nb)
        bucket = ((need + 63) // 64) * 64
        padded = struct.pack("<H", len(nb)) + nb + b"\x00" * (bucket - need)
        assert len(padded) == bucket and bucket % 64 == 0
        tlvs.append((TLV_FILENAME, TLV_FLAG_CRITICAL | TLV_FLAG_ENCRYPTED,
                     tlv_encrypt_value(fek, params.cipher_id, TLV_FILENAME, padded)))

    if params.compress:
        idx = bytearray(struct.pack("<I", len(index_entries)))
        for off, clen, plen in index_entries:
            idx += struct.pack("<QII", off, clen, plen)
        tlvs.append((TLV_COMPRESSION_INDEX, TLV_FLAG_CRITICAL | TLV_FLAG_ENCRYPTED,
                     tlv_encrypt_value(fek, params.cipher_id,
                                       TLV_COMPRESSION_INDEX, bytes(idx))))

    if params.thumbnail:
        tlvs.append((TLV_THUMBNAIL, TLV_FLAG_ENCRYPTED,
                     tlv_encrypt_value(fek, params.cipher_id,
                                       TLV_THUMBNAIL, params.thumbnail)))
    if params.media_meta:
        tlvs.append((TLV_MEDIA_META, TLV_FLAG_ENCRYPTED,
                     tlv_encrypt_value(fek, params.cipher_id, TLV_MEDIA_META,
                                       json.dumps(params.media_meta,
                                                  sort_keys=True).encode())))
    if params.original_meta:
        tlvs.append((TLV_ORIGINAL_META, TLV_FLAG_ENCRYPTED,
                     tlv_encrypt_value(fek, params.cipher_id, TLV_ORIGINAL_META,
                                       json.dumps(params.original_meta,
                                                  sort_keys=True).encode())))
    # 内容哈希，用于解密后完整性自检
    tlvs.append((TLV_CONTENT_HASH, TLV_FLAG_ENCRYPTED,
                 tlv_encrypt_value(fek, params.cipher_id, TLV_CONTENT_HASH,
                                   hashlib.blake2b(plaintext, digest_size=32).digest())))
    tlvs.extend(params.extra_tlv)

    tlv_blob = tlv_encode(tlvs)
    header_len = FIXED_HEADER_LEN + SLOT_AREA_LEN + len(tlv_blob) + HEADER_MAC_LEN

    fixed = bytearray(FIXED_HEADER_LEN)
    struct.pack_into("<8sHHI", fixed, 0, MAGIC, VERSION_MAJOR, VERSION_MINOR, header_len)
    fixed[16:32] = file_uuid
    fixed[32:48] = vault_salt
    struct.pack_into("<I", fixed, 48, flags)
    fixed[52] = params.cipher_id
    fixed[53] = KDF_ARGON2ID
    fixed[54] = COMPRESS_ZSTD if params.compress else COMPRESS_NONE
    fixed[55] = SLOT_COUNT
    struct.pack_into("<III", fixed, 56, prof["m_kib"], prof["t"], prof["p"])
    struct.pack_into("<I", fixed, 68, cs)
    struct.pack_into("<Q", fixed, 72, len(plaintext))
    fixed[80:87] = base_nonce
    fixed[87] = 0
    struct.pack_into("<I", fixed, 88, len(tlv_blob))
    struct.pack_into("<I", fixed, 92, 0)

    mac_key = hkdf(fek, salt=file_uuid, info=INFO_HEADER_MAC)
    mac = hmac.new(mac_key, bytes(fixed) + slot_area + tlv_blob, hashlib.sha256).digest()

    return bytes(fixed) + slot_area + tlv_blob + mac + bytes(payload)


# ---------------------------------------------------------------- 读取

@dataclass
class ParsedHeader:
    version: Tuple[int, int]
    header_len: int
    file_uuid: bytes
    vault_salt: bytes
    flags: int
    cipher_id: int
    kdf_id: int
    compress_id: int
    slot_count: int
    argon2: Tuple[int, int, int]
    chunk_size: int
    plaintext_size: int
    base_nonce: bytes
    tlv_len: int
    slot_area: bytes
    tlv_blob: bytes
    mac: bytes


def is_cvault(prefix: bytes) -> bool:
    return prefix[:8] == MAGIC


def parse_header(blob: bytes) -> ParsedHeader:
    """只需读文件头部若干 KB，不依赖文件总大小"""
    if len(blob) < FIXED_HEADER_LEN:
        raise ValueError("too short")
    magic, vmaj, vmin, header_len = struct.unpack_from("<8sHHI", blob, 0)
    if magic != MAGIC:
        raise ValueError("bad magic")
    if vmaj > VERSION_MAJOR:
        raise ValueError(f"unsupported major version {vmaj}")
    file_uuid = blob[16:32]
    vault_salt = blob[32:48]
    (flags,) = struct.unpack_from("<I", blob, 48)
    cipher_id, kdf_id, compress_id, slot_count = blob[52], blob[53], blob[54], blob[55]
    m_kib, t, p = struct.unpack_from("<III", blob, 56)
    (chunk_size,) = struct.unpack_from("<I", blob, 68)
    (plaintext_size,) = struct.unpack_from("<Q", blob, 72)
    base_nonce = blob[80:87]
    (tlv_len,) = struct.unpack_from("<I", blob, 88)
    if len(blob) < header_len:
        raise ValueError("header incomplete; read more bytes")
    sa = FIXED_HEADER_LEN
    slot_area = blob[sa:sa + SLOT_SIZE * slot_count]
    tb = sa + SLOT_SIZE * slot_count
    tlv_blob = blob[tb:tb + tlv_len]
    mac = blob[tb + tlv_len:tb + tlv_len + HEADER_MAC_LEN]
    return ParsedHeader((vmaj, vmin), header_len, file_uuid, vault_salt, flags,
                        cipher_id, kdf_id, compress_id, slot_count,
                        (m_kib, t, p), chunk_size, plaintext_size,
                        base_nonce, tlv_len, slot_area, tlv_blob, mac)


def try_unlock(h: ParsedHeader, candidates: List[SlotCandidate]) -> Optional[Tuple[bytes, str, int]]:
    """对每个候选 KEK × 每个 slot 尝试解包。纯 HKDF+AEAD，微秒级。"""
    for cand in candidates:
        for i in range(h.slot_count):
            wk = slot_wrap_key(cand.kek, h.file_uuid, i)
            ct = h.slot_area[i * SLOT_SIZE:(i + 1) * SLOT_SIZE]
            try:
                fek = aead_new(h.cipher_id, wk).decrypt(b"\x00" * 12, ct, None)
                return fek, cand.name, i
            except Exception:
                continue
    return None


def verify_header_mac(h: ParsedHeader, fek: bytes, raw_header: bytes) -> bool:
    mac_key = hkdf(fek, salt=h.file_uuid, info=INFO_HEADER_MAC)
    body = raw_header[:h.header_len - HEADER_MAC_LEN]
    expect = hmac.new(mac_key, body, hashlib.sha256).digest()
    return hmac.compare_digest(expect, h.mac)


def read_tlvs(h: ParsedHeader, fek: bytes) -> Dict[int, bytes]:
    out = {}
    for t, flags, value in tlv_decode(h.tlv_blob):
        if flags & TLV_FLAG_ENCRYPTED:
            try:
                value = tlv_decrypt_value(fek, h.cipher_id, t, value)
            except Exception:
                if flags & TLV_FLAG_CRITICAL:
                    raise ValueError(f"critical TLV {t:#06x} undecryptable")
                continue
        out[t] = value
    return out


def get_filename(h: ParsedHeader, tlvs: Dict[int, bytes]) -> Optional[str]:
    if TLV_FILENAME not in tlvs:
        return None
    raw = tlvs[TLV_FILENAME]
    (ln,) = struct.unpack_from("<H", raw, 0)
    name = raw[2:2 + ln].decode("utf-8")
    if h.flags & FLAG_EXT_PRESERVED and TLV_PLAIN_EXT in tlvs:
        name += "." + tlvs[TLV_PLAIN_EXT].decode()
    return name


class ChunkReader:
    """把"明文字节区间"翻译成"需要解密哪些块"——这是 Range 播放的核心。"""

    def __init__(self, blob: bytes, h: ParsedHeader, fek: bytes):
        self.blob, self.h = blob, h
        self.payload_key = hkdf(fek, salt=h.file_uuid, info=INFO_PAYLOAD)
        self.aead = aead_new(h.cipher_id, self.payload_key)
        self.n_chunks = max(1, (h.plaintext_size + h.chunk_size - 1) // h.chunk_size)
        self.index = None
        if h.compress_id == COMPRESS_ZSTD:
            tlvs = read_tlvs(h, fek)
            raw = tlvs[TLV_COMPRESSION_INDEX]
            (n,) = struct.unpack_from("<I", raw, 0)
            self.index = [struct.unpack_from("<QII", raw, 4 + i * 16) for i in range(n)]
            self.n_chunks = n
        self.dctx = zstd.ZstdDecompressor() if h.compress_id == COMPRESS_ZSTD else None

    def chunk_ct_range(self, i: int) -> Tuple[int, int]:
        """块 i 的密文在文件中的 [起始, 长度)。未压缩时 O(1) 线性计算。"""
        if self.index is not None:
            off, clen, _ = self.index[i]
            return self.h.header_len + off, clen
        stride = self.h.chunk_size + TAG_LEN
        start = self.h.header_len + i * stride
        remaining = self.h.plaintext_size - i * self.h.chunk_size
        n = min(self.h.chunk_size, remaining) + TAG_LEN
        return start, n

    def read_chunk(self, i: int) -> bytes:
        start, n = self.chunk_ct_range(i)
        ct = self.blob[start:start + n]
        pt = self.aead.decrypt(chunk_nonce(self.h.base_nonce, i, i == self.n_chunks - 1),
                               ct, chunk_aad(self.h.file_uuid, i))
        return self.dctx.decompress(pt) if self.dctx else pt

    def read_range(self, offset: int, length: int) -> bytes:
        """模拟 HTTP Range：只解密覆盖到的块"""
        end = min(offset + length, self.h.plaintext_size)
        if offset >= end:
            return b""
        if self.index is not None:
            out, cur = bytearray(), 0
            for i, (_, _, plen) in enumerate(self.index):
                if cur + plen > offset and cur < end:
                    c = self.read_chunk(i)
                    lo, hi = max(0, offset - cur), min(plen, end - cur)
                    out += c[lo:hi]
                cur += plen
                if cur >= end:
                    break
            return bytes(out)
        cs = self.h.chunk_size
        first, last = offset // cs, (end - 1) // cs
        out = bytearray()
        for i in range(first, last + 1):
            c = self.read_chunk(i)
            lo = offset - i * cs if i == first else 0
            hi = end - i * cs if i == last else len(c)
            out += c[lo:hi]
        return bytes(out)

    def chunks_touched(self, offset: int, length: int) -> List[int]:
        end = min(offset + length, self.h.plaintext_size)
        if offset >= end:
            return []
        if self.index is not None:
            res, cur = [], 0
            for i, (_, _, plen) in enumerate(self.index):
                if cur + plen > offset and cur < end:
                    res.append(i)
                cur += plen
            return res
        cs = self.h.chunk_size
        return list(range(offset // cs, (end - 1) // cs + 1))

    def read_all(self) -> bytes:
        return b"".join(self.read_chunk(i) for i in range(self.n_chunks))


# ---------------------------------------------------------------- 分片

def shard(blob: bytes, shard_size: int, redundant_header: bool = True) -> List[bytes]:
    h = parse_header(blob)
    header_copy = blob[:h.header_len] if redundant_header else b""
    body = blob
    total = max(1, (len(body) + shard_size - 1) // shard_size)
    out = []
    for i in range(total):
        data = body[i * shard_size:(i + 1) * shard_size]
        sh = bytearray(SHARD_HEADER_LEN)
        sh[0:8] = SHARD_MAGIC
        sh[8:24] = h.file_uuid
        struct.pack_into("<II", sh, 24, i, total)
        struct.pack_into("<QQ", sh, 32, i * shard_size, len(data))
        struct.pack_into("<I", sh, 48, zlib.crc32(data) & 0xFFFFFFFF)
        # 第 0 片本身就含完整头，无需冗余
        rh = header_copy if (redundant_header and i > 0) else b""
        struct.pack_into("<I", sh, 52, len(rh))
        out.append(bytes(sh) + rh + data)
    return out


def unshard(shards: List[bytes]) -> bytes:
    parsed = []
    for s in shards:
        if s[:8] != SHARD_MAGIC:
            raise ValueError("not a shard")
        uuid = s[8:24]
        idx, total = struct.unpack_from("<II", s, 24)
        off, ln = struct.unpack_from("<QQ", s, 32)
        (crc,) = struct.unpack_from("<I", s, 48)
        (rhl,) = struct.unpack_from("<I", s, 52)
        data = s[SHARD_HEADER_LEN + rhl:SHARD_HEADER_LEN + rhl + ln]
        if zlib.crc32(data) & 0xFFFFFFFF != crc:
            raise ValueError(f"shard {idx} CRC mismatch")
        parsed.append((idx, off, data, uuid, total))
    uuids = {p[3] for p in parsed}
    if len(uuids) != 1:
        raise ValueError("shards belong to different files")
    parsed.sort(key=lambda x: x[0])
    got = {p[0] for p in parsed}
    expect = set(range(parsed[0][4]))
    missing = sorted(expect - got)
    if missing:
        raise ValueError(f"missing shards: {missing}")
    return b"".join(p[2] for p in parsed)


def recover_header_from_shard(s: bytes) -> Optional[bytes]:
    """任意带冗余头的分片都能独立识别文件身份"""
    if s[:8] != SHARD_MAGIC:
        return None
    (rhl,) = struct.unpack_from("<I", s, 52)
    if rhl == 0:
        idx, _ = struct.unpack_from("<II", s, 24)
        return s[SHARD_HEADER_LEN:] if idx == 0 else None
    return s[SHARD_HEADER_LEN:SHARD_HEADER_LEN + rhl]
