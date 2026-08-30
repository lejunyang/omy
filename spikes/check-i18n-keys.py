"""检查 .vue 里用到的 i18n 键是否都在文案文件里。

缺键的症状是界面上直接显示 `remote.readonly` 这种原始键名——
构建不会报错，测试也不会失败，只有肉眼看到才发现。
"""
import json, io, re, glob, os

base = r'E:\Projects\omy\crates\omy-gui\frontend'
loc = json.load(io.open(f'{base}\\public\\locales\\zh-CN.json', encoding='utf-8'))
errloc = json.load(io.open(f'{base}\\public\\locales\\zh-CN.errors.json', encoding='utf-8'))


def flat(d, prefix=''):
    out = set()
    for k, v in d.items():
        full = f'{prefix}{k}'
        if isinstance(v, dict):
            out |= flat(v, full + '.')
        else:
            out.add(full)
    return out


have = flat(loc)
haveerr = set(errloc.keys())

# i18n.t('x.y') / i18n.tn('x.y', n) / i18n.te('code')
pat_t = re.compile(r"i18n\.t\(\s*'([a-zA-Z0-9_.]+)'")
pat_tn = re.compile(r"i18n\.tn\(\s*'([a-zA-Z0-9_.]+)'")
pat_e = re.compile(r"i18n\.te\(\s*'([a-zA-Z0-9_.]+)'")

missing = []
missing_err = []
for f in glob.glob(f'{base}\\src\\**\\*.vue', recursive=True) + glob.glob(f'{base}\\src\\*.js'):
    s = io.open(f, encoding='utf-8').read()
    name = os.path.basename(f)
    for k in pat_t.findall(s):
        if k not in have:
            missing.append((name, k))
    # 复数键存成 x_one / x_other 两条，两条都要在
    for k in pat_tn.findall(s):
        for suffix in ('_one', '_other'):
            if f'{k}{suffix}' not in have:
                missing.append((name, f'{k}{suffix}'))
    for k in pat_e.findall(s):
        if k not in haveerr:
            missing_err.append((name, k))

if missing:
    print('MISSING UI KEYS:')
    for n, k in sorted(set(missing)):
        print(f'  {n}: {k}')
if missing_err:
    print('MISSING ERROR KEYS:')
    for n, k in sorted(set(missing_err)):
        print(f'  {n}: {k}')
if not missing and not missing_err:
    print(f'all keys present ({len(have)} ui, {len(haveerr)} err)')
