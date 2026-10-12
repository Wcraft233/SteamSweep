import json, os

with open(r'D:\work\kimi\steamsweep\steamsweep-report.json', encoding='utf-8') as f:
    rep = json.load(f)

os.chdir(r'D:\work\kimi\save-db')

GRAYLIST = ["microsoft","windows","google","chrome","chromium","firefox","mozilla","thunderbird",
"code","vscode","visual studio code","obs-studio","obs","mihomo","clash","clash verge","clashverge",
"bakaxl","vortex","discord","spotify","slack","jetbrains","cursor","nvidia","adobe","edge","brave",
"opera","git","github","zoom","wechat","tencent","netease","gsm backups","packages","fonts","pip",
"npm","nuget","docker","kimi-desktop"]
json.dump({"description": "启发式扫描误报灰名单（目录名小写整树剪枝）", "entries": GRAYLIST},
          open('graylist.json', 'w', encoding='utf-8'), ensure_ascii=False, indent=2)

count = 0
for rec in rep['save_leftovers']['records']:
    if rec['installed'] or not rec.get('files'):
        continue
    dirs = {}
    states = set()
    for fi in rec['files']:
        rel = fi['rel_path'].replace('\\', '/')
        parent = rel.rsplit('/', 1)[0] if '/' in rel else ''
        key = (fi['root'], parent)
        dirs[key] = dirs.get(key, 0) + 1
        states.add(fi['syncstate'])
    save_paths = [{"root": r, "pattern": (d + "/**") if d else "**", "files_seen": n, "verdict": "save"}
                  for (r, d), n in sorted(dirs.items())]
    obs = states.pop() if len(states) == 1 else "mixed"
    entry = {
        "appid": rec['appid'],
        "name": rec.get('name'),
        "installed_at_scan": False,
        "save_paths": save_paths,
        "cloud_behavior": {
            "observed_syncstate": obs,
            "cloud_files": rec['cloud_files'],
            "cloud_bytes": rec['cloud_bytes'],
            "local_files_present": any(fi['exists_locally'] for fi in rec['files']),
        },
        "stats": {"seen": 1, "deleted": 0},
        "source": "steamsweep-m1-coldstart",
    }
    shard = str(rec["appid"])[:3]
    os.makedirs(f"db/{shard}", exist_ok=True)
    json.dump(entry, open(f"db/{shard}/{rec['appid']}.json", 'w', encoding='utf-8'),
              ensure_ascii=False, indent=2)
    count += 1
print("db entries:", count)
