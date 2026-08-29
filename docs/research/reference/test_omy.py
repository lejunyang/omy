#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""omy 格式设计自测：验证规范中的每一条断言。"""

import os, sys, time, json, struct, hashlib
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from omy_ref import *

PASS, FAIL = [], []

def check(name, cond, detail=""):
    (PASS if cond else FAIL).append(name)
    print(f"  [{'PASS' if cond else 'FAIL'}] {name}" + (f"  {detail}" if detail else ""))
    return cond

def unlock(blob, passwords, profile="test"):
    h = parse_header(blob)
    cands = [SlotCandidate(pw, derive_kek_from_password(
        pw, h.vault_salt, *h.argon2)) for pw in passwords]
    r = try_unlock(h, cands)
    return h, r

print("=" * 72)
print("§1  基础往返一致性")
print("=" * 72)
for size, label in [(0, "空文件"), (1, "1字节"), (255, "亚块"),
                    (65536, "恰好整块"), (65537, "整块+1"),
                    (700000, "多块"), (3_000_000, "大文件")]:
    data = os.urandom(size)
    p = EncryptParams(passwords=["pw1"], chunk_size=65536)
    blob = encrypt(data, f"{label}.bin", p)
    h, r = unlock(blob, ["pw1"])
    fek, _, _ = r
    got = ChunkReader(blob, h, fek).read_all()
    check(f"往返一致 {label} ({size}B)", got == data,
          f"密文 {len(blob)}B 膨胀 {len(blob)-size}B")

print()
print("=" * 72)
print("§2  随机访问（Range 播放的核心）")
print("=" * 72)
data = os.urandom(2_000_000)
p = EncryptParams(passwords=["pw1"], chunk_size=262144)
blob = encrypt(data, "video.mp4", p)
h, r = unlock(blob, ["pw1"]); fek, _, _ = r
cr = ChunkReader(blob, h, fek)

for off, ln, desc in [(0, 100, "开头"), (1_000_000, 5000, "中部"),
                      (1_999_000, 1000, "结尾"), (262143, 2, "跨块边界"),
                      (262144, 1, "块首字节"), (0, 2_000_000, "全量")]:
    check(f"Range {desc} @{off}+{ln}", cr.read_range(off, ln) == data[off:off+ln])

n = len(cr.chunks_touched(1_000_000, 5000))
check("跨块最少解密块数 == 1", n == 1, f"实际解密 {n} 块")
n2 = len(cr.chunks_touched(262143, 2))
check("跨边界解密块数 == 2", n2 == 2, f"实际解密 {n2} 块")

# 验证 O(1) 偏移公式
start, ln_ = cr.chunk_ct_range(5)
expect = h.header_len + 5 * (262144 + 16)
check("密文偏移线性公式正确", start == expect, f"{start} == {expect}")

print()
print("=" * 72)
print("§3  多密码 / key slot / 可否认性")
print("=" * 72)
vs = os.urandom(16)
p = EncryptParams(passwords=["alice", "bob", "carol"], vault_salt=vs)
blob = encrypt(b"shared secret payload", "doc.txt", p)
for pw in ["alice", "bob", "carol"]:
    h, r = unlock(blob, [pw])
    check(f"密码 {pw} 可解锁", r is not None)
h, r = unlock(blob, ["mallory"])
check("错误密码被拒绝", r is None)

h = parse_header(blob)
check("slot 区固定 8 槽", h.slot_count == SLOT_COUNT)
check("slot 区总长 384B", len(h.slot_area) == 384)

# 可否认性：1 个密码 vs 3 个密码，外观必须无差别
b1 = encrypt(b"x" * 100, "a.txt", EncryptParams(passwords=["one"], vault_salt=vs))
b3 = encrypt(b"x" * 100, "a.txt", EncryptParams(passwords=["1","2","3"], vault_salt=vs))
check("不同密码数 → 文件大小相同", len(b1) == len(b3), f"{len(b1)} vs {len(b3)}")
h1, h3 = parse_header(b1), parse_header(b3)
check("不同密码数 → header 长度相同", h1.header_len == h3.header_len)

