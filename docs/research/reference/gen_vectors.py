#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""生成确定性测试向量，供第三方实现对照验证。"""
import os, sys, json, hashlib, struct
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from omy_ref import *

class DetRNG:
    """确定性随机源：ChaCha 风格的 SHAKE 流，保证测试向量可复现。"""
    def __init__(self, seed: bytes):
        self.buf, self.ctr, self.seed = b"", 0, seed
    def __call__(self, n: int) -> bytes:
        while len(self.buf) < n:
            self.buf += hashlib.shake_256(
                self.seed + struct.pack("<Q", self.ctr)).digest(64)
            self.ctr += 1
        out, self.buf = self.buf[:n], self.buf[n:]
        return out

def hx(b): return b.hex()

vectors = []

# ---- 向量 1：最小文件，单密码，无压缩
rng = DetRNG(b"omy-test-vector-001")
params = EncryptParams(passwords=["correct horse battery staple"],
                       argon2_profile="test", chunk_size=65536, rng=rng)
pt = b"Hello, omy!"
blob = encrypt(pt, "hello.txt", params)
h = parse_header(blob)
kek = derive_kek_from_password("correct horse battery staple", h.vault_salt, *h.argon2)
fek, _, si = try_unlock(h, [SlotCandidate("p", kek)])
vectors.append({
    "name": "v1-minimal-single-password",
    "description": "最小文件，单密码，ChaCha20-Poly1305，无压缩，文件名加密",
    "input": {
        "plaintext_utf8": pt.decode(), "plaintext_hex": hx(pt),
        "filename": "hello.txt",
        "password": "correct horse battery staple",
        "argon2": {"m_kib": h.argon2[0], "t": h.argon2[1], "p": h.argon2[2],
                   "note": "测试用弱参数，生产环境禁用"},
        "cipher": "XChaCha20-Poly1305(参考实现用ChaCha20-Poly1305)",
        "chunk_size": h.chunk_size,
    },
    "derived": {
        "file_uuid": hx(h.file_uuid), "vault_salt": hx(h.vault_salt),
        "base_nonce": hx(h.base_nonce), "kek": hx(kek), "fek": hx(fek),
        "slot_index_hit": si,
        "payload_key": hx(hkdf(fek, h.file_uuid, INFO_PAYLOAD)),
        "header_mac_key": hx(hkdf(fek, h.file_uuid, INFO_HEADER_MAC)),
        "chunk0_nonce": hx(chunk_nonce(h.base_nonce, 0, True)),
        "chunk0_aad": hx(chunk_aad(h.file_uuid, 0)),
    },
    "output": {
        "total_size": len(blob), "header_len": h.header_len,
        "tlv_len": h.tlv_len,
        "full_file_hex": hx(blob),
        "blake2b256": hashlib.blake2b(blob, digest_size=32).hexdigest(),
    },
})

# ---- 向量 2：多密码 + 可否认性
rng = DetRNG(b"omy-test-vector-002")
vs = rng(16)
params = EncryptParams(passwords=["alpha", "beta", "gamma"], vault_salt=vs,
                       argon2_profile="test", rng=DetRNG(b"omy-test-vector-002b"))
blob2 = encrypt(b"multi-recipient payload", "shared.dat", params)
h2 = parse_header(blob2)
slot_info = []
for pw in ["alpha", "beta", "gamma", "wrong"]:
    k = derive_kek_from_password(pw, h2.vault_salt, *h2.argon2)
    r = try_unlock(h2, [SlotCandidate(pw, k)])
    slot_info.append({"password": pw, "kek": hx(k),
                      "unlocked": r is not None,
                      "slot_index": r[2] if r else None})
vectors.append({
    "name": "v2-multi-password-deniable",
    "description": "3 个密码共享同一 FEK；slot 区固定 8 槽，空槽填随机，外部不可区分",
    "input": {"passwords": ["alpha","beta","gamma"], "filename": "shared.dat",
              "plaintext_utf8": "multi-recipient payload"},
    "derived": {"vault_salt": hx(h2.vault_salt), "file_uuid": hx(h2.file_uuid),
                "slot_results": slot_info,
                "slot_area_hex": hx(h2.slot_area)},
    "output": {"total_size": len(blob2), "header_len": h2.header_len,
               "full_file_hex": hx(blob2),
               "blake2b256": hashlib.blake2b(blob2, digest_size=32).hexdigest()},
})

# ---- 向量 3：多块 + Range 随机访问
rng = DetRNG(b"omy-test-vector-003")
pt3 = bytes((i * 7 + 13) % 256 for i in range(10000))
params = EncryptParams(passwords=["pw3"], argon2_profile="test",
                       chunk_size=4096, rng=rng)
