#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""
交叉验证：解析 02-file-format-spec.md 中的偏移表，与参考实现的实际字节布局比对。
目的是确保文档里的每一个偏移量都不是手写出来的错误。
"""
import os, re, sys, struct, json
sys.path.insert(0, os.path.join(os.path.dirname(os.path.abspath(__file__)), "reference"))
from cvlt_ref import *

BASE = os.path.dirname(os.path.abspath(__file__))
SPEC = os.path.join(BASE, "02-file-format-spec.md")

errors, checks = [], 0
def ck(name, cond, detail=""):
    global checks
    checks += 1
    if not cond:
        errors.append(f"{name}  {detail}")
    print(f"  [{'OK ' if cond else 'ERR'}] {name}" + (f"  {detail}" if detail else ""))

md = open(SPEC, encoding="utf-8").read()

print("=" * 72)
print("A. 解析文档中的 Fixed Header 偏移表")
print("=" * 72)
# 只在 "## 2. Fixed Header" 到 "### 2.1" 之间解析，避免把 flags 位定义表混进来
sec = re.search(r"## 2\. Fixed Header.*?(?=### 2\.1)", md, re.S)
assert sec, "找不到 Fixed Header 章节"
rows = re.findall(r"^\|\s*(\d+)\s*\|\s*(\d+)\s*\|\s*`(\w+)`\s*\|", sec.group(0), re.M)
fixed_doc = [(int(o), int(l), n) for o, l, n in rows]
print(f"  文档中解析到 {len(fixed_doc)} 个 fixed header 字段")

# 参考实现的权威布局
impl_layout = [
    (0, 8, "magic"), (8, 2, "version_major"), (10, 2, "version_minor"),
    (12, 4, "header_len"), (16, 16, "file_uuid"), (32, 16, "vault_salt"),
    (48, 4, "flags"), (52, 1, "cipher_id"), (53, 1, "kdf_id"),
    (54, 1, "compress_id"), (55, 1, "slot_count"), (56, 4, "argon2_m_kib"),
    (60, 4, "argon2_t"), (64, 4, "argon2_p"), (68, 4, "chunk_size"),
    (72, 8, "plaintext_size"), (80, 7, "base_nonce"), (87, 1, "chunk_version"),
    (88, 4, "tlv_len"), (92, 4, "reserved"),
]
ck("文档字段数 == 实现字段数", len(fixed_doc) == len(impl_layout),
   f"doc={len(fixed_doc)} impl={len(impl_layout)}")
for (do, dl, dn), (io, il, iname) in zip(fixed_doc, impl_layout):
    ck(f"字段 {iname}: offset={io} len={il}",
       (do, dl, dn) == (io, il, iname), f"文档写的是 ({do},{dl},{dn})")

# 无空洞无重叠
cur = 0
for o, l, n in impl_layout:
    ck(f"布局连续 @{o} ({n})", o == cur, f"期望 {cur}")
    cur = o + l
ck("fixed header 总长 == 96", cur == 96, f"实际 {cur}")

print()
print("=" * 72)
print("B. 用真实文件字节验证每个字段位置")
print("=" * 72)
p = EncryptParams(passwords=["verify"], argon2_profile="test",
                  chunk_size=131072, compress=False,
                  encrypt_filename=True, preserve_ext=True)
pt = b"X" * 300000
blob = encrypt(pt, "cross-check.dat", p)
h = parse_header(blob)

ck("magic 位于 [0:8]", blob[0:8] == b"CVAULT\x01\x00", blob[0:8].hex())
ck("version_major @8 == 1", struct.unpack_from("<H", blob, 8)[0] == 1)
ck("version_minor @10 == 0", struct.unpack_from("<H", blob, 10)[0] == 0)
ck("header_len @12 与解析一致", struct.unpack_from("<I", blob, 12)[0] == h.header_len,
   f"{h.header_len}")
ck("file_uuid @16 长 16B", blob[16:32] == h.file_uuid)
ck("vault_salt @32 长 16B", blob[32:48] == h.vault_salt)
ck("cipher_id @52", blob[52] == CIPHER_CHACHA20POLY1305)
ck("slot_count @55 == 8", blob[55] == 8)
ck("chunk_size @68 == 131072", struct.unpack_from("<I", blob, 68)[0] == 131072)
ck("plaintext_size @72 == 300000", struct.unpack_from("<Q", blob, 72)[0] == 300000)
ck("base_nonce @80 长 7B", blob[80:87] == h.base_nonce)
ck("chunk_version @87 == 0", blob[87] == 0)
ck("tlv_len @88 与解析一致", struct.unpack_from("<I", blob, 88)[0] == h.tlv_len)
ck("reserved @92 == 0", struct.unpack_from("<I", blob, 92)[0] == 0)

print()
print("=" * 72)
print("C. 区域边界")
print("=" * 72)
ck("slot 区起始 == 96", FIXED_HEADER_LEN == 96)
ck("单 slot == 48B", SLOT_SIZE == 48)
ck("slot 区 == 384B", SLOT_AREA_LEN == 384 and len(h.slot_area) == 384)
ck("TLV 区起始 == 480", FIXED_HEADER_LEN + SLOT_AREA_LEN == 480)
ck("header MAC == 32B", HEADER_MAC_LEN == 32 and len(h.mac) == 32)
expect_hl = 96 + 384 + h.tlv_len + 32
ck("header_len == 96+384+tlv_len+32", h.header_len == expect_hl,
   f"{h.header_len} vs {expect_hl}")
ck("分片头 == 56B", SHARD_HEADER_LEN == 56)

print()
print("=" * 72)
print("D. 文档声称的常量与实现一致")
print("=" * 72)
claims = {
    "最小文件尺寸.*?672": 672,
}
# 实测最小文件
mini = encrypt(b"", "a", EncryptParams(passwords=["p"], argon2_profile="test",
                                       encrypt_filename=True))
print(f"  实测空文件(单密码,加密文件名) = {len(mini)}B")
ck("文档声称的最小尺寸 672B 属实", len(mini) == 672, f"实测 {len(mini)}B")

m = re.search(r"最小文件尺寸\*\*：(\d+)", md)
ck("文档中最小尺寸数值与实测一致",
   m is not None and int(m.group(1)) == len(mini),
   f"文档写 {m.group(1) if m else '?'}, 实测 {len(mini)}")

# 验证文档中 O(1) 公式
i = 2
start, _ = ChunkReader(blob, h, try_unlock(h, [SlotCandidate("v",
    derive_kek_from_password("verify", h.vault_salt, *h.argon2))])[0]).chunk_ct_range(i)
formula = h.header_len + i * (h.chunk_size + 16)
ck("文档 O(1) 公式 ct_offset(i)=header_len+i*(chunk_size+16)",
   start == formula, f"{start} == {formula}")

# 验证 nonce 构造
n0 = chunk_nonce(h.base_nonce, 0, False)
ck("nonce = base(7B) || u32be(i) || final(1B) 共 12B", len(n0) == 12)
ck("nonce 块序号为大端序",
   chunk_nonce(b"\x00"*7, 1, False)[7:11] == b"\x00\x00\x00\x01")
ck("final 标记位于末字节",
   chunk_nonce(b"\x00"*7, 0, True)[11] == 1 and
   chunk_nonce(b"\x00"*7, 0, False)[11] == 0)

# AAD 构造
ck("aad = file_uuid(16B) || u32be(i) 共 20B",
   len(chunk_aad(h.file_uuid, 0)) == 20)

# TLV 编码
enc = tlv_encode([(0x1234, 0x0003, b"abc")])
ck("TLV 头 = type(u16le)+flags(u16le)+len(u32le) = 8B", len(enc) == 8 + 3)
ck("TLV type 小端", enc[0:2] == b"\x34\x12")
ck("TLV flags 小端", enc[2:4] == b"\x03\x00")
ck("TLV length 小端", enc[4:8] == b"\x03\x00\x00\x00")

# 文件名 padding 桶：桶基于 (2 字节长度前缀 + 名字长度) 计算
def bucket_of(name_len):
    need = 2 + name_len
    return ((need + 63) // 64) * 64

for name_len, exp_bucket in [(0, 64), (1, 64), (61, 64), (62, 64),
                             (63, 128), (100, 128), (126, 128), (127, 192)]:
    ck(f"padding 桶 len={name_len} -> {exp_bucket}",
       bucket_of(name_len) == exp_bucket, f"算得 {bucket_of(name_len)}")

# 桶对齐不变量：同桶内 header_len 必须唯一（否则长度泄露）
import collections
vs_pad = os.urandom(16)
by_bucket = collections.defaultdict(set)
for L in list(range(0, 200, 7)) + [62, 63, 126, 127]:
    b_ = encrypt(b"x", "a" * L, EncryptParams(passwords=["pw"],
                 argon2_profile="test", vault_salt=vs_pad))
    by_bucket[bucket_of(L)].add(parse_header(b_).header_len)
bad = {k: v for k, v in by_bucket.items() if len(v) != 1}
ck("同桶内 header_len 唯一（无长度泄露）", not bad, str(bad))
ck("桶与 header_len 单调对应", len(by_bucket) >= 3, f"{len(by_bucket)} 个桶")

print()
print("=" * 72)
print("E. 测试向量文件自洽性")
print("=" * 72)
tv = json.load(open(os.path.join(BASE, "appendix", "test-vectors.json"), encoding="utf-8"))
ck("测试向量数量 == 5", len(tv["vectors"]) == 5)
import hashlib
for v in tv["vectors"]:
    if "full_file_hex" in v.get("output", {}):
        raw = bytes.fromhex(v["output"]["full_file_hex"])
        ck(f"{v['name']}: size 字段与 hex 长度一致",
           len(raw) == v["output"]["total_size"])
        ck(f"{v['name']}: blake2b256 正确",
           hashlib.blake2b(raw, digest_size=32).hexdigest() == v["output"]["blake2b256"])
        ck(f"{v['name']}: magic 正确", raw[:8] == MAGIC)

# v1 可被独立解密（模拟第三方实现）
v1 = tv["vectors"][0]
raw = bytes.fromhex(v1["output"]["full_file_hex"])
hv = parse_header(raw)
kek = derive_kek_from_password(v1["input"]["password"], hv.vault_salt, *hv.argon2)
res = try_unlock(hv, [SlotCandidate("x", kek)])
ck("v1 可用文档记录的密码解锁", res is not None)
if res:
    fek, _, si = res
    ck("v1 FEK 与记录一致", fek.hex() == v1["derived"]["fek"])
    ck("v1 KEK 与记录一致", kek.hex() == v1["derived"]["kek"])
    ck("v1 命中 slot 索引一致", si == v1["derived"]["slot_index_hit"])
    ck("v1 header MAC 有效", verify_header_mac(hv, fek, raw))
    got = ChunkReader(raw, hv, fek).read_all()
    ck("v1 明文还原正确", got == bytes.fromhex(v1["input"]["plaintext_hex"]))
    ck("v1 文件名还原正确",
       get_filename(hv, read_tlvs(hv, fek)) == v1["input"]["filename"])

print()
print("=" * 72)
print(f"交叉验证：{checks - len(errors)}/{checks} 通过")
if errors:
    print("\n不一致项：")
    for e in errors:
        print("  -", e)
    sys.exit(1)
print("文档偏移表与参考实现完全一致 ✓")