# slot 区不可区分性：与等长纯随机数据做统计对照
# 注：384B 样本中不同字节值的理论期望覆盖率 = 1-(1-1/256)^384 ≈ 77.7%，
# 故不能用"接近 100%"作为判据，而应与纯随机基线对照。
def byte_entropy(b):
    from collections import Counter
    import math
    c = Counter(b); n = len(b)
    return -sum((v/n) * math.log2(v/n) for v in c.values())

real_ent = byte_entropy(h3.slot_area)
rand_ents = [byte_entropy(os.urandom(len(h3.slot_area))) for _ in range(30)]
lo, hi = min(rand_ents), max(rand_ents)
check("slot 区与纯随机不可区分（熵落在随机基线区间内）",
      lo <= real_ent <= hi,
      f"real={real_ent:.3f} bits/byte, 随机基线 [{lo:.3f}, {hi:.3f}]")

# 3 个真实 slot vs 5 个随机填充。
# 注：短样本香农熵估计对长度极敏感（144B/240B 对 256 个取值严重欠采样），
# 两段纯随机数据之间的熵差中位数就有 ~0.46，故必须与同长度随机对照组比较，
# 而不能用一个凭直觉设定的绝对阈值。
real_part = byte_entropy(h3.slot_area[:3 * SLOT_SIZE])
pad_part = byte_entropy(h3.slot_area[3 * SLOT_SIZE:])
observed_diff = abs(real_part - pad_part)
baseline = sorted(abs(byte_entropy(os.urandom(3 * SLOT_SIZE)) -
                      byte_entropy(os.urandom(5 * SLOT_SIZE)))
                  for _ in range(500))
p95 = baseline[int(len(baseline) * 0.95) - 1]
check("真实 slot 与随机填充不可区分（熵差在随机基线 p95 内）",
      observed_diff <= p95,
      f"观测熵差={observed_diff:.3f}, 随机基线 p95={p95:.3f}")

# 同 vault 不同文件的 slot 内容必须完全不同（防关联）
ba = encrypt(b"A"*50, "a", EncryptParams(passwords=["same"], vault_salt=vs))
bb = encrypt(b"B"*50, "b", EncryptParams(passwords=["same"], vault_salt=vs))
check("同密码不同文件 slot 不同（不可关联）",
      parse_header(ba).slot_area != parse_header(bb).slot_area)
check("同 vault 文件共享 vault_salt",
      parse_header(ba).vault_salt == parse_header(bb).vault_salt)

print()
print("=" * 72)
print("§4  篡改检测")
print("=" * 72)
data = os.urandom(300000)
blob = encrypt(data, "t.bin", EncryptParams(passwords=["pw"], chunk_size=65536))
h, r = unlock(blob, ["pw"]); fek, _, _ = r
check("原始 header MAC 有效", verify_header_mac(h, fek, blob))

# 篡改 Argon2 参数（降低强度攻击）
bad = bytearray(blob); struct.pack_into("<I", bad, 56, 8)
hb = parse_header(bytes(bad))
check("篡改 Argon2 参数被 MAC 检出", not verify_header_mac(hb, fek, bytes(bad)))

# 篡改 flags
bad = bytearray(blob); struct.pack_into("<I", bad, 48, 0xFFFF)
check("篡改 flags 被 MAC 检出",
      not verify_header_mac(parse_header(bytes(bad)), fek, bytes(bad)))

# 篡改载荷字节
bad = bytearray(blob); bad[h.header_len + 100] ^= 0xFF
try:
    ChunkReader(bytes(bad), parse_header(bytes(bad)), fek).read_chunk(0)
    check("篡改载荷被 AEAD 检出", False)
except Exception:
    check("篡改载荷被 AEAD 检出", True)

