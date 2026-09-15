#!/usr/bin/env python3
"""核实 SLIP-39 词表的性质，并生成 Rust 常量。

不能只信规范文档的描述。这里实测三件事：
  1. 恰好 1024 个词（SLIP-39 是 10 bit/词，与 BIP-39 的 2048 不同）
  2. 前 4 字母唯一（这是选它而非 BIP-39 的理由：手抄时写前 4 个就够）
  3. 长度在 4..8 之间、全小写 ASCII、已排序

任何一条不成立，编解码的假设就要跟着改。
"""
import sys
from pathlib import Path

src = Path(sys.argv[1])
words = [w.strip() for w in src.read_text(encoding="utf-8").splitlines() if w.strip()]

print(f"词数：{len(words)}")
ok = True

if len(words) != 1024:
    print(f"  ✗ 期望 1024（10 bit/词），实际 {len(words)}")
    ok = False
else:
    print("  ✓ 恰好 1024 = 2^10，每词 10 bit")

prefixes = {}
dup = []
for w in words:
    p = w[:4]
    if p in prefixes:
        dup.append((prefixes[p], w))
    prefixes[p] = w
if dup:
    print(f"  ✗ 前 4 字母有重复：{dup[:5]}")
    ok = False
else:
    print("  ✓ 前 4 字母全局唯一——手抄只写前 4 个也不会有歧义")

bad_len = [w for w in words if not (4 <= len(w) <= 8)]
if bad_len:
    print(f"  ✗ 长度越界：{bad_len[:5]}")
    ok = False
else:
    print("  ✓ 长度均在 4..8")

bad_chr = [w for w in words if not w.isascii() or not w.islower() or not w.isalpha()]
if bad_chr:
    print(f"  ✗ 含非小写 ASCII 字母：{bad_chr[:5]}")
    ok = False
else:
    print("  ✓ 全部小写 ASCII 字母")

if words != sorted(words):
    print("  ✗ 未按字典序排列（二分查找会失效）")
    ok = False
else:
    print("  ✓ 已按字典序排列，可二分查找")

if not ok:
    sys.exit(1)

# 生成 Rust 常量。每行放 8 个，便于 review diff
out = Path(sys.argv[2])
lines = [
    "//! SLIP-39 英文词表（1024 词），公有领域。",
    "//!",
    "//! # 为什么用 SLIP-39 的词表而不是 BIP-39",
    "//!",
    "//! 两者都是公开的固定词表，但 SLIP-39 对**手抄**这个真实场景友好得多：",
    "//! 全部 4–8 字母，且**前 4 个字母全局唯一**（已在生成时实测校验）。",
    "//! 用户抄在纸上时写前 4 个字母就够，不会有歧义；BIP-39 的词有长有短、",
    "//! 前缀也不唯一。",
    "//!",
    "//! 我们只借用这张词表，**不套用 SLIP-39 的分片格式**——omy 的恢复码不是",
    "//! Shamir 分片，也没有与钱包互操作的需求。",
    "//!",
    "//! # 1024 而非 2048",
    "//!",
    "//! SLIP-39 是 10 bit/词。256 bit 熵 + 校验位的编码方式见 `recovery` 模块。",
    "//!",
    "//! 本文件由 `spikes/gen-wordlist.py` 从上游词表生成，请勿手工编辑。",
    "",
    "/// 词表长度。每个词承载 10 bit。",
    "pub const WORDLIST_LEN: usize = 1024;",
    "",
    "/// 每个词承载的比特数。",
    "pub const BITS_PER_WORD: usize = 10;",
    "",
    "// 两者必须自洽，否则编解码会静默产出错误长度的熵",
    "const _: () = assert!(1usize << BITS_PER_WORD == WORDLIST_LEN);",
    "",
    "/// SLIP-39 英文词表，字典序，前 4 字母唯一。",
    "pub const WORDS: [&str; WORDLIST_LEN] = [",
]
for i in range(0, len(words), 8):
    chunk = ", ".join(f'"{w}"' for w in words[i:i + 8])
    lines.append(f"    {chunk},")
lines.append("];")
lines.append("")

out.write_text("\n".join(lines), encoding="utf-8", newline="\n")
print(f"\n已生成 {out}（{len(words)} 词）")
