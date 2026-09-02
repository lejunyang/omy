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

# 占位符必须写成 {{name}}：i18n.js 的 interpolate 只认双花括号。
# 写成单花括号不会报任何错，界面上直接显示字面量 {n}——而且这类文案往往
# 出现在不常走的路径上（批量加密、连上远端设备），平时根本看不到
pat_single = re.compile(r'(?<!\{)\{(\w+)\}(?!\})')
bad_ph = []
for locname in ('zh-CN', 'en'):
    data = json.load(io.open(f'{base}\\public\\locales\\{locname}.json', encoding='utf-8'))

    def walk_ph(node, prefix=''):
        if isinstance(node, dict):
            for k, v in node.items():
                walk_ph(v, f'{prefix}.{k}' if prefix else k)
        elif isinstance(node, str):
            found = pat_single.findall(node)
            if found:
                bad_ph.append((locname, prefix, found))

    walk_ph(data)

# 中英键集合必须完全一致：缺一边的话切语言时那一处显示原始键名
en_ui = flat(json.load(io.open(f'{base}\\public\\locales\\en.json', encoding='utf-8')))
en_err = set(json.load(io.open(f'{base}\\public\\locales\\en.errors.json', encoding='utf-8')))
ui_diff = have ^ en_ui
err_diff = haveerr ^ en_err

if missing:
    print('MISSING UI KEYS:')
    for n, k in sorted(set(missing)):
        print(f'  {n}: {k}')
if missing_err:
    print('MISSING ERROR KEYS:')
    for n, k in sorted(set(missing_err)):
        print(f'  {n}: {k}')
if bad_ph:
    print('SINGLE-BRACE PLACEHOLDERS (interpolate 只认 {{name}}):')
    for locname, path, found in bad_ph:
        print(f'  {locname}: {path}  {found}')
if ui_diff:
    print('UI KEY SET MISMATCH zh-CN vs en:')
    for k in sorted(ui_diff):
        print(f'  {k}  （只在 {"zh-CN" if k in have else "en"}）')
if err_diff:
    print('ERROR KEY SET MISMATCH zh-CN vs en:')
    for k in sorted(err_diff):
        print(f'  {k}  （只在 {"zh-CN" if k in haveerr else "en"}）')

if not (missing or missing_err or bad_ph or ui_diff or err_diff):
    print(f'all keys present ({len(have)} ui, {len(haveerr)} err)')
else:
    raise SystemExit(1)