# 块重排攻击
stride = 65536 + 16
c0 = blob[h.header_len:h.header_len+stride]
c1 = blob[h.header_len+stride:h.header_len+2*stride]
sw = bytearray(blob)
sw[h.header_len:h.header_len+stride] = c1
sw[h.header_len+stride:h.header_len+2*stride] = c0
try:
    ChunkReader(bytes(sw), parse_header(bytes(sw)), fek).read_chunk(0)
    check("块重排被检出", False)
except Exception:
    check("块重排被检出", True)

# 截断攻击：删掉尾块后，最后一块的 final 标记不匹配
n_ch = (len(data) + 65535) // 65536
trunc = bytearray(blob[:h.header_len + (n_ch-1)*stride])
struct.pack_into("<Q", trunc, 72, (n_ch-1)*65536)
ht = parse_header(bytes(trunc))
try:
    ChunkReader(bytes(trunc), ht, fek).read_all()
    detected = False
except Exception:
    detected = True
check("截断被 final 标记检出", detected)

# 跨文件块移植
other = encrypt(os.urandom(300000), "o.bin",
                EncryptParams(passwords=["pw"], vault_salt=h.vault_salt, chunk_size=65536))
ho, ro = unlock(other, ["pw"]); feko, _, _ = ro
graft = bytearray(blob)
graft[h.header_len:h.header_len+stride] = other[ho.header_len:ho.header_len+stride]
try:
    ChunkReader(bytes(graft), parse_header(bytes(graft)), fek).read_chunk(0)
    check("跨文件块移植被 AAD 检出", False)
except Exception:
    check("跨文件块移植被 AAD 检出", True)

print()
print("=" * 72)
print("§5  文件名加密与元数据")
print("=" * 72)
for fn, keep in [("机密文档.docx", False), ("机密文档.docx", True),
                 ("noext", False), ("a"*200 + ".txt", True)]:
    p = EncryptParams(passwords=["pw"], encrypt_filename=True, preserve_ext=keep)
    b = encrypt(b"data", fn, p)
    h, r = unlock(b, ["pw"]); fek, _, _ = r
    got = get_filename(h, read_tlvs(h, fek))
    check(f"文件名还原 {'保留后缀' if keep else '全加密'} ({fn[:20]})", got == fn)

# 文件名长度 padding 效果
lens = set()
for fn in ["a.txt", "abcdefgh.txt", "中文名字文件.txt"]:
    b = encrypt(b"x", fn, EncryptParams(passwords=["pw"], vault_salt=vs))
    lens.add(parse_header(b).header_len)
check("短文件名 padding 到同一桶 → header 等长", len(lens) == 1, f"header 长度集合 {lens}")

# 缩略图 + 媒体元信息
thumb = os.urandom(8000)
meta = {"width":1920,"height":1080,"duration":123.4,"codec":"h264"}
p = EncryptParams(passwords=["pw"], thumbnail=thumb, media_meta=meta)
b = encrypt(os.urandom(50000), "v.mp4", p)
h, r = unlock(b, ["pw"]); fek, _, _ = r
t = read_tlvs(h, fek)
check("缩略图还原", t[TLV_THUMBNAIL] == thumb)
check("媒体元信息还原", json.loads(t[TLV_MEDIA_META]) == meta)
check("HAS_THUMBNAIL flag 置位", bool(h.flags & FLAG_HAS_THUMBNAIL))
check("列表页只需读 header（不碰载荷）", h.header_len < 20000,
      f"header {h.header_len}B / 全文件 {len(b)}B")

print()
print("=" * 72)
print("§6  压缩 + 随机访问")
print("=" * 72)
text = ("omy 设计文档测试语料。" * 3000).encode("utf-8")
raw = encrypt(text, "t.txt", EncryptParams(passwords=["pw"], chunk_size=65536))
comp = encrypt(text, "t.txt", EncryptParams(passwords=["pw"], chunk_size=65536,
                                            compress=True, zstd_level=3))