blob3 = encrypt(pt3, "pattern.bin", params)
h3 = parse_header(blob3)
kek3 = derive_kek_from_password("pw3", h3.vault_salt, *h3.argon2)
fek3, _, _ = try_unlock(h3, [SlotCandidate("p", kek3)])
cr3 = ChunkReader(blob3, h3, fek3)
ranges = []
for off, ln in [(0,16),(4095,2),(4096,16),(8192,100),(9990,10)]:
    start, clen = cr3.chunk_ct_range(off // 4096)
    ranges.append({"plain_offset": off, "length": ln,
                   "expected_hex": hx(pt3[off:off+ln]),
                   "chunks_touched": cr3.chunks_touched(off, ln),
                   "first_chunk_ct_offset": start,
                   "first_chunk_ct_len": clen})
vectors.append({
    "name": "v3-multichunk-random-access",
    "description": "10000B / 4096B 块 = 3 块；验证明文偏移→密文偏移的 O(1) 线性映射",
    "input": {"password": "pw3", "filename": "pattern.bin",
              "plaintext_rule": "byte[i] = (i*7+13) mod 256, i in [0,10000)",
              "plaintext_blake2b256": hashlib.blake2b(pt3, digest_size=32).hexdigest(),
              "chunk_size": 4096},
    "derived": {"file_uuid": hx(h3.file_uuid), "fek": hx(fek3),
                "n_chunks": cr3.n_chunks,
                "formula": "ct_offset(i) = header_len + i*(chunk_size+16)",
                "header_len": h3.header_len},
    "range_tests": ranges,
    "output": {"total_size": len(blob3),
               "blake2b256": hashlib.blake2b(blob3, digest_size=32).hexdigest()},
})

# ---- 向量 4：压缩 + 索引表
rng = DetRNG(b"omy-test-vector-004")
pt4 = ("omy" * 500).encode()
params = EncryptParams(passwords=["pw4"], argon2_profile="test",
                       chunk_size=2048, compress=True, zstd_level=3, rng=rng)
blob4 = encrypt(pt4, "repeat.txt", params)
h4 = parse_header(blob4)
kek4 = derive_kek_from_password("pw4", h4.vault_salt, *h4.argon2)
fek4, _, _ = try_unlock(h4, [SlotCandidate("p", kek4)])
cr4 = ChunkReader(blob4, h4, fek4)
vectors.append({
    "name": "v4-compressed-with-index",
    "description": "zstd 分块压缩；块长可变，故必须依赖加密索引表定位",
    "input": {"password": "pw4", "plaintext": "'omy' 重复 500 次",
              "plaintext_size": len(pt4), "chunk_size": 2048, "zstd_level": 3},
    "derived": {"fek": hx(fek4), "n_chunks": cr4.n_chunks,
                "index_entries": [{"ct_offset": e[0], "ct_len": e[1],
                                   "plain_len": e[2]} for e in cr4.index]},
    "output": {"total_size": len(blob4),
               "compression_ratio": f"{len(blob4)/len(pt4):.4f}",
               "roundtrip_ok": cr4.read_all() == pt4,
               "blake2b256": hashlib.blake2b(blob4, digest_size=32).hexdigest()},
})

# ---- 向量 5：分片
rng = DetRNG(b"omy-test-vector-005")
pt5 = bytes(range(256)) * 40
params = EncryptParams(passwords=["pw5"], argon2_profile="test",
                       chunk_size=4096, rng=rng)
blob5 = encrypt(pt5, "sharded.bin", params)
shards5 = shard(blob5, 4000, redundant_header=True)
vectors.append({
    "name": "v5-sharding",
    "description": "分片格式；每片 56B 片头 + 可选冗余主头；乱序/缺片可检测",
    "input": {"password": "pw5", "plaintext_size": len(pt5), "shard_size": 4000,
              "redundant_header": True},
    "output": {
        "original_ct_size": len(blob5), "shard_count": len(shards5),
        "shards": [{"index": i, "total_size": len(s),
                    "magic": s[:8].decode("latin1"),
                    "file_uuid": hx(s[8:24]),
                    "shard_index": struct.unpack_from("<I", s, 24)[0],
                    "shard_total": struct.unpack_from("<I", s, 28)[0],
                    "data_offset": struct.unpack_from("<Q", s, 32)[0],
                    "data_len": struct.unpack_from("<Q", s, 40)[0],
                    "crc32": f"{struct.unpack_from('<I', s, 48)[0]:08x}",
                    "redundant_header_len": struct.unpack_from("<I", s, 52)[0],
                    "blake2b256": hashlib.blake2b(s, digest_size=32).hexdigest()}
                   for i, s in enumerate(shards5)],
        "merge_ok": unshard(shards5) == blob5,
    },
})

out = {
    "format": "omy .omy",
    "spec_version": "1.0",
    "generated_by": "reference/gen_vectors.py",
    "warning": ("测试向量使用 Argon2 弱参数(m=64KiB,t=1,p=1)以便快速复现；"
                "生产实现必须使用 §KDF 档位表中的参数。"),
    "primitives": {
        "aead_reference": "ChaCha20-Poly1305 (RFC 8439)",
        "aead_production": "XChaCha20-Poly1305 (默认) / AES-256-GCM (可选)",
        "kdf_slow": "Argon2id (RFC 9106)",
        "kdf_fast": "HKDF-SHA256 (RFC 5869)",
        "header_mac": "HMAC-SHA256",
        "content_hash": "BLAKE2b-256",
        "shard_checksum": "CRC-32 (IEEE)",
    },
    "vectors": vectors,
}
path = os.path.join(os.path.dirname(os.path.abspath(__file__)),
                    "..", "appendix", "test-vectors.json")
with open(path, "w", encoding="utf-8", newline="\n") as f:
    json.dump(out, f, indent=2, ensure_ascii=False)
    f.write("\n")
print(f"已写出 {len(vectors)} 组测试向量 -> {os.path.normpath(path)}")
for v in vectors:
    print(f"  - {v['name']}")