check("压缩后体积显著减小", len(comp) < len(raw) * 0.3,
      f"{len(raw)} → {len(comp)} ({len(comp)/len(raw):.1%})")
h, r = unlock(comp, ["pw"]); fek, _, _ = r
cr = ChunkReader(comp, h, fek)
check("压缩文件往返一致", cr.read_all() == text)
check("压缩文件 Range 正确", cr.read_range(50000, 3000) == text[50000:53000])
check("压缩文件索引表已建立", cr.index is not None and len(cr.index) > 1,
      f"{len(cr.index) if cr.index else 0} 块")

# 块大小对压缩率的影响（验证"加大块能挽回压缩率"）
print("\n  块大小 vs 压缩率：")
big = os.urandom(4096) * 500     # 2MB 高重复度语料
sizes = {}
for cs in [65536, 262144, 1048576, 4194304]:
    b = encrypt(big, "x.bin", EncryptParams(passwords=["pw"], chunk_size=cs,
                                            compress=True, zstd_level=3))
    sizes[cs] = len(b)
    print(f"    chunk={cs//1024:>5}KiB → {len(b):>9,}B  ({len(b)/len(big):.2%})")
check("加大块提升压缩率", sizes[4194304] < sizes[65536],
      f"4MiB 块比 64KiB 块小 {(1-sizes[4194304]/sizes[65536]):.1%}")

print()
print("=" * 72)
print("§7  分片")
print("=" * 72)
data = os.urandom(1_000_000)
blob = encrypt(data, "big.bin", EncryptParams(passwords=["pw"], chunk_size=65536))
shards = shard(blob, 300_000, redundant_header=True)
check("分片数量正确", len(shards) == 4, f"{len(shards)} 片")
merged = unshard(shards)
check("分片合并 == 原密文", merged == blob)
h, r = unlock(merged, ["pw"]); fek, _, _ = r
check("合并后可正常解密", ChunkReader(merged, h, fek).read_all() == data)

import random
sh2 = shards[:]; random.shuffle(sh2)
check("乱序分片可自动重组", unshard(sh2) == blob)

for i in [1, 2, 3]:
    rh = recover_header_from_shard(shards[i])
    ok = rh is not None and is_omy(rh)
    check(f"第 {i} 片含冗余头可独立识别", ok)

try:
    unshard([shards[0], shards[2], shards[3]])
    check("缺片被检出", False)
except ValueError as e:
    check("缺片被检出", "missing" in str(e), str(e))

bad = bytearray(shards[1]); bad[-1] ^= 0xFF
try:
    unshard([shards[0], bytes(bad), shards[2], shards[3]])
    check("分片 CRC 损坏被检出", False)
except ValueError as e:
    check("分片 CRC 损坏被检出", "CRC" in str(e))

print()
print("=" * 72)
print("§8  两级 KDF 性能（扫描可行性的关键证据）")
print("=" * 72)
prof = ARGON2_PROFILES["interactive"]
salt = os.urandom(16)
t0 = time.perf_counter()
kek = derive_kek_from_password("benchmark-pw", salt, prof["m_kib"], prof["t"], prof["p"])
t_argon = time.perf_counter() - t0
print(f"  Argon2id(m=64MiB,t=3,p=1) 单次: {t_argon*1000:.1f} ms")

uuid = os.urandom(16)
N = 2000
t0 = time.perf_counter()
for i in range(N):
    slot_wrap_key(kek, uuid, i % 8)
t_hkdf = (time.perf_counter() - t0) / N
print(f"  HKDF 单次: {t_hkdf*1e6:.1f} µs")
check("HKDF 比 Argon2 快 3 个数量级以上", t_argon / t_hkdf > 1000,
      f"比值 {t_argon/t_hkdf:,.0f}x")

# 真实扫描模拟：加密与扫描使用同一档 Argon2 参数（interactive）
NF, NP = 500, 5
vs2 = os.urandom(16)
pws = [f"pw{i}" for i in range(NP)]
# 生产实现的正确姿势：批量加密时复用已派生的 KEK，而非每个文件重跑 Argon2
t0 = time.perf_counter()
keks = [SlotCandidate(pw, derive_kek_from_password(pw, vs2, prof["m_kib"],
                                                   prof["t"], prof["p"])) for pw in pws]
t_derive = time.perf_counter() - t0
files = [encrypt(os.urandom(1000), f"f{i}.bin",
                 EncryptParams(keks=[(keks[i % NP].name, keks[i % NP].kek)],
                               vault_salt=vs2, argon2_profile="interactive"))
         for i in range(NF)]
t0 = time.perf_counter()
hits = sum(1 for f in files if try_unlock(parse_header(f), keks) is not None)
t_scan = time.perf_counter() - t0
print(f"  {NP} 个密码派生 KEK: {t_derive*1000:.0f} ms（每会话一次，可缓存）")
print(f"  扫描 {NF} 文件 × {NP} 密码: {t_scan*1000:.0f} ms，命中 {hits}")
print(f"  → 推算 10000 文件: {t_scan/NF*10000:.1f} s")
naive = t_argon * NF * NP
print(f"  → 对比每文件独立 Argon2 方案: {naive:.0f} s（慢 {naive/t_scan:,.0f}x）")
check("全部文件命中", hits == NF, f"{hits}/{NF}")
check("500文件×5密码扫描 < 3s", t_scan < 3.0, f"{t_scan:.2f}s")

# 非本 vault 成员不应命中
outsider = encrypt(b"x", "out.bin",
                   EncryptParams(passwords=["outsider"], vault_salt=os.urandom(16),
                                 argon2_profile="interactive"))
check("非本 vault 文件不误命中",
      try_unlock(parse_header(outsider), keks) is None)

print()
print("=" * 72)
print("§9  跨 vault 隔离 / 版本兼容")
print("=" * 72)
va, vb = os.urandom(16), os.urandom(16)
fa = encrypt(b"vault A", "a", EncryptParams(passwords=["shared"], vault_salt=va))
ha = parse_header(fa)
kb = derive_kek_from_password("shared", vb, *ha.argon2)
check("同密码不同 vault 无法解锁",
      try_unlock(ha, [SlotCandidate("wrong-vault", kb)]) is None)

bad = bytearray(fa); struct.pack_into("<H", bad, 8, 99)
try:
    parse_header(bytes(bad)); check("未来 major 版本被拒绝", False)
except ValueError as e:
    check("未来 major 版本被拒绝", "unsupported" in str(e))

bad = bytearray(fa); struct.pack_into("<H", bad, 10, 99)
try:
    parse_header(bytes(bad)); check("未知 minor 版本可容忍", True)
except Exception:
    check("未知 minor 版本可容忍", False)

# 未知非关键 TLV 应被安全忽略
p = EncryptParams(passwords=["pw"], extra_tlv=[(0xF000, 0, b"future-extension")])
b = encrypt(b"data", "x", p)
h, r = unlock(b, ["pw"]); fek, _, _ = r
t = read_tlvs(h, fek)
check("未知非关键 TLV 被安全忽略并保留", t.get(0xF000) == b"future-extension")

print()
print("=" * 72)
print("§10  AES-256-GCM 备选算法")
print("=" * 72)
data = os.urandom(200000)
b = encrypt(data, "aes.bin", EncryptParams(passwords=["pw"],
                                           cipher_id=CIPHER_AES256GCM, chunk_size=65536))
h, r = unlock(b, ["pw"]); fek, _, _ = r
check("AES-256-GCM 往返一致", ChunkReader(b, h, fek).read_all() == data)
check("cipher_id 正确记录", h.cipher_id == CIPHER_AES256GCM)

print()
print("=" * 72)
print(f"结果：{len(PASS)} 通过 / {len(FAIL)} 失败")
if FAIL:
    print("失败项：")
    for f in FAIL:
        print("  -", f)
    sys.exit(1)
print("全部通过 ✓")
